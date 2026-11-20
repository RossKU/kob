//! The creator lock of `kob token create --lock-until` and `kob token unlock`
//! (`kob_protocol::creator_lock`, `docs/spec/token-metadata.md` *Creator lock*).
//!
//! `--lock-until` locks the share the funding key keeps: with `--sell`, the `--keep` part; without
//! it, `--keep` percent of the share (default all of it), the rest staying free in the wallet.
//!
//! By default the lock opens by itself (`kob_protocol::lock_auto`): the locked genesis output is
//! owned by the `KobLockAuto` script of the terms, the genesis pays the 1 KAS lock coin to its
//! address, and the declaration names the terms in `lock` with `"lock_kind": "auto"`. After the
//! time anyone may send the release, which pays the tokens and the coin's KAS to the key. With
//! `--lock-manual` the owner is the P2SH hash of `<until> OP_CHECKLOCKTIMEVERIFY <key>
//! OP_CHECKSIG`, and only the key's signature opens it.
//!
//! `kob token unlock` reads the lock from the token's launch proof (the indexer's, or the
//! `launch_proof.json` of the creation's directory), verifies it against the genesis, and once the
//! node's past median time is past the lock's time:
//! - an automatic lock: sends the release, with no key (the coin pays the fee);
//! - a manual lock: spends the locked outputs with a KAS coin at the lock's address, signed by the
//!   key. Without such a coin it first sends one (1 KAS, returned as the unlock's change): two
//!   transactions, signed before one confirmation.

use std::path::Path;
use std::time::Duration;

use kob_protocol::artifacts::token_template;
use kob_protocol::build::TokenRef;
use kob_protocol::creator_lock::{
    build_lock_coin, build_unlock, CreatorLock, LockCoin, LockKind, UnlockCreatorLock, VerifiedLock, LOCK_COIN,
};
use kob_protocol::issue::{Holder, IssueSpec, SCHEME_P2SH};
use kob_protocol::json::to_hex;
use kob_protocol::kcc23::{LaunchProof, VerifiedDeclaration};
use kob_protocol::state::TokenState;
use kob_protocol::tx::{BuiltTx, TokenUtxo, Utxo};
use serde_json::{json, Value};

use crate::common::{address_of, lookup, maker_funding, spk_bytes, utxo_of, NODE_TIMEOUT};
use crate::token_sale::{parse_keep, Sale, SaleArgs};
use crate::trade::opts;
use crate::trade::units::{tokens, TokenUnit};

const DAY_MS: u64 = 86_400_000;
/// The farthest ahead `--lock-until` may be: about 10 years (3,653 days). A time in microseconds,
/// or a wrong unit, would lock the share for millennia.
pub const MAX_LOCK_MS: u64 = 3_653 * DAY_MS;
/// A token's declaration the indexer does not have yet (a genesis just sent) is asked again this often, this far apart.
const DECLARATION_ATTEMPTS: u32 = 6;
const DECLARATION_RETRY: std::time::Duration = std::time::Duration::from_secs(5);

/// Now, unix ms.
pub fn now_ms() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0)
}

/// Days since 1970-01-01 of a proleptic Gregorian date (H. Hinnant's `days_from_civil`).
fn days_from_civil(y: i64, m: u32, d: u32) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let mp = (m + 9) % 12;
    let doy = (153 * mp as i64 + 2) / 5 + d as i64 - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// The date of a day number (inverse of [`days_from_civil`]).
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { yoe + era * 400 + 1 } else { yoe + era * 400 }, m, d)
}

/// A unix-ms time as `YYYY-MM-DD HH:MM UTC`.
pub fn utc(ms: u64) -> String {
    let secs = (ms / 1000) as i64;
    let (y, m, d) = civil_from_days(secs.div_euclid(86_400));
    let s = secs.rem_euclid(86_400);
    format!("{y:04}-{m:02}-{d:02} {:02}:{:02} UTC", s / 3600, (s % 3600) / 60)
}

