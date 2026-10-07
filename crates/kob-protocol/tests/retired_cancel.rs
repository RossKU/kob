//! C5-02: a live order of a RETIRED template can always be cancelled by its maker (`kob_protocol::retired`).
//!
//! Every retired template but one is a lot template (protocol v2.3 to v2.6; its state is a `retired::lot::LotState`); the
//! protocol v3 cross limit without lots (360 bytes, token A of either family) has its own layout. Cases: the
//! cross limit before the v2.6 pair-market auction (333-byte state, retired 2026-10-01) on every program pair, the if-done
//! entries retired on 2026-10-02 (the v2.6 lot layout), the v3 cross limit for token A and B of every family, and then
//! EVERY retired template this build can spend (53: every
//! kind of both families pinned by a testnet-10 build since the real-wallet runs of 2026-09-29, the receipt era and its
//! deployment build included, the first cross limit of b75b3e5, the fourteen protocol v2.6 lot templates retired by
//! the v3 no-lot revision, the v3 cross limit, and the two protocol v3 sell-first entries retired 2026-10-06 with today's
//! layout): the maker's cancel of a live order, with its custody (`lotsLeft x lotUnits x unit`), strays of
//! its token(s) and a foreign stray, is built from the retired artifact and validated in the rusty-kaspa v2.1.0 engine on
//! the token programs of its family; another key, another entry, a wrong state length, a state of another kind and an
//! auction under a pre-auction template are refused.

mod common;

use common::pair::*;
use common::retired_orders::*;
use common::*;
use kob_protocol::artifacts::{template, TemplateId};
use kob_protocol::build::{build_cancel_retired, CancelRetired, ForeignStrays, TokenRef};
use kob_protocol::retired::lot::{self, LotState};
use kob_protocol::retired::{self, Retired, RetiredState};
use kob_protocol::tx::{finalize, sighash, sign_digest, sign_locally, FinalizeOptions, SigPlan};
use kob_protocol::verify::{execute, validate_signed};
use kob_protocol::Family;

const PROGRAMS: [TemplateId; 4] =
    [TemplateId::Kcc20Ref8x8, TemplateId::Kcc20KaspaCom025, TemplateId::KronToken2433, TemplateId::KronToken2732];

fn budgets(role: &str) -> kob_protocol::error::Result<u16> {
    kob_protocol::budget::lookup(role)
}

/// The 333-byte cross limit template (before the pair-market auction) of the family of `pa`, a live order of it (no
/// auction) and its state span.
fn retired_cross(pa: TemplateId, pb: TemplateId) -> (&'static Retired, Vec<u8>, LotState) {
    let r = retired::retired()
        .iter()
        .find(|r| r.kind == TemplateId::KobCross && r.family == pa.family() && r.template.state_len == 333)
        .expect("a pre-auction retired cross of the family");
    let st = live_order(r, pa, pb);
    let span = retired::encode(r, &st).unwrap();
    assert_eq!(span.len(), r.template.state_len);
    assert_eq!(retired::decode(r, &span).unwrap(), st, "the retired span decodes back to the same order");
    (r, span, st)
}

fn request(pa: TemplateId, pb: TemplateId, strays: bool) -> CancelRetired {
    let (r, span, st) = retired_cross(pa, pb);
    CancelRetired {
        template_hash: r.template.hash,
        state: span,
        order: utxo(70, PV, 1_000, Some(X_ID)),
        custody: Some(tutxo(pa, TOKEN_COV, 71, st.custody_amount().unwrap(), X_ID, true)),
        strays: if strays { vec![tutxo(pa, TOKEN_COV, 76, 3, X_ID, true), tutxo(pb, TOKEN_B, 77, 5, X_ID, true)] } else { vec![] },
        foreign: vec![],
        funding: vec![key_utxo(3, MAKER_A, 10 * KAS)],
        change: None,
        fee: fee(),
    }
}

