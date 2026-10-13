//! The trading commands end to end against a mock wRPC node and a mock indexer (no network): every command parses its
//! arguments, reads the node and the indexer, plans, shows, signs (with `--yes`), validates in the script engine, writes
//! the signed transaction and submits it. Each written transaction is re-validated here from the file, and what it does is
//! checked against the command (custody, payments, the order's covenant id, amend records).
//!
//! Fixture: the reference 3/3 token (covenant 70..70, extension ee..ee, 3 decimals: `1TST` = 1000 base units), a maker
//! (key 11..11) and a taker (key 22..22) with 1 000 KAS and 100 / 50 tokens each, DAA 1 000 000.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use kaspa_consensus_core::tx::ScriptPublicKey;
use kob_protocol::artifacts::{template, token_template, TemplateId};
use kob_protocol::json::to_hex;
use kob_protocol::payload::{decode, recover_orders, Record};
use kob_protocol::script::p2pk_spk;
use kob_protocol::state::{AnyState, AskState, BidState, TokenState};
use kob_protocol::tx::{pubkey_of, spk_to_string, SignedTx};
use kob_protocol::verify::validate_signed;
use serde_json::{json, Value};

use crate::common::{address_of, spk_bytes};
use crate::trade::http::mock::{self as idx, MockIndexer};
use crate::wrpc::{mock, NodeUtxo};

const NET: &str = "testnet-10";
const KAS: u64 = 100_000_000;
const TOKEN: [u8; 32] = [0x70; 32];
const EXT: [u8; 32] = [0xee; 32];
const P: TemplateId = TemplateId::Kcc20Ref;
const SCALE: i64 = 1000;

fn pk(n: u8) -> [u8; 32] {
    pubkey_of(&[n; 32]).unwrap()
}

fn node_utxo(spk: &ScriptPublicKey, tag: u8, index: u32, amount: u64, cov: Option<[u8; 32]>) -> NodeUtxo {
    NodeUtxo {
        address: Some(address_of(spk, NET).unwrap()),
        txid: [tag; 32],
        index,
        amount,
        spk: spk_bytes(spk),
        daa: 990_000,
        coinbase: false,
        covenant_id: cov,
    }
}

fn user_token(owner: [u8; 32], amount: i64) -> TokenState {
    TokenState::user(P.family(), amount, owner, EXT)
}

fn token_spk(st: &TokenState) -> ScriptPublicKey {
    st.spk_with(token_template(P))
}

fn token_item(tag: u8, st: &TokenState) -> Value {
    json!({"txid": to_hex(&[tag; 32]), "index": 0, "token": to_hex(&TOKEN), "owner": to_hex(&st.owner()), "amount": st.amount().to_string(),
           "value": (10 * KAS).to_string(), "role": "owned", "created_daa": 990_000, "spent": false, "state": st, "program": P.name()})
}

fn tok_fields() -> ([u8; 32], i64, i64) {
    let t = token_template(P);
    (t.hash, t.prefix.len() as i64, t.suffix.len() as i64)
}

fn rtip() -> i64 {
    kob_protocol::defaults::tips(P).unwrap().refund_tip as i64
}

fn ask(maker: [u8; 32], amount: i64, expiry: i64) -> AskState {
    let (h, p, s) = tok_fields();
    AskState {
        maker,
        token_cov_id: TOKEN,
        token_tpl_hash: h,
        tpl_prefix_len: p,
        tpl_suffix_len: s,
        scale: SCALE,
        min_fill: SCALE,
        price: 300_000_000,
        tip: 0,
        tif: 0,
        active_from: 0,
        expiry_daa: expiry,
        refund_tip: rtip(),
        interval: 0,
        max_fill: 0,
        slope: 0,
        price_end: 0,
        decay_step: 1,
        amount_left: amount,
        extension_commitment: EXT,
    }
}

fn bid(maker: [u8; 32]) -> BidState {
    let (h, p, s) = tok_fields();
    BidState {
        maker,
        token_cov_id: TOKEN,
        token_tpl_hash: h,
        tpl_prefix_len: p,
        tpl_suffix_len: s,
        extension_commitment: EXT,
        scale: SCALE,
        min_fill: SCALE,
        price: 200_000_000,
        tip: 0,
        tif: 0,
        active_from: 0,
        expiry_daa: 1_000_000 + 77_000_000,
        refund_tip: rtip(),
        reserve: 0,
        delivery_carrier: 2 * KAS as i64,
        interval: 0,
        max_fill: 0,
        slope: 0,
        price_end: 0,
        decay_step: 1,
    }
}

