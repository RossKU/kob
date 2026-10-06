//! KOB x402: the Kaspa x402 `exact` binding (elldeeone/kaspa-x402 v1.0.0-rc.1, `kaspa-exact-v2`) plus
//! the two KOB contributions to it.
//!
//! | Module | Contents |
//! |---|---|
//! | [`wire`] | x402 v2 wire types, header codecs, network / profile / finality enums |
//! | [`canonical`] | canonical JSON (UTF-16 key order) and the requirements / request hashes |
//! | [`safe_tx`] | `kaspa-sdk-safe-json-v2.0.0` transaction projection (v0 and v1) |
//! | [`sighash`] | the SIGHASH_ALL rule of payer signatures: hash type extraction per owner scheme, checked before the script engine |
//! | [`chain`] | the trusted chain view trait, outpoints, clock |
//! | [`policy`] | resource bounds, token allowlist and custody classes |
//! | [`verify`] | `verify_payment` dispatch and the [`verify::Verified`] result |
//! | [`exact`] | KAS `standard-native` verifier (the binding) |
//! | [`token`] | KCC-20 transfer profile for `exact` (proposal, `docs/spec/x402-kcc20-profile.md`) |
//! | [`swap`] | swap-and-pay extension `extra.route` (proposal, `docs/spec/x402-swap-and-pay.md`) |
//! | [`intent`] | intent-based swap-and-pay (`extra.route` binding `kob-intent-v1`): the payer signs a KOB router intent once, the facilitator executes it |
//! | [`invoice`] | invoices: x402 requirements plus a merchant reference and an expiry, served at a URL; id, validation, status types |
//! | [`client`] | payer and merchant SDK |
//! | [`testkit`] | in-memory chain and fixtures for tests |
//!
//! This crate does no I/O: chain facts come through [`chain::ChainView`], time through
//! [`chain::Clock`]. The facilitator service lives in `kob-executor` (`x402` module).

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

/// The source directory this build of the crate was compiled from (`CARGO_MANIFEST_DIR`). Debug builds only (never
/// embedded in the release / wasm artifacts: those are byte-reproducible across checkouts). Test suites compare it with
/// the checkout they run from, see `tests/families_support`: worktrees that share one `CARGO_TARGET_DIR` share
/// compiled artifacts (cargo's metadata hash is relative to the workspace root, and freshness is decided by file mtimes),
/// so a test binary of one checkout can silently link a library compiled from another.
#[cfg(debug_assertions)]
#[doc(hidden)]
pub const SOURCE_ROOT: &str = env!("CARGO_MANIFEST_DIR");