#[test]
fn the_maker_cancels_a_cross_limit_of_the_retired_template_for_every_program_pair() {
    for pa in PROGRAMS {
        for pb in PROGRAMS {
            for strays in [false, true] {
                let label = format!("{} strays {strays}", pair_name(pa, pb));
                let req = request(pa, pb, strays);
                let built = build_cancel_retired(&req, &budgets).unwrap_or_else(|e| panic!("{label}: build: {e}"));
                // the order input spends the RETIRED script (not today's template)
                let SigPlan::Retired { template_hash, .. } = &built.plans[0] else { panic!("{label}: {:?}", built.plans[0]) };
                assert_eq!(*template_hash, req.template_hash);
                assert!(
                    kob_protocol::artifacts::try_template(TemplateId::KobCross).is_none(),
                    "the cross limit kind has no pinned template"
                );
                let signed = finalize(&built, &sign_locally(&built, &keys()).unwrap(), FinalizeOptions::default())
                    .unwrap_or_else(|e| panic!("{label}: finalize: {e}"));
                let v = validate_signed(&signed).unwrap_or_else(|e| panic!("{label}: engine: {e}\nroles {:?}", built.roles));
                assert!(v.mass.within_block_limits(), "{label}: {:?}", v.mass);
                // the custody (lotsLeft x lotUnits x unit) and the strays of both tokens go back to the maker
                let token_outs = signed.tx.outputs.iter().filter(|o| o.covenant.is_some()).count();
                assert_eq!(token_outs, if strays { 2 } else { 1 }, "{label}");
            }
        }
    }
}

#[test]
fn only_the_maker_can_cancel_and_nothing_but_the_cancel_is_built() {
    let (pa, pb) = (TemplateId::Kcc20Ref8x8, TemplateId::Kcc20Ref8x8);
    let req = request(pa, pb, false);
    let built = build_cancel_retired(&req, &budgets).unwrap();
    // signed by another key: the retired covenant refuses it
    let signed = finalize(&built, &sign_locally(&built, &keys()).unwrap(), FinalizeOptions::default()).unwrap();
    let (tx, entries) = signed.tx.to_tx().unwrap();
    let thief = sign_digest(&sk(MAKER_B), &sighash(&tx, &entries, 0)).unwrap();
    let mut bad = tx.clone();
    bad.inputs[0].signature_script = built.plans[0].sigscript(Some(&thief)).unwrap();
    assert!(execute(&bad, &entries, true).unwrap()[0].is_err(), "a cancel signed by another key must fail");
    // a template this build does not keep, and a state that is not the retired layout, are refused
    let mut bad = req.clone();
    bad.template_hash = template(TemplateId::KobPair).hash;
    assert!(build_cancel_retired(&bad, &budgets).is_err());
    let mut bad = req.clone();
    bad.state.push(0);
    assert!(build_cancel_retired(&bad, &budgets).is_err());
    // a custody that is not exactly lotsLeft x lotUnits x unit is refused (a stray is never custody)
    let mut bad = req.clone();
    bad.custody = Some(tutxo(pa, TOKEN_COV, 71, req.custody.as_ref().unwrap().state.amount() - 1, X_ID, true));
    assert!(build_cancel_retired(&bad, &budgets).is_err());
    // the plan is spend-only: another entry of the retired template does not pass `check`
    let SigPlan::Retired { template_hash, state, .. } = built.plans[0].clone() else { unreachable!() };
    let fill = SigPlan::Retired { template_hash, state, entry: "settle".into(), args: vec![] };
    assert!(fill.check().is_err());
}

// ------------------------------------------------------------------------------------------------ if-done entries

const ENTRY_ID: [u8; 32] = [0x46; 32];

/// A repeating stop entry of each if-done kind (armed, some lots in exits) and an empty repeating sell-first entry, under
/// the if-done templates retired on 2026-10-02 of the family of `p`.
fn retired_entries(p: TemplateId) -> Vec<(&'static Retired, Vec<u8>, LotState)> {
    let early = |kind: TemplateId| {
        retired::retired()
            .iter()
            .find(|r| r.kind.base() == kind && r.family == p.family() && r.note.contains("(69c9014)"))
            .expect("an if-done entry retired on 2026-10-02")
    };
    let (rb, ra) = (early(TemplateId::KobIfdBid), early(TemplateId::KobIfdAsk));
    let ib = live_order(rb, p, p);
    let ia = live_order(ra, p, p);
    let LotState::KobIfdAsk(a) = &ia else { unreachable!() };
    let empty = LotState::KobIfdAsk(lot::IfdAskState { lots_left: 0, ..a.clone() });
    [(rb, ib), (ra, ia), (ra, empty)]
        .into_iter()
        .map(|(r, st)| {
            let span = retired::encode(r, &st).unwrap();
            // the v2.6 lot layout: the span of the protocol v2.6 template of the kind is the same bytes
            let v26 = retired::payload_layout(r.family, r.kind.kind_code().unwrap()).unwrap();
            assert_eq!(retired::encode(v26, &st).unwrap(), span, "same layout as the v2.6 lot template");
            assert_eq!(retired::decode(r, &span).unwrap(), st);
            (r, span, st)
        })
        .collect()
}

