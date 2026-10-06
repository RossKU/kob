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
    /// Lock the data directory and the record-log directory of `cfg`.
    pub fn acquire(cfg: &IndexerConfig) -> Result<WriterLock, LockError> {
        let mut dirs = vec![cfg.data_dir.clone()];
        if cfg.records.enabled {
            // the log has its own lock: another data directory configured with the same `records.dir` is a second writer too
            dirs.push(cfg.records_dir());
        }
        let mut files = vec![];
        for d in &dirs {
            files.push(lock_dir(d)?);
        }
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
    match file.try_lock() {
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
