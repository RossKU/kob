//! Read-side queries over the indexer database, used by the REST and WebSocket API.
//!
//! The conventions are a stable API contract:
//! - Ids, hashes and keys are lowercase hex.
//! - Every sompi amount (`value`, `payout`, `price`, `tip`, `budget_rate`, `reserve`, `cur_value`)
//!   and every token base-unit quantity (`amount`, `amount_left`, `filled_amount`,
//!   `initial_amount`, `min_fill`) is a decimal string, so that JavaScript clients cannot lose
//!   precision.
//! - Prices and tips are sompi per whole token, that is per `scale` base units of the order's
//!   token. `scale` is `10^decimals` of the token, at most `10^9`. The price of a pair order is
//!   token B base units per whole A.
//! - Scales, counts, DAA scores, block sequence numbers and indices are JSON numbers.
//! - Every view derived from a chain position carries `confirmations` (node DAA minus the DAA of
//!   the chain block) and `settled` (confirmations >= the configured settle depth). Both are
//!   `null` or `false` while the node's DAA is unknown.

use crate::hex::{self, Hash32};
use crate::indexer::db::{DbError, DbResult};
use crate::indexer::ids::{id_of, order_of};
use crate::indexer::packed::{sql_genesis, sql_order_ids, sql_order_ids_more, sql_ustate, GENESIS_O, ORDER_IDS_O};
use crate::tokens::TokenAllowlist;
use kob_protocol::artifacts::TemplateId;
use kob_protocol::state::{armed_origin, AnyState};
use rusqlite::types::Value as SqlValue;
use rusqlite::{params_from_iter, Connection, OptionalExtension, Row};
use serde::Serialize;
use serde_json::Value;
use std::collections::BTreeMap;

/// Context for deriving `confirmations`, `settled` and `expired`.
#[derive(Debug, Clone, Copy)]
pub struct ReadCtx {
    /// DAA score of the node, when known.
    pub node_daa: Option<u64>,
    /// Confirmations a view needs to count as settled.
    pub settle_depth_daa: u64,
    /// Wall clock in UTC unix seconds, for day-order deadlines.
    pub now_unix: Option<u64>,
}

impl ReadCtx {
    /// Node DAA minus `daa`, at least 0. `None` while the node's DAA is unknown.
    pub fn confirmations(&self, daa: i64) -> Option<i64> {
        self.node_daa.map(|n| (n as i64).saturating_sub(daa).max(0))
    }

    /// Whether `daa` has at least the settle depth of confirmations.
    pub fn settled(&self, daa: i64) -> bool {
        self.confirmations(daa).is_some_and(|c| c >= self.settle_depth_daa as i64)
    }

    /// The end of a rolling 24 h window, in ms.
    ///
    /// It is the newer of the newest chain block time the store holds an event for
    /// (`newest_event`) and the wall clock, so that the figures of a quiet market age out instead
    /// of standing still at its last event. `None` when neither is known.
    pub fn window_end(&self, newest_event: Option<i64>) -> Option<i64> {
        let clock = self.now_unix.map(|s| (s.min(i64::MAX as u64 / 1_000) as i64) * 1_000);
        newest_event.max(clock)
    }
}

fn hx(b: Vec<u8>) -> String {
    hex::encode(&b)
}

fn hxo(b: Option<Vec<u8>>) -> Option<String> {
    b.map(|b| hex::encode(&b))
}

/// A 64-bit amount (sompi or token base units) as a decimal string.
fn sompi(v: Option<i64>) -> Option<String> {
    v.map(|v| v.to_string())
}

/// A page of results with an opaque continuation cursor.
#[derive(Debug, Clone, Serialize)]
pub struct Page<T> {
    /// The rows of this page.
    pub items: Vec<T>,
    /// Cursor of the next page. `None` on the last page.
    pub next_cursor: Option<String>,
}

// ---------------------------------------------------------------------------------------------
// Tokens

/// A token as `GET /v1/tokens` lists it.
#[derive(Debug, Clone, Serialize)]
pub struct TokenView {
    /// Ticker from the registry. Empty for a token of the open list.
    pub ticker: String,
    /// Covenant id of the token, hex.
    pub covenant_id: String,
    /// Template hash of the token's program, hex.
    pub template_hash: Option<String>,
    /// KCC-20 extension commitment, hex.
    pub extension_commitment: Option<String>,
    /// `kcc20` or `kron`, when the allowlist names the token's family.
    pub family: Option<&'static str>,
    /// Decimals of the token, when known.
    pub decimals: Option<u32>,
    /// Where `decimals` come from. Absent without `decimals`.
    ///
    /// The values are `declaration` (the verified KCC-23 declaration of the token), `registry` (the
    /// registry entry) and `program` (the default of the token's program, a launchpad's
    /// convention).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub decimals_source: Option<&'static str>,
    /// The standard scale of the token's orders (`10^decimals`, at most `10^9`).
    ///
    /// An order is listed only at this scale, so the listed orders of a token quote the same
    /// whole token. `null` without `decimals`.
    pub scale: Option<i64>,
    /// The scale of the earliest order of a token without `decimals`, open or ended
    /// ([`market_scale`]).
    ///
    /// A token without `decimals` has no registry entry. Its first order sets the price scale of
    /// its market, and the scale stays the market's after every order of it has ended. A client
    /// prices new orders at it and refuses decimals that give another scale. `null` for a token
    /// with `decimals`, because a declared or program-default scale is the market's and the
    /// executor lists only orders at it, and for a token no order of which was ever seen.
    pub market_scale: Option<i64>,
    /// `true` when order placements name more than one program of the strict list for this
    /// covenant id. Absent otherwise.
    ///
    /// The indexer holds no token output, so it cannot tell which program is the token's, and it
    /// names none: `template_hash`, `template_id`, `family` and `extension_commitment` are
    /// `null`, and `candidate_templates` lists the claimed programs. A client proves the program
    /// from a token output of the covenant id that the node confirms.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub program_ambiguous: Option<bool>,
    /// The programs claimed for an ambiguous token (template hashes, hex).
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub candidate_templates: Vec<String>,
    /// The registry document the token's facts come from.
    ///
    /// The facts are `decimals`, `genesis_verified` and the derived `warnings`. The document is
    /// this operator's copy, not chain data. A client verifies against `registry.sha256` or
    /// ignores it. No standing is served: KOB makes no judgement about a token. Tickers collide,
    /// so identify a token by covenant id plus template hash.
    pub registry: Option<crate::tokens::RegistryInfo>,
    /// Whether every genesis output of the token was checked against the pinned program.
    ///
    /// It is the registry's `genesis_verified`. `null`: the registry does not say.
    pub genesis_verified: Option<bool>,
    /// Warnings a client must show, derived from the registry's recorded facts.
    ///
    /// - `genesis_unverified`: a hidden genesis output could mint look-alike tokens later. It is
    ///   given unless the registry says `genesis_verified: true`.
    /// - `live_mint_authority`: the genesis record of the token lists a live mint authority, so
    ///   its supply can still grow.
    pub warnings: Vec<&'static str>,
    /// Capabilities of the token's program that reach balances or supply.
    ///
    /// Examples are `freeze`, `seize`, `blacklist` and `mint-authority`. A UI labels tokens whose
    /// program can freeze or seize.
    pub powers: Vec<String>,
    /// Registry id of the token's program, when known.
    pub template_id: Option<String>,
    /// Listed, open or partially filled asks (side 1).
    pub open_asks: i64,
    /// Listed, open or partially filled bids (side 2).
    pub open_bids: i64,
    /// `true` when the indexer holds a verified KCC-23 declaration for the covenant id. Absent
    /// otherwise.
    ///
    /// `GET /v1/tokens/{covenant_id}/declaration` serves its launch proof.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub declaration: Option<bool>,
}

/// The one KCC-20 extension commitment that the order placements of a token and program named.
///
/// The source is `token_events`. The result is `None` when the placements named none or more
/// than one (a genesis that minted a second commitment, or an order that claims another). A
/// client then takes it from the token's orders and shows where it came from. The all-zero
/// commitment (the standard fixed-supply class) is one like any other: a placement that does not
/// know a token's commitment records none (NULL), not zero.
fn sole_extension(conn: &Connection, cov: &[u8], tpl: &[u8]) -> DbResult<Option<String>> {
    // Two seeks of `token_events_token_tpl` (the smallest commitment, then the smallest above it),
    // however many events the token has.
    let mut st =
        conn.prepare_cached("SELECT MIN(ext_commit) FROM token_events WHERE token_cov_id = ?1 AND tpl_hash = ?2 AND ext_commit > ?3")?;
    let mut next = |after: &[u8]| st.query_row(rusqlite::params![cov, tpl, after], |r| r.get::<_, Option<Vec<u8>>>(0));
    Ok(match next(&[])? {
        Some(first) if next(&first)?.is_none() => Some(hex::encode(&first)),
        _ => None,
    })
}

/// Most distinct programs [`claimed_programs`] reads from a token's orders.
///
/// The programs of the orders are rows of `ids`, in the order they were first seen and not in the
/// order of their hashes. So they are read in full, up to this many, and then sorted.
const CLAIMED_ORDER_PROGRAMS_MAX: usize = 1_024;

/// The distinct programs the placements of a token name, at most `limit`, ascending.
///
/// The placements are its order placements and its registry events. The read is a skip-scan of
/// `orders_token_tpl` (by program id) and `token_events_token_tpl`, with one seek per program
/// however many orders and events the token has.
fn claimed_programs(conn: &Connection, token: &[u8], limit: usize) -> DbResult<Vec<Vec<u8>>> {
    let mut ev = conn.prepare_cached("SELECT MIN(tpl_hash) FROM token_events WHERE token_cov_id = ?1 AND tpl_hash > ?2")?;
    let mut or = conn.prepare_cached(concat!(
        "SELECT MIN(token_tpl_id) FROM orders WHERE token_id = ",
        id_of!("?1"),
        " AND token_tpl_id > ?2"
    ))?;
    let mut set: std::collections::BTreeSet<Vec<u8>> = std::collections::BTreeSet::new();
    let mut prev = 0i64;
    while set.len() < CLAIMED_ORDER_PROGRAMS_MAX {
        let Some(next) = or.query_row(rusqlite::params![token, prev], |r| r.get::<_, Option<i64>>(0))? else { break };
        prev = next;
        set.insert(crate::indexer::ids::hash_of(conn, next)?);
    }
    let mut out: Vec<Vec<u8>> = Vec::new();
    let mut prev: Vec<u8> = Vec::new();
    while out.len() < limit {
        let a = ev.query_row(rusqlite::params![token, prev], |r| r.get::<_, Option<Vec<u8>>>(0))?;
        let b = set.range::<Vec<u8>, _>((std::ops::Bound::Excluded(&prev), std::ops::Bound::Unbounded)).next().cloned();
        let Some(next) = [a, b].into_iter().flatten().min() else { break };
        prev.clone_from(&next);
        out.push(next);
    }
    Ok(out)
}

/// Whether the placements of a token name a program other than `tpl`.
///
/// The placements are the orders and the registry events. It takes four seeks, however many
/// orders and events the token has, because it runs for every order the indexer lists at the
/// open-list standard scale.
fn claims_another_program(conn: &Connection, token: &[u8], tpl: &[u8]) -> DbResult<bool> {
    for sql in [
        "SELECT 1 FROM token_events WHERE token_cov_id = ?1 AND tpl_hash > ?2 LIMIT 1",
        "SELECT 1 FROM token_events WHERE token_cov_id = ?1 AND tpl_hash < ?2 LIMIT 1",
    ] {
        if conn.prepare_cached(sql)?.exists(rusqlite::params![token, tpl])? {
            return Ok(true);
        }
    }
    // The orders name their program by id, so another program is another id. A program no order
    // names has no id, so every program an order names differs from it.
    Ok(match crate::indexer::ids::lookup(conn, tpl)? {
        Some(id) => {
            conn.prepare_cached(concat!("SELECT 1 FROM orders WHERE token_id = ", id_of!("?1"), " AND token_tpl_id > ?2 LIMIT 1"))?
                .exists(rusqlite::params![token, id])?
                || conn
                    .prepare_cached(concat!("SELECT 1 FROM orders WHERE token_id = ", id_of!("?1"), " AND token_tpl_id < ?2 LIMIT 1"))?
                    .exists(rusqlite::params![token, id])?
        }
        None => conn
            .prepare_cached(concat!("SELECT 1 FROM orders WHERE token_id = ", id_of!("?1"), " AND token_tpl_id IS NOT NULL LIMIT 1"))?
            .exists([token])?,
    })
}

/// Most of a token's orders (oldest first) that [`market_scale`] reads.
pub const MARKET_SCALE_SCAN_MAX: i64 = 1_000;

/// The price scale of the market of a token without `decimals`.
///
/// It is the scale of the earliest listed order, open or ended, because the first order of the
/// token set it. If none of the oldest [`MARKET_SCALE_SCAN_MAX`] orders was listed, it is the
/// scale of the earliest order. `None` when no order of the token was ever seen. Imported history
/// (`genesis_block` 0) counts as the oldest. Only scales an order can have (powers of ten up to
/// 10^9) count. The read is bounded to the oldest rows of the token (index
/// `orders_token_genesis`).
pub fn market_scale(conn: &Connection, cov: &[u8]) -> DbResult<Option<i64>> {
    let mut st = conn.prepare_cached(
        "SELECT scale FROM (SELECT scale, listed, genesis_block, covenant_id FROM orders WHERE token_id = (SELECT id FROM ids WHERE h = ?1) \
         ORDER BY genesis_block, covenant_id LIMIT ?2) \
         WHERE scale IN (1, 10, 100, 1000, 10000, 100000, 1000000, 10000000, 100000000, 1000000000) \
         ORDER BY listed DESC, genesis_block, covenant_id LIMIT 1",
    )?;
    Ok(st.query_row(rusqlite::params![cov, MARKET_SCALE_SCAN_MAX], |r| r.get(0)).optional()?)
}

/// The programs the chain proves a token has, as template hashes.
///
/// The proof is the programs of its holdings (`token_holdings`). A holding is an output with the
/// covenant id whose script is the P2SH of that program and a state, so the covenant itself
/// created it under that program. This covers every family: a KCC-20 token has one program, and
/// a KRON token has programs of several slot counts under one covenant id. The result is empty
/// when nothing proves it, that is, when no holding of the token was seen. Order placements prove
/// nothing, because anyone can pin any program for any covenant id.
///
/// The read is a skip-scan over `holding_programs` (`MIN(program)`, then the next above it). The
/// triggers keep that table from the holdings (`schema.sql`). It costs one seek per distinct
/// program, however many holdings the token has, and at most one seek per program this build
/// knows. Measured costs: `docs/ops/executor.md`, 5.5.
pub fn proven_programs(conn: &Connection, token: &[u8]) -> DbResult<Vec<Vec<u8>>> {
    let mut st = conn.prepare_cached(concat!(
        "SELECT MIN(program) FROM holding_programs WHERE token_id = ",
        id_of!("?1"),
        " AND program > ?2"
    ))?;
    let mut names: Vec<String> = Vec::new();
    let mut prev = String::new();
    while names.len() <= TemplateId::ALL.len() {
        let Some(next) = st.query_row(rusqlite::params![token, prev], |r| r.get::<_, Option<String>>(0))? else { break };
        prev.clone_from(&next);
        names.push(next);
    }
    Ok(names
        .iter()
        .filter_map(|n| kob_protocol::artifacts::TemplateId::from_name(n))
        .filter_map(kob_protocol::artifacts::try_token_template)
        .map(|t| t.hash.to_vec())
        .collect())
}

