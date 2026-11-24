//! A property test of the book rule (R7, B): for every kind and side a `book_live` or `pair_live` row can hold, orders are
//! drawn from a seeded random source over the parameter space the sanity check admits, the writer decides each row
//! (`book_window`, `pair_window_of`) at a commit's DAA, and then, at several DAA scores across the row's window (its first,
//! its last and random ones), the matcher's own candidate code says what one fill can take then. A row that is open and
//! placed (not dust) must, at every sampled time:
//! - offer a fill worth at least the dust line (half the listing minimum) at the price it is shown at;
//! - show no more than a fill can take then;
//! - show a price a taker can trade at: never better than what the fill gets (an ask's shown price is at least the
//!   fill's, a bid's at most; a curve's first shown level covers its smallest valid fill at that fill's mean price).
//!
//! `KOB_BOOK_PROPS_CASES` sets the cases per kind (default 1,500) and `KOB_BOOK_PROPS_SEED` the seed; a failure prints
//! both, the kind, the case and the time.

use super::*;
use crate::indexer::db::open_memory;
use crate::indexer::ids::intern;
use crate::matcher::candidate::{candidates_of, CandCtx};
use crate::testkit::{ask, bid, ifd_ask, ifd_bid, kron, pk, tok_fields, EXT, T3, TOKEN_COV};
use kob_protocol::state::{CurveAskState, IfdPairState, PairState, SIDE_ASK, SIDE_BID};

const DUST_BELOW: i64 = 50_000_000; // half the default listing minimum (1 KAS)
const A: [u8; 32] = [0x21; 32];
const B: [u8; 32] = [0x22; 32];

/// splitmix64: a small seeded source, so a failing case is reproduced from its seed alone.
struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        z ^ (z >> 31)
    }
    /// Uniform in `lo..=hi`.
    fn range(&mut self, lo: i64, hi: i64) -> i64 {
        if hi <= lo {
            return lo;
        }
        lo + (self.next() % ((hi - lo) as u64 + 1)) as i64
    }
    /// Log-uniform in `lo..=hi` (lo ≥ 0): every order of magnitude as likely, so the extremes are drawn.
    fn log(&mut self, lo: i64, hi: i64) -> i64 {
        if hi <= lo {
            return lo;
        }
        let (a, b) = (((lo.max(1)) as f64).ln(), (hi as f64).ln());
        let x = (a + (b - a) * (self.next() as f64 / u64::MAX as f64)).exp() as i64;
        x.clamp(lo, hi)
    }
    fn chance(&mut self, n: u64) -> bool {
        self.next().is_multiple_of(n)
    }
}

fn compiled(id: TemplateId) -> AnyState {
    let t = kob_protocol::artifacts::template(id);
    AnyState::decode(id, &t.contract().compiled.bytecode[1..1 + t.state_len]).unwrap()
}

/// One drawn order: its state, the KAS value of its UTXO, and whether its custodies are exact.
struct Drawn {
    state: AnyState,
    value: i64,
    exact: bool,
}

