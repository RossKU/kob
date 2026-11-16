//! Option A: key-owned KCC-20 holdings with a borrow setting (borrow scheme 0x01, 0x02 or 0x03) are
//! spent on the owner path like plain ones (`docs/design/borrow-topup.md` section 6).
//!
//! Every transaction is validated in the rusty-kaspa v2.1.0 engine on every KCC-20 program: an ask
//! placed from such a holding (plain custody, change with the same setting), a sale into a bid,
//! and a merge that keeps the setting. The ablations check the refusals: mixed settings in one
//! transaction, a borrow-enabled change to another key, a borrow-enabled top-up next to a plain
//! custody, and a borrow setting on anything but a key-owned holding.

mod common;

use common::*;
use kob_protocol::artifacts::{token_template, TemplateId};
use kob_protocol::build::*;
use kob_protocol::state::*;
use kob_protocol::tx::{finalize, sign_locally, spk_to_string, BuiltTx, FinalizeOptions, TokenUtxo};
use kob_protocol::verify::validate_signed;
use kob_protocol::{Error, Family};

/// One setting per borrow scheme: an amount threshold, somebody else's guard key, a hash-chain head.
fn settings() -> [Borrow; 3] {
    let mut thr = [0u8; 32];
    thr[..8].copy_from_slice(&(3 * WHOLE).to_le_bytes());
    [(1, thr), (2, pk(MAKER_C)), (3, [0x5a; 32])]
}

fn kcc20_programs() -> Vec<TemplateId> {
    PROGRAMS.into_iter().filter(|p| p.family() == Family::Kcc20).collect()
}

fn scenario(p: TemplateId, name: &str) -> Action {
    scenarios_on(p).into_iter().find(|(n, _)| n == name).unwrap_or_else(|| panic!("no scenario {name}")).1
}

fn with_borrow(t: &mut TokenUtxo, b: Borrow) {
    t.state = t.state.clone().with_borrow(b);
}

fn engine(built: &BuiltTx, label: &str) {
    let signed = finalize(built, &sign_locally(built, &keys()).unwrap(), FinalizeOptions::default())
        .unwrap_or_else(|e| panic!("{label}: finalize: {e}"));
    validate_signed(&signed).unwrap_or_else(|e| panic!("{label}: engine: {e}\nroles {:?}", built.roles));
}

/// The script public keys of the token outputs of `TOKEN_COV`.
fn token_spks(built: &BuiltTx) -> Vec<String> {
    built
        .tx
        .outputs
        .iter()
        .filter(|o| o.covenant.as_ref().map(|c| c.covenant_id) == Some(TOKEN_COV))
        .map(|o| o.script_public_key.clone())
        .collect()
}

fn spk(p: TemplateId, s: &TokenState) -> String {
    spk_to_string(&s.spk_with(token_template(p)))
}

fn refused(a: &Action, what: &str, needle: &str) {
    match build(a) {
        Err(Error::Invalid(m)) => assert!(m.contains(needle), "{what}: refused for another reason: {m}"),
        other => panic!("{what}: expected a refusal, got {other:?}"),
    }
}

#[test]
fn an_ask_from_a_borrow_enabled_holding_keeps_the_setting_on_the_change() {
    for p in kcc20_programs() {
        for b in settings() {
            let label = format!("create.ask, borrow scheme {}, {}", b.0, p.name());
            let Action::CreateOrder(mut c) = scenario(p, "create.ask") else { panic!() };
            with_borrow(&mut c.tokens[0], b);
            let held = c.tokens[0].state.amount();
            let left = c.order.custody_amount().unwrap();
            let built = build(&Action::CreateOrder(c.clone())).unwrap_or_else(|e| panic!("{label}: {e}"));
            engine(&built, &label);
            let order_id = built.covenants[0].covenant_id;
            let custody = TokenState::custody(Family::Kcc20, left, order_id, EXT);
            let change = TokenState::user(p, held - left, pk(MAKER_A), EXT);
            let outs = token_spks(&built);
            assert_eq!(
                outs,
                vec![spk(p, &custody), spk(p, &change.clone().with_borrow(b))],
                "{label}: plain custody, change keeps the setting"
            );
            // ablation: a plain change would drop the holder's setting
            assert_ne!(outs[1], spk(p, &change), "{label}");
        }
    }
}

