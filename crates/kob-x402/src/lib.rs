//! The Kaspa x402 `exact` binding (elldeeone/kaspa-x402 v1.0.1, `kaspa-exact-v2`) and two KOB
//! extensions to it.
//!
//! # Modules
//!
//! | Module | Contents |
//! |---|---|
//! | [`wire`] | x402 v2 wire types, header codecs, network, profile and finality enums |
//! | [`canonical`] | canonical JSON (UTF-16 key order), requirements and request hashes |
//! | [`safe_tx`] | `kaspa-sdk-safe-json-v2.0.0` transaction projection (v0 and v1) |
//! | [`sighash`] | payer signature hash types and the SIGHASH_ALL rule of KOB's profiles |
//! | [`chain`] | trusted chain view trait, outpoints, clock |
//! | [`policy`] | resource bounds, token allowlist, custody classes |
//! | [`verify`] | `verify_payment` dispatch and the [`verify::Verified`] result |
//! | [`exact`] | KAS `standard-native` verifier (the binding) |
//! | [`token`] | KCC-20 transfer profile, `docs/spec/x402-kcc20-profile.md` |
//! | [`swap`] | swap-and-pay extension `extra.route`, `docs/spec/x402-swap-and-pay.md` |
//! | [`intent`] | intent-based swap-and-pay (`extra.route` binding `kob-intent-v1`) |
//! | [`invoice`] | invoices: requirements plus merchant reference and expiry, served at a URL |
//! | [`client`] | payer and merchant SDK |
//! | [`testkit`] | in-memory chain and fixtures for tests |
//!
//! # Scope
//!
//! This crate does no I/O. Chain facts come through [`chain::ChainView`] and time through
//! [`chain::Clock`]. The facilitator service lives in `kobd` (`x402` module).

pub mod canonical;
pub mod chain;
pub mod client;
pub mod common;
pub mod error;
pub mod exact;
pub mod intent;
pub mod invoice;
pub mod policy;
pub mod safe_tx;
pub mod sighash;
pub mod swap;
pub mod testkit;
pub mod token;
pub mod verify;
pub mod wire;

pub use error::{Diag, Reason, X402Error};

/// The source directory this build was compiled from (`CARGO_MANIFEST_DIR`).
///
/// Present in debug builds only, so release and wasm artifacts stay byte-reproducible across
/// checkouts. Test suites compare it with the checkout they run from (`tests/families_support`).
/// Worktrees that share one `CARGO_TARGET_DIR` also share compiled artifacts, so a test binary of
/// one checkout can silently link a library compiled from another.
#[cfg(debug_assertions)]
#[doc(hidden)]
pub const SOURCE_ROOT: &str = env!("CARGO_MANIFEST_DIR");
