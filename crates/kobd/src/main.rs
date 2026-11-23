//! The `kobd` binary, the KOB daemon with the Executor and Indexer roles. See `kobd::cli`.

fn main() -> std::process::ExitCode {
    #[cfg(windows)]
    system32_dlls_only();
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
