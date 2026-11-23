//! Token-logo votes: a wallet's signed thumbs up or down on a token's logo, and the totals.
//!
//! The design is in `docs/design/logo-votes.md`, the routes in `docs/api-reference.md` 3.19b.
//!
//! - `POST /v1/logo-votes` casts a vote. It is a message the wallet signs (the Kaspa
//!   personal-message standard that KasWare and Kaspire implement: BIP340 Schnorr over
//!   `PersonalMessageSigningHash`), no transaction and no fee. Any wallet may vote; it does not
//!   need to hold the token.
//! - `GET /v1/logo-votes` serves the totals per token, `up` and `down`, and the tokens the rule
//!   hides by default. Never who voted.
//! - The rule: a logo is judged once at least [`MIN_VOTERS`] addresses voted on it; it is then
//!   hidden by default when its net score (up minus down) is [`HIDE_AT`] or lower. The web app
//!   applies it, and the viewer may show every logo.
//!
//! Reports of illegal images stay separate ([`crate::logo_reports`]): free, unsigned, and they
//! hide a logo at once for the operator to review. Votes change nothing there.
//!
//! # Against one person voting as many
//!
//! 1. **History.** The voting address must have KOB history in the store at least
//!    `min_history_secs` old (default 7 days) and older than the token it votes on (founder
//!    decision 2026-11-23): an order it placed, or a token output it held, before the token's
//!    creation (its KCC-23 genesis, else the first of its outputs or orders the store holds). The
//!    read API never asks the node, so the rule reads the store: a few index seeks.
//! 2. **Work.** Each vote carries a nonce whose SHA-256 over (network, token, address, day,
//!    nonce) has `pow_bits` leading zero bits (default 20: a few seconds in a browser).
//! 3. **Interval.** One vote per wallet across all logos per `interval_secs` (default 24 hours).
//!    A vote can be changed after it. The same vote again is answered `unchanged` and uses
//!    nothing.
//! 4. **Limits.** `per_client_per_hour` per client address and `max_per_day` in all, the
//!    origins and the JSON-only body of the reports, and the guard's heavy route weight.
//!
//! Every accepted vote is kept for good with its signature (`logo_votes.sql`).

use crate::config::LogoVotesConfig;
use crate::hex::Hash32;
use crate::indexer::db::{meta_get, DbError, DbResult};
use axum::body::Bytes;
use kaspa_hashes::{HasherBase, PersonalMessageSigningHash};
use rusqlite::{params, Connection, OpenFlags, OptionalExtension, TransactionBehavior};
use serde_json::json;
use sha2::{Digest, Sha256};
use std::collections::{HashMap, VecDeque};
use std::net::IpAddr;
use std::path::Path;
use std::sync::{Arc, Mutex};

/// The tables. `db::open_writer` creates them too.
pub const SCHEMA: &str = include_str!("logo_votes.sql");
/// A logo is judged once at least this many distinct addresses voted on it.
pub const MIN_VOTERS: i64 = 3;
/// A judged logo whose net score (up minus down) is this or lower is hidden by default.
pub const HIDE_AT: i64 = -1;
/// The most work a configuration may ask of a vote, in leading zero bits.
pub const MAX_POW_BITS: u8 = 32;
/// How long `GET /v1/logo-votes` reuses a rendered answer (a vote drops it at once).
pub const LIST_CACHE_MS: i64 = 5_000;
/// The longest nonce accepted, in decimal digits (a `u64`).
pub const MAX_NONCE_DIGITS: usize = 20;

const HOUR_MS: i64 = 3_600_000;
const DAY_MS: i64 = 24 * HOUR_MS;

/// Up or down.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Vote {
    /// Thumbs up: the logo is fine.
    Up,
    /// Thumbs down: the logo should not be shown.
    Down,
}

impl Vote {
    /// `up` or `down`, as the message names it.
    pub fn as_str(self) -> &'static str {
        match self {
            Vote::Up => "up",
            Vote::Down => "down",
        }
    }

    /// 1 or -1, as stored.
    pub fn value(self) -> i64 {
        match self {
            Vote::Up => 1,
            Vote::Down => -1,
        }
    }

    fn of_value(v: i64) -> Option<Vote> {
        match v {
            1 => Some(Vote::Up),
            -1 => Some(Vote::Down),
            _ => None,
        }
    }
}

/// A vote as the API takes it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Ballot {
    /// The token whose logo is voted on.
    pub covenant_id: Hash32,
    /// Up or down.
    pub vote: Vote,
    /// The P2PK address that signed, as the wallet gave it.
    pub address: String,
    /// The UTC day the message names, `YYYY-MM-DD`.
    pub day: String,
    /// The proof-of-work nonce, decimal.
    pub nonce: String,
    /// The 64-byte BIP340 signature of [`message`].
    pub signature: [u8; 64],
}

/// The text the wallet signs. Every field is one line; the server builds it again from the
/// request, so it never reads free text.
///
/// ```text
/// KOB logo vote
/// Token: <covenant id, 64 lower-case hex>
/// Vote: up
/// Address: kaspatest:...
/// Network: testnet-10
/// Day: 2026-11-20
/// ```
pub fn message(network: &str, covenant_id: &Hash32, vote: Vote, address: &str, day: &str) -> String {
    format!(
        "KOB logo vote\nToken: {}\nVote: {}\nAddress: {address}\nNetwork: {network}\nDay: {day}",
        covenant_id.to_hex(),
        vote.as_str()
    )
}

/// The bytes the proof of work hashes. The vote's direction is not in it: the work is for one
/// vote of this address on this token on this day.
pub fn pow_input(network: &str, covenant_id: &Hash32, address: &str, day: &str, nonce: &str) -> String {
    format!("KOB logo vote pow/1\n{network}\n{}\n{address}\n{day}\n{nonce}", covenant_id.to_hex())
}

/// The leading zero bits of `h`.
pub fn leading_zero_bits(h: &[u8]) -> u32 {
    let mut n = 0;
    for b in h {
        if *b == 0 {
            n += 8;
        } else {
            return n + b.leading_zeros();
        }
    }
    n
}

/// Whether `input` hashes (SHA-256) to at least `bits` leading zero bits.
pub fn pow_ok(input: &str, bits: u8) -> bool {
    leading_zero_bits(&Sha256::digest(input.as_bytes())) >= u32::from(bits)
}

/// The Kaspa personal-message hash of `msg`: BLAKE2b-256 keyed with `PersonalMessageSigningHash`.
pub fn personal_message_hash(msg: &str) -> [u8; 32] {
    let mut h = PersonalMessageSigningHash::new();
    h.update(msg.as_bytes());
    h.finalize().as_bytes()
}

