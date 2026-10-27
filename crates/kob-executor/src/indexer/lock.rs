//! The single-writer lock of a data directory.
//!
//! The database and the record log have exactly one writer: the record log is a hash chain whose next frame
//! commits to the previous one, so two processes appending to it (on TN10, `index import-orders` run next to a
//! running `run`) interleave and break the chain (`record log frame N is corrupt: chain hash mismatch`), after which
//! both refuse the directory. Every command that writes a data directory (`run`, `index`, `index import-orders`,
//! `index rebase`, `index replay`) therefore takes an exclusive OS lock on `<data_dir>/kob-writer.lock` and on
//! `<records_dir>/kob-writer.lock` (the log may be configured elsewhere) for as long as it runs; the OS releases it
//! when the process exits, crashed or not. Readers do not lock: the read API and `index export-orders` open the
//! database read-only and work next to a writer.

use crate::config::IndexerConfig;
use std::fs::{File, OpenOptions, TryLockError};
use std::path::{Path, PathBuf};

const LOCK_FILE: &str = "kob-writer.lock";
const OWNER_FILE: &str = "kob-writer.owner";

/// Held while a process writes a data directory; dropping it releases the lock.
#[derive(Debug)]
pub struct WriterLock {
    _files: Vec<File>,
    dirs: Vec<PathBuf>,
}

#[derive(Debug, thiserror::Error)]
pub enum LockError {
    #[error(
        "{dir} is in use by another kob-executor writer ({owner}): stop it first, or point this command at another data directory. \
         `run` and `index` hold the lock while they run; read-only commands (`index export-orders`, the read API) work next to it. \
         Lock file: {lock}"
    )]
    Busy { dir: String, owner: String, lock: String },
    #[error("cannot lock {0}: {1}")]
    Io(String, std::io::Error),
}

impl WriterLock {
    fn lock_files(cfg: &IndexerConfig) -> Result<(Vec<PathBuf>, Vec<File>), LockError> {
        let mut dirs = vec![cfg.data_dir.clone()];
        if cfg.records.enabled {
            // the log has its own lock: another data directory configured with the same `records.dir` is a second writer too
            dirs.push(cfg.records_dir());
        }
        let mut files = vec![];
        for d in &dirs {
            files.push(lock_dir(d)?);
        }
        Ok((dirs, files))
    }

    /// Whether a writer could lock the directories of `cfg` now: the same refusal as [`acquire`](Self::acquire), nothing kept
    /// and nothing written (not even the owner note). For a command to refuse before it prints or logs anything.
    pub fn check(cfg: &IndexerConfig) -> Result<(), LockError> {
        let mut dirs = vec![cfg.data_dir.clone()];
        if cfg.records.enabled {
            dirs.push(cfg.records_dir());
        }
        for d in &dirs {
            // no lock file, no holder: and a check creates nothing (a command that fails later must not have touched the directory)
            if d.join(LOCK_FILE).exists() {
                lock_dir(d)?;
            }
        }
        Ok(())
    }

    /// Lock the data directory and the record-log directory of `cfg`.
    pub fn acquire(cfg: &IndexerConfig) -> Result<WriterLock, LockError> {
        let (dirs, files) = Self::lock_files(cfg)?;
        let owner = format!(
            "pid {} ({}), since unix {}\n",
            std::process::id(),
            std::env::args().collect::<Vec<_>>().join(" "),
            std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
        );
        for d in &dirs {
            // informative only (the lock is the OS lock): which process holds it, for the error message of the next one
            let _ = std::fs::write(d.join(OWNER_FILE), &owner);
        }
        Ok(WriterLock { _files: files, dirs })
    }

    pub fn dirs(&self) -> &[PathBuf] {
        &self.dirs
    }
}

fn lock_dir(dir: &Path) -> Result<File, LockError> {
    std::fs::create_dir_all(dir).map_err(|e| LockError::Io(dir.display().to_string(), e))?;
    let path = dir.join(LOCK_FILE);
    let file = OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(&path)
        .map_err(|e| LockError::Io(path.display().to_string(), e))?;
    match try_lock_exclusive(&file) {
        Ok(()) => Ok(file),
        Err(TryLockError::WouldBlock) => {
            let owner = std::fs::read_to_string(dir.join(OWNER_FILE))
                .ok()
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .unwrap_or_else(|| "owner unknown".into());
            Err(LockError::Busy { dir: dir.display().to_string(), owner, lock: path.display().to_string() })
        }
        Err(TryLockError::Error(e)) => Err(LockError::Io(path.display().to_string(), e)),
    }
}