/// Inserts the order `k` as the indexer would record it, and returns its `orders.id`.
fn insert(c: &Connection, k: u32, d: &Drawn, udaa: i64) -> i64 {
    let s = &d.state;
    let t = s.template_id();
    let terms = crate::model::terms_of(s);
    let mut cov = [0u8; 32];
    cov[..4].copy_from_slice(&k.to_be_bytes());
    cov[31] = 0xc7;
    let mut utxo = cov;
    utxo[31] = 0xd7;
    let bytes = s.encode();
    let quote = s.pair_tokens().map(|p| intern(c, &p.b.cov_id).unwrap());
    let ext = s.custody_ext(s.token_cov_id()).map(|e| intern(c, &e).unwrap());
    // an if-done entry with a stop is in no book until it is armed (`processor`, as the writer records it)
    let unarmed = match s {
        AnyState::KobIfdAsk(a) | AnyState::KobIfdAskKron(a) => a.entry_stop > 0 && a.armed == 0,
        AnyState::KobIfdBid(b) | AnyState::KobIfdBidKron(b) => b.entry_stop > 0 && b.armed == 0,
        _ => false,
    };
    let in_book = crate::model::in_book(t) && !unarmed;
    c.execute(
        "INSERT INTO orders (covenant_id, contract, template_id, family, side, maker_id, token_id, scale, min_fill, price, tip, tif, \
         expiry_daa, active_from, in_book, initial_amount, genesis_state, genesis_block, genesis_daa, listed, quote_id, ext_id) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, 1, ?18, 1, ?19, ?20)",
        rusqlite::params![
            cov.to_vec(),
            t.name(),
            intern(c, &kob_protocol::artifacts::pinned_hash(t)).unwrap(),
            s.family().code() as i64,
            crate::model::side_of(s) as i64,
            intern(c, &pk(7)).unwrap(),
            intern(c, &s.token_cov_id()).unwrap(),
            terms.scale,
            terms.min_fill,
            if in_book { terms.price } else { None },
            terms.tip,
            terms.tif,
            terms.expiry_daa,
            terms.active_from,
            in_book as i64,
            s.amount_left(),
            bytes,
            udaa,
            quote,
            ext,
        ],
    )
    .unwrap();
    let id: i64 = c.query_row("SELECT id FROM orders WHERE covenant_id = ?1", [cov.to_vec()], |r| r.get(0)).unwrap();
    c.execute(
        "INSERT INTO order_utxos (txid, idx, order_id, value, spk, state, created_block, created_daa) VALUES (?1, 0, ?2, ?3, x'00', ?4, 1, ?5)",
        rusqlite::params![utxo.to_vec(), id, d.value, s.encode(), udaa],
    )
    .unwrap();
    c.execute(
        "INSERT INTO order_state (order_id, status, filled_amount, remaining_amount, amount_exact, cur_txid, cur_idx, cur_value, \
         state_known, last_block, last_daa) VALUES (?1, 'open', 0, ?2, 1, ?3, 0, ?4, 1, 1, ?5)",
        rusqlite::params![id, s.amount_left(), utxo.to_vec(), d.value, udaa],
    )
    .unwrap();
    let custodies: Vec<([u8; 32], i64)> = if s.is_pair() {
        s.custodies()
    } else {
        s.custody_amount().filter(|a| *a > 0).map(|a| (s.token_cov_id(), a)).into_iter().collect()
    };
    for (j, (token, amount)) in custodies.into_iter().filter(|c| c.1 != 0).enumerate() {
        let amount = if d.exact { amount } else { amount.saturating_add(1) };
        c.execute(
            "INSERT INTO token_utxos (txid, idx, token_id, order_id, amount, value, role, created_block, created_daa) \
             VALUES (?1, ?2, ?3, ?4, ?5, 10000000, 'custody', 1, ?6)",
            rusqlite::params![utxo.to_vec(), j as i64 + 1, intern(c, &token).unwrap(), id, amount, udaa],
        )
        .unwrap();
    }
    id
}

fn draw_ask(r: &mut Rng) -> Drawn {
    let price = r.log(1, 10_000_000_000_000_000);
    let amount = r.log(1, 1_000_000_000_000_000);
    let min_fill = if r.chance(2) { 1 } else { r.log(1, amount) };
    let max_fill = if r.chance(2) { 0 } else { r.log(min_fill, amount) };
    let decaying = r.chance(3);
    let price_end = if decaying { r.log(1, price) } else { 0 };
    let s = kob_protocol::state::AskState {
        price,
        tip: if r.chance(2) { 0 } else { r.log(0, price) },
        amount_left: amount,
        min_fill,
        max_fill,
        interval: if r.chance(3) { r.log(1, 100_000) } else { 0 },
        price_end,
        slope: if decaying { r.log(1, (price - price_end).max(1)) } else { 0 },
        decay_step: r.log(1, 1_000_000),
        active_from: if r.chance(4) { r.log(0, 2_000) } else { 0 },
        expiry_daa: if r.chance(2) { 1_000 + r.log(1, 100_000_000) } else { crate::testkit::EXPIRY },
        ..ask(7, price)
    };
    let state = AnyState::KobAsk(s);
    Drawn { state: if r.chance(2) { kron(state) } else { state }, value: r.log(100_000, 100_000_000_000), exact: !r.chance(10) }
}

