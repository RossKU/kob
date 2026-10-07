//! The record log on a full disk: a failed append is rolled back to the last complete frame, so the frames written once
//! space is back follow it directly and the next start opens the log as it was written.
//!
//! The tests need a small tmpfs and skip themselves without `KOB_SMALL_TMPFS`:
//!
//! ```text
//! mount -t tmpfs -o size=2m tmpfs /mnt/kobfull
//! KOB_SMALL_TMPFS=/mnt/kobfull cargo test -p kob-executor --test recordlog_full_disk -- --test-threads=1
//! ```

use std::fs::OpenOptions;
use std::io::Write;
use std::path::{Path, PathBuf};

use kob_executor::indexer::recordlog::{read_all, RecordLog};

fn tmpfs(sub: &str) -> Option<PathBuf> {
    let base = PathBuf::from(std::env::var_os("KOB_SMALL_TMPFS")?);
    let d = base.join(sub);
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    Some(d)
}

/// Fills the file system of `dir` until it is full, then frees `leave` bytes (rounded by the fs to pages).
fn fill(dir: &Path, leave: u64) -> PathBuf {
    let p = dir.join("filler");
    let mut f = OpenOptions::new().create(true).write(true).truncate(true).open(&p).unwrap();
    let chunk = vec![0u8; 4096];
    let mut n = 0u64;
    loop {
        match f.write(&chunk) {
            Ok(0) | Err(_) => break,
            Ok(k) => n += k as u64,
        }
    }
    f.set_len(n.saturating_sub(leave)).unwrap();
    f.sync_all().unwrap();
    p
}

fn seg_len(dir: &Path) -> u64 {
    std::fs::read_dir(dir)
        .unwrap()
        .filter_map(|e| e.ok())
        .filter(|e| e.file_name().to_string_lossy().ends_with(".kobrec"))
        .map(|e| e.metadata().unwrap().len())
        .sum()
}

/// One complete frame, a failed append of `failing` bytes on a full disk, then `later` frames of `later_len` bytes once
/// space is back: the failed append leaves nothing behind, the reopened log holds every complete frame.
fn failed_append_then_more(sub: &str, leave: u64, failing: usize, later: u8, later_len: usize) {
    let Some(dir) = tmpfs(sub) else { return eprintln!("KOB_SMALL_TMPFS not set: skipped") };
    let (mut log, _) = RecordLog::open(&dir, 1 << 30, 0).unwrap();
    log.append_body(&[1u8; 1000]).unwrap();
    let after0 = seg_len(&dir);
    let filler = fill(&dir, leave);
    log.append_body(&vec![2u8; failing]).expect_err("the disk is full");
    assert_eq!(seg_len(&dir), after0, "the partial frame is cut off");
    assert!(!log.is_stopped());
    assert_eq!(log.next_n(), 1);
    std::fs::remove_file(filler).unwrap();
    for i in 0..later {
        assert_eq!(log.append_body(&vec![10 + i; later_len]).unwrap().0, 1 + i as u64);
    }
    drop(log);
    let frames = 1 + later as u64;
    let (log, report) = RecordLog::open(&dir, 1 << 30, frames).unwrap();
    assert_eq!(log.next_n(), frames);
    assert!(!report.torn_tail_removed, "{report:?}");
    let r = read_all(&dir).unwrap();
    assert_eq!((r.frames, r.torn), (frames, false));
}

/// A partial frame whose length would run past the end of the file.
#[test]
fn a_failed_append_on_a_full_disk_is_rolled_back_before_later_frames() {
    failed_append_then_more("rl1", 8192, 256 * 1024, 2, 1000);
}

/// A partial frame whose length would reach into the frames written after it.
#[test]
fn a_failed_small_append_on_a_full_disk_is_rolled_back_before_later_frames() {
    failed_append_then_more("rl2", 4096, 12_000, 4, 20_000);
}
