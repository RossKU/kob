//! Payer and merchant SDK (Rust): parse a 402, select an offer, build and sign a payment for each
//! profile, and construct requirements on the server side. Submodules: `native` (KAS
//! `standard-native`, the binding), `token` (KCC-20 profile), `swap` (swap-and-pay), `intent` (intent-based swap-and-pay).

pub mod intent;
pub mod native;
pub mod swap;
pub mod token;