fn draw_bid(r: &mut Rng) -> Drawn {
    let price = r.log(1, 10_000_000_000_000_000);
    let rising = r.chance(3);
    let price_end = if rising { r.log(price, 10_000_000_000_000_000) } else { 0 };
    let min_fill = r.log(1, 1_000_000_000_000);
    let s = kob_protocol::state::BidState {
        price,
        tip: if r.chance(2) { 0 } else { r.log(0, price) },
        min_fill,
        max_fill: if r.chance(2) { 0 } else { r.log(min_fill, 1_000_000_000_000_000) },
        interval: if r.chance(3) { r.log(1, 100_000) } else { 0 },
        price_end,
        slope: if rising { r.log(1, (price_end - price).max(1)) } else { 0 },
        decay_step: r.log(1, 1_000_000),
        delivery_carrier: r.log(0, 1_000_000_000),
        reserve: if r.chance(3) { r.log(0, 1_000_000_000) } else { 0 },
        active_from: if r.chance(4) { r.log(0, 2_000) } else { 0 },
        expiry_daa: if r.chance(2) { 1_000 + r.log(1, 100_000_000) } else { crate::testkit::EXPIRY },
        ..bid(7, price)
    };
    let state = AnyState::KobBid(s);
    Drawn { state: if r.chance(2) { kron(state) } else { state }, value: r.log(100_000, 10_000_000_000_000), exact: true }
}

fn draw_curve(r: &mut Rng) -> Drawn {
    let (hh, p, s) = tok_fields(T3);
    let level_size = r.log(1, 1_000_000_000_000);
    let n = r.range(1, 20) as usize;
    let mut table: Vec<i64> = (0..n).map(|_| r.log(1, 10_000_000_000_000_000)).collect();
    if r.chance(2) {
        table.sort_unstable();
    }
    let cap = level_size.saturating_mul(n as i64);
    let total = if r.chance(4) { cap.saturating_add(r.log(1, cap.max(1))) } else { cap };
    let sold = if r.chance(2) { 0 } else { r.range(0, total - 1) };
    let amount_left = total - sold;
    let state = AnyState::KobCurveAsk(CurveAskState {
        maker: pk(7),
        token_cov_id: TOKEN_COV,
        token_tpl_hash: hh,
        tpl_prefix_len: p,
        tpl_suffix_len: s,
        scale: crate::testkit::SCALE,
        min_fill: if r.chance(2) { 1 } else { r.log(1, amount_left) },
        tip: if r.chance(2) { 0 } else { r.log(0, *table.iter().min().unwrap()) },
        active_from: if r.chance(4) { r.log(0, 2_000) } else { 0 },
        level_size,
        total,
        amount_left,
        extension_commitment: EXT,
        table,
    });
    Drawn { state, value: r.log(100_000, 100_000_000_000), exact: !r.chance(10) }
}

