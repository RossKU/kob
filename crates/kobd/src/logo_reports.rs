//! The token-logo blocklist: reports in, the list of hidden tokens out (docs/api-reference.md, 3.19 and 3.20).
//!
//! KOB keeps and serves no third-party image: a token's logo is loaded by each viewer's browser from the address its creator gave.
//! What the operator can do about an image that should not be shown is to hide it, and that is this module:
//!
//! * `POST /v1/logo-reports` files a report (a token, one of three reasons, and where it came from: the web app or a GitHub Issue form).
//!   There is no free text, so nothing a stranger typed is stored.
//! * The FIRST report of a token hides its logo at once (the web app then shows the letter avatar). The operator reviews the report
//!   (`kobd logo-reports list`) and may `restore` the token. Reports that arrive for a restored token inside the cooldown
//!   (`cooldown_secs`, default 6 hours) are recorded and change nothing; one after the cooldown hides it again.
//!   An operator can also hide a token for good (`hide`: state `never`).
//! * `GET /v1/logo-blocklist` serves `{"hidden":[covenant ids],"updated_ms":..}`; the web app reads it every two minutes.
//!
//! The state lives in the indexer's database (`logo_reports.sql`, created with the other tables). There is no administrative route on
//! the network: restoring and hiding are subcommands of the executor, run on the host next to the database.
//!
//! Abuse bounds: a report costs the reporting client one of `per_client_per_hour` and the server one of `max_per_day` (a flood from
//! many addresses can hide at most that many tokens a day, and the operator restores them), the request body is at most 1 KiB, the
//! report table keeps its newest [`MAX_REPORT_ROWS`] rows.

use crate::config::LogoReportsConfig;
use crate::hex::Hash32;
use crate::indexer::db::{meta_get, DbError, DbResult};
use axum::body::Bytes;
use rusqlite::{params, Connection, OpenFlags, OptionalExtension, TransactionBehavior};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::collections::{HashMap, VecDeque};
use std::net::IpAddr;
use std::path::Path;
use std::sync::{Arc, Mutex};

/// The tables (also created by `db::open_writer`, so a database opened by this build always has them).
pub const SCHEMA: &str = include_str!("logo_reports.sql");
/// The newest report rows kept.
pub const MAX_REPORT_ROWS: i64 = 50_000;
/// How long `GET /v1/logo-blocklist` reuses a rendered answer (a write drops it at once).
pub const LIST_CACHE_MS: i64 = 5_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Reason {
    Illegal,
    Impersonation,
    Other,
}

impl Reason {
    pub fn as_str(self) -> &'static str {
        match self {
            Reason::Illegal => "illegal",
            Reason::Impersonation => "impersonation",
            Reason::Other => "other",
        }
    }
}

/// Where a report came from. The web app files `Web`; the GitHub workflow files `Github` with the issue number.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Source {
    Web,
    Github,
}

impl Source {
    pub fn as_str(self) -> &'static str {
        match self {
            Source::Web => "web",
            Source::Github => "github",
        }
    }
}

/// One report as the store takes it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Report {
    pub covenant_id: Hash32,
    pub reason: Reason,
    pub source: Source,
    /// The GitHub issue number (only with `Source::Github`).
    pub reference: Option<u32>,
}

/// What a report did. `Hidden`: the token's logo is hidden now (it was, or this report hid it). `Recorded`: kept for the operator, nothing changed
/// (the token is restored and inside the cooldown).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Filed {
    Hidden,
    Recorded,
}

impl Filed {
    pub fn as_str(self) -> &'static str {
        match self {
            Filed::Hidden => "hidden",
            Filed::Recorded => "recorded",
        }
    }
}

/// Opens the database read-write for the report store (no schema work beyond the logo tables, no migration of the others): used by the
/// API and by the administrative subcommands next to a running executor. The file must exist and belong to `network`.
pub fn open(path: &Path, network: &str) -> DbResult<Connection> {
    if !path.exists() {
        return Err(DbError::Invalid(format!("{} does not exist", path.display())));
    }
    let conn = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_NO_MUTEX)?;
    // short: a request that cannot get the write lock soon answers "busy" instead of holding a thread
    conn.busy_timeout(std::time::Duration::from_secs(5))?;
    let net = meta_get(&conn, "network")?.unwrap_or_default();
    if net != network {
        return Err(DbError::NetworkMismatch { found: net, expected: network.to_string() });
    }
    conn.execute_batch(SCHEMA)?;
    Ok(conn)
}

