//! Retired-template fixtures (`kob_protocol::retired`): a live order of every retired template (the lot templates and the
//! protocol v3 cross limit without lots) and its maker's cancel request, shared by the engine tests (`retired_cancel.rs`) and the compute-budget generator (`budget_table.rs`,
//! the `<Kind>.cancel.retired.<hash8>@<program>` roles).
#![allow(dead_code)]

use kob_protocol::artifacts::TemplateId;
use kob_protocol::build::{CancelRetired, ForeignStrays, TokenRef};
use kob_protocol::retired::lot::{self, LotState};
use kob_protocol::retired::{self, nolot, Retired, RetiredState};

use super::pair::*;
use super::*;

/// Covenant id of the retired order.
pub const ORDER_ID: [u8; 32] = [0x47; 32];

/// Base units per old "unit" of the fixture lot orders, and units per lot: one lot is 1000 base units.
const LOT_UNIT: i64 = 1_000;

/// A live order of `r`'s kind in its family, on token program `p` (token B on `pb` for a cross limit), as the retired
/// template holds it, in its lot layout: a partly filled ask / conditional / cross with custody, a bid escrow, an armed
/// repeating entry. Built from the retired artifact's own example instance with the fixture maker and tokens.
pub fn live_order(r: &Retired, p: TemplateId, pb: TemplateId) -> LotState {
    let t = &r.template;
    let example = &t.contract().compiled.bytecode[t.prefix.len()..t.prefix.len() + t.state_len];
    let base = retired::decode(r, example).unwrap_or_else(|e| panic!("{}: example state: {e}", r.note));
    let (h, pre, suf) = tok_fields(p);
    let ext = ext_for(p);
    let maker = pk(MAKER_A);
    match base {
        LotState::KobAsk(a) => LotState::KobAsk(lot::AskState {
            maker,
            token_cov_id: TOKEN_COV,
            token_tpl_hash: h,
            tpl_prefix_len: pre,
            tpl_suffix_len: suf,
            unit: LOT_UNIT,
            lot_units: 1,
            lots_left: 7,
            ..a
        }),
        LotState::KobBid(b) => LotState::KobBid(lot::BidState {
            maker,
            token_cov_id: TOKEN_COV,
            token_tpl_hash: h,
            tpl_prefix_len: pre,
            tpl_suffix_len: suf,
            extension_commitment: ext,
            unit: LOT_UNIT,
            lot_units: 1,
            ..b
        }),
        LotState::KobCondAsk(c) => LotState::KobCondAsk(lot::CondAskState {
            maker,
            token_cov_id: TOKEN_COV,
            token_tpl_hash: h,
            tpl_prefix_len: pre,
            tpl_suffix_len: suf,
            unit: LOT_UNIT,
            lot_units: 1,
            lots_left: 6,
            ..c
        }),
        LotState::KobCondBid(c) => LotState::KobCondBid(lot::CondBidState {
            maker,
            token_cov_id: TOKEN_COV,
            token_tpl_hash: h,
            tpl_prefix_len: pre,
            tpl_suffix_len: suf,
            extension_commitment: ext,
            unit: LOT_UNIT,
            lot_units: 1,
            lots_left: 6,
            ..c
        }),
        LotState::KobIfdBid(i) => LotState::KobIfdBid(lot::IfdBidState {
            maker,
            token_cov_id: TOKEN_COV,
            token_tpl_hash: h,
            tpl_prefix_len: pre,
            tpl_suffix_len: suf,
            extension_commitment: ext,
            unit: LOT_UNIT,
            lot_units: 1,
            entry_stop: P255,
            armed: 1,
            lots_left: 4,
            rpt_lots: 9,
            ..i
        }),
        LotState::KobIfdAsk(i) => LotState::KobIfdAsk(lot::IfdAskState {
            maker,
            token_cov_id: TOKEN_COV,
            token_tpl_hash: h,
            tpl_prefix_len: pre,
            tpl_suffix_len: suf,
            unit: LOT_UNIT,
            lot_units: 1,
            entry_stop: P255,
            armed: 1,
            lots_left: 4,
            rpt_lots: 9,
            ..i
        }),
        LotState::KobCross(x) => {
            let (bh, bp, bs) = tok_fields(pb);
            LotState::KobCross(lot::CrossState {
                maker,
                token_cov_id: TOKEN_COV,
                token_tpl_hash: h,
                tpl_prefix_len: pre,
                tpl_suffix_len: suf,
                unit: LOT_UNIT,
                lot_units: 1,
                b_family: pb.family().code() as i64,
                b_cov_id: TOKEN_B,
                b_tpl_hash: bh,
                b_prefix_len: bp,
                b_suffix_len: bs,
                b_ext: ext_for(pb),
                b_lot: LOT_UNIT,
                b_lot_end: LOT_UNIT,
                auction_daa: 0,
                lots_left: 6,
                ..x
            })
        }
    }
}