/// A live order on the node (order UTXO tag `tag`, custody `tag + 1`) and its indexer view.
fn live(state: &AnyState, cov: [u8; 32], tag: u8, value: u64, utxos: &mut Vec<NodeUtxo>) -> Value {
    utxos.push(node_utxo(&state.spk(), tag, 0, value, Some(cov)));
    let mut v = json!({
        "covenant_id": to_hex(&cov), "template_hash": to_hex(&template(state.template_id()).hash), "status": "open", "state_known": true,
        "state": state, "current": {"txid": to_hex(&[tag; 32]), "index": 0, "value": value.to_string()}, "current_daa": 990_000,
        "extension_commitment": to_hex(&EXT), "strays": [],
    });
    if let Some(a) = state.custody_amount().filter(|a| *a > 0) {
        let c = TokenState::custody(P.family(), a, cov, EXT);
        utxos.push(node_utxo(&token_spk(&c), tag + 1, 0, 2 * KAS, Some(TOKEN)));
        let mut t = token_item(tag + 1, &c);
        t["role"] = json!("custody");
        v["custody"] = json!({"utxo": t, "ok": true});
    }
    v
}

struct World {
    node: String,
    node_state: Arc<Mutex<mock::MockState>>,
    indexer: String,
    dir: PathBuf,
}

