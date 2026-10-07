//! End to end against a mock wRPC node (no network): real orders built with kob-protocol, a kob-backup, an export file and
//! indexer views on disk, `kob recover` (search, report, `--cancel --dry-run`, submit) and `kob order sweep|cancel`.
//! Every signed transaction is re-validated in the rusty-kaspa engine from the file the command wrote.
//!
//! Amounts are base units and prices sompi per whole token. The fixtures use `SCALE = 1000` base units per whole token (a
//! token of three decimals).

use std::path::{Path, PathBuf};

use kaspa_consensus_core::tx::ScriptPublicKey;
use kob_protocol::artifacts::{template, token_template, TemplateId};
use kob_protocol::json::to_hex;
use kob_protocol::script::p2pk_spk;
use kob_protocol::state::{AnyState, AskState, BidState, CondPairState, IfdPairState, PairState, TokenState, SIDE_ASK, SIDE_BID};
use kob_protocol::tx::{pubkey_of, spk_to_string, SignedTx};
use kob_protocol::verify::validate_signed;
use kob_protocol::Family;
use serde_json::{json, Value};

use crate::common::{address_of, decode_state, encode_state, order_template, spk_bytes};
use crate::recover::{VIA_FALLBACK, VIA_HINT, VIA_KNOWN};
use crate::wrpc::{mock, NodeUtxo};
use crate::{inputs, order, recover};

const NET: &str = "testnet-10";
const KAS: u64 = 100_000_000;
const CARRIER: u64 = 10 * KAS;
/// Base units per whole token of the fixtures' tokens (three decimals).
const SCALE: i64 = 1_000;
const TOKEN: [u8; 32] = [0x70; 32];
const TOKEN_B: [u8; 32] = [0x71; 32];
const TOKEN_F: [u8; 32] = [0x72; 32];
const EXT: [u8; 32] = [0xee; 32];
const P: TemplateId = TemplateId::Kcc20Ref8x8;
const PF: TemplateId = TemplateId::KronToken2433;

const A1: [u8; 32] = [0xa1; 32];
const A2: [u8; 32] = [0xa2; 32];
const A3: [u8; 32] = [0xa3; 32];
const A5: [u8; 32] = [0xa5; 32];
const A6: [u8; 32] = [0xa6; 32];
const A7: [u8; 32] = [0xa7; 32];
const A9: [u8; 32] = [0xa9; 32];

fn sk(n: u8) -> [u8; 32] {
    [n; 32]
}
fn pk(n: u8) -> [u8; 32] {
    pubkey_of(&sk(n)).unwrap()
}
fn v(s: &str) -> Vec<String> {
    s.split_whitespace().map(str::to_string).collect()
}

fn tok_fields(p: TemplateId) -> ([u8; 32], i64, i64) {
    let t = token_template(p);
    (t.hash, t.prefix.len() as i64, t.suffix.len() as i64)
}

fn rtip() -> i64 {
    kob_protocol::defaults::tips(P).map(|t| t.refund_tip as i64).unwrap_or(10_000_000)
}

/// A plain ask of `amount` base units at 0.25 KAS per whole token, no smaller fill than one whole token.
fn ask(maker: u8, amount: i64) -> AskState {
    let (h, p, s) = tok_fields(P);
    AskState {
        maker: pk(maker),
        token_cov_id: TOKEN,
        token_tpl_hash: h,
        tpl_prefix_len: p,
        tpl_suffix_len: s,
        scale: SCALE,
        min_fill: SCALE,
        price: 250_000_000,
        tip: 100_000,
        tif: 0,
        active_from: 0,
        expiry_daa: 400_000_000,
        refund_tip: rtip(),
        interval: 0,
        max_fill: 0,
        slope: 0,
        price_end: 0,
        decay_step: 1_000,
        amount_left: amount,
    }
}

fn bid(maker: u8) -> BidState {
    let (h, p, s) = tok_fields(P);
    BidState {
        maker: pk(maker),
        token_cov_id: TOKEN,
        token_tpl_hash: h,
        tpl_prefix_len: p,
        tpl_suffix_len: s,
        extension_commitment: EXT,
        scale: SCALE,
        min_fill: SCALE,
        price: 245_000_000,
        tip: 100_000,
        tif: 0,
        active_from: 0,
        expiry_daa: 400_000_000,
        refund_tip: rtip(),
        reserve: 0,
        delivery_carrier: CARRIER as i64,
        interval: 0,
        max_fill: 0,
        slope: 0,
        price_end: 0,
        decay_step: 1_000,
    }
}

/// A pair order of 10 whole TOKEN (A) for TOKEN_B (B) at 1000 base units of B per whole A: an ASK (custody 10 000 of A) or a
/// BID (custody: the B escrow of four fills).
fn pair_order(maker: u8, ask: bool) -> PairState {
    let (h, p, s) = tok_fields(P);
    let (sc, tc) = if ask { (TOKEN, TOKEN_B) } else { (TOKEN_B, TOKEN) };
    let mut x = PairState {
        maker: pk(maker),
        side: if ask { SIDE_ASK } else { SIDE_BID },
        s_cov_id: sc,
        s_tpl_hash: h,
        s_pre: p,
        s_suf: s,
        s_family: Family::Kcc20.code() as i64,
        s_scale: SCALE,
        t_cov_id: tc,
        t_tpl_hash: h,
        t_pre: p,
        t_suf: s,
        t_family: Family::Kcc20.code() as i64,
        t_ext: EXT,
        t_scale: SCALE,
        min_fill: SCALE,
        price: SCALE,
        tip: 0,
        tif: 0,
        active_from: 0,
        expiry_daa: 400_000_000,
        refund_tip: rtip(),
        delivery_carrier: 2 * KAS as i64,
        interval: 0,
        max_fill: 0,
        slope: 0,
        price_end: 0,
        decay_step: 0,
        amount_left: 10 * SCALE,
        custody: 10 * SCALE,
        s_ext: EXT,
    };
    if !ask {
        x.custody = x.bid_escrow(x.amount_left, 4).unwrap();
    }
    x
}

