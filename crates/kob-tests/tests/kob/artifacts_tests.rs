//! The committed `contracts/artifacts/*.json` must be exactly what the vendored compiler produces
//! from the committed sources and constructor arguments, and must load through `kob-protocol`.
//! (`scripts/build-contracts.sh --check` verifies the same at file level with the silverc binary.)

#[path = "../common/mod.rs"]
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
                // contracts/research holds the same kind of argentc output (the Argent port of KobAsk, not a KOB contract).
                // contracts/third-party holds vendored upstream programs that are not compiled here (a .sil there is a
                // reference copy, tested where it is used: kcc20_reference_tests.rs).
                if path.file_name().is_some_and(|n| n == "argent" || n == "research" || n == "third-party") {
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
    // both families of a token pair); 26 with the optional batch-leader holder (prototypes/kcc20/p2/KCC20Opt, not issued); 22
    // without the four v1 orders (AskOrder, BidOrder, AskOrderKron, BidOrderKron, removed); 23 with KobCurveAsk
    assert_eq!(sources.len(), 23, "expected 23 contract sources, found {sources:?}");
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

/// H1 class. A trigger-evidence reader (KobCondPair, KobIfdPair, the conditional kinds) takes the
/// evidence input's signature script to start with `0x08` + 8 bytes, the fill amount `nb` of the
/// evidence template's fill (`settle` of the asks and the pair, `fill` of the bids). No other entry of an
/// evidence template may take an `int` or a `byte[8]` first parameter, so no other spend of an evidence
/// order can start like a fill and be read as one. The fills are checked to take `byte[8]` first, so the
/// check cannot pass without looking at them.
#[test]
fn evidence_templates_only_the_fill_starts_with_an_8_byte_push() {
    use kob_protocol::artifacts::{template, TemplateId};
    use silverscript_abi::TypeArtifact;
    // The evidence templates of crates/kob-protocol/src/artifacts.rs (`evidence_refs` and the conditional
    // kinds' plain-order refs), with the D0 builds the D0 conditional kinds read.
    let evidence = [
        (TemplateId::KobAsk, "settle"),
        (TemplateId::KobAskKron, "settle"),
        (TemplateId::KobAskKronD0, "settle"),
        (TemplateId::KobBid, "fill"),
        (TemplateId::KobBidKron, "fill"),
        (TemplateId::KobBidKronD0, "fill"),
        (TemplateId::KobPair, "settle"),
    ];
    let eight = |t: &TypeArtifact| matches!(t, TypeArtifact::Int | TypeArtifact::FixedBytes { len: 8 });
    for (tid, fill) in evidence {
        let entries = &template(tid).contract().entries;
        let f = entries.get(fill).unwrap_or_else(|| panic!("{}: no `{fill}` entry", tid.name()));
        assert_eq!(
            f.params.first().map(|p| &p.ty),
            Some(&TypeArtifact::FixedBytes { len: 8 }),
            "{}: `{fill}` must take byte[8] first",
            tid.name()
        );
        assert!(entries.len() >= 2, "{}: the fill and at least the cancel", tid.name());
        for (name, e) in entries {
            if name == fill {
                continue;
            }
            if let Some(p) = e.params.first() {
                assert!(
                    !eight(&p.ty),
                    "{}: entry `{name}` takes {:?} `{}` first, so its signature script can start like a fill",
                    tid.name(),
                    p.ty,
                    p.name
                );
            }
            println!("POSITIVE {}::{name}: first parameter {:?}, not an 8-byte push", tid.name(), e.params.first().map(|p| &p.ty));
        }
    }
}