/// BIP340 verification of `sig` over the personal-message hash of `msg` by the x-only `key`.
pub fn verify_signature(msg: &str, sig: &[u8], key: &[u8; 32]) -> bool {
    let (Ok(sig), Ok(key)) = (secp256k1::schnorr::Signature::from_slice(sig), secp256k1::XOnlyPublicKey::from_slice(key)) else {
        return false;
    };
    let digest = secp256k1::Message::from_digest(personal_message_hash(msg));
    secp256k1::SECP256K1.verify_schnorr(&sig, &digest, &key).is_ok()
}

/// The x-only key of a Schnorr P2PK `address` of `network`.
pub fn voter_key(address: &str, network: &str) -> Result<[u8; 32], VoteError> {
    let a = kaspa_addresses::Address::try_from(address).map_err(|_| VoteError::Address("not a Kaspa address"))?;
    if a.prefix != crate::script::prefix_for_network(network) {
        return Err(VoteError::Address("the address is of another network"));
    }
    if a.version != kaspa_addresses::Version::PubKey {
        return Err(VoteError::Address("only a Schnorr key address (P2PK) can vote"));
    }
    <[u8; 32]>::try_from(&a.payload[..]).map_err(|_| VoteError::Address("not a Kaspa address"))
}

/// The UTC day of `ms` (milliseconds since the Unix epoch), `YYYY-MM-DD`.
pub fn utc_day(ms: i64) -> String {
    // days to the civil date (H. Hinnant)
    let z = ms.div_euclid(DAY_MS) + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!("{y:04}-{m:02}-{d:02}")
}

/// Whether the rule hides a logo with these totals by default: judged (at least [`MIN_VOTERS`]
/// voters) and a net score of [`HIDE_AT`] or lower.
pub fn hidden_by_votes(up: i64, down: i64) -> bool {
    up + down >= MIN_VOTERS && up - down <= HIDE_AT
}

// ------------------------------------------------------------------------------------- the store

/// Opens the database read-write for the vote store, like [`crate::logo_reports::open`].
pub fn open(path: &Path, network: &str) -> DbResult<Connection> {
    if !path.exists() {
        return Err(DbError::Invalid(format!("{} does not exist", path.display())));
    }
    let conn = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_NO_MUTEX)?;
    conn.busy_timeout(std::time::Duration::from_secs(5))?;
    let net = meta_get(&conn, "network")?.unwrap_or_default();
    if net != network {
        return Err(DbError::NetworkMismatch { found: net, expected: network.to_string() });
    }
    conn.execute_batch(SCHEMA)?;
    Ok(conn)
}

/// How old the KOB history of `key` is, in seconds: from the first order it placed or the first
/// token output it held, to the newest block of the store. `None`: it has none.
///
/// Two seeks of the maker and the owner indexes. A pruned row only makes the history look
/// younger.
pub fn history_age_secs(conn: &Connection, key: &[u8; 32], daa_per_second: u64) -> DbResult<Option<u64>> {
    let Some(first) = first_history_daa(conn, key)? else { return Ok(None) };
    Ok(Some((store_tip_daa(conn)?.saturating_sub(first).max(0) as u64) / daa_per_second.max(1)))
}

/// The DAA score of the newest block of the store.
fn store_tip_daa(conn: &Connection) -> DbResult<i64> {
    Ok(conn.prepare_cached("SELECT COALESCE(MAX(daa), 0) FROM blocks")?.query_row([], |r| r.get(0))?)
}

/// The DAA score of the first KOB history of `key`: the first order it placed or the first token
/// output it held. `None`: it has none.
pub fn first_history_daa(conn: &Connection, key: &[u8; 32]) -> DbResult<Option<i64>> {
    let Some(id) = conn.prepare_cached("SELECT id FROM ids WHERE h = ?1")?.query_row([&key[..]], |r| r.get::<_, i64>(0)).optional()?
    else {
        return Ok(None);
    };
    let order: Option<i64> = conn
        .prepare_cached("SELECT genesis_daa FROM orders WHERE maker_id = ?1 ORDER BY genesis_block, covenant_id LIMIT 1")?
        .query_row([id], |r| r.get(0))
        .optional()?;
    let held: Option<i64> = conn
        .prepare_cached("SELECT created_daa FROM token_holdings WHERE owner_id = ?1 ORDER BY created_block LIMIT 1")?
        .query_row([id], |r| r.get(0))
        .optional()?;
    Ok([order, held].into_iter().flatten().min())
}

/// The DAA score the token `id` was created at, as far as the store knows: its KCC-23 genesis, else
/// the first of its token outputs or of the orders that trade it. `None`: the store knows none.
///
/// Index seeks only. A pruned row only makes the token look younger.
pub fn token_created_daa(conn: &Connection, id: &Hash32) -> DbResult<Option<i64>> {
    let declared: Option<i64> = conn
        .prepare_cached("SELECT genesis_daa FROM kcc23_declarations WHERE covenant_id = ?1")?
        .query_row([&id.0[..]], |r| r.get(0))
        .optional()?;
    let Some(tid) =
        conn.prepare_cached("SELECT id FROM ids WHERE h = ?1")?.query_row([&id.0[..]], |r| r.get::<_, i64>(0)).optional()?
    else {
        return Ok(declared);
    };
    let first = |sql: &str| -> DbResult<Option<i64>> { Ok(conn.prepare_cached(sql)?.query_row([tid], |r| r.get(0)).optional()?) };
    let held = first("SELECT created_daa FROM token_holdings WHERE token_id = ?1 ORDER BY created_block LIMIT 1")?;
    let traded = first("SELECT genesis_daa FROM orders WHERE token_id = ?1 ORDER BY genesis_block, covenant_id LIMIT 1")?;
    let quoted = first("SELECT genesis_daa FROM orders WHERE quote_id = ?1 ORDER BY genesis_block, covenant_id LIMIT 1")?;
    Ok([declared, held, traded, quoted].into_iter().flatten().min())
}

/// The newest vote of `key` on `id`, if any.
pub fn current_vote(conn: &Connection, id: &Hash32, key: &[u8; 32]) -> DbResult<Option<Vote>> {
    Ok(conn
        .prepare_cached("SELECT vote FROM logo_votes WHERE covenant_id = ?1 AND voter = ?2 ORDER BY id DESC LIMIT 1")?
        .query_row(params![&id.0[..], &key[..]], |r| r.get::<_, i64>(0))
        .optional()?
        .and_then(Vote::of_value))
}

/// When `key` last voted on any logo, milliseconds since the Unix epoch.
pub fn last_vote_ms(conn: &Connection, key: &[u8; 32]) -> DbResult<Option<i64>> {
    Ok(conn.prepare_cached("SELECT MAX(created_ms) FROM logo_votes WHERE voter = ?1")?.query_row([&key[..]], |r| r.get(0))?)
}

