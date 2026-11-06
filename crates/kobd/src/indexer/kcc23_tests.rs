//! KCC-23 capture: candidates and launch proofs through the live path (follower, ingest, processor), reorgs, pruning, the
//! record log and a rebuild from it.

use super::*;
use crate::indexer::db::open_memory;
use crate::indexer::ingest::{Cursor, Ingest, IngestConfig};
use crate::indexer::record::{Extractor, TxRecord};
use crate::indexer::recordlog::{self, LogBatch, LogBlock, LogCursor, RecordLog};
use crate::rpc::types::{AddedBlock, ChainBlockHeader, VspcBatch};
use crate::testkit::{
    fresh_snapshot, h, kcc23_genesis, kcc23_prep, kcc23_txid, kcc23_wire, processor, snapshot, wide_window, Harness, MockNode,
    KCC23_EXAMPLE,
};
use crate::wire::{Reader, Writer};
use kaspa_consensus_core::tx::Transaction;

fn payload(json: &str) -> Vec<u8> {
    kcc23::encode(json).unwrap()
}

fn harness() -> Harness {
    Harness::new(open_memory("testnet-10").unwrap(), None)
}

fn count(hs: &Harness, table: &str) -> i64 {
    hs.query(&format!("SELECT COUNT(*) FROM {table}"), [])
}

fn proof_of(hs: &Harness, cov: &Hash32) -> Option<LaunchProof> {
    let g = hs.ingest.lock().unwrap();
    proof_json(g.conn(), cov).unwrap().map(|j| serde_json::from_str(&j).unwrap())
}

async fn push(hs: &Harness, txs: Vec<&Transaction>, full: bool) {
    hs.node.push_block(txs.into_iter().map(|t| kcc23_wire(t, full)).collect());
    hs.sync().await;
}

/// The proof a client gets verifies, and names the preparation and genesis that were accepted.
fn assert_declared(hs: &Harness, cov: &Hash32, prep: &Transaction, genesis: &Transaction) {
    let proof = proof_of(hs, cov).expect("a declaration");
    let v = proof.verify().unwrap();
    assert_eq!((v.preparation_txid, v.genesis_txid), (kcc23_txid(prep).0, kcc23_txid(genesis).0));
    assert_eq!((v.covenant_id, v.json.as_str()), (cov.0, KCC23_EXAMPLE));
    assert_eq!(v.token.decimals, Some(8));
    assert_eq!(proof, LaunchProof::of(prep, genesis, kaspa_consensus_core::Hash::from_bytes(cov.0)));
    let row: (Vec<u8>, Vec<u8>) = hs
        .ingest
        .lock()
        .unwrap()
        .conn()
        .query_row("SELECT preparation_txid, genesis_txid FROM kcc23_declarations WHERE covenant_id = ?1", [&cov.0[..]], |r| {
            Ok((r.get(0)?, r.get(1)?))
        })
        .unwrap();
    assert_eq!(row, (kcc23_txid(prep).0.to_vec(), kcc23_txid(genesis).0.to_vec()));
}

#[tokio::test]
async fn a_genesis_of_a_candidate_is_declared_in_the_same_batch_and_across_batches() {
    let hs = harness();
    // the same block (and batch): the preparation is accepted before the genesis that spends it
    let prep_a = kcc23_prep(1, payload(KCC23_EXAMPLE));
    let (gen_a, cov_a) = kcc23_genesis(&prep_a, 0, 1);
    push(&hs, vec![&prep_a, &gen_a], false).await;
    assert_declared(&hs, &cov_a, &prep_a, &gen_a);
    // across batches
    let prep_b = kcc23_prep(2, payload(KCC23_EXAMPLE));
    push(&hs, vec![&prep_b], false).await;
    assert_eq!((count(&hs, "kcc23_candidates"), count(&hs, "kcc23_declarations")), (2, 1));
    hs.node.push_empty(3);
    hs.sync().await;
    let (gen_b, cov_b) = kcc23_genesis(&prep_b, 0, 2);
    push(&hs, vec![&gen_b], false).await;
    assert_declared(&hs, &cov_b, &prep_b, &gen_b);
    // the genesis DAA is the accepting block's
    let daa: i64 = hs.query("SELECT genesis_daa FROM kcc23_declarations WHERE covenant_id = ?1", [&cov_b.0[..]]);
    assert_eq!(daa as u64, hs.node.tip_daa());
    // both declared; nothing of KOB was recorded for them
    assert_eq!(count(&hs, "kcc23_declarations"), 2);
    assert_eq!((count(&hs, "orders"), count(&hs, "rejects")), (0, 0));
    let g = hs.ingest.lock().unwrap();
    assert!(declared(g.conn(), &cov_a).unwrap() && !declared(g.conn(), &h("other", 0)).unwrap());
}

