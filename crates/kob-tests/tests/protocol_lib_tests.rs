//! Cross-check of `kob-protocol` against the compiler: an instance assembled by the library
//! (pinned template prefix ‖ typed state encoding ‖ suffix) must be byte-identical to what the
//! vendored SilverScript compiler produces from the contract source with the same values as
//! constructor arguments (network constants taken from the committed ctor files).

mod common;

use kob_protocol::artifacts::{template, TemplateId};
use kob_protocol::state::*;
use silverscript_abi::ArtifactValue;
use silverscript_lang::compiler::CompileOptions;

fn ctor_constants(name: &str, count: usize) -> Vec<ArtifactValue> {
    let path = common::repo_root().join("contracts/v2").join(format!("{name}.ctor.json"));
    let all: Vec<ArtifactValue> = serde_json::from_slice(&std::fs::read(path).expect("ctor")).expect("ctor json");
    all[..count].to_vec()
}

fn compiled(id: TemplateId, constants: usize, values: std::collections::BTreeMap<String, ArtifactValue>) -> Vec<u8> {
    let t = template(id);
    let mut args = if constants > 0 { ctor_constants(id.name(), constants) } else { vec![] };
    for f in &t.contract().runtime_state.fields {
        args.push(values[&f.name].clone());
    }
    let src = common::contract_source(id.name());
    let art = common::compile_contract(&src, &args, CompileOptions::default()).expect("compile");
    common::bytecode(&art)
}

fn token_fields(tpl: TemplateId) -> ([u8; 32], i64, i64) {
    let t = kob_protocol::artifacts::token_template(tpl);
    (t.hash, t.prefix.len() as i64, t.suffix.len() as i64)
}

