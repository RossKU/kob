//! Disk facts for monitoring.
//!
//! Two numbers are measured. They are the free space of the data directory's file system and the
//! size of the database. The free space comes from `statvfs` on Unix and `GetDiskFreeSpaceExW` on
//! Windows, so no extra dependency is needed. It is `None` on other platforms or on failure.

use std::path::Path;

/// Bytes available to this process on the file system that holds `dir`.
#[cfg(unix)]
#[allow(clippy::unnecessary_cast)] // the field types differ between Unix targets
pub fn free_bytes(dir: &Path) -> Option<u64> {
    use std::os::unix::ffi::OsStrExt;
    let c = std::ffi::CString::new(dir.as_os_str().as_bytes()).ok()?;
    // SAFETY: an all-zero `statvfs` is a valid value of the plain C struct. `statvfs` fills it
    // from a NUL-terminated path that lives for the call.
    let mut s: libc::statvfs = unsafe { std::mem::zeroed() };
    if unsafe { libc::statvfs(c.as_ptr(), &mut s) } != 0 {
        return None;
    }
    Some((s.f_bavail as u64).saturating_mul(s.f_frsize as u64))
}

/// Bytes available to this process on the volume that holds `dir`.
#[cfg(windows)]
pub fn free_bytes(dir: &Path) -> Option<u64> {
    use std::os::windows::ffi::OsStrExt;
    #[link(name = "kernel32")]
    extern "system" {
        fn GetDiskFreeSpaceExW(dir: *const u16, available: *mut u64, total: *mut u64, free: *mut u64) -> i32;
    }
    let dir = if dir.as_os_str().is_empty() { Path::new(".") } else { dir };
    let wide: Vec<u16> = dir.as_os_str().encode_wide().chain(std::iter::once(0)).collect();
    let mut available = 0u64;
    // SAFETY: the wide path is NUL-terminated and lives for the call. The two counts not asked for
    // may be null.
    let ok = unsafe { GetDiskFreeSpaceExW(wide.as_ptr(), &mut available, std::ptr::null_mut(), std::ptr::null_mut()) };
    (ok != 0).then_some(available)
}

/// Always `None`: this platform has no free-space call.
#[cfg(not(any(unix, windows)))]
pub fn free_bytes(_dir: &Path) -> Option<u64> {
    None
}

/// Bytes of the SQLite database at `db` with its write-ahead log and shared-memory file.
///
/// `None` when the database file is missing. A missing side file counts as 0.
pub fn db_bytes(db: &Path) -> Option<u64> {
    let main = std::fs::metadata(db).ok()?.len();
    let side = |suffix: &str| {
        let mut p = db.as_os_str().to_owned();
        p.push(suffix);
        std::fs::metadata(std::path::PathBuf::from(p)).map(|m| m.len()).unwrap_or(0)
    };
    Some(main + side("-wal") + side("-shm"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn free_space_and_database_size_are_measured() {
        let dir = tempfile::tempdir().unwrap();
        #[cfg(any(unix, windows))]
        assert!(free_bytes(dir.path()).is_some_and(|b| b > 0));
        let db = dir.path().join("index.sqlite3");
        assert_eq!(db_bytes(&db), None);
        std::fs::write(&db, vec![0u8; 4096]).unwrap();
        std::fs::write(dir.path().join("index.sqlite3-wal"), vec![0u8; 100]).unwrap();
        assert_eq!(db_bytes(&db), Some(4196));
    }
}