/// A transaction that spends a candidate's output into an existing covenant (a continuation) or under a covenant id that is not
/// the genesis id of that outpoint is no genesis: no proof, and nothing about it is recorded.
#[tokio::test]
async fn a_continuation_or_a_wrong_covenant_id_is_not_a_genesis() {
    let hs = harness();
    let prep = kcc23_prep(1, payload(KCC23_EXAMPLE));
    push(&hs, vec![&prep], false).await;
    // a continuation of covenant X: the authorising input spends the candidate's output, the binding names X
    let (mut cont, real) = kcc23_genesis(&prep, 0, 1);
    let x = h("existing-covenant", 1);
    cont.outputs[0].covenant = Some(kaspa_consensus_core::tx::CovenantBinding::new(0, kaspa_consensus_core::Hash::from_bytes(x.0)));
    {
        let g = hs.ingest.lock().unwrap();
        assert!(Extractor { conn: g.conn() }.extract(&kcc23_wire(&cont, false), 0).unwrap().is_none(), "not recorded at all");
    }
    // spending an output the preparation does not have, and spending it without any covenant output
    let (other_out, other_cov) = kcc23_genesis(&prep, 5, 2);
    let mut plain = cont.clone();
    plain.outputs[0].covenant = None;
    push(&hs, vec![&cont, &other_out, &plain], false).await;
    assert_eq!(count(&hs, "kcc23_declarations"), 0);
    for c in [x, real, other_cov] {
        assert!(proof_of(&hs, &c).is_none());
    }
    // the hand-built proof of the continuation is refused by the same check
    let forged = LaunchProof::of(&prep, &cont, kaspa_consensus_core::Hash::from_bytes(x.0));
    assert!(forged.verify().is_err());
}

/// Payloads that are not a declaration a reader takes, a preparation that is not version 1, and fields that do not hash to the
/// reported id are no candidates: the genesis that spends them declares nothing.
#[tokio::test]
async fn an_invalid_payload_or_unprovable_fields_make_no_candidate() {
    let hs = harness();
    let mut too_large = b"\0{\"d\":\"".to_vec();
    too_large.extend(vec![b'x'; kcc23::MAX_READ_PAYLOAD]);
    too_large.extend(b"\"}");
    let bad: Vec<Vec<u8>> = vec![
        b"\0[]".to_vec(),
        b"\0{\"a\":1,\"a\":2}".to_vec(),
        b"\x01{}".to_vec(),
        b"\0{}{}".to_vec(),
        b"\0\xff{}".to_vec(),
        too_large,
        b"KOB1".to_vec(),
    ];
    let txs: Vec<Transaction> = bad.into_iter().enumerate().map(|(i, p)| kcc23_prep(10 + i as u64, p)).collect();
    // version 0
    let mut v0 = kcc23_prep(30, payload(KCC23_EXAMPLE));
    v0.version = 0;
    // a non-zero lock time at `High`: the standard fields do not hash to the id
    let mut locked = kcc23_prep(31, payload(KCC23_EXAMPLE));
    locked.lock_time = 7;
    let geneses: Vec<Transaction> =
        txs.iter().chain([&v0, &locked]).enumerate().map(|(i, p)| kcc23_genesis(p, 0, 100 + i as u64).0).collect();
    hs.node.push_block(txs.iter().map(|t| kcc23_wire(t, false)).chain([kcc23_wire(&v0, false), kcc23_wire(&locked, false)]).collect());
    hs.sync().await;
    assert_eq!(count(&hs, "kcc23_candidates"), 0);
    push(&hs, geneses.iter().collect(), false).await;
    assert_eq!(count(&hs, "kcc23_declarations"), 0);
    // the fields a `Full` answer reports (a node other than the primary) are used as they are: version 0 is no preparation, a
    // reported lock time hashes to the id (the mock node, like a node, leaves them out at `High`)
    let g = hs.ingest.lock().unwrap();
    assert!(extract(g.conn(), &kcc23_wire(&v0, true)).unwrap().is_none());
    assert!(extract(g.conn(), &kcc23_wire(&locked, false)).unwrap().is_none());
    let d = extract(g.conn(), &kcc23_wire(&locked, true)).unwrap().expect("a candidate");
    assert!(d.preparation && d.binds.is_empty());
    assert_eq!(d.tx, ProofTx::of(&locked));
    // a candidate is bounded: a payload near the reader limit is one, with 2,000 outputs more it is not
    let mut big = kcc23_prep(33, payload(&format!("{{\"d\":\"{}\"}}", "x".repeat(16_000))));
    assert!(extract(g.conn(), &kcc23_wire(&big, false)).unwrap().is_some_and(|d| d.preparation));
    big.outputs = vec![big.outputs[0].clone(); 2_000];
    assert!(encode_tx(&ProofTx::of(&big)).len() > MAX_CANDIDATE_BYTES);
    assert!(extract(g.conn(), &kcc23_wire(&big, false)).unwrap().is_none());
}