/// Most (token, program) rows the open part of [`token_list`] reads.
///
/// The open part holds the tokens without a registry entry.
pub const OPEN_TOKEN_ROWS_MAX: i64 = 5_000;

/// The tokens of `GET /v1/tokens`: the allowlist's tokens, then the open list.
///
/// Each view carries the open ask and bid counts and the decimals of its token.
pub fn token_list(conn: &Connection, tokens: &TokenAllowlist) -> DbResult<Vec<TokenView>> {
    let mut counts: BTreeMap<(Vec<u8>, i64), i64> = BTreeMap::new();
    // The orders the books list (`BOOK_WHERE`, a template this build pins): an ask only with its
    // exact custody, and nothing with no amount left. They are read from the live set,
    // `order_state` first ([`LIVE_FROM`]).
    let pinned = pinned_template();
    let mut st = conn.prepare(&format!(
        "SELECT tk.h, o.side, COUNT(*) {LIVE_FROM} JOIN ids tk ON tk.id = o.token_id \
         WHERE o.listed = 1 AND o.in_book = 1 AND s.status IN ('open','partial') AND s.state_known = 1 AND {NOT_FROZEN} \
         AND {HAS_AMOUNT} AND {CUSTODY_OK} AND {pinned} GROUP BY o.token_id, o.side",
    ))?;
    let rows = st.query_map([], |r| Ok((r.get::<_, Vec<u8>>(0)?, r.get::<_, i64>(1)?, r.get::<_, i64>(2)?)))?;
    for r in rows {
        let (t, side, n) = r?;
        counts.insert((t, side), n);
    }
    let mut out: Vec<TokenView> = tokens
        .list()
        .into_iter()
        .map(|t| {
            let id = t.covenant_id.0.to_vec();
            TokenView {
                ticker: t.ticker.clone(),
                covenant_id: t.covenant_id.to_hex(),
                template_hash: t.template_hash.map(|h| h.to_hex()),
                extension_commitment: t.extension_commitment.map(|h| h.to_hex()),
                family: t.family.map(|f| f.as_str()),
                decimals: t.decimals,
                decimals_source: t.decimals.map(|_| "registry"),
                scale: t.scale(),
                market_scale: None,
                program_ambiguous: None,
                candidate_templates: Vec::new(),
                registry: tokens.info().cloned(),
                genesis_verified: tokens.genesis_verified(&t.covenant_id),
                warnings: tokens.warnings_for(&t.covenant_id),
                powers: t.powers.clone(),
                template_id: t.template_id.clone(),
                open_asks: counts.get(&(id.clone(), 1)).copied().unwrap_or(0),
                open_bids: counts.get(&(id, 2)).copied().unwrap_or(0),
                declaration: None,
            }
        })
        .collect();
    // The open token list holds:
    // - Tokens of a strict-list program that have listed orders but no registry entry (no facts,
    //   no ticker).
    // - The quote token B of a listed pair order, with its program from the token identities its
    //   placement recorded.
    // - Every token identity an order placement recorded (`token_events`). An order that was not
    //   listed (below the minimum value, say) still shows the token's program, so the token is
    //   in the list and its first listed order can be placed from it.
    //
    // The strict-list check below applies to every row. At most `OPEN_TOKEN_ROWS_MAX` rows are
    // read.
    if tokens.is_open() {
        // The strict-list programs the rows name, per token. Rows come in token order, rows of
        // listed orders first.
        let mut claimed: Vec<ClaimedPrograms<'_>> = Vec::new();
        for (cov, tpl, _fam) in open_token_rows(conn, OPEN_TOKEN_ROWS_MAX as usize)? {
            let (Ok(cov32), Ok(tpl32)) = (<[u8; 32]>::try_from(cov.as_slice()), <[u8; 32]>::try_from(tpl.as_slice())) else {
                continue;
            };
            if tokens.get(&crate::hex::Hash32(cov32)).is_some() {
                continue;
            }
            // A token whose program is not on the strict list is not listed, whatever listed
            // orders it still has in the history. An example is a retired program whose orders
            // were listed under an earlier registry. A row of another program (an unlisted order
            // naming the covenant id with a program off the list) neither lists the token nor
            // hides it: the token's next row is read. Rows of listed orders come first
            // (`family` > 0).
            let Some(strict) = tokens.strict_template(&crate::hex::Hash32(tpl32)) else {
                continue;
            };
            match claimed.last_mut() {
                Some((c, programs)) if *c == cov => {
                    if !programs.iter().any(|(t, _)| *t == tpl) {
                        programs.push((tpl, strict));
                    }
                }
                _ => claimed.push((cov, vec![(tpl, strict)])),
            }
        }
        for (cov, mut programs) in claimed {
            // The programs the chain proves win. A placement that pins another program for the
            // covenant id is ignored, whatever number of them there are, and the token is not
            // listed under it when it is the only claim. When nothing is proven, the claims stand
            // and several of them stay ambiguous. The client proves the program from a token
            // output.
            let proven = proven_programs(conn, &cov)?;
            if !proven.is_empty() {
                programs.retain(|(t, _)| proven.contains(t));
                if programs.is_empty() {
                    continue;
                }
            }
            let cid = crate::hex::Hash32(<[u8; 32]>::try_from(cov.as_slice()).expect("32-byte covenant id"));
            let (open_asks, open_bids) =
                (counts.get(&(cov.clone(), 1)).copied().unwrap_or(0), counts.get(&(cov.clone(), 2)).copied().unwrap_or(0));
            if let [(tpl, strict)] = programs.as_slice() {
                let extension_commitment =
                    if strict.family == kob_protocol::Family::Kcc20 { sole_extension(conn, &cov, tpl)? } else { None };
                out.push(TokenView {
                    ticker: String::new(),
                    covenant_id: cid.to_hex(),
                    template_hash: Some(hex::encode(tpl)),
                    extension_commitment,
                    family: Some(strict.family.as_str()),
                    decimals: None,
                    decimals_source: None,
                    scale: None,
                    market_scale: market_scale(conn, &cov)?,
                    program_ambiguous: None,
                    candidate_templates: Vec::new(),
                    registry: tokens.info().cloned(),
                    genesis_verified: None,
                    warnings: tokens.warnings_for(&cid),
                    powers: strict.powers.clone(),
                    template_id: Some(strict.id.clone()),
                    open_asks,
                    open_bids,
                    declaration: None,
                });
            } else {
                // Placements of one covenant id name several programs. Anyone can place an order
                // that pins any strict program for any covenant id, and the indexer holds no token
                // output to compare it with. A pick here would let such an order replace the
                // token's real program in this list. So the view names none, and the client
                // proves the program from the chain.
                out.push(TokenView {
                    ticker: String::new(),
                    covenant_id: cid.to_hex(),
                    template_hash: None,
                    extension_commitment: None,
                    family: None,
                    decimals: None,
                    decimals_source: None,
                    scale: None,
                    market_scale: market_scale(conn, &cov)?,
                    program_ambiguous: Some(true),
                    candidate_templates: programs.iter().map(|(t, _)| hex::encode(t)).collect(),
                    registry: tokens.info().cloned(),
                    genesis_verified: None,
                    warnings: tokens.warnings_for(&cid),
                    powers: Vec::new(),
                    template_id: None,
                    open_asks,
                    open_bids,
                    declaration: None,
                });
            }
        }
    }
    // The KCC-23 declarations the indexer holds (one primary-key lookup per row), and so the
    // decimals of each token. The order is its declaration, else its registry entry, else its
    // program's default ([`resolve_decimals`]).
    for v in &mut out {
        if let Ok(id) = crate::hex::Hash32::parse(&v.covenant_id) {
            let declared = super::kcc23::declaration_decimals(conn, &id)?;
            if declared.is_some() {
                v.declaration = Some(true);
            }
            let program = program_default_of(
                v.template_hash.as_deref().and_then(|h| crate::hex::Hash32::parse(h).ok()).as_ref(),
                v.template_id.as_deref(),
            );
            if let Some((d, source)) = resolve_decimals(declared.flatten(), v.decimals, program) {
                (v.decimals, v.decimals_source) = (Some(d), Some(source));
                // Decimals fix the scale, and the executor lists orders at it only. The first
                // order's scale is not a second opinion.
                v.market_scale = None;
            }
        }
    }
    Ok(out)
}

/// The scale every listed order of an open-list token must quote, when its decimals are known
/// without any order.
///
/// An open-list token has no registry entry. Its decimals come from the verified KCC-23
/// declaration of the token, else from the default of the program the order names (a launchpad's
/// convention, profile `default_decimals`). The scale is `10^min(decimals, 9)`. The result is
/// `None` when nothing fixes it, so that the first order sets the market's scale, or when other
/// programs are named for the covenant id too. `order_tpl` is a claim like any other there, and
/// an order pinning the wrong program must not set the convention.
pub fn open_standard_scale(conn: &Connection, token: &Hash32, order_tpl: Option<&Hash32>) -> DbResult<Option<i64>> {
    let declared = super::kcc23::declaration_decimals(conn, token)?.flatten();
    let d = match declared {
        Some(d) => d,
        None => {
            let Some(tpl) = order_tpl else { return Ok(None) };
            // A program the chain proves is the token's, whatever the placements claim. An order
            // pinning another one has no say.
            let proven = proven_programs(conn, &token.0)?;
            if !proven.is_empty() {
                if !proven.contains(&tpl.0.to_vec()) {
                    return Ok(None);
                }
            } else if claims_another_program(conn, &token.0, &tpl.0)? {
                return Ok(None);
            }
            let Some(d) = program_default_of(Some(tpl), None) else { return Ok(None) };
            d
        }
    };
    Ok(Some(10_i64.pow(d.min(9))))
}

/// The decimals of a token and where they come from.
///
/// The order is: the verified KCC-23 declaration of the token, its registry entry, then the
/// default of its program (a launchpad's convention, profile `default_decimals`, for example 3
/// for KaspaRocket). `None`: none of them says, and the price basis is then the scale of the
/// token's first order.
pub fn resolve_decimals(declared: Option<u32>, registry: Option<u32>, program: Option<u32>) -> Option<(u32, &'static str)> {
    declared.map(|d| (d, "declaration")).or(registry.map(|d| (d, "registry"))).or(program.map(|d| (d, "program")))
}

/// The default decimals of the program with this template hash, else registry id.
///
/// `None`: the program has none, or is unknown.
fn program_default_of(hash: Option<&Hash32>, registry_id: Option<&str>) -> Option<u32> {
    let profile = hash
        .and_then(|h| kob_protocol::profile::by_hash(&h.0))
        .or_else(|| registry_id.and_then(kob_protocol::profile::by_registry_id))?;
    profile.default_decimals.as_ref().map(|d| d.decimals)
}

/// The decimals of a token ([`resolve_decimals`]) for the market views.
///
/// The views are trades, candles, stats, depth and pair candles, and their price basis is the
/// standard scale of the token. The program of a token the registry does not name is read from
/// the template hashes its order placements recorded. Two programs for one covenant id name
/// none. `None` falls back to the scale of the first order.
pub fn token_decimals(conn: &Connection, tokens: &TokenAllowlist, token: &Hash32) -> DbResult<Option<u32>> {
    let declared = super::kcc23::declaration_decimals(conn, token)?.flatten();
    let entry = tokens.get(token);
    let registry = entry.and_then(|t| t.decimals);
    if declared.is_some() || registry.is_some() {
        return Ok(resolve_decimals(declared, registry, None).map(|(d, _)| d));
    }
    let from_entry = entry.and_then(|t| program_default_of(t.template_hash.as_ref(), t.template_id.as_deref()));
    let program = match from_entry {
        Some(d) => Some(d),
        None => {
            // The programs the chain proves outrank the claims of placements. They cost two
            // seeks, however many holdings there are.
            let proven = proven_programs(conn, &token.0)?;
            let hashes = if proven.is_empty() { claimed_programs(conn, &token.0, 2)? } else { proven };
            match hashes.as_slice() {
                [one] => <[u8; 32]>::try_from(one.as_slice()).ok().and_then(|h| program_default_of(Some(&Hash32(h)), None)),
                _ => None,
            }
        }
    };
    Ok(resolve_decimals(None, None, program).map(|(d, _)| d))
}

/// The strict-list programs the placements name for one token.
///
/// The type is (covenant id, [(template hash, template)]).
type ClaimedPrograms<'a> = (Vec<u8>, Vec<(Vec<u8>, &'a crate::tokens::StrictTemplate)>);

/// A (token, program, family) row of the open token list.
type OpenTokenRow = (Vec<u8>, Vec<u8>, i64);

