//! Token holdings (`GET /v1/token-utxos`): user-to-user transfers of allowlisted tokens in both families, spends, reorgs,
//! order custody and strays, the allowlist gate, the record log, and the `scale` listing rule on real orders.

mod common;

use common::*;
use kob_executor::hex::Hash32;
use kob_executor::indexer::db::open_memory;
use kob_executor::indexer::ingest::IngestConfig;
use kob_executor::indexer::reads::{self, HoldingFilter, HoldingView};
use kob_executor::testkit::*;
use kob_executor::tokens::{ListingRules, TokenAllowlist, TokenEntry};
use kob_protocol::artifacts::token_template;
use kob_protocol::build::*;
use kob_protocol::family::Family;
use kob_protocol::state::*;
use kob_protocol::tx::*;
use std::sync::Arc;

fn ctx_of(c: &Ctx) -> reads::ReadCtx {
    reads::ReadCtx { node_daa: Some(c.daa()), settle_depth_daa: 100, now_unix: None }
}

fn list(c: &Ctx, f: &HoldingFilter) -> Vec<HoldingView> {
    let g = c.hs.ingest.lock().unwrap();
    reads::holdings(g.conn(), &ctx_of(c), f, 100, None).unwrap().items
}

fn by_owner(key: u8, spent: bool) -> HoldingFilter {
    HoldingFilter { owner: Some(pk(key).to_vec()), token: None, include_spent: spent }
}

fn cov_of(fam: Family) -> [u8; 32] {
    if fam == Family::Kron {
        TOKEN_COV_KRON
    } else {
        TOKEN_COV
    }
}

/// `sender` sends (the transaction is included in a block and followed) `amount` of the family's token to `to` (change back to the sender): the real `SendTokens` builder, engine-validated.
/// Returns the transaction and the recipient's and the change's token UTXOs.
async fn send(c: &Ctx, fam: Family, input: TokenUtxo, sender: u8, to: u8, amount: i64) -> (SignedTx, TokenUtxo, TokenUtxo) {
    let total = input.state.amount();
    let program = if fam == Family::Kron { K3 } else { T3 };
    let req = SendTokens {
        token: TokenRef { covenant_id: cov_of(fam), program },
        tokens: vec![input],
        recipients: vec![TokenRecipient { pubkey: pk(to), amount, carrier: CARRIER }],
        token_change: Some(pk(sender)),
        token_change_carrier: CARRIER,
        funding: vec![c.w.coin(sender, 100)],
        records: vec![],
        change: None,
        fee: FeeOptions::default(),
    };
    let tx = c.w.sign(&Action::SendTokens(req));
    c.push(&[&tx]).await;
    let tpl = token_template(program);
    let at = |st: &TokenState| {
        let spk = spk_to_string(&st.spk_with(tpl));
        tx.tx.outputs.iter().position(|o| o.script_public_key == spk).expect("output of the state")
    };
    let got = TokenState::user(fam, amount, pk(to), EXT);
    let change = TokenState::user(fam, total - amount, pk(sender), EXT);
    let (gi, ci) = (at(&got), at(&change));
    let (g, ch) = (c.w.token_at(&tx, gi, got), c.w.token_at(&tx, ci, change));
    (tx, g, ch)
}

