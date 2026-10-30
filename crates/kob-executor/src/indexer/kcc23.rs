//! KCC-23 token declarations (kaspanet/kccs `kcc-0023.md`, Draft; `kob_protocol::kcc23`): the metadata JSON in the payload of
//! a version 1 *preparation transaction*, bound to a covenant id because the covenant's genesis spends an output of it.
//!
//! **Capture.** Every accepted transaction whose payload is a declaration (`0x00` and one strict I-JSON object of at most
//! [`kcc23::MAX_READ_PAYLOAD`] bytes) is kept as a *candidate preparation*: the fields its version 1 id commits to
//! ([`ProofTx`], [`MAX_CANDIDATE_BYTES`] at most), keyed by its id. An accepted transaction with a covenant output whose
//! authorising input spends an output of a candidate (accepted in the same batch or earlier) is a genesis candidate: the
//! launch proof of each covenant id it creates is built from the two transactions and kept only when
//! [`LaunchProof::verify`] accepts it (it recomputes both ids and the covenant id, so the continuation of an existing
//! covenant, whose id was not created from that outpoint, is refused). The first declaration of a covenant id stays (a
//! covenant id has one genesis).
//!
//! **Fields a `High` answer lacks.** VSPC v2 at `High` verbosity carries outpoints, sequences, outputs and payloads but not
//! the version, lock time, subnetwork or gas (`Full` does). They are taken as the standard ones (version 1, lock time 0, the
//! native subnetwork, gas 0) unless the node reported them, and the fields are kept only when they hash to the transaction
//! id the node reported: a transaction that hashes otherwise (a non-zero lock time, say) yields no candidate and no proof,
//! never a wrong one.
//!
//! **Reorgs and the record log.** Both kinds of row are stamped with the chain block that accepted the transaction and
//! deleted when it is reverted. The record of a genesis carries a declaration section ([`DeclSection`]: its own fields and the
//! preparations it binds), so a rebuild from the record log re-creates the proofs with the code of the live path. A candidate
//! preparation that no genesis has bound is kept in the database only, never in the permanent record log: any transaction
//! can carry a `0x00{..}` payload of 16 KiB, and the log is permanent and shipped to followers (audit 2026-10-30 I2).
//!
//! **Bounds (audit 2026-10-30 I7, I8).** A genesis binds at most [`MAX_BOUND_PREPARATIONS`] preparations and declares at most
//! [`MAX_BOUND_COVENANTS`] covenant ids ([`MAX_COVENANTS_TRIED`] are examined); the preparation and genesis of a pair are checked
//! once, then each covenant id costs one binding check. A preparation is written to the record log once, in the frame of the first
//! genesis that binds it; the frame of a later genesis names it by transaction id ([`DeclSection::refs`], resolved from
//! `kcc23_txs` where the earlier genesis put it). A record that is relevant to KOB for another reason (a token transfer) carries a
//! declaration section only when it binds: a preparation payload on it is a database candidate like any other.
//!
//! **Proofs.** A proof is stored once per transaction, not once per covenant id: the genesis and the preparations it binds are
//! rows of `kcc23_txs` (content-addressed by transaction id), and each covenant id's declaration row names them. The proof
//! JSON a client fetches is assembled on read ([`proof_json`]), byte for byte the document the covenant id's verified
//! [`LaunchProof`] serialises to. A genesis with K covenant outputs stored K proofs, each with the whole preparation and
//! genesis: about 100x the chain data (audit 2026-10-30 I1). Rows an earlier build wrote (the proof column non-empty) are
//! read as they are and moved to the shared form in bounded batches ([`compact_legacy`]). Candidates no genesis spent are pruned once older than the declaration window
//! (`IndexerConfig::declaration_window_hours`, 72 h by default), unless a declaration of the reorg window still names them
//! (a reverted genesis accepted again finds its preparation).

use super::db::{DbError, DbResult};
use crate::hex::Hash32;
use crate::rpc::types::Tx;
use crate::wire::{Reader, WireError, WireResult, Writer};
use kob_protocol::kcc23::{self, LaunchProof, ProofCovenant, ProofInput, ProofOutput, ProofSpk, ProofTx};
use rusqlite::{params, Connection, OptionalExtension};
use std::collections::HashSet;

