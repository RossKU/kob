//! A dump of every builder shape as JSON, for external replay tools (a red-team harness that attacks the exact bytes).
//!
//! Every builder scenario of the golden-vector set on every token program (with the delivery-mode scenarios, IOC / FOK ends
//! of a mode-1 bid holding its bought-custody, and the claims CL1 to CL3), the pair scenarios and the pair delivery grid on the program pairs of
//! `carrier_fees.rs`, at the default order carrier: built, signed with the fixture keys (`[n; 32]`, throwaway test keys that
//! hold nothing on any network), budgets tightened and validated in the engine. Each record carries the request, the
//! signed transaction in kaspa-wasm "safe JSON" (each input with the UTXO it spends: amount, script public key, DAA score,
//! covenant id) and the same transaction and spent entries as rusty-kaspa's own serde form. `keys.json` lists the keys.
//! The router intents are dumped by `intent_builders.rs` (`dump_router_intents`). `#[ignore]`d:
//!
//! ```text
//! KOB_TX_DUMP_DIR=target/kob-tx-dump cargo test -p kob-protocol --test tx_dump -- --ignored --nocapture
//! ```

mod common;

use std::path::PathBuf;

use common::{with_carriers, Carriers};
use kob_protocol::artifacts::TemplateId;
use kob_protocol::build::{build, Action};
use kob_protocol::defaults::DEFAULT_ORDER_CARRIER;
use kob_protocol::tx::{finalize, sign_locally, FinalizeOptions};
use serde_json::{json, Value};

/// The program pairs of the pair shapes (as `carrier_fees.rs`).
const PAIRS: [(TemplateId, TemplateId); 8] = [
    (TemplateId::Kcc20Ref, TemplateId::Kcc20Ref),
    (TemplateId::Kcc20Ref8x8, TemplateId::Kcc20Ref8x8),
    (TemplateId::Kcc20P2, TemplateId::Kcc20P2),
    (TemplateId::Kcc20KaspaCom025, TemplateId::Kcc20KaspaCom025),
    (TemplateId::Kcc20PublicMint, TemplateId::Kcc20Ref),
    (TemplateId::Kcc20Ref, TemplateId::KronToken2433),
    (TemplateId::KronToken2433, TemplateId::Kcc20Ref),
    (TemplateId::KronToken2433, TemplateId::KronToken2732),
];

fn shapes() -> Vec<(String, Action)> {
    let mut v = vec![];
    for p in common::PROGRAMS {
        v.extend(common::scenarios_on(p).into_iter().map(|(n, a)| (format!("{}/{n}", p.name()), a)));
    }
    for (pa, pb) in PAIRS {
        let tag = common::pair::pair_name(pa, pb);
        v.extend(common::pair::pair_scenarios(pa, pb).into_iter().map(|(n, a)| (format!("{tag}/{n}"), a)));
    }
    // the pair delivery grid (mode-1 fills and refunds releasing a bought-custody, mode-2 top-ups and new receive UTXOs)
    for (pa, pb) in PAIRS {
        let tag = common::pair::pair_name(pa, pb);
        v.extend(common::pair::pair_delivery_grid(pa, pb).into_iter().map(|(n, a)| (format!("{tag}/dlv.{n}"), a)));
    }
    // IOC / FOK ends of a mode-1 bid holding its bought-custody
    for p in common::PROGRAMS.into_iter().filter(|p| p.family() == kob_protocol::Family::Kcc20) {
        v.extend(ioc_custody(p).into_iter().map(|(n, a)| (format!("{}/{n}", p.name()), a)));
    }
    // the budget grids measure script units, not shapes (thousands of builds)
    v.retain(|(n, _)| !n.contains("/grid."));
    v
}

/// A mode-1 bid holding its bought-custody, filled IOC (partly) and FOK: the fill ends it and releases the custody.
fn ioc_custody(t3: TemplateId) -> Vec<(String, Action)> {
    use common::*;
    use kob_protocol::state::*;
    let held = || bid_dlv(Some(bought_custody(23, 4 * WHOLE, cov(0xb1), 1_500)), None, None);
    [(TIF_IOC, "ioc", 3), (TIF_FOK, "fok", 6)]
        .into_iter()
        .map(|(tif, name, n)| {
            let s = BidState { tif, ..mode_bid(bid(MAKER_B, P245, t3), MODE_ACCUMULATE, 4 * WHOLE) };
            let value = s.escrow(6 * WHOLE, 1).unwrap() as u64;
            (format!("take.bid.acc.{name}"), Action::Batch(take_bid_valued(s, n, value, held())))
        })
        .collect()
}