/// What a vote did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Cast {
    /// The first vote of this address on this logo.
    Counted,
    /// The address changed its vote on this logo.
    Changed,
    /// The address had cast this vote already: nothing was written and the interval is not used.
    Unchanged,
}

impl Cast {
    /// The lowercase name of the outcome.
    pub fn as_str(self) -> &'static str {
        match self {
            Cast::Counted => "counted",
            Cast::Changed => "changed",
            Cast::Unchanged => "unchanged",
        }
    }
}

/// Writes `b` by `key` at `now_ms` in one transaction and keeps the totals. It checks nothing but
/// the voter's current vote: the service checks the rest first.
pub fn record(conn: &mut Connection, b: &Ballot, key: &[u8; 32], now_ms: i64) -> DbResult<Cast> {
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let before = current_vote(&tx, &b.covenant_id, key)?;
    let cast = match before {
        Some(v) if v == b.vote => return Ok(Cast::Unchanged),
        Some(_) => Cast::Changed,
        None => Cast::Counted,
    };
    let id = &b.covenant_id.0[..];
    tx.execute(
        "INSERT INTO logo_votes (covenant_id, voter, vote, day, nonce, signature, created_ms, outcome) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
        params![id, &key[..], b.vote.value(), b.day, b.nonce, &b.signature[..], now_ms, cast.as_str()],
    )?;
    let (up, down) = match (before, b.vote) {
        (None, Vote::Up) => (1, 0),
        (None, Vote::Down) => (0, 1),
        (Some(_), Vote::Up) => (1, -1),
        (Some(_), Vote::Down) => (-1, 1),
    };
    tx.execute(
        "INSERT INTO logo_vote_totals (covenant_id, up, down, updated_ms) VALUES (?1, ?2, ?3, ?4) \
         ON CONFLICT (covenant_id) DO UPDATE SET up = up + ?2, down = down + ?3, updated_ms = ?4",
        params![id, up, down, now_ms],
    )?;
    tx.commit()?;
    Ok(cast)
}

/// One token's totals: the covenant id (hex), up and down.
pub type TokenTotals = (String, i64, i64);

/// The totals of every token voted on, sorted by covenant id, and when they last changed.
pub fn totals(conn: &Connection) -> DbResult<(Vec<TokenTotals>, i64)> {
    let mut q = conn.prepare("SELECT covenant_id, up, down FROM logo_vote_totals WHERE up + down > 0 ORDER BY covenant_id")?;
    let rows = q
        .query_map([], |r| Ok((crate::hex::encode(&r.get::<_, Vec<u8>>(0)?), r.get::<_, i64>(1)?, r.get::<_, i64>(2)?)))?
        .collect::<Result<Vec<_>, _>>()?;
    let updated: i64 = conn.query_row("SELECT COALESCE(MAX(updated_ms), 0) FROM logo_vote_totals", [], |r| r.get(0))?;
    Ok((rows, updated))
}

/// Every vote row as JSON, oldest first, for a database that is rebuilt (`kobd db replay`). The
/// totals are rebuilt from them.
pub fn export(conn: &Connection) -> DbResult<serde_json::Value> {
    let mut q =
        conn.prepare("SELECT covenant_id, voter, vote, day, nonce, signature, created_ms, outcome FROM logo_votes ORDER BY id")?;
    let rows = q
        .query_map([], |r| {
            Ok(json!({
                "covenant_id": crate::hex::encode(&r.get::<_, Vec<u8>>(0)?),
                "voter": crate::hex::encode(&r.get::<_, Vec<u8>>(1)?),
                "vote": r.get::<_, i64>(2)?,
                "day": r.get::<_, String>(3)?,
                "nonce": r.get::<_, String>(4)?,
                "signature": crate::hex::encode(&r.get::<_, Vec<u8>>(5)?),
                "created_ms": r.get::<_, i64>(6)?,
                "outcome": r.get::<_, String>(7)?,
            }))
        })?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(serde_json::Value::Array(rows))
}

/// Loads an [`export`] into an empty vote store and rebuilds the totals. Returns the rows loaded.
pub fn import(conn: &mut Connection, rows: &serde_json::Value) -> Result<usize, String> {
    let rows = rows.as_array().ok_or("`votes` is not a list")?;
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate).map_err(|e| e.to_string())?;
    let n: i64 = tx.query_row("SELECT COUNT(*) FROM logo_votes", [], |r| r.get(0)).map_err(|e| e.to_string())?;
    if n != 0 {
        return Err("the vote store is not empty; import loads an export into a rebuilt database only".into());
    }
    let bytes = |v: &serde_json::Value, k: &str, len: usize| -> Result<Vec<u8>, String> {
        let s = v.get(k).and_then(|x| x.as_str()).ok_or_else(|| format!("a vote has no `{k}`"))?;
        let b = crate::hex::decode(s).map_err(|e| format!("`{k}`: {e}"))?;
        if b.len() != len {
            return Err(format!("`{k}` is not {len} bytes"));
        }
        Ok(b)
    };
    let text = |v: &serde_json::Value, k: &str| {
        v.get(k).and_then(|x| x.as_str()).map(str::to_string).ok_or_else(|| format!("a vote has no `{k}`"))
    };
    let int = |v: &serde_json::Value, k: &str| v.get(k).and_then(|x| x.as_i64()).ok_or_else(|| format!("a vote has no integer `{k}`"));
    for v in rows {
        tx.execute(
            "INSERT INTO logo_votes (covenant_id, voter, vote, day, nonce, signature, created_ms, outcome) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![
                bytes(v, "covenant_id", 32)?,
                bytes(v, "voter", 32)?,
                int(v, "vote")?,
                text(v, "day")?,
                text(v, "nonce")?,
                bytes(v, "signature", 64)?,
                int(v, "created_ms")?,
                text(v, "outcome")?
            ],
        )
        .map_err(|e| e.to_string())?;
    }
    rebuild_totals(&tx).map_err(|e| e.to_string())?;
    tx.commit().map_err(|e| e.to_string())?;
    Ok(rows.len())
}

/// The totals counted afresh from the newest vote of each voter on each token.
pub fn rebuild_totals(conn: &Connection) -> rusqlite::Result<()> {
    conn.execute_batch(
        "DELETE FROM logo_vote_totals;
         INSERT INTO logo_vote_totals (covenant_id, up, down, updated_ms)
           SELECT covenant_id, SUM(vote = 1), SUM(vote = -1), MAX(at) FROM (
             SELECT v.covenant_id, v.vote, (SELECT MAX(created_ms) FROM logo_votes x WHERE x.covenant_id = v.covenant_id) AS at
             FROM logo_votes v
             WHERE v.id = (SELECT MAX(id) FROM logo_votes w WHERE w.covenant_id = v.covenant_id AND w.voter = v.voter)
           ) GROUP BY covenant_id;",
    )
}

