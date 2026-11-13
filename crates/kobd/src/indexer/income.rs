//! What an executor's transaction left to its sender, and what it paid the miners.
//!
//! The figures feed `executor_txs` and `GET /v1/stats`.
//!
//! # Income
//!
//! An executor is a matcher's batch or a keeper's refund, kill, expiry, arm or trail. It spends KOB
//! orders on terms that their covenants fix. Each order pays its maker, delivers, refunds or
//! continues at outputs the covenant pins. Whatever its inputs release beyond that is the sender's:
//! the crossing spread, auction margins, priority tips and keeper tips (`docs/spec/matcher.md`
//! 3.2). So the **income** of the transaction is exact from the chain alone:
//!
//! ```text
//! income = KAS of the KOB inputs it spent (order UTXOs, their custody carriers and strays)
//!        - KAS of the outputs the orders own or pay
//! ```
//!
//! The outputs the orders own or pay are:
//!
//! - every output that carries an order's covenant id: continuations, if-done exits and placements;
//! - every token output owned by an order id: custodies, a rest and the exit custody of an if-done
//!   buy;
//! - the outputs that each spent order's covenant pins by position ([`pinned`], read from the
//!   contracts in `contracts/orders/`). These are the maker's payout, delivery or refund at the
//!   order's own input index, a custody's return at its custody input index, and a terminal bid's
//!   refund at output `M - 1 - i`.
//!
//! An output the covenants leave free is the sender's: its change, or anything it chose to pay. The
//! income is what the transaction leaves for its sender before the network fee, the same figure
//! whichever outputs the sender kept.
//!
//! # Own trades
//!
//! A transaction in which the sender **trades for itself** (an own trade, `executor_txs.own_trade`)
//! is not counted in the executors' income. The income is thus what traders paid the executors for
//! matching and keeping, never what an executor paid or got for tokens of its own.
//!
//! An own trade is recognised from the record alone, for all of the history. The transaction has
//! either of these:
//!
//! - a covenant input that is neither a tracked order UTXO nor an order's custody or stray, because
//!   the sender's own tokens go in, as in an inventory sale or a taker selling;
//! - a token output that no order owns and no spent order's covenant pins, because tokens come out
//!   to the sender, as in an inventory buy, a taker buying or the rest of a route it keeps.
//!
//! A matcher's crossing, a route or netting between orders, and a keeper's job move tokens only
//! between orders and makers, and count. One case is not told apart: an executor that crosses one
//! of its own orders against a trader's while no token comes to it, because its own payout counts
//! as an order's payout. Records written before payers were recorded have no keys for it. The
//! network fee of an own trade is counted like any other.
//!
//! # Network fee
//!
//! The **network fee** is what the inputs held, from the node's own UTXO entries
//! (`record::TxRecord::in_value`), minus the outputs. Either figure is unknown (`None`) when the
//! chain data does not settle it, and is never estimated.

use super::record::{RecOut, Reveal};
use kob_protocol::state::{quote_of, AnyState, Round, SIDE_ASK};
use kob_protocol::tx::REFUND_MIN;

/// What a spent order input did, as the processor classifies its entry call.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Spend {
    /// A fill of `n` base units, including a repeat exit's take-profit fill.
    Fill(i64),
    /// A repeating entry re-armed by its exit's take-profit fill.
    Merge,
    /// A refund, kill, expiry or close.
    Refund,
    /// A keeper's arm or trail.
    Update,
}

/// One spent order input of an executor's transaction.
#[derive(Debug, Clone, Copy)]
pub struct Leg<'a> {
    /// The order's state as spent (proven: the revealed redeem script hashes to the spent output).
    pub state: &'a AnyState,
    /// What the input did.
    pub spend: Spend,
    /// The input's index.
    pub input: usize,
    /// What the signature script of the input revealed.
    pub reveal: &'a Reveal,
    /// An output of the transaction carries the order's covenant id (it continues).
    pub continues: bool,
    /// The order is a repeat exit whose entry (`parent`) is spent in the same transaction.
    pub parent_spent: bool,
    /// A fill's quote at its time argument (the processor's `fill_quote`).
    ///
    /// It is what a bid pays per whole token before its tip.
    pub quote: Option<i64>,
    /// The KAS of the spent order UTXO.
    pub value: i64,
}