#[tokio::test]
async fn a_reverted_block_takes_its_candidates_and_declarations_with_it() {
    let hs = harness();
    let prep = kcc23_prep(1, payload(KCC23_EXAMPLE));
    let (gen, cov) = kcc23_genesis(&prep, 0, 1);
    push(&hs, vec![&prep], false).await;
    push(&hs, vec![&gen], false).await;
    assert_declared(&hs, &cov, &prep, &gen);
    // the genesis block is reorged away: the declaration goes, the candidate stays
    hs.node.reorg(1, vec![vec![]]);
    hs.sync().await;
    assert!(proof_of(&hs, &cov).is_none());
    assert_eq!(count(&hs, "kcc23_candidates"), 1);
    assert_eq!(hs.snapshot(), fresh_snapshot(&hs.node.chain()));
    // accepted again on the new chain
    hs.node.reorg(1, vec![vec![], vec![kcc23_wire(&gen, false)]]);
    hs.sync().await;
    assert_declared(&hs, &cov, &prep, &gen);
    assert_eq!(hs.snapshot(), fresh_snapshot(&hs.node.chain()));
    // both blocks reorged away
    hs.node.reorg(3, vec![vec![]]);
    hs.sync().await;
    assert_eq!((count(&hs, "kcc23_candidates"), count(&hs, "kcc23_declarations")), (0, 0));
    assert_eq!(hs.snapshot(), fresh_snapshot(&hs.node.chain()));
}

/// Candidates no genesis spent go once older than the declaration window; one a declaration of the reorg window names stays
/// until that block is final. The declaration itself stays.
#[tokio::test]
async fn unspent_candidates_are_pruned_after_the_declaration_window() {
    let cfg = IngestConfig { reorg_window_daa: 6, checkpoint_daa: u64::MAX / 4, declaration_window_daa: 5, ..IngestConfig::default() };
    let hs = Harness::with_node(MockNode::new("testnet-10"), open_memory("testnet-10").unwrap(), None, cfg);
    let idle = kcc23_prep(1, payload(KCC23_EXAMPLE));
    let used = kcc23_prep(2, payload(KCC23_EXAMPLE));
    let (gen, cov) = kcc23_genesis(&used, 0, 2);
    push(&hs, vec![&idle, &used], false).await; // anchor + 1
    push(&hs, vec![&gen], false).await; // anchor + 2
    let has = |t: &Transaction| -> bool {
        hs.query::<i64>("SELECT COUNT(*) FROM kcc23_candidates WHERE txid = ?1", [&kcc23_txid(t).0[..]]) == 1
    };
    hs.node.push_empty(4); // cursor anchor + 6: nothing older than anchor + 1
    hs.sync().await;
    assert!(has(&idle) && has(&used));
    hs.node.push_empty(2); // cursor anchor + 8: the window starts at anchor + 3, the genesis block (anchor + 2) is still stored
    hs.sync().await;
    assert!(!has(&idle), "never spent: pruned");
    assert!(has(&used), "named by a declaration of a stored block");
    hs.node.push_empty(1); // cursor anchor + 9: blocks before anchor + 3 are final
    hs.sync().await;
    assert!(!has(&used));
    assert_declared(&hs, &cov, &used, &gen);
    // a genesis of the pruned candidate declares nothing
    let (late, late_cov) = kcc23_genesis(&idle, 0, 1);
    push(&hs, vec![&late], false).await;
    assert!(proof_of(&hs, &late_cov).is_none());
}

