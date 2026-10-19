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

/// The record log carries the declaration sections: a rebuild yields the same rows, and a candidate still unspent at the end
/// of the log is declared by a genesis the rebuilt database applies afterwards.
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
    let mut ing = Ingest::new(open_memory("testnet-10").unwrap(), processor(), None).with_config(wide_window());
    ing.init_cursor(&Cursor { hash: logged.records[0].1.start, daa: 0 }).unwrap();
    ing.replay(logged.records).unwrap();
    assert_eq!(snapshot(ing.conn()), live);
    assert!(proof_json(ing.conn(), &cov_a).unwrap().is_some());

    // B is still a candidate in the rebuilt database: its genesis is declared there
    let (gen_b, cov_b) = kcc23_genesis(&prep_b, 0, 2);
    let cursor = ing.cursor().unwrap().unwrap();
    let header = ChainBlockHeader { hash: h("after-replay", 1), daa_score: cursor.daa + 1, ..ChainBlockHeader::default() };
    let batch = VspcBatch { removed: vec![], added: vec![AddedBlock { header, txs: vec![kcc23_wire(&gen_b, false)] }], wire_bytes: 0 };
    ing.apply_batch(cursor.hash, &batch).unwrap();
    let v: LaunchProof = serde_json::from_str(&proof_json(ing.conn(), &cov_b).unwrap().unwrap()).unwrap();
    assert_eq!(v.verify().unwrap().preparation_txid, kcc23_txid(&prep_b).0);
}

/// A record's declaration section round-trips; a frame with one is format 3, any other frame stays format 2 byte for byte.
#[test]
fn the_declaration_section_in_the_record_log() {
    let prep = kcc23_prep(1, payload(KCC23_EXAMPLE));
    let (gen, _) = kcc23_genesis(&prep, 0, 1);
    let ptx = |t: &Transaction| proof_tx(&kcc23_wire(t, false)).expect("standard fields");
    let rec = |t: &Transaction, d: DeclSection| TxRecord::declaration_only(kcc23_txid(t), 3, d);
    let candidate = rec(&prep, DeclSection { tx: ptx(&prep), preparation: true, binds: vec![] });
    let genesis = rec(&gen, DeclSection { tx: ptx(&gen), preparation: false, binds: vec![ptx(&prep)] });
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
