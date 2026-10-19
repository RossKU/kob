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
//! deleted when it is reverted. The record of such a transaction carries a declaration section ([`DeclSection`]: its own
//! fields, and for a genesis the preparations it binds), so a rebuild from the record log re-creates the candidates and the
//! proofs with the code of the live path. Candidates no genesis spent are pruned once older than the declaration window
//! (`IndexerConfig::declaration_window_hours`, 72 h by default), unless a declaration of the reorg window still names them
//! (a reverted genesis accepted again finds its preparation).

use super::db::{DbError, DbResult};
use crate::hex::Hash32;
use crate::rpc::types::Tx;
use crate::wire::{Reader, WireError, WireResult, Writer};
use kob_protocol::kcc23::{self, LaunchProof, ProofCovenant, ProofInput, ProofOutput, ProofSpk, ProofTx, VerifiedDeclaration};
use rusqlite::{params, Connection, OptionalExtension};
use std::collections::HashSet;

/// Largest encoding of a candidate preparation (record log and database): its payload is at most
/// [`kcc23::MAX_READ_PAYLOAD`] bytes, its inputs and outputs get as much again. A token preparation of KOB is a few hundred
/// bytes; a larger transaction is not kept as a candidate.
pub const MAX_CANDIDATE_BYTES: usize = 2 * kcc23::MAX_READ_PAYLOAD;

/// The subnetwork id of ordinary transactions.
const NATIVE_SUBNETWORK: [u8; 20] = [0; 20];

/// Section flags of a record ([`DeclSection::encode`]).
const PREPARATION: u8 = 1;
const GENESIS: u8 = 2;

/// The declaration section of a record: the transaction's id-relevant fields, whether it is a candidate preparation, and the
/// candidate preparations its covenant outputs bind (a genesis; empty otherwise).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeclSection {
    /// The transaction's own fields (they hash to its id).
    pub tx: ProofTx,
    /// Its payload is a declaration: kept as a candidate preparation.
    pub preparation: bool,
    /// The preparations at least one of its covenant ids was verified against.
    pub binds: Vec<ProofTx>,
}

impl DeclSection {
    /// Record-log encoding: flags, the transaction, then (genesis) the preparations.
    pub fn encode(&self, w: &mut Writer) {
        let flags = if self.preparation { PREPARATION } else { 0 } | if self.binds.is_empty() { 0 } else { GENESIS };
        w.u8(flags);
        put_tx(w, &self.tx);
        if !self.binds.is_empty() {
            w.var(self.binds.len() as u64);
            for p in &self.binds {
                put_tx(w, p);
            }
        }
    }

