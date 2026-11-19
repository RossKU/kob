//! P-L4: a `KobCurveAsk` state with no level (or more than one push holds) that an indexer serves
//! is an error of the JSON entries, never a panic, which in the browser would trap the wasm module.
use kob_protocol::artifacts::{token_template, TemplateId};
use kob_protocol::build::{Action, CancelOrder};
use kob_protocol::state::{AnyState, CurveAskState, TokenState};
use kob_protocol::tx::{FeeOptions, KeyUtxo, OrderUtxo, TokenUtxo, Utxo};
use kob_protocol::Family;
use kob_wasm::api;
use serde_json::{json, Value};

/// A valid x-only key (the x coordinate of G).
const M: &str = "79be667ef9dcbbac55a06295ce870b07029bfcdb2dce28d959f2815b16f81798";
const TOKEN: [u8; 32] = [0x70; 32];
const CURVE: [u8; 32] = [0xc5; 32];
const EXT: [u8; 32] = [0xee; 32];
const KAS: u64 = 100_000_000;

fn utxo(tag: u8, amount: u64, cov: Option<[u8; 32]>) -> Utxo {
    Utxo { transaction_id: [tag; 32], index: tag as u32, amount, block_daa_score: 1_000, covenant_id: cov }
}

fn curve() -> CurveAskState {
    let t = token_template(TemplateId::Kcc20Ref);
    CurveAskState {
        maker: kob_protocol::json::hex32(M).unwrap(),
        token_cov_id: TOKEN,
        token_tpl_hash: t.hash,
        tpl_prefix_len: t.prefix.len() as i64,
        tpl_suffix_len: t.suffix.len() as i64,
        scale: 1_000,
        min_fill: 1,
        tip: 0,
        active_from: 0,
        level_size: 1_000,
        total: 4_000,
        amount_left: 4_000,
        extension_commitment: EXT,
        table: vec![250_000_000, 251_000_000, 252_000_000, 253_000_000],
    }
}

/// The maker's cancel of the curve, as the JSON the web app sends.
fn cancel() -> Value {
    let s = curve();
    let c = CancelOrder {
        order: OrderUtxo { utxo: utxo(40, 10 * KAS, Some(CURVE)), state: AnyState::KobCurveAsk(s.clone()) },
        retired: None,
        custody: Some(TokenUtxo {
            utxo: utxo(41, 10 * KAS, Some(TOKEN)),
            state: TokenState::custody(Family::Kcc20, s.amount_left, CURVE, EXT),
        }),
        prefund: None,
        strays: vec![],
        foreign: vec![],
        tokens: vec![],
        funding: vec![KeyUtxo { utxo: utxo(45, 5 * KAS, None), pubkey: s.maker }],
        change: None,
        replace: None,
        lock_time: 1_000_000,
        records: vec![],
        fee: FeeOptions::default(),
    };
    serde_json::to_value(Action::CancelOrder(c)).unwrap()
}

#[test]
fn an_empty_curve_table_is_an_error_of_the_json_entries() {
    let ok = cancel();
    api::build(&ok.to_string()).expect("the cancel of the curve builds");
    let state = json!({ "kind": "KobCurveAsk", "state": ok["order"]["state"]["state"].clone() });
    api::script_public_key(&state.to_string()).expect("its address");
    for table in [json!([]), Value::Array(vec![json!("250000000"); 65_535 / 8 + 1])] {
        let mut bad = ok.clone();
        bad["order"]["state"]["state"]["table"] = table.clone();
        let e = api::build(&bad.to_string()).expect_err("an empty or oversized table is refused");
        assert!(e.contains("table"), "{e}");
        let mut s = state.clone();
        s["state"]["table"] = table;
        let e = api::script_public_key(&s.to_string()).expect_err("no address for it");
        assert!(e.contains("table"), "{e}");
    }
}