// ---------------------------------------------------------------- the service behind the routes

/// Why a vote was not accepted.
#[derive(Debug)]
pub enum VoteError {
    /// The day is neither today nor yesterday (UTC).
    StaleDay,
    /// The nonce does not carry the work asked for.
    BadPow,
    /// The signature does not verify for the address and the message.
    BadSignature,
    /// The address cannot vote, with why.
    Address(&'static str),
    /// The covenant id is not a token the indexer knows.
    UnknownToken,
    /// The address has no KOB history old enough, or none from before the token was created.
    NoHistory,
    /// The wallet voted less than the interval ago; it may vote again after this many seconds.
    TooSoon(u64),
    /// Over a client or daily limit; retry after this many seconds.
    Limited(u64),
    /// A database error.
    Db(DbError),
}

impl From<DbError> for VoteError {
    fn from(e: DbError) -> Self {
        VoteError::Db(e)
    }
}

impl From<rusqlite::Error> for VoteError {
    fn from(e: rusqlite::Error) -> Self {
        VoteError::Db(e.into())
    }
}

/// Sliding-window limits: `per_client` per client key per hour and `per_day` in all. Only
/// admitted votes are remembered.
struct Limiter {
    clients: HashMap<IpAddr, VecDeque<i64>>,
    day: VecDeque<i64>,
}

impl Limiter {
    fn new() -> Self {
        Limiter { clients: HashMap::new(), day: VecDeque::new() }
    }

    fn admit(&mut self, key: IpAddr, now: i64, per_client: u32, per_day: u32) -> Result<(), u64> {
        while self.day.front().is_some_and(|t| now - t >= DAY_MS) {
            self.day.pop_front();
        }
        self.clients.retain(|_, q| {
            while q.front().is_some_and(|t| now - t >= HOUR_MS) {
                q.pop_front();
            }
            !q.is_empty()
        });
        let secs = |ms: i64| (ms.max(0) as u64).div_ceil(1000).max(1);
        if self.day.len() >= per_day as usize {
            return Err(secs(self.day.front().map_or(DAY_MS, |t| t + DAY_MS - now)));
        }
        let q = self.clients.get(&key);
        if q.map_or(0, VecDeque::len) >= per_client as usize {
            return Err(secs(q.and_then(|q| q.front()).map_or(HOUR_MS, |t| t + HOUR_MS - now)));
        }
        self.clients.entry(key).or_default().push_back(now);
        self.day.push_back(now);
        Ok(())
    }
}

/// The result of an accepted vote: what it did and the token's totals after it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Accepted {
    /// What the vote did.
    pub cast: Cast,
    /// Up votes of the token now.
    pub up: i64,
    /// Down votes of the token now.
    pub down: i64,
}

/// The vote store as the API uses it: one connection behind a mutex, the limits and the rendered
/// totals.
pub struct LogoVotes {
    conn: Mutex<Connection>,
    cfg: LogoVotesConfig,
    network: String,
    daa_per_second: u64,
    limiter: Mutex<Limiter>,
    cache: Mutex<Option<(i64, Bytes)>>,
    now_ms: Box<dyn Fn() -> i64 + Send + Sync>,
    ipv6_prefix_bits: u8,
}

impl LogoVotes {
    /// Opens the vote store at `path` for `network`, with the system clock.
    pub fn open(path: &Path, network: &str, cfg: LogoVotesConfig, daa_per_second: u64, ipv6_prefix_bits: u8) -> DbResult<Arc<Self>> {
        Ok(Arc::new(Self::with_connection(
            open(path, network)?,
            network,
            cfg,
            daa_per_second,
            ipv6_prefix_bits,
            Box::new(crate::logo_reports::system_now_ms),
        )))
    }

    /// Builds the service over an open connection and a clock, for tests that fake the time.
    pub fn with_connection(
        conn: Connection,
        network: &str,
        cfg: LogoVotesConfig,
        daa_per_second: u64,
        ipv6_prefix_bits: u8,
        now_ms: Box<dyn Fn() -> i64 + Send + Sync>,
    ) -> Self {
        LogoVotes {
            conn: Mutex::new(conn),
            cfg,
            network: network.to_string(),
            daa_per_second,
            limiter: Mutex::new(Limiter::new()),
            cache: Mutex::new(None),
            now_ms,
            ipv6_prefix_bits,
        }
    }

    /// The origins whose pages may vote (`[logo_votes] origins`, filled from `[logo_reports]`
    /// when empty).
    pub fn origins(&self) -> &[String] {
        &self.cfg.origins
    }

    /// The work a vote must carry, in leading zero bits.
    pub fn pow_bits(&self) -> u8 {
        self.cfg.pow_bits.min(MAX_POW_BITS)
    }

    /// Casts `b` from `client` (the guard's client address). The checks run cheapest first: the
    /// day, the work, the signature, then the store (the token, the history, the interval) and
    /// the limits.
    ///
    /// It is blocking, because it writes to the database. Call it from a blocking task.
    pub fn vote(&self, client: IpAddr, b: &Ballot) -> Result<Accepted, VoteError> {
        let now = (self.now_ms)();
        if b.day != utc_day(now) && b.day != utc_day(now - DAY_MS) {
            return Err(VoteError::StaleDay);
        }
        let key = voter_key(&b.address, &self.network)?;
        if !pow_ok(&pow_input(&self.network, &b.covenant_id, &b.address, &b.day, &b.nonce), self.pow_bits()) {
            return Err(VoteError::BadPow);
        }
        let msg = message(&self.network, &b.covenant_id, b.vote, &b.address, &b.day);
        if !verify_signature(&msg, &b.signature, &key) {
            return Err(VoteError::BadSignature);
        }
        let mut conn = self.conn.lock().unwrap_or_else(|e| e.into_inner());
        if !crate::logo_reports::token_indexed(&conn, &b.covenant_id)? {
            return Err(VoteError::UnknownToken);
        }
        let totals_of = |c: &Connection| -> DbResult<(i64, i64)> {
            Ok(c.prepare_cached("SELECT up, down FROM logo_vote_totals WHERE covenant_id = ?1")?
                .query_row([&b.covenant_id.0[..]], |r| Ok((r.get(0)?, r.get(1)?)))
                .optional()?
                .unwrap_or((0, 0)))
        };
        // The same vote again uses nothing and says so.
        if current_vote(&conn, &b.covenant_id, &key)? == Some(b.vote) {
            let (up, down) = totals_of(&conn)?;
            return Ok(Accepted { cast: Cast::Unchanged, up, down });
        }
        // The history rule (founder decision 2026-11-23, audit C-M3): the address's first KOB
        // history is at least `min_history_secs` old (default 7 days) AND older than the token,
        // so keys made for a vote, or after the token they vote on, do not count.
        let first = first_history_daa(&conn, &key)?;
        let tip = store_tip_daa(&conn)?;
        let old = |f: i64| (tip.saturating_sub(f).max(0) as u64) / self.daa_per_second.max(1);
        let created = token_created_daa(&conn, &b.covenant_id)?;
        let old_enough = first.is_some_and(|f| old(f) >= self.cfg.min_history_secs && created.is_some_and(|t| f < t));
        if !old_enough {
            return Err(VoteError::NoHistory);
        }
        let interval_ms = i64::try_from(self.cfg.interval_secs).unwrap_or(i64::MAX / 2).saturating_mul(1000);
        if let Some(last) = last_vote_ms(&conn, &key)? {
            let wait = last.saturating_add(interval_ms) - now;
            if wait > 0 {
                return Err(VoteError::TooSoon((wait as u64).div_ceil(1000)));
            }
        }
        let ip = crate::api::client_ip::rate_key(client, self.ipv6_prefix_bits);
        self.limiter
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .admit(ip, now, self.cfg.per_client_per_hour, self.cfg.max_per_day)
            .map_err(VoteError::Limited)?;
        let cast = record(&mut conn, b, &key, now)?;
        let (up, down) = totals_of(&conn)?;
        drop(conn);
        *self.cache.lock().unwrap_or_else(|e| e.into_inner()) = None;
        Ok(Accepted { cast, up, down })
    }

