//! The committed `contracts/artifacts/*.json` must be exactly what the vendored compiler produces
//! from the committed sources and constructor arguments, and must load through `kob-protocol`.
//! (`scripts/build-contracts.sh --check` verifies the same at file level with the silverc binary.)

mod common;

use std::path::PathBuf;

use silverscript_abi::ArtifactValue;
use silverscript_lang::compiler::compile_to_sil_abi_artifact;

fn sources_with_ctor() -> Vec<PathBuf> {
    fn walk(dir: &std::path::Path, out: &mut Vec<PathBuf>) {
        for entry in std::fs::read_dir(dir).expect("read dir") {
            let path = entry.expect("dir entry").path();
            if path.is_dir() {
                // contracts/argent holds Argent apps: argentc output (generated .sil without constructor
                // files, checked by scripts/build-argent.sh and the router harness) and their sources.
                if path.file_name().is_some_and(|n| n == "argent") {
                    continue;
                }
                walk(&path, out);
            } else if path.extension().is_some_and(|e| e == "sil") {
                out.push(path);
            }
        }
    }
    let mut out = Vec::new();
    walk(&common::repo_root().join("contracts"), &mut out);
    out.sort();
    out
}

#[test]
fn committed_artifacts_match_sources() {
    let sources = sources_with_ctor();
    // 25 since the pair phase (KobCross replaced by KobPair, KobCondPair, KobIfdPair: one template each for both sides and
    // both families of a token pair)
    assert_eq!(sources.len(), 25, "expected 25 contract sources, found {sources:?}");
    for src in sources {
        let name = src.file_stem().unwrap().to_string_lossy().to_string();
        let ctor_path = src.with_extension("ctor.json");
        let ctor: Vec<ArtifactValue> = serde_json::from_slice(&std::fs::read(&ctor_path).expect("read ctor")).expect("parse ctor");
        let fresh = compile_to_sil_abi_artifact(&std::fs::read_to_string(&src).expect("read source"), &ctor).expect("compile");

        let committed_json =
            std::fs::read_to_string(common::repo_root().join(format!("contracts/artifacts/{name}.json"))).expect("read artifact");
        let committed = kob_protocol::artifacts::parse_artifact(&committed_json).expect("artifact loads via kob-protocol");

        let a = serde_json::to_value(&fresh.contracts).unwrap();
        let b = serde_json::to_value(&committed.contracts).unwrap();
        for (contract, fresh_contract) in a.as_object().unwrap() {
            let committed_contract = &b[contract];
            assert_eq!(fresh_contract["compiled"], committed_contract["compiled"], "{name}: compiled section differs");
            assert_eq!(fresh_contract["entries"], committed_contract["entries"], "{name}: entries differ");
        }
        assert_eq!(fresh.structs, committed.structs, "{name}: structs differ");
    }
}
