//! Chain evidence behind the registry's per-token genesis check (condition C1) and live-mint-authority check (C2), and its
//! verifier (`kob registry verify-genesis`).
//!
//! The evidence file (`registry/evidence/<network>-genesis.json`, written by `web/scripts/registry-genesis-evidence.mjs`
//! from a node and a block explorer) is UNTRUSTED input: every byte the verdict rests on is checked against a hash here.
//!
//! * The genesis transaction id is recomputed from the recorded transaction fields (`Transaction::id`, v1: everything but
//!   signature scripts, payload digest separately). A wrong field, including the `sequence` / `lock_time` / `gas` values an
//!   explorer does not publish, fails the check.
//! * The genesis group (outputs bound to the token's covenant id) and its authorising input's previous outpoint feed
//!   [`verify_genesis`], which recomputes the covenant id: so the outputs are exactly the token's complete genesis group.
//! * Each redeem script (revealed by the spend of the output, or reconstructed for an unspent output) is checked against its
//!   P2SH script public key, then decoded as an instance of the token's pinned program.
//!
//! C2 follows from the genesis for the KRON programs: a token output with `is_minter != 0` can be created only by a
//! transaction that spends a minter (every non-minter token input refuses minter outputs; `review_b2_kron` r_kr_01 and
//! r_kr_15), and the only other source of outputs carrying a covenant id is its genesis. So a genesis without a minter output
//! proves the token never has a live minter. A genesis with one needs its lineage traced to the live cells
//! ([`TokenEvidence::live_minters`]); without that trace C2 stays undetermined.

use serde::{Deserialize, Serialize};

use kaspa_consensus_core::subnets::SubnetworkId;
use kaspa_consensus_core::tx::{
    CovenantBinding, ScriptPublicKey, Transaction, TransactionId, TransactionInput, TransactionOutpoint, TransactionOutput,
};
use kaspa_consensus_core::Hash;

use crate::registry::{parse_hex32, verify_genesis, Family, GenesisError, GenesisOutput, GenesisRecord, Registry};

/// The evidence file.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Evidence {
    /// `mainnet` | `testnet-10` | `devnet`.
    pub network: String,
    /// When the evidence was collected (ISO 8601, informational).
    #[serde(default)]
    pub collected_at: String,
    /// Where the data came from.
    pub sources: Sources,
    /// One entry per token.
    pub tokens: Vec<TokenEvidence>,
}

/// Data sources of an evidence file.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Sources {
    /// The node liveness was read from.
    pub node: NodeSource,
    /// Block explorer base URL (pruned history).
    #[serde(default)]
    pub explorer: String,
    /// Hint source (genesis txid per covenant id).
    #[serde(default)]
    pub hints: String,
    /// Free text.
    #[serde(default)]
    pub note: String,
}

/// The node the evidence read liveness from.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct NodeSource {
    /// wRPC URL.
    pub url: String,
    /// Node version.
    #[serde(default)]
    pub server_version: String,
    /// Virtual DAA score at collection.
    pub virtual_daa_score: u64,
    /// Pruning point hash (informational: blocks below it are no longer served).
    #[serde(default)]
    pub pruning_point: String,
}

/// Evidence for one token.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct TokenEvidence {
    /// Ticker (informational; the covenant id identifies the token).
    pub ticker: String,
    /// Token covenant id.
    pub covenant_id: String,
    /// Registry template id.
    pub template_id: String,
    /// The genesis transaction and its reveals; absent when collection failed (`error`).
    #[serde(default)]
    pub genesis: Option<GenesisEvidence>,
    /// Genesis outputs the node still reports unspent.
    #[serde(default)]
    pub genesis_outputs_live: Vec<LiveCell>,
    /// Live mint-authority cells found by following a genesis minter's lineage (only needed when the genesis has a minter);
    /// `None` = not traced.
    #[serde(default)]
    pub live_minters: Option<Vec<LiveCell>>,
    /// Collection error.
    #[serde(default)]
    pub error: Option<String>,
}

