//! The payer's retry (`kob_x402::client::retry::pay_with_retry`) against the real facilitator core over the deterministic mock
//! chain, with injected failures (no network): a lost order race, an unreachable facilitator, a node rejection, an answer lost
//! after the payment went through, every attempt failing, a worse re-quote above the payer's limit, and two retries of one
//! payment racing. In every case the merchant is paid at most once.

use std::collections::{BTreeSet, HashMap};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Barrier};
use std::thread;
use std::time::Duration;

use kob_executor::x402::ledger::State;
use kob_executor::x402::testutil::{Fixture, MERCHANT_KEY, PAYER_KEY};
use kob_x402::chain::{Outpoint, SubmitError, Txid};
use kob_x402::client::retry::{pay_with_retry, Built, GiveUp, RetryPolicy, RetryTransport, Sent};
use kob_x402::error::{Diag, X402Error};
use kob_x402::testkit::{p2pk_spk, pubkey};
use kob_x402::wire::{hex, FacilitatorRequest, SettlementResponse};

const AMOUNT: u64 = 100_000_000;
const SHOP: &str = "shop";

fn diag(r: &SettlementResponse) -> (String, bool, String) {
    let k = r.extensions.as_ref().map(|e| e["kaspa"].clone()).unwrap_or_default();
    (
        k["diagnostic"].as_str().unwrap_or("").to_string(),
        k["retryable"].as_bool().unwrap_or(false),
        k["message"].as_str().unwrap_or("").to_string(),
    )
}

fn merchant_outputs(f: &Fixture) -> usize {
    f.chain.unspent_of(&p2pk_spk(&pubkey(MERCHANT_KEY))).len()
}

/// What reached the merchant: the transactions of one payment's attempts (`attempts`: every signed id, hex and raw) that the
/// chain accepted, after checking that the merchant holds exactly one payment of `AMOUNT` per accepted attempt and that the
/// facilitator's ledger reports `Accepted` for exactly those attempts (never for one the chain did not accept).
fn paid_attempts(f: &Fixture, attempts: &[(String, Txid)]) -> Vec<String> {
    // a rebuild from unchanged inputs signs the same transaction again: one transaction, counted once
    let attempts: BTreeSet<(String, Txid)> = attempts.iter().cloned().collect();
    let on_chain: Vec<String> = attempts.iter().filter(|(_, id)| f.chain.is_accepted(id)).map(|(h, _)| h.clone()).collect();
    let in_ledger: Vec<String> =
        attempts.iter().filter(|(h, _)| f.ledger.get(h).is_some_and(|e| e.state == State::Accepted)).map(|(h, _)| h.clone()).collect();
    assert_eq!(in_ledger, on_chain, "the facilitator reports paid exactly the attempts the chain accepted");
    let received: u64 = f.chain.unspent_of(&p2pk_spk(&pubkey(MERCHANT_KEY))).iter().map(|(_, u)| u.amount).sum();
    assert_eq!(received, AMOUNT * on_chain.len() as u64, "the merchant holds one payment per accepted attempt");
    on_chain
}

/// Mines the mempool every few ms while it lives.
struct Miner(Arc<AtomicBool>, Option<thread::JoinHandle<()>>);

impl Miner {
    fn start(f: &Fixture) -> Miner {
        let stop = Arc::new(AtomicBool::new(false));
        let (chain, s) = (f.chain.clone(), stop.clone());
        Miner(
            stop,
            Some(thread::spawn(move || {
                while !s.load(Ordering::SeqCst) {
                    thread::sleep(Duration::from_millis(3));
                    chain.mine(0);
                }
            })),
        )
    }
}

impl Drop for Miner {
    fn drop(&mut self) {
        self.0.store(true, Ordering::SeqCst);
        if let Some(h) = self.1.take() {
            let _ = h.join();
        }
    }
}

/// What happens around one send (1-based send number over all attempts).
#[derive(Clone, Copy, PartialEq)]
enum Fault {
    /// The facilitator is unreachable (nothing reaches it): a timeout / 5xx.
    Down,
    /// The facilitator settles but the answer is lost on the way back.
    LoseAnswer,
    /// The node rejects this send's broadcast (mass / fee / policy).
    Reject,
    /// Another taker fills the order this attempt spends while the facilitator verifies it.
    LoseOrderRace,
}

