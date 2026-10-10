//! Token issuance for the wasm surface: the `kob token issue` flow as a `BuiltTx`.
//!
//! The genesis is one transaction: P2PK funding inputs (input 0 authorises the KIP-20 genesis group)
//! -> 1..N token outputs of the reference KCC-20 program in its standard 3 / 3 configuration (`KCC20Ref`, KOB's standard;
//! or with `"program": "public-mint"` the `KCC20` actor of upstream's published `KCC20PublicMint` app, `KCC20PublicMint`.
//! The 8 / 8 prototype is not offered here; covenant id derived as
//! consensus does) + change. `kob_protocol::issue::build_genesis` does the planning and its
//! `GenesisPlan::verify` the consensus-level checks; this module only maps JSON in and the plan out
//! as a normal [`BuiltTx`], so the existing `finalize` / `validate` exports and the wallet signing
//! pipeline (written against `BuiltTx`) work on it unchanged.

use std::collections::BTreeSet;

use kob_protocol::issue::{
    self, build_genesis, hex, p2pk_script, FundingUtxo, GenesisPlan, Holder, IssueProgram, IssueSpec, DEFAULT_CARRIER,
    DEFAULT_FEE_RATE, EXTENSION_CLASS, EXTENSION_FIXED_SUPPLY, MAX_GENESIS_OUTPUTS, MAX_STANDARD_MASS, MAX_SUPPLY,
    OWNER_SCHEMES_ENABLED, REGISTRY_TEMPLATE_ID, REGISTRY_TEMPLATE_ID_PUBLIC_MINT,
};
use kob_protocol::json;
use kob_protocol::kcc20::{ISSUE_MAX_TOKEN_INPUTS, ISSUE_MAX_TOKEN_OUTPUTS};
use kob_protocol::registry::MAX_DECIMALS;
use kob_protocol::script::{push_data, SIGHASH_ALL};
use kob_protocol::tx::{self, BuiltTx, FeeReport, KeyUtxo, NewCovenant, SigPlan, SignRequest, TxJson, MIN_FEE_RATE};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

type R<T> = Result<T, String>;

/// Longest description / website / icon the registry accepts.
const MAX_DISPLAY_CHARS: usize = 512;
const MAX_NAME_CHARS: usize = 64;

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct HolderJson {
    #[serde(with = "json::field")]
    owner: [u8; 32],
    owner_scheme: u8,
    #[serde(with = "json::field")]
    amount: u64,
    #[serde(default)]
    borrow_scheme: u8,
    #[serde(with = "json::field", default)]
    borrow_guard: Option<[u8; 32]>,
}

