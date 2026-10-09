#![allow(dead_code)]

pub mod kron;

use std::path::{Path, PathBuf};

/// Repository root (this crate lives in `crates/kob-tests`).
pub fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..").canonicalize().expect("repo root")
}

fn find_file(dir: &Path, file_name: &str) -> Option<PathBuf> {
    let mut entries: Vec<_> = std::fs::read_dir(dir).ok()?.filter_map(Result::ok).map(|e| e.path()).collect();
    entries.sort();
    for path in entries {
        if path.is_dir() {
            if let Some(found) = find_file(&path, file_name) {
                return Some(found);
            }
        } else if path.file_name().is_some_and(|n| n == file_name) {
            return Some(path);
        }
    }
    None
}

/// Asserts that the committed `contracts/**/<name>.sil` is `generated` (a KOB program derived from the reference KCC-20
/// program by a `kob_protocol::kcc20` generator). With `KOB_REGEN=1` it writes `generated` there instead, which is
/// how those files are regenerated after the reference program changes.
pub fn check_generated_source(name: &str, generated: &str) {
    let file = format!("{name}.sil");
    let path =
        find_file(&repo_root().join("contracts"), &file).unwrap_or_else(|| panic!("contract {file} not found under contracts/"));
    if std::env::var("KOB_REGEN").is_ok_and(|v| v == "1") {
        std::fs::write(&path, generated).expect("write generated source");
    }
    let committed = std::fs::read_to_string(&path).expect("read contract").replace("\r\n", "\n");
    assert_eq!(committed, generated.replace("\r\n", "\n"), "{name}.sil differs from its generator: rerun with KOB_REGEN=1");
}

/// Source of `contracts/**/<name>.sil`. Ablation runs (a check removed from a contract) point the
/// environment variable `KOB_ABLATION_SRC` at a directory holding a mutated `<name>.sil`; that copy
/// wins over the committed source, so the committed contracts are never touched by a mutation run.
pub fn contract_source(name: &str) -> String {
    let file = format!("{name}.sil");
    if let Some(dir) = std::env::var_os("KOB_ABLATION_SRC") {
        let p = Path::new(&dir).join(&file);
        if p.exists() {
            return std::fs::read_to_string(p).expect("read ablation source");
        }
    }
    let path =
        find_file(&repo_root().join("contracts"), &file).unwrap_or_else(|| panic!("contract {file} not found under contracts/"));
    std::fs::read_to_string(path).expect("read contract")
}

/// Bytes of a pinned KRON token template from `contracts/adapters/kron/templates/`.
pub fn kron_template(file: &str) -> Vec<u8> {
    std::fs::read(repo_root().join("contracts/adapters/kron/templates").join(file)).expect("read kron template")
}

use kaspa_consensus_core::hashing::sighash::SigHashReusedValuesUnsync;
use kaspa_consensus_core::tx::{
    CovenantBinding, PopulatedTransaction, ScriptPublicKey, Transaction, TransactionId, TransactionInput, TransactionOutpoint,
    TransactionOutput, UtxoEntry, VerifiableTransaction,
};
use kaspa_consensus_core::Hash;
use kaspa_txscript::caches::Cache;
use kaspa_txscript::covenants::CovenantsContext;
use kaspa_txscript::opcodes::codes::OpTrue;
use kaspa_txscript::script_builder::ScriptBuilder;
use kaspa_txscript::{pay_to_script_hash_script, EngineCtx, EngineFlags, TxScriptEngine};
use kaspa_txscript_errors::TxScriptError;
use silverscript_abi::{
    encode_contract_covenant_decl_sig_script, encode_contract_entry_sig_script, ArtifactValue, CodecError, CodecResult,
    SilAbiArtifact, SilContractArtifact, SilEntryArtifact,
};
use silverscript_lang::compiler::{
    compile_to_sil_abi_artifact_with_options, CompileOptions, CompiledStateLayout, CompilerError, CovenantDeclCallOptions,
};

pub const COV_A: Hash = Hash::from_bytes(*b"AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA");
pub const COV_B: Hash = Hash::from_bytes(*b"BBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBB");

pub fn compile_contract(
    source: &str,
    constructor_args: &[ArtifactValue],
    options: CompileOptions,
) -> Result<SilAbiArtifact, CompilerError> {
    compile_to_sil_abi_artifact_with_options(source, constructor_args, options)
}

pub fn single_contract(artifact: &SilAbiArtifact) -> &SilContractArtifact {
    let Some(contract) = artifact.contracts.values().next().filter(|_| artifact.contracts.len() == 1) else {
        panic!("expected exactly one contract, found {}", artifact.contracts.len());
    };
    contract
}

pub fn bytecode(artifact: &SilAbiArtifact) -> Vec<u8> {
    single_contract(artifact).compiled.bytecode.clone()
}

pub fn state_layout(artifact: &SilAbiArtifact) -> CompiledStateLayout {
    let span = single_contract(artifact).compiled.state_span;
    CompiledStateLayout { start: span.offset, len: span.len }
}

pub fn entry_by_name<'a>(artifact: &'a SilAbiArtifact, name: &str) -> Option<&'a SilEntryArtifact> {
    single_contract(artifact).entry(name)
}

pub fn template_hash(artifact: &SilAbiArtifact) -> [u8; 32] {
    single_contract(artifact).compiled.template_hash
}