/// The outputs the covenant of a leg pins by position.
///
/// The layouts are in `contracts/orders/*.sil`, and the KRON twins have the same ones. The result
/// leaves out the outputs that the caller adds for every leg: an output carrying an order's
/// covenant id and a token output owned by an order id. It is `None` when the answer is not known:
/// a term is out of range, an argument is missing, the combination of kind and entry is one no
/// covenant allows, or a slot is past the last output.
pub fn pinned(l: &Leg<'_>, outputs: usize) -> Option<Vec<usize>> {
    use AnyState::*;
    let i = l.input;
    let arg = |k: usize| l.reveal.int(k).and_then(|v| usize::try_from(v).ok());
    let mut v = Vec::new();
    match (l.state, l.spend) {
        // `settle(nb, tokenIn, tokOut, t)`: the payout at i. The unsold rest (GTC, owned by the
        // order) or the IOC return (the maker's, at the custody's own index) goes at tokOut. A
        // refund returns the whole custody with every carrier at i.
        (KobAsk(a) | KobAskKron(a), Spend::Fill(n)) => {
            v.push(i);
            if a.amount_left.checked_sub(n)? > 0 {
                v.push(arg(2)?);
            }
        }
        (KobAsk(_) | KobAskKron(_), Spend::Refund) => v.push(i),
        // `settle(nb, tokenIn, tokOut, leg, ...)`: the payout at i. A re-arming take-profit's
        // carriers go to the entry's continuation. The rest goes at tokOut and a refund at i. An
        // update pins only its continuation.
        (KobCondAsk(c) | KobCondAskKron(c), Spend::Fill(n)) => {
            v.push(i);
            if c.amount_left.checked_sub(n)? > 0 {
                v.push(arg(2)?);
            }
        }
        (KobCondAsk(_) | KobCondAskKron(_), Spend::Refund) => v.push(i),
        (KobCondAsk(_) | KobCondAskKron(_), Spend::Update) => {}
        // `settle`: a fill pays into its exit and keeps its rest in custody, both the caller's. A
        // merge re-arms into its custody and continuation. A refund, `settle(0)` with the tokens
        // and `close()` with the carrier, is the maker's at i.
        (KobIfdAsk(_) | KobIfdAskKron(_), Spend::Refund) => v.push(i),
        (KobIfdAsk(_) | KobIfdAskKron(_), Spend::Fill(_) | Spend::Merge | Spend::Update) => {}
        // `fill(nb, tokenTplIn, t)`: the delivery at i. A terminal fill's unspent KAS beyond the
        // delivery carrier goes at M - 1 - i (`refundTo`), from REFUND_MIN up. `refund()`: the
        // maker's at i.
        (KobBid(b) | KobBidKron(b), Spend::Fill(n)) => {
            v.push(i);
            if !l.continues {
                let rem = l.value.checked_sub(b.spend_at(n, l.quote?)?)?.checked_sub(b.delivery_carrier)?;
                if rem >= REFUND_MIN as i64 {
                    v.push(outputs.checked_sub(1 + i)?);
                }
            }
        }
        (KobBid(_) | KobBidKron(_), Spend::Refund) => v.push(i),
        // `settle(nb, tokenTplIn, leg, ...)`: the delivery at i, or the maker's profit at i for a
        // re-arming take-profit. A terminal fill that does not re-arm sends what it did not spend
        // beyond its delivery carrier, from REFUND_MIN up, to M - 1 - i.
        (KobCondBid(c) | KobCondBidKron(c), Spend::Fill(n)) => {
            v.push(i);
            let rearm = l.parent_spent && l.reveal.int(2)? == 0;
            if !rearm && !l.continues {
                let spend = quote_of(n, l.quote?.checked_add(c.tip)?, c.scale, Round::Down)?;
                let rem = l.value.checked_sub(spend)?.checked_sub(c.delivery_carrier)?;
                if rem >= REFUND_MIN as i64 {
                    v.push(outputs.checked_sub(1 + i)?);
                }
            }
        }
        (KobCondBid(_) | KobCondBidKron(_), Spend::Refund) => v.push(i),
        (KobCondBid(_) | KobCondBidKron(_), Spend::Update) => {}
        // `fill`: the bought tokens at i, owned by the exit. `refund()`: the maker's at i.
        (KobIfdBid(_) | KobIfdBidKron(_), Spend::Fill(_) | Spend::Refund) => v.push(i),
        (KobIfdBid(_) | KobIfdBidKron(_), Spend::Merge | Spend::Update) => {}
        // `settle(nb, custIn, tTplIn, t, sOut, tOut)`: the T delivery at i. The custody's rest, the
        // order's, or return, the maker's, goes at custIn while some of it is left (custody - sOut
        // > 0). A refund returns the whole custody at i.
        (KobPair(p), Spend::Fill(_)) => {
            v.push(i);
            if p.custody.checked_sub(l.reveal.int(4)?)? > 0 {
                v.push(arg(1)?);
            }
        }
        (KobPair(_), Spend::Refund) => v.push(i),
        // `settle(nb, custIn, tTplIn, t, sOut, tOut, leg, ..)`: the delivery at i, or a re-arming
        // take-profit's profit. The custody's rest or return goes at custIn while some is left.
        // That is custody - sOut, or for a re-arming BID exit that continues custody - proceeds -
        // back, and one that sells out keeps nothing. A refund goes at i. An update pins only its
        // continuation.
        (KobCondPair(c), Spend::Fill(n)) => {
            v.push(i);
            let rearm = l.parent_spent && l.reveal.int(6)? == 0;
            let left = c.amount_left.checked_sub(n)?;
            let kept = if rearm && c.side != SIDE_ASK {
                if left > 0 {
                    c.custody.checked_sub(c.rpt_proceeds(n)?)?.checked_sub(c.rpt_back(n)?)?
                } else {
                    0
                }
            } else {
                c.custody.checked_sub(l.reveal.int(4)?)?
            };
            if kept > 0 && (left > 0 || !rearm) {
                v.push(arg(1)?);
            }
        }
        (KobCondPair(_), Spend::Refund) => v.push(i),
        (KobCondPair(_), Spend::Update) => {}
        // `fill(nb, aIn, bIn, aTplIn, bTplIn, exitOut, amt, ..)`: the exit's custody at i, owned by
        // the exit. A buy-first entry that ends returns the B it did not spend (custody - amt) at
        // bIn. A refund pays the maker the entry's KAS at i and returns each custody at its own
        // index: A held by a sell-first entry, the B escrow or prefund. A merge and an update pin
        // only the entry's own custodies and continuation.
        (KobIfdPair(e), Spend::Fill(_)) => {
            v.push(i);
            if !l.continues && e.is_buy_first() && e.custody.checked_sub(l.reveal.int(6)?)? > 0 {
                v.push(arg(2)?);
            }
        }
        (KobIfdPair(e), Spend::Refund) => {
            v.push(i);
            if !e.is_buy_first() && e.amount_left > 0 {
                v.push(arg(1)?);
            }
            if e.custody > 0 {
                v.push(arg(2)?);
            }
        }
        (KobIfdPair(_), Spend::Merge | Spend::Update) => {}
        _ => return None,
    }
    v.iter().all(|&j| j < outputs).then_some(v)
}