/// Files `r` at `now_ms`. See the module documentation for the rules; one write transaction.
pub fn file_report(conn: &mut Connection, r: &Report, now_ms: i64, cooldown_ms: i64) -> DbResult<Filed> {
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let id = r.covenant_id.0.to_vec();
    let row: Option<(String, Option<i64>)> = tx
        .query_row("SELECT state, restored_ms FROM logo_blocklist WHERE covenant_id = ?1", [&id], |x| Ok((x.get(0)?, x.get(1)?)))
        .optional()?;
    let (filed, outcome) = match row {
        None => {
            tx.execute(
                "INSERT INTO logo_blocklist (covenant_id, state, first_ms, hidden_ms, restored_ms, report_count, updated_ms) \
                 VALUES (?1, 'hidden', ?2, ?2, NULL, 1, ?2)",
                params![id, now_ms],
            )?;
            (Filed::Hidden, "hidden")
        }
        Some((state, _)) if state == "hidden" || state == "never" => {
            tx.execute("UPDATE logo_blocklist SET report_count = report_count + 1 WHERE covenant_id = ?1", [&id])?;
            (Filed::Hidden, "hidden")
        }
        Some((_, restored_ms)) => {
            // restored: reports inside the cooldown are recorded only
            if restored_ms.is_some_and(|at| now_ms < at.saturating_add(cooldown_ms)) {
                tx.execute("UPDATE logo_blocklist SET report_count = report_count + 1 WHERE covenant_id = ?1", [&id])?;
                (Filed::Recorded, "recorded")
            } else {
                tx.execute(
                    "UPDATE logo_blocklist SET state = 'hidden', hidden_ms = ?2, updated_ms = ?2, report_count = report_count + 1 \
                     WHERE covenant_id = ?1",
                    params![id, now_ms],
                )?;
                (Filed::Hidden, "rehidden")
            }
        }
    };
    tx.execute(
        "INSERT INTO logo_reports (covenant_id, reason, source, ref, created_ms, outcome) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        params![id, r.reason.as_str(), r.source.as_str(), r.reference, now_ms, outcome],
    )?;
    tx.execute("DELETE FROM logo_reports WHERE id <= (SELECT MAX(id) FROM logo_reports) - ?1", [MAX_REPORT_ROWS])?;
    tx.commit()?;
    Ok(filed)
}

/// The hidden tokens (states `hidden` and `never`), sorted, and when the set last changed.
pub fn blocklist(conn: &Connection) -> DbResult<(Vec<String>, i64)> {
    let mut q = conn.prepare("SELECT covenant_id FROM logo_blocklist WHERE state IN ('hidden', 'never') ORDER BY covenant_id")?;
    let hidden =
        q.query_map([], |r| r.get::<_, Vec<u8>>(0))?.map(|b| b.map(|b| crate::hex::encode(&b))).collect::<Result<Vec<_>, _>>()?;
    let updated: i64 = conn.query_row("SELECT COALESCE(MAX(updated_ms), 0) FROM logo_blocklist", [], |r| r.get(0))?;
    Ok((hidden, updated))
}

/// What the administrator can do to a listed token.
#[derive(Debug, PartialEq, Eq)]
pub enum AdminError {
    NotListed,
    /// `restore` of a token hidden for good needs `force`.
    Never,
    AlreadyRestored,
    Db(String),
}

impl std::fmt::Display for AdminError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            AdminError::NotListed => write!(f, "that token is not in the blocklist"),
            AdminError::Never => write!(f, "that token was hidden for good (`hide`); pass --force to restore it anyway"),
            AdminError::AlreadyRestored => write!(f, "that token is already restored"),
            AdminError::Db(e) => write!(f, "{e}"),
        }
    }
}