/// `--lock-until`: `+90d`, `+12h`, `+15m` (from `now`), `YYYY-MM-DD` (00:00 UTC),
/// `YYYY-MM-DDTHH:MM[:SS]Z` (UTC), or unix milliseconds (13 digits). It must lie in the future,
/// at most [`MAX_LOCK_MS`] (about 10 years) from `now`.
pub fn parse_lock_until(v: &str, now: u64) -> Result<u64, String> {
    let bad = || {
        format!(
            "--lock-until takes +DAYSd, +HOURSh or +MINUTESm, a date YYYY-MM-DD or YYYY-MM-DDTHH:MMZ (UTC), or unix milliseconds; got `{v}`"
        )
    };
    let t = v.trim();
    let until = if let Some(rel) = t.strip_prefix('+') {
        let (n, unit) = rel.split_at(rel.len().saturating_sub(1));
        let n: u64 = n.parse().map_err(|_| bad())?;
        let step = match unit {
            "d" => DAY_MS,
            "h" => 3_600_000,
            "m" => 60_000,
            _ => return Err(bad()),
        };
        now.checked_add(n.checked_mul(step).ok_or_else(bad)?).ok_or_else(bad)?
    } else if !t.is_empty() && t.bytes().all(|b| b.is_ascii_digit()) {
        // unix milliseconds now and for centuries have 13 digits: 10 are seconds, 16 microseconds
        if t.len() != 13 {
            return Err(format!(
                "--lock-until {t}: a unix time is in milliseconds (13 digits), this one has {} (seconds have 10, microseconds 16)",
                t.len()
            ));
        }
        t.parse().map_err(|_| bad())?
    } else {
        let (date, time) = t.split_once('T').unwrap_or((t, ""));
        let p: Vec<&str> = date.split('-').collect();
        if p.len() != 3 || p[0].len() != 4 || p[1].len() != 2 || p[2].len() != 2 {
            return Err(bad());
        }
        let (y, m, d): (i64, u32, u32) =
            (p[0].parse().map_err(|_| bad())?, p[1].parse().map_err(|_| bad())?, p[2].parse().map_err(|_| bad())?);
        if !(1..=12).contains(&m) || d == 0 || d > 31 || civil_from_days(days_from_civil(y, m, d)) != (y, m, d) {
            return Err(bad());
        }
        let mut secs = days_from_civil(y, m, d) * 86_400;
        if !time.is_empty() {
            let time = time.strip_suffix('Z').ok_or_else(bad)?;
            let hms: Vec<&str> = time.split(':').collect();
            if !(2..=3).contains(&hms.len()) || hms.iter().any(|x| x.len() != 2) {
                return Err(bad());
            }
            let n: Vec<i64> = hms.iter().map(|x| x.parse::<i64>()).collect::<Result<_, _>>().map_err(|_| bad())?;
            if n[0] > 23 || n[1] > 59 || n.get(2).is_some_and(|s| *s > 59) {
                return Err(bad());
            }
            secs += n[0] * 3600 + n[1] * 60 + n.get(2).copied().unwrap_or(0);
        }
        u64::try_from(secs).map_err(|_| bad())? * 1000
    };
    if until < kob_protocol::creator_lock::LOCK_TIME_THRESHOLD {
        return Err(bad());
    }
    if until <= now {
        return Err(format!("--lock-until {} is not in the future", utc(until)));
    }
    if until > kob_protocol::creator_lock::MAX_UNTIL {
        return Err(bad());
    }
    if until - now > MAX_LOCK_MS {
        return Err(format!("--lock-until {} is more than 10 years away: a lock opens within 10 years", utc(until)));
    }
    Ok(until)
}

/// The locked share of a creation.
#[derive(Debug, Clone)]
pub struct Locked {
    /// The holder (genesis output) the lock owns.
    pub at: usize,
    pub amount: u64,
    pub lock: CreatorLock,
    pub kind: LockKind,
}

