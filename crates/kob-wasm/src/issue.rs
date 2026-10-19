//! Token issuance for the wasm surface: the `kob token issue` flow as two `BuiltTx`, and the KCC-23 readers.
//!
//! A creation is two transactions under KCC-23 (`kob_protocol::issue::build_creation`): `prep`, the preparation (the wallet's
//! P2PK funding -> output 0, the wallet's own P2PK of exactly the genesis's carriers and fee, + change; payload = `0x00` + the
//! token metadata JSON), and `built`, the genesis spending `prep`'s output 0 alone (input 0 authorises the KIP-20 genesis group,
//! so the covenant id commits to the declaration) -> 1..N token outputs of the reference KCC-20 program in its standard 3 / 3
//! configuration (`KCC20Ref`, KOB's standard; or with `"program": "public-mint"` the `KCC20` actor of upstream's published
//! `KCC20PublicMint` app, `KCC20PublicMint`; the 8 / 8 prototype is not offered here), no change. Both are signed by the
//! wallet's key; the genesis is built and signed before the preparation is on chain (a transaction id does not depend on
//! signature scripts). `build_creation` does the planning and `CreationPlan::verify` the consensus-level checks (the KCC-23
//! binding included); this module only maps JSON in and the plans out as normal [`BuiltTx`], so the existing `finalize` /
//! `validate` exports and the wallet signing pipeline (written against `BuiltTx`) work on each unchanged.
//!
//! Readers: [`kcc23_verify`] checks a launch proof (`kob-kcc23-proof/1`, from an indexer or any other source) and returns the
//! declaration bound to its covenant id; [`kcc23_decode`] reads a preparation payload.

use std::collections::BTreeSet;

use kob_protocol::issue::{
    self, build_creation, hex, p2pk_script, CreationPlan, FundingUtxo, Holder, IssueProgram, IssueSpec, DEFAULT_CARRIER,
    DEFAULT_FEE_RATE, EXTENSION_CLASS, EXTENSION_FIXED_SUPPLY, MAX_GENESIS_OUTPUTS, MAX_STANDARD_MASS, MAX_SUPPLY,
    OWNER_SCHEMES_ENABLED, REGISTRY_TEMPLATE_ID, REGISTRY_TEMPLATE_ID_PUBLIC_MINT,
};
use kob_protocol::json;
use kob_protocol::kcc20::{ISSUE_MAX_TOKEN_INPUTS, ISSUE_MAX_TOKEN_OUTPUTS};
use kob_protocol::kcc23;
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
    /// The logo (declared as `logo`; registry `display.icon`).
    #[serde(default)]
    icon: Option<String>,
    #[serde(default)]
    website: Option<String>,
    /// X account: a handle or its address (declared as `https://x.com/<handle>`).
    #[serde(default)]
    x: Option<String>,
    /// Telegram: a handle or its address (declared as `https://t.me/<handle>`).
    #[serde(default)]
    telegram: Option<String>,
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
    /// The KCC-23 launch proof (`kob-kcc23-proof/1`): both transactions in the fields their ids commit to.
    launch_proof: Value,
}

/// The declaration the preparation carries.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Declaration {
    /// The JSON text, exactly as committed.
    json: String,
    /// The preparation payload (`00` + the UTF-8 JSON), hex.
    #[serde(with = "json::field")]
    payload: Vec<u8>,
    /// Sompi of the preparation's output 0: the genesis's carriers and fee (the wallet's own UTXO until the genesis spends it).
    #[serde(with = "json::field")]
    funding: u64,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct IssueResult {
    /// Step 1: the preparation (the KCC-23 declaration).
    prep: BuiltTx,
    /// Step 2: the genesis, spending `prep`'s output 0.
    built: BuiltTx,
    declaration: Declaration,
    token: TokenInfo,
    docs: Docs,
    warnings: Vec<String>,
}

/// Characters the registry refuses in display text: control, zero-width and bidi controls.
fn has_bad_char(s: &str) -> bool {
    kcc23::has_bad_char(s)
}

