//! A small local record of the token outputs the key's own transactions created (its token change, a merge's output, a
//! payment to itself), so the next command can spend them before the indexer has seen the transaction: two sells in a row
//! within the indexer's lag found no tokens ("you hold 0"), the change of the first being unknown to the indexer.
//!
//! The record only names candidates. Every one is used only when the node holds that exact outpoint unspent, with that
//! covenant id, at the script its state makes (the same check every token UTXO from the indexer goes through), so a stale,
//! lost or edited record can hide nothing and add nothing. Entries are per network and owner key, kept for a day, at most
//! [`MAX_ENTRIES`]. Writing it never fails a command: a transaction that was submitted stays submitted.
//!
//! The file is `recent-token-outputs.json` in `$KOB_STATE_DIR`, else the platform's per-user state directory
//! (`%LOCALAPPDATA%\kob`, `$XDG_STATE_HOME/kob`, `~/.local/state/kob`).

use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use kob_protocol::json::to_hex;
use kob_protocol::state::TokenState;
use kob_protocol::tx::{spk_to_string, BuiltTx, SigPlan};
use serde_json::{json, Value};

/// The environment variable naming the directory of the record.
pub const STATE_DIR_ENV: &str = "KOB_STATE_DIR";
/// Most entries kept.
pub const MAX_ENTRIES: usize = 256;
/// How long an entry is kept (seconds): far beyond any indexer lag.
pub const KEEP_SECS: u64 = 24 * 3600;
const FILE: &str = "recent-token-outputs.json";

/// One token output a transaction of the key created.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    /// Network.
    pub network: String,
    /// Owner key (x-only, hex).
    pub owner: String,
    /// Token covenant id (hex).
    pub token: String,
    /// Transaction id (hex).
    pub txid: String,
    /// Output index.
    pub index: u32,
    /// The output's token state.
    pub state: TokenState,
    /// When it was recorded (UTC unix seconds).
    pub at: u64,
}

fn now() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

/// The directory of the record (`None`: no state directory is known).
pub fn state_dir() -> Option<PathBuf> {
    if cfg!(test) {
        // one per test thread: the tests of a process do not share a record
        let t = format!("{:?}", std::thread::current().id()).replace(|c: char| !c.is_ascii_alphanumeric(), "");
        return Some(std::env::temp_dir().join(format!("kob-cli-test-state-{}-{t}", std::process::id())));
    }
    let env = |k: &str| std::env::var_os(k).filter(|v| !v.is_empty()).map(PathBuf::from);
    if let Some(d) = env(STATE_DIR_ENV) {
        return Some(d);
    }
    if cfg!(windows) {
        return env("LOCALAPPDATA").map(|d| d.join("kob"));
    }
    env("XDG_STATE_HOME").map(|d| d.join("kob")).or_else(|| env("HOME").map(|h| h.join(".local").join("state").join("kob")))
}

impl Entry {
    fn to_json(&self) -> Value {
        json!({"network": self.network, "owner": self.owner, "token": self.token, "txid": self.txid, "index": self.index,
               "state": self.state, "at": self.at})
    }
    fn from_json(v: &Value) -> Option<Entry> {
        let s = |k: &str| v.get(k).and_then(Value::as_str).map(str::to_string);
        Some(Entry {
            network: s("network")?,
            owner: s("owner")?,
            token: s("token")?,
            txid: s("txid")?,
            index: u32::try_from(v.get("index")?.as_u64()?).ok()?,
            state: serde_json::from_value(v.get("state")?.clone()).ok()?,
            at: v.get("at")?.as_u64()?,
        })
    }
}

/// The entries of the record file (unreadable entries skipped, an unreadable file: none).
fn load_from(path: &std::path::Path) -> Vec<Entry> {
    let v: Value = std::fs::read_to_string(path).ok().and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_default();
    v.as_array().map(|a| a.iter().filter_map(Entry::from_json).collect()).unwrap_or_default()
}

/// The token outputs of `built` that `owner` holds as plain user outputs: the next states of its
/// token transfers, matched to the outputs by the script each state makes and by covenant id.
pub fn own_token_outputs(built: &BuiltTx, owner: &[u8; 32]) -> Vec<([u8; 32], u32, TokenState)> {
    let mut out = vec![];
    for p in &built.plans {
        let (program, states): (_, Vec<TokenState>) = match p {
            SigPlan::TokenLeader { template, next_states, .. } => {
                (*template, next_states.iter().cloned().map(TokenState::from).collect())
            }
            SigPlan::KronToken { template, next_states, .. } => {
                (*template, next_states.iter().cloned().map(TokenState::from).collect())
            }
            _ => continue,
        };
        let Some(tt) = kob_protocol::artifacts::try_token_template(program) else { continue };
        for st in states.into_iter().filter(|s| s.is_user() && s.owner() == *owner) {
            let spk = spk_to_string(&st.spk_with(tt));
            for (i, o) in built.tx.outputs.iter().enumerate() {
                if o.script_public_key == spk {
                    if let Some(c) = &o.covenant {
                        if !out.iter().any(|(_, j, _)| *j == i as u32) {
                            out.push((c.covenant_id, i as u32, st.clone()));
                        }
                    }
                }
            }
        }
    }
    out
}

/// Record the key's own token outputs of a submitted transaction. Never fails: a problem is a warning.
pub fn record(network: &str, owner: &[u8; 32], built: &BuiltTx, txid: &str) {
    let outs = own_token_outputs(built, owner);
    if outs.is_empty() {
        return;
    }
    let Some(dir) = state_dir() else { return };
    let path = dir.join(FILE);
    let t = now();
    let mut all: Vec<Entry> = load_from(&path).into_iter().filter(|e| e.at + KEEP_SECS > t && e.at <= t + 3600).collect();
    for (token, index, state) in outs {
        all.push(Entry {
            network: network.to_string(),
            owner: to_hex(owner),
            token: to_hex(&token),
            txid: txid.to_string(),
            index,
            state,
            at: t,
        });
    }
    let skip = all.len().saturating_sub(MAX_ENTRIES);
    let all = &all[skip..];
    let write = || -> std::io::Result<()> {
        std::fs::create_dir_all(&dir)?;
        let tmp = dir.join(format!("{FILE}.{}.tmp", std::process::id()));
        let doc = Value::Array(all.iter().map(Entry::to_json).collect());
        std::fs::write(&tmp, serde_json::to_string_pretty(&doc).unwrap_or_default())?;
        std::fs::rename(&tmp, &path)
    };
    if let Err(e) = write() {
        eprintln!("warning: cannot keep the record of this transaction's token outputs in {}: {e}", path.display());
    }
}

/// The recorded token outputs of `token` that `owner` holds on `network` (candidates: the caller confirms each on the node):
/// (state, txid, index).
pub fn candidates(network: &str, owner: &[u8; 32], token: &[u8; 32]) -> Vec<(TokenState, [u8; 32], u32)> {
    let Some(dir) = state_dir() else { return vec![] };
    let (owner, token) = (to_hex(owner), to_hex(token));
    load_from(&dir.join(FILE))
        .into_iter()
        .filter(|e| e.network == network && e.owner == owner && e.token == token && e.state.is_user())
        .filter_map(|e| Some((e.state, kob_protocol::registry::parse_hex32(&e.txid)?, e.index)))
        .collect()
}
