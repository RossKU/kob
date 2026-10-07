//! Facilitator tests over the deterministic mock chain (no network).

use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::Duration;

use kaspa_consensus_core::tx::{ScriptPublicKey, Transaction};
use kob_x402::chain::{ChainError, ChainUtxo, ChainView, FixedClock, Outpoint, OutputStatus, SubmitError, Txid};
use kob_x402::policy::{AllowedToken, Custody, Policy};
use kob_x402::testkit::MockChain;
use kob_x402::wire::{hex, Finality, Network, SettlementResponse};

use super::facilitator::Facilitator;
use super::ledger::{Ledger, State};
use super::testutil::*;

const ID1: &str = "payment-id-0000000001";
const ID2: &str = "payment-id-0000000002";

fn diag(r: &SettlementResponse) -> String {
    r.extensions.as_ref().and_then(|e| e["kaspa"]["diagnostic"].as_str()).unwrap_or("").to_string()
}

fn retryable(r: &SettlementResponse) -> bool {
    r.extensions.as_ref().and_then(|e| e["kaspa"]["retryable"].as_bool()).unwrap_or(false)
}

/// Runs `f` on another thread `ms` after the next `submit` reaches the chain (so the action is ordered
/// after the broadcast, not against the wall clock).
fn after_submit(chain: &Arc<MockChain>, ms: u64, f: impl FnOnce(&MockChain) + Send + 'static) -> thread::JoinHandle<()> {
    let c = chain.clone();
    let base = c.submit_count();
    thread::spawn(move || {
        let t = std::time::Instant::now();
        while c.submit_count() <= base && t.elapsed() < Duration::from_secs(10) {
            thread::sleep(Duration::from_millis(1));
        }
        thread::sleep(Duration::from_millis(ms));
        f(&c);
    })
}

/// Mines the mock chain shortly after the next submit.
fn mine_later(chain: &Arc<MockChain>, ms: u64, advance: u64) -> thread::JoinHandle<()> {
    after_submit(chain, ms, move |c| c.mine(advance))
}

fn txid_hex(tx: &Transaction) -> String {
    hex(&tx.id().as_bytes())
}

// ------------------------------------------------------------------------------------------ happy path

#[test]
fn kas_payment_settles_at_accepted_finality() {
    let f = Fixture::new();
    let input = f.fund(500_000_000);
    let (req, tx) = f.payment(&[input], 100_000_000, ID1, 7);
    let miner = mine_later(&f.chain, 20, 0);
    let r = f.fac.settle("shop", &req);
    miner.join().unwrap();
    assert!(r.success, "{r:?}");
    assert_eq!(r.transaction, txid_hex(&tx));
    assert_eq!(r.network.as_deref(), Some("kaspa:testnet-10"));
    assert_eq!(r.amount.as_deref(), Some("100000000"));
    assert_eq!(r.payer.as_deref(), Some(address_of_key(PAYER_KEY).as_str()));
    let kaspa = &r.extensions.as_ref().unwrap()["kaspa"];
    assert_eq!(kaspa["binding"], "kaspa-exact-v2");
    assert_eq!(kaspa["profile"], "standard-native");
    assert_eq!(kaspa["finality"], "accepted");
    assert_eq!(kaspa["paymentOutputIndex"], 0);
    assert_eq!(f.chain.submit_count(), 1);
    let e = f.ledger.get(&txid_hex(&tx)).unwrap();
    assert_eq!(e.state, State::Accepted);
    assert_eq!(e.merchant, "shop");
    assert!(e.response.is_some());
    assert!(e.accepted_daa.is_some());
    assert_eq!(f.fac.metrics.settle_success.load(Ordering::Relaxed), 1);
}

#[test]
fn identical_retry_after_success_returns_the_cached_response() {
    let f = Fixture::new();
    let input = f.fund(500_000_000);
    let (req, _) = f.payment(&[input], 100_000_000, ID1, 7);
    let miner = mine_later(&f.chain, 20, 0);
    let first = f.fac.settle("shop", &req);
    miner.join().unwrap();
    assert!(first.success);
    let calls = f.verifier.calls.load(Ordering::SeqCst);
    // the input is spent by now: a re-verification would fail, the cached outcome does not
    let again = f.fac.settle("shop", &req);
    assert_eq!(again, first);
    assert_eq!(f.chain.submit_count(), 1, "never broadcast twice");
    assert_eq!(f.verifier.calls.load(Ordering::SeqCst), calls, "not re-verified");
    // the same payment identifier and request through another transaction: not this payment, refused (the first
    // settlement is never the answer for it), nothing broadcast, nothing recorded
    let other_input = f.fund(500_000_000);
    let (req2, tx2) = f.payment(&[other_input], 100_000_000, ID1, 7);
    let other = f.fac.settle("shop", &req2);
    assert!(!other.success, "{other:?}");
    assert_eq!(diag(&other), "kaspa_payment_identifier_conflict");
    assert!(other.transaction.is_empty());
    assert_eq!(f.chain.submit_count(), 1);
    assert!(f.ledger.get(&txid_hex(&tx2)).is_none());
    // /verify says the same
    let v = f.fac.verify(&req2);
    assert!(!v.is_valid);
    // the first transaction is still answered with its own outcome
    assert_eq!(f.fac.settle("shop", &req), first);
}

#[test]
fn concurrent_identical_settles_broadcast_once() {
    let f = Fixture::new();
    let input = f.fund(500_000_000);
    let (req, _) = f.payment(&[input], 100_000_000, ID1, 7);
    let miner = mine_later(&f.chain, 40, 0);
    let (a, b) = {
        let (fa, fb, ra, rb) = (f.fac.clone(), f.fac.clone(), req.clone(), req.clone());
        let ta = thread::spawn(move || fa.settle("shop", &ra));
        thread::sleep(Duration::from_millis(5));
        let tb = thread::spawn(move || fb.settle("shop", &rb));
        (ta.join().unwrap(), tb.join().unwrap())
    };
    miner.join().unwrap();
    assert!(a.success && b.success, "{a:?} {b:?}");
    assert_eq!(a.transaction, b.transaction);
    assert_eq!(f.chain.submit_count(), 1);
}