/// Largest encoding of a candidate preparation (record log and database): its payload is at most
/// [`kcc23::MAX_READ_PAYLOAD`] bytes, its inputs and outputs get as much again. A token preparation of KOB is a few hundred
/// bytes; a larger transaction is not kept as a candidate.
pub const MAX_CANDIDATE_BYTES: usize = 2 * kcc23::MAX_READ_PAYLOAD;

/// Most preparations one genesis binds (the ones its covenant outputs' authorising inputs spend first, in output order).
pub const MAX_BOUND_PREPARATIONS: usize = 4;
/// Most covenant ids one genesis declares.
pub const MAX_BOUND_COVENANTS: usize = 32;
/// Most distinct (authorising input, covenant id) pairs of one transaction examined for a binding.
pub const MAX_COVENANTS_TRIED: usize = 256;

/// The subnetwork id of ordinary transactions.
const NATIVE_SUBNETWORK: [u8; 20] = [0; 20];

/// Section flags of a record ([`DeclSection::encode`]).
const PREPARATION: u8 = 1;
const GENESIS: u8 = 2;
/// Some of the bound preparations are named by transaction id instead of carried (an earlier frame has them).
const REFS: u8 = 4;

/// The declaration section of a record: the transaction's id-relevant fields, whether it is a candidate preparation, and the
/// candidate preparations its covenant outputs bind (a genesis; empty otherwise).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeclSection {
    /// The transaction's own fields (they hash to its id).
    pub tx: ProofTx,
    /// Its payload is a declaration: kept as a candidate preparation.
    pub preparation: bool,
    /// The preparations at least one of its covenant ids was verified against, that the record log does not hold yet.
    pub binds: Vec<ProofTx>,
    /// Ids of preparations a genesis binds that an earlier declaration already put in `kcc23_txs` (and so in an earlier frame of the
    /// log): not carried again.
    pub refs: Vec<[u8; 32]>,
}

impl DeclSection {
    /// Whether a record whose only content is this section goes to the permanent record log: a genesis does (it carries the
    /// preparations it binds, so a replay needs nothing else); a candidate no genesis has bound stays in the database only.
    pub fn is_logged(&self) -> bool {
        !self.binds.is_empty() || !self.refs.is_empty()
    }

    /// Record-log encoding: flags, the transaction, then (genesis) the preparations carried, then the ones named. A frame that names
    /// none is byte for byte what the first format wrote.
    pub fn encode(&self, w: &mut Writer) {
        let flags = if self.preparation { PREPARATION } else { 0 }
            | if self.is_logged() { GENESIS } else { 0 }
            | if self.refs.is_empty() { 0 } else { REFS };
        w.u8(flags);
        put_tx(w, &self.tx);
        if self.is_logged() {
            w.var(self.binds.len() as u64);
            for p in &self.binds {
                put_tx(w, p);
            }
        }
        if !self.refs.is_empty() {
            w.var(self.refs.len() as u64);
            for r in &self.refs {
                w.raw(r);
            }
        }
    }

    pub fn decode(r: &mut Reader<'_>) -> WireResult<DeclSection> {
        let flags = r.u8()?;
        if flags & !(PREPARATION | GENESIS | REFS) != 0 || flags == 0 || (flags & REFS != 0 && flags & GENESIS == 0) {
            return Err(WireError("declaration section flags"));
        }
        let tx = get_tx(r)?;
        let mut binds = Vec::new();
        let mut refs = Vec::new();
        if flags & GENESIS != 0 {
            let n = r.count(7)?;
            for _ in 0..n {
                binds.push(get_tx(r)?);
            }
            if flags & REFS != 0 {
                let n = r.count(32)?;
                if n == 0 {
                    return Err(WireError("preparation references"));
                }
                for _ in 0..n {
                    refs.push(r.raw(32)?.try_into().expect("32"));
                }
            }
            if binds.is_empty() && refs.is_empty() {
                return Err(WireError("genesis without a preparation"));
            }
        }
        Ok(DeclSection { tx, preparation: flags & PREPARATION != 0, binds, refs })
    }
}