/// `kob_in - Σ outputs[j]` over the distinct `kob_out` indexes, in sompi.
///
/// It is `None` when an index is past the last output, or the result is outside 64 bits.
pub fn income(kob_in: i128, outputs: &[RecOut], kob_out: &std::collections::BTreeSet<usize>) -> Option<i64> {
    let mut out: i128 = 0;
    for &j in kob_out {
        out += i128::from(outputs.get(j)?.value);
    }
    i64::try_from(kob_in - out).ok()
}

/// The network fee: what the inputs held minus every output.
///
/// It is `None` when the inputs' values are not known, or when they hold less than the outputs,
/// which no accepted transaction can.
pub fn miner_fee(in_value: Option<u64>, outputs: &[RecOut]) -> Option<i64> {
    let out: i128 = outputs.iter().map(|o| i128::from(o.value)).sum();
    let fee = i128::from(in_value?) - out;
    (fee >= 0).then(|| i64::try_from(fee).ok()).flatten()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testkit::{ask, bid, cond_bid, SCALE};
    use kob_protocol::artifacts::TemplateId;

    fn reveal(args: &[i64]) -> Reveal {
        Reveal {
            template: TemplateId::KobAsk,
            state: vec![],
            tag: [0; 4],
            // 8-byte pushes, as the fixed `nb` argument is: a script number up to 8 bytes. These
            // are all >= 0.
            args: args.iter().map(|a| a.to_le_bytes().to_vec()).collect(),
        }
    }

    fn outs(values: &[u64]) -> Vec<RecOut> {
        values.iter().map(|v| RecOut { value: *v, spk: vec![], cov: None }).collect()
    }

    fn leg<'a>(state: &'a AnyState, spend: Spend, input: usize, r: &'a Reveal) -> Leg<'a> {
        Leg { state, spend, input, reveal: r, continues: false, parent_spent: false, quote: None, value: 0 }
    }

    /// An ask's fill pins its payout at its input index, and its custody index only while tokens
    /// are left (rest or IOC return).
    #[test]
    fn an_ask_pins_its_payout_and_its_rest() {
        let mut a = ask(1, 250_000);
        a.amount_left = 10 * SCALE;
        let st = AnyState::KobAsk(a);
        let r = reveal(&[4 * SCALE, 2, 2, 0]);
        assert_eq!(pinned(&leg(&st, Spend::Fill(4 * SCALE), 0, &r), 4), Some(vec![0, 2]));
        // Sold out: the custody index is free, the sender's.
        assert_eq!(pinned(&leg(&st, Spend::Fill(10 * SCALE), 0, &r), 4), Some(vec![0]));
        assert_eq!(pinned(&leg(&st, Spend::Refund, 1, &r), 4), Some(vec![1]));
        // An ask has no update: unknown, never a guess.
        assert_eq!(pinned(&leg(&st, Spend::Update, 1, &r), 4), None);
        // A slot past the last output: unknown.
        assert_eq!(pinned(&leg(&st, Spend::Refund, 4, &r), 4), None);
    }

    /// A terminal bid fill's refund slot, M - 1 - i, is pinned only when what it did not spend
    /// beyond its delivery carrier is REFUND_MIN or more.
    ///
    /// Below that the slot is free and may hold the sender's change.
    #[test]
    fn a_terminal_bid_pins_its_refund_from_one_kas() {
        let b = bid(1, 250_000);
        let (tip, carrier) = (b.tip, b.delivery_carrier);
        let st = AnyState::KobBid(b);
        let r = reveal(&[2 * SCALE, 1, 0]);
        // 2 whole tokens at 240 000 + tip: floor(2 x (240 000 + tip)) sompi. The escrow holds that,
        // the carrier and 1 KAS more.
        let spend = 2 * (240_000 + tip);
        let mut l = leg(&st, Spend::Fill(2 * SCALE), 1, &r);
        l.quote = Some(240_000);
        l.value = spend + carrier + REFUND_MIN as i64;
        assert_eq!(pinned(&l, 5), Some(vec![1, 3]), "the refund at 5 - 1 - 1");
        l.value -= 1;
        assert_eq!(pinned(&l, 5), Some(vec![1]), "a sompi less rides on the delivery");
        l.continues = true;
        l.value += 10 * REFUND_MIN as i64;
        assert_eq!(pinned(&l, 5), Some(vec![1]), "a bid that continues refunds nothing");
        // A terminal fill without its quote: unknown.
        let mut l = leg(&st, Spend::Fill(2 * SCALE), 1, &r);
        l.value = 10 * REFUND_MIN as i64;
        assert_eq!(pinned(&l, 5), None);
    }

    /// A conditional buy's re-arming take-profit pays the maker its profit at i and never a refund.
    /// Otherwise it is as a bid.
    #[test]
    fn a_conditional_bid_refunds_unless_it_rearms() {
        let c = cond_bid(1);
        let (tip, carrier) = (c.tip, c.delivery_carrier);
        let st = AnyState::KobCondBid(c);
        let r = reveal(&[SCALE, 1, 0, 0, 0]);
        let mut l = leg(&st, Spend::Fill(SCALE), 0, &r);
        l.quote = Some(100_000);
        l.value = 100_000 + tip + carrier + 5 * REFUND_MIN as i64;
        assert_eq!(pinned(&l, 3), Some(vec![0, 2]));
        l.parent_spent = true;
        assert_eq!(pinned(&l, 3), Some(vec![0]), "leg 0 next to its entry: a re-arming take-profit");
        let r1 = reveal(&[SCALE, 1, 1, 0, 0]);
        l.reveal = &r1;
        assert_eq!(pinned(&l, 3), Some(vec![0, 2]), "a stop leg never re-arms");
    }

    /// The figures, hand-computed.
    ///
    /// An ask (2 000 000 order UTXO + 1 000 000 custody carrier) and a bid (5 000 000 escrow)
    /// cross. The ask's payout is 1 500 000 + 3 000 000 carriers sold out, and the bid's delivery
    /// is 1 000 000 and its continuation 2 900 000.
    #[test]
    fn income_and_fee_are_exact() {
        let o = outs(&[4_500_000, 1_000_000, 2_900_000, 7_000_000]);
        let kob_in: i128 = 2_000_000 + 1_000_000 + 5_000_000;
        let s: std::collections::BTreeSet<usize> = [0, 1, 2].into();
        // 8 000 000 in, 8 400 000 out to the orders: the sender put 400 000 of its own into this,
        // which is a negative income.
        assert_eq!(income(kob_in, &o, &s), Some(-400_000));
        let s: std::collections::BTreeSet<usize> = [1, 2].into();
        assert_eq!(income(kob_in, &o, &s), Some(4_100_000));
        let s: std::collections::BTreeSet<usize> = [4].into();
        assert_eq!(income(kob_in, &o, &s), None);
        // Inputs 15 403 000 against outputs 15 400 000.
        assert_eq!(miner_fee(Some(15_403_000), &o), Some(3_000));
        assert_eq!(miner_fee(None, &o), None);
        assert_eq!(miner_fee(Some(15_399_999), &o), None, "an input total below the outputs is no fee");
    }
}
