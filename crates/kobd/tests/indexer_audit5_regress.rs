//! Regression tests of the round-5 indexer audit fixes (audit 2026-10-30 V7, I16, I17) and their bypasses. Offline.
#![allow(dead_code, unused_imports)]

mod common;

use std::sync::Arc;

use common::*;
use kob_protocol::artifacts::{token_template, TemplateId};
use kob_protocol::build::*;
use kob_protocol::state::*;
use kob_protocol::tx::*;
use kobd::hex::Hash32;
use kobd::indexer::db::open_memory;
use kobd::indexer::processor::{revert_block, Delta, Processor};
use kobd::indexer::reads;
use kobd::testkit::*;
use kobd::tokens::{ListingRules, StrictTemplate, TokenAllowlist};
use rusqlite::params;

// ------------------------------------------------------------------------------------------------------ V7
mod v4 {
    use super::*;
    use kob_protocol::artifacts::TemplateId;
    use kobd::indexer::processor::Processor;
    use kobd::tokens::{ListingRules, StrictTemplate, TokenAllowlist};
    use std::sync::Arc;

    pub const K2732: TemplateId = TemplateId::KronToken2732;

    fn strict_kron(id: TemplateId) -> (Hash32, StrictTemplate) {
        (
            Hash32(kob_protocol::artifacts::token_template(id).hash),
            StrictTemplate { id: id.name().into(), family: id.family(), powers: vec![] },
        )
    }

    pub fn ctx() -> Ctx {
        let tokens = Arc::new(TokenAllowlist::from_entries([]).with_strict_templates([strict_kron(K3), strict_kron(K2732)]));
        let rules = ListingRules { max_expiry_span_daa: 1 << 40, min_order_value_sompi: 0, ..ListingRules::default() };
        let proc = Processor { tokens, rules };
        Ctx::with(Harness::with_processor(MockNode::new("testnet-10"), conn(), None, wide_window(), proc))
    }

    /// A KRON ask that pins the KronToken2732 program instead of KronToken2433.
    pub fn ask_k2732(price: i64) -> AnyState {
        match kron(AnyState::KobAsk(ask(MAKER_B, price))) {
            AnyState::KobAskKron(mut a) => {
                let (h, p, s) = tok_fields(K2732);
                (a.token_tpl_hash, a.tpl_prefix_len, a.tpl_suffix_len, a.refund_tip) = (h, p, s, rtip(K2732));
                AnyState::KobAskKron(a)
            }
            _ => unreachable!(),
        }
    }

    pub fn bid_k2732(price: i64) -> AnyState {
        match kron(AnyState::KobBid(bid(MAKER_B, price))) {
            AnyState::KobBidKron(mut b) => {
                let (h, p, s) = tok_fields(K2732);
                (b.token_tpl_hash, b.tpl_prefix_len, b.tpl_suffix_len, b.refund_tip) = (h, p, s, rtip(K2732));
                AnyState::KobBidKron(b)
            }
            _ => unreachable!(),
        }
    }

    pub fn programs(c: &Ctx) -> Vec<String> {
        c.hs.query::<String>("SELECT COALESCE(group_concat(DISTINCT program), '') FROM token_holdings WHERE token_id = (SELECT id FROM ids WHERE h = x'7171717171717171717171717171717171717171717171717171717171717171')", [])
            .split(',')
            .map(|s| s.to_string())
            .collect()
    }
}

fn orders_dump(c: &Ctx) -> String {
    c.hs.query::<String>(
        "SELECT COALESCE(group_concat(hex(covenant_id)||':'||listed||':'||COALESCE(unlisted_reason,'')||':'||price||':'||tip||':'||tif||':'||expiry_daa||':'||active_from||':'||COALESCE(deadline,'')||':'||COALESCE(budget_rate,'')||':'||COALESCE(reserve,'')||':'||COALESCE(min_fill,'')), '') FROM (SELECT * FROM orders ORDER BY covenant_id)",
        [],
    )
}

fn listed(c: &Ctx, cov: &Hash32) -> bool {
    view(c, cov).listed
}