/// The record log carries the declaration sections of a genesis: a rebuild yields the same declarations. A candidate no genesis
/// has bound is in the database only (audit 2026-10-30 I2), so a rebuild does not have it: a genesis of it afterwards declares
/// nothing there, and the live database keeps declaring it.
#[tokio::test]
async fn candidates_and_declarations_survive_a_replay_from_the_record_log() {
    let dir = tempfile::tempdir().unwrap();
    let (log, _) = RecordLog::open(dir.path(), 1 << 20, 0).unwrap();
    let hs = Harness::new(open_memory("testnet-10").unwrap(), Some(log));
    let prep_a = kcc23_prep(1, payload(KCC23_EXAMPLE));
    let (gen_a, cov_a) = kcc23_genesis(&prep_a, 0, 1);
    let prep_b = kcc23_prep(2, payload(KCC23_EXAMPLE));
    let prep_c = kcc23_prep(3, payload(KCC23_EXAMPLE));
    let (gen_c, cov_c) = kcc23_genesis(&prep_c, 0, 3);
    push(&hs, vec![&prep_a, &gen_a], false).await;
    push(&hs, vec![&prep_b, &prep_c], false).await;
    push(&hs, vec![&gen_c], false).await;
    // a reorg of the genesis of C: logged as a revert, then accepted again
    hs.node.reorg(1, vec![vec![], vec![kcc23_wire(&gen_c, false)]]);
    hs.sync().await;
    assert_declared(&hs, &cov_c, &prep_c, &gen_c);
    let live = hs.snapshot();
    assert!(live.contains("kcc23_declaration ") && live.contains("kcc23_candidate "));
    drop(hs);

    let logged = recordlog::read_all(dir.path()).unwrap();
    assert!(logged.skipped.is_empty());
    let logged_txs: Vec<(u64, LogBatch)> = logged.records.iter().map(|(n, b)| (*n, b.clone())).collect();
    let mut ing = Ingest::new(open_memory("testnet-10").unwrap(), processor(), None).with_config(wide_window());
    ing.init_cursor(&Cursor { hash: logged.records[0].1.start, daa: 0 }).unwrap();
    ing.replay(logged.records).unwrap();
    let without_candidates = |s: &str| s.lines().filter(|l| !l.starts_with("kcc23_candidate ")).collect::<Vec<_>>().join("\n");
    assert_eq!(without_candidates(&snapshot(ing.conn())), without_candidates(&live));
    assert!(proof_json(ing.conn(), &cov_a).unwrap().is_some());
    // no frame of the log is a bare candidate: B was never bound
    for (_, batch) in &logged_txs {
        for b in &batch.blocks {
            for r in &b.txs {
                assert!(
                    !(r.is_declaration_only() && r.kcc23.as_ref().is_some_and(|d| !d.is_logged())),
                    "an unbound candidate was logged"
                );
            }
        }
    }

    // B is not a candidate in the rebuilt database: its genesis declares nothing there
    let (gen_b, cov_b) = kcc23_genesis(&prep_b, 0, 2);
    let cursor = ing.cursor().unwrap().unwrap();
    let header = ChainBlockHeader { hash: h("after-replay", 1), daa_score: cursor.daa + 1, ..ChainBlockHeader::default() };
    let batch = VspcBatch { removed: vec![], added: vec![AddedBlock { header, txs: vec![kcc23_wire(&gen_b, false)] }], wire_bytes: 0 };
    ing.apply_batch(cursor.hash, &batch).unwrap();
    assert!(proof_json(ing.conn(), &cov_b).unwrap().is_none());
}

/// A record's declaration section round-trips; a frame with one is format 3, any other frame stays format 2 byte for byte.
#[test]
fn the_declaration_section_in_the_record_log() {
    let prep = kcc23_prep(1, payload(KCC23_EXAMPLE));
    let (gen, _) = kcc23_genesis(&prep, 0, 1);
    let ptx = |t: &Transaction| proof_tx(&kcc23_wire(t, false)).expect("standard fields");
    let rec = |t: &Transaction, d: DeclSection| TxRecord::declaration_only(kcc23_txid(t), 3, d);
    let candidate = rec(&prep, DeclSection { tx: ptx(&prep), preparation: true, binds: vec![], refs: vec![] });
    let genesis = rec(&gen, DeclSection { tx: ptx(&gen), preparation: false, binds: vec![ptx(&prep)], refs: vec![] });
    for r in [&candidate, &genesis] {
        let mut w = Writer::new();
        r.encode(&mut w);
        let mut rd = Reader::new(&w.buf);
        assert_eq!(&TxRecord::decode(&mut rd).unwrap(), r);
        assert!(rd.done());
    }
    let batch = |txs: Vec<TxRecord>| LogBatch {
        start: h("start", 0),
        removed: vec![],
        cursor: LogCursor { hash: h("block", 0), daa: 5 },
        blocks: vec![LogBlock { hash: h("block", 0), daa: 5, blue: 5, ts: 1, txs }],
        ops: vec![],
    };
    let with = batch(vec![candidate.clone(), genesis.clone()]);
    let body = with.encode(9);
    assert_eq!(body[..2], [0, recordlog::FORMAT_KCC23]);
    assert_eq!(LogBatch::decode(&body).unwrap().batch, with);
    let mut plain = candidate.clone();
    plain.kcc23 = None;
    assert_eq!(batch(vec![plain]).encode(9)[..2], [0, recordlog::FORMAT]);
    // the database form of a candidate
    assert_eq!(decode_tx(&encode_tx(&ptx(&prep))).unwrap(), ptx(&prep));
    assert!(decode_tx(&[encode_tx(&ptx(&prep)), vec![0]].concat()).is_err());
}