/// A payer of `AMOUNT` sompi through the in-process facilitator. Its coins are the fixture payer's P2PK UTXOs; with `orders`
/// each attempt also spends the first order of the book that is still unspent (a fresh quote per attempt).
struct Payer<'a> {
    f: &'a Fixture,
    tag: &'static str,
    /// The book: order outpoints with what each costs the payer.
    orders: Vec<(Outpoint, u64)>,
    /// The payer's bound on what an order may cost (a worse re-quote above it is refused when it is built).
    max_pay: u64,
    /// A second coin this payer adds to each attempt (two racing payers spend the anchor plus their own coin).
    extra: Option<Outpoint>,
    /// The coin the first attempt spends (its anchor) instead of the payer's largest one: two retries of one payment hold
    /// the same signed first attempt's anchor, whatever the chain looks like when each of them starts.
    first_coin: Option<Outpoint>,
    /// Waited on once the first attempt is signed: racing retries have all signed before any of them sends.
    signed_together: Option<Arc<Barrier>>,
    faults: HashMap<u32, Fault>,
    reqs: HashMap<String, FacilitatorRequest>,
    /// Transaction ids of the attempts, in order.
    txids: Vec<String>,
    ids: Vec<Txid>,
    builds: u32,
    sends: u32,
    now: u64,
}

impl<'a> Payer<'a> {
    fn new(f: &'a Fixture, tag: &'static str) -> Payer<'a> {
        Payer {
            f,
            tag,
            orders: vec![],
            max_pay: u64::MAX,
            extra: None,
            first_coin: None,
            signed_together: None,
            faults: HashMap::new(),
            reqs: HashMap::new(),
            txids: vec![],
            ids: vec![],
            builds: 0,
            sends: 0,
            now: 0,
        }
    }
    fn order_set(&self) -> BTreeSet<Outpoint> {
        self.orders.iter().map(|(o, _)| *o).collect()
    }
    /// Every signed attempt: (hex id, id).
    fn attempts(&self) -> Vec<(String, Txid)> {
        self.txids.iter().cloned().zip(self.ids.iter().copied()).collect()
    }
}

impl RetryTransport for Payer<'_> {
    type Payment = ();

    fn build(&mut self, n: u32, anchor: Option<&Outpoint>) -> Result<Built<()>, X402Error> {
        self.builds += 1;
        let book = self.order_set();
        // the payer's own coins, largest first (the coin the anchor is): never an order, never this payer's extra coin
        let mut coins: Vec<(Outpoint, u64)> = self
            .f
            .chain
            .unspent_of(&p2pk_spk(&pubkey(PAYER_KEY)))
            .into_iter()
            .filter(|(o, _)| !book.contains(o) && Some(*o) != self.extra)
            .map(|(o, u)| (o, u.amount))
            .collect();
        coins.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
        let first = match anchor.or(self.first_coin.as_ref()) {
            Some(a) => *a,
            None => coins.first().ok_or_else(|| X402Error::state(Diag::InvalidKaspaExactUtxo, "no funds"))?.0,
        };
        let mut inputs = vec![first];
        let mut order = None;
        if !self.orders.is_empty() {
            // a fresh quote: the best order of the book that is still there, held to the payer's bound
            let (o, cost) = *self
                .orders
                .iter()
                .find(|(o, _)| self.f.chain.utxo(o).is_some())
                .ok_or_else(|| X402Error::state(Diag::OrderConflict, "the book is empty"))?;
            if cost > self.max_pay {
                return Err(X402Error::requirements(
                    Diag::Overpayment,
                    format!("the quote costs {cost}, above maxPay {}", self.max_pay),
                ));
            }
            inputs.push(o);
            order = Some(o);
        }
        if let Some(x) = self.extra {
            inputs.push(x);
        }
        // a wallet signs only coins the node reports as spendable (an input gone or spent in the mempool fails the build)
        if let Some(gone) = inputs.iter().find(|o| !self.f.chain.is_spendable(o)) {
            return Err(X402Error::state(Diag::InvalidKaspaExactUtxo, format!("input {gone} is not spendable")));
        }
        *self.f.verifier.orders.lock().unwrap() = order.into_iter().collect();
        let id = format!("{}-retry-attempt-{n:02}", self.tag);
        let (req, tx) = self.f.payment(&inputs, AMOUNT, &id, 7);
        self.reqs.insert(id.clone(), req);
        self.txids.push(hex(&tx.id().as_bytes()));
        self.ids.push(tx.id().as_bytes());
        if n == 1 {
            if let Some(b) = &self.signed_together {
                b.wait();
            }
        }
        let payer_inputs = inputs.iter().filter(|o| Some(**o) != order).copied().collect();
        Ok(Built { payment: (), payment_id: id, transaction_id: hex(&tx.id().as_bytes()), consumed: inputs, payer_inputs })
    }

    fn send(&mut self, a: &Built<()>) -> Sent {
        self.sends += 1;
        let fault = self.faults.get(&self.sends).copied();
        if fault == Some(Fault::Down) {
            return Sent::Unknown("the facilitator did not answer (timeout)".into());
        }
        if fault == Some(Fault::Reject) {
            self.f.chain.set_submit_failure(Some(SubmitError::Rejected("transaction mass exceeds the standard limit".into())));
        }
        if fault == Some(Fault::LoseOrderRace) {
            let taken = a.consumed.iter().copied().find(|o| self.order_set().contains(o)).unwrap();
            let c = self.f.chain.clone();
            *self.f.verifier.during_verify.lock().unwrap() = Some(Box::new(move || c.spend_externally(&taken)));
        }
        let r = self.f.fac.settle(SHOP, &self.reqs[&a.payment_id]);
        self.f.chain.set_submit_failure(None);
        *self.f.verifier.during_verify.lock().unwrap() = None;
        if fault == Some(Fault::LoseAnswer) {
            return Sent::Unknown("connection reset after the facilitator answered".into());
        }
        if r.success && r.transaction == a.transaction_id {
            return Sent::Settled(r);
        }
        let (diagnostic, retryable, message) = diag(&r);
        Sent::Failed { diagnostic, retryable, message, next: None }
    }

    fn unspent(&mut self, of: &[Outpoint]) -> Result<BTreeSet<Outpoint>, X402Error> {
        Ok(of.iter().filter(|o| self.f.chain.is_spendable(o)).copied().collect())
    }
    fn now_ms(&mut self) -> u64 {
        self.now
    }
    fn sleep_ms(&mut self, ms: u64) {
        self.now += ms;
    }
}