/// The rows the open token list reads, at most `max`.
///
/// The rows are every distinct (token, program, family) of the listed orders, and every token
/// identity an order placement recorded (`token_events`, family 0). The quote token B of a listed
/// pair order is named through these identities, so they hold every such row. The order is by
/// token, then family descending (rows of listed orders first), then program. The rows of the
/// listed orders are read one index seek each (`orders_listed_tokens`), never by reading the
/// orders, so the read grows with the tokens and not with the history.
fn open_token_rows(conn: &Connection, max: usize) -> DbResult<Vec<OpenTokenRow>> {
    use std::cmp::Reverse;
    use std::collections::BTreeSet;
    // The scan reads up to four times the rows the list shows. Past `max`, the tokens the chain
    // proves (a holding of the token exists) are kept in preference to the rest, so placements
    // naming covenant ids that sort low cannot push such a token out of the list. The listed
    // orders name their token and program by id, so their scan is in the order the ids were
    // first seen and not in the order of the hashes. Past the cap, the rows kept are the first
    // seen, where the scan of the events keeps the lowest hashes. The rows are put in the order
    // of the hashes below either way.
    let scan_max = max.saturating_mul(4);
    // In the list's order: (token, family descending, program). It is a set, so a row named twice
    // is one row.
    let mut rows: BTreeSet<(Vec<u8>, Reverse<i64>, Vec<u8>)> = BTreeSet::new();
    // The distinct (token, program) pairs of the events and (token, program, family) of the
    // listed orders, in index order, by loose index scans. From a row, the next distinct one is
    // found by up to three range seeks: the next family of the same program, the next program of
    // the same token, the next token. It never steps over the duplicates, because a token has as
    // many events as fresh commitments and as many orders as the market placed. A row-value `>`
    // seek steps over all of them. Measured: `docs/ops/executor.md`, 5.5.
    let mut ev_first = conn.prepare_cached(
        "SELECT token_cov_id, tpl_hash FROM token_events WHERE tpl_hash IS NOT NULL ORDER BY token_cov_id, tpl_hash LIMIT 1",
    )?;
    let mut ev_prog =
        conn.prepare_cached("SELECT tpl_hash FROM token_events WHERE token_cov_id = ?1 AND tpl_hash > ?2 ORDER BY tpl_hash LIMIT 1")?;
    let mut ev_tok = conn.prepare_cached(
        "SELECT token_cov_id, tpl_hash FROM token_events WHERE token_cov_id > ?1 AND tpl_hash IS NOT NULL ORDER BY token_cov_id, tpl_hash LIMIT 1",
    )?;
    let ev_row = |r: &Row<'_>| Ok((r.get::<_, Vec<u8>>(0)?, r.get::<_, Vec<u8>>(1)?));
    let mut ev_cur = ev_first.query_row([], ev_row).optional()?;
    while let Some((t, p)) = ev_cur {
        ev_cur = match ev_prog.query_row(rusqlite::params![&t, &p], |r| r.get::<_, Vec<u8>>(0)).optional()? {
            Some(next) => Some((t.clone(), next)),
            None => ev_tok.query_row(rusqlite::params![&t], ev_row).optional()?,
        };
        rows.insert((t, Reverse(0), p));
        if rows.len() >= scan_max {
            break;
        }
    }
    // The orders name their token and program by id, so the scan is in the order of the ids. The
    // rows are put in the order of the hashes in the set below.
    const LISTED: &str =
        "FROM orders INDEXED BY orders_listed_tokens WHERE listed = 1 AND token_id IS NOT NULL AND token_tpl_id IS NOT NULL";
    let row = |r: &Row<'_>| Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?, r.get::<_, i64>(2)?));
    let mut first = conn
        .prepare_cached(&format!("SELECT token_id, token_tpl_id, family {LISTED} ORDER BY token_id, token_tpl_id, family LIMIT 1"))?;
    let mut fam = conn.prepare_cached(&format!(
        "SELECT family {LISTED} AND token_id = ?1 AND token_tpl_id = ?2 AND family > ?3 ORDER BY family LIMIT 1"
    ))?;
    let mut prog = conn.prepare_cached(&format!(
        "SELECT token_tpl_id, family {LISTED} AND token_id = ?1 AND token_tpl_id > ?2 ORDER BY token_tpl_id, family LIMIT 1"
    ))?;
    let mut tok = conn.prepare_cached(&format!(
        "SELECT token_id, token_tpl_id, family {LISTED} AND token_id > ?1 ORDER BY token_id, token_tpl_id, family LIMIT 1"
    ))?;
    let mut cur: Option<(i64, i64, i64)> = first.query_row([], row).optional()?;
    let mut last_token: Option<i64> = None;
    // The budget of the listed orders is its own. Rows of the events must not spend it.
    let mut listed_rows = 0usize;
    while let Some((t, p, f)) = cur {
        if last_token.as_ref() != Some(&t) {
            if listed_rows >= scan_max {
                break;
            }
            last_token = Some(t);
        }
        listed_rows += 1;
        rows.insert((crate::indexer::ids::hash_of(conn, t)?, Reverse(f), crate::indexer::ids::hash_of(conn, p)?));
        cur = if let Some(nf) = fam.query_row(rusqlite::params![t, p, f], |r| r.get::<_, i64>(0)).optional()? {
            Some((t, p, nf))
        } else if let Some((np, nf)) =
            prog.query_row(rusqlite::params![t, p], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?))).optional()?
        {
            Some((t, np, nf))
        } else {
            tok.query_row(rusqlite::params![t], row).optional()?
        };
    }
    if rows.len() > max {
        // Over the cap: the rows of tokens with a holding first, then the others in the list's
        // order. `holding_programs` records a token that ever had a holding, whether or not the
        // pruning has deleted it since.
        let mut held = conn.prepare_cached(concat!("SELECT 1 FROM holding_programs WHERE token_id = ", id_of!("?1"), " LIMIT 1"))?;
        let mut proven: std::collections::HashMap<Vec<u8>, bool> = std::collections::HashMap::new();
        let mut keep: Vec<(Vec<u8>, Reverse<i64>, Vec<u8>)> = Vec::with_capacity(max);
        let mut rest: Vec<(Vec<u8>, Reverse<i64>, Vec<u8>)> = Vec::new();
        for r in std::mem::take(&mut rows) {
            let p = match proven.get(&r.0) {
                Some(p) => *p,
                None => {
                    let p = held.exists([&r.0])?;
                    proven.insert(r.0.clone(), p);
                    p
                }
            };
            if p && keep.len() < max {
                keep.push(r);
            } else {
                rest.push(r);
            }
        }
        let room = max - keep.len();
        keep.extend(rest.into_iter().take(room));
        keep.sort();
        return Ok(keep.into_iter().map(|(t, f, p)| (t, p, f.0)).collect());
    }
    Ok(rows.into_iter().map(|(t, f, p)| (t, p, f.0)).collect())
}

// ---------------------------------------------------------------------------------------------
// Books

/// An aggregated price level of one side of a book.
#[derive(Debug, Clone, Serialize)]
pub struct LevelView {
    /// Sompi per whole token (`scale` base units): the quote of the orders at the evaluation DAA
    /// score.
    ///
    /// The quote is the current price of an auction, else the limit ([`resting_level`]).
    pub price: String,
    /// Base units at this level.
    ///
    /// It is the sum of the remaining amounts of the orders. A bid's amount is its buying power. A
    /// TWAP order counts one slice, at most `maxFill`.
    pub amount: String,
    /// `true` when any order at this level has no exact remaining amount.
    ///
    /// The buying power of a bid is then an upper bound.
    pub amount_estimated: bool,
    /// Number of orders at this level.
    pub orders: i64,
    /// Base units per whole token of the orders at this level.
    ///
    /// Levels are per (`price`, `scale`). The listed orders of a token with `decimals` all have its
    /// standard scale. Orders of another scale (a token without `decimals`) quote another whole
    /// token, so their `price` is not comparable as such. Levels are ordered by the price per base
    /// unit (`price / scale`).
    pub scale: i64,
}

/// An order of the per-order book of one side, with its quote and its expiry state.
#[derive(Debug, Clone, Serialize)]
pub struct BookOrderView {
    pub covenant_id: String,
    pub contract: String,
    pub maker: Option<String>,
    /// The limit (start price of an auction).
    pub price: String,
    /// Quote at the node's current DAA score. It equals `price` unless the order is an auction.
    pub quote: Option<String>,
    pub auction: Option<AuctionView>,
    /// Priority tip, sompi per whole token.
    pub tip: Option<String>,
    /// Smallest fill in base units, unless the fill takes everything left.
    pub min_fill: Option<String>,
    /// Base units per whole token (the price denominator).
    pub scale: Option<i64>,
    /// Base units left. For a bid it is its buying power, an upper bound.
    pub amount_left: Option<String>,
    pub amount_estimated: bool,
    pub status: String,
    pub cur_value: Option<String>,
    pub expiry_daa: Option<i64>,
    pub expired: bool,
    /// Deadline of a day order, in UTC unix seconds. Conforming matchers do not fill after it.
    pub deadline: Option<i64>,
    pub deadline_passed: bool,
    pub genesis_daa: i64,
    pub confirmations: Option<i64>,
    pub settled: bool,
}

/// One side of a book, as aggregated levels or as single orders.
#[derive(Debug, Clone, Serialize)]
#[serde(untagged)]
pub enum BookSide {
    Levels(Vec<LevelView>),
    Orders(Vec<BookOrderView>),
}

/// The listed book of one token.
#[derive(Debug, Clone, Serialize)]
pub struct BookView {
    pub token: String,
    pub aggregated: bool,
    pub node_daa: Option<u64>,
    /// Ascending by price per base unit (`price / scale`).
    pub asks: BookSide,
    /// Descending by price per base unit.
    pub bids: BookSide,
}

/// SQL predicate (aliases `o` = orders, `s` = order_state): the order has the custody it needs.
///
/// An ask-side order is liquidity only while exactly one live custody UTXO holds `amountLeft`
/// base units (`docs/spec/matcher.md` 1.2). Bid-side orders hold KAS and need no custody.
pub(crate) const CUSTODY_OK: &str = "(o.side = 2 OR (\
     (SELECT COUNT(*) FROM token_utxos t WHERE t.order_id = o.id AND t.role = 'custody' AND t.spent_block IS NULL) = 1 \
     AND (SELECT t.amount FROM token_utxos t WHERE t.order_id = o.id AND t.role = 'custody' AND t.spent_block IS NULL) \
         = s.remaining_amount))";

/// SQL order key of a book: the price per base unit (`price / scale`).
///
/// It is a REAL, for display order. The aggregated levels compare exactly
/// ([`book_side_levels`]).
const PER_BASE_UNIT: &str = "(o.price * 1.0 / MAX(COALESCE(o.scale, 1), 1))";

/// Orders flagged possibly frozen are not liquidity.
///
/// The flag is set when the engine pre-simulation failed at the token program
/// (`indexer::flags`). The same predicate is in every book, count and stats query (aliases
/// `o` = orders, `s` = order_state).
macro_rules! not_frozen {
    () => {
        "NOT EXISTS (SELECT 1 FROM order_flags f WHERE f.covenant_id = o.covenant_id AND f.txid = s.cur_txid AND f.idx = s.cur_idx)"
    };
}

/// [`not_frozen!`] for other read modules.
pub(crate) const NOT_FROZEN: &str = not_frozen!();

/// Something left to trade.
///
/// An order whose amount left is 0 is no liquidity. For a bid, the amount left is the amount its
/// escrow funds. An example is a sold-out repeating if-done entry that lives on for the re-arms of
/// its exits. An unknown amount is kept.
pub(crate) const HAS_AMOUNT: &str = "COALESCE(s.remaining_amount, 1) > 0";

/// The FROM of every book, count and stats read: the live set first.
///
/// The aliases are `s` = order_state and `o` = orders. The WHERE of those reads keeps
/// `s.status IN ('open','partial')`, so the rows come from `order_state_status` (the few live
/// orders) and are joined to their orders. `CROSS JOIN` keeps that order, because SQLite does not
/// reorder it. Joined the other way, the planner reads every order of the token (`orders_book`)
/// or of the whole history to find them. Measured costs: `docs/ops/executor.md`, 5.5.
pub(crate) const LIVE_FROM: &str = "FROM order_state s CROSS JOIN orders o ON o.id = s.order_id";

/// SQL predicate (alias `o`): the order's template is one this build pins.
///
/// The pinned templates are [`kob_protocol::artifacts::PINNED`]. An order under any other
/// template is unknown to this build (`docs/spec/template-retirement.md`). The indexer never
/// lists it, and never offers it to a matcher or a keeper. Indexing admits only pinned templates,
/// so the rows this leaves out are those that a database carried over a template change holds,
/// indexed by an older build. Every read that lists or counts orders carries the predicate:
/// - The books and their counts, the stats, and the pair books and routes.
/// - `/v1/orders` and `/v1/orders/{id}`.
/// - The live orders of the matcher and the keepers (`book::live_orders_sql`).
///
/// No index covers `template_id`. The predicate filters the rows the read selects anyway, and the
/// ids of the pinned templates are resolved once per statement.
pub(crate) fn pinned_template() -> &'static str {
    static SQL: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    SQL.get_or_init(|| {
        let hashes: Vec<String> = kob_protocol::artifacts::PINNED.iter().map(|(_, h)| format!("x'{h}'")).collect();
        format!("o.template_id IN (SELECT id FROM ids WHERE h IN ({}))", hashes.join(","))
    })
}

/// [`pinned_template`] or a retired template of the cancel-only table (`kob_protocol::retired`).
///
/// Only the single-order view ([`order`]) uses it. It serves such an order (`cancel_only`) so that
/// its maker can find and cancel it. No list, book, count or plan uses it.
pub(crate) fn pinned_or_cancel_only_template() -> &'static str {
    static SQL: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    SQL.get_or_init(|| {
        let hashes: Vec<String> = kob_protocol::artifacts::PINNED
            .iter()
            .map(|(_, h)| format!("x'{h}'"))
            .chain(kob_protocol::retired::retired().iter().map(|r| format!("x'{}'", r.hash_hex())))
            .collect();
        format!("o.template_id IN (SELECT id FROM ids WHERE h IN ({}))", hashes.join(","))
    })
}

/// WHERE of a book: the listed, live, in-book, priced orders of one token (`?1`) and side (`?2`).
const BOOK_WHERE: &str = concat!(
    "o.token_id = (SELECT id FROM ids WHERE h = ?1) AND o.side = ?2 AND o.listed = 1 AND o.in_book = 1 \
     AND s.status IN ('open','partial') AND s.state_known = 1 AND o.price IS NOT NULL AND COALESCE(s.remaining_amount, 1) > 0 AND ",
    not_frozen!()
);

/// The DAA score a book is evaluated at: the node's, else the store's cursor.
pub(crate) fn book_daa(conn: &Connection, ctx: &ReadCtx) -> DbResult<i64> {
    Ok(match ctx.node_daa {
        Some(n) => n,
        None => super::book::cursor_daa(conn)?,
    }
    .min(i64::MAX as u64) as i64)
}

/// SQL predicate of an order a conforming matcher may fill at DAA `now`.
///
/// See `docs/spec/matcher.md` §2.2 and §2.3. The aliases are `o` (orders), `s` (order_state) and
/// `u` (the current order UTXO, LEFT JOINed). An order qualifies when it is:
/// - Active (`activeFrom <= now`).
/// - Not due for a refund (`AnyState::refund_due`): the soft expiry, 90 days idle since the UTXO,
///   and for a plain ask or bid the IOC / FOK kill time `max(UTXO DAA, activeFrom) + 600`.
/// - With a wall clock, not past its day-order deadline.
///
/// The KAS books (`/v1/books`, the best bid and ask of the stats) list only these orders, as the
/// pair books do from the decoded state (`pairs::eligible`) and the matcher
/// (`matcher::candidate::excluded_by_expiry`). An order outside the predicate waits for a refund
/// keeper (or its activation), and `/v1/orders` shows it flagged `expired`.
pub(crate) fn fillable_now(now: i64, now_unix: Option<u64>) -> String {
    use kob_protocol::state::{IOC_LIFE, MAX_IDLE, TIF_GTC};
    // The current UTXO's DAA, or the order state's last DAA when the UTXO row is not stored.
    let udaa = "COALESCE(u.created_daa, s.last_daa)";
    let mut w = format!(
        "(o.expiry_daa IS NULL OR o.expiry_daa > {now}) AND COALESCE(o.active_from, 0) <= {now} AND {udaa} + {MAX_IDLE} > {now} \
         AND (COALESCE(o.tif, {TIF_GTC}) = {TIF_GTC} OR o.contract NOT IN ('KobAsk','KobAskKron','KobBid','KobBidKron') \
              OR MAX({udaa}, COALESCE(o.active_from, 0)) + {IOC_LIFE} > {now})"
    );
    if let Some(unix) = now_unix {
        w.push_str(&format!(" AND (o.deadline IS NULL OR o.deadline > {})", unix.min(i64::MAX as u64)));
    }
    w
}

/// SQL predicate (alias `o`): not an IOC / FOK order.
///
/// `tif` 1 or 2 is a market, IOC or FOK taker in flight, filled against the book or killed. The
/// KAS books list resting orders only, as the pair books do (see the `pairs` module doc).
pub(crate) const RESTING_TIF: &str = "COALESCE(o.tif, 0) = 0";