#[test]
fn the_maker_cancels_if_done_entries_of_the_retired_templates_for_every_program() {
    for p in PROGRAMS {
        for (r, span, st) in retired_entries(p) {
            for strays in [false, true] {
                let label = format!("{} amount {:?} @{} strays {strays}", r.kind_name(), st.custody_amount(), p.name());
                let custody = match st.custody_amount() {
                    Some(a) if a > 0 => Some(tutxo(p, TOKEN_COV, 71, a, ENTRY_ID, true)),
                    _ => None,
                };
                let req = CancelRetired {
                    template_hash: r.template.hash,
                    state: span.clone(),
                    order: utxo(70, 50 * KAS, 1_000, Some(ENTRY_ID)),
                    custody,
                    strays: if strays { vec![tutxo(p, TOKEN_COV, 76, 3, ENTRY_ID, true)] } else { vec![] },
                    // a foreign stray (another token id) goes back with the cancel too
                    foreign: if strays {
                        vec![ForeignStrays {
                            token: TokenRef { covenant_id: [0x72; 32], program: p },
                            utxos: vec![tutxo(p, [0x72; 32], 80, 9, ENTRY_ID, true)],
                        }]
                    } else {
                        vec![]
                    },
                    funding: vec![key_utxo(3, MAKER_A, 10 * KAS)],
                    change: None,
                    fee: fee(),
                };
                let built = build_cancel_retired(&req, &budgets).unwrap_or_else(|e| panic!("{label}: build: {e}"));
                let SigPlan::Retired { template_hash, .. } = &built.plans[0] else { panic!("{label}: {:?}", built.plans[0]) };
                assert_ne!(*template_hash, template(r.kind).hash, "{label}: the order input spends the retired script");
                let signed = finalize(&built, &sign_locally(&built, &keys()).unwrap(), FinalizeOptions::default())
                    .unwrap_or_else(|e| panic!("{label}: finalize: {e}"));
                let v = validate_signed(&signed).unwrap_or_else(|e| panic!("{label}: engine: {e}\nroles {:?}", built.roles));
                assert!(v.mass.within_block_limits(), "{label}: {:?}", v.mass);
            }
        }
    }
}

// ------------------------------------------------------------------------------------------------ every retired template

#[test]
fn the_maker_cancels_a_live_order_of_every_retired_template() {
    let mut validated = 0;
    for r in retired::retired() {
        for (p, pb) in program_pairs(r, &PROGRAMS) {
            for strays in [false, true] {
                let label = format!("{} {} ({}) {} strays {strays}", r.kind_name(), &r.hash_hex()[..8], r.note, pair_name(p, pb));
                let (req, _) = cancel_request(r, p, pb, strays);
                let built = build_cancel_retired(&req, &budgets).unwrap_or_else(|e| panic!("{label}: build: {e}"));
                // the order input spends the RETIRED script, through its cancel
                let SigPlan::Retired { template_hash, entry, .. } = &built.plans[0] else { panic!("{label}: {:?}", built.plans[0]) };
                assert_eq!(*template_hash, r.template.hash, "{label}");
                assert_eq!(entry, "cancel", "{label}");
                let signed = finalize(&built, &sign_locally(&built, &keys()).unwrap(), FinalizeOptions::default())
                    .unwrap_or_else(|e| panic!("{label}: finalize: {e}"));
                let v = validate_signed(&signed).unwrap_or_else(|e| panic!("{label}: engine: {e}\nroles {:?}", built.roles));
                assert!(v.mass.within_block_limits(), "{label}: {:?}", v.mass);
                validated += 1;
            }
        }
    }
    // 44 templates of the six other kinds on the 2 programs of their family (42 lot templates and the two v3 sell-first
    // entries with today's layout), 8 lot cross limits on 2 x 4 program pairs, the v3 cross limit on 4 x 4
    assert_eq!(retired::retired().len(), 53);
    let crosses = retired::retired().iter().filter(|r| r.kind == TemplateId::KobCross).count();
    assert_eq!(crosses, 9);
    assert_eq!(validated, (44 * 2 + 8 * 8 + 16) * 2);
}

