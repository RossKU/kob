//! Genesis check of a token outside the registry, behind the wasm `verifyGenesis` binding.
//!
//! The module wraps `kob_protocol::genesis_open::check_open_genesis` in JSON, and that function
//! holds the rules. Integers of 64 bits are decimal strings, as elsewhere in kob-protocol.
//!
//! # Request
//!
//! ```text
//! {covenantId, genesisTxid, authorizingOutpoint: {txid, index},
//!  outputs: [{index, value, spk, spkVersion?, redeemScript?}], programHint?, ownerHints?}
//! ```
//!
//! - `outputs` is the whole genesis group, which is every output bound to the covenant id, in
//!   increasing index order.
//! - `spk` is the script public key as hex. `redeemScript` is the revealed redeem script, and
//!   it is absent when not revealed.
//! - `ownerHints` are x-only keys that the genesis transaction pays to. They are the candidate
//!   owners of an unspent `TokenSeed`.
//!
//! # Result
//!
//! ```text
//! {status: "verified", program, programHash, txid, outputs, supply, minterOutputs,
//!  mintAllowance, seedOutputs, companionOutputs, issuer, rebuilt}
//! {status: "failed", code: "mismatch" | "contaminated", message}
//! {status: "unavailable", code: "unrevealed" | "unsupported_program" | "malformed", message}
//! ```
//!
//! A request that is not JSON of this shape is an error, because it is a caller bug. Anything the
//! data can get wrong is a `failed` or `unavailable` result.

use kob_protocol::genesis_open::{check_open_genesis, FailCode, OpenGenesis, OpenOutcome, OpenOutput, UnavailableCode};
use kob_protocol::json;
use serde::Deserialize;
use serde_json::{json, Value};

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Outpoint {
    txid: String,
    index: u32,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct OutputJson {
    index: u32,
    #[serde(with = "json::field")]
    value: u64,
    spk: String,
    #[serde(default)]
    spk_version: u16,
    #[serde(default)]
    redeem_script: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Request {
    covenant_id: String,
    genesis_txid: String,
    authorizing_outpoint: Outpoint,
    outputs: Vec<OutputJson>,
    #[serde(default)]
    program_hint: Option<String>,
    #[serde(default)]
    owner_hints: Vec<String>,
}

fn unavailable(message: String) -> String {
    json!({ "status": "unavailable", "code": "malformed", "message": message }).to_string()
}

/// Checks a genesis group and returns the result JSON, see the module docs.
pub fn verify_genesis(request: &str) -> Result<String, String> {
    let r: Request = serde_json::from_str(request).map_err(|e| format!("genesis request: {e}"))?;
    // Hex that does not decode is the data's fault, so it is `unavailable`, not an error.
    let mut outputs = Vec::with_capacity(r.outputs.len());
    for o in &r.outputs {
        let spk = match json::from_hex(&o.spk) {
            Ok(b) => b,
            Err(e) => return Ok(unavailable(format!("output {}: script public key: {e}", o.index))),
        };
        let redeem = match o.redeem_script.as_deref() {
            None => None,
            Some(h) => match json::from_hex(h) {
                Ok(b) => Some(b),
                Err(e) => return Ok(unavailable(format!("output {}: redeem script: {e}", o.index))),
            },
        };
        outputs.push(OpenOutput { index: o.index, value: o.value, spk_version: o.spk_version, spk, redeem });
    }
    let txid = match json::hex32(&r.authorizing_outpoint.txid) {
        Ok(t) => t,
        Err(e) => return Ok(unavailable(format!("authorising outpoint: {e}"))),
    };
    let program_hint = match r.program_hint.as_deref() {
        None => None,
        Some(h) => match json::hex32(h) {
            Ok(b) => Some(b),
            Err(e) => return Ok(unavailable(format!("program hint: {e}"))),
        },
    };
    let mut owner_hints = Vec::with_capacity(r.owner_hints.len());
    for h in &r.owner_hints {
        match json::hex32(h) {
            Ok(b) => owner_hints.push(b),
            Err(e) => return Ok(unavailable(format!("owner hint: {e}"))),
        }
    }
    let outcome = check_open_genesis(&OpenGenesis {
        covenant_id: r.covenant_id,
        genesis_txid: r.genesis_txid,
        authorizing_outpoint: (txid, r.authorizing_outpoint.index),
        outputs,
        program_hint,
        owner_hints,
    });
    let v: Value = match outcome {
        OpenOutcome::Verified(v) => {
            let rep = &v.report;
            json!({
                "status": "verified",
                "program": v.program,
                "programHash": v.program_hash,
                "txid": rep.txid,
                "outputs": rep.outputs,
                "supply": rep.supply.to_string(),
                "minterOutputs": rep.minter_outputs,
                "mintAllowance": rep.mint_allowance.to_string(),
                "seedOutputs": rep.seed_outputs,
                "companionOutputs": rep.companion_outputs.iter().map(|c| json!({ "index": c.index, "role": c.role })).collect::<Vec<_>>(),
                "issuer": rep.issuer,
                "rebuilt": v.rebuilt,
            })
        }
        OpenOutcome::Failed { code, message } => {
            let code = match code {
                FailCode::Mismatch => "mismatch",
                FailCode::Contaminated => "contaminated",
            };
            json!({ "status": "failed", "code": code, "message": message })
        }
        OpenOutcome::Unavailable { code, message } => {
            let code = match code {
                UnavailableCode::Unrevealed => "unrevealed",
                UnavailableCode::UnsupportedProgram => "unsupported_program",
                UnavailableCode::Malformed => "malformed",
            };
            json!({ "status": "unavailable", "code": code, "message": message })
        }
    };
    Ok(v.to_string())
}