fn draw_ifd_bid(r: &mut Rng) -> Drawn {
    let amount = r.log(1, 1_000_000_000_000);
    let price = r.log(1, 10_000_000_000_000);
    let stop = if r.chance(2) { 0 } else { r.log(1, 10_000_000_000_000) };
    let s = kob_protocol::state::IfdBidState {
        amount_left: amount,
        price,
        tip: if r.chance(2) { 0 } else { r.log(0, price) },
        min_fill: r.log(1, amount),
        entry_stop: stop,
        armed: if stop > 0 && r.chance(2) { r.range(1, 1_500) } else { 0 },
        band_daa: if r.chance(2) { 0 } else { r.log(1, 100_000) },
        delivery_carrier: r.log(0, 1_000_000_000),
        exit_carrier: r.log(0, 1_000_000_000),
        expiry_daa: if r.chance(2) { 1_000 + r.log(1, 100_000_000) } else { crate::testkit::EXPIRY },
        ..ifd_bid(7, 10)
    };
    // the escrow from nothing to more than enough, around what the protocol computes
    let need = s.escrow().unwrap_or(1_000_000_000);
    let value = match r.range(0, 2) {
        0 => need,
        1 => r.log(100_000, need.max(100_000)),
        _ => need.saturating_add(r.log(0, 10_000_000_000)),
    };
    let state = AnyState::KobIfdBid(s);
    Drawn { state: if r.chance(2) { kron(state) } else { state }, value, exact: true }
}

fn draw_ifd_ask(r: &mut Rng) -> Drawn {
    let amount = r.log(1, 1_000_000_000_000);
    let price = r.log(1, 10_000_000_000_000);
    let stop = if r.chance(2) { 0 } else { r.log(1, 10_000_000_000_000) };
    let s = kob_protocol::state::IfdAskState {
        amount_left: amount,
        price,
        tip: if r.chance(2) { 0 } else { r.log(0, price) },
        min_fill: r.log(1, amount),
        entry_stop: stop,
        armed: if stop > 0 && r.chance(2) { r.range(1, 1_500) } else { 0 },
        band_daa: if r.chance(2) { 0 } else { r.log(1, 100_000) },
        prefund: r.log(0, 10_000_000_000),
        exit_carrier: r.log(0, 1_000_000_000),
        expiry_daa: if r.chance(2) { 1_000 + r.log(1, 100_000_000) } else { crate::testkit::EXPIRY },
        ..ifd_ask(7)
    };
    let state = AnyState::KobIfdAsk(s);
    Drawn { state: if r.chance(2) { kron(state) } else { state }, value: r.log(100_000, 10_000_000_000_000), exact: !r.chance(10) }
}

fn pair_base() -> PairState {
    let (th, p, s) = tok_fields(T3);
    let AnyState::KobPair(x) = compiled(TemplateId::KobPair) else { unreachable!() };
    PairState {
        maker: pk(7),
        s_cov_id: A,
        s_tpl_hash: th,
        s_pre: p,
        s_suf: s,
        s_family: 1,
        s_scale: 1000,
        t_cov_id: B,
        t_tpl_hash: th,
        t_pre: p,
        t_suf: s,
        t_family: 1,
        t_ext: EXT,
        t_scale: 1000,
        s_ext: EXT,
        tif: 0,
        mode: 0,
        recv_threshold: 0,
        topup_tip: 0,
        bought: 0,
        refund_tip: crate::testkit::rtip(T3),
        ..x
    }
}

fn draw_pair(r: &mut Rng) -> Drawn {
    let ask = r.chance(2);
    let price = r.log(1, 1_000_000_000_000);
    let auction = r.chance(2);
    let end = if !auction {
        0
    } else if ask {
        r.log(1, price)
    } else {
        r.log(price, 1_000_000_000_000)
    };
    let amount = r.log(1, 1_000_000_000_000);
    let min_fill = r.log(1, amount);
    let mut x = PairState {
        side: if ask { SIDE_ASK } else { SIDE_BID },
        price,
        price_end: end,
        slope: if auction { r.log(1, (price - end).abs().max(1)) } else { 0 },
        decay_step: r.log(1, 1_000_000),
        amount_left: amount,
        min_fill,
        max_fill: if r.chance(2) { 0 } else { r.log(min_fill, amount) },
        interval: if r.chance(3) { r.log(1, 100_000) } else { 0 },
        tip: 0,
        active_from: if r.chance(4) { r.log(0, 2_000) } else { 0 },
        expiry_daa: 1_000 + r.log(1, 100_000_000),
        delivery_carrier: r.log(0, 1_000_000_000),
        custody: 0,
        ..pair_base()
    };
    // an ask holds its amount of A; a bid holds B, from nothing to more than enough
    x.custody = if ask { amount } else { r.log(1, 1_000_000_000_000_000) };
    Drawn { state: AnyState::KobPair(x), value: r.log(100_000, 100_000_000_000), exact: true }
}