    /// The service's connection, for tests.
    #[cfg(test)]
    pub(crate) fn test_connection(&self) -> std::sync::MutexGuard<'_, Connection> {
        self.conn.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// The JSON body of `GET /v1/logo-votes`, rendered at most every [`LIST_CACHE_MS`].
    ///
    /// `{"logos":[{"covenant_id","up","down"}],"hidden":[ids],"min_voters":3,"hide_at":-1,
    /// "pow_bits":20,"interval_secs":86400,"updated_ms":..}`. It is blocking.
    pub fn totals_body(&self) -> DbResult<Bytes> {
        let now = (self.now_ms)();
        if let Some((at, body)) = self.cache.lock().unwrap_or_else(|e| e.into_inner()).as_ref() {
            if now - at < LIST_CACHE_MS {
                return Ok(body.clone());
            }
        }
        let (rows, updated_ms) = totals(&self.conn.lock().unwrap_or_else(|e| e.into_inner()))?;
        let hidden: Vec<&str> = rows.iter().filter(|(_, u, d)| hidden_by_votes(*u, *d)).map(|(id, _, _)| id.as_str()).collect();
        let logos: Vec<serde_json::Value> =
            rows.iter().map(|(id, up, down)| json!({"covenant_id": id, "up": up, "down": down})).collect();
        let body = Bytes::from(
            serde_json::to_vec(&json!({
                "logos": logos,
                "hidden": hidden,
                "min_voters": MIN_VOTERS,
                "hide_at": HIDE_AT,
                "pow_bits": self.pow_bits(),
                "interval_secs": self.cfg.interval_secs,
                "updated_ms": updated_ms,
            }))
            .map_err(|e| DbError::Invalid(e.to_string()))?,
        );
        *self.cache.lock().unwrap_or_else(|e| e.into_inner()) = Some((now, body.clone()));
        Ok(body)
    }
}

#[cfg(test)]
pub(crate) mod testing {
    //! Signing and the work for tests (the API's too).
    use super::*;

    /// The x-only key and a P2PK testnet address of secret `n`.
    pub fn account(n: u8) -> (secp256k1::Keypair, [u8; 32], String) {
        let mut sk = [0u8; 32];
        sk[31] = n;
        sk[0] = 0x11;
        let kp = secp256k1::Keypair::from_seckey_slice(secp256k1::SECP256K1, &sk).unwrap();
        let key = kp.x_only_public_key().0.serialize();
        let addr = kaspa_addresses::Address::new(kaspa_addresses::Prefix::Testnet, kaspa_addresses::Version::PubKey, &key).to_string();
        (kp, key, addr)
    }

    /// A nonce that carries `bits` of work.
    pub fn solve(network: &str, id: &Hash32, address: &str, day: &str, bits: u8) -> String {
        (0u64..).map(|n| n.to_string()).find(|n| pow_ok(&pow_input(network, id, address, day, n), bits)).unwrap()
    }

    /// A signed ballot of secret `n`.
    pub fn ballot(n: u8, id: Hash32, vote: Vote, day: &str, bits: u8) -> Ballot {
        let (kp, _, address) = account(n);
        let msg = message("testnet-10", &id, vote, &address, day);
        let digest = secp256k1::Message::from_digest(personal_message_hash(&msg));
        let sig = secp256k1::SECP256K1.sign_schnorr_no_aux_rand(&digest, &kp);
        let nonce = solve("testnet-10", &id, &address, day, bits);
        Ballot { covenant_id: id, vote, address, day: day.to_string(), nonce, signature: *sig.as_ref() }
    }

    /// Gives `key` KOB history: an order it placed at `daa`, and the store's tip at `tip`.
    pub fn give_history(c: &Connection, key: &[u8; 32], daa: i64, tip: i64, n: u8) {
        c.execute("INSERT OR IGNORE INTO ids (h) VALUES (?1)", [&key[..]]).unwrap();
        let id: i64 = c.query_row("SELECT id FROM ids WHERE h = ?1", [&key[..]], |r| r.get(0)).unwrap();
        c.execute(
            "INSERT INTO orders (covenant_id, contract, template_id, family, side, maker_id, in_book, genesis_state, genesis_block, genesis_daa, listed) \
             VALUES (?1, 'KobAsk', 1, 1, 1, ?2, 0, x'00', 1, ?3, 1)",
            params![vec![n; 32], id, daa],
        )
        .unwrap();
        c.execute("INSERT OR IGNORE INTO blocks (hash, daa) VALUES (?1, ?2)", params![vec![0xb0u8; 32], tip]).unwrap();
        c.execute("UPDATE blocks SET daa = MAX(daa, ?1)", [tip]).unwrap();
    }
}

#[cfg(test)]
mod tests {
    use super::testing::*;
    use super::*;
    use crate::indexer::db::open_memory;
    use std::sync::atomic::{AtomicI64, Ordering};