/// Compact encoding of a [`ProofTx`].
pub fn put_tx(w: &mut Writer, t: &ProofTx) {
    w.var(t.version as u64);
    w.var(t.inputs.len() as u64);
    for i in &t.inputs {
        w.raw(&i.transaction_id);
        w.var(i.index as u64);
        w.var(i.sequence);
    }
    w.var(t.outputs.len() as u64);
    for o in &t.outputs {
        w.var(o.value);
        w.var(o.script_public_key.version as u64);
        w.bytes(&o.script_public_key.script);
        match &o.covenant {
            Some(c) => {
                w.u8(1);
                w.var(c.authorizing_input as u64);
                w.raw(&c.covenant_id);
            }
            None => w.u8(0),
        }
    }
    w.var(t.lock_time);
    w.bytes(&t.subnetwork_id);
    w.var(t.gas);
    w.bytes(&t.payload);
}

fn small<T: TryFrom<u64>>(v: u64, what: &'static str) -> WireResult<T> {
    T::try_from(v).map_err(|_| WireError(what))
}

pub fn get_tx(r: &mut Reader<'_>) -> WireResult<ProofTx> {
    let version = small(r.var()?, "transaction version")?;
    let n = r.count(34)?;
    let mut inputs = Vec::with_capacity(n);
    for _ in 0..n {
        let transaction_id: [u8; 32] = r.raw(32)?.try_into().expect("32");
        inputs.push(ProofInput { transaction_id, index: small(r.var()?, "input index")?, sequence: r.var()? });
    }
    let n = r.count(4)?;
    let mut outputs = Vec::with_capacity(n);
    for _ in 0..n {
        let value = r.var()?;
        let version = small(r.var()?, "script version")?;
        let script = r.bytes()?.to_vec();
        let covenant = match r.u8()? {
            0 => None,
            1 => Some(ProofCovenant {
                authorizing_input: small(r.var()?, "authorizing input")?,
                covenant_id: r.raw(32)?.try_into().expect("32"),
            }),
            _ => return Err(WireError("covenant tag")),
        };
        outputs.push(ProofOutput { value, script_public_key: ProofSpk { version, script }, covenant });
    }
    Ok(ProofTx {
        version,
        inputs,
        outputs,
        lock_time: r.var()?,
        subnetwork_id: r.bytes()?.to_vec(),
        gas: r.var()?,
        payload: r.bytes()?.to_vec(),
    })
}

/// [`put_tx`] into a buffer of its own (the database form of a candidate).
pub fn encode_tx(t: &ProofTx) -> Vec<u8> {
    let mut w = Writer::new();
    put_tx(&mut w, t);
    w.buf
}

/// The transaction of a [`encode_tx`] buffer (nothing may follow it).
pub fn decode_tx(b: &[u8]) -> WireResult<ProofTx> {
    let mut r = Reader::new(b);
    let t = get_tx(&mut r)?;
    if !r.done() {
        return Err(WireError("trailing bytes after a transaction"));
    }
    Ok(t)
}

/// The id of a [`ProofTx`] (`None`: a subnetwork id that is not 20 bytes).
pub fn proof_txid(t: &ProofTx) -> Option<Hash32> {
    t.to_tx().ok().map(|tx| Hash32(kaspa_consensus_core::hashing::tx::id(&tx).as_bytes()))
}

