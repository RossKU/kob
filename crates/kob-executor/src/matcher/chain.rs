//! The next transactions of a tick: book sets and crossings larger than one transaction (`docs/spec/matcher.md` §3.4, §7).
//!
//! After a step is built, [`apply`] predicts the book the next step sees: closed orders leave,
//! partially filled orders continue at their predicted outpoints (same covenant id, the
//! continuation state the covenant writes), the operator's change becomes the next funding. The
//! next step is then planned against that book with the continuations marked *unaccepted*: only
//! fills whose covenant path does not read the parent's DAA score are allowed (the mempool reports
//! an unaccepted UTXO's DAA as `u64::MAX`): an unaccepted continuation is never triggered, updated or trigger
//! evidence (the covenants read its DAA score), and takes no merges or TWAP / DCA slices.
//!
//! Atomicity holds per transaction, not across a chain: every step is valid on its own. IOC orders
//! never continue (their remainder returns to the maker in the same transaction) and a FOK is
//! either complete in one step or never filled, so neither is ever chained.

use std::collections::{BTreeMap, BTreeSet};

use kob_protocol::state::*;
use kob_protocol::tx::{BuiltTx, KeyUtxo, OrderUtxo, TokenUtxo, Utxo};

use super::book::{CovId, ListedOrder};
use super::candidate::token_program;
use super::planner::Plan;

/// DAA score the mempool reports for outputs of unaccepted transactions.
pub const UNACCEPTED_DAA: u64 = u64::MAX;

fn out_utxo(built: &BuiltTx, idx: usize, cov: Option<[u8; 32]>) -> Utxo {
    Utxo {
        transaction_id: built.tx.id,
        index: idx as u32,
        amount: built.tx.outputs[idx].value,
        block_daa_score: UNACCEPTED_DAA,
        covenant_id: cov,
    }
}

fn find_output(built: &BuiltTx, spk: &str, cov: Option<[u8; 32]>) -> Option<usize> {
    built.tx.outputs.iter().position(|o| o.script_public_key == spk && o.covenant.as_ref().map(|c| c.covenant_id) == cov)
}

/// Predicted continuation of one filled order (None: closed, or not predictable for chaining).
fn continuation(o: &ListedOrder, n: i64, leg: u8, trigger: bool, merge: bool, built: &BuiltTx) -> Option<ListedOrder> {
    let id = o.id();
    let udaa = o.utxo_daa() as i64;
    let spk = |s: &AnyState| kob_protocol::tx::spk_to_string(&s.spk());
    let (next, custody_amount): (AnyState, Option<i64>) = match &o.base_state() {
        AnyState::KobAsk(s) => {
            let rest = s.amount_left - n;
            if rest <= 0 || s.tif != TIF_GTC {
                return None;
            }
            (AnyState::KobAsk(AskState { amount_left: rest, ..s.clone() }), Some(rest))
        }
        AnyState::KobBid(s) => (AnyState::KobBid(s.clone()), None),
        AnyState::KobCondAsk(s) => {
            let rest = s.amount_left - n;
            if rest <= 0 || merge || trigger || s.armed == 1 {
                return None;
            }
            (
                AnyState::KobCondAsk(CondAskState { amount_left: rest, armed: s.next_armed(leg as i64, trigger, udaa), ..s.clone() }),
                Some(rest),
            )
        }
        AnyState::KobCondBid(s) => {
            let rest = s.amount_left - n;
            if rest <= 0 || merge || trigger || s.armed == 1 {
                return None;
            }
            (
                AnyState::KobCondBid(CondBidState { amount_left: rest, armed: s.next_armed(leg as i64, trigger, udaa), ..s.clone() }),
                None,
            )
        }
        AnyState::KobIfdBid(s) => {
            let rest = s.amount_left - n;
            if rest <= 0 || s.entry_stop > 0 || s.rpt_amount > 0 {
                return None;
            }
            (AnyState::KobIfdBid(IfdBidState { amount_left: rest, ..s.clone() }), None)
        }
        AnyState::KobIfdAsk(s) => {
            let rest = s.amount_left - n;
            if rest <= 0 || s.entry_stop > 0 || s.rpt_amount > 0 {
                return None;
            }
            (AnyState::KobIfdAsk(IfdAskState { amount_left: rest, ..s.clone() }), Some(rest))
        }
        // a pair order's continuation is not chained: its fill is never chain-safe (`super::pair`)
        AnyState::KobPair(_) | AnyState::KobCondPair(_) | AnyState::KobIfdPair(_) => return None,
        _ => unreachable!("base_state is a KCC-20 kind"),
    };
    let next = next.into_family(o.order.state.family());
    let idx = find_output(built, &spk(&next), Some(id))?;
    let custody = match custody_amount {
        Some(amount) => {
            let c = o.custody.as_ref()?;
            let program = kob_protocol::artifacts::token_template(token_program(&o.order.state)?);
            // the custody holds exactly `amountLeft` base units
            let st = c.state.with_amount(amount);
            let tidx = find_output(built, &kob_protocol::tx::spk_to_string(&st.spk_with(program)), c.utxo.covenant_id)?;
            Some(TokenUtxo { utxo: out_utxo(built, tidx, c.utxo.covenant_id), state: st })
        }
        None => None,
    };
    Some(ListedOrder {
        family: o.family,
        order: OrderUtxo { utxo: out_utxo(built, idx, Some(id)), state: next },
        custody,
        custody_b: None,
        deadline: o.deadline,
        seen_daa: o.seen_daa,
        foreign: o.foreign.clone(),
        strays: o.strays.clone(),
    })
}

/// The book after a step: continuations replace their orders (marked unaccepted), everything else
/// the step spent leaves (updated orders too: an armed continuation's auction starts at its own,
/// still unknown, DAA score, so it waits for the next tick). Returns the ids of the continued orders
/// and the operator's next funding.
pub fn apply(
    by_id: &mut BTreeMap<CovId, ListedOrder>,
    plan: &Plan,
    built: &BuiltTx,
    operator: [u8; 32],
) -> (BTreeSet<CovId>, Option<KeyUtxo>) {
    let mut continued = BTreeSet::new();
    for f in &plan.fills {
        let Some(o) = by_id.remove(&f.cand.id) else { continue };
        if let Some(e) = f.cand.merge {
            by_id.remove(&e);
        }
        if let Some(c) = continuation(&o, f.amount, f.cand.leg, f.evidence.is_some(), f.cand.merge.is_some(), built) {
            continued.insert(c.id());
            by_id.insert(c.id(), c);
        }
    }
    for u in &plan.updates {
        by_id.remove(&u.id);
    }
    let funding = built.fee.change_output.map(|i| KeyUtxo { utxo: out_utxo(built, i as usize, None), pubkey: operator });
    (continued, funding)
}
