//! Windows: the imports of the kobd binary (`bcrypt.dll`, `bcryptprimitives.dll`, ... are not
//! KnownDLLs) load from System32 only, never from the program's own folder, where another account
//! might have planted one (audit 2026-11-23 R2D-2). `/DEPENDENTLOADFLAG` sets the load-config
//! `DependentLoadFlags` that the loader honours since Windows 10 1607; 0x800 is
//! `LOAD_LIBRARY_SEARCH_SYSTEM32`. Later loads are restricted at the start of `main`
//! (`SetDefaultDllDirectories`).

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    let windows = std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows");
    let msvc = std::env::var("CARGO_CFG_TARGET_ENV").as_deref() == Ok("msvc");
    if windows && msvc {
        println!("cargo:rustc-link-arg-bins=/DEPENDENTLOADFLAG:0x800");
    }
}
