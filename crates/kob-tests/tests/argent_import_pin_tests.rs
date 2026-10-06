//! Argentc (pinned upstream + patch 0002) imports a published app artifact only under the id the
//! importing source pins (`import "./KOBOrders/artifact.json" id "<id>";`).
//!
//! A forged but self-consistent `KOBOrders` (here: `KobAsk` with its maker payout check removed, same state and
//! ABI, wrapped by the shipped `sil2argent build`) passes `check_consistency`; before the pin, argentc compiled the
//! third-party closed-ICC example against it and linked the forged handle. With the pin it refuses, and it refuses
//! an artifact import without a pin. The concern is specific to artifact-backed imports: a source import of the same
//! app (`import "../KOBOrders.ag";`) compiles it and links the handle argentc derived itself (docs/argent-feedback.md,
//! item 4). Needs the argentc and sil2argent binaries (`argent/build-argentc.sh`,
//! `cargo build -p sil2argent`): `KOB_ARGENTC` / `KOB_SIL2ARGENT`, else `<target>/argent/release/argentc` and
//! `<target>/debug/sil2argent` of `CARGO_TARGET_DIR` or the repo's `target/`. Skipped when they are absent
//! (scripts/build-argent.sh --check exercises the pin with the genuine artifact in CI).

mod common;

use std::path::PathBuf;
use std::process::Command;

use silverscript_abi::ArtifactValue;
use silverscript_lang::compiler::compile_to_sil_abi_artifact;

const ACTORS: [&str; 9] =
    ["KobAsk", "KobBid", "KobCondAsk", "KobCondBid", "KobCondPair", "KobIfdAsk", "KobIfdBid", "KobIfdPair", "KobPair"];

fn tool(var: &str, rel: &str) -> Option<PathBuf> {
    if let Ok(p) = std::env::var(var) {
        return Some(PathBuf::from(p)).filter(|p| p.exists());
    }
    let mut dirs = vec![common::repo_root().join("target")];
    if let Ok(t) = std::env::var("CARGO_TARGET_DIR") {
        dirs.insert(0, PathBuf::from(t));
    }
    for d in dirs {
        for ext in ["", ".exe"] {
            let p = d.join(format!("{rel}{ext}"));
            if p.exists() {
                return Some(p);
            }
        }
    }
    None
}

fn run(cmd: &mut Command) -> (bool, String) {
    let o = cmd.output().expect("spawn");
    (o.status.success(), format!("{}{}", String::from_utf8_lossy(&o.stdout), String::from_utf8_lossy(&o.stderr)))
}