// --------------------------------------------------------------------------------------------- finality

#[test]
fn mempool_is_never_sufficient_and_a_retry_observes_without_reverifying() {
    let f = Fixture::new();
    f.fac.set_settle_wait(Duration::from_millis(60));
    let input = f.fund(500_000_000);
    let (req, tx) = f.payment(&[input], 100_000_000, ID1, 7);
    let r = f.fac.settle("shop", &req); // nobody mines: the settle wait runs out
    assert!(!r.success);
    assert_eq!(r.error_reason.as_deref(), Some("invalid_transaction_state"));
    assert_eq!(diag(&r), "settlement_pending");
    assert!(retryable(&r));
    assert!(r.transaction.is_empty());
    assert!(f.chain.in_mempool(&tx.id().as_bytes()).unwrap());
    assert_eq!(f.ledger.get(&txid_hex(&tx)).unwrap().state, State::Broadcast);
    let calls = f.verifier.calls.load(Ordering::SeqCst);

    // still pending on an immediate retry, without broadcasting again or re-verifying
    let r2 = f.fac.settle("shop", &req);
    assert_eq!(diag(&r2), "settlement_pending");
    assert_eq!(f.chain.submit_count(), 1);
    assert_eq!(f.verifier.calls.load(Ordering::SeqCst), calls);

    // mined: the retry succeeds although the inputs are spent now
    f.chain.mine(0);
    let r3 = f.fac.settle("shop", &req);
    assert!(r3.success, "{r3:?}");
    assert_eq!(r3.transaction, txid_hex(&tx));
    assert_eq!(f.chain.submit_count(), 1);
    assert_eq!(f.verifier.calls.load(Ordering::SeqCst), calls);
    assert_eq!(f.ledger.get(&txid_hex(&tx)).unwrap().state, State::Accepted);
}

#[test]
fn confirmed_finality_needs_the_configured_depth() {
    let f = Fixture::new();
    f.fac.set_settle_wait(Duration::from_millis(60));
    let input = f.fund(500_000_000);
    let (req, tx) = f.payment_with(&[input], 100_000_000, ID1, 7, Finality::Confirmed);
    // accepted (in the virtual UTXO set) but not deep enough
    let miner = mine_later(&f.chain, 10, 0);
    let r = f.fac.settle("shop", &req);
    miner.join().unwrap();
    assert!(!r.success);
    assert_eq!(diag(&r), "settlement_pending");
    assert_eq!(f.ledger.get(&txid_hex(&tx)).unwrap().state, State::Broadcast);
    // 99 DAA deep: still not confirmed
    let block_daa = f.chain.utxo(&Outpoint::new(tx.id().as_bytes(), 0)).unwrap().block_daa_score;
    let now = f.chain.daa();
    f.chain.advance_daa(block_daa + 99 - now);
    assert!(!f.fac.settle("shop", &req).success);
    // exactly `confirmations_daa` deep
    f.chain.advance_daa(1);
    let r = f.fac.settle("shop", &req);
    assert!(r.success, "{r:?}");
    assert_eq!(r.extensions.as_ref().unwrap()["kaspa"]["finality"], "confirmed");
    assert_eq!(f.chain.submit_count(), 1);
}

#[test]
fn accepted_requirement_is_met_at_once_when_the_tx_is_in_the_virtual_utxo_set() {
    let f = Fixture::new();
    let input = f.fund(500_000_000);
    let (req, _) = f.payment(&[input], 100_000_000, ID1, 7);
    let miner = mine_later(&f.chain, 10, 0);
    let t = std::time::Instant::now();
    assert!(f.fac.settle("shop", &req).success);
    miner.join().unwrap();
    assert!(t.elapsed() < Duration::from_secs(3), "returns as soon as the output is accepted, not after the settle wait");
}

// ---------------------------------------------------------------------------------------------- verify

#[test]
fn verify_is_read_only() {
    let f = Fixture::new();
    let input = f.fund(500_000_000);
    let (req, _) = f.payment(&[input], 100_000_000, ID1, 7);
    let v = f.fac.verify(&req);
    assert!(v.is_valid, "{v:?}");
    assert_eq!(v.payer.as_deref(), Some(address_of_key(PAYER_KEY).as_str()));
    assert!(f.ledger.is_empty(), "verify must not write the ledger");
    assert_eq!(f.chain.submit_count(), 0);
}

#[test]
fn request_hash_is_mandatory_and_never_inferred() {
    let f = Fixture::new();
    let input = f.fund(500_000_000);
    let (mut req, _) = f.payment(&[input], 100_000_000, ID1, 7);
    // absent
    let good = req.request_hash.take().unwrap();
    let v = f.fac.verify(&req);
    assert!(!v.is_valid);
    assert_eq!(v.invalid_reason.as_deref(), Some("invalid_payload"));
    let s = f.fac.settle("shop", &req);
    assert!(!s.success);
    assert_eq!(s.error_reason.as_deref(), Some("invalid_payload"));
    // malformed
    req.request_hash = Some("nothex".into());
    assert!(!f.fac.verify(&req).is_valid);
    // differs from the payload's hash
    req.request_hash = Some(hex(&[8; 32]));
    let v = f.fac.verify(&req);
    assert!(!v.is_valid);
    assert_eq!(v.extensions.unwrap()["kaspa"]["diagnostic"], "invalid_kaspa_x402_payload");
    assert!(!f.fac.settle("shop", &req).success);
    req.request_hash = Some(good);
    assert!(f.fac.verify(&req).is_valid);
    assert!(f.ledger.is_empty());
    assert_eq!(f.chain.submit_count(), 0);
}

