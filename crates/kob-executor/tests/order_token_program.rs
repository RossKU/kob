//! An order's pinned token program must be one the builders can spend it with: the template hash AND the prefix / suffix
//! lengths the state names, of the family its kind trades. A KAS-kind order whose lengths do not match its program decodes,
//! and its placement record encodes, but `kob_protocol::build` refuses every transaction that spends it. Two layers keep such
//! an order from stalling the matcher:
//!
//! * the sanity gate (`kob_executor::sanity::check`) resolves the programs as the builders do
//!   (`kob_protocol::build::order_programs`): the order is never listed or planned;
//! * the engine: a lowering refusal that names the order at fault (`order of leg <i>: ...`) quarantines that order for this
//!   and later ticks and plans the rest again, instead of dropping the plan's honest fills.

#[path = "matcher_common/mod.rs"]
mod common;

use common::*;
use kob_executor::matcher::book::{ListedOrder, MemoryBook};
use kob_executor::matcher::engine::{lowering_leg, tick, TickReport, REFUSED_BY_THE_BUILDER};
use kob_executor::matcher::family::{Families, Family, FamilyAdapter, Kcc20Adapter, Lowered};
use kob_executor::matcher::lower::LowerCtx;
use kob_executor::matcher::planner::Plan;
use kob_protocol::payload::{self, Record};
use kob_protocol::state::*;

/// A plain bid whose token template prefix length does not match its (pinned, supported) token program.
fn wrong_length_bid(maker: u8, price: i64) -> BidState {
    let mut b = bid(maker, price, T3);
    b.tpl_prefix_len += 1;
    b
}

#[test]
fn the_sanity_gate_resolves_the_token_program_like_the_builder() {
    let wrong = AnyState::KobBid(wrong_length_bid(9, P260));
    // the builder refuses the lengths...
    let (pre, suf) = wrong.token_tpl_lens();
    assert!(kob_protocol::build::token_program(&wrong.token_tpl_hash().unwrap(), pre, suf).is_err());
    assert!(kob_protocol::build::order_programs(&wrong).is_err());
    // ...and so does the sanity gate (the order is still indexed: its placement record encodes, its maker can cancel it)
    assert_eq!(kob_executor::sanity::check(&wrong), Err("bad_state:tokenProgram:not_the_pinned_program".into()));
    assert!(payload::encode(&[Record::order(0, &wrong, None, None)]).is_ok());
    // the suffix length too, and every ask-side kind
    let mut a = ask(1, P250, 5 * WHOLE, T8);
    a.tpl_suffix_len -= 1;
    assert!(kob_executor::sanity::check(&AnyState::KobAsk(a)).is_err());
    // a KCC-20 kind that pins a KRON program (and the other way round) is refused: the family follows the kind
    let kron = AnyState::KobBid(BidState {
        extension_commitment: [0; 32],
        ..bid(9, P260, kob_protocol::artifacts::TemplateId::KronToken2433)
    });
    assert!(kob_executor::sanity::check(&kron).is_err(), "a KobBid pinning a KRON program");
    assert_eq!(kob_executor::sanity::check(&kron.clone().into_family(Family::Kron)), Ok(()), "the same state as KobBidKron");
    // honest orders of every kind and both families pass
    for s in [
        AnyState::KobBid(bid(9, P260, T3)),
        AnyState::KobAsk(ask(1, P250, 5 * WHOLE, T8)),
        AnyState::KobCondAsk(cond_ask(1, P260, P245, 5 * WHOLE, T3)),
        AnyState::KobCondBid(cond_bid(1, P250, P255, 5 * WHOLE, T8)),
        AnyState::KobIfdBid(ifd_bid(1, P250, 5 * WHOLE, T3)),
        AnyState::KobIfdAsk(ifd_ask(1, P260, 5 * WHOLE, T8)),
    ] {
        assert_eq!(kob_executor::sanity::check(&s), Ok(()), "{}", s.template_id().name());
    }
}

fn honest_book(extra: Option<ListedOrder>) -> MemoryBook {
    let mut orders = vec![l_bid(3, bid(3, P255, T3), 5 * WHOLE), l_ask(2, ask(1, P250, 5 * WHOLE, T3))];
    orders.extend(extra);
    MemoryBook { daa_score: NOW + 5, orders, wallet_tokens: vec![] }
}

fn matched(r: &TickReport, id: [u8; 32]) -> bool {
    r.prepared.iter().any(|p| p.plan.fills.iter().any(|f| f.cand.id == id))
}