/// Takes the exclusive lock of `file` without waiting: `WouldBlock` if another open file description holds it.
///
/// On Unix this is `flock(LOCK_EX | LOCK_NB)` called directly: std's `File::try_lock` is unsupported on Android (it returns
/// `Unsupported` there, which made the executor on the phone exit at once), and `flock` is what std uses on Linux and macOS
/// anyway, so the semantics are the same on every Unix. Elsewhere (Windows) std's lock is used.
#[cfg(unix)]
fn try_lock_exclusive(file: &File) -> Result<(), TryLockError> {
    use std::os::fd::AsRawFd;
    loop {
        // SAFETY: `flock` on a descriptor that `file` keeps open for the whole call; it touches no memory of ours.
        if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } == 0 {
            return Ok(());
        }
        let e = std::io::Error::last_os_error();
        match e.kind() {
            std::io::ErrorKind::Interrupted => continue,
            std::io::ErrorKind::WouldBlock => return Err(TryLockError::WouldBlock),
            _ => return Err(TryLockError::Error(e)),
        }
    }
}

#[cfg(not(unix))]
fn try_lock_exclusive(file: &File) -> Result<(), TryLockError> {
    file.try_lock()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_check_refuses_like_an_acquire_and_keeps_nothing() {
        let d = tempfile::tempdir().unwrap();
        let cfg = IndexerConfig { data_dir: d.path().join("data"), ..IndexerConfig::default() };
        WriterLock::check(&cfg).expect("free");
        assert!(!cfg.data_dir.exists(), "a check creates nothing");
        assert!(!cfg.data_dir.join(OWNER_FILE).exists(), "a check writes no owner note");
        let held = WriterLock::acquire(&cfg).unwrap();
        assert!(matches!(WriterLock::check(&cfg), Err(LockError::Busy { .. })));
        drop(held);
        WriterLock::check(&cfg).expect("the check kept no lock, and the real one is released");
    }

    #[test]
    fn a_second_writer_of_a_directory_is_refused_until_the_first_lets_go() {
        let d = tempfile::tempdir().unwrap();
        let first = lock_dir(d.path()).unwrap();
        std::fs::write(
            d.path().join(OWNER_FILE),
            "pid 1 (kob-executor run)
",
        )
        .unwrap();
        let e = lock_dir(d.path()).unwrap_err();
        assert!(matches!(e, LockError::Busy { .. }), "{e}");
        let msg = e.to_string();
        assert!(msg.contains("is in use by another kob-executor writer (pid 1 (kob-executor run))"), "{msg}");
        assert!(msg.contains(LOCK_FILE), "{msg}");
        drop(first);
        lock_dir(d.path()).expect("the OS lock is released with the file");
    }

    /// The path every Unix target takes (Android included): `flock` through libc, not std's `File::try_lock`.
    #[cfg(unix)]
    #[test]
    fn the_flock_helper_refuses_a_second_descriptor_and_releases_on_close() {
        let d = tempfile::tempdir().unwrap();
        let path = d.path().join(LOCK_FILE);
        let open = || OpenOptions::new().create(true).truncate(false).write(true).open(&path).unwrap();
        let (a, b) = (open(), open());
        try_lock_exclusive(&a).unwrap();
        assert!(matches!(try_lock_exclusive(&b), Err(TryLockError::WouldBlock)));
        drop(a);
        try_lock_exclusive(&b).unwrap();
    }

    #[test]
    fn writer_lock_covers_the_data_and_the_records_directory() {
        let d = tempfile::tempdir().unwrap();
        // the record log defaults to <data_dir>/records and has its own lock
        let cfg = IndexerConfig { data_dir: d.path().join("data"), ..IndexerConfig::default() };
        let first = WriterLock::acquire(&cfg).unwrap();
        assert_eq!(first.dirs().len(), 2);
        assert!(matches!(WriterLock::acquire(&cfg), Err(LockError::Busy { .. })));
        drop(first);
        WriterLock::acquire(&cfg).unwrap();
    }
}