fn policy() -> RetryPolicy {
    RetryPolicy::default()
}

#[test]
fn a_lost_order_race_is_rebuilt_from_a_fresh_quote_and_paid_once() {
    let f = Fixture::new();
    let _miner = Miner::start(&f);
    let coin = f.fund(500_000_000);
    let (o1, o2) = (f.fund(300_000_000), f.fund(300_000_000));
    let mut p = Payer::new(&f, "race");
    p.orders = vec![(o1, 10), (o2, 12)];
    p.faults.insert(1, Fault::LoseOrderRace);
    let r = pay_with_retry(&policy(), 60, &mut p).unwrap();
    assert_eq!((r.attempts, r.resends, p.builds), (2, 0, 2));
    assert_eq!(r.anchor, Some(coin), "the payer's coin, never the order");
    assert!(r.attempt.consumed.contains(&o2) && r.attempt.consumed.contains(&coin));
    assert_eq!(r.superseded, vec!["race-retry-attempt-01".to_string()]);
    let first = f.ledger.get(&p.txids[0]).unwrap();
    assert_eq!(first.state, State::Failed, "the dead attempt released its inputs");
    assert_ne!(p.txids[0], p.txids[1]);
    assert_eq!(f.ledger.get(&r.attempt.transaction_id).unwrap().state, State::Accepted);
    assert_eq!(merchant_outputs(&f), 1);
    assert_eq!(paid_attempts(&f, &p.attempts()), vec![r.attempt.transaction_id]);
}

#[test]
fn an_unreachable_facilitator_gets_the_same_payment_again() {
    let f = Fixture::new();
    let _miner = Miner::start(&f);
    f.fund(500_000_000);
    let mut p = Payer::new(&f, "down");
    p.faults.insert(1, Fault::Down);
    p.faults.insert(2, Fault::Down);
    let r = pay_with_retry(&policy(), 60, &mut p).unwrap();
    assert_eq!((r.attempts, r.resends, p.builds, p.sends), (1, 2, 1, 3));
    assert_eq!(merchant_outputs(&f), 1);
    assert_eq!(paid_attempts(&f, &p.attempts()), vec![r.attempt.transaction_id]);
}

#[test]
fn a_rejected_broadcast_is_rebuilt_and_paid_once() {
    let f = Fixture::new();
    let _miner = Miner::start(&f);
    f.fund(500_000_000);
    let mut p = Payer::new(&f, "rejected");
    p.faults.insert(1, Fault::Reject);
    let r = pay_with_retry(&policy(), 60, &mut p).unwrap();
    assert_eq!((r.attempts, p.builds, p.sends), (2, 2, 2));
    assert_eq!(merchant_outputs(&f), 1);
    assert_eq!(paid_attempts(&f, &p.attempts()), vec![r.attempt.transaction_id]);
}

#[test]
fn a_payment_whose_answer_was_lost_is_found_never_paid_again() {
    let f = Fixture::new();
    let _miner = Miner::start(&f);
    let coin = f.fund(500_000_000);
    let mut p = Payer::new(&f, "lost-answer");
    p.faults.insert(1, Fault::LoseAnswer);
    let r = pay_with_retry(&policy(), 60, &mut p).unwrap();
    assert_eq!((r.attempts, r.resends, p.builds), (1, 1, 1), "re-sent, never rebuilt");
    assert_eq!(r.settlement.extensions.as_ref().unwrap()["kob"]["replayed"], true, "the facilitator's cached settlement");
    assert!(f.chain.utxo(&coin).is_none());
    assert_eq!(merchant_outputs(&f), 1);
    assert_eq!(paid_attempts(&f, &p.attempts()), vec![r.attempt.transaction_id]);
    assert_eq!(f.chain.submit_count(), 1, "broadcast once");
}

