//! Recovery hooks for the case the chain data is gone (downtime beyond the node's pruning window,
//! a lost database with no record log): maker-side order export/import and gap reconciliation.
//!
//! Trust model: only AVAILABILITY of the order parameters is trusted, never their content. An
//! imported order is accepted only if the P2SH script public key rebuilt from a pinned template
//! and the supplied state span equals the script public key of an unspent output the NODE reports
//! (`getUtxosByAddresses`, needs `--utxoindex`) carrying a covenant id. A wrong or forged export
//! matches no output and is reported as `not_found`. For an ask-side order the export may carry the
//! extension commitment of its custody token (the placement record's custody part): the indexer then
//! rebuilds the custody script (`amountLeft` base units owned by the order id) and verifies the custody UTXO
//! the same way; without it the order is imported with unverified custody.
//!
//! The order parameters exist only in the genesis transaction's placement record, so makers (and the
//! UI or CLI that placed the order) keep an "order receipt" in exactly this export format; `index
//! export-orders` produces it from a healthy indexer, and archive services (api.kaspa.org, kascov)
//! can be turned into the same format.

use crate::hex::{Hash32, HexBytes};
use crate::indexer::db::DbResult;
use crate::indexer::ingest::{Cursor, Ingest, IngestError};
use crate::indexer::processor::OpReport;
use crate::indexer::recordlog::{ImportCustody, LogOp};
use crate::rpc::types::AddressUtxo;
use crate::rpc::{ChainSource, RpcError};
use crate::script::{spk_address, spk_bytes};
use kob_protocol::artifacts::{template_by_hash, token_template_by_hash};
use kob_protocol::state::{AnyState, TokenState};
use rusqlite::{Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};

