//! Library error type.

use crate::state::StateError;

/// Every fallible library call returns this error.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// The request is inconsistent or violates a protocol rule the builder enforces.
    #[error("invalid request: {0}")]
    Invalid(String),
    /// The inputs cannot pay the outputs plus the minimum fee.
    #[error("insufficient funds: need {need} sompi, have {have} sompi")]
    InsufficientFunds { need: u64, have: u64 },
    /// No compute-budget entry for an input role (regenerate the budget table).
    #[error("no compute budget for input role `{0}`")]
    MissingBudget(String),
    /// A supplied signature is malformed or does not verify.
    #[error("signature for input {input}: {reason}")]
    Signature { input: usize, reason: String },
    /// Script or covenant-context failure found while validating through the engine.
    #[error("engine: {0}")]
    Engine(String),
    /// State encoding / decoding failure.
    #[error(transparent)]
    State(#[from] StateError),
    /// JSON (de)serialisation failure.
    #[error("json: {0}")]
    Json(#[from] serde_json::Error),
    /// `KOB1` payload codec failure.
    #[error("payload: {0}")]
    Payload(String),
}

/// Library result type.
pub type Result<T> = std::result::Result<T, Error>;

pub(crate) fn invalid<T>(msg: impl Into<String>) -> Result<T> {
    Err(Error::Invalid(msg.into()))
}