async fn transfers_and_spends(fam: Family) {
    let c = Ctx::new();
    let tok = c.w.token_for(fam, TAKER, 1_000);
    // TAKER's UTXO is a genesis-like output the indexer never saw: it is not listed, its spend creates two proven outputs
    let (tx1, got, change) = send(&c, fam, tok, TAKER, MAKER_A, 300).await;
    let a = list(&c, &by_owner(MAKER_A, false));
    assert_eq!(a.len(), 1, "{a:?}");
    let v = &a[0];
    assert_eq!(
        (v.family, v.role.as_str(), v.amount.as_str(), v.value.as_str()),
        (if fam == Family::Kron { "kron" } else { "kcc20" }, "owned", "300", "1000000000")
    );
    assert_eq!(v.token, hex_of(&cov_of(fam)));
    assert_eq!(v.owner, hex_of(&pk(MAKER_A)));
    assert_eq!(v.program, if fam == Family::Kron { "KronToken2433" } else { "KCC20Ref" });
    assert_eq!(v.txid, hex_of(&tx1.tx.id));
    assert!(!v.spent && v.spent_txid.is_none());
    // the state is exactly what the builders take back
    let state: TokenState = serde_json::from_value(v.state.clone()).unwrap();
    assert_eq!(state, TokenState::user(fam, 300, pk(MAKER_A), EXT));
    assert_eq!(hex_of(&state.encode()), v.state_hex);
    assert_eq!(v.template_hash, hex_of(&token_template(if fam == Family::Kron { K3 } else { T3 }).hash));
    assert_eq!(v.owner_kind, if fam == Family::Kron { 3 } else { 0 });
    // the change is the sender's, and by token both are listed
    assert_eq!(list(&c, &by_owner(TAKER, false)).len(), 1);
    let all = list(&c, &HoldingFilter { owner: None, token: Some(Hash32(cov_of(fam))), include_spent: false });
    assert_eq!(all.len(), 2);

    // MAKER_A passes 100 on: their UTXO is spent, the recipient's and the change are new
    let (tx2, _, _) = send(&c, fam, got, MAKER_A, MAKER_B, 100).await;
    let a_live = list(&c, &by_owner(MAKER_A, false));
    assert_eq!(a_live.len(), 1);
    assert_eq!(a_live[0].amount, "200");
    let a_all = list(&c, &by_owner(MAKER_A, true));
    assert_eq!(a_all.len(), 2);
    let spent = a_all.iter().find(|v| v.spent).expect("the spent one");
    assert_eq!((spent.amount.as_str(), spent.spent_txid.clone()), ("300", Some(hex_of(&tx2.tx.id))));
    assert_eq!(list(&c, &by_owner(MAKER_B, false))[0].amount, "100");
    // the untouched change of the first transfer is still there
    assert_eq!(list(&c, &by_owner(TAKER, false))[0].amount, "700");
    let _ = change;

    // a reorg of the second transfer restores the first state exactly
    c.hs.node.reorg(1, vec![vec![]]);
    c.hs.sync().await;
    assert_eq!(list(&c, &by_owner(MAKER_A, false)).iter().map(|v| v.amount.clone()).collect::<Vec<_>>(), ["300"]);
    assert!(list(&c, &by_owner(MAKER_B, true)).is_empty());
    assert_eq!(list(&c, &by_owner(MAKER_A, true)).len(), 1);
    // and re-including it applies it again
    c.w.include(&c.hs.node, &[&tx2]);
    c.hs.sync().await;
    assert_eq!(list(&c, &by_owner(MAKER_B, false)).len(), 1);
}

fn hex_of(b: &[u8]) -> String {
    kob_executor::hex::encode(b)
}

#[tokio::test]
async fn kcc20_transfers_are_held_spent_and_reverted() {
    transfers_and_spends(Family::Kcc20).await;
}

#[tokio::test]
async fn kron_transfers_are_held_spent_and_reverted() {
    transfers_and_spends(Family::Kron).await;
}

#[tokio::test]
async fn a_token_outside_the_allowlist_is_not_tracked() {
    let allow = TokenAllowlist::from_entries([TokenEntry {
        ticker: "OTHER".into(),
        covenant_id: Hash32([0x55; 32]),
        template_hash: None,
        extension_commitment: None,
        decimals: None,
        family: None,
        enabled: true,
        official: false,
        template_id: None,
        powers: vec![],
    }]);
    let proc = kob_executor::indexer::processor::Processor { tokens: Arc::new(allow), ..processor() };
    let c = Ctx::with(Harness::with_processor(
        MockNode::new("testnet-10"),
        conn(),
        None,
        IngestConfig { reorg_window_daa: u64::MAX / 4, checkpoint_daa: u64::MAX / 4, ..IngestConfig::default() },
        proc,
    ));
    let tok = c.w.token(TAKER, 1_000);
    let (_tx1, _, _) = send(&c, Family::Kcc20, tok, TAKER, MAKER_A, 300).await;
    assert!(list(&c, &HoldingFilter { owner: None, token: Some(Hash32(TOKEN_COV)), include_spent: true }).is_empty());
    assert_eq!(c.hs.query::<i64>("SELECT COUNT(*) FROM token_holdings", []), 0);
}