pub const EXPORT_VERSION: u32 = 2;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MakerExport {
    pub version: u32,
    pub network: String,
    #[serde(default)]
    pub orders: Vec<ExportedOrder>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExportedOrder {
    /// Full template hash of the order contract version.
    pub template_hash: Hash32,
    /// State span of the CURRENT script of the order (hex).
    pub state: HexBytes,
    /// Ask-side kinds: extension commitment of the custody token (placement record, custody part; a pair order: of its first
    /// custody, `AnyState::custodies`).
    #[serde(default)]
    pub extension_commitment: Option<Hash32>,
    /// A sell-first `KobIfdPair`: extension commitment of its second custody (the B prefund; default: the state's `bExt`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prefund_extension_commitment: Option<Hash32>,
    #[serde(default)]
    pub covenant_id: Option<Hash32>,
    /// Informational.
    #[serde(default)]
    pub genesis_txid: Option<Hash32>,
    #[serde(default)]
    pub status: Option<String>,
    /// If-done exits: the covenant id of the entry that booked them (an import links the exit to it when the entry is known,
    /// so the position stays one). Informational for every other check.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent: Option<Hash32>,
}

#[derive(Debug, thiserror::Error)]
pub enum RecoverError {
    #[error("node: {0}")]
    Rpc(#[from] RpcError),
    #[error(transparent)]
    Ingest(#[from] IngestError),
    #[error("database: {0}")]
    Db(#[from] crate::indexer::db::DbError),
    #[error("database: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("{0}")]
    Invalid(String),
}

#[derive(Debug, Default, Clone, Serialize)]
pub struct ImportReport {
    pub imported: u64,
    pub already_known: u64,
    /// Entries for which the node has no unspent output at the rebuilt script (spent, wrong state, or forged).
    pub not_found: Vec<String>,
    pub rejected: Vec<String>,
    /// Imported ask-side orders whose custody token UTXO was not verified (no extension commitment in
    /// the export, or the node has no such UTXO).
    pub custody_unverified: Vec<String>,
}

/// Export the orders of `maker` (all makers when `None`) from the database. An order whose current UTXO the indexer could not
/// prove a state for (`state_known = 0`) is left out (logged): its genesis state names a script that no longer exists, which
/// an import could only report `not_found`.
pub fn export_orders(conn: &Connection, maker: Option<&[u8]>, live_only: bool) -> DbResult<MakerExport> {
    let network = crate::indexer::db::meta_get(conn, "network")?.unwrap_or_default();
    let mut st = conn.prepare(
        "SELECT o.template_hash, CASE WHEN u.txid IS NULL THEN o.genesis_state ELSE u.state END, o.ext_commit, o.covenant_id, o.genesis_txid, s.status, o.parent
         FROM orders o JOIN order_state s ON s.covenant_id = o.covenant_id
         LEFT JOIN order_utxos u ON u.txid = s.cur_txid AND u.idx = s.cur_idx
         WHERE (?1 IS NULL OR o.maker = ?1) AND (?2 = 0 OR s.status IN ('open', 'partial'))
         ORDER BY o.genesis_block, o.covenant_id",
    )?;
    let rows = st.query_map(rusqlite::params![maker, live_only as i64], |r| {
        let th: Vec<u8> = r.get(0)?;
        let state: Option<Vec<u8>> = r.get(1)?;
        let ext: Option<Vec<u8>> = r.get(2)?;
        let cov: Vec<u8> = r.get(3)?;
        let gtx: Option<Vec<u8>> = r.get(4)?;
        Ok(ExportedOrder {
            template_hash: Hash32::from_slice(&th).unwrap_or_default(),
            state: HexBytes(state.unwrap_or_default()),
            extension_commitment: ext.and_then(|e| Hash32::from_slice(&e)),
            covenant_id: Hash32::from_slice(&cov),
            genesis_txid: gtx.and_then(|g| Hash32::from_slice(&g)),
            status: r.get(5)?,
            parent: r.get::<_, Option<Vec<u8>>>(6)?.and_then(|p| Hash32::from_slice(&p)),
            prefund_extension_commitment: None,
        })
    })?;
    let mut orders = Vec::new();
    for o in rows {
        let o = o?;
        let mut o = o;
        if o.state.0.is_empty() {
            tracing::warn!(order = ?o.covenant_id, "export: the current state of this order is not proven; left out");
            continue;
        }
        o.prefund_extension_commitment = prefund_ext(conn, &o)?;
        orders.push(o);
    }
    Ok(MakerExport { version: EXPORT_VERSION, network, orders })
}

/// The extension commitment of a pair order's second custody (a sell-first entry's B prefund): the proven state of its latest
/// custody UTXO of that token.
fn prefund_ext(conn: &Connection, o: &ExportedOrder) -> DbResult<Option<Hash32>> {
    let (Some(cov), Some(t)) = (o.covenant_id, template_by_hash(&o.template_hash.0)) else { return Ok(None) };
    let Ok(state) = AnyState::decode(t.id, &o.state.0) else { return Ok(None) };
    let Some((token, _)) = state.custodies().get(1).copied() else { return Ok(None) };
    let row: Option<(String, Vec<u8>)> = conn
        .query_row(
            "SELECT program, state FROM token_holdings WHERE owner = ?1 AND token_cov_id = ?2 AND role = 'custody' \
             ORDER BY created_block DESC, idx DESC LIMIT 1",
            rusqlite::params![&cov.0[..], &token[..]],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?;
    Ok(row.and_then(|(p, s)| {
        let tpl = kob_protocol::artifacts::TemplateId::from_name(&p).filter(|t| t.is_token())?;
        TokenState::decode_with(kob_protocol::artifacts::token_template(tpl), &s).ok().map(|ts| Hash32(ts.extension()))
    }))
}

/// The custody scripts an exported order must have (token-holding kinds; a pair order one per custody): (script, address,
/// amount, extension commitment, token covenant id). Empty without the extension commitment of the first custody.
fn custody_scripts(state: &AnyState, e: &ExportedOrder, cov: Hash32, network: &str) -> Vec<(Vec<u8>, String, i64, Hash32, Hash32)> {
    let Some(ext) = e.extension_commitment else { return vec![] };
    let amounts: Vec<i64> = if state.is_pair() {
        state.custodies().into_iter().map(|c| c.1).collect()
    } else {
        state.custody_amount().into_iter().collect()
    };
    let parts = crate::indexer::processor::custody_part_tokens(state);
    let mut out = vec![];
    for (k, (amount, (token, tpl))) in amounts.into_iter().zip(parts).enumerate() {
        if amount <= 0 {
            continue;
        }
        let Some(tt) = tpl.and_then(|h| token_template_by_hash(&h)).filter(|t| state.is_pair() || t.family == state.family()) else {
            continue;
        };
        let ext = match (k, state) {
            (0, _) => ext,
            (_, AnyState::KobIfdPair(i)) => e.prefund_extension_commitment.unwrap_or(Hash32(i.b_ext)),
            _ => continue,
        };
        let cs = TokenState::custody(tt.family, amount, cov.0, ext.0);
        let cspk = spk_bytes(&cs.spk_with(tt));
        if let Some(a) = spk_address(&cspk, network) {
            out.push((cspk, a, amount, ext, token));
        }
    }
    out
}

fn lock(i: &Arc<Mutex<Ingest>>) -> std::sync::MutexGuard<'_, Ingest> {
    i.lock().unwrap_or_else(|e| e.into_inner())
}

async fn fetch_utxos<S: ChainSource>(source: &S, addrs: &[String]) -> Result<Vec<AddressUtxo>, RpcError> {
    let mut out = Vec::new();
    for chunk in addrs.chunks(50) {
        out.extend(source.utxos_by_addresses(chunk).await?);
    }
    Ok(out)
}

/// Verify the exported orders against the node and import those that are live.
pub async fn import_orders<S: ChainSource>(
    ingest: &Arc<Mutex<Ingest>>,
    source: &S,
    network: &str,
    export: &MakerExport,
) -> Result<ImportReport, RecoverError> {
    if export.version != 1 && export.version != EXPORT_VERSION {
        return Err(RecoverError::Invalid(format!("unsupported export version {}", export.version)));
    }
    if export.network != network {
        return Err(RecoverError::Invalid(format!("export is for `{}`, indexer runs on `{network}`", export.network)));
    }
    let mut report = ImportReport::default();
    struct Cand {
        spk: Vec<u8>,
        addr: String,
        state: AnyState,
        entry: ExportedOrder,
        /// Custody scripts (token-holding kinds; a pair order one per custody) when the export carries the extension
        /// commitment: (script, address, amount, extension commitment, token).
        custody: Vec<(Vec<u8>, String, i64, Hash32, Hash32)>,
    }
    let mut cands = Vec::new();
    for e in &export.orders {
        let label = e.covenant_id.map(|c| c.to_hex()).unwrap_or_else(|| "(no covenant id)".into());
        let Some(t) = template_by_hash(&e.template_hash.0).filter(|t| crate::model::is_order(t.id)) else {
            report.rejected.push(format!("{label}: unknown order template {}", e.template_hash));
            continue;
        };
        let Ok(state) = AnyState::decode(t.id, &e.state.0) else {
            report.rejected.push(format!("{label}: state does not decode as {}", t.id.name()));
            continue;
        };
        let spk = spk_bytes(&state.spk());
        let Some(addr) = spk_address(&spk, network) else {
            report.rejected.push(format!("{label}: script has no address"));
            continue;
        };
        let custody = match e.covenant_id {
            Some(cov) => custody_scripts(&state, e, cov, network),
            None => vec![],
        };
        cands.push(Cand { spk, addr, state, entry: e.clone(), custody });
    }
    let mut addrs: HashSet<String> = cands.iter().map(|c| c.addr.clone()).collect();
    addrs.extend(cands.iter().flat_map(|c| c.custody.iter().map(|x| x.1.clone())));
    let addrs: Vec<String> = addrs.into_iter().collect();
    let utxos = fetch_utxos(source, &addrs).await?;
    let mut ops = Vec::new();
    for c in cands {
        let label = c.entry.covenant_id.map(|x| x.to_hex()).unwrap_or_else(|| c.addr.clone());
        let matches: Vec<&AddressUtxo> = utxos
            .iter()
            .filter(|u| u.utxo_entry.script_public_key.0 == c.spk && u.utxo_entry.covenant_id.is_some())
            .filter(|u| c.entry.covenant_id.is_none() || u.utxo_entry.covenant_id == c.entry.covenant_id)
            .collect();
        if matches.is_empty() {
            report.not_found.push(label);
            continue;
        }
        for u in matches {
            let cov = u.utxo_entry.covenant_id.expect("filtered");
            let found: Vec<Option<ImportCustody>> = c
                .custody
                .iter()
                .map(|(cspk, _, amount, ext, want_tok)| {
                    utxos
                        .iter()
                        .find(|x| x.utxo_entry.script_public_key.0 == *cspk && x.utxo_entry.covenant_id == Some(*want_tok))
                        .map(|x| ImportCustody {
                            txid: x.outpoint.transaction_id,
                            index: x.outpoint.index,
                            value: x.utxo_entry.amount,
                            amount: *amount,
                            ext: *ext,
                        })
                })
                .collect();
            let custody = found.first().cloned().flatten();
            let prefund = found.get(1).cloned().flatten();
            let wanted = if c.state.is_pair() {
                c.state.custodies().iter().filter(|x| x.1 > 0).count()
            } else {
                usize::from(c.state.holds_tokens() && c.state.amount_left().unwrap_or(0) > 0)
            };
            if found.iter().flatten().count() < wanted {
                report.custody_unverified.push(cov.to_hex());
            }
            ops.push(LogOp::Import {
                template: c.state.template_id(),
                state: c.entry.state.0.clone(),
                covenant_id: cov,
                txid: u.outpoint.transaction_id,
                index: u.outpoint.index,
                value: u.utxo_entry.amount,
                spk: c.spk.clone(),
                daa: u.utxo_entry.block_daa_score,
                custody,
                parent: c.entry.parent,
                prefund,
            });
        }
    }
    if !ops.is_empty() {
        let ing = ingest.clone();
        let applied = tokio::task::spawn_blocking(move || lock(&ing).apply_ops(ops, None)).await.expect("import task panicked")?;
        report.imported = applied.ops.imported;
        report.already_known += applied.ops.already_known;
        report.rejected.extend(applied.ops.rejected);
    }
    Ok(report)
}

/// A live token UTXO owned by an order, with the script public key its proven state produces.
struct OwnedToken {
    owner: Hash32,
    txid: Hash32,
    idx: u32,
    spk: Vec<u8>,
}

/// The live token UTXOs (custody and strays) owned by `covs` whose script the indexer can rebuild: from the proven holding
/// state, or for a custody without one from its amount, the order and its extension commitment. A UTXO with neither is not
/// returned (the reconciliation cannot check it, so it is closed).
fn owned_token_scripts(conn: &Connection, covs: &[Hash32]) -> Result<Vec<OwnedToken>, RecoverError> {
    use kob_protocol::artifacts::{token_template, TemplateId};
    let mut out = Vec::new();
    let mut st = conn.prepare(
        "SELECT t.txid, t.idx, t.role, t.amount, h.program, h.state, o.ext_commit, o.token_tpl_hash, o.family \
         FROM token_utxos t LEFT JOIN token_holdings h ON h.txid = t.txid AND h.idx = t.idx LEFT JOIN orders o ON o.covenant_id = t.owner \
         WHERE t.owner = ?1 AND t.spent_block IS NULL",
    )?;
    for cov in covs {
        type Row = (Vec<u8>, u32, String, i64, Option<String>, Option<Vec<u8>>, Option<Vec<u8>>, Option<Vec<u8>>, Option<i64>);
        let rows: Vec<Row> = st
            .query_map([&cov.0[..]], |r| {
                Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?, r.get(6)?, r.get(7)?, r.get(8)?))
            })?
            .collect::<Result<_, _>>()?;
        for (txid, idx, role, amount, program, state, ext, tpl, family) in rows {
            let proven = program.as_deref().and_then(TemplateId::from_name).filter(|t| t.is_token()).zip(state).and_then(|(t, s)| {
                TokenState::decode_with(token_template(t), &s).ok().map(|ts| spk_bytes(&ts.spk_with(token_template(t))))
            });
            let rebuilt = || {
                if role != "custody" {
                    return None;
                }
                let tt = tpl.as_deref().and_then(|h| <[u8; 32]>::try_from(h).ok()).and_then(|h| token_template_by_hash(&h))?;
                if family.is_some_and(|f| f != tt.family.code() as i64) {
                    return None;
                }
                let ext = ext.as_deref().and_then(|e| <[u8; 32]>::try_from(e).ok()).unwrap_or([0; 32]);
                Some(spk_bytes(&TokenState::custody(tt.family, amount, cov.0, ext).spk_with(tt)))
            };
            if let Some(spk) = proven.or_else(rebuilt) {
                out.push(OwnedToken { owner: *cov, txid: Hash32::from_slice(&txid).unwrap_or_default(), idx, spk });
            }
        }
    }
    Ok(out)
}

/// A custody script, the units it holds, the token covenant id and the extension commitment.
type CustodyScript = (Vec<u8>, i64, Hash32, Hash32);

/// The custody script of an adopted order's state (`spk` is its script): ask-side kinds with an amount left whose token program
/// this build pins and whose extension commitment the order row stores (KRON needs none). Returns (custody script, amount,
/// token covenant id, extension commitment).
fn adopted_custody_script(conn: &Connection, cov: &Hash32, spk: &[u8]) -> Result<Option<CustodyScript>, RecoverError> {
    use kob_protocol::artifacts::TemplateId;
    let row: Option<(String, Option<Vec<u8>>)> = conn
        .query_row("SELECT contract, ext_commit FROM orders WHERE covenant_id = ?1", [&cov.0[..]], |r| Ok((r.get(0)?, r.get(1)?)))
        .optional()?;
    let Some((contract, ext)) = row else { return Ok(None) };
    let Some(tid) = TemplateId::from_name(&contract) else { return Ok(None) };
    let state: Option<Vec<u8>> = conn
        .query_row(
            "SELECT state FROM order_utxos WHERE covenant_id = ?1 AND spk = ?2 AND state IS NOT NULL ORDER BY created_block DESC LIMIT 1",
            rusqlite::params![&cov.0[..], spk],
            |r| r.get(0),
        )
        .optional()?;
    let Some(state) = state.and_then(|s| AnyState::decode(tid, &s).ok()) else { return Ok(None) };
    let Some(amount) = state.custody_amount().filter(|a| *a > 0) else { return Ok(None) };
    // the first custody's token and program (a pair order: the first of `AnyState::custodies`, either token)
    let Some((token, tpl)) = crate::indexer::processor::custody_part_tokens(&state).into_iter().next() else { return Ok(None) };
    let Some(tt) = tpl.and_then(|h| token_template_by_hash(&h)).filter(|t| state.is_pair() || t.family == state.family()) else {
        return Ok(None);
    };
    let ext = match ext.as_deref().and_then(|e| <[u8; 32]>::try_from(e).ok()) {
        Some(e) => e,
        None if tt.family == kob_protocol::Family::Kron => [0; 32],
        None => return Ok(None),
    };
    let cspk = spk_bytes(&TokenState::custody(tt.family, amount, cov.0, ext).spk_with(tt));
    Ok(Some((cspk, amount, token, Hash32(ext))))
}

/// Bring the tracked order outputs in line with the node's UTXO set and move the cursor to
/// `new_cursor`. Used by `index rebase` after a gap: outputs the node no longer has are closed with
/// reason `gap`, and outputs that continued under the same script are adopted.
pub async fn reconcile_open_orders<S: ChainSource>(
    ingest: &Arc<Mutex<Ingest>>,
    source: &S,
    network: &str,
    new_cursor: Cursor,
) -> Result<OpReport, RecoverError> {
    struct Tracked {
        txid: Hash32,
        idx: u32,
        cov: Hash32,
        spk: Vec<u8>,
    }
    let tracked: Vec<Tracked> = {
        let g = lock(ingest);
        let mut st =
            g.conn().prepare("SELECT txid, idx, covenant_id, spk FROM order_utxos WHERE spent_block IS NULL ORDER BY txid, idx")?;
        let rows = st.query_map([], |r| {
            let t: Vec<u8> = r.get(0)?;
            let c: Vec<u8> = r.get(2)?;
            Ok(Tracked {
                txid: Hash32::from_slice(&t).unwrap_or_default(),
                idx: r.get(1)?,
                cov: Hash32::from_slice(&c).unwrap_or_default(),
                spk: r.get(3)?,
            })
        })?;
        rows.collect::<Result<Vec<_>, _>>()?
    };
    let mut addr_of: HashMap<Vec<u8>, String> = HashMap::new();
    for t in &tracked {
        if !addr_of.contains_key(&t.spk) {
            let a = spk_address(&t.spk, network).ok_or_else(|| RecoverError::Invalid("tracked script has no address".into()))?;
            addr_of.insert(t.spk.clone(), a);
        }
    }
    let addrs: Vec<String> = addr_of.values().cloned().collect();
    let utxos = fetch_utxos(source, &addrs).await?;
    let mut by_spk: HashMap<&[u8], Vec<&AddressUtxo>> = HashMap::new();
    for u in &utxos {
        by_spk.entry(&u.utxo_entry.script_public_key.0).or_default().push(u);
    }
    let tracked_outpoints: HashSet<(Hash32, u32)> = tracked.iter().map(|t| (t.txid, t.idx)).collect();
    let mut adoptions = Vec::new();
    let mut ops = Vec::new();
    let mut used: HashSet<(Hash32, u32)> = HashSet::new();
    for t in &tracked {
        let live = by_spk.get(t.spk.as_slice()).map(|v| v.as_slice()).unwrap_or(&[]);
        if live.iter().any(|u| u.outpoint.transaction_id == t.txid && u.outpoint.index == t.idx) {
            continue;
        }
        let successor = live.iter().find(|u| {
            u.utxo_entry.covenant_id == Some(t.cov)
                && !tracked_outpoints.contains(&(u.outpoint.transaction_id, u.outpoint.index))
                && !used.contains(&(u.outpoint.transaction_id, u.outpoint.index))
        });
        match successor {
            Some(u) => {
                used.insert((u.outpoint.transaction_id, u.outpoint.index));
                adoptions.push((t, *u));
            }
            None => ops.push(LogOp::CloseGap { txid: t.txid, index: t.idx, covenant_id: t.cov }),
        }
    }
    // An adopted order (same script, so the same state) keeps the token UTXOs it owns that the node still has, and its
    // custody is looked up at the script the state derives (an ask partially filled and refilled in the gap holds a new one).
    let owned = if adoptions.is_empty() {
        Vec::new()
    } else {
        let covs: Vec<Hash32> = adoptions.iter().map(|(t, _)| t.cov).collect();
        let g = lock(ingest);
        owned_token_scripts(g.conn(), &covs)?
    };
    let mut token_addrs: HashSet<String> = HashSet::new();
    let mut want: Vec<(Hash32, Option<CustodyScript>)> = Vec::new();
    for (t, _) in &adoptions {
        // the custody script of the adopted state (ask-side kinds holding tokens, extension commitment known)
        let cust = {
            let g = lock(ingest);
            adopted_custody_script(g.conn(), &t.cov, &t.spk)?
        };
        if let Some((cspk, ..)) = &cust {
            if let Some(a) = spk_address(cspk, network) {
                token_addrs.insert(a);
            }
        }
        want.push((t.cov, cust));
    }
    for o in &owned {
        if let Some(a) = spk_address(&o.spk, network) {
            token_addrs.insert(a);
        }
    }
    let token_utxos =
        if token_addrs.is_empty() { Vec::new() } else { fetch_utxos(source, &token_addrs.into_iter().collect::<Vec<_>>()).await? };
    let node_has = |txid: &Hash32, idx: u32, spk: &[u8]| {
        token_utxos
            .iter()
            .any(|x| x.outpoint.transaction_id == *txid && x.outpoint.index == idx && x.utxo_entry.script_public_key.0 == spk)
    };
    for ((t, u), (_, cust)) in adoptions.iter().zip(want) {
        let keep: Vec<(Hash32, u32)> =
            owned.iter().filter(|o| o.owner == t.cov && node_has(&o.txid, o.idx, &o.spk)).map(|o| (o.txid, o.idx)).collect();
        let custody = cust.and_then(|(cspk, amount, token, ext)| {
            token_utxos
                .iter()
                .filter(|x| x.utxo_entry.script_public_key.0 == cspk && x.utxo_entry.covenant_id == Some(token))
                .min_by_key(|x| (x.utxo_entry.block_daa_score, x.outpoint.transaction_id.0, x.outpoint.index))
                .map(|x| ImportCustody {
                    txid: x.outpoint.transaction_id,
                    index: x.outpoint.index,
                    value: x.utxo_entry.amount,
                    amount,
                    ext,
                })
        });
        ops.push(LogOp::Adopt {
            covenant_id: t.cov,
            old_txid: t.txid,
            old_index: t.idx,
            txid: u.outpoint.transaction_id,
            index: u.outpoint.index,
            value: u.utxo_entry.amount,
            spk: t.spk.clone(),
            daa: Some(u.utxo_entry.block_daa_score),
            custody,
            keep,
        });
    }
    let ing = ingest.clone();
    let applied =
        tokio::task::spawn_blocking(move || lock(&ing).apply_ops(ops, Some(new_cursor))).await.expect("reconcile task panicked")?;
    Ok(applied.ops)
}