#[test]
fn a_sale_into_a_bid_from_a_borrow_enabled_holding() {
    for p in kcc20_programs() {
        for b in settings() {
            let label = format!("take.bid.partial, borrow scheme {}, {}", b.0, p.name());
            let Action::Batch(mut batch) = scenario(p, "take.bid.partial") else { panic!() };
            let sold = batch.taker_tokens[0].state.amount();
            // the taker holds 2 whole more than it sells: the rest is its change
            batch.taker_tokens[0].state = batch.taker_tokens[0].state.with_amount(sold + 2 * WHOLE).with_borrow(b);
            let built = build(&Action::Batch(batch.clone())).unwrap_or_else(|e| panic!("{label}: {e}"));
            engine(&built, &label);
            let outs = token_spks(&built);
            let delivery = TokenState::user(p, sold, pk(MAKER_B), EXT);
            let change = TokenState::user(p, 2 * WHOLE, pk(TAKER), EXT).with_borrow(b);
            assert!(outs.contains(&spk(p, &delivery)), "{label}: the bid maker's delivery is plain");
            assert!(outs.contains(&spk(p, &change)), "{label}: the taker's change keeps the setting");
            // ablation: the taker's tokens to another receiver cannot carry the taker's setting
            batch.receivers = vec![TokenPayee { covenant_id: TOKEN_COV, pubkey: pk(MAKER_C) }];
            refused(&Action::Batch(batch), &label, "returns to its owner");
        }
    }
}

#[test]
fn a_merge_of_one_setting_keeps_it() {
    for p in kcc20_programs() {
        for b in settings() {
            let label = format!("merge, borrow scheme {}, {}", b.0, p.name());
            let Action::SendTokens(mut s) = scenario(p, "send.tokens") else { panic!() };
            for t in &mut s.tokens {
                with_borrow(t, b);
            }
            let total: i64 = s.tokens.iter().map(|t| t.state.amount()).sum();
            s.recipients = vec![];
            s.token_change_carrier = 2 * carrier();
            let built = build(&Action::SendTokens(s.clone())).unwrap_or_else(|e| panic!("{label}: {e}"));
            engine(&built, &label);
            let merged = TokenState::user(p, total, pk(TAKER), EXT).with_borrow(b);
            assert_eq!(token_spks(&built), vec![spk(p, &merged)], "{label}: one output with the setting");
            // a send: the recipient's output is plain, the change keeps the setting
            let Action::SendTokens(mut send) = scenario(p, "send.tokens") else { panic!() };
            for t in &mut send.tokens {
                with_borrow(t, b);
            }
            let to = send.recipients[0].clone();
            let built = build(&Action::SendTokens(send.clone())).unwrap_or_else(|e| panic!("{label}: send: {e}"));
            engine(&built, &format!("{label}: send"));
            let outs = token_spks(&built);
            assert_eq!(outs[0], spk(p, &TokenState::user(p, to.amount, to.pubkey, EXT)), "{label}: the recipient's output is plain");
            assert_eq!(outs[1], spk(p, &TokenState::user(p, total - to.amount, pk(TAKER), EXT).with_borrow(b)), "{label}: change");
            // ablation: the change to another key cannot carry the setting
            send.token_change = Some(pk(MAKER_C));
            refused(&Action::SendTokens(send), &label, "returns to its owner");
        }
    }
}