/// V7: a KRON token with two programs on one covenant id. K3 is proven first (its ask), which unlists the bare bid pinning K2732; the
/// honest ask of K2732 is its own proof, and from then on the orders pinning K2732 are listed again, the placing ask included.
#[tokio::test]
async fn v7_the_second_program_proven_relists_its_orders() {
    use v4::*;
    let c = ctx();
    let b = bid_k2732(250_000_000);
    let AnyState::KobBidKron(bs) = &b else { unreachable!() };
    let esc = bs.escrow(WHOLE, 2).unwrap() as u64;
    let bid_tx = c.w.create_tx(b, esc, MAKER_B, 0);
    c.push(&[&bid_tx]).await;
    let bid_cov = c.w.cov(&bid_tx, 0);
    assert!(listed(&c, &bid_cov), "nothing proven yet: the bare bid is listed");
    // K3 proven by an honest ask: the K2732 bid pins a program the chain does not prove for the token
    let ask3 = c.w.create_tx(kron(AnyState::KobAsk(ask(MAKER_A, P250))), CARRIER, MAKER_A, 10 * WHOLE);
    c.push(&[&ask3]).await;
    assert!(listed(&c, &c.w.cov(&ask3, 0)));
    assert!(!listed(&c, &bid_cov), "K2732 is unproven: its bid is out of the books");
    assert_eq!(view(&c, &bid_cov).unlisted_reason.as_deref(), Some("token_program_conflict"));
    // an honest ask pinning K2732: its custody proves K2732 for the token
    let ask2 = c.w.create_tx(ask_k2732(P245), CARRIER, MAKER_B, 10 * WHOLE);
    c.push(&[&ask2]).await;
    assert_eq!(programs(&c).len(), 2, "both programs are proven: {:?}", programs(&c));
    assert!(listed(&c, &c.w.cov(&ask2, 0)), "the ask whose custody proves K2732 is listed");
    assert!(listed(&c, &bid_cov), "the bid pinning K2732 is listed again");
    // the K3 order is still listed
    assert!(listed(&c, &c.w.cov(&ask3, 0)));
}

/// V7: a reorg of the proving block restores every row exactly, and the same block applied again gives the same rows.
#[tokio::test]
async fn v7_reorg_of_the_proving_holding_restores_exactly() {
    use v4::*;
    let c = ctx();
    let ask3 = c.w.create_tx(kron(AnyState::KobAsk(ask(MAKER_A, P250))), CARRIER, MAKER_A, 10 * WHOLE);
    c.push(&[&ask3]).await;
    let b = bid_k2732(250_000_000);
    let AnyState::KobBidKron(bs) = &b else { unreachable!() };
    let esc = bs.escrow(WHOLE, 2).unwrap() as u64;
    let bid_tx = c.w.create_tx(b, esc, MAKER_B, 0);
    c.push(&[&bid_tx]).await;
    c.hs.node.push_empty(2);
    c.hs.sync().await;
    let before = orders_dump(&c);
    let ask2 = c.w.create_tx(ask_k2732(P245), CARRIER, MAKER_B, 10 * WHOLE);
    c.push(&[&ask2]).await;
    let mid = orders_dump(&c);
    assert_ne!(before, mid);
    c.hs.node.reorg(1, vec![vec![]]);
    c.hs.sync().await;
    assert_eq!(orders_dump(&c).replace(&hex_of(&c.w.cov(&ask2, 0)), ""), before.replace(&hex_of(&c.w.cov(&ask2, 0)), ""));
    assert_eq!(c.hs.query::<i64>("SELECT COUNT(*) FROM order_amends", []), 0, "the amend rows of the reverted block are gone");
    assert!(!listed(&c, &c.w.cov(&bid_tx, 0)), "the bid is out again");
    c.hs.node.reorg(1, vec![vec![kobd::testkit::wire_tx(&ask2.tx)]]);
    c.hs.sync().await;
    assert_eq!(orders_dump(&c), mid);
}

fn hex_of(h: &Hash32) -> String {
    h.0.iter().map(|b| format!("{:02X}", b)).collect()
}