/// Locks the share the funding key keeps (see the module docs) and declares the lock.
pub fn apply_lock(spec: &mut IssueSpec, s: &SaleArgs, sale: Option<&Sale>, funder: [u8; 32]) -> Result<Option<Locked>, String> {
    let Some(until) = s.lock_until else { return Ok(None) };
    let lock = CreatorLock::new(until, funder).map_err(|e| e.to_string())?;
    let kind = if s.lock_manual { LockKind::Manual } else { LockKind::Auto };
    spec.lock = Some(lock);
    spec.lock_kind = kind;
    let authority = spec.lock_authority().map_err(|e| e.to_string())?.ok_or("--lock-until: no lock")?;
    let locked =
        |h: &Holder| Holder { owner: authority, owner_scheme: SCHEME_P2SH, borrow_scheme: 0, borrow_guard: [0; 32], ..h.clone() };
    let at = match sale {
        Some(sale) => {
            if sale.keep == 0 {
                return Err("--lock-until locks the share kept in the wallet: give --keep PERCENT".into());
            }
            let at = sale.at + 1;
            let h = spec.holders.get(at).filter(|h| h.owner == funder && h.owner_scheme == 0 && h.amount == sale.keep);
            let h = h.ok_or("--lock-until: the kept share is not where the sale put it")?.clone();
            spec.holders[at] = locked(&h);
            at
        }
        None => {
            let at = spec
                .holders
                .iter()
                .position(|h| h.owner == funder && h.owner_scheme == 0)
                .ok_or("--lock-until: the funding key receives no tokens to lock (give it a share)")?;
            let h = spec.holders[at].clone();
            if h.borrow_scheme != 0 {
                return Err("--lock-until: the funding key's share has borrowing enabled".into());
            }
            let bps = s.keep.as_deref().map(parse_keep).transpose()?.unwrap_or(10_000);
            let amount = (h.amount as u128 * bps as u128 / 10_000) as u64;
            if amount == 0 {
                return Err("--lock-until: nothing to lock: raise --keep".into());
            }
            if amount == h.amount {
                spec.holders[at] = locked(&h);
                at
            } else {
                spec.holders[at].amount = h.amount - amount;
                spec.holders.insert(at + 1, locked(&Holder { amount, ..h }));
                at + 1
            }
        }
    };
    Ok(Some(Locked { at, amount: spec.holders[at].amount, lock, kind }))
}

/// Usage text of `kob token unlock`.
pub const UNLOCK_USAGE: &str = "\
kob token unlock: release a creator lock's tokens after its time (`kob token create --lock-until`)

USAGE
  kob token unlock <TOKEN_ID | DIR> [--outpoint TXID:INDEX] [--key-file PATH | --key-env NAME]
                   [--dry-run | --yes] [--node URL] [--indexer URL] [--network NET]

  TOKEN_ID          the token's covenant id: its lock is read from the indexer's launch proof
                    (GET /v1/tokens/<id>/declaration), verified against the genesis
  DIR               ... or the directory `kob token create` wrote (its launch_proof.json; no indexer needed)
  --outpoint        unlock only this locked output (default: every locked output still unspent)

The node takes it only once its past median time is past the lock's time (a minute or two after the clock).

An automatic lock (the default of `kob token create --lock-until`) needs no key: anyone may send the release. It
spends the locked output and the lock coin of the genesis and pays both to the lock's key (the coin less the fee).

A manual lock (`--lock-manual`) opens to its key: the signing key must be it. The unlock spends the locked output(s)
and a KAS coin at the lock's address and pays the tokens to the key. Without such a coin, a first transaction sends one
(1 KAS from the key's UTXOs, returned as the unlock's change); both are signed before the one confirmation (or --yes).
A dry run signs, runs everything in the script engine and sends nothing.
";

fn parse_outpoint(s: &str) -> Result<([u8; 32], u32), String> {
    let bad = || format!("--outpoint takes TXID:INDEX, got `{s}`");
    let (t, i) = s.split_once(':').ok_or_else(bad)?;
    Ok((kob_protocol::json::hex32(t).map_err(|_| bad())?, i.parse().map_err(|_| bad())?))
}

/// The lock read from a launch proof, verified.
struct Source {
    decl: VerifiedDeclaration,
    lock: VerifiedLock,
}

