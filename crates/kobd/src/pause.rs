//! Pause files (kill switches): `kobd --executor --pause-file`, the matcher's and keeper's `--pause-file` and the x402
//! facilitator's `killSwitchFile`.
//!
//! A pause file pauses while it exists. It fails closed: only "no such file in an existing directory" means running; any
//! other answer (permission denied on its directory, an I/O error, a path through a regular file, a directory that does not
//! exist) counts as paused, so a pause that cannot be checked is never ignored. A missing directory is a pause file on a
//! volume that is not mounted, a mistyped path, or a directory deleted together with the file in it: trading then waits until
//! the directory exists again (a warning names it), never trades on the assumption that nothing paused it.

use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};

/// True when the pause file at `path` is set, or when its state cannot be read (anything but `NotFound`).
pub fn is_set(path: &Path) -> bool {
    match std::fs::symlink_metadata(path) {
        Ok(_) => true,
        // absent only when its directory exists (Windows reports a path through a regular file as NotFound,
        // ERROR_PATH_NOT_FOUND, where Unix says NotADirectory: a directory that is a file is no directory either)
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => !in_a_directory(path),
        Err(e) => {
            static WARNED: AtomicBool = AtomicBool::new(false);
            if !WARNED.swap(true, Ordering::Relaxed) {
                tracing::warn!(path = %path.display(), error = %e, "pause file cannot be checked: treated as set (paused)");
            }
            true
        }
    }
}

/// True when the directory of `path` exists (a relative path without one: the working directory). Warns once when it does not.
fn in_a_directory(path: &Path) -> bool {
    let dir = match path.parent() {
        Some(d) if !d.as_os_str().is_empty() => d,
        _ => return true,
    };
    if std::fs::metadata(dir).is_ok_and(|m| m.is_dir()) {
        return true;
    }
    static WARNED: AtomicBool = AtomicBool::new(false);
    if !WARNED.swap(true, Ordering::Relaxed) {
        tracing::warn!(
            path = %path.display(),
            "the pause file's directory does not exist: treated as paused (create the directory, or fix the path)"
        );
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
        // a missing directory is not "absent": the pause may have been deleted with it, or the volume is not mounted
        assert!(is_set(&d.path().join("missing").join("pause")));
        assert!(is_set(&d.path().join("missing").join("deeper").join("pause")));
        std::fs::create_dir(d.path().join("missing")).unwrap();
        assert!(!is_set(&d.path().join("missing").join("pause")), "running again once the directory is there");
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