    const T: Hash32 = Hash32([0x7a; 32]);
    const U: Hash32 = Hash32([0x7b; 32]);
    /// 2026-11-20 12:00 UTC.
    const NOW: i64 = 1_795_176_000_000;
    const DAY: &str = "2026-11-20";
    /// The store's tip in the service tests: eight days of DAA (10 a second).
    const TIP: i64 = 8 * 864_000;
    /// The tokens' genesis in the service tests, after the voters' first history (DAA 0).
    const TOKEN_DAA: i64 = 10;

    fn hex_bytes(s: &str) -> Vec<u8> {
        crate::hex::decode(s).unwrap()
    }

    #[test]
    fn the_kaspa_personal_message_vector_verifies_and_a_changed_message_does_not() {
        // rusty-kaspa wallet/core/src/message.rs, test_sign_and_verify_test_case_0 (KIP vectors)
        let key: [u8; 32] = hex_bytes("f9308a019258c31049344f85f89d5229b531c845836f99b08601f113bce036f9").try_into().unwrap();
        let sig = hex_bytes(concat!(
            "40b9bb2be0ae02607279eda64015a8d86e3763279170340b8243f7ce5344d77a",
            "ff1191598baf2fd26149cac3b4b12c2c433261c00834db6098cb172aa48ef522"
        ));
        assert!(verify_signature("Hello Kaspa!", &sig, &key));
        assert!(!verify_signature("Hello Kaspa?", &sig, &key));
        assert!(!verify_signature("Hello Kaspa!", &sig[..63], &key));
    }

    #[test]
    fn a_signature_verifies_for_its_own_address_only() {
        let b = ballot(1, T, Vote::Up, DAY, 0);
        let (_, key, address) = account(1);
        assert_eq!(voter_key(&address, "testnet-10").unwrap(), key);
        let msg = message("testnet-10", &T, Vote::Up, &address, DAY);
        assert!(verify_signature(&msg, &b.signature, &key), "good");
        // a bad signature: one bit flipped
        let mut bad = b.signature;
        bad[10] ^= 1;
        assert!(!verify_signature(&msg, &bad, &key), "bad");
        // the same signature presented for another address
        let (_, other, other_addr) = account(2);
        assert!(!verify_signature(&message("testnet-10", &T, Vote::Up, &other_addr, DAY), &b.signature, &other), "wrong address");
        // or another vote, token or day
        assert!(!verify_signature(&message("testnet-10", &T, Vote::Down, &address, DAY), &b.signature, &key));
        assert!(!verify_signature(&message("testnet-10", &U, Vote::Up, &address, DAY), &b.signature, &key));
        assert!(!verify_signature(&message("testnet-10", &T, Vote::Up, &address, "2026-11-21"), &b.signature, &key));
        // addresses that cannot vote
        assert!(matches!(voter_key(&address, "mainnet"), Err(VoteError::Address(_))), "another network");
        let p2sh =
            kaspa_addresses::Address::new(kaspa_addresses::Prefix::Testnet, kaspa_addresses::Version::ScriptHash, &key).to_string();
        assert!(matches!(voter_key(&p2sh, "testnet-10"), Err(VoteError::Address(_))), "a script hash");
        assert!(matches!(voter_key("kaspatest:nope", "testnet-10"), Err(VoteError::Address(_))));
    }

    #[test]
    fn the_message_has_one_field_per_line() {
        let m = message("testnet-10", &T, Vote::Down, "kaspatest:qq", DAY);
        assert_eq!(
            m,
            format!("KOB logo vote\nToken: {}\nVote: down\nAddress: kaspatest:qq\nNetwork: testnet-10\nDay: 2026-11-20", T.to_hex())
        );
    }

    #[test]
    fn the_work_counts_leading_zero_bits() {
        assert_eq!(leading_zero_bits(&[0, 0, 0x1f, 0xff]), 19);
        assert_eq!(leading_zero_bits(&[0x80]), 0);
        assert_eq!(leading_zero_bits(&[0; 4]), 32);
        let n = solve("testnet-10", &T, "kaspatest:qq", DAY, 12);
        assert!(pow_ok(&pow_input("testnet-10", &T, "kaspatest:qq", DAY, &n), 12));
        // the nonce is bound to the token, the address and the day
        let again = |id: &Hash32, a: &str, d: &str| (0..64).all(|_| pow_ok(&pow_input("testnet-10", id, a, d, &n), 12));
        assert!(!again(&U, "kaspatest:qq", DAY) || !again(&T, "kaspatest:qr", DAY) || !again(&T, "kaspatest:qq", "2026-11-19"));
        assert!(pow_ok("anything", 0));
    }

    #[test]
    fn utc_days() {
        assert_eq!(utc_day(0), "1970-01-01");
        assert_eq!(utc_day(NOW), DAY);
        assert_eq!(utc_day(951_782_400_000), "2000-02-29");
        assert_eq!(utc_day(NOW - DAY_MS), "2026-11-19");
    }

    #[test]
    fn a_logo_is_judged_from_three_voters_and_hidden_at_minus_one() {
        assert!(!hidden_by_votes(0, 2), "two voters: not judged");
        assert!(!hidden_by_votes(0, 0));
        assert!(hidden_by_votes(1, 2), "three voters, net -1");
        assert!(hidden_by_votes(0, 3));
        assert!(!hidden_by_votes(2, 1), "net +1");
        assert!(!hidden_by_votes(2, 2), "net 0 shows");
        assert!(hidden_by_votes(3, 4));
    }

    #[test]
    fn the_history_rule_reads_the_first_order_or_holding_against_the_tip() {
        let c = open_memory("testnet-10").unwrap();
        let (_, key, _) = account(3);
        assert_eq!(history_age_secs(&c, &key, 10).unwrap(), None, "unknown key");
        give_history(&c, &key, 1_000, 1_000 + 864_000, 1);
        assert_eq!(history_age_secs(&c, &key, 10).unwrap(), Some(86_400));
        // a token output it held earlier counts from then
        let (_, other, _) = account(4);
        c.execute("INSERT INTO ids (h) VALUES (?1)", [&other[..]]).unwrap();
        let oid: i64 = c.query_row("SELECT id FROM ids WHERE h = ?1", [&other[..]], |r| r.get(0)).unwrap();
        c.execute("INSERT OR IGNORE INTO programs (id, name) VALUES (1, 'KCC20Ref_8x8')", []).unwrap();
        c.execute(
            "INSERT INTO token_holdings (tx_id, idx, token_id, program_id, family, owner_id, owner_kind, amount, value, state, role_code, created_block, created_daa) \
             VALUES (1, 0, 1, 1, 1, ?1, 0, 5, 1000, x'00', 0, 1, 864000)",
            [oid],
        )
        .unwrap();
        assert_eq!(history_age_secs(&c, &other, 10).unwrap(), Some(100), "one holding, 1 000 DAA before the tip");
    }