#[test]
fn argentc_imports_only_the_pinned_kob_orders_artifact() {
    let (Some(s2a), Some(argentc)) = (tool("KOB_SIL2ARGENT", "debug/sil2argent"), tool("KOB_ARGENTC", "argent/release/argentc"))
    else {
        println!("SKIPPED: argentc / sil2argent not found (KOB_ARGENTC, KOB_SIL2ARGENT)");
        return;
    };
    let root = common::repo_root();
    let work = root.join("target/import_pin");
    let _ = std::fs::remove_dir_all(&work);
    std::fs::create_dir_all(work.join("genuine/examples")).unwrap();
    std::fs::create_dir_all(work.join("forged/examples")).unwrap();
    std::fs::create_dir_all(work.join("forged/KOBOrders")).unwrap();
    std::fs::create_dir_all(work.join("genuine/KOBOrders")).unwrap();
    let example = std::fs::read_to_string(root.join("contracts/argent/examples/closed_icc_gate.ag")).unwrap();
    let published = root.join("contracts/argent/KOBOrders/artifact.json");

    // the forged KOBOrders: KobAsk without the maker payout check, wrapped by sil2argent (consistent, new id)
    let good = std::fs::read_to_string(root.join("contracts/v2/KobAsk.sil")).unwrap().replace("\r\n", "\n");
    let needle = "            require(tx.outputs[self].scriptPubKey == byte[](new ScriptPubKeyP2PK(maker)));\n";
    assert_eq!(good.matches(needle).count(), 1);
    let ctor: Vec<ArtifactValue> =
        serde_json::from_slice(&std::fs::read(root.join("contracts/v2/KobAsk.ctor.json")).unwrap()).unwrap();
    let evil = compile_to_sil_abi_artifact(&good.replace(needle, ""), &ctor).expect("compile the forged KobAsk");
    let evil_json = work.join("EvilKobAsk.json");
    std::fs::write(&evil_json, serde_json::to_string_pretty(&evil).unwrap() + "\n").unwrap();
    let forged = work.join("forged/KOBOrders/artifact.json");
    let mut cmd = Command::new(&s2a);
    cmd.arg("build").arg(&published).arg(&forged);
    for a in ACTORS {
        let p = if a == "KobAsk" { evil_json.clone() } else { root.join(format!("contracts/artifacts/{a}.json")) };
        cmd.arg(format!("{a}={}", p.display()));
    }
    let (ok, log) = run(&mut cmd);
    assert!(ok, "sil2argent build: {log}");

    let build = |dir: &str, src: &str| {
        std::fs::write(work.join(dir).join("examples/closed_icc_gate.ag"), src).unwrap();
        run(Command::new(&argentc).current_dir(work.join(dir).join("examples")).args([
            "build",
            "closed_icc_gate.ag",
            "--out",
            "../out",
        ]))
    };
    // the genuine artifact under its pin compiles
    std::fs::copy(&published, work.join("genuine/KOBOrders/artifact.json")).unwrap();
    let (ok, log) = build("genuine", &example);
    assert!(ok, "the pinned genuine artifact must compile: {log}");
    // the forged artifact under the genuine pin is refused
    let (ok, log) = build("forged", &example);
    assert!(!ok && log.contains("the import pins"), "argentc must refuse the forged artifact: {log}");
    // an artifact import without a pin is refused
    let unpinned: String = example
        .lines()
        .map(|l| if l.starts_with("import \"../KOBOrders/artifact.json\"") { "import \"../KOBOrders/artifact.json\";" } else { l })
        .collect::<Vec<_>>()
        .join("\n");
    let (ok, log) = build("genuine", &unpinned);
    assert!(!ok && log.contains("has no pinned id"), "argentc must refuse an unpinned artifact import: {log}");

    // The concern is specific to artifact-backed imports: the linked handle is whatever the artifact says (the genuine
    // handle above, the forged one without the pin). A SOURCE import compiles the exporter and derives the handle
    // itself: importing the interface source KOBOrders.ag links the handle of what argentc compiled from it (its
    // placeholder skeleton), never one an artifact supplies.
    let linked = |dir: &str| {
        let sil = std::fs::read_to_string(work.join(dir).join("out/sil/PriceGate.sil")).expect("PriceGate.sil");
        let line = sil.lines().find(|l| l.contains("gen__kob_orders__kob_ask_template_const =")).expect("linked KobAsk handle");
        line.split("0x").nth(1).expect("hex").chars().take(64).collect::<String>()
    };
    let hex = |v: &serde_json::Value| -> String {
        v.as_array().expect("byte array").iter().map(|b| format!("{:02x}", b.as_u64().expect("byte"))).collect()
    };
    let handle_of = |artifact: &std::path::Path| -> String {
        let a: serde_json::Value = serde_json::from_slice(&std::fs::read(artifact).expect("artifact")).expect("artifact json");
        let t = a["argent"]["template_plan"]["templates"].as_array().expect("templates");
        hex(&t.iter().find(|t| t["actor"] == "KobAsk").expect("KobAsk template")["sil_template_hash"])
    };
    let (ok, log) = build("genuine", &example);
    assert!(ok, "{log}");
    assert_eq!(linked("genuine"), handle_of(&published), "an artifact import links the artifact's handle");
    std::fs::create_dir_all(work.join("source/examples")).unwrap();
    std::fs::copy(root.join("contracts/argent/KOBOrders.ag"), work.join("source/KOBOrders.ag")).unwrap();
    let from_source: String = example
        .lines()
        .map(|l| if l.starts_with("import \"../KOBOrders/artifact.json\"") { "import \"../KOBOrders.ag\";" } else { l })
        .collect::<Vec<_>>()
        .join("\n");
    let (ok, log) = build("source", &from_source);
    assert!(ok, "a source import of the interface compiles: {log}");
    let derived = handle_of(&work.join("source/out/apps/KOBOrders/artifact.json"));
    assert_eq!(linked("source"), derived, "a source import links the handle argentc derived from the source it compiled");
    assert_ne!(derived, handle_of(&published), "the interface source is not the hand-written order");
}