#[tokio::test]
async fn order_custody_and_strays_are_holdings_owned_by_the_order_id() {
    let c = Ctx::new();
    let a = ask(MAKER_A, P250);
    let create = c.w.create_tx(AnyState::KobAsk(a.clone()), CARRIER, MAKER_A, 10 * WHOLE);
    c.push(&[&create]).await;
    let cov = c.w.cov(&create, 0);
    // the placement record reveals the custody state; it is proven against the output
    let by_order = |c: &Ctx| list(c, &HoldingFilter { owner: Some(cov.0.to_vec()), token: None, include_spent: false });
    let live = by_order(&c);
    assert_eq!(live.len(), 1);
    assert_eq!((live[0].role.as_str(), live[0].amount.as_str(), live[0].owner_kind), ("custody", "10000", 4));
    assert_eq!(live[0].state["owner_scheme"], 4);

    let stray = c.w.stray(cov, 7, TAKER);
    c.push(&[&stray]).await;
    let live = by_order(&c);
    let mut roles: Vec<_> = live.iter().map(|v| (v.role.clone(), v.amount.clone())).collect();
    roles.sort();
    assert_eq!(roles, [("custody".to_string(), "10000".to_string()), ("stray".to_string(), "7".to_string())]);

    // a fill replaces the custody: the old one is spent, the new one is live (still one custody of the remaining amount)
    let custody = c.w.token_at(&create, 1, Kcc20State::custody(10 * WHOLE, cov.0, EXT));
    let leg = Leg::Ask { order: c.w.order(&create, 0, a), custody, amount: 4 * WHOLE, t: None };
    let fill = c.w.sign(&Action::Batch(batch(&c.w, c.daa(), vec![leg])));
    c.push(&[&fill]).await;
    let live = by_order(&c);
    let cust: Vec<_> = live.iter().filter(|v| v.role == "custody").collect();
    assert_eq!(cust.len(), 1);
    assert_eq!(cust[0].amount, "6000");
    // the taker's 4 whole tokens arrive as an owned holding that the fill's leader revealed
    let taker = list(&c, &by_owner(TAKER, false));
    assert!(taker.iter().any(|v| v.amount == "4000" && v.role == "owned"), "{taker:?}");
}

#[tokio::test]
async fn holdings_survive_a_record_log_replay() {
    // the property test of `flow_follower` covers replay under random traffic; this one pins the holdings section of the log
    let dir = tempfile::tempdir().unwrap();
    let (log, _) = kob_executor::indexer::recordlog::RecordLog::open(dir.path(), 1 << 20, 0).unwrap();
    let c = Ctx::with(Harness::new(conn(), Some(log)));
    let tok = c.w.token_for(Family::Kron, TAKER, 1_000);
    let (_tx1, got, _) = send(&c, Family::Kron, tok, TAKER, MAKER_A, 300).await;
    let (_tx2, _, _) = send(&c, Family::Kron, got, MAKER_A, MAKER_B, 100).await;
    let live = c.hs.snapshot();
    assert!(live.contains("holding "));
    drop(c);
    let fresh = open_memory("testnet-10").unwrap();
    let mut ing = kob_executor::indexer::ingest::Ingest::new(fresh, processor(), None).with_config(wide_window());
    let logged = kob_executor::indexer::recordlog::read_all(dir.path()).unwrap();
    ing.init_cursor(&kob_executor::indexer::ingest::Cursor { hash: logged.records[0].1.start, daa: 0 }).unwrap();
    ing.replay(logged.records).unwrap();
    assert_eq!(snapshot(ing.conn()), live);
}