#[test]
fn wrong_version_and_tampered_offer_are_rejected_without_state() {
    let f = Fixture::new();
    let input = f.fund(500_000_000);
    let (mut req, _) = f.payment(&[input], 100_000_000, ID1, 7);
    req.x402_version = 1;
    assert_eq!(f.fac.verify(&req).invalid_reason.as_deref(), Some("invalid_x402_version"));
    req.x402_version = 2;
    // the payer changed the price: accepted differs from the merchant's offer
    req.payment_requirements.amount = "1".into();
    let s = f.fac.settle("shop", &req);
    assert!(!s.success);
    assert_eq!(diag(&s), "invalid_kaspa_x402_accepted");
    assert!(f.ledger.is_empty());
    assert_eq!(f.chain.submit_count(), 0);
}

#[test]
fn verify_reports_replay_evidence_without_writing() {
    let f = Fixture::new();
    let input = f.fund(500_000_000);
    let (req, _) = f.payment(&[input], 100_000_000, ID1, 7);
    let miner = mine_later(&f.chain, 10, 0);
    assert!(f.fac.settle("shop", &req).success);
    miner.join().unwrap();
    // another payment reusing the payment identifier with another request hash
    let input2 = f.fund(500_000_000);
    let (req2, _) = f.payment(&[input2], 100_000_000, ID1, 9);
    let v = f.fac.verify(&req2);
    assert!(!v.is_valid);
    assert_eq!(v.extensions.unwrap()["kaspa"]["diagnostic"], "kaspa_payment_identifier_conflict");
    assert_eq!(f.ledger.len(), 1);
}

// ----------------------------------------------------------------------------------------- replay rules

#[test]
fn payment_identifier_bound_to_the_request() {
    let f = Fixture::new();
    let a = f.fund(500_000_000);
    let b = f.fund(500_000_000);
    let (r1, _) = f.payment(&[a], 100_000_000, ID1, 7);
    let (r2, tx2) = f.payment(&[b], 100_000_000, ID1, 8); // same id, different request hash
    let miner = mine_later(&f.chain, 10, 0);
    assert!(f.fac.settle("shop", &r1).success);
    miner.join().unwrap();
    let s = f.fac.settle("shop", &r2);
    assert!(!s.success);
    assert_eq!(s.error_reason.as_deref(), Some("invalid_transaction_state"));
    assert_eq!(diag(&s), "kaspa_payment_identifier_conflict");
    assert_eq!(f.chain.submit_count(), 1);
    assert!(f.ledger.get(&txid_hex(&tx2)).is_none());
}

#[test]
fn an_outpoint_is_consumed_at_most_once() {
    let f = Fixture::new();
    let a = f.fund(500_000_000);
    let (q1, _) = f.payment(&[a], 100_000_000, ID1, 7);
    let (q2, tx2) = f.payment(&[a], 120_000_000, ID2, 8); // a different payment spending the same input
                                                          // the first attempt ends with an unknown outcome: its evidence stays consumed
    f.chain.set_submit_failure(Some(SubmitError::Unavailable("down".into())));
    let s1 = f.fac.settle("shop", &q1);
    assert_eq!(diag(&s1), "node_unavailable");
    f.chain.set_submit_failure(None);
    let s2 = f.fac.settle("shop", &q2);
    assert!(!s2.success);
    assert_eq!(s2.error_reason.as_deref(), Some("invalid_transaction_state"));
    assert_eq!(diag(&s2), "replay");
    assert_eq!(f.chain.submit_count(), 1, "the conflicting spend was never broadcast");
    assert!(f.ledger.get(&txid_hex(&tx2)).is_none());
}

#[test]
fn same_transaction_for_another_request_or_merchant_conflicts() {
    let f = Fixture::new();
    let a = f.fund(500_000_000);
    let (req, _) = f.payment(&[a], 100_000_000, ID1, 7);
    let miner = mine_later(&f.chain, 10, 0);
    assert!(f.fac.settle("shop", &req).success);
    miner.join().unwrap();
    // same transaction, another merchant asking
    let s = f.fac.settle("other-shop", &req);
    assert!(!s.success);
    assert_eq!(diag(&s), "replay");
    // same transaction with another request hash
    let mut other = req.clone();
    other.request_hash = Some(hex(&[9; 32]));
    other.payment_payload.payload.request_hash = hex(&[9; 32]);
    let s = f.fac.settle("shop", &other);
    assert!(!s.success);
    assert_eq!(diag(&s), "replay");
    assert_eq!(f.chain.submit_count(), 1);
}

// ------------------------------------------------------------------------------------------------ expiry

#[test]
fn expiry_is_rechecked_after_verification_and_before_any_state() {
    let f = Fixture::new();
    let a = f.fund(500_000_000);
    let (req, _) = f.payment(&[a], 100_000_000, ID1, 7);
    // verification "takes" 61 s: the authorization lapses during the awaited work
    *f.verifier.advance_clock.lock().unwrap() = Some((f.clock.clone(), 61_000));
    let s = f.fac.settle("shop", &req);
    assert!(!s.success);
    assert_eq!(s.error_reason.as_deref(), Some("invalid_transaction_state"));
    assert_eq!(diag(&s), "expired_authorization");
    assert!(f.ledger.is_empty(), "no settlement was created");
    assert_eq!(f.chain.submit_count(), 0);
    assert_eq!(f.fac.metrics.expired_rejected.load(Ordering::Relaxed), 1);
}

#[test]
fn expiry_exactly_at_now_is_expired() {
    let f = Fixture::new();
    let a = f.fund(500_000_000);
    let (req, _) = f.payment(&[a], 100_000_000, ID1, 7);
    f.verifier.expires_at_ms.store(START_MS, std::sync::atomic::Ordering::SeqCst);
    assert_eq!(diag(&f.fac.settle("shop", &req)), "expired_authorization");
    f.verifier.expires_at_ms.store(START_MS + 1, std::sync::atomic::Ordering::SeqCst);
    let miner = mine_later(&f.chain, 10, 0);
    assert!(f.fac.settle("shop", &req).success);
    miner.join().unwrap();
}