/// What a live order of the KAS book rests with at DAA `now`: `(quote, amount)`.
///
/// The pair books read the KAS orders the same way (`pairs::kas_orders`). The quote is the
/// current price: an auction's (a decaying ask, a rising bid, an armed stop entry,
/// [`auction_of`]), else the limit `price`. The amount is `remaining`, and a TWAP order's is one
/// slice, at most `maxFill`. The result is `None` for an order that is not resting liquidity
/// now: an IOC / FOK taker, or a TWAP order whose next slice is not due, because its UTXO,
/// created at `udaa`, is not yet `interval` DAA old (the covenant's CSV). An order whose tip
/// state is unknown rests at its limit.
pub(crate) fn resting_level(
    contract: &str,
    state: Option<Vec<u8>>,
    udaa: i64,
    price: i64,
    remaining: Option<i64>,
    now: i64,
) -> Option<(i64, Option<i64>)> {
    use kob_protocol::state::TIF_GTC;
    let Some(s) = tip_state(contract, state) else { return Some((price, remaining)) };
    let resting = |tif: i64, interval: i64| tif == TIF_GTC && (interval <= 0 || now >= udaa.saturating_add(interval));
    let slice = |max_fill: i64| if max_fill > 0 { remaining.map(|r| r.min(max_fill)) } else { remaining };
    let amount = match &s {
        AnyState::KobAsk(a) | AnyState::KobAskKron(a) => resting(a.tif, a.interval).then(|| slice(a.max_fill))?,
        AnyState::KobBid(b) | AnyState::KobBidKron(b) => resting(b.tif, b.interval).then(|| slice(b.max_fill))?,
        _ => remaining,
    };
    let quote = auction_of(&s, udaa, Some(now.max(0) as u64)).and_then(|a| a.current_price.parse::<i64>().ok()).unwrap_or(price);
    Some((quote, amount))
}

/// The orders of one side of the book of a token, best first, at most `depth`.
fn book_side_orders(conn: &Connection, ctx: &ReadCtx, token: &Hash32, side: i64, depth: usize) -> DbResult<Vec<BookOrderView>> {
    let dir = if side == 1 { "ASC" } else { "DESC" };
    let now = book_daa(conn, ctx)?;
    let live = fillable_now(now, ctx.now_unix);
    let pinned = pinned_template();
    let sql = format!(
        "SELECT o.covenant_id, o.contract, o_mk.h, o.price, o.tip, o.min_fill, o.scale, o.expiry_daa, o.genesis_daa, \
                s.amount_exact, s.status, s.remaining_amount, s.cur_value, o.deadline, kob_ustate(u.state, {GENESIS_O}), u.created_daa, s.last_daa \
         {LIVE_FROM} \
         LEFT JOIN order_utxos u ON u.txid = s.cur_txid AND u.idx = s.cur_idx{ORDER_IDS_O} \
         WHERE {BOOK_WHERE} AND {CUSTODY_OK} AND {live} AND {pinned} AND {RESTING_TIF} \
         ORDER BY {PER_BASE_UNIT} {dir}, o.genesis_block ASC, o.covenant_id ASC"
    );
    let mut st = conn.prepare(&sql)?;
    let rows = st.query_map(rusqlite::params![token.0.to_vec(), side], |r| {
        let expiry: Option<i64> = r.get(7)?;
        let genesis_daa: i64 = r.get(8)?;
        let exact: i64 = r.get(9)?;
        let status: String = r.get(10)?;
        let remaining: Option<i64> = r.get(11)?;
        let contract: String = r.get(1)?;
        let price: i64 = r.get(3)?;
        let deadline: Option<i64> = r.get(13)?;
        let raw_state: Option<Vec<u8>> = r.get(14)?;
        let utxo_daa: Option<i64> = r.get(15)?;
        let last_daa: i64 = r.get(16)?;
        // A taker in flight or a TWAP order between its slices is not in the book. The aggregated
        // levels leave it out the same way.
        let Some((quote, _)) = resting_level(&contract, raw_state.clone(), utxo_daa.unwrap_or(last_daa), price, remaining, now) else {
            return Ok(None);
        };
        let scale: Option<i64> = r.get(6)?;
        let state = tip_state(&contract, raw_state);
        let auction = state.as_ref().zip(utxo_daa).and_then(|(s, d)| auction_of(s, d, ctx.node_daa));
        let view = BookOrderView {
            covenant_id: hx(r.get(0)?),
            contract,
            maker: hxo(r.get(2)?),
            price: price.to_string(),
            quote: Some(auction.as_ref().map(|a| a.current_price.clone()).unwrap_or_else(|| price.to_string())),
            auction,
            tip: sompi(r.get(4)?),
            min_fill: sompi(r.get(5)?),
            scale,
            amount_left: sompi(remaining),
            amount_estimated: exact == 0 || remaining.is_none(),
            expired: expiry.is_some_and(|e| ctx.node_daa.is_some_and(|n| n as i64 >= e)),
            expiry_daa: expiry,
            status,
            cur_value: sompi(r.get(12)?),
            deadline_passed: deadline_passed(deadline, ctx),
            deadline,
            genesis_daa,
            confirmations: ctx.confirmations(genesis_daa),
            settled: ctx.settled(genesis_daa),
        };
        Ok(Some((quote, scale.unwrap_or(1).max(1), view)))
    })?;
    let mut out: Vec<(i64, i64, BookOrderView)> = rows.filter_map(Result::transpose).collect::<rusqlite::Result<Vec<_>>>()?;
    // Best first by the quote now per base unit (an auction moves away from its limit). The SQL
    // order (age, covenant id) breaks ties.
    out.sort_by(|(qa, sa, _), (qb, sb, _)| {
        let o = (*qa as i128 * *sb as i128).cmp(&(*qb as i128 * *sa as i128));
        if side == 1 {
            o
        } else {
            o.reverse()
        }
    });
    Ok(out.into_iter().take(depth).map(|(_, _, v)| v).collect())
}

/// One aggregated level before it is rendered: `((price, scale), (amount, orders, estimated))`.
pub(crate) type RawBookLevel = ((i64, i64), (i128, i64, bool));

/// Aggregated levels of one side of a book, as numbers.
///
/// The levels are per (`price`, `scale`). The amounts are summed in 128 bits, because base units
/// of hostile orders can sum past an `i64`, where SQLite's `SUM` fails. The order is exact by the
/// price per base unit (`price / scale`, compared by cross-multiplication): asks ascending, bids
/// descending, and at one price per base unit by `price`, then `scale`, in the same direction.
///
/// Every live listed order of the side that a matcher may fill now ([`fillable_now`]) and that
/// rests there is read (the book of one token). It counts at its quote now for its resting
/// amount ([`resting_level`]): no IOC / FOK taker, a TWAP order once its slice is due and for
/// that slice, an auction at its current price. [`book_side_levels`] renders the first `depth`
/// levels. The 24 h statistics read the ones near the touch without a string per level. Either
/// way it is one scan of the live orders of the token.
pub(crate) fn book_side_sorted(conn: &Connection, ctx: &ReadCtx, token: &Hash32, side: i64) -> DbResult<Vec<RawBookLevel>> {
    let now = book_daa(conn, ctx)?;
    let live = fillable_now(now, ctx.now_unix);
    let pinned = pinned_template();
    let sql = format!(
        "SELECT o.price, COALESCE(o.scale, 1), s.remaining_amount, s.amount_exact, o.contract, kob_ustate(u.state, {GENESIS_O}), \
                COALESCE(u.created_daa, s.last_daa) \
         {LIVE_FROM} \
         LEFT JOIN order_utxos u ON u.txid = s.cur_txid AND u.idx = s.cur_idx{ORDER_IDS_O} \
         WHERE {BOOK_WHERE} AND {CUSTODY_OK} AND {live} AND {pinned} AND {RESTING_TIF}"
    );
    let mut st = conn.prepare(&sql)?;
    let rows = st.query_map(rusqlite::params![token.0.to_vec(), side], |r| {
        let (price, remaining): (i64, Option<i64>) = (r.get(0)?, r.get(2)?);
        let level = resting_level(&r.get::<_, String>(4)?, r.get(5)?, r.get(6)?, price, remaining, now);
        Ok((level, r.get::<_, i64>(1)?, r.get::<_, i64>(3)?))
    })?;
    // Maps (price, scale) to (amount, orders, estimated).
    let mut levels: BTreeMap<(i64, i64), (i128, i64, bool)> = BTreeMap::new();
    for r in rows {
        let (level, scale, exact) = r?;
        let Some((price, remaining)) = level else { continue };
        let l = levels.entry((price, scale.max(1))).or_insert((0, 0, false));
        l.0 += remaining.unwrap_or(0).max(0) as i128;
        l.1 += 1;
        l.2 |= remaining.is_none() || exact == 0;
    }
    let mut out: Vec<_> = levels.into_iter().collect();
    out.sort_by(|((pa, sa), _), ((pb, sb), _)| {
        let o = (*pa as i128 * *sb as i128).cmp(&(*pb as i128 * *sa as i128)).then(pa.cmp(pb)).then(sa.cmp(sb));
        if side == 1 {
            o
        } else {
            o.reverse()
        }
    });
    Ok(out)
}

fn book_side_levels(conn: &Connection, ctx: &ReadCtx, token: &Hash32, side: i64, depth: usize) -> DbResult<Vec<LevelView>> {
    Ok(book_side_sorted(conn, ctx, token, side)?
        .into_iter()
        .take(depth)
        .map(|((price, scale), (amount, orders, estimated))| LevelView {
            price: price.to_string(),
            amount: amount.to_string(),
            amount_estimated: estimated,
            orders,
            scale,
        })
        .collect())
}

fn deadline_passed(deadline: Option<i64>, ctx: &ReadCtx) -> bool {
    matches!((deadline, ctx.now_unix), (Some(d), Some(n)) if n as i64 >= d)
}

/// The decoded tip state of an order UTXO, or `None` when unknown or undecodable.
pub fn tip_state(contract: &str, state: Option<Vec<u8>>) -> Option<AnyState> {
    let id = TemplateId::from_name(contract)?;
    AnyState::decode(id, &state?).ok()
}

/// An auction in progress: a decaying ask, a rising bid, an armed stop leg or an armed stop entry.
#[derive(Debug, Clone, Serialize)]
pub struct AuctionView {
    /// `decay` (ask), `rise` (bid), `stop` (armed stop leg) or `entry` (armed stop entry).
    ///
    /// A pair order's auctions have the same kinds with prices in token B base units per whole A
    /// (`pair_decay`, `pair_rise`, `pair_stop`, `pair_entry`).
    pub kind: &'static str,
    /// DAA score the price path starts from.
    pub origin_daa: i64,
    pub start_price: String,
    /// The bound the price path ends at.
    pub end_price: String,
    /// The quote at the node's current DAA score (the start price until `origin_daa`).
    pub current_price: String,
    pub elapsed_daa: i64,
    /// The path has reached its end price.
    pub complete: bool,
}

/// Auction path of a tip state at the node's DAA score.
///
/// All arithmetic is guarded, because the values come from chain content.
pub fn auction_of(state: &AnyState, utxo_daa: i64, node_daa: Option<u64>) -> Option<AuctionView> {
    let now = node_daa.map(|n| n as i64);
    crate::indexer::processor::guarded(|| {
        let view = |kind: &'static str, origin: i64, start: i64, end: i64, cur: &dyn Fn(i64) -> i64| {
            let t = now.map(|n| n.max(origin)).unwrap_or(origin);
            let current = cur(t);
            AuctionView {
                kind,
                origin_daa: origin,
                start_price: start.to_string(),
                end_price: end.to_string(),
                current_price: current.to_string(),
                elapsed_daa: t - origin,
                complete: current == end,
            }
        };
        match state {
            AnyState::KobAsk(a) | AnyState::KobAskKron(a) if a.slope > 0 && a.decay_step > 0 => {
                Some(view("decay", a.origin(utxo_daa)?, a.price, a.price_end, &|t| a.price_at(t, utxo_daa).unwrap_or(a.price_end)))
            }
            AnyState::KobBid(b) | AnyState::KobBidKron(b) if b.slope > 0 && b.decay_step > 0 => {
                Some(view("rise", b.origin(utxo_daa)?, b.price, b.price_end, &|t| b.price_at(t, utxo_daa).unwrap_or(b.price_end)))
            }
            AnyState::KobCondAsk(c) | AnyState::KobCondAskKron(c) if c.armed != 0 && c.band_daa > 0 => {
                let o = armed_origin(c.armed, utxo_daa)?;
                Some(view("stop", o, c.stop_price, c.stop_floor(), &|t| c.stop_at(false, t, utxo_daa).unwrap_or(c.stop_floor())))
            }
            AnyState::KobCondBid(c) | AnyState::KobCondBidKron(c) if c.armed != 0 && c.band_daa > 0 => {
                let o = armed_origin(c.armed, utxo_daa)?;
                Some(view("stop", o, c.stop_price, c.stop_ceiling(), &|t| c.stop_at(false, t, utxo_daa).unwrap_or(c.stop_ceiling())))
            }
            AnyState::KobIfdBid(b) | AnyState::KobIfdBidKron(b) if b.entry_stop > 0 && b.armed != 0 && b.band_daa > 0 => {
                let o = armed_origin(b.armed, utxo_daa)?;
                Some(view("entry", o, b.entry_stop, b.price, &|t| b.price_at(false, t, utxo_daa).unwrap_or(b.price)))
            }
            AnyState::KobIfdAsk(a) | AnyState::KobIfdAskKron(a) if a.entry_stop > 0 && a.armed != 0 && a.band_daa > 0 => {
                let o = armed_origin(a.armed, utxo_daa)?;
                Some(view("entry", o, a.entry_stop, a.price, &|t| a.price_at(false, t, utxo_daa).unwrap_or(a.price)))
            }
            // The pair kinds: prices in token B base units per whole A.
            AnyState::KobPair(p) if p.slope > 0 && p.decay_step > 0 => {
                let kind = if p.is_ask() { "pair_decay" } else { "pair_rise" };
                Some(view(kind, p.origin(utxo_daa)?, p.price, p.price_end, &|t| p.price_at(t, utxo_daa).unwrap_or(p.price_end)))
            }
            AnyState::KobCondPair(c) if c.armed != 0 && c.band_daa > 0 => {
                let o = armed_origin(c.armed, utxo_daa)?;
                Some(view("pair_stop", o, c.stop_price, c.stop_worst(), &|t| c.stop_at(false, t, utxo_daa).unwrap_or(c.stop_worst())))
            }
            AnyState::KobIfdPair(e) if e.entry_stop > 0 && e.armed != 0 && e.band_daa > 0 => {
                let o = armed_origin(e.armed, utxo_daa)?;
                Some(view("pair_entry", o, e.entry_stop, e.price, &|t| e.price_at(false, t, utxo_daa).unwrap_or(e.price)))
            }
            _ => None,
        }
    })
    .flatten()
}

/// The listed book of one token: asks ascending, bids descending (ties by age, then covenant id).
pub fn book(conn: &Connection, ctx: &ReadCtx, token: &Hash32, depth: usize, aggregate: bool) -> DbResult<BookView> {
    let (asks, bids) = if aggregate {
        (
            BookSide::Levels(book_side_levels(conn, ctx, token, 1, depth)?),
            BookSide::Levels(book_side_levels(conn, ctx, token, 2, depth)?),
        )
    } else {
        (
            BookSide::Orders(book_side_orders(conn, ctx, token, 1, depth)?),
            BookSide::Orders(book_side_orders(conn, ctx, token, 2, depth)?),
        )
    };
    Ok(BookView { token: token.to_hex(), aggregated: aggregate, node_daa: ctx.node_daa, asks, bids })
}

// ---------------------------------------------------------------------------------------------
// Orders

/// Where an order was created: its transaction, output, block and confirmations.
#[derive(Debug, Clone, Serialize)]
pub struct GenesisView {
    pub txid: Option<String>,
    pub out: Option<i64>,
    pub block_seq: i64,
    pub daa: i64,
    pub confirmations: Option<i64>,
    pub settled: bool,
}