/// The id-relevant fields of an accepted transaction, the ones a `High` answer lacks taken as the standard ones (see the module
/// docs); `None` when they do not hash to the id the node reported.
pub fn proof_tx(tx: &Tx) -> Option<ProofTx> {
    let inputs = tx
        .inputs
        .iter()
        .map(|i| ProofInput {
            transaction_id: i.previous_outpoint.transaction_id.0,
            index: i.previous_outpoint.index,
            sequence: i.sequence.unwrap_or(0),
        })
        .collect();
    let outputs = tx
        .outputs
        .iter()
        .map(|o| {
            let spk = &o.script_public_key.0;
            let version = u16::from_be_bytes([*spk.first()?, *spk.get(1)?]);
            let covenant = match &o.covenant {
                Some(c) => {
                    Some(ProofCovenant { authorizing_input: u16::try_from(c.authorizing_input).ok()?, covenant_id: c.covenant_id.0 })
                }
                None => None,
            };
            Some(ProofOutput { value: o.value, script_public_key: ProofSpk { version, script: spk[2..].to_vec() }, covenant })
        })
        .collect::<Option<Vec<_>>>()?;
    let p = ProofTx {
        version: tx.version.unwrap_or(1),
        inputs,
        outputs,
        lock_time: tx.lock_time.unwrap_or(0),
        subnetwork_id: tx.subnetwork_id.as_ref().map(|s| s.0.clone()).unwrap_or_else(|| NATIVE_SUBNETWORK.to_vec()),
        gas: tx.gas.unwrap_or(0),
        payload: tx.payload.0.clone(),
    };
    (proof_txid(&p) == Some(tx.verbose_data.transaction_id)).then_some(p)
}

/// A payload a reader takes as a declaration, of a version 1 transaction.
pub fn is_declaration(version: u16, payload: &[u8]) -> bool {
    version == 1 && kcc23::looks_like_declaration(payload) && kcc23::decode_limited(payload, kcc23::MAX_READ_PAYLOAD).is_ok()
}

/// A covenant id `genesis` creates from an output of one of `preps`, verified.
pub struct Binding {
    /// Index into the preparations.
    pub prep: usize,
    pub covenant_id: Hash32,
    /// The recomputed id of the preparation.
    pub preparation_txid: [u8; 32],
}

/// Every covenant id of `genesis` (whose id is `genesis_id`) authorised by an input that spends an output of one of `preps` and bound
/// to it as [`LaunchProof::verify`] demands, at most [`MAX_BOUND_COVENANTS`] of them out of [`MAX_COVENANTS_TRIED`] examined.
///
/// The transactions are rebuilt and hashed, and the declaration decoded, once per preparation and genesis (not once per covenant
/// id: 41 ms at 100 ids, 241 ms at 300, audit 2026-10-30 I8); a covenant id then costs one binding check.
pub fn bindings(genesis: &ProofTx, genesis_id: &Hash32, preps: &[ProofTx]) -> Vec<Binding> {
    use kaspa_consensus_core::Hash;
    // per preparation, built when a covenant output first names it: `Some(None)` = it fails the id-independent checks
    type Built = Option<(kaspa_consensus_core::tx::Transaction, Hash)>;
    let ids: Vec<Option<Hash32>> = preps.iter().map(proof_txid).collect();
    let mut prepared: Vec<Option<Built>> = (0..preps.len()).map(|_| None).collect();
    let genesis_tx = match genesis.to_tx() {
        Ok(t) if proof_txid(genesis).as_ref() == Some(genesis_id) => t,
        _ => return Vec::new(),
    };
    let mut seen: HashSet<(u16, [u8; 32])> = HashSet::new();
    let mut out: Vec<Binding> = Vec::new();
    for c in genesis.outputs.iter().filter_map(|o| o.covenant.as_ref()) {
        if out.len() >= MAX_BOUND_COVENANTS || seen.len() >= MAX_COVENANTS_TRIED {
            break;
        }
        if !seen.insert((c.authorizing_input, c.covenant_id)) || out.iter().any(|b: &Binding| b.covenant_id.0 == c.covenant_id) {
            continue;
        }
        let Some(input) = genesis.inputs.get(c.authorizing_input as usize) else { continue };
        let Some(k) = ids.iter().position(|id| id.is_some_and(|id| id.0 == input.transaction_id)) else { continue };
        let built = prepared[k].get_or_insert_with(|| {
            let tx = preps[k].to_tx().ok()?;
            kcc23::check_launch(&tx, &genesis_tx).ok()?;
            Some((tx, Hash::from_bytes(input.transaction_id)))
        });
        match built {
            Some((tx, id)) if kcc23::is_bound(tx, *id, &genesis_tx, Hash::from_bytes(c.covenant_id)) => {
                out.push(Binding { prep: k, covenant_id: Hash32(c.covenant_id), preparation_txid: input.transaction_id })
            }
            _ => tracing::debug!(covenant_id = %Hash32(c.covenant_id), "not a KCC-23 genesis"),
        }
    }
    out
}

