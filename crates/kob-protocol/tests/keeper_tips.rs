//! Derives the keeper tips from measured fees, in two tables:
//!
//! * `data/keeper_tips.json`, per token program, for the KAS order kinds: the fee floor of every keeper transaction shape
//!   (refunds, IOC / FOK kills, the close of an empty repeating entry) built as a funded keeper with its own change output
//!   (the costlier shape), and, for arm and trail updates of conditionals and stop entries (always inside a matcher's
//!   batch, next to the evidence fill), the MARGINAL fee the update adds to that batch (its input and continuation);
//! * `data/pair_keeper_tips.json`, per program PAIR (`<A>+<B>`), for the pair kinds (`KobPair`, `KobCondPair`,
//!   `KobIfdPair`): the same over the pair shapes on that pair (refunds and kills of every pair kind in every custody
//!   state: an entry's two custodies, A only, a waiting entry's B rest only, nothing held; arm and trail updates of both
//!   sides in both evidence modes, `common::pair::pair_refund_grid` / `pair_update_grid`). The pair kinds never raise the
//!   KAS kinds' tips.
//!
//! The default tip is twice the largest fee, rounded up to 0.001 KAS ([`kob_protocol::defaults::tip_for_fee`]).
//! `KOB_REGEN=1` rewrites both files; otherwise the committed tables must match.

mod common;

use std::collections::BTreeMap;

use kob_protocol::artifacts::TemplateId;
use kob_protocol::build::{build, Action};
use kob_protocol::defaults::{pair_tips_key, pair_tips_table, tip_for_fee, tips_table, Tips};
use kob_protocol::tx::{finalize, sign_locally, FinalizeOptions};
use kob_protocol::verify::validate_signed;

/// The largest refund fee and the largest marginal update fee over `scenarios` (`label` names them in errors).
fn fees(label: &str, scenarios: Vec<(String, Action)>) -> (u64, u64, usize) {
    let keys = common::keys();
    let (mut refund_fee, mut update_fee, mut refunds) = (0u64, 0u64, 0usize);
    for (name, action) in scenarios {
        let funded = match action {
            Action::RefundOrder(mut r) => {
                r.funding = vec![common::key_utxo(250, common::KEEPER, 5 * common::KAS)];
                r.change = Some(common::pk(common::KEEPER));
                Action::RefundOrder(r)
            }
            Action::Batch(b) if !b.updates.is_empty() => {
                // the marginal fee of the updates: the batch with them minus the batch without them
                let bare = kob_protocol::build::Batch { updates: vec![], ..b.clone() };
                let with = build(&Action::Batch(b.clone())).unwrap_or_else(|e| panic!("{name}@{label}: {e}"));
                let without = build(&Action::Batch(bare)).unwrap_or_else(|e| panic!("{name}@{label}: bare: {e}"));
                let signed = finalize(&with, &sign_locally(&with, &keys).unwrap(), FinalizeOptions::default()).unwrap();
                validate_signed(&signed).unwrap_or_else(|e| panic!("{name}@{label}: {e}"));
                let per = (with.fee.min_fee - without.fee.min_fee).div_ceil(b.updates.len() as u64);
                update_fee = update_fee.max(per);
                continue;
            }
            _ => continue,
        };
        let built = build(&funded).unwrap_or_else(|e| panic!("{name}@{label}: {e}"));
        // The measured shape itself must be valid.
        let signed = finalize(&built, &sign_locally(&built, &keys).unwrap(), FinalizeOptions::default()).unwrap();
        validate_signed(&signed).unwrap_or_else(|e| panic!("{name}@{label}: {e}"));
        refund_fee = refund_fee.max(built.fee.min_fee);
        refunds += 1;
    }
    assert!(refund_fee > 0 && update_fee > 0, "{label}: no keeper shapes measured");
    (refund_fee, update_fee, refunds)
}

fn tips_of(refund_fee: u64, update_fee: u64) -> Tips {
    Tips { refund_fee, refund_tip: tip_for_fee(refund_fee), update_fee, keeper_tip: tip_for_fee(update_fee) }
}

/// The KAS kinds' table (per program) and the pair kinds' table (per program pair).
fn measure() -> (BTreeMap<String, Tips>, BTreeMap<String, Tips>) {
    let mut kas = BTreeMap::new();
    for p in common::PROGRAMS {
        let (r, u, _) = fees(p.name(), common::scenarios_on(p));
        kas.insert(p.name().to_string(), tips_of(r, u));
    }
    let mut pair = BTreeMap::new();
    for pa in common::PROGRAMS {
        for pb in common::PROGRAMS {
            let key = pair_tips_key(pa, pb);
            // the scenarios plus the refund of every custody state and every update (side x arm / trail x evidence mode)
            let mut shapes = common::pair::pair_scenarios(pa, pb);
            shapes.extend(common::pair::pair_refund_grid(pa, pb));
            shapes.extend(common::pair::pair_update_grid(pa, pb));
            let (r, u, refunds) = fees(&key, shapes);
            assert_eq!(refunds, 9 + 21, "{key}: the pair refunds and kills");
            pair.insert(key, tips_of(r, u));
        }
    }
    (kas, pair)
}

#[test]
fn keeper_tips_are_derived_from_measured_fees() {
    let (kas, pair) = measure();
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("data");
    if std::env::var("KOB_REGEN").is_ok_and(|v| v == "1") {
        for (file, table) in [("keeper_tips.json", &kas), ("pair_keeper_tips.json", &pair)] {
            let mut json = serde_json::to_string_pretty(table).unwrap();
            json.push('\n');
            std::fs::write(dir.join(file), json).unwrap();
            println!("wrote {} entries to {file}", table.len());
        }
        return;
    }
    assert_eq!(tips_table(), &kas, "keeper tips are stale: run `KOB_REGEN=1 cargo test -p kob-protocol --test keeper_tips`");
    assert_eq!(
        pair_tips_table(),
        &pair,
        "pair keeper tips are stale: run `KOB_REGEN=1 cargo test -p kob-protocol --test keeper_tips`"
    );
    // A keeper without capital (no funding, no change: the whole tip is the fee) is covered too:
    // every refund scenario of the golden set runs that way on every program, and every update rides in a
    // matcher's batch (tests/builders.rs validates them in the engine).
}

/// The pair kinds have their own tips: a sell-first entry's refund (two custodies) is priced in the pair table only, never in
/// the KAS kinds' table, and `tips_for` selects the table by the order's kind.
#[test]
fn pair_kinds_have_their_own_tips() {
    let p = TemplateId::Kcc20Ref8x8;
    let pair = kob_protocol::defaults::pair_tips(p, p).unwrap();
    let kas = kob_protocol::defaults::tips(p).unwrap();
    assert!(pair.refund_fee > kas.refund_fee, "{pair:?} {kas:?}");
    let e = common::pair::ifd_pair(common::MAKER_A, false, p, p);
    let s = kob_protocol::state::AnyState::KobIfdPair(e);
    assert_eq!(kob_protocol::defaults::tips_for(&s).unwrap(), pair);
    let a = kob_protocol::state::AnyState::KobAsk(common::ask(common::MAKER_A, common::P250, p));
    assert_eq!(kob_protocol::defaults::tips_for(&a).unwrap(), kas);
}
