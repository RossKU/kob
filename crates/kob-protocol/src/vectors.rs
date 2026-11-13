//! Golden-vector file format (`vectors/golden.json`).
//!
//! Typed so that serialisation keeps field declaration order: `JSON.stringify` of a parsed vector
//! entry reproduces exactly the string the library (native or wasm) returns for it.

use serde::{Deserialize, Serialize};

use crate::artifacts::TemplateInfo;
use std::collections::BTreeMap;

use crate::build::Action;
use crate::defaults::{DayOrder, Tips};
use crate::payload::Payload;
use crate::state::AnyState;
use crate::tx::{BuiltTx, FinalizeOptions, InputSignature, SignedTx};

/// The whole file.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Golden {
    /// Format version of the file.
    pub format: u32,
    /// Protocol version (`VERSION`) the vectors were generated for.
    pub protocol: String,
    /// Summary of every template (`artifacts::template_infos`).
    pub templates: Vec<TemplateInfo>,
    /// Transaction vectors.
    pub transactions: Vec<TxVector>,
    /// State vectors.
    pub states: Vec<StateVector>,
    /// Payload vectors.
    pub payloads: Vec<PayloadVector>,
    /// Per-program keeper tips of the KAS kinds (the committed `data/keeper_tips.json`).
    pub keeper_tips: BTreeMap<String, Tips>,
    /// Per-program-pair keeper tips of the pair kinds (the committed `data/pair_keeper_tips.json`,
    /// keys `<A>+<B>`).
    #[serde(default)]
    pub pair_keeper_tips: BTreeMap<String, Tips>,
    /// Day order vectors.
    pub day_orders: Vec<DayOrderVector>,
}

/// request -> built (unsigned + plan) -> signatures -> signed.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TxVector {
    /// Vector name.
    pub name: String,
    /// The request the transaction is built from.
    pub request: Action,
    /// The unsigned transaction and its plan.
    pub built: BuiltTx,
    /// The signatures applied to it.
    pub signatures: Vec<InputSignature>,
    /// The finalize options.
    pub finalize: FinalizeOptions,
    /// The signed transaction.
    pub signed: SignedTx,
}

/// A state and its encoded span.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StateVector {
    /// Vector name.
    pub name: String,
    /// The state to encode.
    pub state: AnyState,
    /// The encoded state span.
    #[serde(with = "crate::json::field")]
    pub encoded: Vec<u8>,
}

/// A payload and its decoding.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PayloadVector {
    /// Vector name.
    pub name: String,
    /// The payload bytes.
    #[serde(with = "crate::json::field")]
    pub payload: Vec<u8>,
    /// The decoded payload.
    pub decoded: Payload,
}

/// A day order computed from the node's DAA score and the wall clock.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DayOrderVector {
    /// The node's virtual DAA score.
    #[serde(with = "crate::json::field")]
    pub d0: u64,
    /// The UTC wall clock, in unix seconds.
    #[serde(with = "crate::json::field")]
    pub t0: u64,
    /// The measured DAA rate in milli-DAA per second, if any.
    #[serde(with = "crate::json::field")]
    pub rate_milli: Option<u64>,
    /// The day order computed from the inputs.
    pub result: DayOrder,
}