#[test]
fn an_expired_retry_still_resumes_an_accepted_attempt() {
    let f = Fixture::new();
    let a = f.fund(500_000_000);
    let (req, _) = f.payment(&[a], 100_000_000, ID1, 7);
    let miner = mine_later(&f.chain, 10, 0);
    let first = f.fac.settle("shop", &req);
    miner.join().unwrap();
    assert!(first.success);
    f.clock.set(START_MS + 3_600_000);
    assert_eq!(f.fac.settle("shop", &req), first, "expiry does not invalidate recovery of an accepted attempt");
}

// -------------------------------------------------------------------------------------- node outcomes

#[test]
fn node_outage_at_submit_is_ambiguous_and_recovers_on_retry() {
    let f = Fixture::new();
    let a = f.fund(500_000_000);
    let (req, tx) = f.payment(&[a], 100_000_000, ID1, 7);
    f.chain.set_submit_failure(Some(SubmitError::Unavailable("connection reset".into())));
    let s = f.fac.settle("shop", &req);
    assert!(!s.success);
    assert_eq!(s.error_reason.as_deref(), Some("unexpected_settle_error"));
    assert_eq!(diag(&s), "node_unavailable");
    assert!(retryable(&s));
    let e = f.ledger.get(&txid_hex(&tx)).unwrap();
    assert_eq!(e.state, State::Ambiguous, "the outcome is unknown: the evidence stays consumed");
    assert!(f.ledger.is_consumed(&Outpoint::new(tx.inputs[0].previous_outpoint.transaction_id.as_bytes(), 0)));
    // node back: the identical request re-verifies and resubmits the same transaction
    f.chain.set_submit_failure(None);
    let miner = mine_later(&f.chain, 20, 0);
    let s = f.fac.settle("shop", &req);
    miner.join().unwrap();
    assert!(s.success, "{s:?}");
    assert_eq!(f.chain.submit_count(), 2);
    assert_eq!(f.ledger.get(&txid_hex(&tx)).unwrap().state, State::Accepted);
}

/// C5 X-9: an ambiguous payment the node still does not know after `ambiguous_grace` (not in the mempool, its merchant
/// output not on chain) is failed and its outpoints released; before that it stays ambiguous (the evidence consumed).
#[test]
fn an_ambiguous_payment_unknown_to_the_node_is_released_after_the_grace() {
    let f = Fixture::new();
    let a = f.fund(500_000_000);
    let (req, tx) = f.payment(&[a], 100_000_000, ID1, 7);
    f.chain.set_submit_failure(Some(SubmitError::Unavailable("connection reset".into())));
    let s = f.fac.settle("shop", &req);
    assert!(!s.success);
    f.chain.set_submit_failure(None);
    let input = Outpoint::new(tx.inputs[0].previous_outpoint.transaction_id.as_bytes(), 0);
    assert_eq!(f.ledger.get(&txid_hex(&tx)).unwrap().state, State::Ambiguous);
    // within the grace (an hour by default): still ambiguous, the evidence stays consumed
    f.clock.set(START_MS + 30 * 60_000);
    let rep = f.fac.reconcile();
    assert_eq!(rep.released_ambiguous, 0);
    assert_eq!(f.ledger.get(&txid_hex(&tx)).unwrap().state, State::Ambiguous);
    assert!(f.ledger.is_consumed(&input));
    // past it: failed, released
    f.clock.set(START_MS + 61 * 60_000);
    let rep = f.fac.reconcile();
    assert_eq!(rep.released_ambiguous, 1);
    let e = f.ledger.get(&txid_hex(&tx)).unwrap();
    assert_eq!(e.state, State::Failed);
    assert!(e.reason.as_deref().unwrap().starts_with("ambiguous"), "{:?}", e.reason);
    assert!(!f.ledger.is_consumed(&input), "the payer's outpoints are released");
    assert_eq!(f.chain.submit_count(), 1, "nothing was resubmitted");
}

#[test]
fn a_dead_node_fails_verification_closed_and_creates_nothing() {
    let f = Fixture::new();
    let a = f.fund(500_000_000);
    let (req, _) = f.payment(&[a], 100_000_000, ID1, 7);
    f.chain.set_unavailable(true);
    let s = f.fac.settle("shop", &req);
    assert!(!s.success);
    assert_eq!(diag(&s), "node_unavailable");
    assert!(retryable(&s));
    assert!(f.ledger.is_empty());
    let v = f.fac.verify(&req);
    assert!(!v.is_valid);
}

#[test]
fn node_going_away_while_observing_keeps_the_broadcast() {
    let f = Fixture::new();
    let a = f.fund(500_000_000);
    let (req, tx) = f.payment(&[a], 100_000_000, ID1, 7);
    // submit works, then every lookup fails
    struct DieAfterSubmit(Arc<MockChain>);
    impl ChainView for DieAfterSubmit {
        fn utxos(&self, w: &[(Outpoint, ScriptPublicKey)]) -> Result<Vec<Option<ChainUtxo>>, ChainError> {
            self.0.utxos(w)
        }
        fn utxos_of(&self, s: &ScriptPublicKey) -> Result<Vec<(Outpoint, ChainUtxo)>, ChainError> {
            self.0.utxos_of(s)
        }
        fn virtual_daa_score(&self) -> Result<u64, ChainError> {
            self.0.virtual_daa_score()
        }
        fn output_status(&self, o: &Outpoint, s: &ScriptPublicKey) -> Result<OutputStatus, ChainError> {
            self.0.output_status(o, s)
        }
        fn in_mempool(&self, t: &Txid) -> Result<bool, ChainError> {
            self.0.in_mempool(t)
        }
        fn submit(&self, tx: &Transaction) -> Result<Txid, SubmitError> {
            let r = self.0.submit(tx);
            self.0.set_unavailable(true);
            r
        }
    }
    let chain = f.chain.clone();
    let g = Fixture::with_view(f.ledger.clone(), chain.clone(), Arc::new(DieAfterSubmit(chain.clone())), f.clock.clone());
    g.fac.set_settle_wait(Duration::from_millis(60));
    let s = g.fac.settle("shop", &req);
    assert!(!s.success);
    assert_eq!(diag(&s), "node_unavailable");
    assert_eq!(g.ledger.get(&txid_hex(&tx)).unwrap().state, State::Broadcast);
    chain.set_unavailable(false);
    chain.mine(0);
    assert!(g.fac.settle("shop", &req).success);
}