fn candidate(conn: &Connection, txid: &Hash32) -> DbResult<Option<ProofTx>> {
    let b: Option<Vec<u8>> =
        conn.prepare_cached("SELECT tx FROM kcc23_candidates WHERE txid = ?1")?.query_row([&txid.0[..]], |r| r.get(0)).optional()?;
    Ok(b.and_then(|b| decode_tx(&b).ok()))
}

/// The declaration section of an accepted transaction (`None`: neither a candidate preparation nor a verified genesis), looked
/// up against the candidates the database holds.
pub fn extract(conn: &Connection, tx: &Tx) -> DbResult<Option<DeclSection>> {
    let version = tx.version.unwrap_or(1);
    let prepares = is_declaration(version, &tx.payload.0);
    let mut preps = Vec::new();
    if version >= 1 {
        let mut asked: HashSet<Hash32> = HashSet::new();
        // bounded: one primary-key lookup per distinct spent transaction, and only so many preparations are carried
        for c in tx.outputs.iter().filter_map(|o| o.covenant.as_ref()).take(MAX_COVENANTS_TRIED) {
            if preps.len() >= MAX_BOUND_PREPARATIONS {
                break;
            }
            let Some(input) = tx.inputs.get(c.authorizing_input as usize) else { continue };
            let prev = input.previous_outpoint.transaction_id;
            if asked.insert(prev) {
                preps.extend(candidate(conn, &prev)?);
            }
        }
    }
    if !prepares && preps.is_empty() {
        return Ok(None);
    }
    let txid = tx.verbose_data.transaction_id;
    let Some(me) = proof_tx(tx) else {
        tracing::debug!(%txid, "a KCC-23 candidate whose fields do not hash to its id with the standard version, lock time, subnetwork and gas: not kept");
        return Ok(None);
    };
    let preparation = prepares && encode_tx(&me).len() <= MAX_CANDIDATE_BYTES;
    // the preparations a covenant id was verified against: carried in full the first time (nothing of the store names them), then by id
    let (mut binds, mut refs): (Vec<ProofTx>, Vec<[u8; 32]>) = (vec![], vec![]);
    if !preps.is_empty() {
        let found = bindings(&me, &txid, &preps);
        for p in preps.into_iter().enumerate().filter(|(k, _)| found.iter().any(|b| b.prep == *k)).map(|(_, p)| p) {
            match proof_txid(&p) {
                Some(id) if stored_tx(conn, &id.0)?.is_some() => refs.push(id.0),
                _ => binds.push(p),
            }
        }
    }
    if !preparation && binds.is_empty() && refs.is_empty() {
        return Ok(None);
    }
    Ok(Some(DeclSection { tx: me, preparation, binds, refs }))
}

/// A transaction of `kcc23_txs` (the ones declarations name).
fn stored_tx(conn: &Connection, id: &[u8; 32]) -> DbResult<Option<ProofTx>> {
    let b: Option<Vec<u8>> =
        conn.prepare_cached("SELECT tx FROM kcc23_txs WHERE txid = ?1")?.query_row([&id[..]], |r| r.get(0)).optional()?;
    Ok(b.and_then(|b| decode_tx(&b).ok()))
}