/// An outpoint with its value in sompi.
#[derive(Debug, Clone, Serialize)]
pub struct OutpointView {
    pub txid: String,
    pub index: i64,
    pub value: Option<String>,
}

/// An order as `GET /v1/orders` and `GET /v1/orders/{id}` serve it.
#[derive(Debug, Clone, Serialize)]
pub struct OrderView {
    pub covenant_id: String,
    pub contract: String,
    pub template_hash: String,
    pub family: i64,
    /// 1 sells the token, 2 buys the token.
    pub side: i64,
    pub maker: Option<String>,
    pub token: Option<String>,
    pub token_template_hash: Option<String>,
    /// KCC-20 extension commitment of the token.
    ///
    /// For the bid kinds it comes from the state, for the ask kinds from the placement custody.
    pub extension_commitment: Option<String>,
    /// Base units per whole token, the price denominator.
    ///
    /// For a pair order it is the scale of its base token A.
    pub scale: Option<i64>,
    /// Smallest fill in base units, unless the fill takes everything left.
    ///
    /// For a bid, unless it ends the bid.
    pub min_fill: Option<String>,
    /// The limit, in sompi per whole token.
    ///
    /// It is `null` for the conditional kinds and the pair orders (see `pair.price`).
    pub price: Option<String>,
    /// Priority tip, in sompi per whole token.
    ///
    /// For a pair order it is KAS per whole A, released to the filler.
    pub tip: Option<String>,
    pub tif: Option<i64>,
    pub expiry_daa: Option<i64>,
    pub active_from: Option<i64>,
    pub in_book: bool,
    /// Bids: the rate a fill consumes the escrow at, `pMax + tip` sompi per whole token.
    ///
    /// For buy-first entries it is `price + tip`.
    pub budget_rate: Option<String>,
    pub reserve: Option<String>,
    /// Base units at creation (`amountLeft`).
    ///
    /// It is `null` for a bid, whose quantity is its escrow.
    pub initial_amount: Option<String>,
    pub listed: bool,
    pub unlisted_reason: Option<String>,
    pub origin: String,
    pub parent: Option<String>,
    pub genesis: GenesisView,
    pub status: String,
    /// Base units filled so far.
    pub filled_amount: String,
    /// Base units left: `amountLeft` of the current state.
    ///
    /// For a bid it is the buying power, the most its escrow buys in one fill.
    pub amount_left: Option<String>,
    pub amount_estimated: bool,
    pub current: Option<OutpointView>,
    pub state_known: bool,
    /// The decoded state of the current UTXO, when proven.
    ///
    /// It is `{"kind", "state"}`, with 64-bit fields as strings.
    pub state: Option<Value>,
    /// The placement state of an order without a current state, decoded like `state`.
    ///
    /// An order without a current state is closed or not proven. The placement state lets a client
    /// still show the type and terms of the order. It is never the basis of a spend.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub placed_state: Option<Value>,
    /// DAA score of the block that created the current UTXO.
    pub current_daa: Option<i64>,
    /// Deadline of a day order, in UTC unix seconds, from the placement record.
    ///
    /// Conforming matchers do not fill after it.
    pub deadline: Option<i64>,
    pub deadline_passed: bool,
    /// DAA score from which anyone may refund.
    ///
    /// It follows from the soft expiry, 90 days idle, and the IOC / FOK kill time.
    pub refund_due_daa: Option<i64>,
    /// The kill time of an IOC / FOK order: `max(UTXO DAA, activeFrom) + 600`, or the expiry if
    /// earlier.
    pub kill_daa: Option<i64>,
    /// Auction in progress (decay, rise, armed stop leg or stop entry): start and current price.
    pub auction: Option<AuctionView>,
    /// Quote now: the auction price, else the limit.
    pub quote: Option<String>,
    /// Repeat IFD / IFO: the entry role or the booked exit role.
    ///
    /// The entry role carries `rpt_amount`, the amount left to re-arm.
    pub repeat: Option<RepeatView>,
    /// Exact custody of the ask kinds: the token UTXO holding `amountLeft` base units.
    ///
    /// It is set on single-order lookups and on the live orders of a `maker=` list.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub custody: Option<CustodyView>,
    /// Stray token UTXOs owned by the order id. They are never liquidity.
    ///
    /// It is set on single-order lookups and on the live orders of a `maker=` list.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub strays: Option<Vec<TokenUtxoView>>,
    /// Number of live strays, when `strays` holds only the first [`MAX_VIEW_STRAYS`] of them.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub strays_total: Option<i64>,
    pub last_block: i64,
    pub last_daa: i64,
    pub confirmations: Option<i64>,
    pub settled: bool,
    /// The order is open or partially filled past `expiry_daa`, so a refund is pending.
    pub expired: bool,
    /// The pre-simulation of this order's next fill was rejected by the token program.
    ///
    /// A frozen or blacklisted balance, or a program that changed its rules, can cause that. The
    /// order is not listed as live liquidity. Cancel or refund still work if the program lets the
    /// covenant move the tokens. The flag clears when a later pre-simulation passes or the order
    /// moves.
    pub possibly_frozen: bool,
    /// What `possibly_frozen` rests on: this operator's own engine pre-simulation.
    ///
    /// It is a judgement of the matcher of the operator, not chain data, and another operator may
    /// see none. `null` when the order is not flagged.
    pub possibly_frozen_basis: Option<&'static str>,
    /// Why the order is flagged (`possibly_frozen`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub frozen_reason: Option<String>,
    /// Covenant ids of orders created by this order (IFD exits).
    ///
    /// It is set only on single-order lookups.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub children: Option<Vec<String>>,
    /// Number of child orders, when `children` holds only the newest [`MAX_VIEW_CHILDREN`] of
    /// them.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub children_total: Option<i64>,
    /// Pair orders (`KobPair`, `KobCondPair`, `KobIfdPair`): the pair, the side, the price in token
    /// B and the custodies.
    ///
    /// `token` is the base token A. `price` and `quote` are null, because there is no KAS price.
    /// `tip` is the KAS tip per whole A.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pair: Option<PairView>,
    /// Warnings a client must show for the tokens of the order, derived from the registry's facts.
    ///
    /// See `docs/spec/matcher.md` 1.2a. `genesis_unverified` is given unless the registry says
    /// `genesis_verified: true` of every token the order trades (a pair order: A and B).
    /// `live_mint_authority` is given when a genesis record of one of them lists a live mint
    /// authority. The API fills the field ([`OrderView::with_warnings`]), because it holds the
    /// token registry.
    pub warnings: Vec<&'static str>,
    /// The order is under a retired template of the cancel-only table (`kob_protocol::retired`).
    ///
    /// `template_hash` names the template. Only the cancel of its maker is ever built for it. Only
    /// the single-order view serves such an order, and no list, book or plan includes it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cancel_only: Option<bool>,
}

impl OrderView {
    /// The covenant ids of the tokens the order trades: its token and a pair order's quote token B.
    pub fn token_ids(&self) -> Vec<Hash32> {
        let mut out: Vec<Hash32> = vec![];
        for t in self.token.iter().chain(self.pair.as_ref().map(|p| &p.quote)) {
            if let Ok(h) = Hash32::parse(t) {
                if !out.contains(&h) {
                    out.push(h);
                }
            }
        }
        out
    }

    /// The order with the warnings of its tokens, each code once.
    ///
    /// `warnings_of` gives the warnings of the covenant id of a token.
    pub fn with_warnings(mut self, warnings_of: impl Fn(&Hash32) -> Vec<&'static str>) -> Self {
        for t in self.token_ids() {
            for w in warnings_of(&t) {
                if !self.warnings.contains(&w) {
                    self.warnings.push(w);
                }
            }
        }
        self
    }
}

/// One custody a pair order holds (`AnyState::custodies`).
#[derive(Debug, Clone, Serialize)]
pub struct PairCustodyView {
    /// The custody's token (A or B).
    pub token: String,
    /// `base` (A) or `quote` (B).
    pub role: &'static str,
    /// The exact amount the current state requires (base units of that token).
    pub expected_amount: String,
    /// The live custody UTXO of that token (`role = custody`), when there is exactly one.
    ///
    /// It is set on single-order lookups and on the live orders of a `maker=` list.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub utxo: Option<TokenUtxoView>,
    /// Exactly one live custody UTXO of that token holds the expected amount (set with `utxo`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ok: Option<bool>,
}

/// A pair order's pair, side, price and custodies.
///
/// Token base units and prices are decimal strings.
#[derive(Debug, Clone, Serialize)]
pub struct PairView {
    /// The base token A (the order's `token`) and the quote token B.
    pub base: String,
    pub quote: String,
    pub base_family: Option<&'static str>,
    pub quote_family: Option<&'static str>,
    pub base_template_hash: String,
    pub quote_template_hash: String,
    /// Base units per whole A (the price's denominator) and per whole B.
    pub base_scale: i64,
    pub quote_scale: i64,
    /// `ask` (sells A for B; a sell-first entry) or `bid` (buys A with B; a buy-first entry).
    pub side: &'static str,
    /// The limit in B base units per whole A.
    ///
    /// For `KobPair` it is its price, the start of a decay. For `KobIfdPair` it is the limit of the
    /// entry. For `KobCondPair` it is its take-profit / limit leg, `null` without one.
    pub price: Option<String>,
    /// `price` as a reduced fraction per A base unit (`price / base_scale`).
    pub price_num: Option<String>,
    pub price_den: Option<String>,
    /// The stop price.
    ///
    /// For `KobCondPair` it is the stop (B per whole A, `null` without a stop leg). For
    /// `KobIfdPair` it is the entry stop (`null` for a limit entry).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stop_price: Option<String>,
    /// The quote now (an auction's current price, else `price`), B per whole A.
    pub quote_now: Option<String>,
    /// Base units of A still to trade, from the current state. `null` once closed.
    pub amount_left: Option<String>,
    /// The custodies the current state holds, in record order.
    ///
    /// A sell-first entry holds A, then its B prefund.
    pub custodies: Vec<PairCustodyView>,
    /// `KobIfdPair` sell-first: the B prefund per whole A.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub prefund: Option<String>,
    /// KAS on each token delivery.
    pub delivery_carrier: String,
}

fn family_name(code: i64) -> Option<&'static str> {
    kob_protocol::state::family_of_code(code).map(|f| if f == kob_protocol::family::Family::Kron { "kron" } else { "kcc20" })
}

/// The pair view of a pair order state.
///
/// `current` says that the tip state is known, so its custodies and amount are current.
pub fn pair_view(s: &AnyState, current: bool, quote_now: Option<String>) -> Option<PairView> {
    let t = s.pair_tokens()?;
    let (side, price, stop, prefund, carrier) = match s {
        AnyState::KobPair(p) => (p.is_ask(), Some(p.price), None, None, p.delivery_carrier),
        AnyState::KobCondPair(c) => (
            c.is_ask(),
            (c.tp_price > 0).then_some(c.tp_price),
            Some((c.stop_price > 0).then_some(c.stop_price)),
            None,
            c.delivery_carrier,
        ),
        AnyState::KobIfdPair(e) => (
            !e.is_buy_first(),
            Some(e.price),
            Some((e.entry_stop > 0).then_some(e.entry_stop)),
            (!e.is_buy_first()).then_some(e.prefund),
            e.delivery_carrier,
        ),
        _ => return None,
    };
    let frac = price.and_then(|p| reduce(p as i128, t.a.scale as i128));
    let custodies = if current {
        s.custodies()
            .into_iter()
            .map(|(tok, amount)| PairCustodyView {
                token: hex::encode(&tok),
                role: if tok == t.a.cov_id { "base" } else { "quote" },
                expected_amount: amount.to_string(),
                utxo: None,
                ok: None,
            })
            .collect()
    } else {
        vec![]
    };
    Some(PairView {
        base: hex::encode(&t.a.cov_id),
        quote: hex::encode(&t.b.cov_id),
        base_family: family_name(t.a.family),
        quote_family: family_name(t.b.family),
        base_template_hash: hex::encode(&t.a.tpl_hash),
        quote_template_hash: hex::encode(&t.b.tpl_hash),
        base_scale: t.a.scale,
        quote_scale: t.b.scale,
        side: if side { "ask" } else { "bid" },
        price: price.map(|p| p.to_string()),
        price_num: frac.map(|f| f.0.to_string()),
        price_den: frac.map(|f| f.1.to_string()),
        stop_price: stop.flatten().map(|v| v.to_string()),
        quote_now: quote_now.or_else(|| price.map(|p| p.to_string())),
        amount_left: if current { s.amount_left().map(|a| a.to_string()) } else { None },
        custodies,
        prefund: prefund.map(|p| p.to_string()),
        delivery_carrier: carrier.to_string(),
    })
}

fn gcd_u128(a: u128, b: u128) -> u128 {
    if b == 0 {
        a
    } else {
        gcd_u128(b, a % b)
    }
}

/// `(num / g, den / g)` of two positive values, or `None` when either is not positive.
pub fn reduce(num: i128, den: i128) -> Option<(u128, u128)> {
    if num <= 0 || den <= 0 {
        return None;
    }
    let (n, d) = (num as u128, den as u128);
    let g = gcd_u128(n, d).max(1);
    Some((n / g, d / g))
}

const ORDER_SELECT: &str = concat!(
    "SELECT o.covenant_id, o.contract, o_tm.h, o.family, o.side, o_mk.h, o_tk.h, \
    o_tp.h, o.scale, o.min_fill, o.price, o.tip, o.tif, o.expiry_daa, o.active_from, o.in_book, o.budget_rate, \
    o.reserve, o.initial_amount, o.genesis_txid, o.genesis_out, o.genesis_block, o.genesis_daa, o.parent, o.listed, \
    o.unlisted_reason, o.origin, s.status, s.filled_amount, s.remaining_amount, s.cur_txid, s.cur_idx, s.cur_value, \
    s.state_known, s.last_block, s.last_daa, o_ex.h, o.deadline, s.amount_exact, ",
    sql_ustate!("u", "o"),
    ", u.created_daa, fl.reason, ",
    sql_genesis!("o"),
    " \
    FROM orders o LEFT JOIN order_state s ON s.order_id = o.id \
    LEFT JOIN order_utxos u ON u.txid = s.cur_txid AND u.idx = s.cur_idx \
    LEFT JOIN order_flags fl ON fl.covenant_id = o.covenant_id AND fl.txid = s.cur_txid AND fl.idx = s.cur_idx",
    sql_order_ids!("o"),
    sql_order_ids_more!("o")
);