#[test]
fn definitive_rejections_release_the_evidence() {
    for (err, want_diag) in [
        (SubmitError::Rejected("transaction is not standard".into()), "invalid_kaspa_exact_transaction"),
        (SubmitError::Conflict("output already spent by another transaction in the mempool".into()), "invalid_kaspa_exact_utxo"),
    ] {
        let f = Fixture::new();
        let a = f.fund(500_000_000);
        let (req, tx) = f.payment(&[a], 100_000_000, ID1, 7);
        f.chain.set_submit_failure(Some(err));
        let s = f.fac.settle("shop", &req);
        assert!(!s.success);
        assert_eq!(s.error_reason.as_deref(), Some("invalid_transaction_state"));
        assert_eq!(diag(&s), want_diag);
        assert!(!retryable(&s));
        assert_eq!(f.ledger.get(&txid_hex(&tx)).unwrap().state, State::Failed);
        assert!(!f.ledger.is_consumed(&Outpoint::new(tx.inputs[0].previous_outpoint.transaction_id.as_bytes(), 0)));
        // released: the same payment can be attempted again once the node accepts it
        f.chain.set_submit_failure(None);
        let miner = mine_later(&f.chain, 10, 0);
        let s = f.fac.settle("shop", &req);
        miner.join().unwrap();
        assert!(s.success, "{s:?}");
    }
}

#[test]
fn already_known_by_the_node_is_benign() {
    let f = Fixture::new();
    let a = f.fund(500_000_000);
    let (req, tx) = f.payment(&[a], 100_000_000, ID1, 7);
    // the payer broadcast it themselves
    f.chain.submit(&tx).unwrap();
    let miner = mine_later(&f.chain, 10, 0);
    let s = f.fac.settle("shop", &req);
    miner.join().unwrap();
    assert!(s.success, "{s:?}");
}

// --------------------------------------------------------------------------------------- crash recovery

/// A chain view that "crashes" (panics) around the submit call.
struct CrashChain {
    inner: Arc<MockChain>,
    /// 0 = off, 1 = crash before the node sees the submit, 2 = crash after the node accepted it.
    mode: AtomicU8,
}

impl ChainView for CrashChain {
    fn utxos(&self, w: &[(Outpoint, ScriptPublicKey)]) -> Result<Vec<Option<ChainUtxo>>, ChainError> {
        self.inner.utxos(w)
    }
    fn utxos_of(&self, s: &ScriptPublicKey) -> Result<Vec<(Outpoint, ChainUtxo)>, ChainError> {
        self.inner.utxos_of(s)
    }
    fn virtual_daa_score(&self) -> Result<u64, ChainError> {
        self.inner.virtual_daa_score()
    }
    fn output_status(&self, o: &Outpoint, s: &ScriptPublicKey) -> Result<OutputStatus, ChainError> {
        self.inner.output_status(o, s)
    }
    fn in_mempool(&self, t: &Txid) -> Result<bool, ChainError> {
        self.inner.in_mempool(t)
    }
    fn submit(&self, tx: &Transaction) -> Result<Txid, SubmitError> {
        match self.mode.load(Ordering::SeqCst) {
            1 => panic!("simulated crash before the broadcast"),
            2 => {
                let _ = self.inner.submit(tx);
                panic!("simulated crash after the broadcast");
            }
            _ => self.inner.submit(tx),
        }
    }
}

fn crash_scenario(
    mode: u8,
) -> (tempfile::TempDir, std::path::PathBuf, Arc<MockChain>, Arc<FixedClock>, kob_x402::wire::FacilitatorRequest, Transaction) {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("ledger.jsonl");
    let chain = Arc::new(MockChain::new());
    let clock = Arc::new(FixedClock::new(START_MS));
    let crash = Arc::new(CrashChain { inner: chain.clone(), mode: AtomicU8::new(mode) });
    let f = Fixture::with_view(Arc::new(Ledger::open(&path).unwrap()), chain.clone(), crash.clone(), clock.clone());
    let a = f.fund(500_000_000);
    let (req, tx) = f.payment(&[a], 100_000_000, ID1, 7);
    let fac = f.fac.clone();
    let r = req.clone();
    // the "process" dies mid-settle
    assert!(thread::spawn(move || fac.settle("shop", &r)).join().is_err());
    (dir, path, chain, clock, req, tx)
}

#[test]
fn crash_between_claim_and_broadcast_reconciles_instead_of_double_broadcasting() {
    let (_dir, path, chain, clock, req, tx) = crash_scenario(1);
    // restart: a new process opens the same ledger
    let ledger = Arc::new(Ledger::open(&path).unwrap());
    let e = ledger.get(&txid_hex(&tx)).expect("the claim was durable");
    assert_eq!(e.state, State::Pending);
    let f = Fixture::with_ledger(ledger.clone(), chain.clone(), clock);
    // the evidence is still consumed: a different payment of the same input is refused
    let rep = f.fac.reconcile();
    assert_eq!(rep.unseen_pending, 1, "never reached the node: reported, kept pending");
    assert_eq!(ledger.get(&txid_hex(&tx)).unwrap().state, State::Pending);
    assert_eq!(chain.submit_count(), 0);
    // the identical retry resumes: verified again, submitted once, observed
    let miner = mine_later(&chain, 20, 0);
    let s = f.fac.settle("shop", &req);
    miner.join().unwrap();
    assert!(s.success, "{s:?}");
    assert_eq!(chain.submit_count(), 1);
    assert_eq!(ledger.get(&txid_hex(&tx)).unwrap().state, State::Accepted);
}