fn draw_ifd_pair(r: &mut Rng) -> Drawn {
    let (th, p, s) = tok_fields(T3);
    let AnyState::KobIfdPair(base) = compiled(TemplateId::KobIfdPair) else { unreachable!() };
    let buy = r.chance(2);
    let price = r.log(1, 1_000_000_000_000);
    let stop = if r.chance(2) { 0 } else { r.log(1, 1_000_000_000_000) };
    let amount = r.log(1, 1_000_000_000_000);
    let mut e = IfdPairState {
        maker: pk(7),
        side: if buy { SIDE_BID } else { SIDE_ASK },
        a_cov_id: A,
        a_tpl_hash: th,
        a_pre: p,
        a_suf: s,
        a_family: 1,
        a_scale: 1000,
        a_ext: EXT,
        b_cov_id: B,
        b_tpl_hash: th,
        b_pre: p,
        b_suf: s,
        b_family: 1,
        b_scale: 1000,
        b_ext: EXT,
        price,
        tip: 0,
        active_from: 0,
        expiry_daa: 1_000 + r.log(1, 100_000_000),
        refund_tip: crate::testkit::rtip(T3),
        min_fill: r.log(1, amount),
        entry_stop: stop,
        band_daa: if r.chance(2) { 0 } else { r.log(1, 100_000) },
        armed: if stop > 0 && r.chance(2) { r.range(1, 1_500) } else { 0 },
        amount_left: amount,
        prefund: if buy { 0 } else { r.log(0, 1_000_000_000_000) },
        delivery_carrier: r.log(0, 1_000_000_000),
        exit_carrier: r.log(0, 1_000_000_000),
        ..base
    };
    // the exit it commits to: an OCO of the other side, of the same two tokens
    let AnyState::KobCondPair(cb) = compiled(TemplateId::KobCondPair) else { unreachable!() };
    let (sc, tc) = if buy { (A, B) } else { (B, A) };
    let exit = kob_protocol::state::CondPairState {
        maker: pk(7),
        side: if buy { SIDE_ASK } else { SIDE_BID },
        s_cov_id: sc,
        s_tpl_hash: th,
        s_pre: p,
        s_suf: s,
        s_family: 1,
        s_scale: 1000,
        s_ext: EXT,
        t_cov_id: tc,
        t_tpl_hash: th,
        t_pre: p,
        t_suf: s,
        t_family: 1,
        t_scale: 1000,
        t_ext: EXT,
        tp_price: if buy { price.saturating_mul(2) } else { (price / 2).max(1) },
        stop_price: if buy { (price / 2).max(1) } else { price.saturating_mul(2) },
        expiry_daa: crate::testkit::NO_EXPIRY,
        refund_tip: crate::testkit::rtip(T3),
        amount_left: 0,
        custody: 0,
        armed: 0,
        parent: [0; 32],
        ..cb
    };
    e.exit_state = IfdPairState::commit_exit(&exit);
    let need = e.b_custody_needed().unwrap_or(0);
    e.custody = match r.range(0, 2) {
        0 => need,
        1 => r.log(0, need.max(1)),
        _ => need.saturating_add(r.log(0, 1_000_000_000_000)),
    };
    Drawn { state: AnyState::KobIfdPair(e), value: r.log(100_000, 100_000_000_000), exact: true }
}

/// The DAA scores a row is checked at: its first and last of the window from the commit `d`, and three in between.
fn times(r: &mut Rng, first: i64, last: i64) -> Vec<i64> {
    let mut v = vec![first, last];
    for _ in 0..3 {
        v.push(r.range(first, last));
    }
    v
}