/// A genesis of `k` covenant outputs, each authorised by its own input that spends output `i` of one preparation of `k` outputs.
fn wide_launch(k: u32, salt: u64) -> (Transaction, Transaction, Vec<Hash32>) {
    use kaspa_consensus_core::subnets::SUBNETWORK_ID_NATIVE;
    use kaspa_consensus_core::tx::{
        CovenantBinding as Binding, ScriptPublicKey, TransactionInput, TransactionOutpoint, TransactionOutput,
    };
    let mut prep = kcc23_prep(salt, payload(KCC23_EXAMPLE));
    prep.outputs = (0..k).map(|_| TransactionOutput::new(100_000_000, ScriptPublicKey::from_vec(0, vec![0x20, 0x51, 0xac]))).collect();
    prep.finalize();
    let pid = kcc23_txid(&prep);
    let spk =
        |i: u32| ScriptPublicKey::from_vec(0, [&[0xaa, 0x20][..], &h("wide-covenant", salt * 1000 + i as u64).0, &[0x87]].concat());
    let inputs = (0..k)
        .map(|i| TransactionInput::new(TransactionOutpoint::new(kaspa_consensus_core::Hash::from_bytes(pid.0), i), vec![], 0, 0))
        .collect();
    let outs = (0..k).map(|i| TransactionOutput::new(90_000_000, spk(i))).collect();
    let mut g = Transaction::new(1, inputs, outs, 0, SUBNETWORK_ID_NATIVE, 0, vec![]);
    let mut ids = vec![];
    for i in 0..k {
        let spent = g.inputs[i as usize].previous_outpoint;
        let id = kaspa_consensus_core::hashing::covenant_id::covenant_id(spent, [(i, &g.outputs[i as usize])].into_iter());
        g.outputs[i as usize].covenant = Some(Binding::new(i as u16, id));
        ids.push(Hash32(id.as_bytes()));
    }
    g.finalize();
    (prep, g, ids)
}

/// Audit 2026-10-30 I1: a genesis with K covenant outputs stored K proofs, each with the whole preparation and genesis (about
/// 100x the chain data at K = 40). The two transactions are stored once, however many covenant ids; every proof a client fetches
/// is still the whole, verifying document.
#[tokio::test]
async fn a_wide_genesis_stores_its_transactions_once() {
    let hs = harness();
    let k = 12;
    let (prep, gen, ids) = wide_launch(k, 7);
    push(&hs, vec![&prep], false).await;
    push(&hs, vec![&gen], false).await;
    assert_eq!(count(&hs, "kcc23_declarations"), k as i64);
    assert_eq!(count(&hs, "kcc23_txs"), 2);
    let stored: i64 = hs.query(
        "SELECT (SELECT COALESCE(SUM(LENGTH(proof)), 0) FROM kcc23_declarations) + (SELECT SUM(LENGTH(tx)) FROM kcc23_txs)",
        [],
    );
    let chain = (encode_tx(&proof_tx(&kcc23_wire(&prep, false)).unwrap()).len()
        + encode_tx(&proof_tx(&kcc23_wire(&gen, false)).unwrap()).len()) as i64;
    assert!(stored <= chain, "stored {stored} bytes for {chain} bytes of chain data");
    for (i, id) in ids.iter().enumerate() {
        let p = proof_of(&hs, id).expect("a declaration");
        assert_eq!(p.verify().unwrap().covenant_id, id.0, "covenant {i}");
        assert_eq!(p, LaunchProof::of(&prep, &gen, kaspa_consensus_core::Hash::from_bytes(id.0)));
    }
    // a reorg of the genesis takes the declarations and the transactions with it
    hs.node.reorg(1, vec![vec![]]);
    hs.sync().await;
    assert_eq!((count(&hs, "kcc23_declarations"), count(&hs, "kcc23_txs")), (0, 0));
}