impl std::error::Error for AdminError {}

impl From<rusqlite::Error> for AdminError {
    fn from(e: rusqlite::Error) -> Self {
        AdminError::Db(e.to_string())
    }
}

/// Restores a hidden token at `now_ms`: its logo shows again, and new reports are recorded without hiding it again for the cooldown.
pub fn restore(conn: &mut Connection, id: &Hash32, now_ms: i64, force: bool) -> Result<(), AdminError> {
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let state: Option<String> =
        tx.query_row("SELECT state FROM logo_blocklist WHERE covenant_id = ?1", [id.0.to_vec()], |r| r.get(0)).optional()?;
    match state.as_deref() {
        None => return Err(AdminError::NotListed),
        Some("restored") => return Err(AdminError::AlreadyRestored),
        Some("never") if !force => return Err(AdminError::Never),
        _ => {}
    }
    tx.execute(
        "UPDATE logo_blocklist SET state = 'restored', restored_ms = ?2, updated_ms = ?2 WHERE covenant_id = ?1",
        params![id.0.to_vec(), now_ms],
    )?;
    tx.commit()?;
    Ok(())
}

/// Hides a token for good (state `never`), listed or not: reports never restore it, only `restore --force` does.
pub fn hide_for_good(conn: &mut Connection, id: &Hash32, now_ms: i64) -> Result<(), AdminError> {
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    tx.execute(
        "INSERT INTO logo_blocklist (covenant_id, state, first_ms, hidden_ms, restored_ms, report_count, updated_ms) \
         VALUES (?1, 'never', ?2, ?2, NULL, 0, ?2) \
         ON CONFLICT (covenant_id) DO UPDATE SET state = 'never', hidden_ms = ?2, updated_ms = ?2",
        params![id.0.to_vec(), now_ms],
    )?;
    tx.commit()?;
    Ok(())
}

/// One listed token with what was reported about it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Listed {
    pub covenant_id: String,
    pub state: String,
    pub reports: i64,
    pub first_ms: i64,
    pub hidden_ms: i64,
    pub restored_ms: Option<i64>,
    /// Reports per reason, in the order illegal, impersonation, other.
    pub by_reason: [i64; 3],
    /// GitHub issue numbers among the reports (newest 5).
    pub issues: Vec<i64>,
}

/// Every listed token, those hidden and not yet reviewed first (newest first), then the rest.
pub fn list(conn: &Connection) -> DbResult<Vec<Listed>> {
    let mut q = conn.prepare(
        "SELECT covenant_id, state, report_count, first_ms, hidden_ms, restored_ms FROM logo_blocklist \
         ORDER BY (state = 'hidden') DESC, hidden_ms DESC, covenant_id",
    )?;
    let rows = q
        .query_map([], |r| {
            Ok((
                r.get::<_, Vec<u8>>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, i64>(2)?,
                r.get::<_, i64>(3)?,
                r.get::<_, i64>(4)?,
                r.get::<_, Option<i64>>(5)?,
            ))
        })?
        .collect::<Result<Vec<_>, _>>()?;
    let mut out = Vec::with_capacity(rows.len());
    for (id, state, reports, first_ms, hidden_ms, restored_ms) in rows {
        let mut by_reason = [0i64; 3];
        let mut rq = conn.prepare("SELECT reason, COUNT(*) FROM logo_reports WHERE covenant_id = ?1 GROUP BY reason")?;
        for r in rq.query_map([&id], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)))? {
            let (reason, n) = r?;
            match reason.as_str() {
                "illegal" => by_reason[0] = n,
                "impersonation" => by_reason[1] = n,
                _ => by_reason[2] = n,
            }
        }
        let mut iq =
            conn.prepare("SELECT ref FROM logo_reports WHERE covenant_id = ?1 AND ref IS NOT NULL ORDER BY id DESC LIMIT 5")?;
        let issues = iq.query_map([&id], |r| r.get::<_, i64>(0))?.collect::<Result<Vec<_>, _>>()?;
        out.push(Listed { covenant_id: crate::hex::encode(&id), state, reports, first_ms, hidden_ms, restored_ms, by_reason, issues });
    }
    Ok(out)
}