/// The claims of a bought-custody (bid-delivery-spec section 7): CL1, CL2, CL3 continuing and ending of a `KobBid`, CL1 and
/// CL3 ending of a `KobCondBid`, CL1 and CL2 of a `KobPair` / `KobCondPair` ask, on every KCC-20 program. Maker-signed.
fn claims() -> Vec<(String, kob_protocol::build::ClaimOrder)> {
    use common::pair::{cond_pair_ask, pair, pv, tutxo, RATE};
    use common::*;
    use kob_protocol::build::{ClaimOrder, Replacement};
    use kob_protocol::state::*;
    use kob_protocol::tx::{OrderUtxo, TokenUtxo};
    let claim = |order: OrderUtxo<AnyState>, custody: TokenUtxo, tokens: Vec<TokenUtxo>, place: Option<AnyState>, end: bool| {
        let maker = order.state.maker();
        let key = (1..=40u8).find(|k| pk(*k) == maker).unwrap();
        ClaimOrder {
            order,
            bought_custody: custody,
            tokens,
            place: place.map(|o| Replacement { order: o, value: CARRIER, token_carrier: None, deadline: None }),
            end,
            value: None,
            token_carrier: None,
            funding: vec![key_utxo(30, key, 100 * KAS)],
            change: None,
            lock_time: 0,
            records: vec![],
            fee: fee(),
        }
    };
    let mut v = vec![];
    for p in PROGRAMS.into_iter().filter(|p| p.family() == kob_protocol::Family::Kcc20) {
        let n = p.name();
        let (b_id, e_id, x_id) = ([0xb1; 32], [0xe1; 32], [0x83; 32]);
        let a4 = mode_bid(bid(MAKER_B, P245, p), MODE_ACCUMULATE, 4 * WHOLE);
        let o = order(20, a4.escrow(10 * WHOLE, 1).unwrap() as u64, b_id, 1_000, AnyState::KobBid(a4));
        let c = bought_custody(23, 4 * WHOLE, b_id, 1_500);
        let wallet = |tag: u8| tok(tag, WHOLE, pk(MAKER_B), SCHEME_P2PK, 1_000);
        let max_in = p.token_slots().unwrap().0.min(kob_protocol::Family::Kcc20.max_tok_in());
        v.push((format!("{n}/claim.bid.CL1"), claim(o.clone(), c.clone(), vec![], None, false)));
        let w: Vec<TokenUtxo> = (0..max_in.saturating_sub(1)).map(|k| wallet(40 + k as u8)).collect();
        v.push((format!("{n}/claim.bid.CL2"), claim(o.clone(), c.clone(), w, None, false)));
        for (end, nm) in [(false, "CL3"), (true, "CL3end")] {
            let ask3 = AnyState::KobAsk(ask_n(MAKER_B, P250, p, 3));
            v.push((format!("{n}/claim.bid.{nm}"), claim(o.clone(), c.clone(), vec![wallet(41)], Some(ask3), end)));
        }
        let c4 = CondBidState { amount_left: 6 * WHOLE, ..mode_cond_bid(cond_bid(MAKER_B, p), MODE_ACCUMULATE, 4 * WHOLE) };
        let co = order(40, c4.escrow(1).unwrap() as u64, e_id, 2_000, AnyState::KobCondBid(c4));
        let cc = bought_custody(43, 4 * WHOLE, e_id, 2_500);
        v.push((format!("{n}/claim.condBid.CL1"), claim(co.clone(), cc.clone(), vec![], None, false)));
        v.push((format!("{n}/claim.condBid.CL3end"), claim(co, cc, vec![], Some(AnyState::KobAsk(ask_n(MAKER_B, P250, p, 4))), true)));
        if p.serves_pairs() {
            let kp = PairState { mode: MODE_ACCUMULATE, bought: 3 * WHOLE, ..pair(MAKER_A, true, p, p, RATE) };
            let kc = CondPairState { mode: MODE_ACCUMULATE, bought: 3 * WHOLE, ..cond_pair_ask(MAKER_A, p, p) };
            for (nm, st) in [("pair", AnyState::KobPair(kp)), ("condPair", AnyState::KobCondPair(kc))] {
                let o = order(70, pv(), x_id, 1_000, st);
                let c = tutxo(p, TOKEN_B, 60, 3 * WHOLE, x_id, true);
                for k in [0usize, 1] {
                    let w: Vec<TokenUtxo> = (0..k).map(|i| tutxo(p, TOKEN_B, 61 + i as u8, WHOLE, pk(MAKER_A), false)).collect();
                    v.push((format!("{n}/claim.{nm}.CL{}", k + 1), claim(o.clone(), c.clone(), w, None, false)));
                }
            }
        }
    }
    v
}