/// Request of `issue` (kob-protocol JSON conventions).
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct IssueRequest {
    name: String,
    ticker: String,
    decimals: u8,
    #[serde(with = "json::field")]
    supply: u64,
    holders: Vec<HolderJson>,
    #[serde(with = "json::field", default)]
    extension_commitment: Option<[u8; 32]>,
    #[serde(with = "json::field", default)]
    carrier: Option<u64>,
    #[serde(with = "json::field", default)]
    fee_rate: Option<u64>,
    funding: Vec<KeyUtxo>,
    /// x-only public key that receives the change (default: the owner of funding[0]).
    #[serde(with = "json::field", default)]
    change_to: Option<[u8; 32]>,
    /// Permit non-zero borrow schemes on non-0x04 holders (never set by the web app's default form).
    #[serde(default)]
    allow_borrow: bool,
    #[serde(default)]
    description: Option<String>,
    #[serde(default)]
    icon: Option<String>,
    #[serde(default)]
    website: Option<String>,
    #[serde(default)]
    network: Option<String>,
    /// `3x3` (default) or `public-mint` (`kob_protocol::issue::IssueProgram::parse`); the `8x8-prototype` the CLI knows is
    /// refused here.
    #[serde(default)]
    program: Option<String>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct TokenOutput {
    index: u32,
    #[serde(with = "json::field")]
    amount: u64,
    #[serde(with = "json::field")]
    owner: [u8; 32],
    owner_scheme: u8,
    borrow_scheme: u8,
    #[serde(with = "json::field")]
    borrow_guard: [u8; 32],
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct TokenInfo {
    #[serde(with = "json::field")]
    covenant_id: [u8; 32],
    program: &'static str,
    #[serde(with = "json::field")]
    template_hash: [u8; 32],
    #[serde(with = "json::field")]
    extension_commitment: [u8; 32],
    name: String,
    ticker: String,
    decimals: u8,
    #[serde(with = "json::field")]
    supply: u64,
    /// KAS (sompi) on every token output.
    #[serde(with = "json::field")]
    carrier: u64,
    outputs: Vec<TokenOutput>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Docs {
    supply: Value,
    metadata: Value,
    registry_entry: Value,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct IssueResult {
    built: BuiltTx,
    token: TokenInfo,
    docs: Docs,
    warnings: Vec<String>,
}

/// Characters the registry refuses in display text: control, zero-width and bidi controls.
fn has_bad_char(s: &str) -> bool {
    s.chars().any(|c| {
        c.is_control()
            || matches!(c, '\u{200B}'..='\u{200F}' | '\u{202A}'..='\u{202E}' | '\u{2060}'..='\u{2064}' | '\u{2066}'..='\u{2069}' | '\u{FEFF}')
    })
}

/// The registry's display rules, applied up front so the emitted registry entry is always submittable.
fn check_display(r: &IssueRequest) -> R<()> {
    if r.name.trim().is_empty() || r.name.chars().count() > MAX_NAME_CHARS || has_bad_char(&r.name) {
        return Err(format!(
            "invalid parameter: name must be 1..={MAX_NAME_CHARS} printable characters (no control, zero-width or bidi characters)"
        ));
    }
    for (label, v) in [("description", &r.description), ("website", &r.website), ("icon", &r.icon)] {
        if let Some(v) = v {
            if v.chars().count() > MAX_DISPLAY_CHARS || has_bad_char(v) {
                return Err(format!(
                    "invalid parameter: {label} must be at most {MAX_DISPLAY_CHARS} characters without control characters"
                ));
            }
        }
    }
    if r.website.as_deref().is_some_and(|w| !w.starts_with("https://")) {
        return Err("invalid parameter: website must be an https:// URL".into());
    }
    if r.icon.as_deref().is_some_and(|i| !(i.starts_with("https://") || i.starts_with("ipfs://"))) {
        return Err("invalid parameter: icon must be an https:// or ipfs:// URL".into());
    }
    Ok(())
}

fn to_spec(r: &IssueRequest, fee_rate: u64) -> R<IssueSpec> {
    if fee_rate < MIN_FEE_RATE {
        return Err(format!("invalid parameter: fee rate {fee_rate} is below the relay minimum {MIN_FEE_RATE}"));
    }
    let mut seen = BTreeSet::new();
    let mut total: u64 = 0;
    for f in &r.funding {
        if !seen.insert((f.utxo.transaction_id, f.utxo.index)) {
            return Err("invalid parameter: a funding UTXO is listed twice".into());
        }
        if f.utxo.covenant_id.is_some() {
            return Err("invalid parameter: funding UTXOs must be plain P2PK outputs (this one carries a covenant id)".into());
        }
        total = total.checked_add(f.utxo.amount).ok_or("invalid parameter: funding amounts overflow")?;
    }
    let holders = r
        .holders
        .iter()
        .map(|h| Holder {
            owner: h.owner,
            owner_scheme: h.owner_scheme,
            amount: h.amount,
            borrow_scheme: h.borrow_scheme,
            borrow_guard: h.borrow_guard.unwrap_or([0u8; 32]),
        })
        .collect();
    let funding =
        r.funding.iter().map(|f| FundingUtxo { outpoint: f.utxo.outpoint(), amount: f.utxo.amount, owner_pubkey: f.pubkey }).collect();
    let mut spec = IssueSpec::new(&r.name, &r.ticker, r.decimals, r.supply, holders, funding);
    spec.extension_commitment = r.extension_commitment.unwrap_or(EXTENSION_FIXED_SUPPLY);
    spec.carrier = r.carrier.unwrap_or(DEFAULT_CARRIER);
    spec.fee_rate = fee_rate;
    spec.allow_borrow = r.allow_borrow;
    spec.change_spk = r.change_to.map(|pk| p2pk_script(&pk));
    spec.description = r.description.clone();
    spec.icon = r.icon.clone();
    spec.website = r.website.clone();
    if let Some(n) = &r.network {
        spec.network = n.clone();
    }
    if let Some(p) = &r.program {
        spec.program = IssueProgram::parse(p).map_err(|e| e.to_string())?;
        if spec.program.is_prototype() {
            return Err("the 8 / 8 program is a prototype and is not issued from the app: use 3x3 (the default) or public-mint".into());
        }
    }
    Ok(spec)
}

/// The plan as the shared `BuiltTx` (unsigned tx with funding UTXO entries, P2PK plans, SIGHASH_ALL sign requests, fee report).
fn to_built(plan: &GenesisPlan, funding: &[KeyUtxo]) -> R<BuiltTx> {
    let mut tx_json = TxJson::from_tx(&plan.tx, &plan.entries);
    // The plan's entries carry DAA score 0; report the real scores (the Schnorr sighash does not commit to them,
    // which `finalize` re-checks by recomputing every digest from this JSON).
    for (i, f) in funding.iter().enumerate() {
        tx_json.inputs[i].utxo.block_daa_score = f.utxo.block_daa_score;
    }
    let (unsigned, entries) = tx_json.to_tx().map_err(|e| e.to_string())?;

    // Mass of the transaction as it will be signed (placeholder signatures have the final size).
    let dummy_sig = push_data(&[[0u8; 64].as_slice(), &[SIGHASH_ALL]].concat());
    let mut measured = unsigned.clone();
    for i in measured.inputs.iter_mut() {
        i.signature_script = dummy_sig.clone();
    }
    let rate = plan.spec.fee_rate;
    let mass = tx::masses(&measured, &entries);
    let min_fee = tx::min_fee(&mass, rate);
    if plan.fee < min_fee {
        return Err(format!("internal: genesis fee {} is below the minimum {min_fee}", plan.fee));
    }

    let mut plans = vec![];
    let mut roles = vec![];
    let mut sign = vec![];
    for (i, f) in funding.iter().enumerate() {
        plans.push(SigPlan::P2pk { pubkey: f.pubkey });
        roles.push("p2pk".to_string());
        sign.push(SignRequest {
            input_index: i,
            pubkey: f.pubkey,
            sighash_type: SIGHASH_ALL,
            sighash: tx::sighash(&unsigned, &entries, i),
            redeem_script: None,
        });
    }
    let covenants = vec![NewCovenant {
        outputs: (0..plan.states.len() as u32).collect(),
        authorizing_input: 0,
        covenant_id: plan.covenant_id.as_bytes(),
        template: Some(plan.spec.program.template_id()),
    }];
    Ok(BuiltTx {
        tx: tx_json,
        plans,
        roles,
        sign,
        fee: FeeReport {
            fee: plan.fee,
            min_fee,
            fee_rate: rate,
            fee_mode: tx::FeeMode::Relay,
            mass,
            change_output: plan.change.map(|(i, _)| i as u32),
        },
        covenants,
    })
}

/// Plans, verifies and describes a fixed-supply KCC-20 issuance (see the module docs and `issue` in `lib.rs`).
pub fn issue(spec_json: &str) -> R<String> {
    let req: IssueRequest = serde_json::from_str(spec_json).map_err(|e| format!("spec: {e}"))?;
    check_display(&req)?;
    let spec = to_spec(&req, req.fee_rate.unwrap_or(DEFAULT_FEE_RATE))?;
    let plan = build_genesis(&spec).map_err(|e| e.to_string())?;
    // covenant context (genesis group), covenant id, exact scripts, supply, storage-mass commitment, mass bound and
    // the owner-scheme-0x04 self-test of the program; script execution follows in `validate` once signed
    plan.verify().map_err(|e| e.to_string())?;
    let built = to_built(&plan, &req.funding)?;

    let token = TokenInfo {
        covenant_id: plan.covenant_id.as_bytes(),
        program: plan.program.name,
        template_hash: plan.program.template_hash,
        extension_commitment: spec.extension_commitment,
        name: spec.name.clone(),
        ticker: spec.ticker.clone(),
        decimals: spec.decimals,
        supply: spec.supply,
        carrier: spec.carrier,
        outputs: plan
            .states
            .iter()
            .enumerate()
            .map(|(i, s)| TokenOutput {
                index: i as u32,
                amount: s.amount,
                owner: s.owner,
                owner_scheme: s.owner_scheme,
                borrow_scheme: s.borrow_scheme,
                borrow_guard: s.borrow_guard,
            })
            .collect(),
    };
    let docs = Docs { supply: plan.supply_json(), metadata: plan.metadata_json(), registry_entry: plan.registry_entry() };
    let result = IssueResult { built, token, docs, warnings: plan.warnings.clone() };
    serde_json::to_string(&result).map_err(|e| e.to_string())
}

/// Constants and rules of the issuance flow, so the UI does not hard-code them.
pub fn issue_limits() -> R<String> {
    let limits = json!({
        "maxSupply": MAX_SUPPLY.to_string(),
        "maxGenesisOutputs": MAX_GENESIS_OUTPUTS,
        "defaultCarrier": DEFAULT_CARRIER.to_string(),
        "defaultFeeRate": DEFAULT_FEE_RATE.to_string(),
        "minFeeRate": MIN_FEE_RATE.to_string(),
        "maxStandardMass": MAX_STANDARD_MASS.to_string(),
        "maxTokenInputs": ISSUE_MAX_TOKEN_INPUTS,
        "maxTokenOutputs": ISSUE_MAX_TOKEN_OUTPUTS,
        "maxDecimals": MAX_DECIMALS,
        "ticker": { "minLength": 2, "maxLength": 12, "pattern": "^[A-Z0-9]{2,12}$" },
        "name": { "minLength": 1, "maxLength": MAX_NAME_CHARS },
        "maxDisplayChars": MAX_DISPLAY_CHARS,
        "ownerSchemes": OWNER_SCHEMES_ENABLED,
        "covenantOwnerScheme": issue::SCHEME_COVENANT_ID,
        "program": "KCC20Ref",
        "registryTemplateId": REGISTRY_TEMPLATE_ID,
        // the published public-mint build of the reference (`"program": "public-mint"` in the request)
        "publicMint": { "program": "KCC20PublicMint", "registryTemplateId": REGISTRY_TEMPLATE_ID_PUBLIC_MINT, "maxTokenInputs": 3, "maxTokenOutputs": 3 },
        "extensionClass": EXTENSION_CLASS,
        "extensionCommitment": hex(&EXTENSION_FIXED_SUPPLY),
    });
    serde_json::to_string(&limits).map_err(|e| e.to_string())
}