/// A live order of the protocol v3 cross limit (no lots): token A on program `pa` (either family, `aFamily`), token B on
/// `pb`, 6 whole A left, built from the retired artifact's own example instance.
pub fn live_nolot_cross(r: &Retired, pa: TemplateId, pb: TemplateId) -> nolot::CrossState {
    let t = &r.template;
    let example = &t.contract().compiled.bytecode[t.prefix.len()..t.prefix.len() + t.state_len];
    let RetiredState::NoLotCross(x) = retired::decode_any(r, example).unwrap_or_else(|e| panic!("{}: example state: {e}", r.note))
    else {
        panic!("{}: the no-lot layout", r.note)
    };
    let (h, pre, suf) = tok_fields(pa);
    let (bh, bp, bs) = tok_fields(pb);
    nolot::CrossState {
        maker: pk(MAKER_A),
        token_cov_id: TOKEN_COV,
        token_tpl_hash: h,
        tpl_prefix_len: pre,
        tpl_suffix_len: suf,
        a_family: pa.family().code() as i64,
        scale: SCALE,
        min_fill: WHOLE,
        b_family: pb.family().code() as i64,
        b_cov_id: TOKEN_B,
        b_tpl_hash: bh,
        b_prefix_len: bp,
        b_suffix_len: bs,
        b_ext: ext_for(pb),
        price: RATE,
        price_end: 0,
        auction_daa: 0,
        amount_left: 6 * WHOLE,
        ..x
    }
}

/// A live order of a retired template with today's layout (the protocol v3 sell-first entries retired 2026-10-06): today's
/// state of its kind on token program `p`, 4 whole tokens in custody, built from the retired artifact's own example instance.
pub fn live_current(r: &Retired, p: TemplateId) -> AnyState {
    let t = &r.template;
    let example = &t.contract().compiled.bytecode[t.prefix.len()..t.prefix.len() + t.state_len];
    let RetiredState::Current(a) = retired::decode_any(r, example).unwrap_or_else(|e| panic!("{}: example state: {e}", r.note)) else {
        panic!("{}: today's layout", r.note)
    };
    let (h, pre, suf) = tok_fields(p);
    let live = |s: IfdAskState| IfdAskState {
        maker: pk(MAKER_A),
        token_cov_id: TOKEN_COV,
        token_tpl_hash: h,
        tpl_prefix_len: pre,
        tpl_suffix_len: suf,
        amount_left: 4 * s.scale,
        ..s
    };
    match a {
        AnyState::KobIfdAsk(s) => AnyState::KobIfdAsk(live(s)),
        AnyState::KobIfdAskKron(s) => AnyState::KobIfdAskKron(live(s)),
        other => panic!("{}: no live fixture of a retired {} with today's layout", r.note, other.template_id().name()),
    }
}