/// Applies a record's declaration section in chain block `seq` (DAA `daa`): the candidate, and the verified proofs of a genesis
/// (the first declaration of a covenant id stays). Returns the covenant ids that got a declaration.
pub fn apply(conn: &Connection, seq: i64, daa: u64, txid: &Hash32, d: &DeclSection) -> DbResult<Vec<Hash32>> {
    if d.preparation {
        conn.prepare_cached("INSERT OR IGNORE INTO kcc23_candidates (txid, tx, created_block, daa) VALUES (?1, ?2, ?3, ?4)")?
            .execute(params![&txid.0[..], encode_tx(&d.tx), seq, daa as i64])?;
    }
    let mut declared = Vec::new();
    if !d.is_logged() {
        return Ok(declared);
    }
    // the preparations the frame carries, then the ones it names (an earlier declaration stored them)
    let mut preps = d.binds.clone();
    for r in &d.refs {
        match stored_tx(conn, r)? {
            Some(p) => preps.push(p),
            None => {
                tracing::warn!(preparation = %Hash32(*r), genesis = %txid, "a KCC-23 genesis names a preparation the store lacks: no declaration")
            }
        }
    }
    for b in bindings(&d.tx, txid, &preps) {
        // the proof is assembled on read from the two transactions, stored once each (the proof column stays empty)
        let n = conn
            .prepare_cached(
                "INSERT OR IGNORE INTO kcc23_declarations (covenant_id, proof, preparation_txid, genesis_txid, genesis_block, genesis_daa) \
                 VALUES (?1, '', ?2, ?3, ?4, ?5)",
            )?
            .execute(params![&b.covenant_id.0[..], &b.preparation_txid[..], &txid.0[..], seq, daa as i64])?;
        if n > 0 {
            let mut put = conn.prepare_cached("INSERT OR IGNORE INTO kcc23_txs (txid, tx) VALUES (?1, ?2)")?;
            put.execute(params![&txid.0[..], encode_tx(&d.tx)])?;
            put.execute(params![&b.preparation_txid[..], encode_tx(&preps[b.prep])])?;
            declared.push(b.covenant_id);
        }
    }
    Ok(declared)
}

/// Deletes what chain block `seq` added (its candidates and declarations, and the transactions no declaration names any more).
/// Returns the declarations deleted (a candidate no genesis bound is not in the record log, so it does not make a revert
/// relevant to it).
pub fn revert(conn: &Connection, seq: i64) -> DbResult<usize> {
    conn.prepare_cached("DELETE FROM kcc23_candidates WHERE created_block = ?1")?.execute([seq])?;
    let named: Vec<Vec<u8>> = {
        let mut st = conn.prepare_cached(
            "SELECT genesis_txid FROM kcc23_declarations WHERE genesis_block = ?1 UNION SELECT preparation_txid FROM kcc23_declarations WHERE genesis_block = ?1",
        )?;
        let v = st.query_map([seq], |r| r.get(0))?.collect::<rusqlite::Result<_>>()?;
        v
    };
    let n = conn.prepare_cached("DELETE FROM kcc23_declarations WHERE genesis_block = ?1")?.execute([seq])?;
    for t in named {
        conn.prepare_cached(
            "DELETE FROM kcc23_txs WHERE txid = ?1 AND NOT EXISTS (SELECT 1 FROM kcc23_declarations WHERE genesis_txid = ?1) \
             AND NOT EXISTS (SELECT 1 FROM kcc23_declarations WHERE preparation_txid = ?1)",
        )?
        .execute([t])?;
    }
    Ok(n)
}

/// Deletes the candidates accepted before DAA `before` that no declaration of the stored chain blocks (the reorg window) names.
pub fn prune(conn: &Connection, before: u64) -> DbResult<usize> {
    Ok(conn
        .prepare_cached(
            "DELETE FROM kcc23_candidates WHERE daa < ?1 AND NOT EXISTS (SELECT 1 FROM kcc23_declarations d JOIN blocks b \
             ON b.seq = d.genesis_block WHERE d.preparation_txid = kcc23_candidates.txid)",
        )?
        .execute([before as i64])?)
}