    fn service(cfg: LogoVotesConfig) -> (LogoVotes, Arc<AtomicI64>) {
        let now = Arc::new(AtomicI64::new(NOW));
        let clock = now.clone();
        let conn = open_memory("testnet-10").unwrap();
        for id in [T, U] {
            conn.execute(
                "INSERT INTO kcc23_declarations (covenant_id, proof, preparation_txid, genesis_txid, genesis_block, genesis_daa) \
                 VALUES (?1, 'x', zeroblob(32), zeroblob(32), 0, ?2)",
                params![&id.0[..], TOKEN_DAA],
            )
            .unwrap();
        }
        // accounts 1..=9 have eight days of history, from before the tokens
        for n in 1..=9u8 {
            give_history(&conn, &account(n).1, 0, TIP, n);
        }
        let svc = LogoVotes::with_connection(conn, "testnet-10", cfg, 10, 64, Box::new(move || clock.load(Ordering::SeqCst)));
        (svc, now)
    }

    fn cfg() -> LogoVotesConfig {
        LogoVotesConfig { enabled: true, pow_bits: 8, ..Default::default() }
    }

    fn ip(n: u8) -> IpAddr {
        IpAddr::from([10, 0, 0, n])
    }

    #[test]
    fn a_vote_is_checked_day_work_signature_token_history_then_counted() {
        let (svc, _) = service(cfg());
        // the day must be today or yesterday
        assert!(matches!(svc.vote(ip(1), &ballot(1, T, Vote::Up, "2026-11-18", 8)), Err(VoteError::StaleDay)));
        assert!(matches!(svc.vote(ip(1), &ballot(1, T, Vote::Up, "2026-11-21", 8)), Err(VoteError::StaleDay)));
        // the work
        let mut b = ballot(1, T, Vote::Up, DAY, 8);
        let good_nonce = b.nonce.clone();
        b.nonce = (0u64..).map(|n| n.to_string()).find(|n| !pow_ok(&pow_input("testnet-10", &T, &b.address, DAY, n), 8)).unwrap();
        assert!(matches!(svc.vote(ip(1), &b), Err(VoteError::BadPow)));
        // the signature
        b.nonce = good_nonce;
        b.signature[0] ^= 1;
        assert!(matches!(svc.vote(ip(1), &b), Err(VoteError::BadSignature)));
        // an unknown token
        assert!(matches!(svc.vote(ip(1), &ballot(1, Hash32([9; 32]), Vote::Up, DAY, 8)), Err(VoteError::UnknownToken)));
        // an address with no history, or too young
        assert!(matches!(svc.vote(ip(1), &ballot(20, T, Vote::Up, DAY, 8)), Err(VoteError::NoHistory)));
        give_history(&svc.test_connection(), &account(21).1, TIP - 10, TIP, 21);
        assert!(matches!(svc.vote(ip(1), &ballot(21, T, Vote::Up, DAY, 8)), Err(VoteError::NoHistory)), "one second of history");
        // six days of history is not seven
        give_history(&svc.test_connection(), &account(22).1, TIP - 6 * 864_000, TIP, 22);
        assert!(matches!(svc.vote(ip(1), &ballot(22, T, Vote::Up, DAY, 8)), Err(VoteError::NoHistory)), "six days");
        // good: counted, and yesterday's day is still accepted
        let a = svc.vote(ip(1), &ballot(1, T, Vote::Up, "2026-11-19", 8)).unwrap();
        assert_eq!(a, Accepted { cast: Cast::Counted, up: 1, down: 0 });
    }

    /// Founder decision 2026-11-23 (audit C-M3): a vote counts only from an address whose first history is at least 7
    /// days old AND older than the token. Fresh dust sent to new keys, or keys whose first output came after the token
    /// was created, do not vote however old they are; an old address does.
    #[test]
    fn only_an_address_older_than_the_token_and_seven_days_votes() {
        let (svc, _) = service(cfg());
        // fresh dust: a one-unit token output to a new key a minute ago
        let (_, fresh, _) = account(30);
        let c = svc.test_connection();
        c.execute("INSERT INTO ids (h) VALUES (?1)", [&fresh[..]]).unwrap();
        let fid: i64 = c.query_row("SELECT id FROM ids WHERE h = ?1", [&fresh[..]], |r| r.get(0)).unwrap();
        c.execute("INSERT OR IGNORE INTO programs (id, name) VALUES (1, 'KCC20Ref_8x8')", []).unwrap();
        c.execute(
            "INSERT INTO token_holdings (tx_id, idx, token_id, program_id, family, owner_id, owner_kind, amount, value, state, role_code, created_block, created_daa) \
             VALUES (1, 0, 1, 1, 1, ?1, 0, 1, 1000, x'00', 0, 1, ?2)",
            params![fid, TIP - 600],
        )
        .unwrap();
        drop(c);
        assert!(matches!(svc.vote(ip(1), &ballot(30, T, Vote::Down, DAY, 8)), Err(VoteError::NoHistory)), "fresh dust");
        // eight days old, but first seen after the token's genesis
        give_history(&svc.test_connection(), &account(31).1, TOKEN_DAA + 1, TIP, 31);
        assert!(matches!(svc.vote(ip(1), &ballot(31, T, Vote::Down, DAY, 8)), Err(VoteError::NoHistory)), "after the token");
        // an old address, from before the token: counted
        assert_eq!(svc.vote(ip(1), &ballot(2, T, Vote::Down, DAY, 8)).unwrap().cast, Cast::Counted);
        let c = svc.test_connection();
        assert_eq!(token_created_daa(&c, &T).unwrap(), Some(TOKEN_DAA));
        assert_eq!(token_created_daa(&c, &Hash32([0x55; 32])).unwrap(), None);
    }

    #[test]
    fn one_vote_per_wallet_per_24_hours_across_logos_and_a_change_after_it() {
        let (svc, now) = service(cfg());
        assert_eq!(svc.vote(ip(1), &ballot(1, T, Vote::Down, DAY, 8)).unwrap().cast, Cast::Counted);
        // the same vote again: unchanged, and it uses nothing
        assert_eq!(svc.vote(ip(1), &ballot(1, T, Vote::Down, DAY, 8)).unwrap(), Accepted { cast: Cast::Unchanged, up: 0, down: 1 });
        // another logo inside the 24 hours: too soon
        now.fetch_add(3_600_000, Ordering::SeqCst);
        match svc.vote(ip(1), &ballot(1, U, Vote::Up, DAY, 8)) {
            Err(VoteError::TooSoon(s)) => assert_eq!(s, 23 * 3600),
            other => panic!("{other:?}"),
        }
        // a change of the same logo inside the 24 hours: too soon as well
        assert!(matches!(svc.vote(ip(1), &ballot(1, T, Vote::Up, DAY, 8)), Err(VoteError::TooSoon(_))));
        // after the interval the vote can be changed
        now.store(NOW + DAY_MS, Ordering::SeqCst);
        let a = svc.vote(ip(1), &ballot(1, T, Vote::Up, "2026-11-21", 8)).unwrap();
        assert_eq!(a, Accepted { cast: Cast::Changed, up: 1, down: 0 });
        // every vote is kept
        let rows: i64 = svc.test_connection().query_row("SELECT COUNT(*) FROM logo_votes", [], |r| r.get(0)).unwrap();
        assert_eq!(rows, 2);
    }

