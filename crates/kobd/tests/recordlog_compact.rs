//! Compressed record-log segments (`kobd::indexer::recordlog`): a log whose sealed segments are plain, gzip (older builds) and zstd
//! replays into the same database as the live one, and `kobd db compact-log` (`indexer::compact_log`) converts them next to a running
//! writer (the gzips only) and offline (every sealed segment), the log reading the same throughout.

mod common;

use common::*;
use kob_protocol::build::*;
use kob_protocol::state::*;
use kobd::config::{IndexerConfig, StartMode};
use kobd::hex::Hash32;
use kobd::indexer::{compact_log, recordlog, replay_from_log, Indexer};
use kobd::testkit::*;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

fn test_config(dir: &Path, node: &MockNode) -> IndexerConfig {
    let tokens_path = dir.join("tokens.json");
    std::fs::write(&tokens_path, format!(r#"[{{"ticker":"TST","covenantId":"{}"}}]"#, Hash32(TOKEN_COV))).unwrap();
    let mut cfg = IndexerConfig {
        network: "testnet-10".into(),
        data_dir: dir.join("data"),
        tokens_path: Some(tokens_path),
        start: StartMode::Hash(node.anchor()),
        ..Default::default()
    };
    cfg.rules.min_order_value_sompi = 1;
    cfg.rules.max_expiry_span_daa = 1 << 40;
    cfg.poll_interval_ms = 1;
    cfg.api.enabled = false;
    // small segments: a rotation every frame or two
    cfg.records.segment_bytes = 1024;
    cfg
}

async fn sync<S: kobd::rpc::ChainSource>(f: &kobd::indexer::follower::Follower<S>) {
    for _ in 0..200 {
        if matches!(f.step().await, kobd::indexer::follower::StepOutcome::Idle) {
            return;
        }
    }
    panic!("did not settle");
}

/// The bytes of a segment, whatever its form.
fn plain_bytes(p: &Path) -> Vec<u8> {
    let f = std::fs::File::open(p).unwrap();
    let mut out = Vec::new();
    match p.extension().and_then(|e| e.to_str()) {
        Some("zst") => zstd::stream::read::Decoder::new(f).unwrap().read_to_end(&mut out).unwrap(),
        Some("gz") => flate2::read::GzDecoder::new(f).read_to_end(&mut out).unwrap(),
        _ => std::io::BufReader::new(f).read_to_end(&mut out).unwrap(),
    };
    out
}

/// The segment at `p` rewritten as an older build kept it: `ext` "gz" (gzip) or "" (plain).
fn rewrite_as(p: &Path, ext: &str) -> PathBuf {
    let bytes = plain_bytes(p);
    let name = p.file_name().unwrap().to_str().unwrap();
    let stem = &name[..name.find(".kobrec").unwrap() + ".kobrec".len()];
    let to = p.with_file_name(if ext.is_empty() { stem.to_string() } else { format!("{stem}.{ext}") });
    if ext == "gz" {
        let mut e = flate2::write::GzEncoder::new(std::fs::File::create(&to).unwrap(), flate2::Compression::new(6));
        e.write_all(&bytes).unwrap();
        e.finish().unwrap();
    } else {
        std::fs::write(&to, &bytes).unwrap();
    }
    std::fs::remove_file(p).unwrap();
    to
}

fn ext(p: &Path) -> &str {
    p.extension().and_then(|e| e.to_str()).unwrap()
}

#[tokio::test]
async fn a_log_of_plain_gzip_and_zstd_segments_replays_and_compacts_next_to_a_running_writer_and_offline() {
    let dir = tempfile::tempdir().unwrap();
    let node = MockNode::new("testnet-10");
    let cfg = test_config(dir.path(), &node);
    let w = World::new();
    let idx = Indexer::open(cfg.clone()).unwrap();
    let f = idx.follower(node.clone());
    for (k, price) in [P250, P245, P250 + 1, P245 + 1, P250 + 2, P245 + 2].into_iter().enumerate() {
        let a = ask(MAKER_A, price);
        let create = w.create_tx(AnyState::KobAsk(a.clone()), CARRIER, MAKER_A, 10 * WHOLE);
        w.include(&node, &[&create]);
        let custody = w.token_at(&create, 1, Kcc20State::custody(10 * WHOLE, w.cov(&create, 0).0, EXT));
        let leg = Leg::Ask { order: w.order(&create, 0, a), custody, amount: (k as i64 + 1) * WHOLE, t: None };
        w.include(&node, &[&w.sign(&Action::Batch(batch(&w, node.tip_daa(), vec![leg])))]);
        sync(&f).await;
    }
    let log_dir = cfg.records_dir();
    let segs = recordlog::segment_paths(&log_dir).unwrap();
    assert!(segs.len() >= 5, "{segs:?}");
    let (sealed, newest) = segs.split_at(segs.len() - 1);
    assert!(sealed.iter().all(|p| ext(p) == "zst"), "the writer keeps its sealed segments as zstd: {segs:?}");
    assert_eq!(ext(&newest[0]), "kobrec", "the newest stays plain");
    let want = recordlog::read_all(&log_dir).unwrap();
    // an older build's log: gzips and plain sealed segments among the zstd ones
    rewrite_as(&sealed[0], "gz");
    rewrite_as(&sealed[1], "");
    rewrite_as(&sealed[2], "gz");
    let mixed = recordlog::read_all(&log_dir).unwrap();
    assert_eq!((&mixed.records, mixed.frames), (&want.records, want.frames));

    // next to the running writer: only the gzips are recompressed (the writer owns the plain ones)
    let r = compact_log(&cfg).unwrap();
    assert!(!r.offline);
    assert_eq!(r.report.segments, 2);
    let forms: Vec<String> = recordlog::segment_paths(&log_dir).unwrap().iter().map(|p| ext(p).to_string()).collect();
    assert_eq!(forms[..3], ["zst", "kobrec", "zst"]);
    assert_eq!(recordlog::read_all(&log_dir).unwrap().records, want.records);
    // the writer goes on over the compacted log
    let a = ask(MAKER_B, P245 + 3);
    w.include(&node, &[&w.create_tx(AnyState::KobAsk(a), CARRIER, MAKER_B, 10 * WHOLE)]);
    sync(&f).await;
    let live = snapshot(idx.ingest.lock().unwrap().conn());
    drop(f);
    drop(idx);
    let after = recordlog::read_all(&log_dir).unwrap();
    assert!(after.frames > want.frames);
    assert_eq!(after.records[..want.records.len()], want.records[..]);

    // `kobd db replay` of a mixed log, elsewhere, rebuilds the live database
    rewrite_as(&recordlog::segment_paths(&log_dir).unwrap()[0], "gz");
    rewrite_as(&recordlog::segment_paths(&log_dir).unwrap()[2], "");
    let mut cfg2 = cfg.clone();
    cfg2.data_dir = dir.path().join("rebuilt");
    cfg2.records.dir = Some(log_dir.clone());
    replay_from_log(&cfg2).unwrap();
    let conn = kobd::indexer::db::open_writer(&cfg2.db_path(), "testnet-10").unwrap();
    assert_eq!(snapshot(&conn), live);
    drop(conn);

    // offline: every sealed segment, and what a crash left
    std::fs::write(log_dir.join("seg-000000000001.kobrec.zst.tmp"), b"left by a crash").unwrap();
    let r = compact_log(&cfg).unwrap();
    assert!(r.offline);
    assert!(r.report.segments >= 2, "{:?}", r.report);
    let segs = recordlog::segment_paths(&log_dir).unwrap();
    assert!(segs[..segs.len() - 1].iter().all(|p| ext(p) == "zst"), "{segs:?}");
    assert!(!log_dir.join("seg-000000000001.kobrec.zst.tmp").exists(), "an offline compaction removes what a crash left");
    assert_eq!(compact_log(&cfg).unwrap().report.segments, 0, "idempotent");
    assert_eq!(recordlog::read_all(&log_dir).unwrap().records, after.records);
    // and the writer opens the compacted log and goes on
    let idx = Indexer::open(cfg.clone()).unwrap();
    let f = idx.follower(node.clone());
    sync(&f).await;
}

#[test]
fn compact_log_refuses_a_data_directory_without_a_record_log() {
    let dir = tempfile::tempdir().unwrap();
    let mut cfg = IndexerConfig { network: "testnet-10".into(), data_dir: dir.path().join("data"), ..Default::default() };
    cfg.records.enabled = false;
    assert!(compact_log(&cfg).is_err());
}