/// The whole state as JSON (`export`): the blocklist and the report rows, for a database that is rebuilt (`kobd db replay`).
pub fn export(conn: &Connection) -> DbResult<serde_json::Value> {
    let mut q = conn.prepare("SELECT covenant_id, state, first_ms, hidden_ms, restored_ms, report_count, updated_ms FROM logo_blocklist ORDER BY covenant_id")?;
    let blocklist = q
        .query_map([], |r| {
            Ok(json!({
                "covenant_id": crate::hex::encode(&r.get::<_, Vec<u8>>(0)?),
                "state": r.get::<_, String>(1)?,
                "first_ms": r.get::<_, i64>(2)?,
                "hidden_ms": r.get::<_, i64>(3)?,
                "restored_ms": r.get::<_, Option<i64>>(4)?,
                "report_count": r.get::<_, i64>(5)?,
                "updated_ms": r.get::<_, i64>(6)?,
            }))
        })?
        .collect::<Result<Vec<_>, _>>()?;
    let mut q = conn.prepare("SELECT covenant_id, reason, source, ref, created_ms, outcome FROM logo_reports ORDER BY id")?;
    let reports = q
        .query_map([], |r| {
            Ok(json!({
                "covenant_id": crate::hex::encode(&r.get::<_, Vec<u8>>(0)?),
                "reason": r.get::<_, String>(1)?,
                "source": r.get::<_, String>(2)?,
                "ref": r.get::<_, Option<i64>>(3)?,
                "created_ms": r.get::<_, i64>(4)?,
                "outcome": r.get::<_, String>(5)?,
            }))
        })?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(json!({ "format": "kob-logo-reports/1", "blocklist": blocklist, "reports": reports }))
}

/// Loads an [`export`] into an EMPTY report store (it refuses to merge into rows that exist). Returns (listed tokens, report rows).
pub fn import(conn: &mut Connection, doc: &serde_json::Value) -> Result<(usize, usize), String> {
    if doc.get("format").and_then(|f| f.as_str()) != Some("kob-logo-reports/1") {
        return Err("not a kob-logo-reports/1 export".into());
    }
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate).map_err(|e| e.to_string())?;
    let n: i64 = tx
        .query_row("SELECT (SELECT COUNT(*) FROM logo_blocklist) + (SELECT COUNT(*) FROM logo_reports)", [], |r| r.get(0))
        .map_err(|e| e.to_string())?;
    if n != 0 {
        return Err("the report store is not empty; import loads an export into a rebuilt database only".into());
    }
    let list = |k: &str| doc.get(k).and_then(|v| v.as_array()).ok_or_else(|| format!("`{k}` is missing"));
    let id_of = |v: &serde_json::Value| -> Result<Vec<u8>, String> {
        Hash32::parse(v.get("covenant_id").and_then(|x| x.as_str()).ok_or("a row has no covenant_id")?)
            .map(|h| h.0.to_vec())
            .map_err(|e| e.to_string())
    };
    let int = |v: &serde_json::Value, k: &str| v.get(k).and_then(|x| x.as_i64()).ok_or_else(|| format!("a row has no integer `{k}`"));
    let text = |v: &serde_json::Value, k: &str| {
        v.get(k).and_then(|x| x.as_str()).map(str::to_string).ok_or_else(|| format!("a row has no `{k}`"))
    };
    let (mut tokens, mut rows) = (0, 0);
    for v in list("blocklist")? {
        tx.execute(
            "INSERT INTO logo_blocklist (covenant_id, state, first_ms, hidden_ms, restored_ms, report_count, updated_ms) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![id_of(v)?, text(v, "state")?, int(v, "first_ms")?, int(v, "hidden_ms")?, v.get("restored_ms").and_then(|x| x.as_i64()), int(v, "report_count")?, int(v, "updated_ms")?],
        )
        .map_err(|e| e.to_string())?;
        tokens += 1;
    }
    for v in list("reports")? {
        tx.execute(
            "INSERT INTO logo_reports (covenant_id, reason, source, ref, created_ms, outcome) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                id_of(v)?,
                text(v, "reason")?,
                text(v, "source")?,
                v.get("ref").and_then(|x| x.as_i64()),
                int(v, "created_ms")?,
                text(v, "outcome")?
            ],
        )
        .map_err(|e| e.to_string())?;
        rows += 1;
    }
    tx.commit().map_err(|e| e.to_string())?;
    Ok((tokens, rows))
}