// ------------------------------------------------------------------------------------------------------ I16

fn amend_row(conn: &rusqlite::Connection, cov: &[u8], seq: i64, price: i64, min_fill: Option<i64>, listed: i64, reason: Option<&str>) {
    conn.execute(
        "INSERT INTO order_amends (order_id, block_seq, price, tip, tif, expiry_daa, active_from, deadline, listed, unlisted_reason, budget_rate, reserve, min_fill) \
         VALUES ((SELECT id FROM orders WHERE covenant_id = ?1), ?2, ?3, 0, 0, 1, 1, NULL, ?4, ?5, NULL, NULL, ?6)",
        params![cov, seq, price, listed, reason, min_fill],
    )
    .unwrap();
}

fn insert_order(conn: &rusqlite::Connection, cov: &[u8], token: &[u8], tpl: &[u8], price: i64, listed: i64) {
    for h in [&[1u8; 32][..], &[1u8][..], token, tpl] {
        kobd::indexer::ids::intern(conn, h).unwrap();
    }
    conn.execute(
        "INSERT INTO orders (covenant_id, contract, template_id, family, side, maker_id, token_id, token_tpl_id, scale, price, min_fill, tip, tif, expiry_daa, active_from, in_book, genesis_state, genesis_block, genesis_daa, listed, unlisted_reason, origin) \
         VALUES (?1, 'KobBid', (SELECT id FROM ids WHERE h = ?2), 1, 2, (SELECT id FROM ids WHERE h = x'01'), (SELECT id FROM ids WHERE h = ?3), (SELECT id FROM ids WHERE h = ?4), 1000, ?5, 9, 0, 0, 1, 1, 1, x'00', 1, 1, ?6, CASE WHEN ?6 = 0 THEN 'x' END, 'chain')",
        params![cov, vec![1u8; 32], token, tpl, price, listed],
    )
    .unwrap();
}

/// I16: the revert of a block gives the amended rows back with one statement. An order amended twice in the block takes the terms of the
/// oldest row, its minimum fill the oldest one recorded (what restoring newest first gave), rows of other blocks and orders stay.
#[test]
fn i16_revert_restores_amends_exactly() {
    let conn = open_memory("testnet-10").unwrap();
    let tok = [9u8; 32];
    let tpl = [8u8; 32];
    for i in 1u8..=4 {
        insert_order(&conn, &[i; 32], &tok, &tpl, 300, 0);
    }
    // order 1: two amends in block 7 (the oldest has no min_fill); order 2: one amend with a min_fill; order 3: an amend of block 6; order 4: none
    amend_row(&conn, &[1; 32], 7, 100, None, 1, None);
    amend_row(&conn, &[1; 32], 7, 200, Some(5), 0, Some("a"));
    amend_row(&conn, &[2; 32], 7, 150, Some(7), 1, None);
    amend_row(&conn, &[3; 32], 6, 120, Some(3), 1, None);
    revert_block(&conn, 7, &mut Delta::default()).unwrap();
    let row = |i: u8| -> (i64, i64, Option<i64>, Option<String>) {
        conn.query_row("SELECT price, listed, min_fill, unlisted_reason FROM orders WHERE covenant_id = ?1", [&[i; 32][..]], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))
        })
        .unwrap()
    };
    assert_eq!(row(1), (100, 1, Some(5), None), "the oldest terms, the oldest recorded minimum fill");
    assert_eq!(row(2), (150, 1, Some(7), None));
    assert_eq!(row(3).0, 300, "an amend of another block is not touched");
    assert_eq!(row(4).0, 300);
    let left: i64 = conn.query_row("SELECT COUNT(*) FROM order_amends", [], |r| r.get(0)).unwrap();
    assert_eq!(left, 1, "only the rows of the reverted block are gone");
}