/// A live order of any retired template ([`live_order`], [`live_nolot_cross`], [`live_current`]).
pub fn live_any(r: &Retired, p: TemplateId, pb: TemplateId) -> RetiredState {
    if r.is_current_layout() {
        RetiredState::Current(live_current(r, p))
    } else if r.is_no_lot() {
        RetiredState::NoLotCross(live_nolot_cross(r, p, pb))
    } else {
        RetiredState::Lot(live_order(r, p, pb))
    }
}

/// The (program, program of token B) pairs a retired template is exercised on, from `programs`: the programs of its family
/// (and for a cross limit, token B on every program of `programs`, of either family; the protocol v3 cross limit, token A
/// of either family too).
pub fn program_pairs(r: &Retired, programs: &[TemplateId]) -> Vec<(TemplateId, TemplateId)> {
    let fam: Vec<TemplateId> = programs.iter().copied().filter(|p| r.serves(p.family())).collect();
    assert!(!fam.is_empty(), "a program of the family");
    match r.kind {
        TemplateId::KobCross => fam.iter().flat_map(|pa| programs.iter().map(|pb| (*pa, *pb))).collect(),
        _ => fam.iter().map(|p| (*p, *p)).collect(),
    }
}

/// The maker's cancel request of a live order of `r` ([`live_any`]): its custody (`lotsLeft x lotUnits x unit`; the v3 cross
/// limit's `amountLeft`), and (with `strays`) a stray of each of its tokens and a foreign stray. Checks the retired span
/// round trip on the way.
pub fn cancel_request(r: &Retired, p: TemplateId, pb: TemplateId, strays: bool) -> (CancelRetired, RetiredState) {
    let st = live_any(r, p, pb);
    let span = retired::encode_any(r, &st).unwrap_or_else(|e| panic!("{}: encode: {e}", r.note));
    assert_eq!(span.len(), r.template.state_len, "{}", r.note);
    assert_eq!(retired::decode_any(r, &span).unwrap(), st, "{}: the retired span decodes back to the same order", r.note);
    let custody = if st.holds_tokens() {
        let a = st.custody_amount().expect("a lot custody");
        assert!(a > 0, "{}: a token-holding kind holds tokens", r.note);
        Some(tutxo(p, TOKEN_COV, 71, a, ORDER_ID, true))
    } else {
        None
    };
    let mut stray_utxos = vec![];
    let mut foreign = vec![];
    if strays {
        stray_utxos.push(tutxo(p, TOKEN_COV, 76, 3, ORDER_ID, true));
        if r.kind == TemplateId::KobCross {
            stray_utxos.push(tutxo(pb, TOKEN_B, 77, 5, ORDER_ID, true));
        }
        foreign.push(ForeignStrays {
            token: TokenRef { covenant_id: [0x72; 32], program: p },
            utxos: vec![tutxo(p, [0x72; 32], 80, 9, ORDER_ID, true)],
        });
    }
    let req = CancelRetired {
        template_hash: r.template.hash,
        state: span,
        order: utxo(70, 50 * KAS, 1_000, Some(ORDER_ID)),
        custody,
        strays: stray_utxos,
        foreign,
        funding: vec![key_utxo(3, MAKER_A, 10 * KAS)],
        change: None,
        fee: fee(),
    };
    (req, st)
}

/// Every retired cancel shape the compute-budget table measures: every retired template on every program of its family
/// (token B on every program for a cross limit; token A on every program for the v3 cross limit), without and with
/// strays.
pub fn retired_cancel_shapes() -> Vec<(String, CancelRetired)> {
    let mut out = vec![];
    for r in retired::retired() {
        for (p, pb) in program_pairs(r, &PROGRAMS) {
            for strays in [false, true] {
                let name =
                    format!("retired.{}.cancel{}@{}", &r.hash_hex()[..8], if strays { ".strays" } else { "" }, pair_name(p, pb));
                out.push((name, cancel_request(r, p, pb, strays).0));
            }
        }
    }
    out
}