fn load(arg: &str, indexer: Option<&str>) -> Result<Source, String> {
    let path = Path::new(arg);
    let proof: Value = if path.is_dir() || arg.ends_with(".json") {
        let file = if path.is_dir() { path.join("launch_proof.json") } else { path.to_path_buf() };
        let text = std::fs::read_to_string(&file).map_err(|e| format!("{}: {e}", file.display()))?;
        serde_json::from_str(&text).map_err(|e| format!("{}: {e}", file.display()))?
    } else {
        let cov = kob_protocol::json::hex32(arg).map_err(|_| format!("`{arg}` is neither a token id (64 hex) nor a directory"))?;
        let indexer = indexer.ok_or("give the token's creation directory, or --indexer to read its launch proof")?;
        let p = format!("/v1/tokens/{}/declaration", to_hex(&cov));
        // a genesis just sent reaches the indexer a few seconds later: a 404 is asked again for half a minute
        let mut attempt = 0;
        loop {
            match crate::trade::http::get(indexer, &p, crate::trade::data::INDEXER_TIMEOUT)? {
                (200, v) => break v,
                (404, _) if attempt < DECLARATION_ATTEMPTS => {
                    if attempt == 0 {
                        eprintln!("waiting for the indexer to have the token's declaration ...");
                    }
                    attempt += 1;
                    std::thread::sleep(DECLARATION_RETRY);
                }
                (404, _) => return Err(format!("the indexer has no declaration of {}: give the creation's directory", to_hex(&cov))),
                (status, _) => return Err(format!("indexer {p}: HTTP {status}")),
            }
        }
    };
    let proof: LaunchProof = serde_json::from_value(proof).map_err(|e| format!("launch proof: {e}"))?;
    let decl = proof.verify()?;
    if !arg.ends_with(".json") && !Path::new(arg).is_dir() && kob_protocol::json::hex32(arg).ok() != Some(decl.covenant_id) {
        return Err("the launch proof is of another token".into());
    }
    let lock = decl.lock.clone().ok_or_else(|| {
        let why: Vec<&String> = decl.token.issues.iter().filter(|i| i.starts_with("lock")).collect();
        if why.is_empty() {
            "the token declares no creator lock".to_string()
        } else {
            format!("the token's declared lock holds nothing: {}", why.iter().map(|s| s.as_str()).collect::<Vec<_>>().join("; "))
        }
    })?;
    Ok(Source { decl, lock })
}

/// `kob token unlock` of an automatic lock: the release, which nobody signs.
fn release(p: &opts::Parsed, c: &opts::Common, src: &Source) -> Result<(), String> {
    use kob_protocol::lock_auto::{build_release, ReleaseAutoLock};
    let auto = src.lock.auto().ok_or("the automatic lock does not fit its token program")?;
    let coin_at = src.lock.coin.as_ref().ok_or("the automatic lock has no lock coin")?;
    let node = c.need_node()?.to_string();
    crate::wrpc::check_node_network(&node, &c.network, Duration::from_secs(30))?;
    let cov = src.decl.covenant_id;
    let gid = src.decl.genesis_txid;
    let (index, state) = src.lock.states().into_iter().next().ok_or("the lock holds no output")?;
    if p.get("--outpoint").map(parse_outpoint).transpose()?.is_some_and(|(t, i)| t != gid || i != index) {
        return Err("that outpoint is not the lock's output".into());
    }
    let tpl = token_template(src.lock.program);
    let st = TokenState::Kcc20(state);
    let spk = st.spk_with(tpl);
    let found = lookup(&node, &c.network, &[spk.clone(), auto.spk()])?;
    let token = found
        .get(&spk_bytes(&spk))
        .and_then(|v| v.iter().find(|u| u.txid == gid && u.index == index && u.covenant_id == Some(cov)))
        .map(|u| TokenUtxo { utxo: utxo_of(u), state: st.clone() });
    let coin = found
        .get(&spk_bytes(&auto.spk()))
        .and_then(|v| v.iter().find(|u| u.txid == gid && u.index == coin_at.index && u.covenant_id.is_none()))
        .map(utxo_of);
    let unit =
        TokenUnit { ticker: src.decl.token.symbol.clone().unwrap_or_default(), decimals: src.decl.token.decimals.unwrap_or(0).into() };
    println!("token        {} ({})", to_hex(&cov), unit.ticker);
    println!(
        "lock         {} until {} ({} ms), opens by itself to the key {}",
        tokens(src.lock.amount as i128, &unit),
        utc(auto.until),
        auto.until,
        to_hex(&auto.key)
    );
    println!("lock address {}", address_of(&auto.spk(), &c.network)?);
    let (Some(token), Some(coin)) = (token, coin) else {
        return Err("nothing to release: the locked output is spent (already released)".into());
    };
    let median = median_time(&node)?;
    if median < auto.until && !c.dry_run {
        let left = (auto.until - median).div_ceil(60_000);
        return Err(format!("still locked: the node's past median time is {} ({left} min before the lock's time)", utc(median)));
    }
    let req = ReleaseAutoLock {
        token: TokenRef { covenant_id: cov, program: src.lock.program },
        lock: auto.terms(),
        tokens: token,
        coin,
        keeper: None,
        tip: 0,
        fee: c.fee_options(),
    };
    let b = build_release(&req, &kob_protocol::budget::lookup).map_err(|e| format!("the release: {e}"))?;
    println!(
        "release      {}  ({} and {} sompi of the lock coin to {}, lock time {}, fee {} sompi, no signature)",
        to_hex(&b.tx.id),
        tokens(src.lock.amount as i128, &unit),
        req.coin.amount.saturating_sub(b.fee.fee),
        to_hex(&auto.key),
        auto.until,
        b.fee.fee
    );
    if median < auto.until {
        println!(
            "note         still locked: the node takes the release after {} (its past median time is {})",
            utc(auto.until),
            utc(median)
        );
    }
    let ceiling = if c.dry_run {
        crate::feecap::Ceiling::offline(c.allow_high_fee)
    } else {
        crate::feecap::Ceiling::read(&node, c.allow_high_fee)
    };
    if let Some(m) = ceiling.violation_of(&b) {
        return Err(format!("the release: {m} [{}]", crate::feecap::CODE));
    }
    if !c.dry_run {
        opts::confirm(c, &format!("the release of {} of {} to its creator", tokens(src.lock.amount as i128, &unit), to_hex(&cov)))?;
    }
    // Nobody signs a release: any key stands in for the sender.
    let nobody = crate::common::Signer { secret: [1; 32], pubkey: kob_protocol::tx::pubkey_of(&[1; 32]).map_err(|e| e.to_string())? };
    let info = json!({ "token": to_hex(&cov), "until": auto.until.to_string(), "kind": "auto" });
    crate::common::sign_validate_send(&b, &nobody, None, "", Value::Null, true, &node, &c.network)?;
    if c.dry_run {
        println!("dry run: built and executed in the script engine; nothing submitted");
        return Ok(());
    }
    let sent = crate::common::sign_validate_send(&b, &nobody, c.out_dir.as_deref(), "release.json", info, false, &node, &c.network)?;
    println!("submitted    release {}", sent.txid);
    Ok(())
}