#[test]
fn a_pending_entry_the_chain_never_saw_expires_after_the_grace_and_frees_its_outpoints() {
    // nothing else resolves a claim whose transaction never reached the node and whose payer never retries
    let (_dir, path, chain, clock, req, tx) = crash_scenario(1);
    let ledger = Arc::new(Ledger::open(&path).unwrap());
    let f = Fixture::with_ledger(ledger.clone(), chain.clone(), clock.clone());
    let rep = f.fac.reconcile();
    assert_eq!((rep.unseen_pending, rep.expired_pending), (1, 0), "inside the grace it is kept");
    assert_eq!(ledger.get(&txid_hex(&tx)).unwrap().state, State::Pending);
    let consumed = ledger.get(&txid_hex(&tx)).unwrap().consumed_outpoints();
    assert!(consumed.iter().all(|o| ledger.is_consumed(o)));

    clock.set(START_MS + 16 * 60 * 1000); // past the 15 minute default
    let rep = f.fac.reconcile();
    assert_eq!(rep.expired_pending, 1);
    let e = ledger.get(&txid_hex(&tx)).unwrap();
    assert_eq!(e.state, State::Failed);
    assert!(consumed.iter().all(|o| !ledger.is_consumed(o)), "the payer's outpoints are free again");
    let _ = req;
}

#[test]
fn crash_after_the_node_took_the_transaction_never_rebroadcasts() {
    let (_dir, path, chain, clock, req, tx) = crash_scenario(2);
    assert_eq!(chain.submit_count(), 1);
    let ledger = Arc::new(Ledger::open(&path).unwrap());
    assert_eq!(ledger.get(&txid_hex(&tx)).unwrap().state, State::Pending, "the crash lost the broadcast record");
    let f = Fixture::with_ledger(ledger.clone(), chain.clone(), clock);
    // reconcile finds it in the mempool
    let rep = f.fac.reconcile();
    assert_eq!(rep.seen, 1);
    assert_eq!(ledger.get(&txid_hex(&tx)).unwrap().state, State::Broadcast);
    // mined while the service was down: the retry finishes it without a second broadcast or re-verification
    chain.mine(0);
    let s = f.fac.settle("shop", &req);
    assert!(s.success, "{s:?}");
    assert_eq!(chain.submit_count(), 1);
    assert_eq!(f.verifier.calls.load(Ordering::SeqCst), 0);
}

#[test]
fn crash_recovery_without_a_reconcile_pass_uses_the_ledger_on_retry() {
    let (_dir, path, chain, clock, req, tx) = crash_scenario(2);
    chain.mine(0);
    let ledger = Arc::new(Ledger::open(&path).unwrap());
    let f = Fixture::with_ledger(ledger, chain.clone(), clock);
    let s = f.fac.settle("shop", &req);
    assert!(s.success, "{s:?}");
    assert_eq!(s.transaction, txid_hex(&tx));
    assert_eq!(chain.submit_count(), 1);
}

// ------------------------------------------------------------------------------------------------ reorg

#[test]
fn reconcile_marks_a_vanished_accepted_output_ambiguous_and_keeps_the_evidence() {
    let f = Fixture::new();
    let a = f.fund(500_000_000);
    let (req, tx) = f.payment(&[a], 100_000_000, ID1, 7);
    let miner = mine_later(&f.chain, 10, 0);
    assert!(f.fac.settle("shop", &req).success);
    miner.join().unwrap();
    let out = Outpoint::new(tx.id().as_bytes(), 0);
    // healthy: nothing to do
    let rep = f.fac.reconcile();
    assert_eq!((rep.checked, rep.reorged), (1, 0));
    // a reorg drops the block: the merchant output is gone and the transaction is not in the mempool
    f.chain.spend_externally(&out);
    let rep = f.fac.reconcile();
    assert_eq!(rep.reorged, 1);
    let e = f.ledger.get(&txid_hex(&tx)).unwrap();
    assert_eq!(e.state, State::Ambiguous);
    assert!(e.reason.unwrap().contains("vanished"));
    assert!(f.ledger.is_consumed(&Outpoint::new(tx.inputs[0].previous_outpoint.transaction_id.as_bytes(), 0)), "never reusable");
    assert_eq!(f.fac.metrics.reorgs_detected.load(Ordering::Relaxed), 1);
}

#[test]
fn a_reorged_transaction_that_returns_to_the_mempool_is_not_ambiguous() {
    /// The node re-queued the transaction after a reorg: it reports it in its mempool.
    struct Requeued(Arc<MockChain>);
    impl ChainView for Requeued {
        fn utxos(&self, w: &[(Outpoint, ScriptPublicKey)]) -> Result<Vec<Option<ChainUtxo>>, ChainError> {
            self.0.utxos(w)
        }
        fn utxos_of(&self, s: &ScriptPublicKey) -> Result<Vec<(Outpoint, ChainUtxo)>, ChainError> {
            self.0.utxos_of(s)
        }
        fn virtual_daa_score(&self) -> Result<u64, ChainError> {
            self.0.virtual_daa_score()
        }
        fn output_status(&self, o: &Outpoint, s: &ScriptPublicKey) -> Result<OutputStatus, ChainError> {
            self.0.output_status(o, s)
        }
        fn in_mempool(&self, _: &Txid) -> Result<bool, ChainError> {
            Ok(true)
        }
        fn submit(&self, tx: &Transaction) -> Result<Txid, SubmitError> {
            self.0.submit(tx)
        }
    }
    let f = Fixture::new();
    let a = f.fund(500_000_000);
    let (req, tx) = f.payment(&[a], 100_000_000, ID1, 7);
    let miner = mine_later(&f.chain, 10, 0);
    assert!(f.fac.settle("shop", &req).success);
    miner.join().unwrap();
    f.chain.spend_externally(&Outpoint::new(tx.id().as_bytes(), 0));
    let g = Fixture::with_view(f.ledger.clone(), f.chain.clone(), Arc::new(Requeued(f.chain.clone())), f.clock.clone());
    let rep = g.fac.reconcile();
    assert_eq!(rep.reorged, 0);
    assert_eq!(f.ledger.get(&txid_hex(&tx)).unwrap().state, State::Accepted);
}