/// A genesis transaction as recorded.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct GenesisEvidence {
    /// Transaction id (checked by recomputation).
    pub txid: String,
    /// Accepting block hash (informational).
    #[serde(default)]
    pub accepting_block_hash: String,
    /// DAA score of the accepting block.
    pub accepting_block_daa_score: u64,
    /// Blue score of the accepting block (informational).
    #[serde(default)]
    pub accepting_block_blue_score: u64,
    /// Block time (ms, informational).
    #[serde(default)]
    pub block_time_ms: u64,
    /// Where the transaction came from.
    pub source: String,
    /// The transaction fields the id commits to.
    pub tx: TxFields,
    /// Redeem scripts of the genesis group's outputs.
    pub reveals: Vec<Reveal>,
}

/// Transaction fields (v1 id preimage: no signature scripts).
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct TxFields {
    /// Transaction version.
    pub version: u16,
    /// Inputs.
    pub inputs: Vec<InputFields>,
    /// Outputs.
    pub outputs: Vec<OutputFields>,
    /// Lock time.
    pub lock_time: u64,
    /// Subnetwork id, hex.
    pub subnetwork_id: String,
    /// Gas.
    pub gas: u64,
    /// Payload, hex.
    #[serde(default)]
    pub payload: String,
}

/// An input.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct InputFields {
    /// Previous outpoint transaction id.
    pub txid: String,
    /// Previous outpoint index.
    pub index: u32,
    /// Sequence.
    pub sequence: u64,
    /// Covenant id of the spent UTXO as the source reported it (informational; the covenant-id recomputation decides).
    #[serde(default)]
    pub covenant_id: Option<String>,
}

/// An output.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct OutputFields {
    /// Value in sompi.
    pub value: u64,
    /// Script public key version.
    pub spk_version: u16,
    /// Script public key, hex.
    pub spk: String,
    /// Covenant binding.
    #[serde(default)]
    pub covenant: Option<CovenantFields>,
}

/// A covenant binding.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CovenantFields {
    /// Authorising input index.
    pub authorizing_input: u16,
    /// Covenant id, hex.
    pub covenant_id: String,
}

/// The redeem script of one genesis output.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Reveal {
    /// Output index.
    pub index: u32,
    /// Redeem script, hex (`None`: not revealed).
    #[serde(default)]
    pub redeem_script: Option<String>,
    /// `txid:input` of the spend that revealed it (`None`: reconstructed or unknown).
    #[serde(default)]
    pub revealed_by: Option<String>,
    /// How it was obtained.
    #[serde(default)]
    pub source: String,
}

/// A live UTXO as the node reported it.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct LiveCell {
    /// Address (informational).
    #[serde(default)]
    pub address: String,
    /// Outpoint transaction id.
    pub txid: String,
    /// Outpoint index.
    pub index: u32,
    /// Value in sompi.
    #[serde(default)]
    pub amount: u64,
    /// Script public key, hex.
    #[serde(default)]
    pub spk: String,
    /// Covenant id.
    #[serde(default)]
    pub covenant_id: Option<String>,
    /// DAA score of the block that created it.
    #[serde(default)]
    pub block_daa_score: u64,
}

/// The verdict for one token.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct TokenVerdict {
    /// Ticker.
    pub ticker: String,
    /// Covenant id.
    pub covenant_id: String,
    /// C1: `Ok` with the record, or why the genesis check failed.
    pub result: Result<GenesisRecord, String>,
}

impl TokenVerdict {
    /// C1: every genesis output checked and clean.
    pub fn genesis_verified(&self) -> bool {
        self.result.is_ok()
    }
    /// C2: the record says no live mint authority.
    pub fn no_live_minter(&self) -> bool {
        self.result.as_ref().is_ok_and(|g| g.live_minters.as_ref().is_some_and(|l| l.is_empty()))
    }
}

fn hex_bytes(s: &str, what: &str) -> Result<Vec<u8>, String> {
    if !s.len().is_multiple_of(2) || !s.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)) {
        return Err(format!("{what}: not lowercase hex"));
    }
    (0..s.len()).step_by(2).map(|i| u8::from_str_radix(&s[i..i + 2], 16).map_err(|e| format!("{what}: {e}"))).collect()
}
fn hash32(s: &str, what: &str) -> Result<[u8; 32], String> {
    parse_hex32(s).ok_or_else(|| format!("{what}: expected 64 lowercase hex characters"))
}