#[test]
fn every_retired_template_refuses_another_key_another_entry_and_a_wrong_state() {
    for r in retired::retired() {
        let label = format!("{} {} ({})", r.kind_name(), &r.hash_hex()[..8], r.note);
        let p = PROGRAMS.into_iter().find(|p| p.family() == r.family).unwrap();
        let (req, st) = cancel_request(r, p, p, false);
        let built = build_cancel_retired(&req, &budgets).unwrap();
        // signed by another key: the retired covenant refuses it
        let signed = finalize(&built, &sign_locally(&built, &keys()).unwrap(), FinalizeOptions::default()).unwrap();
        let (tx, entries) = signed.tx.to_tx().unwrap();
        assert!(execute(&tx, &entries, true).unwrap()[0].is_ok(), "{label}: the maker's cancel passes");
        let thief = sign_digest(&sk(MAKER_B), &sighash(&tx, &entries, 0)).unwrap();
        let mut bad = tx.clone();
        bad.inputs[0].signature_script = built.plans[0].sigscript(Some(&thief)).unwrap();
        assert!(execute(&bad, &entries, true).unwrap()[0].is_err(), "{label}: a cancel signed by another key must fail");
        // a state one byte longer or shorter than the retired layout, or a state of another kind, is refused
        let mut long = req.clone();
        long.state.push(0);
        assert!(build_cancel_retired(&long, &budgets).is_err(), "{label}");
        let mut short = req.clone();
        short.state.pop();
        assert!(build_cancel_retired(&short, &budgets).is_err(), "{label}");
        let other = retired::retired()
            .iter()
            .find(|o| o.family == r.family && o.kind.base() != r.kind.base() && !o.is_no_lot() && !o.is_current_layout())
            .unwrap();
        assert!(retired::encode(r, &live_order(other, p, p)).is_err(), "{label}: another kind's state");
        // the plan is spend-only: no other entry of the retired template passes `check`
        let SigPlan::Retired { template_hash, state, .. } = built.plans[0].clone() else { unreachable!() };
        for name in r.template.contract().entries.keys().filter(|n| *n != "cancel") {
            let call = SigPlan::Retired { template_hash, state: state.clone(), entry: name.clone(), args: vec![] };
            assert!(call.check().is_err(), "{label}: {name} must not be built");
        }
        // a cross limit with an auction has no span under a 333-byte template (it had no auction)
        if r.template.state_len == 333 {
            let RetiredState::Lot(LotState::KobCross(x)) = &st else { unreachable!() };
            let auctioned = LotState::KobCross(lot::CrossState { b_lot_end: x.b_lot - 1, auction_daa: 200, ..x.clone() });
            assert!(retired::encode(r, &auctioned).is_err(), "{label}: an auction under the pre-auction template");
        }
        // a KRON order carries no extension commitment
        if r.family == Family::Kron {
            if let RetiredState::Lot(LotState::KobBid(b)) = &st {
                let ext = LotState::KobBid(lot::BidState { extension_commitment: [0xee; 32], ..b.clone() });
                assert!(retired::encode(r, &ext).is_err(), "{label}: a KRON bid with an extension commitment");
            }
        }
    }
}