/// I16: a block that unlisted a large share of the book is reverted in time (it held the writer for 19 s at 300,000 orders).
#[test]
fn i16_revert_of_a_mass_unlisting_is_bounded() {
    let conn = open_memory("testnet-10").unwrap();
    let n: i64 = std::env::var("I16_ROWS").ok().and_then(|v| v.parse().ok()).unwrap_or(60_000);
    conn.execute("INSERT OR IGNORE INTO ids (h) VALUES (x'01'), (x'09'), (x'08')", []).unwrap();
    conn.execute(
        "WITH RECURSIVE n(i) AS (SELECT 1 UNION ALL SELECT i + 1 FROM n WHERE i < ?1) \
         INSERT INTO orders (covenant_id, contract, template_id, family, side, maker_id, token_id, token_tpl_id, scale, price, in_book, genesis_state, genesis_block, genesis_daa, listed, origin) \
         SELECT randomblob(32), 'KobBid', (SELECT id FROM ids WHERE h = x'01'), 1, 2, (SELECT id FROM ids WHERE h = x'01'), (SELECT id FROM ids WHERE h = x'09'), (SELECT id FROM ids WHERE h = x'08'), 1000, 1000, 1, x'00', 1, 1, 0, 'chain' FROM n",
        [n],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO order_amends (order_id, block_seq, price, tip, tif, expiry_daa, active_from, listed, min_fill) \
         SELECT id, 7, 1000, 0, 0, 1, 1, 1, NULL FROM orders",
        [],
    )
    .unwrap();
    let t = std::time::Instant::now();
    revert_block(&conn, 7, &mut Delta::default()).unwrap();
    let ms = t.elapsed().as_millis();
    println!("I16 revert of {n} restored rows: {ms} ms");
    let back: i64 = conn.query_row("SELECT COUNT(*) FROM orders WHERE listed = 1", [], |r| r.get(0)).unwrap();
    assert_eq!(back, n);
    // a generous bound (the row-at-a-time version took 65 us a row): set-based, tens of microseconds a row at the very most
    assert!(ms < (n as u128) * 30 / 1000 + 500, "{ms} ms for {n} rows");
}

// ------------------------------------------------------------------------------------------------------ I17

/// I17: tokens placements name but the chain does not prove (ids that sort first) cannot push a token with a holding out of the open
/// token list past its cap.
#[test]
fn i17_a_token_with_a_holding_is_not_evicted_by_junk_tokens() {
    let conn = open_memory("testnet-10").unwrap();
    let program = TemplateId::Kcc20Ref8x8;
    let tpl = token_template(program).hash.to_vec();
    let bid = kob_protocol::artifacts::pinned_hash(TemplateId::KobBid).to_vec();
    let junk = reads::OPEN_TOKEN_ROWS_MAX + 100;
    conn.execute(
        "WITH RECURSIVE n(i) AS (SELECT 1 UNION ALL SELECT i + 1 FROM n WHERE i < ?1) \
         INSERT INTO ids (h) SELECT CAST(printf('%032d', i) AS BLOB) FROM n",
        params![junk],
    )
    .unwrap();
    conn.execute("INSERT OR IGNORE INTO ids (h) VALUES (?1), (?2)", params![bid, tpl]).unwrap();
    // every junk order has its own token and its own maker (the maker is the token's row too: only the distinctness matters)
    conn.execute(
        "WITH RECURSIVE n(i) AS (SELECT 1 UNION ALL SELECT i + 1 FROM n WHERE i < ?1) \
         INSERT INTO orders (covenant_id, contract, template_id, family, side, maker_id, token_id, token_tpl_id, scale, price, in_book, genesis_state, genesis_block, genesis_daa, listed, origin) \
         SELECT randomblob(32), 'KobBid', (SELECT id FROM ids WHERE h = ?2), 1, 2, t.id, t.id, (SELECT id FROM ids WHERE h = ?3), 1000, 1000, 1, randomblob(8), i, i, 1, 'chain' \
         FROM n JOIN ids t ON t.h = CAST(printf('%032d', n.i) AS BLOB)",
        params![junk, bid, tpl],
    )
    .unwrap();
    let real = vec![0x70u8; 32];
    let owner = vec![0x71u8; 32];
    kobd::indexer::ids::intern(&conn, &real).unwrap();
    kobd::indexer::ids::intern(&conn, &owner).unwrap();
    insert_order(&conn, &[0xee; 32], &real, &tpl, 1000, 1);
    conn.execute(
        "INSERT INTO token_holdings (txid, idx, token_id, program, family, owner_id, owner_kind, amount, value, state, role, created_block, created_daa) \
         VALUES (randomblob(32), 0, (SELECT id FROM ids WHERE h = ?1), ?2, 1, (SELECT id FROM ids WHERE h = ?3), 0, 5, 100000000, randomblob(112), 'owned', 1, 1)",
        params![real, program.name(), owner],
    )
    .unwrap();
    let strict =
        (Hash32(token_template(program).hash), StrictTemplate { id: program.name().into(), family: program.family(), powers: vec![] });
    let tokens = TokenAllowlist::from_entries([]).with_strict_templates([strict]);
    let list = reads::token_list(&conn, &tokens).unwrap();
    let real_hex: String = real.iter().map(|b| format!("{b:02x}")).collect();
    assert!(list.len() <= reads::OPEN_TOKEN_ROWS_MAX as usize, "{} tokens", list.len());
    assert!(list.iter().any(|t| t.covenant_id == real_hex), "the token with a holding is in the list");
}