/// Rebuilds the genesis transaction from its recorded fields (no signature scripts: the v1 id does not commit to them).
pub fn rebuild_tx(f: &TxFields) -> Result<Transaction, String> {
    let inputs = f
        .inputs
        .iter()
        .enumerate()
        .map(|(n, i)| {
            let id = hash32(&i.txid, &format!("input {n} txid"))?;
            Ok(TransactionInput::new_with_compute_budget(
                TransactionOutpoint::new(TransactionId::from_bytes(id), i.index),
                vec![],
                i.sequence,
                0,
            ))
        })
        .collect::<Result<Vec<_>, String>>()?;
    let outputs = f
        .outputs
        .iter()
        .enumerate()
        .map(|(n, o)| {
            let spk = ScriptPublicKey::new(o.spk_version, hex_bytes(&o.spk, &format!("output {n} spk"))?.into());
            let covenant = match &o.covenant {
                None => None,
                Some(c) => Some(CovenantBinding::new(
                    c.authorizing_input,
                    Hash::from_bytes(hash32(&c.covenant_id, &format!("output {n} covenant id"))?),
                )),
            };
            Ok(TransactionOutput::with_covenant(o.value, spk, covenant))
        })
        .collect::<Result<Vec<_>, String>>()?;
    let sub = hex_bytes(&f.subnetwork_id, "subnetwork id")?;
    let sub: [u8; 20] = sub.try_into().map_err(|_| "subnetwork id: expected 20 bytes".to_string())?;
    Ok(Transaction::new(
        f.version,
        inputs,
        outputs,
        f.lock_time,
        SubnetworkId::from_bytes(sub),
        f.gas,
        hex_bytes(&f.payload, "payload")?,
    ))
}

/// Verifies one token's evidence against the registry: C1 (genesis) and C2 (live mint authority). `checked_at_daa` is the
/// node's virtual DAA score of the evidence.
pub fn verify_token(reg: &Registry, ev: &TokenEvidence, checked_at_daa: u64) -> TokenVerdict {
    let verdict = |result| TokenVerdict { ticker: ev.ticker.clone(), covenant_id: ev.covenant_id.clone(), result };
    verdict(check_token(reg, ev, checked_at_daa))
}

fn check_token(reg: &Registry, ev: &TokenEvidence, checked_at_daa: u64) -> Result<GenesisRecord, String> {
    if let Some(e) = &ev.error {
        return Err(format!("evidence collection failed: {e}"));
    }
    let tok = reg
        .tokens
        .iter()
        .find(|t| t.covenant_id == ev.covenant_id)
        .ok_or_else(|| format!("covenant id {} is not in the registry", ev.covenant_id))?;
    if tok.template_id != ev.template_id {
        return Err(format!("evidence names template {} but the registry says {}", ev.template_id, tok.template_id));
    }
    let tpl = reg.template(&tok.template_id).ok_or_else(|| format!("unknown template {}", tok.template_id))?;
    let g = ev.genesis.as_ref().ok_or("no genesis transaction in the evidence")?;
    let cov = hash32(&ev.covenant_id, "covenant id")?;

    // 1. the transaction is the one its id names
    let tx = rebuild_tx(&g.tx)?;
    let txid = tx.id();
    if txid.to_string() != g.txid {
        return Err(format!("the recorded genesis fields hash to txid {txid}, not {}", g.txid));
    }

    // 2. the genesis group: outputs bound to the covenant id, all authorised by the same input
    let group: Vec<(u32, &TransactionOutput)> = tx
        .outputs
        .iter()
        .enumerate()
        .filter(|(_, o)| o.covenant.is_some_and(|c| c.covenant_id.as_bytes() == cov))
        .map(|(n, o)| (n as u32, o))
        .collect();
    let Some(&(_, first)) = group.first() else {
        return Err("the genesis transaction has no output bound to the covenant id".into());
    };
    let auth = first.covenant.expect("filtered").authorizing_input;
    if group.iter().any(|(_, o)| o.covenant.expect("filtered").authorizing_input != auth) {
        return Err("the covenant id's outputs name different authorising inputs (not one genesis group)".into());
    }
    let auth_input = tx.inputs.get(auth as usize).ok_or("authorising input out of range")?;
    let outpoint = (auth_input.previous_outpoint.transaction_id.as_bytes(), auth_input.previous_outpoint.index);

    // 3. every output of the group, with its redeem script (checked against the P2SH hash inside verify_genesis)
    let mut outs = vec![];
    for (index, o) in &group {
        let redeem = match g.reveals.iter().find(|r| r.index == *index).and_then(|r| r.redeem_script.as_deref()) {
            Some(h) => Some(hex_bytes(h, &format!("redeem script of output {index}"))?),
            None => None,
        };
        outs.push(GenesisOutput {
            index: *index,
            value: o.value,
            script_public_key: o.script_public_key.clone(),
            redeem_script: redeem,
        });
    }
    let report = verify_genesis(tpl, &ev.covenant_id, &g.txid, outpoint, &outs).map_err(|e: GenesisError| e.to_string())?;

    // 4. C2: live mint authority
    let live_minters = if report.minter_outputs.is_empty() && tpl.family == Family::Kron {
        // no genesis minter: the program never lets one appear later (verify_genesis accepted the output as an instance of an
        // embedded program, and the embedded KRON programs are exactly the two `review_b2_kron` r_kr_15 tests this on)
        Some(vec![])
    } else {
        ev.live_minters.as_ref().map(|l| l.iter().map(|c| format!("{}:{}", c.txid, c.index)).collect())
    };

    let mut source = format!(
        "genesis tx {} from {}; txid recomputed, covenant id recomputed over the {} genesis output(s), each redeem script matched to its P2SH hash",
        g.txid,
        g.source,
        outs.len()
    );
    let reconstructed: Vec<String> =
        g.reveals.iter().filter(|r| r.redeem_script.is_some() && r.revealed_by.is_none()).map(|r| r.index.to_string()).collect();
    if !reconstructed.is_empty() {
        source.push_str(&format!(
            " (output(s) {} unspent: state reconstructed from the issuer's published data)",
            reconstructed.join(", ")
        ));
    }
    let source: String = source.chars().take(512).collect();
    Ok(GenesisRecord {
        txid: g.txid.clone(),
        daa_score: g.accepting_block_daa_score,
        outputs: group.iter().map(|(n, _)| *n).collect(),
        supply: report.supply,
        minter_outputs: report.minter_outputs,
        live_minters,
        checked_at_daa,
        source,
    })
}