fn record(name: &str, action: &Action) -> Result<Value, String> {
    record_built(name, &serde_json::to_value(action).unwrap(), build(action).map_err(|e| e.to_string())?)
}

fn record_built(name: &str, request: &Value, built: kob_protocol::tx::BuiltTx) -> Result<Value, String> {
    let sigs = sign_locally(&built, &common::keys()).map_err(|e| e.to_string())?;
    let signed = finalize(&built, &sigs, FinalizeOptions { tighten_budgets: true }).map_err(|e| e.to_string())?;
    kob_protocol::verify::validate_signed(&signed).map_err(|e| e.to_string())?;
    let (tx, entries) = signed.tx.to_tx().map_err(|e| e.to_string())?;
    Ok(json!({
        "name": name,
        "request": request,
        "signed": signed,
        "consensus": { "transaction": tx, "entries": entries },
        "txid": hex(&tx.id().as_bytes()),
    }))
}

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

/// The fixture keys: `n`, the secret key `[n; 32]` and its x-only public key, and the role a scenario gives it.
pub fn keys_json() -> Value {
    let role = |n: u8| match n {
        1 => "maker A",
        2 => "maker B",
        3 => "maker C",
        4 => "taker",
        5 => "matcher",
        6 => "keeper",
        7 => "founder",
        8 => "merchant",
        11 => "x402 payer",
        12 => "keeper (intents)",
        _ => "",
    };
    Value::Array(
        (1..=40u8)
            .map(|n| json!({ "n": n, "secret_key": hex(&common::sk(n)), "xonly_public_key": hex(&common::pk(n)), "role": role(n) }))
            .collect(),
    )
}

pub fn out_dir() -> PathBuf {
    let dir = PathBuf::from(std::env::var("KOB_TX_DUMP_DIR").expect("set KOB_TX_DUMP_DIR to the output directory"));
    std::fs::create_dir_all(&dir).expect("create the output directory");
    dir
}

#[test]
#[ignore = "writes the dump: KOB_TX_DUMP_DIR=<dir> cargo test -p kob-protocol --test tx_dump -- --ignored"]
fn dump_every_builder_shape() {
    let dir = out_dir();
    let (mut ok, mut failed) = (vec![], vec![]);
    with_carriers(Carriers::order_side(DEFAULT_ORDER_CARRIER), || {
        for (name, action) in shapes() {
            match record(&name, &action) {
                Ok(r) => ok.push(r),
                // a shape a program refuses (no room for it, say) is listed with its reason, never silently left out
                Err(e) => failed.push(json!({ "name": name, "error": e })),
            }
        }
        for (name, r) in claims() {
            let built = kob_protocol::build::build_claim_order(&r, &kob_protocol::budget::lookup).map_err(|e| e.to_string());
            match built.and_then(|b| record_built(&name, &serde_json::to_value(&r).unwrap(), b)) {
                Ok(x) => ok.push(x),
                Err(e) => failed.push(json!({ "name": name, "error": e })),
            }
        }
    });
    let n = ok.len();
    let doc = json!({
        "format": "kob-tx-dump/1",
        "network": "none (synthetic fixture UTXOs; the keys are throwaway test keys)",
        "order_carrier_sompi": DEFAULT_ORDER_CARRIER,
        "transactions": ok,
        "not_built": failed,
    });
    std::fs::write(dir.join("shapes.json"), serde_json::to_string_pretty(&doc).unwrap()).unwrap();
    std::fs::write(dir.join("keys.json"), serde_json::to_string_pretty(&keys_json()).unwrap()).unwrap();
    println!("{n} transactions, {} not built, in {}", doc["not_built"].as_array().map_or(0, Vec::len), dir.display());
    assert!(n > 1000, "every shape is in the dump ({n})");
}