/// The node's past median time (unix ms), the clock the node checks a lock time against.
fn median_time(node: &str) -> Result<u64, String> {
    let p = crate::wrpc::call(node, "getBlockDagInfo", json!({}), NODE_TIMEOUT).map_err(|e| crate::wrpc::node_failure(node, &e))?;
    match p.get("pastMedianTime") {
        Some(Value::Number(n)) => n.as_u64(),
        Some(Value::String(s)) => s.parse().ok(),
        _ => None,
    }
    .ok_or_else(|| format!("node {}: no pastMedianTime in getBlockDagInfo", crate::wrpc::shown(node)))
}

/// Run `kob token unlock`; returns the process exit code.
pub fn run_unlock(args: &[String]) -> i32 {
    if let Some(code) = crate::help::help_or_usage(args, &[UNLOCK_USAGE], true) {
        return code;
    }
    let parsed = match opts::parse(args, &["--outpoint"], &[]) {
        Ok(p) if p.pos.len() == 1 => p,
        Ok(_) => {
            eprintln!("error: give one token id or creation directory (see `kob token unlock --help`)");
            return crate::help::USAGE_EXIT;
        }
        Err(e) => {
            eprintln!("error: {e}");
            return crate::help::USAGE_EXIT;
        }
    };
    match unlock(&parsed, args) {
        Ok(()) => 0,
        Err(e) => {
            eprintln!("error: {e}");
            1
        }
    }
}