/// Verifies every token of an evidence file. Fails when the evidence is for another network than the registry.
pub fn verify_evidence(reg: &Registry, ev: &Evidence) -> Result<Vec<TokenVerdict>, String> {
    if ev.network != reg.network {
        return Err(format!("evidence is for {}, the registry for {}", ev.network, reg.network));
    }
    Ok(ev.tokens.iter().map(|t| verify_token(reg, t, ev.sources.node.virtual_daa_score)).collect())
}

/// Where the registry disagrees with a verdict (the registry must say exactly what the evidence proves).
pub fn registry_mismatches(reg: &Registry, verdicts: &[TokenVerdict]) -> Vec<String> {
    let mut out = vec![];
    for v in verdicts {
        let Some(tok) = reg.tokens.iter().find(|t| t.covenant_id == v.covenant_id) else {
            out.push(format!("{}: not in the registry", v.ticker));
            continue;
        };
        match &v.result {
            Ok(rec) => {
                if tok.genesis_verified != Some(true) {
                    out.push(format!(
                        "{}: evidence verifies the genesis but the registry has genesis_verified {:?}",
                        v.ticker, tok.genesis_verified
                    ));
                }
                match &tok.genesis {
                    None => out.push(format!("{}: the registry has no genesis record", v.ticker)),
                    Some(g) => {
                        // the source text and the liveness DAA are the maintainer's; the facts must match
                        let facts = |r: &GenesisRecord| {
                            (
                                r.txid.clone(),
                                r.daa_score,
                                r.outputs.clone(),
                                r.supply,
                                r.minter_outputs.clone(),
                                r.live_minters.clone(),
                            )
                        };
                        if facts(g) != facts(rec) {
                            out.push(format!(
                                "{}: the registry's genesis record differs from the evidence: registry {:?}, evidence {:?}",
                                v.ticker,
                                facts(g),
                                facts(rec)
                            ));
                        }
                    }
                }
                if tok.official && !v.no_live_minter() {
                    out.push(format!("{}: official, but the evidence does not show the absence of a live mint authority", v.ticker));
                }
            }
            Err(e) => {
                if tok.genesis_verified == Some(true) {
                    out.push(format!("{}: genesis_verified true, but the evidence does not verify: {e}", v.ticker));
                }
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::registry::DEFAULT_REGISTRY_JSON;

    const MAINNET_EVIDENCE: &str = include_str!("../../../registry/evidence/mainnet-genesis.json");

    fn evidence() -> Evidence {
        serde_json::from_str(MAINNET_EVIDENCE).expect("evidence parses")
    }

    #[test]
    fn shipped_registry_matches_the_mainnet_evidence() {
        // C1 and C2 for every KRON token of the shipped registry, re-derived offline from the committed chain evidence
        let reg = Registry::parse(DEFAULT_REGISTRY_JSON).unwrap();
        let ev = evidence();
        let verdicts = verify_evidence(&reg, &ev).unwrap();
        assert_eq!(verdicts.len(), reg.tokens.len(), "evidence for every registry token");
        for v in &verdicts {
            let rec = v.result.as_ref().unwrap_or_else(|e| panic!("{}: {e}", v.ticker));
            assert!(v.genesis_verified() && v.no_live_minter(), "{}", v.ticker);
            assert!(rec.minter_outputs.is_empty(), "{}: no KRON genesis carries a minter", v.ticker);
            assert_eq!(rec.outputs, vec![1, 2], "{}: curve inventory and creator allocation", v.ticker);
        }
        assert_eq!(registry_mismatches(&reg, &verdicts), Vec::<String>::new());
    }

    #[test]
    fn tampered_evidence_fails() {
        let reg = Registry::parse(DEFAULT_REGISTRY_JSON).unwrap();
        let base = evidence();
        let check = |f: &dyn Fn(&mut TokenEvidence)| {
            let mut t = base.tokens[0].clone();
            f(&mut t);
            verify_token(&reg, &t, 0).result.expect_err("tampered evidence must fail")
        };
        // a changed output value: the txid no longer matches
        let e = check(&|t| t.genesis.as_mut().unwrap().tx.outputs[1].value += 1);
        assert!(e.contains("hash to txid"), "{e}");
        // the explorer-omitted sequence guessed wrong
        let e = check(&|t| t.genesis.as_mut().unwrap().tx.inputs[0].sequence = u64::MAX);
        assert!(e.contains("hash to txid"), "{e}");
        // a redeem script byte flipped (the state's amount)
        let e = check(&|t| {
            let r = t.genesis.as_mut().unwrap().reveals[0].redeem_script.as_mut().unwrap();
            let flipped = if &r[80..82] == "00" { "01" } else { "00" };
            r.replace_range(80..82, flipped);
        });
        assert!(e.contains("does not match the P2SH"), "{e}");
        // an unrevealed output
        let e = check(&|t| t.genesis.as_mut().unwrap().reveals[1].redeem_script = None);
        assert!(e.contains("not revealed"), "{e}");
        // the wrong genesis transaction (another token's)
        let other = base.tokens[1].genesis.clone();
        let e = check(&|t| t.genesis = other.clone());
        assert!(e.contains("no output bound"), "{e}");
        // a template the registry does not give the token
        let e = check(&|t| t.template_id = "kron-2732".into());
        assert!(e.contains("template"), "{e}");
        // network mismatch
        let mut ev = base.clone();
        ev.network = "testnet-10".into();
        assert!(verify_evidence(&reg, &ev).is_err());
    }

    #[test]
    fn a_genesis_minter_leaves_c2_undetermined_unless_traced() {
        let reg = Registry::parse(DEFAULT_REGISTRY_JSON).unwrap();
        let ev = evidence();
        let v = verify_token(&reg, &ev.tokens[0], 7);
        let mut rec = v.result.unwrap();
        assert_eq!((rec.live_minters.clone(), rec.checked_at_daa), (Some(vec![]), 7));
        // the registry check refuses a record that claims a different liveness
        let mut r2 = reg.clone();
        rec.live_minters = Some(vec![format!("{}:0", "ab".repeat(32))]);
        r2.tokens[0].genesis = Some(rec);
        let verdicts = verify_evidence(&r2, &ev).unwrap();
        assert!(registry_mismatches(&r2, &verdicts).iter().any(|m| m.starts_with("KRON: the registry's genesis record differs")));
    }
}