/// Rows an earlier build wrote (the whole proof per covenant id) are read as they are and move to the shared form in bounded
/// steps, the document a client fetches unchanged byte for byte.
#[tokio::test]
async fn legacy_proofs_are_compacted_in_steps_without_changing_what_is_served() {
    let hs = harness();
    let (prep, gen, ids) = wide_launch(5, 9);
    push(&hs, vec![&prep], false).await;
    push(&hs, vec![&gen], false).await;
    let g = hs.ingest.lock().unwrap();
    let conn = g.conn();
    let served: Vec<String> = ids.iter().map(|id| proof_json(conn, id).unwrap().unwrap()).collect();
    // an earlier build's rows: the whole proof in the row, no shared transactions
    for (id, json) in ids.iter().zip(&served) {
        conn.execute("UPDATE kcc23_declarations SET proof = ?1 WHERE covenant_id = ?2", rusqlite::params![json, &id.0[..]]).unwrap();
    }
    conn.execute("DELETE FROM kcc23_txs", []).unwrap();
    let legacy: Vec<String> = ids.iter().map(|id| proof_json(conn, id).unwrap().unwrap()).collect();
    assert_eq!(legacy, served, "a legacy row is served as stored");
    let mut steps = vec![];
    loop {
        let n = compact_legacy(conn, 2).unwrap();
        if n == 0 {
            break;
        }
        steps.push(n);
        // between steps, every proof is still served, converted or not
        let now: Vec<String> = ids.iter().map(|id| proof_json(conn, id).unwrap().unwrap()).collect();
        assert_eq!(now, served);
    }
    assert_eq!(steps, vec![2, 2, 1]);
    let left: i64 = conn.query_row("SELECT COUNT(*) FROM kcc23_declarations WHERE proof != ''", [], |r| r.get(0)).unwrap();
    let txs: i64 = conn.query_row("SELECT COUNT(*) FROM kcc23_txs", [], |r| r.get(0)).unwrap();
    assert_eq!((left, txs), (0, 2));
}

/// Audit 2026-10-30 I2: a plain transaction with a `0x00{..}` payload of 16 KiB is a candidate preparation in the database
/// (a later genesis can bind it) but never a frame of the permanent record log; a genesis that binds it is.
#[tokio::test]
async fn a_junk_declaration_payload_stays_out_of_the_record_log() {
    let dir = tempfile::tempdir().unwrap();
    let (log, _) = RecordLog::open(dir.path(), 1 << 20, 0).unwrap();
    let hs = Harness::new(open_memory("testnet-10").unwrap(), Some(log));
    let mut body = br#"{"pad":""#.to_vec();
    body.resize(kcc23::MAX_READ_PAYLOAD - 3, b'a');
    body.extend_from_slice(br#""}"#);
    let junk = [&[0u8][..], &body].concat();
    assert_eq!(junk.len(), kcc23::MAX_READ_PAYLOAD);
    let tx = kcc23_prep(5, junk);
    push(&hs, vec![&tx], false).await;
    assert_eq!(count(&hs, "kcc23_candidates"), 1, "kept as a candidate");
    let logged = recordlog::read_all(dir.path()).unwrap();
    let bytes: usize = logged.records.iter().map(|(_, b)| b.encode(0).len()).sum();
    assert!(bytes < 1_000, "the 16 KiB payload reached the record log ({bytes} bytes)");
    // a genesis of it is declared, and logged with the preparation it binds
    let (gen, cov) = kcc23_genesis(&tx, 0, 5);
    push(&hs, vec![&gen], false).await;
    assert!(proof_of(&hs, &cov).is_some());
    let logged = recordlog::read_all(dir.path()).unwrap();
    let genesis_frames = logged
        .records
        .iter()
        .flat_map(|(_, b)| b.blocks.iter())
        .flat_map(|b| b.txs.iter())
        .filter(|r| r.kcc23.as_ref().is_some_and(|d| d.is_logged()))
        .count();
    assert_eq!(genesis_frames, 1);
}

#[path = "kcc23_bounds_tests.rs"]
mod bounds;
