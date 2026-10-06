//! Validation through the rusty-kaspa v2.1.0 script engine.
//!
//! Runs every input of a transaction the way consensus does (`check_scripts`): covenant context
//! from the transaction, script-unit limit from the committed compute budget, sig-op price of
//! 1,000 grams. On top of the scripts it checks the transaction-level rules the engine does not see:
//! the exact storage-mass commitment, the relay fee floor, static sig-op bounds and block limits.

use kaspa_consensus_core::hashing::sighash::SigHashReusedValuesUnsync;
use kaspa_consensus_core::tx::{PopulatedTransaction, Transaction, UtxoEntry};
use kaspa_txscript::caches::Cache;
use kaspa_txscript::covenants::CovenantsContext;
use kaspa_txscript::{EngineCtx, EngineFlags, TxScriptEngine};
use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};
use crate::tx::{masses, min_fee, MassReport, SignedTx, MIN_FEE_RATE};

/// Script units charged per signature operation (mass_per_sig_op 1,000 grams x 100).
pub const SIGOP_SCRIPT_UNITS: u64 = 100_000;

/// Per-input engine outcome: used script units or the script error.
pub type InputOutcome = std::result::Result<u64, String>;

/// Executes every input. `enforce` applies each input's committed compute budget; otherwise the
/// meter is unlimited.
pub fn execute(tx: &Transaction, entries: &[UtxoEntry], enforce: bool) -> Result<Vec<InputOutcome>> {
    if entries.len() != tx.inputs.len() {
        return Err(Error::Engine("entry count differs from input count".into()));
    }
    let populated = PopulatedTransaction::new(tx, entries.to_vec());
    let cov_ctx = CovenantsContext::from_tx(&populated).map_err(|e| Error::Engine(format!("covenant context: {e:?}")))?;
    let reused = SigHashReusedValuesUnsync::new();
    let cache = Cache::new(10_000);
    Ok((0..tx.inputs.len())
        .map(|i| {
            let input = tx.inputs[i].clone();
            let limit = if enforce { input.compute_commit.allowed_script_units() } else { u64::MAX.into() };
            let mut vm = TxScriptEngine::from_transaction_input_with_script_units_limit(
                &populated,
                &input,
                i,
                &entries[i],
                EngineCtx::new(&cache).with_reused(&reused).with_covenants_ctx(&cov_ctx),
                EngineFlags { sigop_script_units: SIGOP_SCRIPT_UNITS.into() },
                limit,
            );
            vm.execute().map(|_| vm.used_script_units().0).map_err(|e| format!("{e:?}"))
        })
        .collect())
}

/// Script units every input needs (unlimited meter); fails if any input fails.
pub fn measure_units(tx: &Transaction, entries: &[UtxoEntry]) -> Result<Vec<u64>> {
    execute(tx, entries, false)?
        .into_iter()
        .enumerate()
        .map(|(i, r)| r.map_err(|e| Error::Engine(format!("input {i}: {e}"))))
        .collect()
}

/// Result of [`validate`].
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Validation {
    /// Script units used by each input.
    pub units: Vec<u64>,
    /// Committed compute budget of each input.
    pub budgets: Vec<u16>,
    /// Static sig-op upper bound of each input.
    pub sigops: Vec<u32>,
    pub mass: MassReport,
    #[serde(with = "crate::json::field")]
    pub fee: u64,
    #[serde(with = "crate::json::field")]
    pub min_fee: u64,
}

/// Full validation of a signed transaction: scripts with enforced budgets, storage-mass
/// commitment, fee floor, static sig-op cap of 15 per input, block mass limits.
pub fn validate(tx: &Transaction, entries: &[UtxoEntry]) -> Result<Validation> {
    let res = execute(tx, entries, true)?;
    let mut units = vec![];
    for (i, r) in res.into_iter().enumerate() {
        units.push(r.map_err(|e| Error::Engine(format!("input {i}: {e}")))?);
    }
    let mass = masses(tx, entries);
    if tx.storage_mass() != mass.storage {
        return Err(Error::Engine(format!("storage mass commitment {} != computed {}", tx.storage_mass(), mass.storage)));
    }
    let total_in: u64 = entries.iter().map(|e| e.amount).sum();
    let total_out: u64 = tx.outputs.iter().map(|o| o.value).sum();
    if total_out > total_in {
        return Err(Error::Engine(format!("outputs {total_out} exceed inputs {total_in}")));
    }
    let fee = total_in - total_out;
    let min = min_fee(&mass, MIN_FEE_RATE);
    if fee < min {
        return Err(Error::Engine(format!("fee {fee} below the floor {min}")));
    }
    if !mass.within_block_limits() {
        return Err(Error::Engine(format!("mass exceeds block limits: {mass:?}")));
    }
    let mut sigops = vec![];
    for (i, input) in tx.inputs.iter().enumerate() {
        let so = kaspa_txscript::get_sig_op_count_upper_bound::<PopulatedTransaction, SigHashReusedValuesUnsync>(
            &input.signature_script,
            &entries[i].script_public_key,
        ) as u32;
        if so > 15 {
            return Err(Error::Engine(format!("input {i}: static sig-op bound {so} > 15")));
        }
        sigops.push(so);
    }
    let budgets = tx.inputs.iter().map(|i| i.compute_commit.compute_budget().unwrap_or(0)).collect();
    Ok(Validation { units, budgets, sigops, mass, fee, min_fee: min })
}

/// [`validate`] for a [`SignedTx`].
pub fn validate_signed(signed: &SignedTx) -> Result<Validation> {
    let (tx, entries) = signed.tx.to_tx()?;
    validate(&tx, &entries)
}