/// The listing rule on real orders: a token with `decimals` lists only the orders whose `scale` (base units per whole token,
/// the price denominator) is the standard `10^decimals`, so every listed order of the token quotes the same whole token.
#[tokio::test]
async fn scale_listing_rule_on_real_orders() {
    // the fixture order quotes per SCALE = 1000 base units (decimals 3); a second order quotes per 100 base units (decimals 2)
    for (decimals, want) in [(3u32, [true, false]), (2, [false, true]), (4, [false, false])] {
        let allow = TokenAllowlist::from_entries([TokenEntry {
            ticker: "TST".into(),
            covenant_id: Hash32(TOKEN_COV),
            template_hash: None,
            extension_commitment: None,
            decimals: Some(decimals),
            family: None,
            enabled: true,
            official: false,
            template_id: None,
            powers: vec![],
        }]);
        let proc = kob_executor::indexer::processor::Processor {
            tokens: Arc::new(allow),
            rules: ListingRules { min_order_value_sompi: 1, max_expiry_span_daa: 1 << 40, ..ListingRules::default() },
        };
        let c = Ctx::with(Harness::with_processor(MockNode::new("testnet-10"), conn(), None, wide_window(), proc));
        let a = ask(MAKER_A, P250);
        let create = c.w.create_tx(AnyState::KobAsk(a), CARRIER, MAKER_A, 10 * WHOLE);
        let b = AskState { scale: SCALE / 10, ..ask(MAKER_B, P250) };
        let create2 = c.w.create_tx(AnyState::KobAsk(b), CARRIER, MAKER_B, 10 * WHOLE);
        c.push(&[&create, &create2]).await;
        for (tx, want_listed) in [&create, &create2].into_iter().zip(want) {
            let cov = c.w.cov(tx, 0);
            let (listed, reason): (i64, Option<String>) = {
                let g = c.hs.ingest.lock().unwrap();
                g.conn()
                    .query_row("SELECT listed, unlisted_reason FROM orders WHERE covenant_id = ?1", [&cov.0[..]], |r| {
                        Ok((r.get(0)?, r.get(1)?))
                    })
                    .unwrap()
            };
            assert_eq!(listed == 1, want_listed, "decimals {decimals}: {reason:?}");
            if !want_listed {
                assert_eq!(reason.as_deref(), Some("non_standard_scale"));
            }
        }
    }
}

/// The operator's own key-owned token UTXOs (what cross limit routes leave it) are read with their programs for the
/// maintenance jobs (`kob_executor::maintenance`): live ones only, never an order's custody or another key's.
async fn own_tokens_for_maintenance(fam: Family) {
    let c = Ctx::new();
    let tok = c.w.token_for(fam, TAKER, 1_000);
    let (_, got, change) = send(&c, fam, tok, TAKER, MAKER_A, 300).await;
    let own = |key: u8| {
        let g = c.hs.ingest.lock().unwrap();
        kob_executor::indexer::book::operator_tokens(g.conn(), &pk(key)).unwrap()
    };
    let a = own(MAKER_A);
    assert_eq!(a.len(), 1);
    assert_eq!(a[0].program, if fam == Family::Kron { K3 } else { T3 });
    assert_eq!(a[0].token.state, got.state);
    assert_eq!(outpoint_of(&a[0].token.utxo), outpoint_of(&got.utxo));
    assert_eq!(a[0].token.utxo.amount, CARRIER);
    assert_eq!(a[0].token.utxo.covenant_id, Some(cov_of(fam)));
    assert_eq!(own(TAKER).iter().map(|t| t.token.state.amount()).collect::<Vec<_>>(), [change.state.amount()]);
    // spent: gone (the change of the next transfer is the one left)
    let spent = outpoint_of(&got.utxo);
    send(&c, fam, got, MAKER_A, MAKER_B, 200).await;
    let a = own(MAKER_A);
    assert_eq!(a.iter().map(|t| t.token.state.amount()).collect::<Vec<_>>(), [100]);
    assert!(a.iter().all(|t| outpoint_of(&t.token.utxo) != spent));
}

fn outpoint_of(u: &Utxo) -> ([u8; 32], u32) {
    (u.transaction_id, u.index)
}

#[tokio::test]
async fn the_operators_own_kcc20_tokens_are_read_for_maintenance() {
    own_tokens_for_maintenance(Family::Kcc20).await;
}

#[tokio::test]
async fn the_operators_own_kron_tokens_are_read_for_maintenance() {
    own_tokens_for_maintenance(Family::Kron).await;
}