pub fn build_sig_script_for_covenant_decl(
    artifact: &SilAbiArtifact,
    function_name: &str,
    args: Vec<ArtifactValue>,
    options: CovenantDeclCallOptions,
) -> Result<Vec<u8>, CompilerError> {
    let Some((contract_name, _)) = artifact.contracts.first_key_value().filter(|_| artifact.contracts.len() == 1) else {
        return Err(CompilerError::Unsupported(format!("expected exactly one contract, found {}", artifact.contracts.len())));
    };
    encode_contract_covenant_decl_sig_script(artifact, contract_name, function_name, options.is_leader, &args)
        .map_err(|err| CompilerError::Unsupported(err.to_string()))
}

/// Encodes an invocation for an artifact containing exactly one contract and
/// exactly one public entrypoint.
pub fn encode_single_entry_sig_script(artifact: &SilAbiArtifact, args: &[ArtifactValue]) -> CodecResult<Vec<u8>> {
    let Some((contract_name, contract)) = artifact.contracts.first_key_value().filter(|_| artifact.contracts.len() == 1) else {
        return Err(CodecError::UnsupportedType(format!("expected exactly one contract, found {}", artifact.contracts.len())));
    };
    let Some((entry_name, _)) = contract.entries.first_key_value().filter(|_| contract.entries.len() == 1) else {
        return Err(CodecError::UnsupportedType(format!(
            "expected exactly one entry in contract `{}`, found {}",
            contract_name,
            contract.entries.len()
        )));
    };
    encode_contract_entry_sig_script(artifact, contract_name, entry_name, args)
}

/// Encodes a named entry invocation for an artifact containing exactly one
/// contract.
pub fn encode_entry_sig_script(artifact: &SilAbiArtifact, entry_name: &str, args: &[ArtifactValue]) -> CodecResult<Vec<u8>> {
    let Some((contract_name, _)) = artifact.contracts.first_key_value().filter(|_| artifact.contracts.len() == 1) else {
        return Err(CodecError::UnsupportedType(format!("expected exactly one contract, found {}", artifact.contracts.len())));
    };
    encode_contract_entry_sig_script(artifact, contract_name, entry_name, args)
}

pub fn push_redeem_script(bytecode: &[u8]) -> Vec<u8> {
    ScriptBuilder::with_flags(EngineFlags::default()).add_data(bytecode).expect("push redeem script").drain()
}

pub fn covenant_decl_sigscript(compiled: &SilAbiArtifact, function_name: &str, args: Vec<ArtifactValue>, is_leader: bool) -> Vec<u8> {
    let mut sigscript = build_sig_script_for_covenant_decl(compiled, function_name, args, CovenantDeclCallOptions { is_leader })
        .expect("build covenant declaration sigscript");
    sigscript.extend_from_slice(&push_redeem_script(&bytecode(compiled)));
    sigscript
}

pub fn covenant_utxo(compiled: &SilAbiArtifact, covenant_id: Hash) -> UtxoEntry {
    UtxoEntry::new(1_500, pay_to_script_hash_script(&bytecode(compiled)), 0, false, Some(covenant_id))
}

pub fn plain_covenant_output(authorizing_input: u16, covenant_id: Hash) -> TransactionOutput {
    TransactionOutput {
        value: 1_000,
        script_public_key: ScriptPublicKey::new(0, vec![OpTrue].into()),
        covenant: Some(kaspa_consensus_core::tx::CovenantBinding { authorizing_input, covenant_id }),
    }
}

pub fn plain_utxo(covenant_id: Hash) -> UtxoEntry {
    UtxoEntry::new(1_500, ScriptPublicKey::new(0, vec![OpTrue].into()), 0, false, Some(covenant_id))
}

pub fn execute_input_with_covenants(tx: Transaction, entries: Vec<UtxoEntry>, input_idx: usize) -> Result<(), TxScriptError> {
    let reused_values = SigHashReusedValuesUnsync::new();
    let sig_cache = Cache::new(10_000);
    let input: TransactionInput = tx.inputs[input_idx].clone();
    let populated = PopulatedTransaction::new(&tx, entries);
    let cov_ctx = CovenantsContext::from_tx(&populated).map_err(TxScriptError::from)?;
    let utxo = populated.utxo(input_idx).expect("selected input utxo");

    let mut vm = TxScriptEngine::from_transaction_input(
        &populated,
        &input,
        input_idx,
        utxo,
        EngineCtx::new(&sig_cache).with_reused(&reused_values).with_covenants_ctx(&cov_ctx),
        EngineFlags { sigop_script_units: 0.into() },
    );
    vm.execute()
}

pub fn assert_verify_like_error(err: TxScriptError) {
    assert!(matches!(err, TxScriptError::VerifyError | TxScriptError::EvalFalse), "expected verify/eval-false, got {err:?}");
}

pub fn tx_input(index: u32, signature_script: Vec<u8>) -> TransactionInput {
    TransactionInput::new(
        TransactionOutpoint { transaction_id: TransactionId::from_bytes([index as u8 + 1; 32]), index },
        signature_script,
        0,
        0,
    )
}

pub fn covenant_output(compiled: &SilAbiArtifact, authorizing_input: u16, covenant_id: Hash) -> TransactionOutput {
    TransactionOutput {
        value: 1_000,
        script_public_key: pay_to_script_hash_script(&bytecode(compiled)),
        covenant: Some(CovenantBinding { authorizing_input, covenant_id }),
    }
}

pub fn compiled_template_parts_and_hash(compiled: &SilAbiArtifact) -> (Vec<u8>, Vec<u8>, Vec<u8>) {
    let layout = state_layout(compiled);
    let bytecode = bytecode(compiled);
    let prefix = bytecode[..layout.start].to_vec();
    let suffix = bytecode[layout.start + layout.len..].to_vec();
    let template_hash = template_hash(compiled).to_vec();
    (prefix, suffix, template_hash)
}