#[test]
fn library_instances_equal_compiler_output() {
    let (h, p, s) = token_fields(TemplateId::Kcc20Ref8x8);
    // amounts in base units (scale 1000: a 3-decimal token), deliberately not multiples of the scale
    let ask = AskState {
        maker: [0x11; 32],
        token_cov_id: [0x70; 32],
        token_tpl_hash: h,
        tpl_prefix_len: p,
        tpl_suffix_len: s,
        scale: 1_000,
        min_fill: 3_000,
        price: 250_000_001,
        tip: 12_345,
        tif: 1,
        active_from: 1_000,
        expiry_daa: 400_000_000,
        refund_tip: 3_000_000,
        interval: 600,
        max_fill: 2_500,
        slope: 7,
        price_end: 1,
        decay_step: 3,
        amount_left: 9_001,
    };
    assert_eq!(ask.redeem(), compiled(TemplateId::KobAsk, 0, ask.to_values()), "KobAsk");

    let bid = BidState {
        maker: [0x12; 32],
        token_cov_id: [0x70; 32],
        token_tpl_hash: h,
        tpl_prefix_len: p,
        tpl_suffix_len: s,
        extension_commitment: [0xee; 32],
        scale: 1_000,
        min_fill: 2_000,
        price: 245_000_000,
        tip: 0,
        tif: 2,
        active_from: 0,
        expiry_daa: 499_999_999_999,
        refund_tip: 3_000_000,
        reserve: 100_000_000,
        delivery_carrier: 1_000_000_000,
        interval: 0,
        max_fill: 0,
        slope: 1_000_000,
        price_end: 260_000_000,
        decay_step: 1,
    };
    assert_eq!(bid.redeem(), compiled(TemplateId::KobBid, 0, bid.to_values()), "KobBid");

    let ca = CondAskState {
        maker: [0x14; 32],
        token_cov_id: [0x70; 32],
        token_tpl_hash: h,
        tpl_prefix_len: p,
        tpl_suffix_len: s,
        scale: 1_000,
        min_fill: 1_000,
        tip: 100_000,
        active_from: 5,
        expiry_daa: 400_000_000,
        refund_tip: 3_000_000,
        tp_price: 300_000_000,
        stop_price: 200_000_000,
        slip_bps: 300,
        trail_step: 5_000_000,
        trail_gap: 10_000_000,
        trail_wait: 600,
        min_touch: 1_000,
        min_rest_daa: 600,
        armed: 1,
        band_daa: 300,
        keeper_tip: 2_500_000,
        amount_left: 4_321,
        parent: [0xd1; 32],
        rpt_price: 260_100_000,
        rpt_until: 78_760_000,
    };
    assert_eq!(ca.redeem(), compiled(TemplateId::KobCondAsk, 6, ca.to_values()), "KobCondAsk");

    let cb = CondBidState {
        maker: [0x15; 32],
        token_cov_id: [0x70; 32],
        token_tpl_hash: h,
        tpl_prefix_len: p,
        tpl_suffix_len: s,
        extension_commitment: [0xee; 32],
        scale: 1_000,
        min_fill: 1_000,
        tip: 100_000,
        active_from: 0,
        expiry_daa: 400_000_000,
        refund_tip: 3_000_000,
        delivery_carrier: 1_000_000_000,
        tp_price: 200_000_000,
        stop_price: 300_000_000,
        slip_bps: 300,
        trail_step: 0,
        trail_gap: 0,
        trail_wait: 0,
        min_touch: 1_500,
        min_rest_daa: 600,
        amount_left: 7_777,
        armed: 0,
        band_daa: 300,
        keeper_tip: 2_500_000,
        parent: [0xf1; 32],
        rpt_price: 249_900_000,
        rpt_pre: 50_000_000,
        rpt_until: 78_760_000,
    };
    assert_eq!(cb.redeem(), compiled(TemplateId::KobCondBid, 6, cb.to_values()), "KobCondBid");

    let ib = IfdBidState {
        maker: [0x16; 32],
        token_cov_id: [0x70; 32],
        token_tpl_hash: h,
        tpl_prefix_len: p,
        tpl_suffix_len: s,
        extension_commitment: [0xee; 32],
        scale: 1_000,
        amount_left: 10_250,
        price: 260_000_000,
        tip: 100_000,
        active_from: 0,
        expiry_daa: 400_000_000,
        refund_tip: 3_000_000,
        delivery_carrier: 1_000_000_000,
        exit_carrier: 1_000_000_000,
        min_fill: 3_000,
        entry_stop: 255_000_000,
        band_daa: 300,
        min_touch: 1_000,
        min_rest_daa: 600,
        keeper_tip: 2_500_000,
        armed: 999_850,
        rpt_amount: 21_001,
        exit_state: IfdBidState::commit_exit(&CondAskState { parent: [0; 32], rpt_price: 0, rpt_until: 0, armed: 0, ..ca.clone() }),
    };
    assert_eq!(ib.redeem(), compiled(TemplateId::KobIfdBid, 6, ib.to_values()), "KobIfdBid");

    let ia = IfdAskState {
        maker: [0x17; 32],
        token_cov_id: [0x70; 32],
        token_tpl_hash: h,
        tpl_prefix_len: p,
        tpl_suffix_len: s,
        scale: 1_000,
        price: 250_000_000,
        tip: 100_000,
        active_from: 0,
        expiry_daa: 400_000_000,
        refund_tip: 3_000_000,
        prefund: 50_000_000,
        exit_carrier: 1_000_000_000,
        min_fill: 2_000,
        entry_stop: 255_000_000,
        band_daa: 300,
        min_touch: 1_000,
        min_rest_daa: 600,
        keeper_tip: 2_500_000,
        armed: 0,
        amount_left: 6_123,
        rpt_amount: 17_001,
        exit_state: IfdAskState::commit_exit(&CondBidState { parent: [0; 32], rpt_price: 0, rpt_pre: 0, rpt_until: 0, ..cb.clone() }),
    };
    assert_eq!(ia.redeem(), compiled(TemplateId::KobIfdAsk, 6, ia.to_values()), "KobIfdAsk");

    // Pair orders (one template each for both sides and both families). KobPair: no build constants; KobCondPair: 15
    // (the KAS-book templates of both families and KobPair); KobIfdPair: 18 (KobCondPair first). Token A KRON (scale
    // 10^8), token B KCC-20 (scale 10^3); odd amounts, prices and rates.
    let (bh, bp, bs) = token_fields(TemplateId::Kcc20Ref8x8);
    let (ah, ap, asf) = token_fields(TemplateId::KronToken2433);
    let x = PairState {
        maker: [0x18; 32],
        side: 2,
        s_cov_id: [0x73; 32],
        s_tpl_hash: bh,
        s_pre: bp,
        s_suf: bs,
        s_family: 1,
        s_scale: 1_000,
        t_cov_id: [0x71; 32],
        t_tpl_hash: ah,
        t_pre: ap,
        t_suf: asf,
        t_family: 2,
        t_ext: [0; 32],
        t_scale: 100_000_000,
        min_fill: 150_000_001,
        price: 1_234_567,
        tip: 99_999,
        tif: 1,
        active_from: 10,
        expiry_daa: 400_000_000,
        refund_tip: 3_000_000,
        delivery_carrier: 1_000_000_000,
        interval: 600,
        max_fill: 500_000_003,
        slope: 7,
        price_end: 1_300_001,
        decay_step: 60,
        amount_left: 987_654_321,
        custody: 12_195_000,
    };
    assert_eq!(x.redeem(), compiled(TemplateId::KobPair, 0, x.to_values()), "KobPair");
    let c = CondPairState {
        maker: [0x19; 32],
        side: 1,
        s_cov_id: [0x71; 32],
        s_tpl_hash: ah,
        s_pre: ap,
        s_suf: asf,
        s_family: 2,
        s_scale: 100_000_000,
        t_cov_id: [0x73; 32],
        t_tpl_hash: bh,
        t_pre: bp,
        t_suf: bs,
        t_family: 1,
        t_ext: [0xee; 32],
        t_scale: 1_000,
        min_fill: 25_000_001,
        tip: 12_345,
        active_from: 11,
        expiry_daa: 400_000_001,
        refund_tip: 3_000_001,
        delivery_carrier: 200_000_000,
        tp_price: 1_500_003,
        slip_bps: 300,
        trail_step: 1_001,
        trail_gap: 2_003,
        trail_wait: 120,
        min_touch: 1_000_007,
        min_rest_daa: 600,
        band_daa: 300,
        keeper_tip: 2_500_000,
        stop_price: 1_000_009,
        armed: 0,
        amount_left: 987_654_321,
        custody: 987_654_321,
        parent: [0x44; 32],
        rpt_price: 1_111_111,
        rpt_pre: 0,
        rpt_until: 77_760_123,
    };
    assert_eq!(c.redeem(), compiled(TemplateId::KobCondPair, 15, c.to_values()), "KobCondPair");
    let e = IfdPairState {
        maker: [0x19; 32],
        side: 1,
        a_cov_id: [0x71; 32],
        a_tpl_hash: ah,
        a_pre: ap,
        a_suf: asf,
        a_family: 2,
        a_scale: 100_000_000,
        a_ext: [0; 32],
        b_cov_id: [0x73; 32],
        b_tpl_hash: bh,
        b_pre: bp,
        b_suf: bs,
        b_family: 1,
        b_scale: 1_000,
        b_ext: [0xee; 32],
        price: 1_234_567,
        prefund: 200_001,
        tip: 99_999,
        active_from: 10,
        expiry_daa: 400_000_000,
        refund_tip: 3_000_000,
        delivery_carrier: 600_000_000,
        exit_carrier: 600_000_001,
        min_fill: 250_000_003,
        entry_stop: 1_100_007,
        band_daa: 300,
        min_touch: 1_000_007,
        min_rest_daa: 600,
        keeper_tip: 2_500_000,
        armed: 0,
        amount_left: 987_654_321,
        custody: 1_975_309,
        rpt_amount: 17_001,
        exit_state: IfdPairState::commit_exit(&CondPairState { side: 2, parent: [0; 32], rpt_price: 0, rpt_until: 0, ..c.clone() }),
    };
    assert_eq!(e.redeem(), compiled(TemplateId::KobIfdPair, 18, e.to_values()), "KobIfdPair");

    // Token instance under the 8/8 program.
    let tok = Kcc20State::custody(12_345, [0x61; 32], [0xee; 32]);
    let t = template(TemplateId::Kcc20Ref8x8);
    let args: Vec<ArtifactValue> = t.contract().runtime_state.fields.iter().map(|f| tok.to_values()[&f.name].clone()).collect();
    let art = common::compile_contract(&common::contract_source("KCC20Ref_8x8"), &args, CompileOptions::default()).expect("compile");
    assert_eq!(tok.redeem_with(t), common::bytecode(&art), "KCC20Ref_8x8");
}
