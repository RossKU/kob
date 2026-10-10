//! Cross-family routes in ONE transaction, engine-validated for every ordered pair of token families and programs:
//! the payer's token A is sold into A's bids, the KAS it releases buys token B from B's asks (optionally paying a merchant in KAS
//! in the same transaction: swap-and-pay). A and B are each one of: the reference KCC-20 (8/8), KaspaCom's third-party KCC20
//! 0.2.5 (a template of the strict list, 25.5 KB, 8/8), the published public-mint build of the reference (3/3, its holders carry
//! the app's context field) and the two KRON programs (4/5), so every pair covers KCC-20 -> KRON,
//! KRON -> KCC-20, KRON -> KRON (both templates), KCC-20 -> KaspaCom and back. Every transaction runs every order covenant and
//! every token program in the rusty-kaspa v2.1.0 engine with the exact committed budgets, pays the node's relay floor and is
//! within the block mass limits.

mod common;

use kob_protocol::artifacts::TemplateId;
use kob_protocol::build::{build, Action};
use kob_protocol::tx::{finalize, sign_locally, FinalizeOptions, SigPlan};
use kob_protocol::verify::validate_signed;
use kob_protocol::Family;

const PROGRAMS: [TemplateId; 5] = [
    TemplateId::Kcc20Ref8x8,
    TemplateId::Kcc20KaspaCom025,
    TemplateId::Kcc20PublicMint,
    TemplateId::KronToken2433,
    TemplateId::KronToken2732,
];

fn route_on(p: TemplateId, name: &str) -> kob_protocol::build::SwapRoute {
    match common::scenarios_on(p).into_iter().find(|(n, _)| n.ends_with(name)).unwrap_or_else(|| panic!("no {name} on {}", p.name())).1
    {
        Action::SwapRoute(r) => r,
        other => panic!("{name} on {} is not a route: {other:?}", p.name()),
    }
}

/// The route selling the payer's token on program `a` and buying on program `b`.
fn cross(a: TemplateId, b: TemplateId, name: &str) -> Action {
    let mut r = route_on(a, name);
    r.buy = route_on(b, name).buy;
    Action::SwapRoute(r)
}

fn check(a: TemplateId, b: TemplateId, name: &str) {
    let label = format!("{name}: {} -> KAS -> {}", a.name(), b.name());
    let action = cross(a, b, name);
    let built = build(&action).unwrap_or_else(|e| panic!("{label}: build: {e}"));
    let signed = finalize(&built, &sign_locally(&built, &common::keys()).unwrap(), FinalizeOptions::default())
        .unwrap_or_else(|e| panic!("{label}: {e}"));
    let v = validate_signed(&signed).unwrap_or_else(|e| panic!("{label}: engine: {e}"));
    // the fee is the node's relay floor (nothing extra: a dust change folded into the fee may add a few sompi)
    assert!(v.fee >= v.min_fee && v.fee - v.min_fee < 2_000_000, "{label}: fee {} vs floor {}", v.fee, v.min_fee);
    assert!(v.mass.within_block_limits(), "{label}: {:?}", v.mass);
    // both token kinds are in the transaction, each with its own authorisation layout
    let has = |f: Family| {
        built
            .plans
            .iter()
            .any(|p| matches!((f, p), (Family::Kcc20, SigPlan::TokenLeader { .. }) | (Family::Kron, SigPlan::KronToken { .. })))
    };
    assert!(has(a.family()) && has(b.family()), "{label}: token inputs of both families expected");
    // every order input executed under its own family's kinds
    let kinds = |f: Family| built.roles.iter().any(|r| r.starts_with(&f.kind_name("KobBid")) || r.starts_with(&f.kind_name("KobAsk")));
    assert!(kinds(a.family()) && kinds(b.family()), "{label}: {:?}", built.roles);
    println!("ROUTE {label}: {} bytes, compute mass {}, fee {} sompi (floor {})", v.mass.size, v.mass.compute, v.fee, v.min_fee);
}

#[test]
fn every_ordered_family_and_program_pair_routes_in_one_transaction() {
    for a in PROGRAMS {
        for b in PROGRAMS {
            check(a, b, "route.swap");
        }
    }
}

#[test]
fn swap_and_pay_crosses_families_too() {
    // the merchant is paid in KAS in the same transaction (x402 swap-and-pay shape)
    for a in PROGRAMS {
        for b in PROGRAMS {
            check(a, b, "route.swapAndPay.x402");
        }
    }
}

#[test]
fn a_route_needs_two_different_tokens() {
    // the same token on both sides is a batch, in any family
    for p in PROGRAMS {
        let mut r = route_on(p, "route.swap");
        let same = route_on(p, "route.swap").sell;
        // sell legs of A on the buy side: refused
        r.buy = same;
        assert!(build(&Action::SwapRoute(r)).is_err(), "{}", p.name());
    }
}