/// A state's debug form without its hashes and keys, to keep a failure readable.
fn short(s: &str) -> String {
    let mut out = String::new();
    let mut depth = 0;
    for ch in s.chars() {
        match ch {
            '[' => {
                depth += 1;
                if depth == 1 {
                    out.push_str("[..]");
                }
            }
            ']' => depth -= 1,
            _ if depth > 0 => {}
            _ => out.push(ch),
        }
    }
    out
}

#[test]
fn the_book_rule_holds_for_every_kind_at_every_time_of_its_window() {
    let _heavy = super::bounds_tests::heavy();
    let cases: usize = std::env::var("KOB_BOOK_PROPS_CASES").ok().and_then(|v| v.parse().ok()).unwrap_or(1_500);
    let seed: u64 = std::env::var("KOB_BOOK_PROPS_SEED").ok().and_then(|v| v.parse().ok()).unwrap_or(0x6b6f_6237);
    type Gen = fn(&mut Rng) -> Drawn;
    let kinds: [(&str, Gen); 7] = [
        ("ask (plain, TWAP, decaying; KRON)", draw_ask),
        ("bid (plain, DCA, rising; KRON)", draw_bid),
        ("curve ask", draw_curve),
        ("if-done buy-first (armed or not; KRON)", draw_ifd_bid),
        ("if-done sell-first (armed or not; KRON)", draw_ifd_ask),
        ("pair (plain, TWAP, decaying ask, rising bid)", draw_pair),
        ("if-done pair (armed or not)", draw_ifd_pair),
    ];
    let mut failures: Vec<String> = vec![];
    let started = std::time::Instant::now();
    for (kind, gen) in kinds {
        let conn = open_memory("testnet-10").unwrap();
        let mut r = Rng(seed ^ (kind.len() as u64).wrapping_mul(0x1000_0001));
        let (mut sane, mut placed, mut checks) = (0usize, 0usize, 0usize);
        let mut insane: Option<String> = None;
        for case in 0..cases {
            let d = gen(&mut r);
            if let Err(e) = crate::sanity::check(&d.state) {
                insane.get_or_insert(e);
                continue;
            }
            sane += 1;
            let udaa = r.range(1, 1_000);
            let commit = udaa + if r.chance(2) { 0 } else { r.log(0, 1_000_000) };
            let id = insert(&conn, case as u32, &d, udaa);
            let pair = d.state.is_pair();
            let w = if pair {
                super::super::pairs::pair_window_of(&conn, id, commit).unwrap()
            } else {
                book_window(&conn, id, DUST_BELOW, commit).unwrap()
            };
            let Some(w) = w else { continue };
            let in_book: bool = conn.query_row("SELECT in_book FROM orders WHERE id = ?1", [id], |r| r.get(0)).unwrap();
            let open = (pair || in_book) && w.ok && w.open_from <= commit && commit < w.closed_from && (pair || !w.dust);
            if !open {
                continue;
            }
            placed += 1;
            let Some(lo) = super::super::book::listed_order_of(&conn, id).unwrap() else {
                failures.push(format!("{kind} seed {seed} case {case}: placed but not a listed order the matcher reads"));
                continue;
            };
            let last = w.closed_from.saturating_sub(1).min(commit.saturating_add(100_000_000));
            for t in times(&mut r, commit.max(w.open_from).max(udaa), last.max(commit)) {
                checks += 1;
                let at = |what: &str| {
                    let st = format!("{:?}", d.state);
                    format!(
                        "{kind} seed {seed} case {case} at DAA {t} (commit {commit}, udaa {udaa}, value {}): {what}; {}",
                        d.value,
                        short(&st)
                    )
                };
                if pair {
                    let spec = w.pair.expect("a placed pair row has its spec");
                    match crate::indexer::takeable::pair_offer(&lo, t) {
                        None => failures.push(at("placed, but no fill can take anything then")),
                        Some((cap, price)) => {
                            if spec.amount > cap {
                                failures.push(at(&format!("shows {} base units, one fill takes {cap}", spec.amount)));
                            }
                            if let Some(q) = spec.quote_at(t) {
                                if (spec.ask && q < price) || (!spec.ask && q > price) {
                                    failures.push(at(&format!("shows quote {q}, the fill trades at {price}")));
                                }
                            }
                        }
                    }
                    continue;
                }
                let off = crate::indexer::takeable::kas_offer(&lo, t);
                if off.value < DUST_BELOW as i128 {
                    failures.push(at(&format!("placed, but one fill is worth {} sompi then", off.value)));
                }
                let terms = crate::model::terms_of(&d.state);
                let shown = shown_levels(&d.state, udaa, terms.price.unwrap_or(0), d.state.amount_left(), &w, t);
                let total: i64 = shown.iter().map(|l| l.2.unwrap_or(0)).sum();
                if total > off.amount {
                    failures.push(at(&format!("shows {total} base units, one fill takes {}", off.amount)));
                }
                let (ex, by) = (std::collections::BTreeSet::new(), std::collections::BTreeMap::new());
                let cx = CandCtx { t, utc: 0, excluded: &ex, by_id: &by, unaccepted: &ex, pair_ticks: None };
                let cands = candidates_of(&[&lo], &cx);
                let ask_side = crate::model::side_of(&d.state) == 1;
                if let (Some(first), Some(c0)) = (shown.first(), cands.first()) {
                    let fill_price = match (&d.state, &c0.curve) {
                        // a curve's smallest valid fill is its minimum fill, at the mean of its levels
                        (AnyState::KobCurveAsk(cv), Some(leg)) => {
                            let m = leg.rules.min_fill.max(1);
                            if first.2.unwrap_or(0) < m.min(cv.amount_left) {
                                failures.push(at(&format!("first level shows {:?} units, the smallest fill is {m}", first.2)));
                            }
                            crate::model::curve_fill_price(cv, m.min(cv.amount_left))
                        }
                        _ => {
                            let qs = cands.iter().map(|c| c.quote);
                            if ask_side {
                                qs.max()
                            } else {
                                qs.min()
                            }
                        }
                    };
                    if let Some(fp) = fill_price {
                        if (ask_side && first.1 < fp) || (!ask_side && first.1 > fp) {
                            failures.push(at(&format!("shows price {}, the fill trades at {fp}", first.1)));
                        }
                    }
                }
            }
        }
        eprintln!(
            "book rule property: {kind}: {cases} drawn, {sane} sane, {placed} placed, {checks} checks; first refused: {insane:?}"
        );
        if sane * 4 < cases {
            failures.push(format!("{kind}: the generator draws too few sane orders ({sane} of {cases}), first refused: {insane:?}"));
        }
        if placed < cases / 50 {
            failures.push(format!("{kind}: too few placed rows to test ({placed})"));
        }
    }
    eprintln!("book rule property: {:?} in all", started.elapsed());
    if !failures.is_empty() {
        // the first case of each kind and kind of violation
        let mut seen: std::collections::BTreeMap<String, (usize, String)> = Default::default();
        for f in &failures {
            let kind = f.split(" seed ").next().unwrap_or("").to_string();
            let what = f.split("): ").nth(1).unwrap_or(f).split([',', ';']).next().unwrap_or("");
            let what: String = what.chars().filter(|c| !c.is_ascii_digit()).collect();
            let e = seen.entry(format!("{kind} | {what}")).or_insert((0, f.clone()));
            e.0 += 1;
        }
        let report: Vec<String> = seen.into_iter().map(|(k, (n, f))| format!("{n} x {k}\n    e.g. {f}")).collect();
        panic!("{} violations of the book rule:\n{}", failures.len(), report.join("\n"));
    }
}
