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
    pub format: u32,
    pub protocol: String,
    pub templates: Vec<TemplateInfo>,
    pub transactions: Vec<TxVector>,
    pub states: Vec<StateVector>,
    pub payloads: Vec<PayloadVector>,
    /// Per-program keeper tips of the KAS kinds (the committed `data/keeper_tips.json`).
    pub keeper_tips: BTreeMap<String, Tips>,
    /// Per-program-pair keeper tips of the pair kinds (the committed `data/pair_keeper_tips.json`, keys `<A>+<B>`).
    #[serde(default)]
    pub pair_keeper_tips: BTreeMap<String, Tips>,
    pub day_orders: Vec<DayOrderVector>,
}

/// request -> built (unsigned + plan) -> signatures -> signed.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TxVector {
    pub name: String,
    pub request: Action,
    pub built: BuiltTx,
    pub signatures: Vec<InputSignature>,
    pub finalize: FinalizeOptions,
    pub signed: SignedTx,
}

/// A state and its encoded span.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StateVector {
    pub name: String,
    pub state: AnyState,
    #[serde(with = "crate::json::field")]
    pub encoded: Vec<u8>,
}

/// A payload and its decoding.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PayloadVector {
    pub name: String,
    #[serde(with = "crate::json::field")]
    pub payload: Vec<u8>,
    pub decoded: Payload,
}

/// A day order computed from the node's DAA score and the wall clock.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DayOrderVector {
    #[serde(with = "crate::json::field")]
    pub d0: u64,
    #[serde(with = "crate::json::field")]
    pub t0: u64,
    #[serde(with = "crate::json::field")]
    pub rate_milli: Option<u64>,
    pub result: DayOrder,
}