fn order_from_row(r: &Row<'_>, ctx: &ReadCtx) -> rusqlite::Result<OrderView> {
    let genesis_daa: i64 = r.get(22)?;
    let status: Option<String> = r.get(27)?;
    let status = status.unwrap_or_else(|| "unknown".to_string());
    let contract: String = r.get(1)?;
    let remaining: Option<i64> = r.get(29)?;
    let amount_exact: i64 = r.get::<_, Option<i64>>(38)?.unwrap_or(0);
    let last_daa: Option<i64> = r.get(35)?;
    let last_daa = last_daa.unwrap_or(genesis_daa);
    let expiry: Option<i64> = r.get(13)?;
    let active = status == "open" || status == "partial";
    let cur_txid: Option<Vec<u8>> = r.get(30)?;
    let cur_idx: Option<i64> = r.get(31)?;
    let cur_value: Option<i64> = r.get(32)?;
    let current = match (cur_txid, cur_idx) {
        (Some(t), Some(i)) => Some(OutpointView { txid: hx(t), index: i, value: sompi(cur_value) }),
        _ => None,
    };
    let deadline: Option<i64> = r.get(37)?;
    let state = tip_state(&contract, r.get(39)?);
    let current_daa: Option<i64> = r.get(40)?;
    let frozen_reason: Option<String> = r.get(41)?;
    let tif: Option<i64> = r.get(12)?;
    // Pair orders: the pair, the side and the price come from the state. That is the current one,
    // else the genesis state of a closed order, whose custodies and amount are then not shown.
    let genesis: Option<Vec<u8>> = r.get(42).ok().flatten();
    let placed = if state.is_none() { tip_state(&contract, genesis) } else { None };
    let pair_state = TemplateId::from_name(&contract)
        .filter(|t| t.is_pair())
        .and_then(|_| state.clone().map(|s| (s, true)).or_else(|| Some((placed.clone()?, false))));
    let (mut refund_due, mut kill) = (None, None);
    let (mut auction, mut quote, mut repeat) = (None, None, None);
    if let (Some(s), Some(d)) = (&state, current_daa) {
        refund_due = crate::indexer::processor::guarded(|| s.refund_due(d)).flatten();
        if matches!(tif, Some(1) | Some(2)) {
            kill = refund_due;
        }
        auction = auction_of(s, d, ctx.node_daa);
        quote = auction.as_ref().map(|a| a.current_price.clone());
        repeat = repeat_of(s);
    }
    // A pair order has no KAS quote. Its quote now (B per whole A) is `pair.quote_now`.
    let pair = pair_state.and_then(|(s, cur)| pair_view(&s, cur, quote.take()));
    if quote.is_none() && pair.is_none() {
        quote = sompi(r.get(10)?);
    }
    Ok(OrderView {
        cancel_only: None,
        covenant_id: hx(r.get(0)?),
        contract,
        template_hash: hx(r.get(2)?),
        family: r.get(3)?,
        side: r.get(4)?,
        maker: hxo(r.get(5)?),
        token: hxo(r.get(6)?),
        token_template_hash: hxo(r.get(7)?),
        extension_commitment: hxo(r.get(36)?),
        scale: r.get(8)?,
        min_fill: sompi(r.get(9)?),
        price: sompi(r.get(10)?),
        tip: sompi(r.get(11)?),
        tif,
        expiry_daa: expiry,
        active_from: r.get(14)?,
        in_book: r.get::<_, i64>(15)? != 0,
        budget_rate: sompi(r.get(16)?),
        reserve: sompi(r.get(17)?),
        initial_amount: sompi(r.get(18)?),
        genesis: GenesisView {
            txid: hxo(r.get(19)?),
            out: r.get(20)?,
            block_seq: r.get(21)?,
            daa: genesis_daa,
            confirmations: ctx.confirmations(genesis_daa),
            settled: ctx.settled(genesis_daa),
        },
        parent: hxo(r.get(23)?),
        listed: r.get::<_, i64>(24)? != 0,
        unlisted_reason: r.get(25)?,
        origin: r.get(26)?,
        expired: active && expiry.is_some_and(|e| ctx.node_daa.is_some_and(|n| n as i64 >= e)),
        possibly_frozen: frozen_reason.is_some() && active,
        possibly_frozen_basis: (frozen_reason.is_some() && active).then_some("operator-engine-pre-simulation"),
        frozen_reason: frozen_reason.filter(|_| active),
        status,
        filled_amount: r.get::<_, Option<i64>>(28)?.unwrap_or(0).to_string(),
        amount_left: sompi(remaining),
        amount_estimated: amount_exact == 0 || remaining.is_none(),
        current,
        state_known: r.get::<_, Option<i64>>(33)?.unwrap_or(0) != 0,
        state: state.as_ref().and_then(|s| serde_json::to_value(s).ok()),
        placed_state: placed.as_ref().and_then(|s| serde_json::to_value(s).ok()),
        current_daa,
        deadline_passed: active && deadline_passed(deadline, ctx),
        deadline,
        refund_due_daa: refund_due,
        kill_daa: kill,
        auction,
        quote,
        repeat,
        custody: None,
        strays: None,
        strays_total: None,
        last_block: r.get::<_, Option<i64>>(34)?.unwrap_or(0),
        confirmations: ctx.confirmations(last_daa),
        settled: ctx.settled(last_daa),
        last_daa,
        children: None,
        children_total: None,
        pair,
        warnings: vec![],
    })
}

/// Repeat IFD / IFO position data of a tip state.
///
/// It is an entry (`rptAmount > 0`) or a booked exit.
#[derive(Debug, Clone, Serialize)]
pub struct RepeatView {
    /// `entry` or `exit`.
    pub role: &'static str,
    /// Entry: `rptAmount`. 0 means not repeating, else 1 plus the base units it may still re-arm.
    pub rpt_amount: Option<String>,
    /// Entry: base units it may still re-arm (`rptAmount - 1`, never below 0).
    pub rearm_amount: Option<String>,
    /// Exit: the entry it re-arms.
    pub parent: Option<String>,
    /// Exit: from this DAA score a take-profit may skip the entry's merge.
    pub rpt_until: Option<String>,
    /// Exit: sompi per whole token returned to the entry, rounded up per merge.
    ///
    /// For a buy-first entry it is its budget rate `price + tip`. For a sell-first entry it is its
    /// proceeds rate `price - tip`.
    pub rpt_price: Option<String>,
}

fn repeat_of(s: &AnyState) -> Option<RepeatView> {
    let entry = |rpt: i64| RepeatView {
        role: "entry",
        rpt_amount: Some(rpt.to_string()),
        rearm_amount: Some(rpt.saturating_sub(1).max(0).to_string()),
        parent: None,
        rpt_until: None,
        rpt_price: None,
    };
    let exit = |parent: &[u8; 32], until: i64, price: i64| RepeatView {
        role: "exit",
        rpt_amount: None,
        rearm_amount: None,
        parent: Some(hex::encode(parent)),
        rpt_until: Some(until.to_string()),
        rpt_price: Some(price.to_string()),
    };
    match s {
        AnyState::KobIfdBid(b) | AnyState::KobIfdBidKron(b) if b.rpt_amount > 0 => Some(entry(b.rpt_amount)),
        AnyState::KobIfdAsk(a) | AnyState::KobIfdAskKron(a) if a.rpt_amount > 0 => Some(entry(a.rpt_amount)),
        AnyState::KobCondAsk(c) | AnyState::KobCondAskKron(c) if c.parent != [0; 32] => {
            Some(exit(&c.parent, c.rpt_until, c.rpt_price))
        }
        AnyState::KobCondBid(c) | AnyState::KobCondBidKron(c) if c.parent != [0; 32] => {
            Some(exit(&c.parent, c.rpt_until, c.rpt_price))
        }
        _ => None,
    }
}

/// A token UTXO owned by an order covenant id.
#[derive(Debug, Clone, Serialize)]
pub struct TokenUtxoView {
    pub txid: String,
    pub index: i64,
    pub token: String,
    pub owner: String,
    /// Token base units.
    pub amount: String,
    /// KAS carrier.
    pub value: String,
    /// `custody` or `stray`.
    pub role: String,
    pub created_daa: i64,
    pub spent: bool,
    pub spent_txid: Option<String>,
    pub confirmations: Option<i64>,
    pub settled: bool,
    /// The token state the indexer proved against the output's script public key, as JSON.
    ///
    /// It has the shape the builders of kob-wasm take, which is what a client needs to spend the
    /// UTXO in a cancel, cancel-replace or refund. Absent when the indexer holds no proven state
    /// (a token it does not track). A client then rebuilds a custody state from the owner, the
    /// amount and the extension commitment of the order.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub state: Option<Value>,
    /// The token program `state` was proven under (`KCC20Ref_8x8`, `KronToken2433`, ...).
    ///
    /// A client needs it to move a FOREIGN stray. The program of the order's own token is in the
    /// state of the order.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub program: Option<String>,
    /// `true` for a FOREIGN stray: a token other than the order's own.
    ///
    /// For a pair order, it is a token that is neither of its two (`matcher.md` 1.2). The program
    /// of the token authorises it with any spend of the order, so whoever builds that spend can
    /// move it. The maker's cancel or sweep does, and so do keepers that return foreign strays to
    /// the maker with a refund.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub foreign: bool,
}

/// The columns [`token_utxo_from_row`] reads.
///
/// `t` is `token_utxos` and `h` is the proven `token_holdings` row of the output.
const TOKEN_UTXO_COLUMNS: &str =
    "t.txid, t.idx, tk.h, fo.covenant_id, t.amount, t.value, t.role, t.created_daa, t.spent_block, t.spent_txid, h.program, kob_hstate(h.state, h.family, hw.h, h.owner_kind, h.amount), \
     (fo.token_id IS NOT NULL AND t.token_id != fo.token_id AND (fo.quote_id IS NULL OR t.token_id != fo.quote_id))";
/// The FROM of the token UTXO columns.
///
/// `fo` is the owning order, which always exists because the UTXO was stored under its row. `tk`
/// is the token.
const TOKEN_UTXO_FROM: &str = "FROM token_utxos t JOIN ids tk ON tk.id = t.token_id JOIN orders fo ON fo.id = t.order_id \
     LEFT JOIN token_holdings h ON h.txid = t.txid AND h.idx = t.idx LEFT JOIN ids hw ON hw.id = h.owner_id";

/// A proven token state span as JSON, decoded with the program the indexer proved it for.
fn proven_state_json(program: Option<String>, state: Option<Vec<u8>>) -> Option<Value> {
    let tpl = TemplateId::from_name(&program?).filter(|t| t.is_token()).map(kob_protocol::artifacts::token_template)?;
    let decoded = kob_protocol::state::TokenState::decode_with(tpl, &state?).ok()?;
    serde_json::to_value(&decoded).ok()
}

fn token_utxo_from_row(r: &Row<'_>, ctx: &ReadCtx) -> rusqlite::Result<TokenUtxoView> {
    let daa: i64 = r.get(7)?;
    let spent_block: Option<i64> = r.get(8)?;
    Ok(TokenUtxoView {
        txid: hx(r.get(0)?),
        index: r.get(1)?,
        token: hx(r.get(2)?),
        owner: hx(r.get(3)?),
        amount: r.get::<_, i64>(4)?.to_string(),
        value: r.get::<_, i64>(5)?.to_string(),
        role: r.get(6)?,
        created_daa: daa,
        spent: spent_block.is_some(),
        spent_txid: hxo(r.get(9)?),
        confirmations: ctx.confirmations(daa),
        settled: ctx.settled(daa),
        state: proven_state_json(r.get(10)?, r.get(11)?),
        program: r.get::<_, Option<String>>(10)?.filter(|p| TemplateId::from_name(p).is_some_and(|t| t.is_token())),
        foreign: r.get::<_, Option<bool>>(12)?.unwrap_or(false),
    })
}

/// The exact-custody check of an ask-side order (`docs/spec/matcher.md` 1.2).
#[derive(Debug, Clone, Serialize)]
pub struct CustodyView {
    /// `amountLeft` of the current state (base units), when known.
    pub expected_amount: Option<String>,
    /// The live custody UTXO (`role = custody`), when there is exactly one.
    pub utxo: Option<TokenUtxoView>,
    /// Exactly one live custody UTXO holds the expected amount (or the order needs none).
    pub ok: bool,
}

fn live_token_utxos(conn: &Connection, ctx: &ReadCtx, owner: &Hash32, role: &str, limit: usize) -> DbResult<Vec<TokenUtxoView>> {
    let sql = format!(
        "SELECT {TOKEN_UTXO_COLUMNS} {TOKEN_UTXO_FROM} WHERE t.order_id = (SELECT id FROM orders WHERE covenant_id = ?1) AND t.role = ?2 AND t.spent_block IS NULL \
         ORDER BY t.created_block, t.txid, t.idx LIMIT ?3"
    );
    let mut st = conn.prepare(&sql)?;
    let rows = st.query_map(rusqlite::params![owner.0.to_vec(), role, limit as i64], |r| token_utxo_from_row(r, ctx))?;
    Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
}

/// Most strays an order view carries.
///
/// Anyone can send token dust to an order id (about 0.08 KAS each), and the view is read on the
/// light lane. An unbounded list would make an answer of megabytes per request for 20,000
/// strays. One cancel transaction cannot take anywhere near this many (the CLI reports what it
/// leaves). `strays_total` says how many more there are, and `GET /v1/strays?maker=` pages all of
/// a maker's.
pub const MAX_VIEW_STRAYS: usize = 100;

/// One order by covenant id, with its exit orders, custody and strays.
///
/// The exit orders are given as the covenant ids of any orders it created.
///
/// An order under a template this build neither pins nor keeps in its cancel-only table is not
/// found (`None`, a 404), as `/v1/orders` never lists it (`docs/spec/template-retirement.md`).
/// Its view would carry a state decoded with today's template and a current UTXO whose script
/// nothing this build plans hashes to. A client building its cancel from that view would fail on
/// chain. Instead, the web wallet resolves the order from its own placement record and marks it
/// `old-template`, ended with the release it was placed with.
///
/// An order under a retired template of the cancel-only table (`kob_protocol::retired`) is
/// served, marked `cancel_only`, and its `template_hash` is the retired one. Its maker's cancel
/// spends the `cancel(sig)` of that template, the only thing this build builds for it. No list
/// includes it.
pub fn order(conn: &Connection, ctx: &ReadCtx, covenant_id: &Hash32) -> DbResult<Option<OrderView>> {
    let pinned = pinned_template();
    let sql = format!("{ORDER_SELECT} WHERE o.covenant_id = ?1 AND {}", pinned_or_cancel_only_template());
    let v = conn.query_row(&sql, [covenant_id.0.to_vec()], |r| order_from_row(r, ctx)).optional()?;
    let Some(mut v) = v else { return Ok(None) };
    if kob_protocol::json::hex32(&v.template_hash).ok().and_then(|h| kob_protocol::retired::retired_by_hash(&h)).is_some() {
        v.cancel_only = Some(true);
    }
    // The newest children, oldest first. A repeating entry books an exit per fill, so the list is
    // bounded like the strays.
    let mut st = conn.prepare(&format!(
        "SELECT o.covenant_id FROM orders o WHERE o.parent = ?1 AND {pinned} ORDER BY o.genesis_block DESC, o.covenant_id DESC LIMIT ?2"
    ))?;
    let mut kids = st
        .query_map(rusqlite::params![covenant_id.0.to_vec(), MAX_VIEW_CHILDREN as i64 + 1], |r| Ok(hx(r.get(0)?)))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    if kids.len() > MAX_VIEW_CHILDREN {
        kids.truncate(MAX_VIEW_CHILDREN);
        v.children_total = Some(conn.query_row(
            &format!("SELECT COUNT(*) FROM orders o WHERE o.parent = ?1 AND {pinned}"),
            [covenant_id.0.to_vec()],
            |r| r.get(0),
        )?);
    }
    kids.reverse();
    v.children = Some(kids);
    attach_holdings(conn, ctx, covenant_id, &mut v)?;
    Ok(Some(v))
}

/// Most child orders (if-done exits) an order view carries: the newest. `children_total` says how
/// many there are when there are more.
pub const MAX_VIEW_CHILDREN: usize = 100;