fn world(name: &str, orders: &[(AnyState, [u8; 32], u8, u64)]) -> World {
    let (maker, taker) = (pk(0x11), pk(0x22));
    let mut utxos = vec![
        node_utxo(&p2pk_spk(&maker), 0x01, 0, 1000 * KAS, None),
        node_utxo(&p2pk_spk(&taker), 0x02, 0, 1000 * KAS, None),
        node_utxo(&token_spk(&user_token(maker, 100 * SCALE)), 0x03, 0, 10 * KAS, Some(TOKEN)),
        node_utxo(&token_spk(&user_token(taker, 50 * SCALE)), 0x04, 0, 10 * KAS, Some(TOKEN)),
    ];
    let mut routes: BTreeMap<String, Value> = BTreeMap::new();
    routes.insert(
        "/v1/tokens".into(),
        json!({"tokens": [{"ticker": "TST", "covenant_id": to_hex(&TOKEN), "template_hash": to_hex(&token_template(P).hash),
                           "extension_commitment": to_hex(&EXT), "decimals": 3, "open_asks": 0, "open_bids": 0}]}),
    );
    for (who, tag, amount) in [(maker, 0x03u8, 100 * SCALE), (taker, 0x04, 50 * SCALE)] {
        routes.insert(
            format!("/v1/token-utxos?owner={}&token={}&limit=500", to_hex(&who), to_hex(&TOKEN)),
            json!({"items": [token_item(tag, &user_token(who, amount))], "next_cursor": null}),
        );
        routes.insert(
            format!("/v1/orders?maker={}&token={}&status=active&limit=200", to_hex(&who), to_hex(&TOKEN)),
            json!({"items": [], "next_cursor": null}),
        );
    }
    routes.insert(
        format!("/v1/books/{}?depth=100&aggregate=false", to_hex(&TOKEN)),
        json!({"token": to_hex(&TOKEN), "aggregated": false, "asks": [], "bids": []}),
    );
    routes.insert(format!("/v1/trades/{}?limit=1", to_hex(&TOKEN)), json!({"items": [], "price_basis": "1000"}));
    for (st, cov, tag, value) in orders {
        let v = live(st, *cov, *tag, *value, &mut utxos);
        routes.insert(format!("/v1/orders/{}", to_hex(cov)), v);
    }
    let (node, node_state) = mock::start(utxos);
    let (indexer, _): (String, Arc<Mutex<MockIndexer>>) = idx::start(routes);
    let dir = std::env::temp_dir().join(format!("kob-cli-trade-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    for n in [0x11u8, 0x22] {
        std::fs::write(dir.join(format!("key{n:02x}")), to_hex(&[n; 32])).unwrap();
    }
    World { node, node_state, indexer, dir }
}

impl World {
    fn args(&self, key: u8, rest: &str) -> Vec<String> {
        let mut v: Vec<String> = rest.split_whitespace().map(str::to_string).collect();
        v.extend(
            [
                "--network",
                NET,
                "--node",
                &self.node,
                "--indexer",
                &self.indexer,
                "--key-file",
                &self.dir.join(format!("key{key:02x}")).display().to_string(),
                "--out-dir",
                &self.dir.join("out").display().to_string(),
            ]
            .map(str::to_string),
        );
        v
    }
    fn submitted(&self) -> usize {
        self.node_state.lock().unwrap().submitted.len()
    }
    /// The newest signed transaction written for `suffix`, re-validated in the engine.
    fn signed(&self, suffix: &str) -> SignedTx {
        let dir = self.dir.join("out");
        let mut files: Vec<PathBuf> = std::fs::read_dir(&dir)
            .unwrap()
            .map(|e| e.unwrap().path())
            .filter(|p| p.display().to_string().ends_with(suffix))
            .collect();
        files.sort_by_key(|p| std::fs::metadata(p).unwrap().modified().unwrap());
        let path = files.last().unwrap_or_else(|| panic!("no {suffix} in {}", dir.display()));
        let doc: Value = serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
        assert_eq!(doc["submit_request"]["method"], "submitTransaction");
        let s: SignedTx = serde_json::from_value(doc["signed"].clone()).unwrap();
        validate_signed(&s).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
        s
    }
}

fn paid_to(s: &SignedTx, key: &[u8; 32]) -> u64 {
    let spk = spk_to_string(&p2pk_spk(key));
    s.tx.outputs.iter().filter(|o| o.script_public_key == spk && o.covenant.is_none()).map(|o| o.value).sum()
}

fn spent_from(s: &SignedTx, key: &[u8; 32]) -> u64 {
    let spk = spk_to_string(&p2pk_spk(key));
    s.tx.inputs.iter().filter(|i| i.utxo.script_public_key == spk).map(|i| i.utxo.amount).sum()
}

fn run(f: fn(&[String]) -> i32, args: Vec<String>) -> i32 {
    f(&args)
}

#[test]
fn place_signs_only_with_yes_and_matches_the_planner() {
    let w = world("place", &[]);
    let place = crate::trade::place::run;
    let base = "limit --token TST --side sell --amount 10TST --price 3KAS";
    // a bare number has no unit; a key on the command line is refused; no --yes without a terminal: nothing signed
    assert_eq!(run(place, w.args(0x11, "limit --token TST --side sell --amount 10 --price 3KAS --yes")), 2);
    let mut leak = w.args(0x11, &format!("{base} --yes"));
    leak.extend(["--key".into(), "11".repeat(32)]);
    assert_eq!(run(place, leak), 2);
    assert_eq!(run(place, w.args(0x11, base)), 1);
    assert_eq!(run(place, w.args(0x11, &format!("{base} --dry-run"))), 0);
    assert_eq!(w.submitted(), 0);
    assert!(!w.dir.join("out").exists(), "nothing written before signing");
    // signed and submitted
    assert_eq!(run(place, w.args(0x11, &format!("{base} --yes"))), 0);
    assert_eq!(w.submitted(), 1);
    let s = w.signed(".place.json");
    let rec = recover_orders(&s.tx).unwrap();
    let AnyState::KobAsk(a) = &rec[0].order else { panic!("{:?}", rec[0].order) };
    // the wallet's defaults: 2 KAS carriers, minimum fill worth 10 KAS at the limit, 90 days, the program's refund tip
    assert_eq!(
        (a.maker, a.price, a.amount_left, a.min_fill, a.tip, a.refund_tip),
        (pk(0x11), 300_000_000, 10 * SCALE, 3334, 0, rtip())
    );
    assert_eq!(a.expiry_daa, 1_000_000 + 77_760_000);
    assert_eq!((rec[0].value, rec[0].custody.as_ref().unwrap().value), (2 * KAS, 2 * KAS));
    assert_eq!(rec[0].custody.as_ref().unwrap().state.amount(), 10 * SCALE);
    // a buy, a stop-market sell, an IFD buy-first, a market order against an empty book (refused: no liquidity)
    assert_eq!(run(place, w.args(0x11, "limit --token TST --side buy --amount 5TST --price 2KAS --tip 0.001KAS --yes")), 0);
    let s = w.signed(".place.json");
    let AnyState::KobBid(b) = &recover_orders(&s.tx).unwrap()[0].order else { panic!() };
    assert_eq!((b.price, b.tip, b.delivery_carrier, b.min_fill), (200_000_000, 100_000, 2 * KAS as i64, 5000));
    assert_eq!(run(place, w.args(0x11, "stop-market --token TST --side sell --amount 4TST --stop 2.5KAS --yes")), 0);
    let s = w.signed(".place.json");
    assert!(
        matches!(recover_orders(&s.tx).unwrap()[0].order, AnyState::KobCondAsk(ref c) if c.stop_price == 250_000_000 && c.slip_bps == 300)
    );
    assert_eq!(run(place, w.args(0x11, "ifd --token TST --side buy --amount 8TST --price 2KAS --take-profit 2.5KAS --yes")), 0);
    let s = w.signed(".place.json");
    assert!(matches!(recover_orders(&s.tx).unwrap()[0].order, AnyState::KobIfdBid(_)));
    assert_eq!(run(place, w.args(0x11, "market --token TST --side buy --amount 1TST --yes")), 1);
    // a carrier below the covenant minimum, an option of another kind
    assert_eq!(run(place, w.args(0x11, &format!("{base} --carrier 0.05KAS --yes"))), 1);
    assert_eq!(run(place, w.args(0x11, &format!("{base} --stop 1KAS --yes"))), 2);
    assert_eq!(w.submitted(), 4);
}

#[test]
fn replace_amends_in_place_or_cancel_replaces() {
    let maker = pk(0x11);
    let cov = [0xa1; 32];
    let order = AnyState::KobAsk(ask(maker, 10 * SCALE, 1_000_000 + 77_000_000));
    let w = world("replace", &[(order.clone(), cov, 0x10, 2 * KAS)]);
    let replace = crate::trade::actions::run_replace;
    // a new price: in place, the covenant id continues, an AMEND record
    assert_eq!(run(replace, w.args(0x11, &format!("{} --price 3.1KAS --yes", to_hex(&cov)))), 0);
    let s = w.signed(".replace.json");
    assert_eq!(s.tx.outputs[0].covenant.as_ref().map(|c| c.covenant_id), Some(cov));
    let p = decode(&s.tx.payload).unwrap().unwrap();
    assert!(p.records.iter().any(|r| matches!(r, Record::Amend { .. })), "{:?}", p.records);
    // a larger amount: cancel-replace with a top-up from the maker's free tokens, a new order of 15 tokens
    assert_eq!(run(replace, w.args(0x11, &format!("{} --amount 15TST --renew --yes", to_hex(&cov)))), 0);
    let s = w.signed(".replace.json");
    let rec = recover_orders(&s.tx).unwrap();
    let AnyState::KobAsk(a) = &rec[0].order else { panic!() };
    assert_eq!((a.amount_left, a.expiry_daa, a.price), (15 * SCALE, 1_000_000 + 77_760_000, 300_000_000));
    assert_eq!(rec[0].custody.as_ref().unwrap().state.amount(), 15 * SCALE);
    // another key cannot amend it; a bid cannot take --amount
    assert_eq!(run(replace, w.args(0x22, &format!("{} --price 3.1KAS --yes", to_hex(&cov)))), 1);
    assert_eq!(w.submitted(), 2);
}

#[test]
fn cancel_take_and_refund() {
    let (maker, taker) = (pk(0x11), pk(0x22));
    let (a, b, e) = ([0xa1; 32], [0xb1; 32], [0xe1; 32]);
    let ask_state = AnyState::KobAsk(ask(maker, 10 * SCALE, 1_000_000 + 77_000_000));
    let bid_state = bid(maker);
    let bid_value = bid_state.escrow(10 * SCALE, 3).unwrap() as u64;
    // an order whose GTD ended before now: anyone refunds it
    let expired = AnyState::KobAsk(ask(maker, 7 * SCALE, 999_000));
    let w =
        world("act", &[(ask_state, a, 0x10, 2 * KAS), (AnyState::KobBid(bid_state), b, 0x20, bid_value), (expired, e, 0x30, 2 * KAS)]);
    let actions = crate::trade::actions::run_take;
    // the taker buys 4 tokens from the ask: pays 4 x 3 KAS to the maker, receives 4 tokens
    assert_eq!(run(actions, w.args(0x22, &format!("{} --amount 4TST --yes", to_hex(&a)))), 0);
    let s = w.signed(".take.json");
    assert_eq!(paid_to(&s, &maker), 12 * KAS);
    assert!(spent_from(&s, &taker) > 12 * KAS);
    // the taker sells 2 tokens into the bid: receives about 2 x 2 KAS
    assert_eq!(run(actions, w.args(0x22, &format!("{} --amount 2TST --yes", to_hex(&b)))), 0);
    let s = w.signed(".take.json");
    assert!(paid_to(&s, &taker) > 3 * KAS, "{}", paid_to(&s, &taker));
    // the maker cannot take its own order; a fill below the minimum fill is refused by the protocol
    assert_eq!(run(actions, w.args(0x11, &format!("{} --amount 4TST --yes", to_hex(&a)))), 1);
    assert_eq!(run(actions, w.args(0x22, &format!("{} --amount 0.5TST --yes", to_hex(&a)))), 1);
    // the maker cancels the ask: the custody and the KAS come back
    assert_eq!(run(crate::trade::actions::run_cancel, w.args(0x11, &to_hex(&a))), 1, "no --yes: refused");
    assert_eq!(run(crate::trade::actions::run_cancel, w.args(0x11, &format!("{} --yes", to_hex(&a)))), 0);
    let s = w.signed(".cancel.json");
    assert!(paid_to(&s, &maker) > KAS);
    // the taker refunds the expired order and keeps the refund tip less the fee; the live ask is not refundable yet
    assert_eq!(run(crate::trade::actions::run_refund, w.args(0x22, &format!("{} --yes", to_hex(&a)))), 1);
    assert_eq!(run(crate::trade::actions::run_refund, w.args(0x22, &format!("{} --yes", to_hex(&e)))), 0);
    let s = w.signed(".refund.json");
    let earned = paid_to(&s, &taker) as i128 - spent_from(&s, &taker) as i128;
    assert!(earned > 0 && earned < rtip() as i128, "{earned}");
    // the custody and the order's KAS less the tip go back to the maker on one token output
    let back = spk_to_string(&token_spk(&user_token(maker, 7 * SCALE)));
    assert!(s.tx.outputs.iter().any(|o| o.script_public_key == back && o.value > 3 * KAS), "the custody back to the maker");
    assert_eq!(w.submitted(), 4);
}

#[test]
fn send_kas_and_tokens() {
    let w = world("send", &[]);
    let (maker, taker) = (pk(0x11), pk(0x22));
    let send = crate::trade::actions::run_send;
    let to = address_of(&p2pk_spk(&taker), NET).unwrap();
    assert_eq!(run(send, w.args(0x11, &format!("KAS {to} 2.5KAS --yes"))), 0);
    let s = w.signed(".json");
    assert_eq!(paid_to(&s, &taker), 250_000_000);
    assert_eq!(run(send, w.args(0x11, &format!("TST {} 12.5TST --yes", to_hex(&taker)))), 0);
    let s = w.signed(".json");
    let spk = spk_to_string(&token_spk(&user_token(taker, 12_500)));
    assert!(s.tx.outputs.iter().any(|o| o.script_public_key == spk && o.value == 2 * KAS), "the recipient's token output");
    let change = spk_to_string(&token_spk(&user_token(maker, 87_500)));
    assert!(s.tx.outputs.iter().any(|o| o.script_public_key == change), "the token change");
    // a mainnet address on testnet, a bare amount, more than held
    let main = address_of(&p2pk_spk(&taker), "mainnet").unwrap();
    assert_eq!(run(send, w.args(0x11, &format!("KAS {main} 1KAS --yes"))), 1);
    assert_eq!(run(send, w.args(0x11, &format!("KAS {to} 1 --yes"))), 1);
    assert_eq!(run(send, w.args(0x11, &format!("TST {to} 101TST --yes"))), 1);
    assert_eq!(w.submitted(), 2);
    let _ = Path::new(".");
}