/// The protocol v3 cross limit (no lots, 360 bytes; never deployed, retired for safety): its maker cancels a live order for
/// token A of either family (`aFamily`) with its custody of exactly `amountLeft` base units and strays of A and B; the lot
/// reader refuses its state, and an `aFamily` that is not the token program's, or a custody of another amount, is refused.
#[test]
fn the_maker_cancels_a_v3_cross_limit_without_lots() {
    let r = retired::retired().iter().find(|r| r.is_no_lot()).expect("the v3 cross limit");
    assert_eq!((r.kind, r.template.state_len), (TemplateId::KobCross, 360));
    for (pa, pb) in [(TemplateId::KronToken2433, TemplateId::Kcc20Ref8x8), (TemplateId::Kcc20KaspaCom025, TemplateId::KronToken2732)] {
        let label = pair_name(pa, pb);
        let (req, st) = cancel_request(r, pa, pb, true);
        assert!(retired::decode(r, &req.state).is_err(), "{label}: no lot layout");
        assert_eq!(st.custody_amount(), Some(6 * WHOLE), "{label}");
        assert_eq!(req.custody.as_ref().unwrap().state.amount(), 6 * WHOLE, "{label}");
        let built = build_cancel_retired(&req, &budgets).unwrap_or_else(|e| panic!("{label}: build: {e}"));
        assert!(built.roles[0].starts_with("KobCross.cancel.retired.ea23f1fe@"), "{label}: {:?}", built.roles);
        let signed = finalize(&built, &sign_locally(&built, &keys()).unwrap(), FinalizeOptions::default()).unwrap();
        validate_signed(&signed).unwrap_or_else(|e| panic!("{label}: engine: {e}"));
        // custody of A with the stray of A, the stray of B, the foreign stray: three token outputs to the maker
        assert_eq!(signed.tx.outputs.iter().filter(|o| o.covenant.is_some()).count(), 3, "{label}");
        // aFamily of the other family than token A's program
        let RetiredState::NoLotCross(x) = &st else { unreachable!() };
        let other = if pa.family() == Family::Kron { 1 } else { 2 };
        let wrong = RetiredState::NoLotCross(kob_protocol::retired::nolot::CrossState { a_family: other, ..x.clone() });
        let bad = CancelRetired { state: retired::encode_any(r, &wrong).unwrap(), ..req.clone() };
        assert!(build_cancel_retired(&bad, &budgets).unwrap_err().to_string().contains("other family"), "{label}");
        // a custody that is not exactly amountLeft
        let mut bad = req.clone();
        bad.custody = Some(tutxo(pa, TOKEN_COV, 71, 6 * WHOLE - 1, ORDER_ID, true));
        assert!(build_cancel_retired(&bad, &budgets).is_err(), "{label}");
    }
}

/// The protocol v3 sell-first entries retired 2026-10-06 (their refund did not require tokens held): today's layout, so the
/// order reads with today's state type; its maker cancels a live entry and an EMPTY repeating one (no custody, the case the
/// old refund accepted) on every program of its family, and nothing but the cancel is built.
#[test]
fn the_maker_cancels_the_v3_sell_first_entries_retired_for_the_empty_refund() {
    let cur: Vec<&Retired> = retired::retired().iter().filter(|r| r.is_current_layout()).collect();
    assert_eq!(cur.len(), 2);
    for r in cur {
        for p in PROGRAMS.into_iter().filter(|p| p.family() == r.family) {
            let label = format!("{} {} @{}", r.kind_name(), &r.hash_hex()[..8], p.name());
            let live = live_current(r, p);
            let mut empty = live.clone();
            match &mut empty {
                kob_protocol::state::AnyState::KobIfdAsk(s) | kob_protocol::state::AnyState::KobIfdAskKron(s) => {
                    s.amount_left = 0;
                    s.rpt_amount = 9;
                }
                _ => unreachable!(),
            }
            for st in [live, empty] {
                let span = retired::encode_any(r, &RetiredState::Current(st.clone())).unwrap();
                assert_eq!(span, st.encode(), "{label}: today's encoding is the retired span");
                let amount = st.amount_left().unwrap();
                let req = CancelRetired {
                    template_hash: r.template.hash,
                    state: span,
                    order: utxo(70, 50 * KAS, 1_000, Some(ENTRY_ID)),
                    custody: (amount > 0).then(|| tutxo(p, TOKEN_COV, 71, amount, ENTRY_ID, true)),
                    strays: vec![],
                    foreign: vec![],
                    funding: vec![key_utxo(3, MAKER_A, 10 * KAS)],
                    change: None,
                    fee: fee(),
                };
                let built = build_cancel_retired(&req, &budgets).unwrap_or_else(|e| panic!("{label} amount {amount}: build: {e}"));
                let SigPlan::Retired { template_hash, entry, .. } = &built.plans[0] else { panic!("{label}") };
                assert_eq!((*template_hash, entry.as_str()), (r.template.hash, "cancel"), "{label}");
                assert_ne!(*template_hash, template(r.kind).hash, "{label}: the retired script, not today's");
                let signed = finalize(&built, &sign_locally(&built, &keys()).unwrap(), FinalizeOptions::default()).unwrap();
                validate_signed(&signed).unwrap_or_else(|e| panic!("{label} amount {amount}: engine: {e}"));
                assert_eq!(signed.tx.outputs.iter().filter(|o| o.covenant.is_some()).count(), usize::from(amount > 0), "{label}");
            }
        }
    }
}

