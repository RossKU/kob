//! Payer and merchant SDK: parse a 402, select an offer, and build and sign a payment per profile.
//!
//! # Modules
//!
//! - [`native`]: KAS payments of the `standard-native` profile.
//! - [`token`]: KCC-20 token payments.
//! - [`swap`]: swap-and-pay through KOB orders.
//! - [`intent`]: intent-based swap-and-pay.
//! - [`retry`]: the safe retry of one payment. It re-sends, rebuilds anchored to the first attempt,
//!   or stops.

pub mod intent;
pub mod native;
pub mod retry;
pub mod swap;
pub mod token;