#[test]
fn old_accepted_entries_leave_the_reorg_watch() {
    let f = Fixture::new();
    let a = f.fund(500_000_000);
    let (req, tx) = f.payment(&[a], 100_000_000, ID1, 7);
    let miner = mine_later(&f.chain, 10, 0);
    assert!(f.fac.settle("shop", &req).success);
    miner.join().unwrap();
    f.chain.advance_daa(10_001); // beyond reorg_watch_daa
    f.chain.spend_externally(&Outpoint::new(tx.id().as_bytes(), 0)); // e.g. the merchant spent it
    let rep = f.fac.reconcile();
    assert_eq!((rep.checked, rep.reorged), (0, 0));
    assert_eq!(f.ledger.get(&txid_hex(&tx)).unwrap().state, State::Accepted);
}

#[test]
fn reconcile_finalizes_broadcast_entries_and_a_lost_broadcast_is_resubmitted_on_retry() {
    let f = Fixture::new();
    let a = f.fund(500_000_000);
    let (req, tx) = f.payment(&[a], 100_000_000, ID1, 7);
    f.fac.set_settle_wait(Duration::from_millis(60));
    let s = f.fac.settle("shop", &req);
    assert_eq!(diag(&s), "settlement_pending");
    f.fac.set_settle_wait(Duration::from_secs(5));
    // the node restarts and forgets its mempool: the ledger still says broadcast
    f.chain.evict_mempool();
    let rep = f.fac.reconcile();
    assert_eq!(rep.finalized, 0);
    assert_eq!(f.ledger.get(&txid_hex(&tx)).unwrap().state, State::Broadcast);
    // the identical retry notices the chain does not know it any more and resubmits the same transaction
    let miner = mine_later(&f.chain, 20, 0);
    let s = f.fac.settle("shop", &req);
    miner.join().unwrap();
    assert!(s.success, "{s:?}");
    assert_eq!(f.chain.submit_count(), 2);

    // and an entry that got mined while nobody was watching is finalized by reconcile
    let b = f.fund(500_000_000);
    let (req2, tx2) = f.payment(&[b], 110_000_000, ID2, 9);
    f.fac.set_settle_wait(Duration::from_millis(60));
    assert_eq!(diag(&f.fac.settle("shop", &req2)), "settlement_pending");
    f.chain.mine(0);
    let rep = f.fac.reconcile();
    assert_eq!(rep.finalized, 1);
    let e = f.ledger.get(&txid_hex(&tx2)).unwrap();
    assert_eq!(e.state, State::Accepted);
    assert_eq!(f.fac.settle("shop", &req2).transaction, txid_hex(&tx2));
    assert_eq!(f.chain.submit_count(), 3);
}

// -------------------------------------------------------------------------------------- swap-and-pay

#[test]
fn order_conflict_when_the_node_reports_a_conflict_on_an_order_payment() {
    let f = Fixture::new();
    let order = f.fund(300_000_000);
    let payer_in = f.fund(300_000_000);
    let (req, tx) = f.payment(&[order, payer_in], 100_000_000, ID1, 7);
    *f.verifier.orders.lock().unwrap() = vec![order];
    f.chain.set_submit_failure(Some(SubmitError::Conflict("output already spent by transaction abc in the mempool".into())));
    let s = f.fac.settle("shop", &req);
    assert!(!s.success);
    assert_eq!(s.error_reason.as_deref(), Some("invalid_transaction_state"));
    assert_eq!(diag(&s), "order_conflict");
    assert!(retryable(&s), "the payer re-quotes and re-signs");
    let details = &s.extensions.as_ref().unwrap()["kaspa"]["details"];
    assert_eq!(details["orders"][0]["txid"], hex(&order.txid));
    // the payer's inputs are released
    let e = f.ledger.get(&txid_hex(&tx)).unwrap();
    assert_eq!(e.state, State::Failed);
    assert!(!f.ledger.is_consumed(&order));
    assert!(!f.ledger.is_consumed(&payer_in));
    assert_eq!(e.order_inputs.len(), 1);
}

#[test]
fn order_conflict_when_an_order_is_spent_and_the_transaction_never_gets_accepted() {
    let f = Fixture::new();
    let order = f.fund(300_000_000);
    let payer_in = f.fund(300_000_000);
    let (req, tx) = f.payment(&[order, payer_in], 100_000_000, ID1, 7);
    *f.verifier.orders.lock().unwrap() = vec![order];
    // another taker fills the order after our broadcast
    let spender = after_submit(&f.chain, 15, move |c| c.spend_externally(&order));
    let s = f.fac.settle("shop", &req);
    spender.join().unwrap();
    assert!(!s.success);
    assert_eq!(diag(&s), "order_conflict");
    assert!(retryable(&s));
    let spent = &s.extensions.as_ref().unwrap()["kaspa"]["details"]["orders"];
    assert_eq!(spent.as_array().unwrap().len(), 1);
    assert_eq!(f.ledger.get(&txid_hex(&tx)).unwrap().state, State::Failed);
    assert!(!f.ledger.is_consumed(&payer_in));
}

#[test]
fn an_unspent_order_is_only_pending_and_our_own_acceptance_is_no_conflict() {
    // still-unspent order + no mining: pending, not a conflict
    let f = Fixture::new();
    f.fac.set_settle_wait(Duration::from_millis(60));
    let order = f.fund(300_000_000);
    let payer_in = f.fund(300_000_000);
    let (req, tx) = f.payment(&[order, payer_in], 100_000_000, ID1, 7);
    *f.verifier.orders.lock().unwrap() = vec![order];
    let s = f.fac.settle("shop", &req);
    assert_eq!(diag(&s), "settlement_pending");
    assert_eq!(f.ledger.get(&txid_hex(&tx)).unwrap().state, State::Broadcast);
    // our transaction consumes the order when it is accepted: that is success, not an order conflict
    f.chain.mine(0);
    assert!(f.chain.utxo(&order).is_none());
    let s = f.fac.settle("shop", &req);
    assert!(s.success, "{s:?}");
}

// -------------------------------------------------------------------------------------- kill switch etc.