/// Whether [`order`] finds the order: the same template rule, without building the view.
pub fn order_exists(conn: &Connection, covenant_id: &Hash32) -> DbResult<bool> {
    let sql = format!("SELECT 1 FROM orders o WHERE o.covenant_id = ?1 AND {}", pinned_or_cancel_only_template());
    Ok(conn.prepare_cached(&sql)?.exists([covenant_id.0.to_vec()])?)
}

/// The exact custody of an order (ask kinds) and its live strays, as the single-order view has it.
fn attach_holdings(conn: &Connection, ctx: &ReadCtx, covenant_id: &Hash32, v: &mut OrderView) -> DbResult<()> {
    if let Some(p) = v.pair.as_mut() {
        // A pair order: each custody of its current state (one per token), checked against the
        // live custody rows of that token.
        let live = live_token_utxos(conn, ctx, covenant_id, "custody", 2)?;
        let mut all_ok = live.len() == p.custodies.len();
        for c in p.custodies.iter_mut() {
            let mut mine: Vec<TokenUtxoView> = live.iter().filter(|u| u.token == c.token).cloned().collect();
            let ok = mine.len() == 1 && mine[0].amount == c.expected_amount;
            all_ok &= ok;
            c.ok = Some(ok);
            c.utxo = if mine.len() == 1 { mine.pop() } else { None };
        }
        if v.state.is_some() {
            let first = p.custodies.first();
            v.custody = Some(CustodyView {
                expected_amount: first.map(|c| c.expected_amount.clone()),
                utxo: first.and_then(|c| c.utxo.clone()),
                ok: all_ok,
            });
        }
    } else if v.side == 1 {
        let expected = v
            .state
            .as_ref()
            .and_then(|s| serde_json::from_value::<AnyState>(s.clone()).ok())
            .and_then(|s| crate::indexer::processor::guarded(|| s.custody_amount()).flatten());
        let mut live = live_token_utxos(conn, ctx, covenant_id, "custody", 2)?;
        let ok = match expected {
            Some(0) => live.is_empty(),
            Some(e) => live.len() == 1 && live[0].amount == e.to_string(),
            None => false,
        };
        v.custody = Some(CustodyView {
            expected_amount: expected.map(|e| e.to_string()),
            utxo: if live.len() == 1 { live.pop() } else { None },
            ok,
        });
    }
    let strays = live_token_utxos(conn, ctx, covenant_id, "stray", MAX_VIEW_STRAYS + 1)?;
    if strays.len() > MAX_VIEW_STRAYS {
        v.strays_total = Some(conn.query_row(
            concat!(
                "SELECT COUNT(*) FROM token_utxos WHERE order_id = ",
                order_of!("?1"),
                " AND role = 'stray' AND spent_block IS NULL"
            ),
            [covenant_id.0.to_vec()],
            |r| r.get(0),
        )?);
    }
    v.strays = Some(strays.into_iter().take(MAX_VIEW_STRAYS).collect());
    Ok(())
}

/// A live stray with the order it is stuck on.
#[derive(Debug, Clone, Serialize)]
pub struct StrayView {
    #[serde(flatten)]
    pub utxo: TokenUtxoView,
    pub order_status: Option<String>,
    pub maker: Option<String>,
    /// The order has terminated: the maker's cancel can no longer sweep this stray.
    pub lost: bool,
}

/// Stray token UTXOs (sent to an order id outside the protocol), newest first.
pub fn strays(conn: &Connection, ctx: &ReadCtx, maker: Option<&[u8]>, limit: usize) -> DbResult<Vec<StrayView>> {
    let sql = format!(
        "SELECT {TOKEN_UTXO_COLUMNS}, s.status, fo_mk.h \
         {TOKEN_UTXO_FROM} LEFT JOIN order_state s ON s.order_id = t.order_id LEFT JOIN ids fo_mk ON fo_mk.id = fo.maker_id \
         WHERE t.role = 'stray' AND t.spent_block IS NULL AND (?1 IS NULL OR fo.maker_id = (SELECT id FROM ids WHERE h = ?1)) \
         ORDER BY t.created_block DESC, t.txid, t.idx LIMIT ?2"
    );
    let mut st = conn.prepare(&sql)?;
    let rows = st.query_map(rusqlite::params![maker, limit as i64], |r| {
        let status: Option<String> = r.get(13)?;
        let lost = !matches!(status.as_deref(), Some("open") | Some("partial"));
        Ok(StrayView { utxo: token_utxo_from_row(r, ctx)?, order_status: status, maker: hxo(r.get(14)?), lost })
    })?;
    Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
}

/// Token registry events (first sightings of a token identity), oldest first.
#[derive(Debug, Clone, Serialize)]
pub struct TokenEventView {
    pub token: String,
    pub template_hash: Option<String>,
    pub extension_commitment: Option<String>,
    pub kind: String,
    pub txid: String,
    pub daa: i64,
}

/// The token registry events, oldest first, of one token or of all, at most `limit`.
pub fn token_events(conn: &Connection, token: Option<&Hash32>, limit: usize) -> DbResult<Vec<TokenEventView>> {
    let mut st = conn.prepare(
        "SELECT token_cov_id, tpl_hash, ext_commit, kind, txid, daa FROM token_events \
         WHERE (?1 IS NULL OR token_cov_id = ?1) ORDER BY id ASC LIMIT ?2",
    )?;
    let rows = st.query_map(rusqlite::params![token.map(|t| t.0.to_vec()), limit as i64], |r| {
        Ok(TokenEventView {
            token: hx(r.get(0)?),
            template_hash: hxo(r.get(1)?),
            extension_commitment: hxo(r.get(2)?),
            kind: r.get(3)?,
            txid: hx(r.get(4)?),
            daa: r.get(5)?,
        })
    })?;
    Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
}

/// The filters of the orders list.
#[derive(Debug, Clone, Default)]
pub struct OrderFilter {
    pub maker: Option<Vec<u8>>,
    pub token: Option<Hash32>,
    /// Status filter, one of [`ORDER_STATUSES`].
    ///
    /// `active` means open or partial.
    pub status: Option<String>,
}

/// The order statuses a filter may name.
pub const ORDER_STATUSES: [&str; 8] = ["open", "partial", "filled", "cancelled", "refunded", "killed", "closed", "active"];

/// Cursor of the orders list: `<genesis_block>:<covenant id hex>`.
pub fn parse_order_cursor(s: &str) -> Option<(i64, Vec<u8>)> {
    let (b, c) = s.split_once(':')?;
    Some((b.parse().ok()?, hex::decode(c).ok()?))
}

/// [`ORDER_SELECT`] driven from `order_state`, for a status filter whose orders are few.
///
/// The rows of the statuses are read through `order_state_status` and joined to their orders
/// (`CROSS JOIN` keeps that order), instead of every order of the token or maker being read for
/// the few of that status.
const ORDER_SELECT_BY_STATUS: &str = concat!(
    "SELECT o.covenant_id, o.contract, o_tm.h, o.family, o.side, o_mk.h, o_tk.h, \
    o_tp.h, o.scale, o.min_fill, o.price, o.tip, o.tif, o.expiry_daa, o.active_from, o.in_book, o.budget_rate, \
    o.reserve, o.initial_amount, o.genesis_txid, o.genesis_out, o.genesis_block, o.genesis_daa, o.parent, o.listed, \
    o.unlisted_reason, o.origin, s.status, s.filled_amount, s.remaining_amount, s.cur_txid, s.cur_idx, s.cur_value, \
    s.state_known, s.last_block, s.last_daa, o_ex.h, o.deadline, s.amount_exact, ",
    sql_ustate!("u", "o"),
    ", u.created_daa, fl.reason, ",
    sql_genesis!("o"),
    " \
    FROM order_state s CROSS JOIN orders o ON o.id = s.order_id \
    LEFT JOIN order_utxos u ON u.txid = s.cur_txid AND u.idx = s.cur_idx \
    LEFT JOIN order_flags fl ON fl.covenant_id = o.covenant_id AND fl.txid = s.cur_txid AND fl.idx = s.cur_idx",
    sql_order_ids!("o"),
    sql_order_ids_more!("o")
);

/// The number of orders in `statuses`, from the row counters the write path keeps.
///
/// `None` without the counters.
fn status_count(conn: &Connection, statuses: &[&str]) -> DbResult<Option<i64>> {
    Ok(kept_counters(conn)?.map(|k| statuses.iter().map(|s| k.get(&format!("status:{s}")).copied().unwrap_or(0)).sum()))
}

/// Orders newest first, filtered, keyset-paginated on (genesis block, covenant id).
///
/// A page costs one index range read in `(genesis_block, covenant_id)` order that stops at the
/// end of the page. The index is `orders_token_genesis` and `orders_quote_genesis` merged for a
/// token, `orders_maker_genesis` for a maker, and `orders_genesis_page` otherwise. A status
/// filter whose orders are few (`active`, `open`, `partial`: the live set) reads those orders
/// instead and sorts them. Both give the same rows in the same order.
pub fn orders(
    conn: &Connection,
    ctx: &ReadCtx,
    f: &OrderFilter,
    limit: usize,
    cursor: Option<(i64, Vec<u8>)>,
) -> DbResult<Page<OrderView>> {
    // Never an order under a template this build does not pin ([`pinned_template`]).
    let mut conds: Vec<String> = vec![pinned_template().to_string()];
    let mut args: Vec<SqlValue> = Vec::new();
    if let Some(m) = &f.maker {
        args.push(SqlValue::Blob(m.clone()));
        conds.push(format!("o.maker_id = (SELECT id FROM ids WHERE h = ?{})", args.len()));
    }
    let statuses: Option<Vec<&str>> = f.status.as_deref().map(|s| if s == "active" { vec!["open", "partial"] } else { vec![s] });
    // Read the status rows and sort them when that is the cheaper side: n rows of the statuses,
    // against about (limit x orders / n) rows of the history read in order before a page of them
    // is found. That is n^2 <= limit x orders.
    let by_status = match &statuses {
        None => false,
        Some(s) => {
            let n = status_count(conn, s)?;
            let total = kept_counters(conn)?.and_then(|k| k.get("orders").copied());
            match (n, total) {
                (Some(n), Some(total)) => (n as i128) * (n as i128) <= (limit as i128 + 1) * total.max(1) as i128,
                // No counters (a database a writer of this build has not opened): the live
                // statuses are the few ones.
                _ => s.iter().all(|s| *s == "open" || *s == "partial"),
            }
        }
    };
    if let Some(s) = &f.status {
        // Read in history order, the status is a filter on the rows (`+`). The planner, left to
        // choose, reads every order of the status through `order_state_status` and sorts them.
        // Measured: `docs/ops/executor.md`, 5.5.
        let col = if by_status { "s.status" } else { "+s.status" };
        if s == "active" {
            conds.push(format!("{col} IN ('open','partial')"));
        } else {
            args.push(SqlValue::Text(s.clone()));
            conds.push(format!("{col} = ?{}", args.len()));
        }
    }
    if let Some((b, c)) = cursor {
        args.push(SqlValue::Integer(b));
        args.push(SqlValue::Blob(c));
        conds.push(format!("(o.genesis_block, o.covenant_id) < (?{}, ?{})", args.len() - 1, args.len()));
    }
    let token = f.token.as_ref().map(|t| {
        args.push(SqlValue::Blob(t.0.to_vec()));
        args.len()
    });
    let where_ = |extra: &[String]| {
        let all: Vec<&String> = conds.iter().chain(extra).collect();
        if all.is_empty() {
            String::new()
        } else {
            format!(" WHERE {}", all.iter().map(|s| s.as_str()).collect::<Vec<_>>().join(" AND "))
        }
    };
    args.push(SqlValue::Integer(limit as i64 + 1));
    let lim = args.len();
    let sql = match token {
        // A pair order trades two tokens: it is listed under either (base A, quote B).
        Some(n) if by_status => format!(
            "{ORDER_SELECT_BY_STATUS}{} ORDER BY o.genesis_block DESC, o.covenant_id DESC LIMIT ?{lim}",
            where_(&[format!(
                "(o.token_id = (SELECT id FROM ids WHERE h = ?{n}) OR o.quote_id = (SELECT id FROM ids WHERE h = ?{n}))"
            )])
        ),
        // One index-ordered read per column, merged. A pair order of A / A is read once, from the
        // first.
        Some(n) => format!(
            "{ORDER_SELECT}{} UNION ALL {ORDER_SELECT}{} ORDER BY 22 DESC, 1 DESC LIMIT ?{lim}",
            where_(&[format!("o.token_id = (SELECT id FROM ids WHERE h = ?{n})")]),
            where_(&[
                format!("o.quote_id = (SELECT id FROM ids WHERE h = ?{n})"),
                format!("o.token_id IS NOT (SELECT id FROM ids WHERE h = ?{n})")
            ])
        ),
        None if by_status => {
            format!("{ORDER_SELECT_BY_STATUS}{} ORDER BY o.genesis_block DESC, o.covenant_id DESC LIMIT ?{lim}", where_(&[]))
        }
        None => format!("{ORDER_SELECT}{} ORDER BY o.genesis_block DESC, o.covenant_id DESC LIMIT ?{lim}", where_(&[])),
    };
    let mut st = conn.prepare(&sql)?;
    let mut items = st.query_map(params_from_iter(args.iter()), |r| order_from_row(r, ctx))?.collect::<rusqlite::Result<Vec<_>>>()?;
    let next_cursor = if items.len() > limit {
        items.truncate(limit);
        items.last().map(|o| format!("{}:{}", o.genesis.block_seq, o.covenant_id))
    } else {
        None
    };
    // A maker's own list (the wallet's My orders) carries the custody and strays of its LIVE
    // orders, as the single-order view does. A cancel sweeps them and the wallet shows them per
    // order. It costs one indexed lookup per live order of one maker.
    if f.maker.is_some() {
        for v in items.iter_mut().filter(|v| v.status == "open" || v.status == "partial") {
            if let Ok(id) = Hash32::parse(&v.covenant_id) {
                attach_holdings(conn, ctx, &id, v)?;
            }
        }
    }
    Ok(Page { items, next_cursor })
}

// ---------------------------------------------------------------------------------------------
// Events and fills

/// A lifecycle event of an order.
#[derive(Debug, Clone, Serialize)]
pub struct EventView {
    pub id: i64,
    pub covenant_id: String,
    pub block_seq: i64,
    pub daa: i64,
    pub ts: i64,
    pub txid: String,
    pub tx_pos: i64,
    pub kind: String,
    pub token: Option<String>,
    pub side: Option<i64>,
    /// Base units, by event kind.
    ///
    /// It is the amount filled (`fill`), `amountLeft` at creation (`create`), returned (`kill`),
    /// left (`refund`, `amend`, `sweep`) or merged back (`rearm`).
    pub amount: Option<String>,
    /// Sompi per whole token.
    ///
    /// For a fill it is the quote of the fill (an auction: at the time argument of the fill),
    /// otherwise the limit of the order.
    pub price: Option<String>,
    pub payout: Option<String>,
    pub closes: bool,
    pub detail: Option<Value>,
    pub confirmations: Option<i64>,
    pub settled: bool,
}

/// The columns of an event read.
///
/// `e` is the event, `o` its order and `t` its token. `CROSS JOIN` keeps the events first, in the
/// order the page reads them.
const EVENT_SELECT: &str = "SELECT e.id, o.covenant_id, e.block_seq, e.daa, e.ts, e.txid, e.tx_pos, e.kind, t.h, e.side, e.amount, e.price, e.payout, e.closes, \
     kob_dtext(e.detail) FROM order_events e CROSS JOIN orders o ON o.id = e.order_id LEFT JOIN ids t ON t.id = e.token_id";