fn unlock(p: &opts::Parsed, argv: &[String]) -> Result<(), String> {
    let c = opts::Common::from(p)?;
    let src = load(&p.pos[0], c.indexer.as_deref())?;
    if src.lock.kind == LockKind::Auto {
        return release(p, &c, &src);
    }
    let lock = src.lock.lock();
    let signer = c.signer(argv)?;
    if signer.pubkey != lock.key {
        return Err(format!("the lock opens to the key {}, not to this key", to_hex(&lock.key)));
    }
    let node = c.need_node()?.to_string();
    crate::wrpc::check_node_network(&node, &c.network, Duration::from_secs(30))?;
    let cov = src.decl.covenant_id;
    let tpl = token_template(src.lock.program);
    let only = p.get("--outpoint").map(parse_outpoint).transpose()?;
    // The locked outputs still unspent, as the node holds them.
    let states: Vec<(u32, TokenState)> = src.lock.states().into_iter().map(|(i, s)| (i, TokenState::Kcc20(s))).collect();
    let spks: Vec<_> = states.iter().map(|(_, s)| s.spk_with(tpl)).collect();
    let at = lookup(&node, &c.network, &spks)?;
    let mut held: Vec<TokenUtxo> = vec![];
    for ((index, st), spk) in states.iter().zip(&spks) {
        if only.is_some_and(|(t, i)| t != src.decl.genesis_txid || i != *index) {
            continue;
        }
        let found = at
            .get(&spk_bytes(spk))
            .and_then(|v| v.iter().find(|u| u.txid == src.decl.genesis_txid && u.index == *index && u.covenant_id == Some(cov)));
        if let Some(u) = found {
            held.push(TokenUtxo { utxo: utxo_of(u), state: st.clone() });
        }
    }
    let unit =
        TokenUnit { ticker: src.decl.token.symbol.clone().unwrap_or_default(), decimals: src.decl.token.decimals.unwrap_or(0).into() };
    println!("token        {} ({})", to_hex(&cov), unit.ticker);
    println!(
        "lock         {} until {} ({} ms), key {}",
        tokens(src.lock.amount as i128, &unit),
        utc(lock.until),
        lock.until,
        to_hex(&lock.key)
    );
    println!("lock address {}", address_of(&lock.spk(), &c.network)?);
    if held.is_empty() {
        return Err(if only.is_some() {
            "that outpoint is not a locked output still unspent".into()
        } else {
            "nothing to unlock: the locked outputs are spent (already unlocked)".into()
        });
    }
    let median = median_time(&node)?;
    if median < lock.until && !c.dry_run {
        let left = (lock.until - median).div_ceil(60_000);
        return Err(format!("still locked: the node's past median time is {} ({left} min before the lock's time)", utc(median)));
    }
    // The coin: one already at the lock's address, else one sent there first.
    let coins = lookup(&node, &c.network, &[lock.spk()])?.remove(&spk_bytes(&lock.spk())).unwrap_or_default();
    let fee = c.fee_options();
    let (coin_tx, coin): (Option<BuiltTx>, Utxo) = match coins.iter().filter(|u| u.covenant_id.is_none()).max_by_key(|u| u.amount) {
        Some(u) => (None, utxo_of(u)),
        None => {
            let mut funding = maker_funding(&node, &c.network, &lock.key)?;
            funding.sort_by(|x, y| y.utxo.amount.cmp(&x.utxo.amount));
            funding.truncate(4);
            if funding.is_empty() {
                return Err("the key holds no KAS to send the lock coin (1 KAS) with".into());
            }
            let b = build_lock_coin(
                &LockCoin { lock, funding, amount: None, change: None, fee: fee.clone() },
                &kob_protocol::budget::lookup,
            )
            .map_err(|e| format!("the lock coin: {e}"))?;
            let coin = Utxo { transaction_id: b.tx.id, index: 0, amount: LOCK_COIN, block_daa_score: 0, covenant_id: None };
            (Some(b), coin)
        }
    };
    let req = UnlockCreatorLock {
        token: TokenRef { covenant_id: cov, program: src.lock.program },
        lock,
        tokens: held.clone(),
        coin,
        to: None,
        carrier: None,
        funding: vec![],
        change: None,
        fee,
    };
    let b = build_unlock(&req, &kob_protocol::budget::lookup).map_err(|e| format!("the unlock: {e}"))?;
    let amount: i64 = held.iter().map(|t| t.state.amount()).sum();
    match &coin_tx {
        Some(t) => println!("lock coin    {}  (1 KAS to the lock's address, fee {} sompi)", to_hex(&t.tx.id), t.fee.fee),
        None => println!(
            "lock coin    {}:{}  ({} sompi at the lock's address)",
            to_hex(&req.coin.transaction_id),
            req.coin.index,
            req.coin.amount
        ),
    }
    println!(
        "unlock       {}  ({} from {} output(s) to {}, lock time {}, fee {} sompi)",
        to_hex(&b.tx.id),
        tokens(amount as i128, &unit),
        held.len(),
        to_hex(&lock.key),
        lock.until,
        b.fee.fee
    );
    if median < lock.until {
        println!(
            "note         still locked: the node takes the unlock after {} (its past median time is {})",
            utc(lock.until),
            utc(median)
        );
    }
    let ceiling = if c.dry_run {
        crate::feecap::Ceiling::offline(c.allow_high_fee)
    } else {
        crate::feecap::Ceiling::read(&node, c.allow_high_fee)
    };
    for (what, t) in coin_tx.iter().map(|t| ("the lock coin", t)).chain([("the unlock", &b)]) {
        if let Some(m) = ceiling.violation_of(t) {
            return Err(format!("{what}: {m} [{}]", crate::feecap::CODE));
        }
    }
    if !c.dry_run {
        opts::confirm(&c, &format!("the unlock of {} of {}", tokens(amount as i128, &unit), to_hex(&cov)))?;
    }
    let dir = c.out_dir.as_deref();
    let info = json!({ "token": to_hex(&cov), "until": lock.until.to_string() });
    // Both are signed and run in the script engine before anything is sent.
    if let Some(t) = &coin_tx {
        crate::common::sign_validate_send(t, &signer, None, "", Value::Null, true, &node, &c.network)?;
    }
    crate::common::sign_validate_send(&b, &signer, None, "", Value::Null, true, &node, &c.network)?;
    if c.dry_run {
        println!("dry run: both signed and executed in the script engine; nothing submitted");
        return Ok(());
    }
    if let Some(t) = &coin_tx {
        let sent = crate::common::sign_validate_send(t, &signer, dir, "lock_coin.json", info.clone(), false, &node, &c.network)?;
        println!("submitted    lock coin {}", sent.txid);
    }
    let mut last = String::new();
    for attempt in 1..=6 {
        match crate::common::sign_validate_send(&b, &signer, dir, "unlock.json", info.clone(), false, &node, &c.network) {
            Ok(sent) => {
                println!("submitted    unlock {}", sent.txid);
                return Ok(());
            }
            Err(e) => {
                last = e;
                if attempt < 6 && coin_tx.is_some() {
                    std::thread::sleep(Duration::from_secs(2));
                } else {
                    break;
                }
            }
        }
    }
    Err(format!("the unlock was not accepted: {last}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lock_until_forms() {
        let now = 1_800_000_000_000;
        assert_eq!(parse_lock_until("+90d", now), Ok(now + 90 * DAY_MS));
        assert_eq!(parse_lock_until("+15m", now), Ok(now + 15 * 60_000));
        assert_eq!(parse_lock_until("2027-02-16", now).map(utc), Ok("2027-02-16 00:00 UTC".into()));
        assert_eq!(parse_lock_until("2027-02-16T13:45Z", now).map(utc), Ok("2027-02-16 13:45 UTC".into()));
        assert_eq!(parse_lock_until("2027-02-16T13:45:30Z", now), Ok(parse_lock_until("2027-02-16T13:45Z", now).unwrap() + 30_000));
        assert_eq!(parse_lock_until("1900000000000", now), Ok(1_900_000_000_000));
        for bad in ["90d", "+90y", "2027-02-30", "2027-2-16", "2027-02-16T13:45", "+d", "yesterday", "+0d"] {
            assert!(parse_lock_until(bad, now).is_err(), "{bad}");
        }
        assert!(parse_lock_until("2020-01-01", now).unwrap_err().contains("not in the future"));
        assert_eq!(civil_from_days(days_from_civil(2028, 2, 29)), (2028, 2, 29));
        assert_eq!(days_from_civil(1970, 1, 1), 0);
    }

    /// E-L6: a lock more than 10 years away is refused, and so is a unix time that is not in
    /// milliseconds (seconds, or microseconds that would lock the share until about the year 57,000).
    #[test]
    fn lock_until_has_a_ten_year_bound_and_takes_milliseconds_only() {
        let now = 1_800_000_000_000;
        assert_eq!(parse_lock_until("+3650d", now), Ok(now + 3_650 * DAY_MS));
        for far in ["+3700d", "+99999d", "+999999h", "2099-01-01", "2099-01-01T00:00Z", "9999999999999"] {
            let e = parse_lock_until(far, now).unwrap_err();
            assert!(e.contains("10 years"), "{far}: {e}");
        }
        for unit in ["1900000000", "1900000000000000", "19000000000000"] {
            let e = parse_lock_until(unit, now).unwrap_err();
            assert!(e.contains("milliseconds (13 digits)"), "{unit}: {e}");
        }
    }
}