#[test]
fn kill_switch_refuses_verify_and_settle_and_empties_supported() {
    let f = Fixture::new();
    let a = f.fund(500_000_000);
    let (req, _) = f.payment(&[a], 100_000_000, ID1, 7);
    assert!(!f.fac.supported()["kinds"].as_array().unwrap().is_empty());
    f.fac.set_kill(true);
    assert!(f.fac.supported()["kinds"].as_array().unwrap().is_empty());
    assert!(!f.fac.verify(&req).is_valid);
    let s = f.fac.settle("shop", &req);
    assert!(!s.success);
    assert!(f.ledger.is_empty());
    assert_eq!(f.chain.submit_count(), 0);
    f.fac.set_kill(false);
    assert!(f.fac.verify(&req).is_valid);
}

#[test]
fn kill_switch_file_disables_the_facilitator() {
    let dir = tempfile::tempdir().unwrap();
    let flag = dir.path().join("x402.kill");
    let f = Fixture::new();
    let mut fac = Facilitator::new(
        f.fac.policy.clone(),
        f.chain.clone(),
        f.clock.clone(),
        f.ledger.clone(),
        super::facilitator::FacilitatorConfig { kill_switch_file: Some(flag.clone()), ..Default::default() },
    );
    fac = fac.with_verifier(f.verifier.clone());
    assert!(!fac.killed());
    std::fs::write(&flag, b"1").unwrap();
    assert!(fac.killed());
    std::fs::remove_file(&flag).unwrap();
    assert!(!fac.killed());
}

/// `kob-executor run --pause-file` disables the facilitator like its own kill-switch file; a pause file whose state
/// cannot be read counts as present.
#[test]
fn the_process_pause_file_disables_the_facilitator() {
    let dir = tempfile::tempdir().unwrap();
    let flag = dir.path().join("pause");
    let f = Fixture::new();
    let fac = Facilitator::new(
        f.fac.policy.clone(),
        f.chain.clone(),
        f.clock.clone(),
        f.ledger.clone(),
        super::facilitator::FacilitatorConfig { pause_file: Some(flag.clone()), ..Default::default() },
    );
    assert!(!fac.killed());
    std::fs::write(&flag, b"").unwrap();
    assert!(fac.killed());
    assert!(fac.supported()["kinds"].as_array().unwrap().is_empty());
    std::fs::remove_file(&flag).unwrap();
    assert!(!fac.killed());
    // a path below a regular file cannot be checked: paused
    std::fs::write(dir.path().join("file"), b"").unwrap();
    let fac = Facilitator::new(
        f.fac.policy.clone(),
        f.chain.clone(),
        f.clock.clone(),
        f.ledger.clone(),
        super::facilitator::FacilitatorConfig { kill_switch_file: Some(dir.path().join("file").join("kill")), ..Default::default() },
    );
    assert!(fac.killed());
}

fn facilitator_with(policy: Policy) -> Facilitator {
    let f = Fixture::new();
    Facilitator::new(policy, f.chain.clone(), f.clock.clone(), f.ledger.clone(), Default::default())
}

#[test]
fn supported_advertises_only_what_is_configured() {
    // KAS only
    let fac = facilitator_with(Policy::new(Network::Testnet10));
    let s = fac.supported();
    let kinds = s["kinds"].as_array().unwrap();
    assert_eq!(kinds.len(), 1);
    let k = &kinds[0];
    assert_eq!(k["x402Version"], 2);
    assert_eq!(k["scheme"], "exact");
    assert_eq!(k["network"], "kaspa:testnet-10");
    let extra = &k["extra"];
    assert_eq!(extra["asset"], "KAS");
    assert_eq!(extra["binding"], "kaspa-exact-v2");
    assert_eq!(extra["defaultProfile"], "standard-native");
    assert_eq!(extra["profiles"], serde_json::json!(["standard-native"]));
    assert_eq!(extra["modes"], serde_json::json!(["verify", "settle"]));
    assert!(extra.get("routeBindings").is_none() && extra.get("tokens").is_none());
    assert!(!s.to_string().contains("additive"), "additive is never advertised");

    // with a token
    let mut policy = Policy::new(Network::Testnet10);
    let prog = kob_protocol::artifacts::template(kob_protocol::artifacts::TemplateId::Kcc20Ref);
    policy.tokens.insert(AllowedToken::new([0xaa; 32], prog.id, [0xcc; 32], Custody::IssuerControlled, "TKN", 6)).unwrap();
    let s = facilitator_with(policy.clone()).supported();
    let extra = &s["kinds"][0]["extra"];
    assert_eq!(extra["profiles"], serde_json::json!(["standard-native", "kcc20"]));
    assert_eq!(extra["routeBindings"], serde_json::json!(["kob-swap-v1"]));
    let t = &extra["tokens"][0];
    assert_eq!(t["asset"], "aa".repeat(32));
    assert_eq!(t["templateHash"], hex(&prog.hash));
    assert_eq!(t["extensionCommitment"], "cc".repeat(32));
    assert_eq!(t["custody"], "issuer-controlled");
    assert_eq!(t["ticker"], "TKN");
    assert_eq!(t["decimals"], 6);
    assert!(!s.to_string().contains("additive"));

    // swap disabled: no route binding
    policy.swap_enabled = false;
    let s = facilitator_with(policy).supported();
    assert!(s["kinds"][0]["extra"].get("routeBindings").is_none());
    assert_eq!(s["kinds"][0]["extra"]["profiles"], serde_json::json!(["standard-native", "kcc20"]));
}

#[test]
fn metrics_text_counts_settlements_and_ledger_states() {
    let f = Fixture::new();
    let a = f.fund(500_000_000);
    let (req, _) = f.payment(&[a], 100_000_000, ID1, 7);
    let miner = mine_later(&f.chain, 10, 0);
    assert!(f.fac.settle("shop", &req).success);
    miner.join().unwrap();
    let text = f.fac.metrics_text();
    assert!(text.contains("kob_x402_settle_requests 1"), "{text}");
    assert!(text.contains("kob_x402_settle_success 1"));
    assert!(text.contains("kob_x402_broadcasts 1"));
    assert!(text.contains("kob_x402_ledger_entries{state=\"accepted\"} 1"));
}