/// The order templates the running testnet-10 soak pinned (`contracts/deploy/testnet-10/deployment.json` at 217d452, the
/// `--features deploy-tn10` build of 2026-10-06) are each either pinned by today's testnet-10 deployment record under the same
/// kind, or a retired template of that kind this build can spend: no live order of the soak becomes uncancellable when its
/// executors and wallets move to today's build (the soak keeps its indexer databases and live orders across the switch).
#[test]
fn every_order_template_of_the_tn10_soak_build_is_pinned_or_retired() {
    const SOAK_217D452: [(&str, &str); 15] = [
        ("KobAsk", "126ff059375b459674df139dc644f6518cc26e831e998e9be7a4008c8c8e0aa1"),
        ("KobBid", "b995661f8b17c7c558b85975e361c26b00a2d57214cf631083a464b835e54fa3"),
        ("KobCondAsk", "40a8eb7321e9dc157ca05683fcf01c1967d0c2be91fb8644e02025b0df087b74"),
        ("KobCondBid", "076e7bd8bfb6e1b59a8f2a19a6cad5961c7978368a0816a2b9df5f39d805bdfb"),
        ("KobIfdBid", "50b58e7b26e0bdc1a0eed7f7d7e5bcee88f95ed6ff367be32b8e74979a9de292"),
        ("KobIfdAsk", "189b9c3297cac33c0de6b5defefcbee5deee42306e8aa72b4615d8f2d4515a17"),
        ("KobPair", "107388074abe4d223d6fc1e17f6d5aa09bd76a7b739aa2e82774516ddbef7d16"),
        ("KobCondPair", "37ab667f662779e97e75ea712e855c073b4d2c9a5183285fa7ec11daf8ae0820"),
        ("KobIfdPair", "9975b2f3131179217050165f51b844700cf9f99f47c5b7954dabf00cf0441d5c"),
        ("KobAskKron", "f7274b79b081fbbf05d14b006359883c144304adb0ec0c6f9b8741feaef8f76d"),
        ("KobBidKron", "6ec1a3dd4a287b73295a08db5f75fedcac4966539d793e9d1a659711ad888efc"),
        ("KobCondAskKron", "6c4f92cee1613899b5d680e784b11fd567b1bfac63a5006018cc6c54cfd35839"),
        ("KobCondBidKron", "fb392f88a136f14708841895b2efae2a5070749ddeb0168c7405806aea7ed953"),
        ("KobIfdBidKron", "e5cff7ede2faeb58706cfd28e095dcefad552f1e3e92d8a12bd05a22c053496d"),
        ("KobIfdAskKron", "85d8783813f861d16d2679d13fd062d81ce572f7fe4efa111943bd5a6a6b4ac8"),
    ];
    let record: serde_json::Value =
        serde_json::from_str(include_str!("../../../contracts/deploy/testnet-10/deployment.json")).expect("deployment record");
    let pinned_now = |name: &str| -> String {
        let t = record["templates"].as_array().unwrap().iter().find(|t| t["name"] == name);
        t.unwrap_or_else(|| panic!("{name} is in today's testnet-10 record"))["hash"].as_str().unwrap().to_string()
    };
    let mut changed = vec![];
    for (name, hash) in SOAK_217D452 {
        if pinned_now(name) == hash {
            continue;
        }
        let mut h = [0u8; 32];
        faster_hex::hex_decode(hash.as_bytes(), &mut h).unwrap();
        let r = retired::by_hash(&h)
            .unwrap_or_else(|| panic!("{name} {hash}: changed but not retired (its live orders would be stranded)"));
        assert_eq!(r.kind_name(), name, "{hash}: retired under its own kind");
        assert!(r.is_current_layout(), "{name}: the soak's v3 state reads with today's state type");
        changed.push(name);
    }
    assert_eq!(changed, ["KobIfdAsk", "KobIfdAskKron"], "the templates that changed between 217d452 and today");
}