// ------------------------------------------------------------------------------------------------ the service behind the routes

/// Why a report was not accepted.
#[derive(Debug)]
pub enum ReportError {
    /// Over a limit; retry after this many seconds.
    Limited(u64),
    Db(DbError),
}

/// Sliding-window limits: `per_client_per_hour` per client key, `max_per_day` in all. Only admitted reports are remembered, so the table
/// holds at most `max_per_day` entries and a flood of refused requests cannot grow it.
struct Limiter {
    clients: HashMap<IpAddr, VecDeque<i64>>,
    day: VecDeque<i64>,
}

const HOUR_MS: i64 = 3_600_000;
const DAY_MS: i64 = 24 * HOUR_MS;

impl Limiter {
    fn new() -> Self {
        Limiter { clients: HashMap::new(), day: VecDeque::new() }
    }

    fn admit(&mut self, key: IpAddr, now: i64, per_client_per_hour: u32, max_per_day: u32) -> Result<(), u64> {
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
        if self.day.len() >= max_per_day as usize {
            return Err(secs(self.day.front().map_or(DAY_MS, |t| t + DAY_MS - now)));
        }
        let q = self.clients.get(&key);
        if q.map_or(0, VecDeque::len) >= per_client_per_hour as usize {
            return Err(secs(q.and_then(|q| q.front()).map_or(HOUR_MS, |t| t + HOUR_MS - now)));
        }
        self.clients.entry(key).or_default().push_back(now);
        self.day.push_back(now);
        Ok(())
    }
}

/// The report store as the API uses it: one connection behind a mutex, the limits, and the rendered blocklist.
pub struct LogoReports {
    conn: Mutex<Connection>,
    cfg: LogoReportsConfig,
    limiter: Mutex<Limiter>,
    cache: Mutex<Option<(i64, Bytes)>>,
    now_ms: Box<dyn Fn() -> i64 + Send + Sync>,
    ipv6_prefix_bits: u8,
}

pub fn system_now_ms() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_millis() as i64)
}

impl LogoReports {
    pub fn open(path: &Path, network: &str, cfg: LogoReportsConfig, ipv6_prefix_bits: u8) -> DbResult<Arc<Self>> {
        Ok(Arc::new(Self::with_connection(open(path, network)?, cfg, ipv6_prefix_bits, Box::new(system_now_ms))))
    }

    pub fn with_connection(
        conn: Connection,
        cfg: LogoReportsConfig,
        ipv6_prefix_bits: u8,
        now_ms: Box<dyn Fn() -> i64 + Send + Sync>,
    ) -> Self {
        LogoReports {
            conn: Mutex::new(conn),
            cfg,
            limiter: Mutex::new(Limiter::new()),
            cache: Mutex::new(None),
            now_ms,
            ipv6_prefix_bits,
        }
    }

    /// Files a report from `client` (the guard's client address). Blocking (a database write): call it from a blocking task.
    pub fn report(&self, client: IpAddr, r: &Report) -> Result<Filed, ReportError> {
        let now = (self.now_ms)();
        let key = crate::api::client_ip::rate_key(client, self.ipv6_prefix_bits);
        self.limiter
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .admit(key, now, self.cfg.per_client_per_hour, self.cfg.max_per_day)
            .map_err(ReportError::Limited)?;
        let cooldown_ms = i64::try_from(self.cfg.cooldown_secs).unwrap_or(i64::MAX / 2).saturating_mul(1000);
        let filed =
            file_report(&mut self.conn.lock().unwrap_or_else(|e| e.into_inner()), r, now, cooldown_ms).map_err(ReportError::Db)?;
        *self.cache.lock().unwrap_or_else(|e| e.into_inner()) = None;
        Ok(filed)
    }