    pub fn decode(r: &mut Reader<'_>) -> WireResult<DeclSection> {
        let flags = r.u8()?;
        if flags & !(PREPARATION | GENESIS) != 0 || flags == 0 {
            return Err(WireError("declaration section flags"));
        }
        let tx = get_tx(r)?;
        let mut binds = Vec::new();
        if flags & GENESIS != 0 {
            let n = r.count(7)?;
            if n == 0 {
                return Err(WireError("genesis without a preparation"));
            }
            for _ in 0..n {
                binds.push(get_tx(r)?);
            }
        }
        Ok(DeclSection { tx, preparation: flags & PREPARATION != 0, binds })
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

/// A covenant id `genesis` creates from an output of one of `preps`, with its verified proof.
pub struct Binding {
    /// Index into the preparations.
    pub prep: usize,
    pub covenant_id: Hash32,
    pub proof: LaunchProof,
    pub verified: VerifiedDeclaration,
}

/// Every covenant id of `genesis` (whose id is `genesis_id`) authorised by an input that spends an output of one of `preps`,
/// with the launch proof, when [`LaunchProof::verify`] accepts it and the genesis it recomputes is `genesis_id`.
pub fn bindings(genesis: &ProofTx, genesis_id: &Hash32, preps: &[ProofTx]) -> Vec<Binding> {
    let ids: Vec<Option<Hash32>> = preps.iter().map(proof_txid).collect();
    let mut seen: HashSet<(u16, [u8; 32])> = HashSet::new();
    let mut out = Vec::new();
    for c in genesis.outputs.iter().filter_map(|o| o.covenant.as_ref()) {
        if !seen.insert((c.authorizing_input, c.covenant_id)) || out.iter().any(|b: &Binding| b.covenant_id.0 == c.covenant_id) {
            continue;
        }
        let Some(input) = genesis.inputs.get(c.authorizing_input as usize) else { continue };
        let Some(k) = ids.iter().position(|id| id.is_some_and(|id| id.0 == input.transaction_id)) else { continue };
        let proof = LaunchProof {
            format: kcc23::PROOF_FORMAT.into(),
            covenant_id: c.covenant_id,
            preparation: preps[k].clone(),
            genesis: genesis.clone(),
        };
        match proof.verify() {
            Ok(v) if v.genesis_txid == genesis_id.0 => {
                out.push(Binding { prep: k, covenant_id: Hash32(c.covenant_id), proof, verified: v })
            }
            Ok(_) => {}
            Err(e) => tracing::debug!(covenant_id = %Hash32(c.covenant_id), "not a KCC-23 genesis: {e}"),
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
        for c in tx.outputs.iter().filter_map(|o| o.covenant.as_ref()) {
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
    let binds: Vec<ProofTx> = if preps.is_empty() {
        vec![]
    } else {
        let found = bindings(&me, &txid, &preps);
        preps.into_iter().enumerate().filter(|(k, _)| found.iter().any(|b| b.prep == *k)).map(|(_, p)| p).collect()
    };
    if !preparation && binds.is_empty() {
        return Ok(None);
    }
    Ok(Some(DeclSection { tx: me, preparation, binds }))
}

/// Applies a record's declaration section in chain block `seq` (DAA `daa`): the candidate, and the verified proofs of a genesis
/// (the first declaration of a covenant id stays). Returns the covenant ids that got a declaration.
pub fn apply(conn: &Connection, seq: i64, daa: u64, txid: &Hash32, d: &DeclSection) -> DbResult<Vec<Hash32>> {
    if d.preparation {
        conn.prepare_cached("INSERT OR IGNORE INTO kcc23_candidates (txid, tx, created_block, daa) VALUES (?1, ?2, ?3, ?4)")?
            .execute(params![&txid.0[..], encode_tx(&d.tx), seq, daa as i64])?;
    }
    let mut declared = Vec::new();
    if d.binds.is_empty() {
        return Ok(declared);
    }
    for b in bindings(&d.tx, txid, &d.binds) {
        let json = serde_json::to_string(&b.proof).map_err(|e| DbError::Invalid(format!("launch proof: {e}")))?;
        let n = conn
            .prepare_cached(
                "INSERT OR IGNORE INTO kcc23_declarations (covenant_id, proof, preparation_txid, genesis_txid, genesis_block, genesis_daa) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            )?
            .execute(params![&b.covenant_id.0[..], json, &b.verified.preparation_txid[..], &txid.0[..], seq, daa as i64])?;
        if n > 0 {
            declared.push(b.covenant_id);
        }
    }
    Ok(declared)
}

/// Deletes what chain block `seq` added (its candidates and declarations). Returns the rows deleted.
pub fn revert(conn: &Connection, seq: i64) -> DbResult<usize> {
    Ok(conn.prepare_cached("DELETE FROM kcc23_candidates WHERE created_block = ?1")?.execute([seq])?
        + conn.prepare_cached("DELETE FROM kcc23_declarations WHERE genesis_block = ?1")?.execute([seq])?)
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
    Ok(conn
        .prepare_cached("SELECT proof FROM kcc23_declarations WHERE covenant_id = ?1")?
        .query_row([&covenant_id.0[..]], |r| r.get(0))
        .optional()?)
}

/// Whether the indexer holds a declaration for a covenant id.
pub fn declared(conn: &Connection, covenant_id: &Hash32) -> DbResult<bool> {
    Ok(conn.prepare_cached("SELECT 1 FROM kcc23_declarations WHERE covenant_id = ?1")?.exists([&covenant_id.0[..]])?)
}

#[cfg(test)]
#[path = "kcc23_tests.rs"]
mod tests;