    #[test]
    fn the_totals_publish_counts_and_the_hidden_set_never_the_voters() {
        let (svc, _) = service(cfg());
        let body = |s: &LogoVotes| serde_json::from_slice::<serde_json::Value>(&s.totals_body().unwrap()).unwrap();
        assert_eq!(body(&svc)["logos"], json!([]));
        svc.vote(ip(1), &ballot(1, T, Vote::Up, DAY, 8)).unwrap();
        svc.vote(ip(2), &ballot(2, T, Vote::Down, DAY, 8)).unwrap();
        let v = body(&svc);
        assert_eq!(v["logos"], json!([{"covenant_id": T.to_hex(), "up": 1, "down": 1}]));
        assert_eq!(v["hidden"], json!([]), "two voters: not judged");
        svc.vote(ip(3), &ballot(3, T, Vote::Down, DAY, 8)).unwrap();
        let v = body(&svc);
        assert_eq!(v["hidden"], json!([T.to_hex()]), "three voters, net -1");
        assert_eq!((v["min_voters"].as_i64(), v["hide_at"].as_i64(), v["pow_bits"].as_i64()), (Some(3), Some(-1), Some(8)));
        assert_eq!(v["interval_secs"], 86_400);
        let text = v.to_string();
        for n in 1..=3 {
            let (_, key, addr) = account(n);
            assert!(!text.contains(&addr) && !text.contains(&crate::hex::encode(&key)), "no voter in the totals");
        }
        // net 0 shows again
        svc.vote(ip(4), &ballot(4, T, Vote::Up, DAY, 8)).unwrap();
        assert_eq!(body(&svc)["hidden"], json!([]), "the vote dropped the cached body");
    }

    #[test]
    fn the_limits_are_per_client_per_hour_and_per_day() {
        let (svc, _) = service(LogoVotesConfig { per_client_per_hour: 2, max_per_day: 3, ..cfg() });
        svc.vote(ip(1), &ballot(1, T, Vote::Up, DAY, 8)).unwrap();
        svc.vote(ip(1), &ballot(2, T, Vote::Up, DAY, 8)).unwrap();
        assert!(matches!(svc.vote(ip(1), &ballot(3, T, Vote::Up, DAY, 8)), Err(VoteError::Limited(_))), "the client's hour");
        svc.vote(ip(2), &ballot(3, T, Vote::Up, DAY, 8)).unwrap();
        assert!(matches!(svc.vote(ip(3), &ballot(4, T, Vote::Up, DAY, 8)), Err(VoteError::Limited(_))), "the day's total");
        // a refused vote is not written
        let rows: i64 = svc.test_connection().query_row("SELECT COUNT(*) FROM logo_votes", [], |r| r.get(0)).unwrap();
        assert_eq!(rows, 3);
    }

    #[test]
    fn the_configuration_defaults() {
        let c = LogoVotesConfig::default();
        assert_eq!(
            (c.enabled, c.pow_bits, c.interval_secs, c.min_history_secs, c.per_client_per_hour, c.max_per_day),
            (false, 20, 86_400, 604_800, 20, 20_000)
        );
        let cfg: crate::config::IndexerConfig = toml::from_str("[logo_votes]\nenabled = true\npow_bits = 18").unwrap();
        assert!(cfg.logo_votes.enabled);
        assert_eq!(cfg.logo_votes.pow_bits, 18);
        assert!(toml::from_str::<crate::config::IndexerConfig>("[logo_votes]\nhide_at = 0").is_err(), "the rule is not a setting");
    }

    #[test]
    fn export_and_import_carry_the_votes_and_rebuild_the_totals() {
        let (svc, now) = service(cfg());
        svc.vote(ip(1), &ballot(1, T, Vote::Down, DAY, 8)).unwrap();
        svc.vote(ip(2), &ballot(2, T, Vote::Down, DAY, 8)).unwrap();
        now.store(NOW + DAY_MS, Ordering::SeqCst);
        svc.vote(ip(1), &ballot(1, T, Vote::Up, "2026-11-21", 8)).unwrap();
        svc.vote(ip(2), &ballot(2, U, Vote::Up, "2026-11-21", 8)).unwrap();
        let c = svc.test_connection();
        let doc = export(&c).unwrap();
        let before = totals(&c).unwrap();
        let mut d = open_memory("testnet-10").unwrap();
        assert_eq!(import(&mut d, &doc).unwrap(), 4);
        assert_eq!(export(&d).unwrap(), doc);
        assert_eq!(totals(&d).unwrap(), before);
        assert_eq!(before.0, vec![(T.to_hex(), 1, 1), (U.to_hex(), 1, 0)]);
        assert!(import(&mut d, &doc).unwrap_err().contains("not empty"));
    }

    #[test]
    fn a_schema_14_store_without_the_tables_gets_them_at_open() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("index.sqlite3");
        {
            let c = crate::indexer::db::open_writer(&p, "testnet-10").unwrap();
            // what a store written by the build before this one holds
            c.execute_batch("DROP TABLE logo_votes; DROP TABLE logo_vote_totals;").unwrap();
        }
        let c = crate::indexer::db::open_writer(&p, "testnet-10").unwrap();
        let n: i64 = c
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name IN ('logo_votes', 'logo_vote_totals')",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(n, 2);
        assert_eq!(meta_get(&c, "schema_version").unwrap(), Some(crate::indexer::db::SCHEMA_VERSION.to_string()));
        assert_eq!(crate::indexer::db::SCHEMA_VERSION, 14, "no version change");
        drop(c);
        // the service's own open creates them as well
        let c = Connection::open(&p).unwrap();
        c.execute_batch("DROP TABLE logo_votes; DROP TABLE logo_vote_totals;").unwrap();
        drop(c);
        let c = open(&p, "testnet-10").unwrap();
        assert!(c.prepare("SELECT 1 FROM logo_votes").is_ok());
        assert!(matches!(open(&p, "mainnet"), Err(DbError::NetworkMismatch { .. })));
    }
}