    /// The service's connection, for tests that play the administrator.
    #[cfg(test)]
    pub(crate) fn test_connection(&self) -> std::sync::MutexGuard<'_, Connection> {
        self.conn.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// The JSON body of `GET /v1/logo-blocklist`, rendered at most every [`LIST_CACHE_MS`]. Blocking.
    pub fn blocklist_body(&self) -> DbResult<Bytes> {
        let now = (self.now_ms)();
        if let Some((at, body)) = self.cache.lock().unwrap_or_else(|e| e.into_inner()).as_ref() {
            if now - at < LIST_CACHE_MS {
                return Ok(body.clone());
            }
        }
        let (hidden, updated_ms) = blocklist(&self.conn.lock().unwrap_or_else(|e| e.into_inner()))?;
        let body = Bytes::from(
            serde_json::to_vec(&json!({ "hidden": hidden, "updated_ms": updated_ms })).map_err(|e| DbError::Invalid(e.to_string()))?,
        );
        *self.cache.lock().unwrap_or_else(|e| e.into_inner()) = Some((now, body.clone()));
        Ok(body)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::indexer::db::open_memory;

    const A: Hash32 = Hash32([0xaa; 32]);
    const B: Hash32 = Hash32([0xbb; 32]);
    const COOLDOWN: i64 = 21_600_000;

    fn rep(id: Hash32, reason: Reason) -> Report {
        Report { covenant_id: id, reason, source: Source::Web, reference: None }
    }

    fn hidden(c: &Connection) -> Vec<String> {
        blocklist(c).unwrap().0
    }

    #[test]
    fn the_configuration_defaults_are_off_6_hours_5_an_hour_200_a_day_and_unknown_keys_are_refused() {
        let c = LogoReportsConfig::default();
        assert_eq!((c.enabled, c.cooldown_secs, c.per_client_per_hour, c.max_per_day), (false, 21_600, 5, 200));
        let cfg: crate::config::IndexerConfig = toml::from_str(
            "[logo_reports]
enabled = true
cooldown_secs = 60
max_per_day = 10",
        )
        .unwrap();
        assert_eq!(
            (
                cfg.logo_reports.enabled,
                cfg.logo_reports.cooldown_secs,
                cfg.logo_reports.per_client_per_hour,
                cfg.logo_reports.max_per_day
            ),
            (true, 60, 5, 10)
        );
        assert!(!crate::config::IndexerConfig::default().logo_reports.enabled);
        // a misspelt key (or an admin token this build does not have) is an error, never ignored
        assert!(toml::from_str::<crate::config::IndexerConfig>(
            "[logo_reports]
admin_token_file = \"x\""
        )
        .is_err());
    }

    #[test]
    fn the_first_report_hides_and_the_set_is_sorted() {
        let mut c = open_memory("testnet-10").unwrap();
        assert_eq!(blocklist(&c).unwrap(), (vec![], 0));
        assert_eq!(file_report(&mut c, &rep(B, Reason::Illegal), 1_000, COOLDOWN).unwrap(), Filed::Hidden);
        assert_eq!(file_report(&mut c, &rep(A, Reason::Other), 2_000, COOLDOWN).unwrap(), Filed::Hidden);
        assert_eq!(hidden(&c), vec![A.to_hex(), B.to_hex()]);
        assert_eq!(blocklist(&c).unwrap().1, 2_000);
        // a second report of a hidden token changes nothing about the set
        assert_eq!(file_report(&mut c, &rep(A, Reason::Illegal), 9_000, COOLDOWN).unwrap(), Filed::Hidden);
        assert_eq!(blocklist(&c).unwrap().1, 2_000, "the set did not change");
        let l = list(&c).unwrap();
        let a = l.iter().find(|x| x.covenant_id == A.to_hex()).unwrap();
        assert_eq!((a.reports, a.by_reason, a.state.as_str()), (2, [1, 0, 1], "hidden"));
    }

    #[test]
    fn a_restored_token_is_recorded_only_inside_the_cooldown_then_hidden_again() {
        let mut c = open_memory("testnet-10").unwrap();
        file_report(&mut c, &rep(A, Reason::Impersonation), 1_000, COOLDOWN).unwrap();
        restore(&mut c, &A, 100_000, false).unwrap();
        assert!(hidden(&c).is_empty(), "restored: shown again");
        assert_eq!(blocklist(&c).unwrap().1, 100_000);
        // inside the cooldown
        assert_eq!(file_report(&mut c, &rep(A, Reason::Illegal), 100_000 + COOLDOWN - 1, COOLDOWN).unwrap(), Filed::Recorded);
        assert!(hidden(&c).is_empty());
        assert_eq!(blocklist(&c).unwrap().1, 100_000, "a recorded report changes nothing");
        // exactly at the end of the cooldown it hides again
        let at = 100_000 + COOLDOWN;
        assert_eq!(file_report(&mut c, &rep(A, Reason::Illegal), at, COOLDOWN).unwrap(), Filed::Hidden);
        assert_eq!(hidden(&c), vec![A.to_hex()]);
        assert_eq!(blocklist(&c).unwrap().1, at);
        // every report is kept with its outcome
        let outcomes: Vec<String> = c
            .prepare("SELECT outcome FROM logo_reports ORDER BY id")
            .unwrap()
            .query_map([], |r| r.get(0))
            .unwrap()
            .map(|x| x.unwrap())
            .collect();
        assert_eq!(outcomes, ["hidden", "recorded", "rehidden"]);
        // restoring again starts a new cooldown
        restore(&mut c, &A, at + 5, false).unwrap();
        assert_eq!(file_report(&mut c, &rep(A, Reason::Other), at + 10, COOLDOWN).unwrap(), Filed::Recorded);
    }

    #[test]
    fn hide_for_good_survives_reports_and_needs_force_to_restore() {
        let mut c = open_memory("testnet-10").unwrap();
        // an unlisted token can be hidden for good
        hide_for_good(&mut c, &A, 5).unwrap();
        assert_eq!(hidden(&c), vec![A.to_hex()]);
        assert_eq!(file_report(&mut c, &rep(A, Reason::Other), 6, COOLDOWN).unwrap(), Filed::Hidden);
        assert_eq!(restore(&mut c, &A, 7, false), Err(AdminError::Never));
        assert_eq!(hidden(&c), vec![A.to_hex()]);
        restore(&mut c, &A, 8, true).unwrap();
        assert!(hidden(&c).is_empty());
        assert_eq!(restore(&mut c, &A, 9, false), Err(AdminError::AlreadyRestored));
        assert_eq!(restore(&mut c, &B, 9, false), Err(AdminError::NotListed));
        // a restored token can be hidden for good again
        hide_for_good(&mut c, &A, 10).unwrap();
        assert_eq!(hidden(&c), vec![A.to_hex()]);
    }

    #[test]
    fn github_reports_keep_the_issue_number() {
        let mut c = open_memory("testnet-10").unwrap();
        let r = Report { covenant_id: A, reason: Reason::Illegal, source: Source::Github, reference: Some(42) };
        file_report(&mut c, &r, 1, COOLDOWN).unwrap();
        let l = list(&c).unwrap();
        assert_eq!(l[0].issues, vec![42]);
    }

    #[test]
    fn the_report_table_is_bounded() {
        let mut c = open_memory("testnet-10").unwrap();
        c.execute(
            "WITH RECURSIVE n(i) AS (SELECT 1 UNION ALL SELECT i + 1 FROM n WHERE i < ?1) \
             INSERT INTO logo_reports (covenant_id, reason, source, created_ms, outcome) SELECT zeroblob(32), 'other', 'web', i, 'recorded' FROM n",
            [MAX_REPORT_ROWS + 20],
        )
        .unwrap();
        file_report(&mut c, &rep(A, Reason::Other), 1, COOLDOWN).unwrap();
        let n: i64 = c.query_row("SELECT COUNT(*) FROM logo_reports", [], |r| r.get(0)).unwrap();
        assert_eq!(n, MAX_REPORT_ROWS, "the newest rows are kept");
        let mine: i64 = c.query_row("SELECT COUNT(*) FROM logo_reports WHERE covenant_id = ?1", [A.0.to_vec()], |r| r.get(0)).unwrap();
        assert_eq!(mine, 1, "the report just filed is among them");
    }

    #[test]
    fn export_and_import_carry_the_state_into_a_rebuilt_database() {
        let mut c = open_memory("testnet-10").unwrap();
        file_report(&mut c, &rep(A, Reason::Illegal), 1, COOLDOWN).unwrap();
        file_report(
            &mut c,
            &Report { covenant_id: B, reason: Reason::Other, source: Source::Github, reference: Some(7) },
            2,
            COOLDOWN,
        )
        .unwrap();
        restore(&mut c, &B, 3, false).unwrap();
        let doc = export(&c).unwrap();
        let mut d = open_memory("testnet-10").unwrap();
        assert_eq!(import(&mut d, &doc).unwrap(), (2, 2));
        assert_eq!(export(&d).unwrap(), doc);
        assert_eq!(hidden(&d), vec![A.to_hex()]);
        // a second import would double the history: refused
        assert!(import(&mut d, &doc).unwrap_err().contains("not empty"));
        assert!(import(&mut d, &json!({"format": "other"})).is_err());
    }

    #[test]
    fn the_limits_are_per_client_per_hour_and_per_day_in_all() {
        let ip = |n: u8| IpAddr::from([10, 0, 0, n]);
        let mut l = Limiter::new();
        for i in 0..5 {
            l.admit(ip(1), i * 1000, 5, 7).unwrap();
        }
        // the sixth within the hour: wait until the first leaves the window
        assert_eq!(l.admit(ip(1), 10_000, 5, 7), Err(3_590));
        // another client has its own budget, until the day's total is used
        l.admit(ip(2), 11_000, 5, 7).unwrap();
        l.admit(ip(2), 12_000, 5, 7).unwrap();
        assert_eq!(l.admit(ip(3), 13_000, 5, 7), Err(86_387), "the day's total is spent: the first report leaves in a day");
        // an hour later the first client may report again (the total is still counted over the day)
        assert!(l.admit(ip(1), HOUR_MS + 1, 5, 7).is_err(), "the total is still spent");
        assert!(l.admit(ip(1), DAY_MS + 1, 5, 7).is_ok());
        // a refused request is not remembered
        let mut l = Limiter::new();
        for _ in 0..1000 {
            let _ = l.admit(ip(9), 0, 0, 5);
        }
        assert!(l.day.is_empty() && l.clients.is_empty());
    }

    #[test]
    fn the_service_hides_at_once_serves_a_cached_list_and_drops_it_on_a_write() {
        let now = Arc::new(std::sync::atomic::AtomicI64::new(1_000_000));
        let clock = now.clone();
        let svc = LogoReports::with_connection(
            open_memory("testnet-10").unwrap(),
            LogoReportsConfig { enabled: true, ..Default::default() },
            64,
            Box::new(move || clock.load(std::sync::atomic::Ordering::SeqCst)),
        );
        let ip = IpAddr::from([10, 0, 0, 1]);
        let body = |s: &LogoReports| serde_json::from_slice::<serde_json::Value>(&s.blocklist_body().unwrap()).unwrap();
        assert_eq!(body(&svc), json!({"hidden": [], "updated_ms": 0}));
        assert_eq!(svc.report(ip, &rep(A, Reason::Illegal)).unwrap(), Filed::Hidden);
        assert_eq!(body(&svc), json!({"hidden": [A.to_hex()], "updated_ms": 1_000_000}), "a report drops the cached list");
        // the operator's change reaches clients within the cache time
        restore(&mut svc.conn.lock().unwrap(), &A, 1_000_500, false).unwrap();
        assert_eq!(body(&svc)["hidden"], json!([A.to_hex()]), "served from the cache");
        now.fetch_add(LIST_CACHE_MS, std::sync::atomic::Ordering::SeqCst);
        assert_eq!(body(&svc), json!({"hidden": [], "updated_ms": 1_000_500}));
    }
}