/// A listed wrong-length bid at the best price on top of an honest crossing (ask 2.50 x bid 2.55): the honest pair is matched
/// every tick, the wrong-length bid is quarantined by the sanity gate and never planned.
#[test]
fn a_wrong_length_bid_does_not_block_the_honest_crossing() {
    let base = honest_book(None);
    let r0 = tick(&input(&base), &cfg(), &Families::default(), &signer());
    assert_eq!(r0.prepared.len(), 1, "control: the honest pair matches");

    let book = honest_book(Some(l_bid(9, wrong_length_bid(9, P260), 5 * WHOLE)));
    for t in 0..3 {
        let r = tick(&input(&book), &cfg(), &Families::default(), &signer());
        assert_eq!(r.prepared.len(), 1, "tick {t}: skipped {:?} anomalies {:?}", r.skipped, r.anomalies);
        assert!(matched(&r, cid(2)) && matched(&r, cid(3)), "tick {t}: the honest crossing is matched");
        assert!(!matched(&r, cid(9)), "tick {t}");
        assert!(
            r.quarantined.iter().any(|(id, why)| *id == cid(9) && why.starts_with("bad_state:tokenProgram")),
            "{:?}",
            r.quarantined
        );
        assert!(r.anomalies.is_empty(), "tick {t}: {:?}", r.anomalies);
    }
}

const TOKEN2: [u8; 32] = [0x71; 32];

fn on_token2(mut o: ListedOrder) -> ListedOrder {
    match &mut o.order.state {
        AnyState::KobAsk(s) => s.token_cov_id = TOKEN2,
        AnyState::KobBid(s) => s.token_cov_id = TOKEN2,
        _ => unreachable!(),
    }
    if let Some(c) = o.custody.as_mut() {
        c.utxo.covenant_id = Some(TOKEN2);
    }
    o
}

fn two_books(extra: Option<ListedOrder>) -> MemoryBook {
    let mut b = honest_book(extra);
    b.orders.extend([on_token2(l_bid(13, bid(13, P255, T3), 5 * WHOLE)), on_token2(l_ask(12, ask(11, P250, 5 * WHOLE, T3)))]);
    b
}

/// The planner builds one global batch over every book: a wrong-length bid in one book leaves the crossing of an unrelated
/// book alone.
#[test]
fn a_wrong_length_bid_in_one_book_leaves_the_other_books_alone() {
    let r0 = tick(&input(&two_books(None)), &cfg(), &Families::default(), &signer());
    assert!(matched(&r0, cid(2)) && matched(&r0, cid(12)), "control: both books match");
    let r = tick(&input(&two_books(Some(l_bid(9, wrong_length_bid(9, P260), 5 * WHOLE)))), &cfg(), &Families::default(), &signer());
    assert!(matched(&r, cid(2)) && matched(&r, cid(12)), "both books match: skipped {:?}", r.skipped);
}

/// A KCC-20 adapter whose builder refuses one order on its own, as `kob_protocol::build` names such a refusal: the engine
/// layer under the sanity gate, for a refusal the gate does not know about.
struct Refusing(CovId);
type CovId = [u8; 32];

impl FamilyAdapter for Refusing {
    fn family(&self) -> Family {
        Family::Kcc20
    }
    fn lower(&self, plan: &Plan, cx: &LowerCtx) -> Result<Lowered, String> {
        if let Some(i) = plan.fills.iter().position(|f| f.cand.id == self.0) {
            return Err(format!("invalid request: order of leg {i}: a refusal of this order alone"));
        }
        Kcc20Adapter.lower(plan, cx)
    }
}

#[test]
fn a_builder_refusal_naming_the_order_quarantines_that_order_only() {
    assert_eq!(lowering_leg("lowering: invalid request: order of leg 12: token prefix/suffix lengths"), Some(12));
    assert_eq!(lowering_leg("invalid request: bid at leg 3 is not active before DAA 9"), None);
    assert_eq!(lowering_leg("invalid request: leg 3: the bid cannot afford 5 base units"), None);

    // the refused bid quotes the best price, so every plan takes it first
    let mut fams = Families::default();
    fams.register(Box::new(Refusing(cid(9))));
    let book = two_books(Some(l_bid(9, bid(9, P260, T3), 5 * WHOLE)));
    let r = tick(&input(&book), &cfg(), &fams, &signer());
    assert!(matched(&r, cid(2)) && matched(&r, cid(3)), "the honest crossing of the same book: skipped {:?}", r.skipped);
    assert!(matched(&r, cid(12)) && matched(&r, cid(13)), "the other book");
    assert!(!matched(&r, cid(9)));
    let q: Vec<_> = r.quarantined.iter().filter(|(id, _)| *id == cid(9)).collect();
    assert_eq!(q.len(), 1, "quarantined once: {:?}", r.quarantined);
    assert!(q[0].1.starts_with(REFUSED_BY_THE_BUILDER), "{}", q[0].1);
    assert_eq!(r.quarantined.len(), 1, "no honest order is quarantined: {:?}", r.quarantined);
}