/// The launch proof (JSON, `kob-kcc23-proof/1`) the indexer holds for a covenant id.
pub fn proof_json(conn: &Connection, covenant_id: &Hash32) -> DbResult<Option<String>> {
    let row: Option<(String, Vec<u8>, Vec<u8>)> = conn
        .prepare_cached("SELECT proof, preparation_txid, genesis_txid FROM kcc23_declarations WHERE covenant_id = ?1")?
        .query_row([&covenant_id.0[..]], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
        .optional()?;
    let Some((stored, prep, gen)) = row else { return Ok(None) };
    if !stored.is_empty() {
        return Ok(Some(stored)); // a row an earlier build wrote
    }
    let tx = |id: &[u8]| -> DbResult<ProofTx> {
        let b: Vec<u8> = conn
            .prepare_cached("SELECT tx FROM kcc23_txs WHERE txid = ?1")?
            .query_row([id], |r| r.get(0))
            .optional()?
            .ok_or_else(|| DbError::Invalid("a KCC-23 declaration names a transaction the store lacks".into()))?;
        decode_tx(&b).map_err(|e| DbError::Invalid(format!("stored KCC-23 transaction: {e}")))
    };
    let proof =
        LaunchProof { format: kcc23::PROOF_FORMAT.into(), covenant_id: covenant_id.0, preparation: tx(&prep)?, genesis: tx(&gen)? };
    Ok(Some(serde_json::to_string(&proof).map_err(|e| DbError::Invalid(format!("launch proof: {e}")))?))
}

/// Moves up to `limit` declarations an earlier build stored with their whole proof (once per covenant id) to the shared form: the
/// preparation and genesis go to `kcc23_txs` once, the proof column is emptied. A row whose proof does not parse, or does not
/// serialise back to the stored document, is left as it is. Each call is one short transaction, so a large store is converted in
/// steps (`compact_legacy` until it returns 0) and an interrupted run resumes. The pages the proofs held are reused by the
/// database; the file shrinks only on a `VACUUM`. Returns the rows converted.
pub fn compact_legacy(conn: &Connection, limit: usize) -> DbResult<usize> {
    let tx = conn.unchecked_transaction()?;
    let rows: Vec<(Vec<u8>, String)> = {
        let mut st = tx.prepare_cached("SELECT covenant_id, proof FROM kcc23_declarations WHERE proof != '' LIMIT ?1")?;
        let v = st.query_map([limit as i64], |r| Ok((r.get(0)?, r.get(1)?)))?.collect::<rusqlite::Result<_>>()?;
        v
    };
    let mut done = 0;
    for (cov, json) in rows {
        let Ok(p) = serde_json::from_str::<LaunchProof>(&json) else { continue };
        let (Some(pid), Some(gid)) = (proof_txid(&p.preparation), proof_txid(&p.genesis)) else { continue };
        // only a proof that assembles back to the very document that was stored is replaced (nothing is lost)
        if serde_json::to_string(&p).ok().as_deref() != Some(json.as_str()) {
            continue;
        }
        let mut put = tx.prepare_cached("INSERT OR IGNORE INTO kcc23_txs (txid, tx) VALUES (?1, ?2)")?;
        put.execute(params![&pid.0[..], encode_tx(&p.preparation)])?;
        put.execute(params![&gid.0[..], encode_tx(&p.genesis)])?;
        tx.prepare_cached(
            "UPDATE kcc23_declarations SET proof = '' WHERE covenant_id = ?1 AND preparation_txid = ?2 AND genesis_txid = ?3",
        )?
        .execute(params![cov, &pid.0[..], &gid.0[..]])?;
        done += 1;
    }
    tx.commit()?;
    Ok(done)
}

/// Whether the indexer holds a declaration for a covenant id.
pub fn declared(conn: &Connection, covenant_id: &Hash32) -> DbResult<bool> {
    Ok(conn.prepare_cached("SELECT 1 FROM kcc23_declarations WHERE covenant_id = ?1")?.exists([&covenant_id.0[..]])?)
}

/// The decimals the indexer's declaration of a covenant id names: `None` without a declaration, `Some(None)` for one that names no
/// usable `decimals`. The stored proof was verified when it was kept, so only the declaration's own payload is read again.
pub fn declaration_decimals(conn: &Connection, covenant_id: &Hash32) -> DbResult<Option<Option<u32>>> {
    let Some(json) = proof_json(conn, covenant_id)? else { return Ok(None) };
    let decimals = serde_json::from_str::<LaunchProof>(&json)
        .ok()
        .and_then(|p| kcc23::TokenDeclaration::from_payload(&p.preparation.payload).ok())
        .and_then(|d| d.decimals)
        .map(u32::from);
    Ok(Some(decimals))
}

#[cfg(test)]
#[path = "kcc23_tests.rs"]
mod tests;