// ------------------------------------------------------------------------------------------------------ I15

/// I15: the pair book of two tokens without a listed order does not walk the live set (0.7 s a scan, four scans, on 300,000 open bids
/// of another token), nor does one with a listed order on a single leg.
#[test]
fn i15_a_pair_route_without_listed_legs_does_not_scan_the_live_set() {
    use kobd::indexer::pairs::pair_book;
    let conn = open_memory("testnet-10").unwrap();
    let n: i64 = std::env::var("I15_ROWS").ok().and_then(|v| v.parse().ok()).unwrap_or(100_000);
    let tpl = token_template(TemplateId::Kcc20Ref8x8).hash.to_vec();
    let bid = kob_protocol::artifacts::pinned_hash(TemplateId::KobBid).to_vec();
    conn.execute(
        "INSERT OR IGNORE INTO ids (h) VALUES (?1), (?2), (x'7272727272727272727272727272727272727272727272727272727272727272')",
        params![bid, tpl],
    )
    .unwrap();
    conn.execute(
        "WITH RECURSIVE n(i) AS (SELECT 1 UNION ALL SELECT i + 1 FROM n WHERE i < ?1) \
         INSERT INTO orders (covenant_id, contract, template_id, family, side, maker_id, token_id, token_tpl_id, scale, price, in_book, genesis_state, genesis_block, genesis_daa, listed, origin) \
         SELECT randomblob(32), 'KobBid', (SELECT id FROM ids WHERE h = ?2), 1, 2, NULL, (SELECT id FROM ids WHERE h = x'7272727272727272727272727272727272727272727272727272727272727272'), (SELECT id FROM ids WHERE h = ?3), 1000, 1000, 1, randomblob(8), i, i, 1, 'chain' FROM n",
        params![n, bid, tpl],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO order_state (order_id, status, filled_amount, remaining_amount, amount_exact, cur_txid, cur_idx, cur_value, state_known, last_block, last_daa) \
         SELECT id, 'open', 0, 10, 1, randomblob(32), 0, 1000, 1, 1, 1 FROM orders",
        [],
    )
    .unwrap();
    let ctx = reads::ReadCtx { node_daa: Some(1_000), settle_depth_daa: 100, now_unix: Some(1_790_000_000) };
    let (a, b, t2) = (Hash32([0x61; 32]), Hash32([0x62; 32]), Hash32([0x72; 32]));
    for (base, quote) in [(a, b), (t2, b), (a, t2)] {
        let t = std::time::Instant::now();
        let book = pair_book(&conn, &ctx, &base, &quote, 10).unwrap();
        let ms = t.elapsed().as_millis();
        println!("I15 pair_book over {n} open bids of another token: {ms} ms");
        assert!(book.asks.is_empty() && book.bids.is_empty());
        if base != t2 && quote != t2 {
            assert!(ms < 100, "{ms} ms: the live set was scanned");
        }
    }
}