#[test]
fn inputs_of_different_settings_never_share_a_transaction() {
    let p = TemplateId::Kcc20Ref;
    let [s1, s2, s3] = settings();
    let pairs = [(NO_BORROW, s1), (s1, s2), (s2, s3), (s1, (1, [0x22; 32])), (s2, (2, pk(MAKER_B)))];
    for (a, b) in pairs {
        let what = format!("{a:?} with {b:?}");
        // send / merge
        let Action::SendTokens(mut s) = scenario(p, "send.tokens") else { panic!() };
        with_borrow(&mut s.tokens[0], a);
        with_borrow(&mut s.tokens[1], b);
        refused(&Action::SendTokens(s.clone()), &format!("send: {what}"), "mix borrow settings");
        s.recipients = vec![];
        refused(&Action::SendTokens(s), &format!("merge: {what}"), "mix borrow settings");
        // ask placement
        let Action::CreateOrder(mut c) = scenario(p, "create.ask") else { panic!() };
        let t = c.tokens[0].clone();
        c.tokens = vec![t.clone(), TokenUtxo { utxo: utxo(9, carrier(), 500, Some(TOKEN_COV)), state: t.state.clone() }];
        with_borrow(&mut c.tokens[0], a);
        with_borrow(&mut c.tokens[1], b);
        refused(&Action::CreateOrder(c.clone()), &format!("create.ask: {what}"), "mix borrow settings");
        // the same two of one setting build
        let mut same = c.clone();
        with_borrow(&mut same.tokens[1], a);
        build(&Action::CreateOrder(same)).unwrap_or_else(|e| panic!("create.ask, both {a:?}: {e}"));
        // a sale into a bid
        let Action::Batch(mut batch) = scenario(p, "take.bid.partial") else { panic!() };
        let t = batch.taker_tokens[0].clone();
        batch.taker_tokens = vec![t.clone(), TokenUtxo { utxo: utxo(29, carrier(), 1_000, Some(TOKEN_COV)), state: t.state.clone() }];
        with_borrow(&mut batch.taker_tokens[0], a);
        with_borrow(&mut batch.taker_tokens[1], b);
        refused(&Action::Batch(batch), &format!("take.bid: {what}"), "mix borrow settings");
    }
}

#[test]
fn custodies_stay_plain_and_other_owners_stay_refused() {
    let p = TemplateId::Kcc20Ref;
    let [s1, s2, _] = settings();
    // a taker's borrow-enabled tokens never join a plain custody of the same token in one output
    let Action::Batch(mut b) = scenario(p, "take.ask.partial") else { panic!() };
    b.taker_tokens = vec![tok(29, 2 * WHOLE, pk(TAKER), SCHEME_P2PK, 1_000)];
    build(&Action::Batch(b.clone())).expect("plain taker tokens next to a custody");
    with_borrow(&mut b.taker_tokens[0], s2);
    refused(&Action::Batch(b), "take.ask with borrow taker tokens", "plain custodies");
    // a cancel-replace top-up joins the plain custody: plain only
    let Action::CancelOrder(c) = scenario(p, "cancel.ask") else { panic!() };
    let custody = c.custody.clone().expect("custody");
    let mut top = tok(19, 3 * WHOLE, pk(MAKER_A), SCHEME_P2PK, 500);
    with_borrow(&mut top, s1);
    let mut rep = c.clone();
    rep.tokens = vec![top];
    refused(&Action::CancelOrder(rep), "cancel with a borrow-enabled top-up", "plain custody");
    // a custody (owner scheme 0x04) with a borrow setting is refused
    let mut bad = c.clone();
    let mut cu = custody;
    with_borrow(&mut cu, s1);
    bad.custody = Some(cu);
    refused(&Action::CancelOrder(bad), "borrow-enabled custody", "borrowing enabled");
    // another owner scheme with a borrow setting (a key hash, 0x01) stays refused
    let Action::CreateOrder(mut o) = scenario(p, "create.ask") else { panic!() };
    if let TokenState::Kcc20(k) = &mut o.tokens[0].state {
        k.owner_scheme = 0x01;
        k.borrow_scheme = 1;
    }
    refused(&Action::CreateOrder(o), "owner scheme 0x01", "borrowing enabled");
}
