//! The `kobd` binary, the KOB daemon with the Executor and Indexer roles. See `kobd::cli`.

fn main() -> std::process::ExitCode {
    #[cfg(windows)]
    system32_dlls_only();
    #[cfg(unix)]
    no_group_write_umask();
    kobd::cli::main()
}

/// Every library loaded from here on comes from System32 (or a full path), never from kobd's own
/// folder or the working directory, where another account might have planted one (audit
/// 2026-11-23 R2D-2). The imports loaded before `main` are restricted by the linker flag of
/// `build.rs`.
#[cfg(windows)]
fn system32_dlls_only() {
    #[link(name = "kernel32")]
    extern "system" {
        fn SetDefaultDllDirectories(flags: u32) -> i32;
    }
    const LOAD_LIBRARY_SEARCH_SYSTEM32: u32 = 0x800;
    // SAFETY: a flags-only call with no pointers; a failure leaves the default search order.
    unsafe {
        SetDefaultDllDirectories(LOAD_LIBRARY_SEARCH_SYSTEM32);
    }
}

/// Whatever kobd creates from here on (its log and each rotation of it, the data directory, the
/// store, `--state-dir`, the files of `kobd setup`) is never writable by the group or others,
/// whatever the umask it was started with (a desktop's 002): the start check refuses such paths on
/// mainnet (audit 2026-11-24 R6D-1). Before any file is created.
#[cfg(unix)]
fn no_group_write_umask() {
    // SAFETY: umask has no preconditions and cannot fail; it is read and set again at once,
    // before any other thread exists.
    unsafe {
        let old = libc::umask(0o022);
        libc::umask(old | 0o022);
    }
}
