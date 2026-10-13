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
    indexer_state: Arc<Mutex<MockIndexer>>,
    dir: PathBuf,
}

fn world(name: &str, orders: &[(AnyState, [u8; 32], u8, u64)]) -> World {
    world_x(name, orders, |_, _| {})
}

/// A world with more on the node and the indexer (`extra` adds UTXOs and routes).
fn world_x(
    name: &str,
    orders: &[(AnyState, [u8; 32], u8, u64)],
    extra: impl FnOnce(&mut Vec<NodeUtxo>, &mut BTreeMap<String, Value>),
) -> World {
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
    extra(&mut utxos, &mut routes);
    let (node, node_state) = mock::start(utxos);
    let (indexer, indexer_state): (String, Arc<Mutex<MockIndexer>>) = idx::start(routes);
    let dir = std::env::temp_dir().join(format!("kob-cli-trade-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    for n in [0x11u8, 0x22] {
        std::fs::write(dir.join(format!("key{n:02x}")), to_hex(&[n; 32])).unwrap();
    }
    World { node, node_state, indexer, indexer_state, dir }
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
    /// The codes of the findings the newest file written for `suffix` carries.
    fn issue_codes(&self, suffix: &str) -> Vec<String> {
        let dir = self.dir.join("out");
        let mut files: Vec<PathBuf> = std::fs::read_dir(&dir)
            .unwrap()
            .map(|e| e.unwrap().path())
            .filter(|p| p.display().to_string().ends_with(suffix))
            .collect();
        files.sort_by_key(|p| std::fs::metadata(p).unwrap().modified().unwrap());
        let path = files.last().unwrap_or_else(|| panic!("no {suffix} in {}", dir.display()));
        let doc: Value = serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
        doc["info"]["issues"].as_array().unwrap().iter().map(|i| i["code"].as_str().unwrap().to_string()).collect()
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
fn place_every_new_kind() {
    let w = world("kinds", &[]);
    let place = crate::trade::place::run;
    let order = |suffix: &str| recover_orders(&w.signed(suffix).tx).unwrap()[0].order.clone();
    // TWAP (sell implied) and DCA (buy implied): interval and slice in the state
    assert_eq!(run(place, w.args(0x11, "twap --token TST --amount 10TST --slice 2TST --interval 60s --price 3KAS --yes")), 0);
    let AnyState::KobAsk(a) = order(".place.json") else { panic!() };
    assert_eq!((a.interval, a.max_fill, a.amount_left), (600, 2 * SCALE, 10 * SCALE));
    assert_eq!(run(place, w.args(0x11, "dca --token TST --amount 4TST --slice 1TST --interval 600daa --price 2KAS --yes")), 0);
    let AnyState::KobBid(b) = order(".place.json") else { panic!() };
    assert_eq!((b.interval, b.max_fill), (600, SCALE));
    // a Dutch decay, a streaming buy from the displayed price, a trailing stop, a repeat IFD with a stop entry
    assert_eq!(
        run(place, w.args(0x11, "dutch --token TST --side sell --amount 2TST --price 3KAS --price-end 2KAS --duration 10m --yes")),
        0
    );
    let AnyState::KobAsk(a) = order(".place.json") else { panic!() };
    assert_eq!((a.price, a.price_end, a.active_from), (300_000_000, 200_000_000, 1_000_030));
    assert_eq!(
        run(place, w.args(0x11, "streaming --token TST --side buy --amount 1TST --displayed-price 2.5KAS --tolerance 1% --yes")),
        0
    );
    let AnyState::KobBid(b) = order(".place.json") else { panic!() };
    assert_eq!((b.price, b.price_end, b.tif), (250_000_000, 252_500_000, 1));
    assert_eq!(
        run(
            place,
            w.args(
                0x11,
                "trailing-stop --token TST --side sell --amount 3TST --stop 2KAS --trail-step 0.01KAS --trail-gap 0.05KAS --yes"
            )
        ),
        0
    );
    let AnyState::KobCondAsk(c) = order(".place.json") else { panic!() };
    assert_eq!((c.stop_price, c.trail_step, c.trail_gap, c.trail_wait), (200_000_000, 1_000_000, 5_000_000, 6_000));
    assert_eq!(
        run(
            place,
            w.args(0x11, "repeat-ifd --token TST --side buy --amount 4TST --price 2KAS --entry-stop 1.9KAS --take-profit 2.5KAS --repeat-count 3 --yes")
        ),
        0
    );
    let AnyState::KobIfdBid(e) = order(".place.json") else { panic!() };
    assert_eq!((e.rpt_amount, e.entry_stop), (1 + 3 * 4 * SCALE, 190_000_000));
    assert_eq!(w.submitted(), 6);
    // refused before anything is read: a side against the kind, a half trailing stop, an option of another kind
    assert_eq!(
        run(place, w.args(0x11, "twap --token TST --side buy --amount 1TST --slice 1TST --interval 60s --price 3KAS --yes")),
        2
    );
    assert_eq!(
        run(place, w.args(0x11, "trailing-stop --token TST --side sell --amount 1TST --stop 2KAS --trail-step 0.01KAS --yes")),
        2
    );
    assert_eq!(
        run(
            place,
            w.args(0x11, "dutch --token TST --side sell --amount 1TST --price 3KAS --price-end 2KAS --duration 1m --slice 1TST --yes")
        ),
        2
    );
    // close against an empty book: no bids to sell into
    assert_eq!(run(place, w.args(0x11, "close --token TST --yes")), 1);
    assert_eq!(w.submitted(), 6);
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

/// A node of another network than `--network` is refused before anything is signed: a signature commits to no network,
/// so a testnet-10 command against a mainnet node (a mainnet `$KOB_NODE_WRPC` left set) would spend mainnet coins of the
/// same key.
#[test]
fn a_node_of_another_network_is_refused_before_signing() {
    let w = world("othernet", &[]);
    w.node_state.lock().unwrap().network = "mainnet".into();
    let taker = pk(0x22);
    let to = address_of(&p2pk_spk(&taker), NET).unwrap();
    let place = crate::trade::place::run;
    assert_eq!(run(place, w.args(0x11, "limit --token TST --side sell --amount 10TST --price 3KAS --yes")), 1);
    assert_eq!(run(crate::trade::actions::run_send, w.args(0x11, &format!("KAS {to} 2.5KAS --yes"))), 1);
    assert_eq!(w.submitted(), 0);
    assert!(!w.dir.join("out").exists(), "nothing signed or written");
    // the same node on the named network signs
    w.node_state.lock().unwrap().network = "testnet-10".into();
    assert_eq!(run(crate::trade::actions::run_send, w.args(0x11, &format!("KAS {to} 2.5KAS --yes"))), 0);
    assert_eq!(w.submitted(), 1);
}

/// A KAS payment never pays a change that is not dust as network fee. A payment just above the dust bound from a 1 KAS
/// coin: with its change the transaction's KIP-9 storage mass exceeds the block limit; without it, 0.98 KAS would have gone
/// to the fee. Refused; an ordinary payment from the same coin keeps its change.
#[test]
fn a_kas_payment_never_pays_its_change_as_fee() {
    use kob_protocol::tx::{FeeOptions, KeyUtxo, Utxo, DUST_OUTPUT_MIN};
    let me = pk(0x11);
    let coin =
        KeyUtxo { utxo: Utxo { transaction_id: [1; 32], index: 0, amount: KAS, block_daa_score: 500, covenant_id: None }, pubkey: me };
    let to = p2pk_spk(&pk(0x22));
    let e = crate::trade::actions::kas_payment(std::slice::from_ref(&coin), DUST_OUTPUT_MIN + 100, &to, &me, &FeeOptions::default())
        .unwrap_err();
    assert!(e.contains("would otherwise be paid as network fee"), "{e}");
    let b = crate::trade::actions::kas_payment(std::slice::from_ref(&coin), KAS / 2, &to, &me, &FeeOptions::default()).unwrap();
    assert_eq!((b.tx.outputs.len(), b.fee.fee), (2, b.fee.min_fee));
    // a change below the dust bound still goes to the fee (it could not be an output)
    let b = crate::trade::actions::kas_payment(&[coin], KAS - DUST_OUTPUT_MIN / 2, &to, &me, &FeeOptions::default()).unwrap();
    assert_eq!(b.tx.outputs.len(), 1);
    assert!(b.fee.fee < DUST_OUTPUT_MIN, "{}", b.fee.fee);
}

/// `--registry FILE` is validated for the command's network, and a registry token that is not tradable there (here the
/// fixture token, delisted) is refused for trading; a send still goes (with a warning).
#[test]
fn a_delisted_registry_token_is_not_traded() {
    let w = world("delisted", &[]);
    let mut v: Value = serde_json::from_str(include_str!("../../../../registry/tokens.example.json")).unwrap();
    v["tokens"] = json!([{
        "ticker": "TST", "name": "Test token", "family": "kcc20", "covenant_id": to_hex(&TOKEN), "template_id": "kcc20-ref-3x3",
        "extension_commitment": to_hex(&EXT), "extension_class": "fixed-supply-standard", "decimals": 3, "status": "delisted",
        "verified": false,
    }]);
    let reg = w.dir.join("registry.json");
    std::fs::write(&reg, v.to_string()).unwrap();
    let with_reg = |key: u8, rest: &str| {
        let mut a = w.args(key, rest);
        a.extend(["--registry".to_string(), reg.display().to_string()]);
        a
    };
    let place = crate::trade::place::run;
    assert_eq!(run(place, with_reg(0x11, "limit --token TST --side sell --amount 10TST --price 3KAS --yes")), 1);
    assert_eq!(w.submitted(), 0);
    assert_eq!(run(crate::trade::actions::run_send, with_reg(0x11, &format!("TST {} 1TST --yes", to_hex(&pk(0x22))))), 0);
    assert_eq!(w.submitted(), 1);
    // a registry of another network is refused
    v["network"] = "mainnet".into();
    std::fs::write(&reg, v.to_string()).unwrap();
    assert_eq!(run(place, with_reg(0x11, "limit --token TST --side sell --amount 10TST --price 3KAS --yes")), 1);
    assert_eq!(w.submitted(), 1);
}

/// An unlisted KCC-20 token whose extension commitment the indexer names differently from the wallet's own node-checked
/// holdings of it: a buy (which would pin the indexer's commitment) is refused; with the holdings' commitment it is placed.
/// The indexer's commitment is never taken alone: a wallet that holds none of the token places with the commitment marked
/// unchecked (as the web app's pre-sign row), one that holds it with the commitment marked checked.
#[test]
fn an_unlisted_token_commitment_must_match_the_wallets_holdings() {
    let w = world("extmismatch", &[]);
    let set_ext = |e: [u8; 32]| {
        let mut s = w.indexer_state.lock().unwrap();
        let t = s.routes.get_mut("/v1/tokens").unwrap();
        t["tokens"][0]["extension_commitment"] = json!(to_hex(&e));
    };
    set_ext([0xdd; 32]);
    let place = crate::trade::place::run;
    let buy = "limit --token TST --side buy --amount 5TST --price 2KAS --yes";
    assert_eq!(run(place, w.args(0x11, buy)), 1);
    assert_eq!(w.submitted(), 0);
    set_ext(EXT);
    assert_eq!(run(place, w.args(0x11, buy)), 0);
    assert_eq!(w.submitted(), 1);
    let codes = w.issue_codes(".place.json");
    assert!(
        codes.iter().any(|c| c == "TOKEN_EXTENSION_CHECKED") && !codes.iter().any(|c| c == "TOKEN_EXTENSION_UNCHECKED"),
        "{codes:?}"
    );
    // a wallet without the token: nothing to compare the indexer's commitment with
    w.indexer_state.lock().unwrap().routes.insert(
        format!("/v1/token-utxos?owner={}&token={}&limit=500", to_hex(&pk(0x22)), to_hex(&TOKEN)),
        json!({"items": [], "next_cursor": null}),
    );
    assert_eq!(run(place, w.args(0x22, buy)), 0);
    assert_eq!(w.submitted(), 2);
    let codes = w.issue_codes(".place.json");
    assert!(
        codes.iter().any(|c| c == "TOKEN_EXTENSION_UNCHECKED") && !codes.iter().any(|c| c == "TOKEN_EXTENSION_CHECKED"),
        "{codes:?}"
    );
    // an indexer that names no commitment (TN10 2026-10-14 B-4): the book needs none, an order takes the one of the wallet's
    // node-checked holdings, and a wallet without the token has nothing to take it from (refused)
    w.indexer_state.lock().unwrap().routes.get_mut("/v1/tokens").unwrap()["tokens"][0]["extension_commitment"] = Value::Null;
    w.indexer_state.lock().unwrap().routes.insert(
        format!("/v1/books/{}?depth=20", to_hex(&TOKEN)),
        json!({"token": to_hex(&TOKEN), "aggregated": true, "asks": [], "bids": []}),
    );
    assert_eq!(run(crate::trade::views::run_book, w.args(0x11, "TST")), 0);
    assert_eq!(run(place, w.args(0x11, buy)), 0);
    assert_eq!(w.submitted(), 3);
    let codes = w.issue_codes(".place.json");
    assert!(codes.iter().any(|c| c == "TOKEN_EXTENSION_FROM_HOLDINGS"), "{codes:?}");
    let s = w.signed(".place.json");
    let AnyState::KobBid(b) = &recover_orders(&s.tx).unwrap()[0].order else { panic!() };
    assert_eq!(b.extension_commitment, EXT);
    assert_eq!(run(place, w.args(0x22, buy)), 1);
    assert_eq!(w.submitted(), 3);
}

/// A market sell whose start is only the indexer's best bid does not go on `--yes` alone: it needs `--ack-market-start`
/// every time, or a floor price of the user's own; the same for `close`.
#[test]
fn a_market_sell_needs_the_start_acknowledged_or_a_floor() {
    let w = world("marketsell", &[]);
    w.indexer_state.lock().unwrap().routes.insert(
        format!("/v1/books/{}?depth=100&aggregate=false", to_hex(&TOKEN)),
        json!({"token": to_hex(&TOKEN), "aggregated": false, "asks": [],
               "bids": [{"covenant_id": to_hex(&[0xb1; 32]), "price": "250000000", "amount_left": "40000", "scale": 1000}]}),
    );
    let place = crate::trade::place::run;
    let sell = "market --token TST --side sell --amount 1TST --yes";
    assert_eq!(run(place, w.args(0x11, sell)), 1);
    assert_eq!(run(place, w.args(0x11, "close --token TST --amount 1TST --yes")), 1);
    assert_eq!(w.submitted(), 0);
    assert_eq!(run(place, w.args(0x11, &format!("{sell} --ack-market-start"))), 0);
    assert_eq!(w.submitted(), 1);
    assert!(w.issue_codes(".place.json").iter().any(|c| c == "MARKET_START_UNVERIFIED_ACKNOWLEDGED"));
    // the acknowledgement is per command: the next sell asks again
    assert_eq!(run(place, w.args(0x11, sell)), 1);
    assert_eq!(run(place, w.args(0x11, &format!("{sell} --floor-price 2.45KAS"))), 0);
    assert_eq!(w.submitted(), 2);
    let AnyState::KobAsk(a) = &recover_orders(&w.signed(".place.json").tx).unwrap()[0].order else { panic!() };
    assert_eq!((a.price, a.price_end), (250_000_000, 245_000_000));
    // a floor names a sell's bound only
    assert_eq!(run(place, w.args(0x11, "market --token TST --side buy --amount 1TST --floor-price 2KAS --yes")), 1);
    assert_eq!(w.submitted(), 2);
}

const TOKEN_B: [u8; 32] = [0x71; 32];
const PB: TemplateId = TemplateId::Kcc20Ref8x8;

fn tst() -> crate::trade::market::TokenMarket {
    crate::trade::market::TokenMarket::new(TOKEN, "TST", 3, P, Some(EXT), None).unwrap()
}
fn bbb() -> crate::trade::market::TokenMarket {
    crate::trade::market::TokenMarket::new(TOKEN_B, "BBB", 2, PB, Some(EXT), None).unwrap()
}

/// An order of the maker (key 11..11) as the planner places it: its state and its value.
fn planned(intent: crate::trade::plan::Intent, pair: bool) -> (AnyState, u64) {
    use crate::trade::plan::*;
    use kob_protocol::tx::{KeyUtxo, TokenUtxo, Utxo};
    let (a, b) = (tst(), bbb());
    let utxo = |n: u8, amount: u64, cov| Utxo { transaction_id: [n; 32], index: 0, amount, block_daa_score: 500, covenant_id: cov };
    let env = Env {
        market: a.clone(),
        maker: pk(0x11),
        clock: Clock { daa: 1_000_000, unix: 1_790_694_000, rate_milli: 10_000 },
        book: Book::default(),
        own: vec![],
        guards_unavailable: vec![],
        guards_acknowledged: false,
        last_fill: None,
        market_start_ack: true,
        funding: vec![KeyUtxo { utxo: utxo(1, 1000 * KAS, None), pubkey: pk(0x11) }],
        token_utxos: vec![TokenUtxo { utxo: utxo(2, 2 * KAS, Some(TOKEN)), state: user_token(pk(0x11), 100 * SCALE) }],
        fee_rate: None,
        carrier: None,
        pair: pair.then(|| PairPart {
            quote_utxos: vec![TokenUtxo {
                utxo: utxo(3, 2 * KAS, Some(TOKEN_B)),
                state: TokenState::user(PB.family(), 1_000_000, pk(0x11), EXT),
            }],
            quote: b,
            kas_per_whole_a: Some(3 * KAS as i64),
            kas_per_whole_b: Some(KAS as i64 / 5),
        }),
    };
    let p = plan_order(&env, &intent);
    assert!(p.ok, "{:?}", p.issues);
    (p.states[0].clone(), p.request.unwrap().value)
}

/// The token B of a pair world: in the token list, a UTXO of the maker on the node and in the indexer, an empty pair book.
fn with_b(utxos: &mut Vec<NodeUtxo>, routes: &mut BTreeMap<String, Value>) {
    let maker = pk(0x11);
    let st = TokenState::user(PB.family(), 1_000_000, maker, EXT);
    utxos.push(node_utxo(&st.spk_with(token_template(PB)), 0x05, 0, 10 * KAS, Some(TOKEN_B)));
    routes.insert(
        "/v1/tokens".into(),
        json!({"tokens": [
            {"ticker": "TST", "covenant_id": to_hex(&TOKEN), "template_hash": to_hex(&token_template(P).hash),
             "extension_commitment": to_hex(&EXT), "decimals": 3, "open_asks": 0, "open_bids": 0},
            {"ticker": "BBB", "covenant_id": to_hex(&TOKEN_B), "template_hash": to_hex(&token_template(PB).hash),
             "extension_commitment": to_hex(&EXT), "decimals": 2, "open_asks": 0, "open_bids": 0}]}),
    );
    let mut item = token_item(0x05, &st);
    item["token"] = json!(to_hex(&TOKEN_B));
    item["program"] = json!(PB.name());
    routes.insert(
        format!("/v1/token-utxos?owner={}&token={}&limit=500", to_hex(&maker), to_hex(&TOKEN_B)),
        json!({"items": [item], "next_cursor": null}),
    );
    routes.insert(format!("/v1/pairs/{}/{}/book?depth=100", to_hex(&TOKEN), to_hex(&TOKEN_B)), json!({"asks": [], "bids": []}));
}

#[test]
fn replace_replans_trailing_if_done_and_pair_orders() {
    use crate::trade::plan::*;
    let (t_cov, i_cov, p_cov) = ([0xc1; 32], [0xc2; 32], [0xc3; 32]);
    let trailing = planned(
        Intent::Cond(CondIntent {
            kind: CondKind::Trailing,
            side: Side::Sell,
            amount: 5 * SCALE,
            tip: 0,
            min_fill: None,
            stop: Some(200_000_000),
            limit: None,
            take_profit: None,
            slip_bps: None,
            min_touch: None,
            lifetime: Lifetime::Gtc,
            active_from: None,
            max_fills: None,
            trail: Some(Trail { step: 1_000_000, gap: 5_000_000, wait: None, updates: None }),
            knobs: StopKnobs::default(),
        }),
        false,
    );
    let ifd = planned(
        Intent::Ifd(IfdIntent {
            side: Side::Buy,
            amount: 4 * SCALE,
            tip: 0,
            min_fill: None,
            price: 200_000_000,
            take_profit: Some(250_000_000),
            stop: None,
            stop_limit: None,
            exit_tip: 0,
            prefund: None,
            lifetime: Lifetime::Gtc,
            active_from: None,
            more: IfdMore::default(),
        }),
        false,
    );
    let pair = planned(
        Intent::Limit(LimitIntent {
            side: Side::Sell,
            amount: 5 * SCALE,
            price: 1_600,
            tip: 0,
            min_fill: None,
            lifetime: Lifetime::Gtc,
            active_from: None,
            crossing: Crossing::Auction,
            max_fills: None,
        }),
        true,
    );
    let orders = [(trailing.0, t_cov, 0x30, trailing.1), (ifd.0, i_cov, 0x40, ifd.1), (pair.0, p_cov, 0x50, pair.1)];
    let w = world_x("replan", &orders, with_b);
    let replace = crate::trade::actions::run_replace;
    let rec = |s: &SignedTx| recover_orders(&s.tx).unwrap()[0].clone();
    // a token the registry delists is not re-planned either (refused like every amendment)
    let mut v: Value = serde_json::from_str(include_str!("../../../../registry/tokens.example.json")).unwrap();
    v["tokens"] = json!([{
        "ticker": "TST", "name": "Test token", "family": "kcc20", "covenant_id": to_hex(&TOKEN), "template_id": "kcc20-ref-3x3",
        "extension_commitment": to_hex(&EXT), "extension_class": "fixed-supply-standard", "decimals": 3, "status": "delisted",
        "verified": false,
    }]);
    let reg = w.dir.join("registry.json");
    std::fs::write(&reg, v.to_string()).unwrap();
    let mut delisted = w.args(0x11, &format!("{} --stop 2.1KAS --yes", to_hex(&t_cov)));
    delisted.extend(["--registry".to_string(), reg.display().to_string()]);
    assert_eq!(run(replace, delisted), 1);
    assert_eq!(w.submitted(), 0);
    // a trailing stop: a new stop, the trail kept, a new covenant id, the custody moved
    assert_eq!(run(replace, w.args(0x11, &format!("{} --stop 2.1KAS --yes", to_hex(&t_cov)))), 0);
    let s = w.signed(".replace.json");
    let AnyState::KobCondAsk(c) = rec(&s).order else { panic!() };
    assert_eq!((c.stop_price, c.trail_step, c.trail_gap, c.amount_left), (210_000_000, 1_000_000, 5_000_000, 5 * SCALE));
    assert_ne!(s.tx.outputs[0].covenant.as_ref().map(|x| x.covenant_id), Some(t_cov));
    assert_eq!(rec(&s).custody.unwrap().state.amount(), 5 * SCALE);
    // an if-done entry: a new entry price and exit take-profit, the exit committed again
    assert_eq!(run(replace, w.args(0x11, &format!("{} --price 2.1KAS --take-profit 2.6KAS --yes", to_hex(&i_cov)))), 0);
    let AnyState::KobIfdBid(e) = rec(&w.signed(".replace.json")).order else { panic!() };
    assert_eq!((e.price, e.amount_left, e.exit().unwrap().tp_price), (210_000_000, 4 * SCALE, 260_000_000));
    // a pair limit: a new price in B per whole A, the A custody moved to the new order
    assert_eq!(run(replace, w.args(0x11, &format!("{} --price 15BBB --yes", to_hex(&p_cov)))), 0);
    let r = rec(&w.signed(".replace.json"));
    let AnyState::KobPair(x) = &r.order else { panic!() };
    assert_eq!((x.price, x.amount_left, x.custody, x.t_cov_id), (1_500, 5 * SCALE, 5 * SCALE, TOKEN_B));
    assert_eq!(r.custody.unwrap().state.amount(), 5 * SCALE);
    assert_eq!(w.submitted(), 3);
    // refused: --add-kas on a re-planned order, --trail-step on an order that does not trail, nothing to change
    assert_eq!(run(replace, w.args(0x11, &format!("{} --add-kas 1KAS --yes", to_hex(&t_cov)))), 1);
    assert_eq!(run(replace, w.args(0x11, &format!("{} --trail-step 0.01KAS --yes", to_hex(&i_cov)))), 1);
    assert_eq!(run(replace, w.args(0x11, &format!("{} --yes", to_hex(&p_cov)))), 1);
    assert_eq!(w.submitted(), 3);
}

fn view_args(
    w: &World,
    key: u8,
    rest: &str,
    value_flags: &[&str],
    bool_flags: &[&str],
) -> (crate::trade::opts::Parsed, crate::trade::opts::Common, Vec<String>) {
    let args = w.args(key, rest);
    let p = crate::trade::opts::parse(&args, value_flags, bool_flags).unwrap();
    let c = crate::trade::opts::Common::from(&p).unwrap();
    (p, c, args)
}

#[test]
fn book_orders_and_balances_views() {
    use crate::trade::views;
    let maker = pk(0x11);
    let cov = [0xa1; 32];
    let order = AnyState::KobAsk(ask(maker, 10 * SCALE, 1_000_000 + 77_000_000));
    let w = world_x("views", &[(order, cov, 0x10, 2 * KAS)], |_, routes| {
        routes.insert(
            format!("/v1/books/{}", to_hex(&TOKEN)),
            json!({"token": to_hex(&TOKEN), "aggregated": true,
                   "asks": [{"price": "250000000", "amount": "5000", "amount_estimated": false, "orders": 2, "scale": 1000},
                            {"price": "260000000", "amount": "7000", "amount_estimated": false, "orders": 1, "scale": 1000}],
                   "bids": [{"price": "245000000", "amount": "3000", "amount_estimated": true, "orders": 1, "scale": 1000}]}),
        );
        let view = routes[&format!("/v1/orders/{}", to_hex(&cov))].clone();
        routes.insert(
            format!("/v1/orders?maker={}&status=active&limit=200", to_hex(&maker)),
            json!({"items": [view], "next_cursor": null}),
        );
        routes.insert(
            format!("/v1/token-utxos?owner={}&limit=500", to_hex(&maker)),
            json!({"items": [token_item(0x03, &user_token(maker, 100 * SCALE))], "next_cursor": null}),
        );
    });
    // the book: asks highest first above the spread, bids below, prices per whole token
    let (p, c, _) = view_args(&w, 0x11, "TST", &["--quote", "--depth"], &[]);
    let (v, lines) = views::book_view(&c, &p).unwrap();
    assert_eq!((v["best_ask"].as_str(), v["best_bid"].as_str()), (Some("250000000"), Some("245000000")));
    assert_eq!(v["asks"][0]["price_text"], "2.5 KAS per TST");
    let at = |s: &str| lines.iter().position(|l| l.starts_with("    ") && l.contains(s)).unwrap_or_else(|| panic!("{s}: {lines:?}"));
    let spread = lines.iter().position(|l| l.contains("spread 0.05 KAS per TST")).unwrap_or_else(|| panic!("{lines:?}"));
    assert!(at("2.6 KAS per TST") < at("2.5 KAS per TST") && at("2.5 KAS per TST") < spread && spread < at("2.45 KAS per TST"));
    // the maker's orders: the live ask in words
    let (p, c, args) = view_args(&w, 0x11, "", &["--token", "--maker"], &["--all"]);
    let (v, lines) = views::orders_view(&c, &p, &args).unwrap();
    assert_eq!(v["orders"][0]["covenant_id"], to_hex(&cov));
    assert_eq!(v["orders"][0]["what"], "sell 10 TST at 3 KAS per TST");
    assert!(lines[1].contains(&to_hex(&cov)) && lines[1].contains("[open, expiry DAA 78000000]"), "{lines:?}");
    // another owner by address: no key needed
    let addr = address_of(&p2pk_spk(&pk(0x22)), NET).unwrap();
    let (p, c, args) = view_args(&w, 0x22, &format!("--maker {addr}"), &["--token", "--maker"], &["--all"]);
    assert!(views::orders_view(&c, &p, &args).unwrap_err().contains("HTTP 404"), "the mock lists no orders of the taker");
    // balances: free KAS, the free tokens confirmed on the node, the ask's custody and KAS
    let (p, c, args) = view_args(&w, 0x11, "", &["--maker"], &[]);
    let (v, lines) = views::balances_view(&c, &p, &args).unwrap();
    assert_eq!(v["kas"]["free"], (1000 * KAS).to_string());
    let t = &v["tokens"][0];
    assert_eq!((t["ticker"].as_str(), t["free"].as_str(), t["free_verified"].as_bool()), (Some("TST"), Some("100000"), Some(true)));
    assert_eq!((t["in_orders"].as_str(), t["orders"].as_u64()), (Some("10000"), Some(1)));
    // the ask's UTXO and its custody's carrier (10 KAS as the view reports it) are locked; no budget
    assert_eq!((t["kas_escrow"].as_str(), t["kas_locked"].as_str()), (Some("0"), Some((12 * KAS).to_string().as_str())));
    assert!(lines.iter().any(|l| l.contains("TST: free 100 TST in 1 UTXO, in orders 10 TST")), "{lines:?}");
    // nothing is signed or submitted by a view
    assert_eq!(w.submitted(), 0);
}

#[test]
fn merge_chains_transfers_to_the_key() {
    let maker = pk(0x11);
    let extra = [(0x06u8, 5 * SCALE), (0x07, 7 * SCALE), (0x08, SCALE), (0x09, 2 * SCALE)];
    let w = world_x("merge", &[], |utxos, routes| {
        let mut items = vec![token_item(0x03, &user_token(maker, 100 * SCALE))];
        for (tag, a) in extra {
            utxos.push(node_utxo(&token_spk(&user_token(maker, a)), tag, 0, KAS, Some(TOKEN)));
            items.push(token_item(tag, &user_token(maker, a)));
        }
        routes.insert(
            format!("/v1/token-utxos?owner={}&token={}&limit=500", to_hex(&maker), to_hex(&TOKEN)),
            json!({"items": items, "next_cursor": null}),
        );
    });
    let merge = crate::trade::merge::run;
    // the plan alone; a key with one UTXO has nothing to merge
    assert_eq!(run(merge, w.args(0x11, "TST --dry-run")), 0);
    assert_eq!(run(merge, w.args(0x22, "TST --yes")), 1);
    assert_eq!(w.submitted(), 0);
    // five UTXOs: 1 + 2 + 5 TST in the first link, its output + 7 + 100 TST in the second
    assert_eq!(run(merge, w.args(0x11, "TST --yes --wait-secs 0")), 0);
    assert_eq!(w.submitted(), 2);
    let s1 = w.signed("merge-1.json");
    let s2 = w.signed("merge-2.json");
    let one = spk_to_string(&token_spk(&user_token(maker, 8 * SCALE)));
    let out1 = s1.tx.outputs.iter().position(|o| o.script_public_key == one).expect("the first link's merged output");
    assert!(s2.tx.inputs.iter().any(|i| i.transaction_id == s1.tx.id && i.index == out1 as u32), "the second link spends it");
    let all = spk_to_string(&token_spk(&user_token(maker, 115 * SCALE)));
    assert_eq!(s2.tx.outputs.iter().filter(|o| o.covenant.is_some()).count(), 1);
    // the largest input carrier (the 100 TST UTXO's 10 KAS), at most 2 KAS
    assert!(s2.tx.outputs.iter().any(|o| o.script_public_key == all && o.value == 2 * KAS), "one output of the whole amount");
}