/// A sell-first pair entry of 10 whole TOKEN (A) at 1000 B per whole A with a prefund of 200 B per whole A: it holds two
/// custodies (its A and its B prefund). Its exit buys A back (limit 800, stop 1100).
fn ifd_sell_first(maker: u8) -> IfdPairState {
    let (h, p, s) = tok_fields(P);
    let b = pair_order(maker, false);
    let exit = CondPairState {
        maker: b.maker,
        side: SIDE_BID,
        s_cov_id: b.s_cov_id,
        s_tpl_hash: b.s_tpl_hash,
        s_pre: b.s_pre,
        s_suf: b.s_suf,
        s_family: b.s_family,
        s_scale: b.s_scale,
        t_cov_id: b.t_cov_id,
        t_tpl_hash: b.t_tpl_hash,
        t_pre: b.t_pre,
        t_suf: b.t_suf,
        t_family: b.t_family,
        t_ext: EXT,
        t_scale: b.t_scale,
        min_fill: SCALE,
        tip: 0,
        active_from: 0,
        expiry_daa: 499_999_999_999,
        refund_tip: rtip(),
        delivery_carrier: 2 * KAS as i64,
        tp_price: 800,
        slip_bps: 300,
        trail_step: 0,
        trail_gap: 0,
        trail_wait: 0,
        min_touch: SCALE,
        min_rest_daa: 50,
        band_daa: 300,
        keeper_tip: 5_000_000,
        stop_price: 1_100,
        armed: 0,
        amount_left: 0,
        custody: 0,
        parent: [0; 32],
        rpt_price: 0,
        rpt_pre: 0,
        rpt_until: 0,
        s_ext: EXT,
    };
    let mut e = IfdPairState {
        maker: pk(maker),
        side: SIDE_ASK,
        a_cov_id: TOKEN,
        a_tpl_hash: h,
        a_pre: p,
        a_suf: s,
        a_family: Family::Kcc20.code() as i64,
        a_scale: SCALE,
        a_ext: EXT,
        b_cov_id: TOKEN_B,
        b_tpl_hash: h,
        b_pre: p,
        b_suf: s,
        b_family: Family::Kcc20.code() as i64,
        b_scale: SCALE,
        b_ext: EXT,
        price: SCALE,
        prefund: 200,
        tip: 0,
        active_from: 0,
        expiry_daa: 400_000_000,
        refund_tip: rtip(),
        delivery_carrier: 2 * KAS as i64,
        exit_carrier: 6 * KAS as i64,
        min_fill: SCALE,
        entry_stop: 0,
        band_daa: 300,
        min_touch: SCALE,
        min_rest_daa: 50,
        keeper_tip: 5_000_000,
        armed: 0,
        amount_left: 10 * SCALE,
        custody: 0,
        rpt_amount: 0,
        exit_state: IfdPairState::commit_exit(&exit),
    };
    e.custody = e.b_custody_needed().unwrap();
    e
}

fn node_utxo(spk: &ScriptPublicKey, tag: u8, index: u32, amount: u64, cov: Option<[u8; 32]>) -> NodeUtxo {
    NodeUtxo {
        address: Some(address_of(spk, NET).unwrap()),
        txid: [tag; 32],
        index,
        amount,
        spk: spk_bytes(spk),
        daa: 1_000,
        coinbase: false,
        covenant_id: cov,
    }
}

/// A covenant-owned token state of `p` (KCC-20 with EXT, KRON without).
fn owned(p: TemplateId, amount: i64, owner: [u8; 32]) -> TokenState {
    let ext = if p.family() == Family::Kron { [0; 32] } else { EXT };
    TokenState::custody(p.family(), amount, owner, ext)
}

fn token_node_utxo(p: TemplateId, token: [u8; 32], tag: u8, amount: i64, owner: [u8; 32]) -> NodeUtxo {
    node_utxo(&owned(p, amount, owner).spk_with(token_template(p)), tag, 0, CARRIER, Some(token))
}

/// The orders of the fixture and their node outputs.
struct Fixture {
    a1: AnyState,
    a2: AnyState,
    a3_placed: AnyState,
    a3_now: AnyState,
    a5: AnyState,
    a6: AnyState,
    a7_placed: AnyState,
    a7_now: AnyState,
    /// A plain ask partly filled to 6500 base units: not on the `amountLeft - k x minFill` grid of its placed state.
    a9_placed: AnyState,
    a9_now: AnyState,
    bid_value: u64,
    utxos: Vec<NodeUtxo>,
}

fn fixture() -> Fixture {
    let a1 = AnyState::KobAsk(ask(1, 10 * SCALE));
    let b = bid(1);
    let bid_value = b.escrow(10 * SCALE, 3).unwrap() as u64;
    let a2 = AnyState::KobBid(b);
    let a3_placed = AnyState::KobAsk(ask(1, 10 * SCALE));
    let a3_now = AnyState::KobAsk(ask(1, 7 * SCALE));
    let a5 = AnyState::KobAsk(AskState { price: 260_000_000, ..ask(1, 10 * SCALE) });
    let a6 = AnyState::KobAsk(ask(2, 10 * SCALE));
    let a7_placed = AnyState::KobAsk(AskState { price: 270_000_000, ..ask(1, 10 * SCALE) });
    let a7_now = AnyState::KobAsk(AskState { price: 270_000_000, ..ask(1, 6 * SCALE) });
    let a9_placed = AnyState::KobAsk(AskState { price: 280_000_000, ..ask(1, 10 * SCALE) });
    let a9_now = AnyState::KobAsk(AskState { price: 280_000_000, ..ask(1, 6_500) });
    let mut utxos = vec![
        node_utxo(&a1.spk(), 0x11, 0, CARRIER, Some(A1)),
        token_node_utxo(P, TOKEN, 0x12, 10 * SCALE, A1),
        node_utxo(&a2.spk(), 0x21, 0, bid_value, Some(A2)),
        // a partially filled ask whose carrier is too small to pay its cancel: funding comes from the node
        node_utxo(&a3_now.spk(), 0x31, 0, 5_000, Some(A3)),
        token_node_utxo(P, TOKEN, 0x32, 7 * SCALE, A3),
        node_utxo(&a6.spk(), 0x61, 0, CARRIER, Some(A6)),
        token_node_utxo(P, TOKEN, 0x62, 10 * SCALE, A6),
        node_utxo(&a7_now.spk(), 0x71, 0, CARRIER, Some(A7)),
        token_node_utxo(P, TOKEN, 0x72, 6 * SCALE, A7),
        // the ask partly filled to 6500 base units
        node_utxo(&a9_now.spk(), 0xc1, 0, CARRIER, Some(A9)),
        token_node_utxo(P, TOKEN, 0xc2, 6_500, A9),
        // the maker's plain KAS: a dust-sized one (tried first, too small) and a sufficient one
        node_utxo(&p2pk_spk(&pk(1)), 0x92, 0, 1_000, None),
        node_utxo(&p2pk_spk(&pk(1)), 0x91, 0, 50 * KAS, None),
        // strays of A1: two of its own token, one foreign (KRON)
        token_node_utxo(P, TOKEN, 0x13, 3, A1),
        token_node_utxo(P, TOKEN, 0x14, 4, A1),
        token_node_utxo(PF, TOKEN_F, 0x15, 9, A1),
        // a stray of A2 (bid)
        token_node_utxo(P, TOKEN, 0x23, 5, A2),
    ];
    // an unrelated output at the same address as A5 but without A5's covenant id: never counts
    utxos.push(node_utxo(&a5.spk(), 0x51, 0, CARRIER, Some([0x55; 32])));
    Fixture { a1, a2, a3_placed, a3_now, a5, a6, a7_placed, a7_now, a9_placed, a9_now, bid_value, utxos }
}

