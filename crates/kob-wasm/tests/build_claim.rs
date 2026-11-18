//! Tests `api::build` of a `claimOrder` (bid-delivery-spec §7): the JSON entry the web app uses
//! builds the maker's claim, and with `ownKeys` the order maker, the change key and a placed
//! order's maker must be the wallet's.
use kob_protocol::artifacts::{token_template, TemplateId};
use kob_protocol::build::{Action, ClaimOrder, Replacement};
use kob_protocol::state::{AnyState, AskState, BidState, TokenState, MODE_ACCUMULATE};
use kob_protocol::tx::{FeeOptions, KeyUtxo, OrderUtxo, TokenUtxo, Utxo};
use kob_protocol::Family;
use kob_wasm::api;
use serde_json::{json, Value};

/// Two valid x-only keys (the x coordinates of G and 2G).
const M: &str = "79be667ef9dcbbac55a06295ce870b07029bfcdb2dce28d959f2815b16f81798";
const O: &str = "c6047f9441ed7d6d3045406e95c07cd85c778e4b8cef3ca7abac09b95c709ee5";
const TOKEN: [u8; 32] = [0x70; 32];
const BID: [u8; 32] = [0xb1; 32];
const EXT: [u8; 32] = [0xee; 32];
const KAS: u64 = 100_000_000;

fn key(h: &str) -> [u8; 32] {
    kob_protocol::json::hex32(h).unwrap()
}

fn utxo(tag: u8, amount: u64, cov: Option<[u8; 32]>) -> Utxo {
    Utxo { transaction_id: [tag; 32], index: tag as u32, amount, block_daa_score: 1_000, covenant_id: cov }
}

fn bid(maker: [u8; 32]) -> BidState {
    let t = token_template(TemplateId::Kcc20Ref);
    BidState {
        maker,
        token_cov_id: TOKEN,
        token_tpl_hash: t.hash,
        tpl_prefix_len: t.prefix.len() as i64,
        tpl_suffix_len: t.suffix.len() as i64,
        extension_commitment: EXT,
        scale: 1_000,
        min_fill: 1_000,
        price: 245_000_000,
        tip: 100_000,
        tif: 0,
        active_from: 0,
        expiry_daa: 400_000_000,
        refund_tip: 10_000_000,
        reserve: 0,
        delivery_carrier: 10 * KAS as i64,
        interval: 0,
        max_fill: 0,
        slope: 0,
        price_end: 0,
        decay_step: 1_000,
        mode: MODE_ACCUMULATE,
        recv_threshold: 0,
        topup_tip: 0,
        bought: 4_000,
    }
}

fn ask(maker: [u8; 32]) -> AskState {
    let b = bid(maker);
    AskState {
        maker,
        token_cov_id: TOKEN,
        token_tpl_hash: b.token_tpl_hash,
        tpl_prefix_len: b.tpl_prefix_len,
        tpl_suffix_len: b.tpl_suffix_len,
        scale: 1_000,
        min_fill: 1_000,
        price: 250_000_000,
        tip: 100_000,
        tif: 0,
        active_from: 0,
        expiry_daa: 400_000_000,
        refund_tip: 10_000_000,
        interval: 0,
        max_fill: 0,
        slope: 0,
        price_end: 0,
        decay_step: 1_000,
        amount_left: 4_000,
        extension_commitment: EXT,
    }
}

fn request(place: Option<[u8; 32]>, change: Option<[u8; 32]>, own: Option<&str>) -> String {
    let b = bid(key(M));
    let value = b.escrow(10_000, 1).unwrap() as u64;
    let claim = ClaimOrder {
        order: OrderUtxo { utxo: utxo(20, value, Some(BID)), state: AnyState::KobBid(b) },
        bought_custody: TokenUtxo {
            utxo: utxo(23, 10 * KAS, Some(TOKEN)),
            state: TokenState::custody(Family::Kcc20, 4_000, BID, EXT),
        },
        tokens: vec![],
        place: place.map(|m| Replacement { order: AnyState::KobAsk(ask(m)), value: 10 * KAS, token_carrier: None, deadline: None }),
        end: false,
        value: None,
        token_carrier: None,
        funding: vec![KeyUtxo { utxo: utxo(30, 100 * KAS, None), pubkey: key(M) }],
        change,
        lock_time: 0,
        records: vec![],
        fee: FeeOptions::default(),
    };
    let mut v = serde_json::to_value(Action::ClaimOrder(claim)).unwrap();
    assert_eq!(v["action"], "claimOrder");
    if let Some(k) = own {
        v["ownKeys"] = json!([k]);
    }
    v.to_string()
}

#[test]
fn a_claim_builds_through_the_json_entry() {
    for place in [None, Some(key(M))] {
        let built: Value = serde_json::from_str(&api::build(&request(place, None, Some(M))).expect("claim builds")).unwrap();
        assert_eq!(built["plans"][0]["entry"], "cancel", "the order input is the maker's cancel");
        assert_eq!(built["tx"]["outputs"][0]["covenant"]["covenantId"], hex(&BID), "the order continues");
    }
}

#[test]
fn own_keys_cover_the_maker_the_change_and_the_placed_order() {
    let e = api::build(&request(None, None, Some(O))).unwrap_err();
    assert!(e.contains("order maker") && e.contains("not one of the wallet keys"), "{e}");
    let e = api::build(&request(None, Some(key(O)), Some(M))).unwrap_err();
    assert!(e.contains("change key") && e.contains("not one of the wallet keys"), "{e}");
    let e = api::build(&request(Some(key(O)), None, Some(M))).unwrap_err();
    assert!(e.contains("placed order maker") && e.contains("not one of the wallet keys"), "{e}");
}

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}