fn event_from_row(r: &Row<'_>, ctx: &ReadCtx) -> rusqlite::Result<EventView> {
    let daa: i64 = r.get(3)?;
    let detail: Option<String> = r.get(14)?;
    Ok(EventView {
        id: r.get(0)?,
        covenant_id: hx(r.get(1)?),
        block_seq: r.get(2)?,
        daa,
        ts: r.get(4)?,
        txid: hx(r.get(5)?),
        tx_pos: r.get(6)?,
        kind: r.get(7)?,
        token: hxo(r.get(8)?),
        side: r.get(9)?,
        amount: sompi(r.get(10)?),
        price: sompi(r.get(11)?),
        payout: sompi(r.get(12)?),
        closes: r.get::<_, i64>(13)? != 0,
        detail: detail.map(|d| serde_json::from_str(&d).unwrap_or(Value::String(d))),
        confirmations: ctx.confirmations(daa),
        settled: ctx.settled(daa),
    })
}

/// Lifecycle events of one order, oldest first.
///
/// `after` is the id of the last event already seen.
pub fn order_events(
    conn: &Connection,
    ctx: &ReadCtx,
    covenant_id: &Hash32,
    limit: usize,
    after: Option<i64>,
) -> DbResult<Page<EventView>> {
    let sql = format!(
        "{EVENT_SELECT} WHERE e.order_id = (SELECT id FROM orders WHERE covenant_id = ?1) AND e.id > ?2 ORDER BY e.id ASC LIMIT ?3"
    );
    let mut st = conn.prepare(&sql)?;
    let mut items = st
        .query_map(rusqlite::params![covenant_id.0.to_vec(), after.unwrap_or(0), limit as i64 + 1], |r| event_from_row(r, ctx))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let next_cursor = if items.len() > limit {
        items.truncate(limit);
        items.last().map(|e| e.id.to_string())
    } else {
        None
    };
    Ok(Page { items, next_cursor })
}

/// Recent fills, newest first.
///
/// `before` is the id of the oldest fill already seen.
pub fn fills(
    conn: &Connection,
    ctx: &ReadCtx,
    token: Option<&Hash32>,
    side: Option<i64>,
    before: Option<i64>,
    limit: usize,
) -> DbResult<Page<EventView>> {
    let mut conds = vec!["e.kind = 'fill'".to_string()];
    let mut args: Vec<SqlValue> = Vec::new();
    if let Some(t) = token {
        args.push(SqlValue::Blob(t.0.to_vec()));
        conds.push(format!("e.token_id = (SELECT id FROM ids WHERE h = ?{})", args.len()));
    }
    if let Some(s) = side {
        args.push(SqlValue::Integer(s));
        conds.push(format!("e.side = ?{}", args.len()));
    }
    if let Some(b) = before {
        args.push(SqlValue::Integer(b));
        conds.push(format!("e.id < ?{}", args.len()));
    }
    args.push(SqlValue::Integer(limit as i64 + 1));
    let sql = format!("{EVENT_SELECT} WHERE {} ORDER BY e.id DESC LIMIT ?{}", conds.join(" AND "), args.len());
    let mut st = conn.prepare(&sql)?;
    let mut items = st.query_map(params_from_iter(args.iter()), |r| event_from_row(r, ctx))?.collect::<rusqlite::Result<Vec<_>>>()?;
    let next_cursor = if items.len() > limit {
        items.truncate(limit);
        items.last().map(|e| e.id.to_string())
    } else {
        None
    };
    Ok(Page { items, next_cursor })
}

// ---------------------------------------------------------------------------------------------
// Counters

/// The `/v1/health` counters of the store.
#[derive(Debug, Clone, Serialize)]
pub struct Counters {
    pub orders_total: i64,
    pub orders_listed: i64,
    pub orders_by_status: BTreeMap<String, i64>,
    pub rejects_total: i64,
    pub fills_total: i64,
    pub last_block_seq: i64,
    pub last_block_daa: i64,
}

fn count(conn: &Connection, sql: &str) -> DbResult<i64> {
    conn.query_row(sql, [], |r| r.get::<_, i64>(0)).map_err(DbError::from)
}

/// The row counters the write path keeps (`counters`, `schema.sql`).
///
/// It is `None` on a database a writer of this build has not opened yet: no table, or not counted
/// yet ([`super::db::backfill_counters`]).
fn kept_counters(conn: &Connection) -> DbResult<Option<BTreeMap<String, i64>>> {
    let kept = conn.prepare_cached("SELECT 1 FROM meta WHERE k = 'counters'")?.exists([])?
        && conn.prepare_cached("SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = 'counters'")?.exists([])?;
    if !kept {
        return Ok(None);
    }
    let mut st = conn.prepare_cached("SELECT name, n FROM counters")?;
    let rows = st.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)))?;
    Ok(Some(rows.collect::<rusqlite::Result<_>>()?))
}

/// Where the trade history of a lean store is complete from, in unix ms.
///
/// It feeds `history_from` of `/v1/health` and `/v1/stats`. It is the last event of the oldest
/// closed order that the store still holds and that its pruning could delete (no live exit). When
/// there is no such order, it is the oldest event. `None` for an empty store. The lean pruning
/// ([`super::lean`]) deletes closed orders, oldest last activity first, with their fills. So
/// every order with a fill after that time is still there, while closed orders before it may be
/// gone (the live book keeps its own older events). The read is read only, in two index reads
/// (`order_state_closed_daa`, `order_events_cov`, `order_events_ts`).
pub fn history_from(conn: &Connection) -> DbResult<Option<i64>> {
    let closed: Option<i64> = conn.query_row(
        "SELECT MAX(e.ts) FROM order_events e WHERE e.order_id = (\
           SELECT s.order_id FROM order_state s WHERE s.status NOT IN ('open','partial') \
           AND NOT EXISTS (SELECT 1 FROM orders p JOIN orders c ON c.parent = p.covenant_id JOIN order_state cs ON cs.order_id = c.id \
                           WHERE p.id = s.order_id AND cs.status IN ('open','partial')) \
           ORDER BY s.last_daa LIMIT 1)",
        [],
        |r| r.get(0),
    )?;
    let oldest: Option<i64> = conn.query_row("SELECT MIN(ts) FROM order_events", [], |r| r.get(0))?;
    Ok(closed.max(oldest))
}

/// The `/v1/health` counters.
///
/// They come from the counters the write path keeps (a few rows). A database not yet opened by a
/// writer of this build has none, and then they are counted: a scan of every order, event and
/// state.
pub fn counters(conn: &Connection) -> DbResult<Counters> {
    let last = conn
        .query_row("SELECT seq, daa FROM blocks ORDER BY seq DESC LIMIT 1", [], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?)))
        .optional()?;
    let (last_block_seq, last_block_daa) = last.unwrap_or((0, 0));
    if let Some(kept) = kept_counters(conn)? {
        let get = |k: &str| kept.get(k).copied().unwrap_or(0);
        // A status no order has any more is left out, as the count by status leaves it out.
        let by_status =
            kept.iter().filter_map(|(k, n)| Some((k.strip_prefix("status:")?.to_string(), *n)).filter(|_| *n != 0)).collect();
        return Ok(Counters {
            orders_total: get("orders"),
            orders_listed: get("orders_listed"),
            orders_by_status: by_status,
            rejects_total: get("rejects"),
            fills_total: get("fills"),
            last_block_seq,
            last_block_daa,
        });
    }
    counted(conn, last_block_seq, last_block_daa)
}

/// [`counters`] counted from the tables.
pub fn counted(conn: &Connection, last_block_seq: i64, last_block_daa: i64) -> DbResult<Counters> {
    let mut by_status = BTreeMap::new();
    let mut st = conn.prepare("SELECT status, COUNT(*) FROM order_state GROUP BY status")?;
    for r in st.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)))? {
        let (s, n) = r?;
        by_status.insert(s, n);
    }
    Ok(Counters {
        orders_total: count(conn, "SELECT COUNT(*) FROM orders")?,
        orders_listed: count(conn, "SELECT COUNT(*) FROM orders WHERE listed = 1")?,
        orders_by_status: by_status,
        rejects_total: count(conn, "SELECT COUNT(*) FROM rejects")?,
        fills_total: count(conn, "SELECT COUNT(*) FROM order_events WHERE kind = 'fill'")?,
        last_block_seq,
        last_block_daa,
    })
}

/// Number of KOB1 payloads that did not produce an order.
pub fn rejects_count(conn: &Connection) -> DbResult<i64> {
    match kept_counters(conn)? {
        Some(kept) => Ok(kept.get("rejects").copied().unwrap_or(0)),
        None => count(conn, "SELECT COUNT(*) FROM rejects"),
    }
}

// ---------------------------------------------------------------------------------------------
// Token holdings (`GET /v1/token-utxos`)

/// A token UTXO of a tracked token with its PROVEN state.
///
/// The state is proven when its P2SH equals the script public key of the output. The leading
/// fields are those of [`TokenUtxoView`]. `role` is `owned` for a holder that is not a KOB order.
#[derive(Debug, Clone, Serialize)]
pub struct HoldingView {
    pub txid: String,
    pub index: i64,
    pub token: String,
    /// `kcc20` or `kron`.
    pub family: &'static str,
    /// Token program template (`KCC20Ref_8x8`, `KronToken2433`, ...).
    ///
    /// It is what `tokenScriptPublicKey` and the builders take.
    pub program: String,
    pub template_hash: String,
    /// The state's 32-byte owner (x-only key, script hash or covenant id).
    pub owner: String,
    /// The owner scheme of the state.
    ///
    /// kcc20: `owner_scheme` (0 key, 4 covenant id). kron: `id_type` (0 key, 1 script hash, 2
    /// covenant id, 3 address presence).
    pub owner_kind: i64,
    /// Token base units.
    pub amount: String,
    /// KAS carrier.
    pub value: String,
    /// `owned`, `custody` or `stray`.
    pub role: String,
    /// The token state as JSON, in the shape the token state functions of kob-wasm take.
    ///
    /// The builders take the same shape.
    pub state: Value,
    pub state_hex: String,
    pub created_daa: i64,
    pub spent: bool,
    pub spent_txid: Option<String>,
    pub confirmations: Option<i64>,
    pub settled: bool,
}

/// The filters of the holdings list.
#[derive(Debug, Clone, Default)]
pub struct HoldingFilter {
    /// The state's owner (32 bytes).
    pub owner: Option<Vec<u8>>,
    pub token: Option<Hash32>,
    /// Include spent holdings.
    pub include_spent: bool,
}

/// Cursor of the holdings list: `<created_block>:<txid hex>:<index>`.
pub fn parse_holding_cursor(s: &str) -> Option<(i64, Vec<u8>, i64)> {
    let mut it = s.splitn(3, ':');
    let b = it.next()?.parse().ok()?;
    let t = hex::decode(it.next()?).ok()?;
    let i = it.next()?.parse().ok()?;
    Some((b, t, i))
}

/// Holdings, oldest first, keyset-paginated on (created block, txid, index).
pub fn holdings(
    conn: &Connection,
    ctx: &ReadCtx,
    f: &HoldingFilter,
    limit: usize,
    cursor: Option<(i64, Vec<u8>, i64)>,
) -> DbResult<Page<HoldingView>> {
    let mut conds: Vec<String> = Vec::new();
    let mut args: Vec<SqlValue> = Vec::new();
    if let Some(o) = &f.owner {
        args.push(SqlValue::Blob(o.clone()));
        conds.push(format!("x.owner_id = (SELECT id FROM ids WHERE h = ?{})", args.len()));
    }
    if let Some(t) = &f.token {
        args.push(SqlValue::Blob(t.0.to_vec()));
        // An owner's unspent holdings are read through `token_holdings_live_owner` (its live rows
        // only, in page order). The token is a filter on them (`+`: no index may take it). They
        // are never read through `token_holdings_owner` (owner, token), which holds every spent
        // row of the pair as well. Measured: `docs/ops/executor.md`, 5.5.
        let col = if f.owner.is_some() && !f.include_spent { "+x.token_id" } else { "x.token_id" };
        conds.push(format!("{col} = (SELECT id FROM ids WHERE h = ?{})", args.len()));
    }
    if !f.include_spent {
        conds.push("x.spent_block IS NULL".to_string());
    }
    if let Some((b, t, i)) = cursor {
        args.push(SqlValue::Integer(b));
        let nb = args.len();
        args.push(SqlValue::Blob(t));
        let nt = args.len();
        args.push(SqlValue::Integer(i));
        let ni = args.len();
        conds.push(format!("(x.created_block, x.txid, x.idx) > (?{nb}, ?{nt}, ?{ni})"));
    }
    args.push(SqlValue::Integer(limit as i64 + 1));
    let sql = format!(
        "SELECT x.txid, x.idx, tk.h, x.program, x.family, ow.h, x.owner_kind, x.amount, x.value, \
         kob_hstate(x.state, x.family, ow.h, x.owner_kind, x.amount), x.role, x.created_block, x.created_daa, x.spent_block, x.spent_txid \
         FROM token_holdings x CROSS JOIN ids tk ON tk.id = x.token_id CROSS JOIN ids ow ON ow.id = x.owner_id {} ORDER BY x.created_block, x.txid, x.idx LIMIT ?{}",
        if conds.is_empty() { String::new() } else { format!("WHERE {}", conds.join(" AND ")) },
        args.len()
    );
    let mut st = conn.prepare(&sql)?;
    let rows = st.query_map(params_from_iter(args.iter()), |r| {
        let program: String = r.get(3)?;
        let state: Vec<u8> = r.get(9)?;
        let daa: i64 = r.get(12)?;
        let spent_block: Option<i64> = r.get(13)?;
        let block: i64 = r.get(11)?;
        let family: i64 = r.get(4)?;
        let tpl = TemplateId::from_name(&program).filter(|t| t.is_token()).map(kob_protocol::artifacts::token_template);
        let decoded = tpl.and_then(|t| kob_protocol::state::TokenState::decode_with(t, &state).ok());
        let view = HoldingView {
            txid: hx(r.get(0)?),
            index: r.get(1)?,
            token: hx(r.get(2)?),
            family: if family == 2 { "kron" } else { "kcc20" },
            template_hash: tpl.map(|t| hex::encode(&t.hash)).unwrap_or_default(),
            program,
            owner: hx(r.get(5)?),
            owner_kind: r.get(6)?,
            amount: r.get::<_, i64>(7)?.to_string(),
            value: r.get::<_, i64>(8)?.to_string(),
            role: r.get(10)?,
            state: decoded.and_then(|s| serde_json::to_value(&s).ok()).unwrap_or(Value::Null),
            state_hex: hex::encode(&state),
            created_daa: daa,
            spent: spent_block.is_some(),
            spent_txid: hxo(r.get(14)?),
            confirmations: ctx.confirmations(daa),
            settled: ctx.settled(daa),
        };
        Ok((block, view))
    })?;
    let mut all = rows.collect::<rusqlite::Result<Vec<_>>>()?;
    let next_cursor = if all.len() > limit {
        all.truncate(limit);
        all.last().map(|(b, v)| format!("{b}:{}:{}", v.txid, v.index))
    } else {
        None
    };
    Ok(Page { items: all.into_iter().map(|(_, v)| v).collect(), next_cursor })
}

#[cfg(test)]
#[path = "reads_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "reads_scale_tests.rs"]
mod scale_tests;