fn hex_state(s: &AnyState) -> String {
    to_hex(&s.encode())
}

fn record(cov: [u8; 32], kind: &str, tpl_hash: [u8; 32], state: &str, maker: [u8; 32]) -> Value {
    json!({
        "version": 1, "network": NET, "maker": to_hex(&maker), "txid": null, "output": null, "covenantId": to_hex(&cov),
        "kind": kind, "templateHash": to_hex(&tpl_hash), "state": state, "amount": "10000", "custody": null, "value": "0",
        "placedAtUnix": "0", "placedAtDaa": "0",
    })
}

fn write(dir: &Path, name: &str, v: &Value) -> PathBuf {
    std::fs::create_dir_all(dir).unwrap();
    let p = dir.join(name);
    std::fs::write(&p, serde_json::to_string_pretty(v).unwrap()).unwrap();
    p
}

fn tmp(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("kob-cli-e2e-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    d
}

/// The backup (A1 with its custody state, A3 as placed, A5, A7 with a later `last`) and the export (A2, A6, A1 again, A9 as
/// placed, a template this build does not pin, two malformed entries).
fn files(f: &Fixture, dir: &Path) -> (PathBuf, PathBuf) {
    let ask_hash = template(TemplateId::KobAsk).hash;
    let mut r1 = record(A1, "KobAsk", ask_hash, &hex_state(&f.a1), pk(1));
    r1["custody"] = json!({"output": 1, "value": CARRIER.to_string(), "tokenState": owned(P, 10 * SCALE, A1)});
    let mut r3 = record(A3, "KobAsk", ask_hash, &hex_state(&f.a3_placed), pk(1));
    r3["ext"] = json!(to_hex(&EXT));
    let r5 = record(A5, "KobAsk", ask_hash, &hex_state(&f.a5), pk(1));
    let mut r7 = record(A7, "KobAsk", ask_hash, &hex_state(&f.a7_placed), pk(1));
    r7["ext"] = json!(to_hex(&EXT));
    r7["last"] = json!({"state": hex_state(&f.a7_now), "txid": to_hex(&[0x71; 32]), "index": 0, "daa": "1000"});
    let backup = json!({"format": "kob-backup", "version": 1, "network": NET, "exportedAt": "2026-10-02T00:00:00Z",
                        "records": [r1, r3, r5, r7, {"version": 1, "network": NET, "covenantId": "zz"}], "txs": {}});
    let entry = |cov: [u8; 32], tpl: [u8; 32], s: &str| json!({"template_hash": to_hex(&tpl), "state": s, "covenant_id": to_hex(&cov), "extension_commitment": to_hex(&EXT)});
    let export = json!({"version": 2, "network": NET, "orders": [
        entry(A2, template(TemplateId::KobBid).hash, &hex_state(&f.a2)),
        entry(A6, ask_hash, &hex_state(&f.a6)),
        entry(A1, ask_hash, &hex_state(&f.a1)),
        entry(A9, ask_hash, &hex_state(&f.a9_placed)),
        entry([0xb0; 32], [0x99; 32], "00"),
        {"template_hash": to_hex(&ask_hash), "state": "zz", "covenant_id": to_hex(&[0xb1; 32])},
        {"template_hash": to_hex(&ask_hash), "state": hex_state(&f.a1)},
    ]});
    (write(dir, "backup.json", &backup), write(dir, "export.json", &export))
}

fn by_cov(report: &Value, cov: [u8; 32]) -> Value {
    report["orders"].as_array().unwrap().iter().find(|o| o["covenant_id"] == to_hex(&cov)).cloned().unwrap_or(Value::Null)
}

/// The signed transaction a command wrote, re-validated in the engine.
fn signed_from(path: &str) -> SignedTx {
    let doc: Value = serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
    assert_eq!(doc["submit_request"]["method"], "submitTransaction");
    let s: SignedTx = serde_json::from_value(doc["signed"].clone()).unwrap();
    validate_signed(&s).unwrap_or_else(|e| panic!("{path}: {e}"));
    s
}

#[test]
fn recover_finds_cancels_and_submits() {
    let f = fixture();
    let (url, node) = mock::start(f.utxos.clone());
    let dir = tmp("recover");
    let (backup, export) = files(&f, &dir);
    let base = format!("--from {} --from {} --node {url} --maker {}", backup.display(), export.display(), to_hex(&pk(1)));

    // the search
    let a = recover::parse_args(&v(&base)).unwrap();
    let (report, ok) = recover::execute(&a).unwrap();
    assert!(ok);
    let st = |cov| by_cov(&report, cov)["status"].as_str().unwrap_or("absent").to_string();
    assert_eq!(st(A1), "live");
    assert_eq!(by_cov(&report, A1)["custody"]["status"], "verified", "the custody state of the record gives the extension");
    assert_eq!(by_cov(&report, A1)["amount_left"], "10000");
    assert_eq!((by_cov(&report, A1)["scale"].as_str(), by_cov(&report, A1)["min_fill"].as_str()), (Some("1000"), Some("1000")));
    assert_eq!(by_cov(&report, A1)["found_by"], VIA_KNOWN);
    assert_eq!(st(A2), "live");
    assert_eq!(by_cov(&report, A2)["custody"]["status"], "none");
    assert_eq!(
        by_cov(&report, A2)["amount_left"],
        bid(1).buying_power(f.bid_value as i64).to_string(),
        "a bid's amount left is its remaining buying power"
    );
    assert_eq!(st(A3), "live");
    assert_eq!(by_cov(&report, A3)["amount_left"], "7000", "found by the amountLeft - k x minFill search");
    assert_eq!(by_cov(&report, A3)["found_by"], VIA_FALLBACK);
    assert_eq!(by_cov(&report, A3)["state"], hex_state(&f.a3_now));
    assert_eq!(by_cov(&report, A3)["custody"]["amount"], (7 * SCALE).to_string());
    assert_eq!(st(A5), "not_found", "an output at the script without the covenant id does not count");
    assert!(by_cov(&report, A5)["note"].as_str().unwrap().starts_with("spent"));
    assert_eq!(st(A6), "other_maker");
    assert_eq!(st(A7), "live");
    assert_eq!(by_cov(&report, A7)["amount_left"], "6000", "the last proven state is a candidate");
    assert_eq!(by_cov(&report, A7)["found_by"], VIA_KNOWN);
    // 6500 is not on the amountLeft - k x minFill grid of the placed state: not found without a hint, and honest about it
    assert_eq!(st(A9), "not_found");
    let note = by_cov(&report, A9)["note"].as_str().unwrap().to_string();
    assert!(note.contains("--amount-left") && note.contains("not exhaustive"), "{note}");
    // a template this build does not pin is unknown: unsupported, never searched
    assert_eq!(st([0xb0; 32]), "unsupported");
    assert_eq!(by_cov(&report, [0xb0; 32])["note"], "template is not pinned by this build");
    assert_eq!(by_cov(&report, [0xb0; 32])["candidates"], 0);
    assert_eq!(report["summary"]["live"], 4);
    assert_eq!(report["summary"]["not_found"], 2);
    assert_eq!(report["rejected"].as_array().unwrap().len(), 3, "{}", report["rejected"]);
    assert_eq!(report["orders"].as_array().unwrap().len(), 8, "A1 is in both files: one order");
    assert!(by_cov(&report, A1)["source"].as_str().unwrap().contains("export.json"));
    assert!(node.lock().unwrap().submitted.is_empty());

    // --cancel --dry-run: engine-validated signed cancels written, nothing submitted
    let out = dir.join("signed");
    let a = recover::parse_args(&v(&format!(
        "{base} --cancel --dry-run --key {} --out-dir {} --out {}",
        to_hex(&sk(1)),
        out.display(),
        dir.join("report.json").display()
    )))
    .unwrap();
    let (report, ok) = recover::execute(&a).unwrap();
    assert!(ok, "{report}");
    for cov in [A1, A2, A3, A7] {
        let c = &by_cov(&report, cov)["cancel"];
        assert_eq!(c["status"], "signed", "{}: {c}", to_hex(&cov));
        let s = signed_from(c["file"].as_str().unwrap());
        assert_eq!(s.tx.inputs[0].transaction_id, by_cov(&report, cov)["outpoint"].as_str().unwrap()[..64].parse_hex());
    }
    assert!(by_cov(&report, A5).get("cancel").is_none());
    assert!(by_cov(&report, A6).get("cancel").is_none());
    assert_eq!(by_cov(&report, A1)["cancel"]["funding"], "order");
    assert_eq!(by_cov(&report, A3)["cancel"]["funding"], format!("{}:0", to_hex(&[0x91; 32])), "the smallest SUFFICIENT UTXO");
    assert!(node.lock().unwrap().submitted.is_empty(), "a dry run submits nothing");

    // submit: one transaction per live order of the key
    let a = recover::parse_args(&v(&format!("{base} --cancel --key {}", to_hex(&sk(1))))).unwrap();
    let (report, ok) = recover::execute(&a).unwrap();
    assert!(ok, "{report}");
    assert_eq!(by_cov(&report, A7)["cancel"]["status"], "submitted");
    assert!(by_cov(&report, [0xb0; 32]).get("cancel").is_none());
    assert_eq!(node.lock().unwrap().submitted.len(), 4);

    // without --maker: another maker's order is found live, and its cancel is skipped, never attempted with this key
    let a = recover::parse_args(&v(&format!("--from {} --node {url} --cancel --dry-run --key {}", export.display(), to_hex(&sk(1)))))
        .unwrap();
    let (report, ok) = recover::execute(&a).unwrap();
    assert!(ok);
    assert_eq!(by_cov(&report, A6)["status"], "live");
    assert_eq!(by_cov(&report, A6)["cancel"]["status"], "skipped");
    // the exit code path end to end, with the report file
    let code = recover::run(&v(&format!("{base} --out {}", dir.join("r2.json").display())));
    assert_eq!(code, 0);
    let r2: Value = serde_json::from_str(&std::fs::read_to_string(dir.join("r2.json")).unwrap()).unwrap();
    assert_eq!(r2["summary"]["live"], 4);
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn amount_left_hint_finds_a_partial_fill_the_fallback_misses() {
    let f = fixture();
    let (url, _node) = mock::start(f.utxos.clone());
    let dir = tmp("hint");
    let (_, export) = files(&f, &dir);
    let run = |extra: &str| {
        let a =
            recover::parse_args(&v(&format!("--from {} --node {url} --maker {} {extra}", export.display(), to_hex(&pk(1))))).unwrap();
        recover::execute(&a).unwrap().0
    };
    // no hint: the fallback grid (10000, 9000, ..., 1000) does not reach 6500
    assert_eq!(by_cov(&run(""), A9)["status"], "not_found");
    // base units or whole tokens (the order's own scale gives the decimals), for this order or for every order
    for hint in
        [format!("{}=6500", to_hex(&A9)), "6500".to_string(), format!("{}=6.5", to_hex(&A9)), "6.5".to_string(), "6.500".to_string()]
    {
        let report = run(&format!("--amount-left {hint}"));
        let o = by_cov(&report, A9);
        assert_eq!(o["status"], "live", "{hint}: {o}");
        assert_eq!(o["found_by"], VIA_HINT, "{hint}");
        assert_eq!(o["amount_left"], "6500");
        assert_eq!(o["state"], hex_state(&f.a9_now));
        assert_eq!(o["custody"]["status"], "verified");
        assert_eq!(o["custody"]["amount"], "6500");
        // the orders found without a hint stay as they were
        assert_eq!(by_cov(&report, A1)["found_by"], VIA_KNOWN);
    }
    // a hint for another order does not apply to this one
    assert_eq!(by_cov(&run(&format!("--amount-left {}=6500", to_hex(&A1))), A9)["status"], "not_found");
    // more decimals than the token has (3): ignored, and said so
    let report = run("--amount-left 6.5001");
    assert_eq!(by_cov(&report, A9)["status"], "not_found");
    assert!(by_cov(&report, A9)["note"].as_str().unwrap().contains("--amount-left ignored"), "{report}");
    // the hinted order is cancellable like any other
    let out = dir.join("signed");
    let report = run(&format!("--amount-left 6.5 --cancel --dry-run --key {} --out-dir {}", to_hex(&sk(1)), out.display()));
    let c = &by_cov(&report, A9)["cancel"];
    assert_eq!(c["status"], "signed", "{c}");
    let s = signed_from(c["file"].as_str().unwrap());
    assert_eq!(s.tx.inputs.len(), 2, "order + custody of 6500");
    // arguments
    assert!(recover::parse_args(&v("--from a.json --node ws://x:1 --amount-left abc")).is_err());
    assert!(recover::parse_args(&v("--from a.json --node ws://x:1 --amount-left 1.2.3")).is_err());
    assert!(recover::parse_args(&v("--from a.json --node ws://x:1 --amount-left zz=5")).is_err());
    assert!(recover::parse_args(&v("--from a.json --node ws://x:1 --amount-left")).is_err());
    let a =
        recover::parse_args(&v(&format!("--from a.json --node ws://x:1 --amount-left {}=1.5 --amount-left 7", to_hex(&A9)))).unwrap();
    assert_eq!(a.amount_left, vec![(Some(A9), "1.5".to_string()), (None, "7".to_string())]);
    let _ = std::fs::remove_dir_all(dir);
}

trait ParseHex {
    fn parse_hex(&self) -> [u8; 32];
}
impl ParseHex for str {
    fn parse_hex(&self) -> [u8; 32] {
        kob_protocol::json::hex32(self).unwrap()
    }
}

fn tview(p: TemplateId, token: [u8; 32], tag: u8, amount: i64, owner: [u8; 32], foreign: bool, proven: bool) -> Value {
    let mut t = json!({
        "txid": to_hex(&[tag; 32]), "index": 0, "token": to_hex(&token), "owner": to_hex(&owner), "amount": amount.to_string(),
        "value": CARRIER.to_string(), "role": "stray", "created_daa": 1000, "spent": false, "settled": true,
    });
    if proven {
        t["state"] = serde_json::to_value(owned(p, amount, owner)).unwrap();
        t["program"] = json!(p.name());
    }
    if foreign {
        t["foreign"] = json!(true);
    }
    t
}

fn any_json(s: &AnyState) -> Value {
    serde_json::to_value(s).unwrap()
}

/// An indexer order view; `state` is the state JSON (`{"kind": .., "state": {..}}`).
fn view(
    cov: [u8; 32],
    tpl_hash: [u8; 32],
    state: Value,
    cur: (u8, u32),
    value: u64,
    custody: Option<Value>,
    strays: Vec<Value>,
) -> Value {
    let mut v = json!({
        "covenant_id": to_hex(&cov), "template_hash": to_hex(&tpl_hash), "status": "open", "state_known": true,
        "state": state,
        "current": {"txid": to_hex(&[cur.0; 32]), "index": cur.1, "value": value.to_string()},
        "current_daa": 1000, "extension_commitment": to_hex(&EXT), "strays": strays,
    });
    if let Some(c) = custody {
        v["custody"] = json!({"utxo": c, "ok": true});
    }
    v
}

fn a1_view(f: &Fixture, cur: (u8, u32)) -> Value {
    let mut custody = tview(P, TOKEN, 0x12, 10 * SCALE, A1, false, true);
    custody["role"] = json!("custody");
    view(
        A1,
        template(TemplateId::KobAsk).hash,
        any_json(&f.a1),
        cur,
        CARRIER,
        Some(custody),
        vec![
            tview(P, TOKEN, 0x13, 3, A1, false, true),
            tview(P, TOKEN, 0x14, 4, A1, false, true),
            tview(PF, TOKEN_F, 0x15, 9, A1, true, true),
            tview(P, TOKEN, 0x16, 1, A1, false, false),
            tview(PF, [0x73; 32], 0x17, 1, A1, true, false),
        ],
    )
}

#[test]
fn order_sweep_and_cancel_from_views() {
    let f = fixture();
    let (url, node) = mock::start(f.utxos.clone());
    let dir = tmp("order");
    let key = to_hex(&sk(1));
    let sweep = |file: &Path, extra: &str| {
        let a = order::parse_args(
            order::Op::Sweep,
            &v(&format!("--view {} --key {key} --node {url} --out-dir {} {extra}", file.display(), dir.join("out").display())),
        )
        .unwrap();
        order::execute(&a).unwrap()
    };

    // an ask with own and foreign strays: swept in place, paid by its carrier
    let p = write(&dir, "a1.json", &a1_view(&f, (0x11, 0)));
    let (report, ok) = sweep(&p, "--dry-run");
    assert!(ok, "{report}");
    let o = &report["orders"][0];
    assert_eq!(o["status"], "signed", "{o}");
    assert_eq!(o["funding"], "order");
    assert_eq!(o["returned"].as_array().unwrap().len(), 2, "own token and the foreign token: {o}");
    assert_eq!(o["left_behind"].as_array().unwrap().len(), 2, "unproven stray and foreign stray without a program: {o}");
    assert!(o["disclosure"].as_str().unwrap().contains("90-day idle window restarts"));
    let s = signed_from(o["file"].as_str().unwrap());
    assert_eq!(s.tx.outputs[0].script_public_key, spk_to_string(&f.a1.spk()), "output 0 continues the order");
    assert_eq!(s.tx.outputs[0].covenant.as_ref().unwrap().covenant_id, A1);
    assert_eq!(s.tx.inputs.len(), 4, "order + 2 own strays + 1 foreign, no custody, no funding");

    // a bid's sweep needs funding: the smallest sufficient plain KAS UTXO of the maker from the node
    let bid_view = view(
        A2,
        template(TemplateId::KobBid).hash,
        any_json(&f.a2),
        (0x21, 0),
        f.bid_value,
        None,
        vec![tview(P, TOKEN, 0x23, 5, A2, false, true)],
    );
    let (report, ok) = sweep(&write(&dir, "a2.json", &bid_view), "--dry-run");
    assert!(ok, "{report}");
    let o = &report["orders"][0];
    assert_eq!(o["funding"], format!("{}:0", to_hex(&[0x91; 32])));
    let s = signed_from(o["file"].as_str().unwrap());
    assert_eq!(s.tx.outputs[0].script_public_key, spk_to_string(&f.a2.spk()));
    assert_eq!(s.tx.outputs[0].value, f.bid_value, "a funded sweep keeps the bid's escrow");

    // a stale view (the order moved on): refused, nothing built
    let (report, ok) = sweep(&write(&dir, "stale.json", &a1_view(&f, (0x11, 5))), "--dry-run");
    assert!(!ok);
    assert_eq!(report["orders"][0]["status"], "refused");
    assert!(report["orders"][0]["reason"].as_str().unwrap().contains("stale view"));
    assert_eq!(order::run(&v(&format!("sweep --view {} --key {key} --node {url} --dry-run", dir.join("stale.json").display()))), 1);

    // a page: the key's order is swept and submitted, another maker's is skipped
    let other = view(
        A6,
        template(TemplateId::KobAsk).hash,
        any_json(&f.a6),
        (0x61, 0),
        CARRIER,
        None,
        vec![tview(P, TOKEN, 0x63, 1, A6, false, true)],
    );
    let page = json!({"items": [a1_view(&f, (0x11, 0)), other], "next_cursor": null});
    let (report, ok) = sweep(&write(&dir, "page.json", &page), "");
    assert!(ok, "{report}");
    assert_eq!(report["orders"][0]["status"], "submitted");
    assert_eq!(report["orders"][1]["status"], "skipped");
    assert_eq!(node.lock().unwrap().submitted.len(), 1);

    // the maker's cancel from the view: custody, own and foreign strays back to the maker
    let a = order::parse_args(
        order::Op::Cancel,
        &v(&format!("--view {} --key {key} --node {url} --dry-run --out-dir {}", p.display(), dir.join("out").display())),
    )
    .unwrap();
    let (report, ok) = order::execute(&a).unwrap();
    assert!(ok, "{report}");
    let o = &report["orders"][0];
    assert_eq!(o["status"], "signed", "{o}");
    let s = signed_from(o["file"].as_str().unwrap());
    assert_eq!(s.tx.inputs.len(), 5, "order + custody + 2 own strays + 1 foreign");
    assert!(o["disclosure"].as_str().unwrap().contains("the custody (10000 units)"));

    // an order of a template this build does not pin is unknown: neither cancelled nor swept
    let mut unpinned = a1_view(&f, (0x11, 0));
    unpinned["template_hash"] = json!(to_hex(&[0x98; 32]));
    let up = write(&dir, "unpinned.json", &unpinned);
    let a = order::parse_args(
        order::Op::Cancel,
        &v(&format!("--view {} --key {key} --node {url} --dry-run --out-dir {}", up.display(), dir.join("out").display())),
    )
    .unwrap();
    let (report, _) = order::execute(&a).unwrap();
    assert_eq!(report["orders"][0]["status"], "skipped");
    assert_eq!(report["orders"][0]["reason"], "template is not pinned by this build");
    let (report, _) = sweep(&up, "--dry-run");
    assert_eq!(report["orders"][0]["status"], "skipped");

    // a key that is not the maker builds nothing
    let a =
        order::parse_args(order::Op::Cancel, &v(&format!("--view {} --key {} --node {url} --dry-run", p.display(), to_hex(&sk(2)))))
            .unwrap();
    let (report, _) = order::execute(&a).unwrap();
    assert_eq!(report["orders"][0]["status"], "skipped");
    // a backup is not a view
    let (b, _) = files(&f, &dir);
    let a = order::parse_args(order::Op::Sweep, &v(&format!("--view {} --key {key} --node {url}", b.display()))).unwrap();
    assert!(order::execute(&a).is_err());
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn input_formats_and_bad_input() {
    let f = fixture();
    let dir = tmp("inputs");
    let (backup, export) = files(&f, &dir);
    let p = inputs::read_file(&backup, NET).unwrap();
    assert_eq!(p.entries.len(), 4);
    assert_eq!(p.rejected.len(), 1);
    let a7 = p.entries.iter().find(|e| e.covenant_id == A7).unwrap();
    assert_eq!(a7.spans, vec![f.a7_now.encode(), f.a7_placed.encode()], "the last proven state first");
    assert!(a7.later);
    let a1 = p.entries.iter().find(|e| e.covenant_id == A1).unwrap();
    assert_eq!(a1.ext, Some(EXT), "from the custody token state");
    assert_eq!(p.entries.iter().find(|e| e.covenant_id == A5).unwrap().ext, None);
    assert!(inputs::read_file(&backup, "mainnet").unwrap_err().contains("network"));
    let e = inputs::read_file(&export, NET).unwrap();
    assert_eq!((e.entries.len(), e.rejected.len()), (5, 2));
    assert!(e.rejected.iter().any(|r| r.reason.contains("covenant_id")));
    // a view and a page
    let p = inputs::parse(&a1_view(&f, (0x11, 0)), "v", NET).unwrap();
    assert_eq!(p.views.len(), 1);
    let vw = &p.views[0];
    assert_eq!(vw.state.as_ref(), Some(&f.a1));
    assert_eq!(vw.strays.len(), 5);
    assert_eq!(vw.strays[2].program, Some(PF));
    assert!(vw.strays[2].foreign && vw.strays[3].state.is_none());
    assert_eq!(vw.custody.as_ref().unwrap().amount, 10 * SCALE);
    assert_eq!(vw.entry().spans, vec![f.a1.encode()]);
    let mut bad = a1_view(&f, (0x11, 0));
    bad["state"] = json!({"kind": "KobAsk", "state": {"maker": "nothex"}});
    let page = inputs::parse(&json!({"items": [a1_view(&f, (0x11, 0)), bad]}), "page", NET).unwrap();
    assert_eq!((page.views.len(), page.rejected.len()), (1, 1));
    let mut bad = a1_view(&f, (0x11, 0));
    bad["strays"][0]["program"] = json!("KobAsk");
    assert_eq!(inputs::parse(&bad, "v", NET).unwrap().rejected.len(), 1, "a stray's program must be a token program");
    // an unproven view has no state
    let mut unproven = a1_view(&f, (0x11, 0));
    unproven["state_known"] = json!(false);
    assert!(inputs::parse(&unproven, "v", NET).unwrap().views[0].state.is_none());
    // documents that are none of the three
    for doc in [
        json!([1, 2]),
        json!({"hello": 1}),
        json!({"format": "kob-backup", "version": 2, "network": NET, "records": []}),
        json!({"version": 3, "network": NET, "orders": []}),
        json!({"version": 2, "orders": []}),
        json!({"items": 5}),
    ] {
        assert!(inputs::parse(&doc, "x", NET).is_err(), "{doc}");
    }
    std::fs::write(dir.join("nope.json"), "not json").unwrap();
    assert!(inputs::read_file(&dir.join("nope.json"), NET).unwrap_err().contains("not JSON"));
    assert!(inputs::read_file(&dir.join("missing.json"), NET).is_err());
    // merging: one order per covenant id, every distinct span kept, the later entry first
    let mut all = inputs::read_file(&backup, NET).unwrap().entries;
    all.extend(inputs::read_file(&export, NET).unwrap().entries);
    let (merged, notes) = inputs::merge(all);
    assert_eq!(merged.len(), 8);
    assert!(notes.is_empty());
    assert_eq!(merged.iter().find(|e| e.covenant_id == A1).unwrap().spans.len(), 1);
    // arguments
    assert!(recover::parse_args(&v("--node ws://x:1")).is_err(), "needs --from");
    assert!(recover::parse_args(&v("--from a.json")).is_err(), "needs --node");
    assert!(recover::parse_args(&v("--from a.json --node ws://x:1 --cancel")).is_err(), "--cancel needs --key");
    assert!(recover::parse_args(&v("--from a.json --node ws://x:1 --dry-run")).is_err(), "--dry-run belongs to --cancel");
    assert!(recover::parse_args(&v("--from a.json --node ws://x:1 --network moon")).is_err());
    assert!(recover::parse_args(&v("--from a.json --node ws://x:1 --fee-rate 5 --cancel --key 00")).is_err());
    assert!(recover::parse_args(&v("--from a.json --node ws://x:1 --bogus")).is_err());
    assert!(recover::parse_args(&v("--from a.json --node ws://x:1 --lots 3")).is_err(), "no lot flags remain");
    assert!(order::parse_args(order::Op::Sweep, &v("--view a.json --node ws://x:1")).is_err(), "needs --key");
    assert!(order::parse_args(order::Op::Sweep, &v("--view a.json --key 00")).is_err(), "needs --node");
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn candidate_search() {
    let tpl = TemplateId::KobAsk;
    let placed = AnyState::KobAsk(ask(1, 10 * SCALE));
    let amounts = |c: &[recover::Candidate]| c.iter().map(|c| c.state.amount_left().unwrap()).collect::<Vec<_>>();
    // the known state, then amountLeft - k x minFill (minFill = 1000): 9000 .. 1000
    let c = recover::candidates(tpl, std::slice::from_ref(&placed), &[]);
    assert_eq!(c.len(), 10, "known + 9000 .. 1000");
    assert_eq!(c[0].state, placed);
    assert_eq!(c[0].via, VIA_KNOWN);
    assert_eq!(amounts(&c), (1..=10).rev().map(|k| k * SCALE).collect::<Vec<_>>());
    assert!(c[1..].iter().all(|c| c.via == VIA_FALLBACK));
    assert_eq!(c[9].state.amount_left(), Some(SCALE));
    // the maker's hints come right after the known states, before the fallback; a hint on the grid is not repeated
    let c = recover::candidates(tpl, std::slice::from_ref(&placed), &[6_500, 7_000]);
    assert_eq!(c.len(), 11, "known + two hints + the fallback grid without the 7000 the hint already gave");
    assert_eq!(amounts(&c)[..3], [10_000, 6_500, 7_000]);
    assert_eq!((c[1].via, c[2].via), (VIA_HINT, VIA_HINT));
    assert!(c[3..].iter().all(|c| c.via == VIA_FALLBACK));
    // a later state first, no duplicate scripts
    let later = AnyState::KobAsk(ask(1, 6 * SCALE));
    let c = recover::candidates(tpl, &[later.clone(), placed.clone()], &[]);
    assert_eq!(c.len(), 10);
    assert_eq!((c[0].state.amount_left(), c[1].state.amount_left()), (Some(6000), Some(10000)));
    // capped
    let big = AnyState::KobAsk(ask(1, 1_000 * SCALE));
    assert_eq!(recover::candidates(tpl, &[big], &[]).len(), recover::MAX_CANDIDATES);
    // a bid has no amount left (its quantity is its escrow) and a fixed script: one candidate, hints or not
    let bid_state = AnyState::KobBid(bid(1));
    assert_eq!(recover::candidates(TemplateId::KobBid, std::slice::from_ref(&bid_state), &[5_000]).len(), 1);
    assert!(crate::common::script_is_fixed(&bid_state));
    assert!(!crate::common::script_is_fixed(&placed));
    assert!(crate::common::amount_is_the_only_mutable_field(TemplateId::KobAsk));
    assert!(!crate::common::amount_is_the_only_mutable_field(TemplateId::KobCondAsk));
    // template resolution: only the order templates this build pins
    assert_eq!(order_template(&template(TemplateId::KobAsk).hash), Some(TemplateId::KobAsk));
    assert_eq!(order_template(&token_template(P).hash), None, "a token program is not an order template");
    assert_eq!(order_template(&[0x99; 32]), None);
    // a non-canonical or foreign span does not decode, and a state is encoded only under the template of its own kind
    assert!(decode_state(tpl, &[0u8; 3]).is_err());
    assert_eq!(decode_state(tpl, &placed.encode()).unwrap(), placed);
    assert!(encode_state(TemplateId::KobBid, &placed).is_err());
    // the custody a state implies: KCC-20 needs the extension commitment, a bid none
    assert!(recover::custody_target(&placed, A1, None).is_err());
    let (st, _) = recover::custody_target(&placed, A1, Some(EXT)).unwrap().unwrap();
    assert_eq!((st.amount(), st.owner()), (10 * SCALE, A1));
    assert!(recover::custody_target(&bid_state, A2, None).unwrap().is_none());
}

/// Pair orders (protocol v3): a sell-first `KobIfdPair` holds TWO custodies (its A and its B prefund) and a `KobPair` BID a
/// custody of token B. `kob order cancel` returns both custodies and a stray of token B (the order's own token) from an
/// indexer view; `kob recover` finds the entry and verifies both custodies, and its cancel carries the prefund.
#[test]
fn pair_orders_cancel_from_views_and_recover_with_two_custodies() {
    const B1: [u8; 32] = [0xb2; 32];
    const B2: [u8; 32] = [0xb3; 32];
    let e = ifd_sell_first(1);
    let prefund = e.custody;
    assert_eq!(prefund, 2_009);
    let ev = AnyState::KobIfdPair(e.clone());
    let e_value = e.kas_value().unwrap() as u64 + CARRIER;
    let y = AnyState::KobPair(pair_order(1, false));
    let y_escrow = match &y {
        AnyState::KobPair(p) => p.custody,
        _ => unreachable!(),
    };
    let utxos = vec![
        node_utxo(&ev.spk(), 0xd1, 0, e_value, Some(B1)),
        token_node_utxo(P, TOKEN, 0xd2, 10 * SCALE, B1),
        token_node_utxo(P, TOKEN_B, 0xd3, prefund, B1),
        token_node_utxo(P, TOKEN_B, 0xd4, 7, B1),
        node_utxo(&y.spk(), 0xe1, 0, CARRIER + 8 * KAS, Some(B2)),
        token_node_utxo(P, TOKEN_B, 0xe2, y_escrow, B2),
        node_utxo(&p2pk_spk(&pk(1)), 0x91, 0, 50 * KAS, None),
    ];
    let (url, node) = mock::start(utxos);
    let dir = tmp("pair");
    let key = to_hex(&sk(1));

    // the entry's view: the A custody at `custody`, the B prefund at `pair.custodies[1]`, a stray of B
    let mut ca = tview(P, TOKEN, 0xd2, 10 * SCALE, B1, false, true);
    ca["role"] = json!("custody");
    let mut cb = tview(P, TOKEN_B, 0xd3, prefund, B1, false, true);
    cb["role"] = json!("custody");
    let mut bv = view(
        B1,
        template(TemplateId::KobIfdPair).hash,
        any_json(&ev),
        (0xd1, 0),
        e_value,
        Some(ca.clone()),
        vec![tview(P, TOKEN_B, 0xd4, 7, B1, false, true)],
    );
    bv["pair"] = json!({"custodies": [{"token": to_hex(&TOKEN), "utxo": ca}, {"token": to_hex(&TOKEN_B), "utxo": cb}]});
    let bp = write(&dir, "b1.json", &bv);
    let cancel_args = |file: &Path| {
        order::parse_args(
            order::Op::Cancel,
            &v(&format!("--view {} --key {key} --node {url} --dry-run --out-dir {}", file.display(), dir.join("out").display())),
        )
        .unwrap()
    };
    let (report, ok) = order::execute(&cancel_args(&bp)).unwrap();
    assert!(ok, "{report}");
    let o = &report["orders"][0];
    assert_eq!((o["status"].as_str(), o["kind"].as_str()), (Some("signed"), Some("KobIfdPair")), "{o}");
    let s = signed_from(o["file"].as_str().unwrap());
    let spent: Vec<([u8; 32], u32)> = s.tx.inputs.iter().map(|i| (i.transaction_id, i.index)).collect();
    for tag in [0xd1u8, 0xd2, 0xd3, 0xd4] {
        assert!(spent.contains(&([tag; 32], 0)), "{tag:#x} spent: {o}");
    }
    let d = o["disclosure"].as_str().unwrap();
    assert!(d.contains("the custody (10000 units of") && d.contains("the custody (2009 units of"), "{d}");
    // a view without the prefund custody is refused (the cancel must return both)
    let mut no_b = bv.clone();
    no_b["pair"] = json!({"custodies": [{"token": to_hex(&TOKEN), "utxo": no_b["custody"]["utxo"].clone()}]});
    let (report, ok) = order::execute(&cancel_args(&write(&dir, "b1x.json", &no_b))).unwrap();
    assert!(!ok);
    assert_eq!(report["orders"][0]["status"], "refused");

    // a pair BID: its custody is the B escrow (token B's program)
    let mut cy = tview(P, TOKEN_B, 0xe2, y_escrow, B2, false, true);
    cy["role"] = json!("custody");
    let yv = view(B2, template(TemplateId::KobPair).hash, any_json(&y), (0xe1, 0), CARRIER + 8 * KAS, Some(cy), vec![]);
    let (report, ok) = order::execute(&cancel_args(&write(&dir, "b2.json", &yv))).unwrap();
    assert!(ok, "{report}");
    let s = signed_from(report["orders"][0]["file"].as_str().unwrap());
    assert!(s.tx.inputs.iter().any(|i| i.transaction_id == [0xe2; 32]), "the escrow custody is spent");

    // recover: an export entry of each; both custodies of the entry verified, the cancel carries the prefund
    let entry = |cov: [u8; 32], tpl: [u8; 32], s: &AnyState| json!({"template_hash": to_hex(&tpl), "state": hex_state(s), "covenant_id": to_hex(&cov), "extension_commitment": to_hex(&EXT)});
    let export = json!({"version": 2, "network": NET, "orders": [
        entry(B1, template(TemplateId::KobIfdPair).hash, &ev),
        entry(B2, template(TemplateId::KobPair).hash, &y),
    ]});
    let ep = write(&dir, "export.json", &export);
    let a = recover::parse_args(&v(&format!(
        "--from {} --node {url} --maker {} --cancel --dry-run --key {key} --out-dir {}",
        ep.display(),
        to_hex(&pk(1)),
        dir.join("signed").display()
    )))
    .unwrap();
    let (report, ok) = recover::execute(&a).unwrap();
    assert!(ok, "{report}");
    let b1 = by_cov(&report, B1);
    assert_eq!((b1["status"].as_str(), b1["custody"]["status"].as_str()), (Some("live"), Some("verified")), "{b1}");
    assert_eq!((b1["prefund"]["status"].as_str(), b1["prefund"]["amount"].as_str()), (Some("verified"), Some("2009")), "{b1}");
    assert_eq!(b1["cancel"]["status"], "signed", "{b1}");
    let s = signed_from(b1["cancel"]["file"].as_str().unwrap());
    let spent: Vec<[u8; 32]> = s.tx.inputs.iter().map(|i| i.transaction_id).collect();
    assert!(spent.contains(&[0xd2; 32]) && spent.contains(&[0xd3; 32]), "both custodies");
    let b2 = by_cov(&report, B2);
    assert_eq!((b2["status"].as_str(), b2["custody"]["status"].as_str()), (Some("live"), Some("verified")), "{b2}");
    assert!(b2.get("prefund").is_none());
    assert_eq!(b2["cancel"]["status"], "signed");
    assert!(node.lock().unwrap().submitted.is_empty());
    let _ = std::fs::remove_dir_all(dir);
}
