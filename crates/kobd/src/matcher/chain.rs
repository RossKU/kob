//! The next transactions of a tick, for book sets and crossings larger than one transaction.
//!
//! See `docs/spec/matcher.md` §3.4 and §7. After a step is built, [`apply`] predicts the book the
//! next step sees. Closed orders leave. Partially filled orders continue at their predicted
//! outpoints, with the same covenant id and the continuation state the covenant writes. The
//! operator's change becomes the next funding.
//!
//! The next step is planned against that book with the continuations marked *unaccepted*. The
//! mempool reports the DAA score of an unaccepted UTXO as `u64::MAX`, and some covenant paths read
//! the parent's DAA score. So only fills whose covenant path does not read it are allowed. An
//! unaccepted continuation is never triggered, updated or trigger evidence, and takes no merges or
//! TWAP / DCA slices.
//!
//! A mode-1 bid continues with its bought-custody grown at its delivery output, and a topped-up
//! receive UTXO is replaced by its successor in every order of its maker that lists it, so the
//! next step can fill into them.
//!
//! Atomicity holds per transaction, not across a chain: every step is valid on its own. IOC orders
//! never continue, because their remainder returns to the maker in the same transaction. A FOK is
//! either complete in one step or never filled. So neither is ever chained.

use std::collections::{BTreeMap, BTreeSet};

use kob_protocol::state::*;
use kob_protocol::tx::{BuiltTx, KeyUtxo, OrderUtxo, TokenUtxo, Utxo};

use super::book::{outpoint, CovId, ListedOrder, Outpoint};
use super::candidate::token_program;
use super::planner::Plan;

/// The DAA score the mempool reports for outputs of unaccepted transactions.
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

/// The predicted continuation of one filled order, or `None` if it is closed or cannot be chained.
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
        // A mode-1 bid's continuation counts what its bought-custody holds.
        AnyState::KobBid(s) => (AnyState::KobBid(BidState { bought: bought_after(s.mode, s.bought, n), ..s.clone() }), None),
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
                AnyState::KobCondBid(CondBidState {
                    amount_left: rest,
                    armed: s.next_armed(leg as i64, trigger, udaa),
                    bought: bought_after(s.mode, s.bought, n),
                    ..s.clone()
                }),
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
        // A curve continues with amountLeft - n, bound to its custody's token by the fill.
        AnyState::KobCurveAsk(s) => {
            let rest = s.amount_left - n;
            if rest <= 0 {
                return None;
            }
            let next = CurveAskState { amount_left: rest, ..s.clone() };
            (crate::model::bound(&AnyState::KobCurveAsk(next), o.custody.as_ref()?.utxo.covenant_id?), Some(rest))
        }
        // A pair order is not chained: its fill is never chain-safe (`super::pair`).
        AnyState::KobPair(_) | AnyState::KobCondPair(_) | AnyState::KobIfdPair(_) => return None,
        _ => unreachable!("base_state is a KCC-20 kind"),
    };
    let next = next.into_family(o.order.state.family());
    let idx = find_output(built, &spk(&next), Some(id))?;
    let custody = match custody_amount {
        Some(amount) => {
            let c = o.custody.as_ref()?;
            let program = kob_protocol::artifacts::token_template(token_program(&o.order.state)?);
            // The custody holds exactly `amountLeft` base units.
            let st = c.state.with_amount(amount);
            let tidx = find_output(built, &kob_protocol::tx::spk_to_string(&st.spk_with(program)), c.utxo.covenant_id)?;
            Some(TokenUtxo { utxo: out_utxo(built, tidx, c.utxo.covenant_id), state: st })
        }
        None => None,
    };
    // The grown bought-custody, at the bid's delivery output.
    let bought_custody = match next.bought_custody() {
        Some(total) => {
            let program = kob_protocol::artifacts::token_template(token_program(&o.order.state)?);
            let st = TokenState::custody(o.order.state.family(), total, id, o.extension()?);
            let tidx = find_output(built, &kob_protocol::tx::spk_to_string(&st.spk_with(program)), Some(next.token_cov_id()))?;
            Some(TokenUtxo { utxo: out_utxo(built, tidx, Some(next.token_cov_id())), state: st })
        }
        None => None,
    };
    Some(ListedOrder {
        family: o.family,
        order: OrderUtxo { utxo: out_utxo(built, idx, Some(id)), state: next },
        custody,
        custody_b: None,
        bought_custody,
        deadline: o.deadline,
        seen_daa: o.seen_daa,
        foreign: o.foreign.clone(),
        strays: o.strays.clone(),
        receive: o.receive.clone(),
    })
}

/// `bought` after a fill of `n`: grown in mode 1, unchanged otherwise.
fn bought_after(mode: i64, bought: i64, n: i64) -> i64 {
    if mode == MODE_ACCUMULATE {
        bought.saturating_add(n)
    } else {
        bought
    }
}

/// The receive UTXOs a step tops up, each with its successor (`None` when it is not found).
fn topped_up(by_id: &BTreeMap<CovId, ListedOrder>, plan: &Plan, built: &BuiltTx) -> BTreeMap<Outpoint, Option<TokenUtxo>> {
    let spent: BTreeSet<Outpoint> = built.tx.inputs.iter().map(|i| (i.transaction_id, i.index)).collect();
    let mut out = BTreeMap::new();
    for f in &plan.fills {
        let Some(o) = by_id.get(&f.cand.id) else { continue };
        for r in o.receive.iter().filter(|r| spent.contains(&outpoint(&r.utxo))) {
            let Some(program) = token_program(&o.order.state) else { continue };
            let st = r.state.with_amount(r.state.amount().saturating_add(f.amount));
            let spk = kob_protocol::tx::spk_to_string(&st.spk_with(kob_protocol::artifacts::token_template(program)));
            let next = find_output(built, &spk, r.utxo.covenant_id)
                .map(|i| TokenUtxo { utxo: out_utxo(built, i, r.utxo.covenant_id), state: st });
            let e = out.entry(outpoint(&r.utxo)).or_insert(None);
            if e.is_none() {
                *e = next;
            }
        }
    }
    out
}

/// The book after a step.
///
/// Continuations replace their orders and are marked unaccepted. Everything else the step spent
/// leaves, updated orders too: an armed continuation's auction starts at its own DAA score, which
/// is not known yet, so it waits for the next tick. Returns the ids of the continued orders and the
/// operator's next funding.
pub fn apply(
    by_id: &mut BTreeMap<CovId, ListedOrder>,
    plan: &Plan,
    built: &BuiltTx,
    operator: [u8; 32],
) -> (BTreeSet<CovId>, Option<KeyUtxo>) {
    let mut continued = BTreeSet::new();
    let receives = topped_up(by_id, plan, built);
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
    // Every order that lists a topped-up receive UTXO now lists its successor.
    if !receives.is_empty() {
        for o in by_id.values_mut() {
            o.receive = std::mem::take(&mut o.receive)
                .into_iter()
                .filter_map(|r| match receives.get(&outpoint(&r.utxo)) {
                    Some(next) => next.clone(),
                    None => Some(r),
                })
                .collect();
            o.receive.dedup_by(|a, b| outpoint(&a.utxo) == outpoint(&b.utxo));
        }
    }
    let funding = built.fee.change_output.map(|i| KeyUtxo { utxo: out_utxo(built, i as usize, None), pubkey: operator });
    (continued, funding)
}