/// The registry's display rules, applied up front so the emitted registry entry is always submittable.
fn check_display(r: &IssueRequest) -> R<()> {
    if r.name.trim().is_empty() || r.name.chars().count() > MAX_NAME_CHARS || has_bad_char(&r.name) {
        return Err(format!(
            "invalid parameter: name must be 1..={MAX_NAME_CHARS} printable characters (no control, zero-width or bidi characters)"
        ));
    }
    for (label, v) in [("description", &r.description), ("website", &r.website), ("icon", &r.icon), ("x", &r.x), ("telegram", &r.telegram)] {
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

/// A social account of the request in its canonical address, `None` when absent or blank.
fn social(v: &Option<String>, label: &str, canonical: fn(&str) -> Option<String>) -> R<Option<String>> {
    match v.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
        Some(s) => canonical(s).map(Some).ok_or_else(|| format!("invalid parameter: {label} {s:?} is not a handle or the account's address")),
        None => Ok(None),
    }
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
    spec.x = social(&r.x, "x", kcc23::x_url)?;
    spec.telegram = social(&r.telegram, "telegram", kcc23::telegram_url)?;
    if let Some(n) = &r.network {
        spec.network = n.clone();
    }
    if let Some(p) = &r.program {
        spec.program = IssueProgram::parse(p).map_err(|e| e.to_string())?;
        if spec.program.is_testnet_third_party() {
            return Err("KaspaRocket's testnet-10 program is issued from the CLI only (`--program kasparocket-testnet`)".into());
        }
        if spec.program.is_prototype() {
            return Err("the 8 / 8 program is a prototype and is not issued from the app: use 3x3 (the default) or public-mint".into());
        }
    }
    Ok(spec)
}

/// A transaction (as `TxJson`, with its UTXO entries) of P2PK inputs of `pubkeys` (one per input) as the shared `BuiltTx`: P2PK plans,
/// SIGHASH_ALL sign requests and the fee report. `daa`: the real DAA scores of the inputs where known (the Schnorr sighash does
/// not commit to them; `finalize` recomputes every digest from the JSON).
fn to_built(
    mut tx_json: TxJson,
    pubkeys: &[[u8; 32]],
    daa: &[u64],
    fee: u64,
    fee_rate: u64,
    change_output: Option<u32>,
    covenants: Vec<NewCovenant>,
) -> R<BuiltTx> {
    for (i, d) in daa.iter().enumerate() {
        tx_json.inputs[i].utxo.block_daa_score = *d;
    }
    let (unsigned, entries) = tx_json.to_tx().map_err(|e| e.to_string())?;

    // Mass of the transaction as it will be signed (placeholder signatures have the final size).
    let dummy_sig = push_data(&[[0u8; 64].as_slice(), &[SIGHASH_ALL]].concat());
    let mut measured = unsigned.clone();
    for i in measured.inputs.iter_mut() {
        i.signature_script = dummy_sig.clone();
    }
    let mass = tx::masses(&measured, &entries);
    let min_fee = tx::min_fee(&mass, fee_rate);
    if fee < min_fee {
        return Err(format!("internal: fee {fee} is below the minimum {min_fee}"));
    }

    let mut plans = vec![];
    let mut roles = vec![];
    let mut sign = vec![];
    for (i, pubkey) in pubkeys.iter().enumerate() {
        plans.push(SigPlan::P2pk { pubkey: *pubkey });
        roles.push("p2pk".to_string());
        sign.push(SignRequest {
            input_index: i,
            pubkey: *pubkey,
            sighash_type: SIGHASH_ALL,
            sighash: tx::sighash(&unsigned, &entries, i),
            redeem_script: None,
        });
    }
    Ok(BuiltTx {
        tx: tx_json,
        plans,
        roles,
        sign,
        fee: FeeReport { fee, min_fee, fee_rate, fee_mode: tx::FeeMode::Relay, mass, change_output },
        covenants,
    })
}

/// The preparation and the genesis of `plan` as `BuiltTx` (funding DAA scores from the request).
fn to_builts(plan: &CreationPlan, funding: &[KeyUtxo]) -> R<(BuiltTx, BuiltTx)> {
    let rate = plan.genesis.spec.fee_rate;
    let p = &plan.prep;
    let keys: Vec<[u8; 32]> = funding.iter().map(|f| f.pubkey).collect();
    let daa: Vec<u64> = funding.iter().map(|f| f.utxo.block_daa_score).collect();
    let prep = to_built(TxJson::from_tx(&p.tx, &p.entries), &keys, &daa, p.fee, rate, p.change.map(|(i, _)| i as u32), vec![])?;
    let g = &plan.genesis;
    let maker = g.spec.funding[0].owner_pubkey;
    let covenants = vec![NewCovenant {
        outputs: (0..g.states.len() as u32).collect(),
        authorizing_input: 0,
        covenant_id: g.covenant_id.as_bytes(),
        template: Some(g.spec.program.template_id()),
    }];
    let genesis = to_built(TxJson::from_tx(&g.tx, &g.entries), &[maker], &[], g.fee, rate, None, covenants)?;
    Ok((prep, genesis))
}

/// Plans, verifies and describes a fixed-supply KCC-20 creation (see the module docs and `issue` in `lib.rs`).
pub fn issue(spec_json: &str) -> R<String> {
    let req: IssueRequest = serde_json::from_str(spec_json).map_err(|e| format!("spec: {e}"))?;
    check_display(&req)?;
    let spec = to_spec(&req, req.fee_rate.unwrap_or(DEFAULT_FEE_RATE))?;
    let plan = build_creation(&spec).map_err(|e| e.to_string())?;
    // both transactions, the KCC-23 binding, covenant context (genesis group), covenant id, exact scripts, supply,
    // storage-mass commitments, mass bounds and the owner-scheme-0x04 self-test; script execution follows in `validate` once signed
    plan.verify().map_err(|e| e.to_string())?;
    let (prep, built) = to_builts(&plan, &req.funding)?;
    let g = &plan.genesis;

    let token = TokenInfo {
        covenant_id: g.covenant_id.as_bytes(),
        program: g.program.name,
        template_hash: g.program.template_hash,
        extension_commitment: spec.extension_commitment,
        name: spec.name.clone(),
        ticker: spec.ticker.clone(),
        decimals: spec.decimals,
        supply: spec.supply,
        carrier: spec.carrier,
        outputs: g
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
    let docs = Docs {
        supply: plan.supply_json(),
        metadata: g.metadata_json(),
        registry_entry: g.registry_entry(),
        launch_proof: serde_json::to_value(plan.launch_proof()).map_err(|e| e.to_string())?,
    };
    let declaration = Declaration { json: plan.prep.metadata.json(), payload: plan.prep.tx.payload.clone(), funding: plan.prep.value };
    let result = IssueResult { prep, built, declaration, token, docs, warnings: plan.warnings().to_vec() };
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
        // the declaration's payload limit (codec byte included): name, symbol, decimals and the optional fields together
        "maxMetadataBytes": kcc23::MAX_TOKEN_PAYLOAD,
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

/// Verifies a KCC-23 launch proof (`kob-kcc23-proof/1`, JSON) and returns the declaration bound to its covenant id:
/// `{covenantId, preparationTxid, genesisTxid, json, token: {name, symbol, decimals, description, logo, website, x, telegram,
/// issues}}`. Whoever supplied the proof, the ids are recomputed here: an error means it does not bind a declaration to that id.
pub fn kcc23_verify(proof_json: &str) -> R<String> {
    let proof: kcc23::LaunchProof = serde_json::from_str(proof_json).map_err(|e| format!("proof: {e}"))?;
    let v = proof.verify()?;
    serde_json::to_string(&v).map_err(|e| e.to_string())
}

/// Decodes a preparation payload (hex) under KCC-23 and KOB's token convention: `{json, token}`; an error names why it is not a
/// declaration. Decoding binds nothing: only [`kcc23_verify`] ties a declaration to a covenant id.
pub fn kcc23_decode(payload_hex: &str) -> R<String> {
    let payload = json::from_hex(payload_hex)?;
    let (text, obj) = kcc23::decode_limited(&payload, kcc23::MAX_READ_PAYLOAD).map_err(|e| e.to_string())?;
    serde_json::to_string(&json!({ "json": text, "token": kcc23::TokenDeclaration::from_object(&obj) })).map_err(|e| e.to_string())
}
