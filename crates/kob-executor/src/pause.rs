//! Pause files (kill switches): `kob-executor run --pause-file`, the matcher's and keeper's `--pause-file` and the x402
//! facilitator's `killSwitchFile`.
//!
//! A pause file pauses while it exists. It fails closed: only "no such file" means running; any other answer (permission
//! denied on its directory, an I/O error, a path through a regular file) counts as paused, so a pause that cannot be
//! checked is never ignored.

use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};

/// True when the pause file at `path` is set, or when its state cannot be read (anything but `NotFound`).
pub fn is_set(path: &Path) -> bool {
    match std::fs::symlink_metadata(path) {
        Ok(_) => true,
        // Windows reports a path through a regular file as NotFound (ERROR_PATH_NOT_FOUND), where Unix says
        // NotADirectory: absent only when the nearest existing ancestor is a directory.
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => below_a_non_directory(path),
        Err(e) => {
            static WARNED: AtomicBool = AtomicBool::new(false);
            if !WARNED.swap(true, Ordering::Relaxed) {
                tracing::warn!(path = %path.display(), error = %e, "pause file cannot be checked: treated as set (paused)");
            }
            true
        }
    }
}

/// True when the nearest ancestor of `path` that exists is not a directory (or cannot be read).
fn below_a_non_directory(path: &Path) -> bool {
    for a in path.ancestors().skip(1).filter(|a| !a.as_os_str().is_empty()) {
        match std::fs::metadata(a) {
            Ok(m) => return !m.is_dir(),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
            Err(_) => return true,
        }
    }
    false
}

/// [`is_set`] of an optional pause file (`None`: never paused).
pub fn is_set_opt(path: Option<&Path>) -> bool {
    path.is_some_and(is_set)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_pause_file_that_cannot_be_checked_counts_as_set() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("pause");
        assert!(!is_set(&p));
        assert!(!is_set_opt(None));
        std::fs::write(&p, b"").unwrap();
        assert!(is_set(&p) && is_set_opt(Some(&p)));
        // a path through a regular file: neither present nor absent
        assert!(is_set(&p.join("pause")));
        // a missing directory is "absent" (nothing paused it)
        assert!(!is_set(&d.path().join("missing").join("pause")));
    }

    #[cfg(unix)]
    #[test]
    fn a_pause_file_in_an_unreadable_directory_counts_as_set() {
        use std::os::unix::fs::PermissionsExt;
        let d = tempfile::tempdir().unwrap();
        let dir = d.path().join("kob");
        std::fs::create_dir(&dir).unwrap();
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o000)).unwrap();
        // root can still look into the directory: then the file is simply absent
        let unreadable = std::fs::symlink_metadata(dir.join("pause")).is_err_and(|e| e.kind() == std::io::ErrorKind::PermissionDenied);
        let paused = is_set(&dir.join("pause"));
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700)).unwrap();
        assert_eq!(paused, unreadable);
    }
}