#[test]
fn every_attempt_failing_gives_up_after_three_and_moves_no_funds() {
    let f = Fixture::new();
    let _miner = Miner::start(&f);
    let coin = f.fund(500_000_000);
    let mut p = Payer::new(&f, "always-rejected");
    for n in 1..=10 {
        p.faults.insert(n, Fault::Reject);
    }
    let e = pay_with_retry(&policy(), 60, &mut p).unwrap_err();
    assert_eq!((e.why, e.attempts, e.signed.len(), p.sends), (GiveUp::Exhausted, 3, 3, 3));
    assert_eq!(e.last.diag, Diag::InvalidKaspaExactTransaction);
    assert!(!e.outcome_unknown);
    assert!(e.to_string().contains("after 3 attempt(s)"), "{e}");
    // every attempt spent the same anchor, none reached the chain: the payer's coin is untouched
    assert!(e.signed.iter().all(|b| b.consumed.contains(&coin)));
    assert!(f.chain.utxo(&coin).is_some());
    assert_eq!(merchant_outputs(&f), 0);
    assert!(paid_attempts(&f, &p.attempts()).is_empty());
}

#[test]
fn a_worse_requote_above_the_payers_bound_is_not_paid() {
    let f = Fixture::new();
    let _miner = Miner::start(&f);
    f.fund(500_000_000);
    let (o1, o2) = (f.fund(300_000_000), f.fund(300_000_000));
    let mut p = Payer::new(&f, "worse-quote");
    p.orders = vec![(o1, 10), (o2, 25)];
    p.max_pay = 20;
    p.faults.insert(1, Fault::LoseOrderRace);
    let e = pay_with_retry(&policy(), 60, &mut p).unwrap_err();
    assert_eq!((e.why, p.builds, p.sends), (GiveUp::Build, 2, 1));
    assert_eq!(e.last.diag, Diag::Overpayment);
    assert_eq!(merchant_outputs(&f), 0);
    assert!(paid_attempts(&f, &p.attempts()).is_empty());
}

#[test]
fn two_retries_of_one_payment_racing_pay_once() {
    let f = Arc::new(Fixture::new());
    let _miner = Miner::start(&f);
    let anchor = f.fund(500_000_000);
    let (x, y) = (f.fund(200_000_000), f.fund(200_000_000));
    // both retry the payment whose anchor is `anchor`, each adding a coin of its own (two different transactions); both have
    // signed their first attempt before either sends, so neither can start from a chain where the other already paid (it
    // would then pick another coin, the other's change, and make a second, unrelated payment)
    let signed = Arc::new(Barrier::new(2));
    let run = |tag: &'static str, extra: Outpoint, f: Arc<Fixture>, signed: Arc<Barrier>| {
        thread::spawn(move || {
            let mut p = Payer::new(&f, tag);
            p.extra = Some(extra);
            p.first_coin = Some(anchor);
            p.signed_together = Some(signed);
            let r = pay_with_retry(&RetryPolicy::default(), 60, &mut p);
            let anchored = match &r {
                Ok(r) => r.anchor,
                Err(e) => e.anchor,
            };
            assert_eq!(anchored, Some(anchor), "{tag}: every attempt sent spends the one anchor");
            (r.map(|r| r.attempt.transaction_id).map_err(|e| (e.why, e.last.diag)), p.attempts())
        })
    };
    let (a, b) = (run("racer-a", x, f.clone(), signed.clone()), run("racer-b", y, f.clone(), signed));
    let ((a, mut attempts), (b, attempts_b)) = (a.join().unwrap(), b.join().unwrap());
    attempts.extend(attempts_b);
    let paid = [&a, &b].iter().filter(|r| r.is_ok()).count();
    assert_eq!(paid, 1, "{a:?} {b:?}");
    let (winner, refused) = if a.is_ok() { (&a, &b) } else { (&b, &a) };
    assert!(
        matches!(refused, Err((GiveUp::Stopped, Diag::Replay)) | Err((GiveUp::AnchorSpent, _))),
        "the loser is refused or finds the anchor spent: {refused:?}"
    );
    // the chain, the merchant and the facilitator's ledger agree: one attempt of the payment went through, the reported one
    assert_eq!(paid_attempts(&f, &attempts), vec![winner.clone().unwrap()]);
    assert!(f.chain.utxo(&anchor).is_none());
    assert_eq!(merchant_outputs(&f), 1);
}
