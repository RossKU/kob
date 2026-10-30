//! Audit 2026-10-30 I7 and I8: the record log carries a preparation once however many geneses bind it, one genesis costs one
//! verification of its preparation, and the numbers of preparations and covenant ids a transaction can have declared are bounded.

use super::*;
use kaspa_consensus_core::subnets::SUBNETWORK_ID_NATIVE;
use kaspa_consensus_core::tx::{
    CovenantBinding as Binding, ScriptPublicKey, TransactionInput, TransactionOutpoint, TransactionOutput,
};

/// A valid declaration of `size` bytes (a padding field).
fn big_declaration(size: usize) -> Vec<u8> {
    let mut body = br#"{"decimals":8,"pad":""#.to_vec();
    body.resize(size - 1 - 2, b'a');
    body.extend_from_slice(br#""}"#);
    let p = [&[0u8][..], &body].concat();
    assert_eq!(p.len(), size);
    p
}

/// A preparation of `outputs` outputs with a declaration of `size` bytes.
fn multi_prep(outputs: u32, salt: u64, size: usize) -> Transaction {
    let mut prep = kcc23_prep(salt, big_declaration(size));
    prep.outputs =
        (0..outputs).map(|_| TransactionOutput::new(100_000_000, ScriptPublicKey::from_vec(0, vec![0x20, 0x51, 0xac]))).collect();
    prep.finalize();
    prep
}

fn log_bytes(dir: &std::path::Path) -> usize {
    let logged = recordlog::read_all(dir).unwrap();
    logged.records.iter().map(|(_, b)| b.encode(0).len()).sum()
}

fn wire_len(t: &Transaction) -> usize {
    encode_tx(&proof_tx(&kcc23_wire(t, false)).unwrap()).len()
}

/// A genesis of `k` covenant outputs, each authorised by its own input that spends output `i` of `prep`.
fn wide_genesis(prep: &Transaction, k: u32, salt: u64) -> (Transaction, Vec<Hash32>) {
    let pid = kcc23_txid(prep);
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
    (g, ids)
}

/// I7 (a): a thousand small geneses of one 16 KiB preparation wrote the preparation into every genesis frame (about 150 times the
/// chain data). It is written once; the later frames name it, and a rebuild from the log declares every covenant id all the same.
#[tokio::test]
async fn geneses_of_one_preparation_carry_it_in_the_record_log_once() {
    let dir = tempfile::tempdir().unwrap();
    let (log, _) = RecordLog::open(dir.path(), 1 << 20, 0).unwrap();
    let hs = Harness::new(open_memory("testnet-10").unwrap(), Some(log));
    let n = 40u32;
    let prep = multi_prep(n, 70, kcc23::MAX_READ_PAYLOAD);
    push(&hs, vec![&prep], false).await;
    let geneses: Vec<(Transaction, Hash32)> = (0..n).map(|i| kcc23_genesis(&prep, i, 700 + i as u64)).collect();
    for (g, _) in &geneses {
        push(&hs, vec![g], false).await;
    }
    for (g, cov) in &geneses {
        let v = proof_of(&hs, cov).expect("a declaration").verify().unwrap();
        assert_eq!((v.covenant_id, v.genesis_txid), (cov.0, kcc23_txid(g).0));
    }
    let chain: usize = wire_len(&prep) + geneses.iter().map(|(g, _)| wire_len(g)).sum::<usize>();
    let logged = log_bytes(dir.path());
    assert!(logged < chain * 2, "{logged} bytes of record log for {chain} bytes of chain data (was about 150x)");
    // the frames: the first genesis carries the preparation, every later one names it
    let all = recordlog::read_all(dir.path()).unwrap();
    let sections: Vec<DeclSection> =
        all.records.iter().flat_map(|(_, b)| b.blocks.iter()).flat_map(|b| b.txs.iter()).filter_map(|r| r.kcc23.clone()).collect();
    assert_eq!(sections.len(), n as usize);
    assert_eq!((sections[0].binds.len(), sections[0].refs.len()), (1, 0));
    assert!(sections[1..].iter().all(|s| s.binds.is_empty() && s.refs == vec![kcc23_txid(&prep).0]));
    let live = hs.snapshot();
    drop(hs);
    // a rebuild from the log declares every covenant id the live path did
    let mut ing = Ingest::new(open_memory("testnet-10").unwrap(), processor(), None).with_config(wide_window());
    ing.init_cursor(&Cursor { hash: all.records[0].1.start, daa: 0 }).unwrap();
    ing.replay(all.records).unwrap();
    let without_candidates = |s: &str| s.lines().filter(|l| !l.starts_with("kcc23_candidate ")).collect::<Vec<_>>().join("\n");
    assert_eq!(without_candidates(&snapshot(ing.conn())), without_candidates(&live));
    for (_, cov) in &geneses {
        assert!(proof_json(ing.conn(), cov).unwrap().is_some());
    }
}

/// A frame with references is not a frame the first format could read; one without references is byte for byte the first
/// format's (a follower that has not been upgraded reads every frame that names nothing).
#[test]
fn the_reference_section_round_trips_and_a_frame_without_references_keeps_its_bytes() {
    let prep = kcc23_prep(1, payload(KCC23_EXAMPLE));
    let (gen, _) = kcc23_genesis(&prep, 0, 1);
    let ptx = |t: &Transaction| proof_tx(&kcc23_wire(t, false)).expect("standard fields");
    let plain = DeclSection { tx: ptx(&gen), preparation: false, binds: vec![ptx(&prep)], refs: vec![] };
    let by_ref = DeclSection { tx: ptx(&gen), preparation: false, binds: vec![], refs: vec![kcc23_txid(&prep).0] };
    let both = DeclSection { tx: ptx(&gen), preparation: true, binds: vec![ptx(&prep)], refs: vec![[7; 32], [8; 32]] };
    for d in [&plain, &by_ref, &both] {
        let mut w = Writer::new();
        d.encode(&mut w);
        let mut r = Reader::new(&w.buf);
        assert_eq!(&DeclSection::decode(&mut r).unwrap(), d);
        assert!(r.done());
    }
    // the first format: flags, the transaction, a count, the preparations
    let mut w = Writer::new();
    plain.encode(&mut w);
    let mut old = Writer::new();
    old.u8(GENESIS);
    put_tx(&mut old, &plain.tx);
    old.var(1);
    put_tx(&mut old, &plain.binds[0]);
    assert_eq!(w.buf, old.buf);
    // a named-only frame without the genesis flag, or a genesis with nothing to bind, is refused
    for flags in [REFS, GENESIS] {
        let mut w = Writer::new();
        w.u8(flags);
        put_tx(&mut w, &plain.tx);
        w.var(0);
        assert!(DeclSection::decode(&mut Reader::new(&w.buf)).is_err(), "{flags}");
    }
}

/// I8: 300 covenant ids of one genesis were each verified with the whole preparation rebuilt, hashed and decoded (241 ms at 300, in
/// the single-threaded ingest pass). The pair is checked once; at most 32 ids are declared and 256 examined.
#[test]
fn a_wide_genesis_costs_one_verification_and_declares_a_bounded_number_of_ids() {
    let prep = multi_prep(300, 80, kcc23::MAX_READ_PAYLOAD);
    let (gen, ids) = wide_genesis(&prep, 300, 80);
    let p = proof_tx(&kcc23_wire(&prep, false)).unwrap();
    let g = proof_tx(&kcc23_wire(&gen, false)).unwrap();
    let gid = kcc23_txid(&gen);
    let t = std::time::Instant::now();
    let found = bindings(&g, &gid, std::slice::from_ref(&p));
    let took = t.elapsed();
    assert_eq!(found.len(), MAX_BOUND_COVENANTS);
    assert!(found.iter().all(|b| ids.contains(&b.covenant_id) && b.preparation_txid == kcc23_txid(&prep).0));
    eprintln!("bindings: 300 covenant outputs, 16 KiB preparation: {took:?}");
    assert!(took < std::time::Duration::from_millis(60), "{took:?} for one genesis (241 ms before)");
    // the rebuilt pair is checked once whatever the number of ids: the same genesis with 32 outputs costs about as much
    let prep2 = multi_prep(32, 81, kcc23::MAX_READ_PAYLOAD);
    let (gen2, _) = wide_genesis(&prep2, 32, 81);
    let (p2, g2) = (proof_tx(&kcc23_wire(&prep2, false)).unwrap(), proof_tx(&kcc23_wire(&gen2, false)).unwrap());
    let t = std::time::Instant::now();
    assert_eq!(bindings(&g2, &kcc23_txid(&gen2), std::slice::from_ref(&p2)).len(), 32);
    assert!(t.elapsed() < took * 3 + std::time::Duration::from_millis(5), "{:?} against {took:?}", t.elapsed());
}

/// The worst case of the bound: 300 covenant outputs whose ids are all wrong (each is examined, none is declared).
#[test]
fn a_genesis_of_wrong_covenant_ids_costs_a_bounded_amount() {
    let prep = multi_prep(300, 82, kcc23::MAX_READ_PAYLOAD);
    let (mut gen, _) = wide_genesis(&prep, 300, 82);
    for (i, o) in gen.outputs.iter_mut().enumerate() {
        o.covenant = Some(Binding::new(i as u16, kaspa_consensus_core::Hash::from_bytes(h("bogus", i as u64).0)));
    }
    gen.finalize();
    let p = proof_tx(&kcc23_wire(&prep, false)).unwrap();
    let g = proof_tx(&kcc23_wire(&gen, false)).unwrap();
    let t = std::time::Instant::now();
    assert!(bindings(&g, &kcc23_txid(&gen), std::slice::from_ref(&p)).is_empty());
    let took = t.elapsed();
    eprintln!("bindings: 300 wrong covenant ids: {took:?}");
    assert!(took < std::time::Duration::from_millis(100), "{took:?}");
}

/// A genesis that binds more preparations than the bound keeps the first ones only (and is still a correct declaration of those).
#[tokio::test]
async fn a_genesis_binds_at_most_a_few_preparations() {
    let hs = harness();
    let n = MAX_BOUND_PREPARATIONS as u64 + 3;
    let preps: Vec<Transaction> = (0..n).map(|i| kcc23_prep(200 + i, payload(KCC23_EXAMPLE))).collect();
    push(&hs, preps.iter().collect(), false).await;
    // one genesis, one input per preparation, each authorising its own covenant output
    let inputs = preps
        .iter()
        .map(|p| {
            TransactionInput::new(TransactionOutpoint::new(kaspa_consensus_core::Hash::from_bytes(kcc23_txid(p).0), 0), vec![], 0, 0)
        })
        .collect();
    let outs = (0..n).map(|i| TransactionOutput::new(90_000_000, ScriptPublicKey::from_vec(0, vec![0x51, i as u8]))).collect();
    let mut g = Transaction::new(1, inputs, outs, 0, SUBNETWORK_ID_NATIVE, 0, vec![]);
    for i in 0..n as usize {
        let id = kaspa_consensus_core::hashing::covenant_id::covenant_id(
            g.inputs[i].previous_outpoint,
            [(i as u32, &g.outputs[i])].into_iter(),
        );
        g.outputs[i].covenant = Some(Binding::new(i as u16, id));
    }
    g.finalize();
    let section = {
        let c = hs.ingest.lock().unwrap();
        extract(c.conn(), &kcc23_wire(&g, false)).unwrap().expect("a genesis")
    };
    assert_eq!(section.binds.len() + section.refs.len(), MAX_BOUND_PREPARATIONS);
    push(&hs, vec![&g], false).await;
    assert_eq!(count(&hs, "kcc23_declarations"), MAX_BOUND_PREPARATIONS as i64);
}
