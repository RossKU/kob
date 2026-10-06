//! The committed Argent artifacts (contracts/argent) against the committed silverc artifacts
//! (contracts/artifacts). These run without argentc; scripts/build-argent.sh --check is the
//! reproducibility check that needs it.

use std::path::{Path, PathBuf};

use argent_artifact::Artifact;
use sil2argent::{check_links, transplant, verify, verify_handles, ActorSource};

fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..").canonicalize().expect("repo root")
}

fn read<T: serde::de::DeserializeOwned>(rel: &str) -> T {
    let p = root().join(rel);
    serde_json::from_str(&std::fs::read_to_string(&p).unwrap_or_else(|e| panic!("read {}: {e}", p.display())))
        .unwrap_or_else(|e| panic!("parse {}: {e}", p.display()))
}

fn source(actor: &str) -> ActorSource {
    ActorSource {
        actor: actor.into(),
        label: format!("contracts/artifacts/{actor}.json"),
        abi: read(&format!("contracts/artifacts/{actor}.json")),
    }
}

/// Every contracts/v2/*.sil is an actor of KOBOrders, and nothing else is.
fn order_names() -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(root().join("contracts/v2"))
        .expect("contracts/v2")
        .filter_map(|e| e.ok())
        .filter_map(|e| e.file_name().to_str().and_then(|n| n.strip_suffix(".sil")).map(str::to_string))
        .collect();
    names.sort();
    names
}

fn orders() -> Artifact {
    read("contracts/argent/KOBOrders/artifact.json")
}

fn orders_kron() -> Artifact {
    read("contracts/argent/KOBOrdersKron/artifact.json")
}

/// The KRON limit orders of KOBOrdersKron.
const KRON_ORDERS: [&str; 2] = ["KobAskKron", "KobBidKron"];

#[test]
fn kob_orders_kron_wraps_the_kron_limit_orders() {
    let art = orders_kron();
    assert_eq!(art.app, "KOBOrdersKron");
    let mut actors: Vec<String> = art.argent.actors.iter().map(|a| a.name.clone()).collect();
    actors.sort();
    assert_eq!(actors, KRON_ORDERS);
    let sources: Vec<ActorSource> = KRON_ORDERS.iter().map(|n| source(n)).collect();
    let log = verify(&art, &sources).expect("KOBOrdersKron verifies against the silverc artifacts");
    assert_eq!(log.len(), 2);
    assert!(art.dependencies.is_empty(), "KOBOrdersKron has no app dependencies");
    // the router and its generator pin it
    let pin_line = format!("import \"./KOBOrdersKron/artifact.json\" id \"{}\";", art.id);
    for file in ["tools/router-gen/router_head.ag", "contracts/argent/kob_router.ag"] {
        let text = std::fs::read_to_string(root().join(file)).expect(file);
        assert!(text.lines().any(|l| l == pin_line), "{file} does not pin KOBOrdersKron {}", art.id);
    }
}

#[test]
fn kob_orders_wraps_every_v2_contract() {
    let names = order_names();
    assert_eq!(names.len(), 9, "{names:?}");
    let art = orders();
    let mut actors: Vec<String> = art.argent.actors.iter().map(|a| a.name.clone()).collect();
    actors.sort();
    assert_eq!(actors, names);
    let sources: Vec<ActorSource> = names.iter().map(|n| source(n)).collect();
    let log = verify(&art, &sources).expect("KOBOrders verifies against the silverc artifacts");
    assert_eq!(log.len(), 9);
    assert_eq!(art.app, "KOBOrders");
    assert!(art.dependencies.is_empty(), "KOBOrders has no app dependencies");
}

#[test]
fn kob_orders_handles_are_instance_independent_templates() {
    // One template per order kind: the handle is the template hash of the silverc artifact, and
    // the state span is non-empty (the per-order values live in it).
    let art = orders();
    for name in order_names() {
        let src = source(&name);
        let compiled = &src.abi.contracts[&name].compiled;
        assert!(compiled.state_span.len > 0, "{name}: empty state span");
        let receipt = art.argent.template_plan.templates.iter().find(|t| t.actor == name).expect("template receipt");
        assert_eq!(receipt.actor_type_handle.template.hash, compiled.template_hash, "{name}");
        assert_eq!(receipt.actor_type_handle.template.prefix.len(), compiled.state_span.offset, "{name}");
        assert_eq!(
            receipt.actor_type_handle.template.prefix.len()
                + compiled.state_span.len
                + receipt.actor_type_handle.template.suffix.len(),
            compiled.bytecode.len(),
            "{name}"
        );
        assert!(receipt.actor_type_handle.context_fields.is_empty(), "{name}: compiler-owned context in a hand-written handle");
    }
}

#[test]
fn kob_token_is_the_8x8_reference_program() {
    let art: Artifact = read("contracts/argent/KOBToken/artifact.json");
    assert_eq!(art.app, "KOBToken");
    let log = verify_handles(&art, &[ActorSource { actor: "KCC20".into(), ..source_named("KCC20Ref_8x8", "KCC20") }])
        .expect("KOBToken handle is the silverc template of KCC20Ref_8x8");
    assert_eq!(log.len(), 1);
    // and it is not the 3/3 reference token
    let ref33 = ActorSource { actor: "KCC20".into(), ..source_named("KCC20Ref", "KCC20") };
    assert!(verify_handles(&art, &[ref33]).is_err(), "8/8 and 3/3 tokens must have different templates");
}

fn source_named(file: &str, actor: &str) -> ActorSource {
    ActorSource {
        actor: actor.into(),
        label: format!("contracts/artifacts/{file}.json"),
        abi: read(&format!("contracts/artifacts/{file}.json")),
    }
}

#[test]
fn router_links_exactly_the_published_artifacts() {
    let router: Artifact = read("contracts/argent/router/artifact.json");
    let published = [orders(), orders_kron(), read::<Artifact>("contracts/argent/KOBToken/artifact.json")];
    let log = check_links(&router, &published).expect("router links the published artifacts");
    // the orders by closed ICC; the token programs are open ICC handles in the intents' state, not linked
    assert_eq!(log.len(), 2, "{log:?}");
    assert!(router.dependencies.iter().all(|d| d.app != "KOBToken"), "the router links no token program");
    // a stale KOBOrders id is refused
    let mut stale = orders();
    stale.id = "00".repeat(32);
    assert!(check_links(&router, &[stale, orders_kron()]).is_err());
    let mut stale = orders_kron();
    stale.id = "00".repeat(32);
    assert!(check_links(&router, &[orders(), stale]).is_err());
}

#[test]
fn transplant_is_idempotent_on_the_published_artifact() {
    let published = orders();
    let mut again = published.clone();
    let sources: Vec<ActorSource> = order_names().iter().map(|n| source(n)).collect();
    transplant(&mut again, &sources).expect("transplant");
    assert_eq!(again.id, published.id);
    assert_eq!(again, published);
}

#[test]
fn transplant_refuses_a_contract_with_another_state_or_abi() {
    // KobBid's code under KobAsk's name: different state layout and entries.
    let mut art = orders();
    let mut sources: Vec<ActorSource> = order_names().iter().map(|n| source(n)).collect();
    let bid = source("KobBid");
    let ask = sources.iter_mut().find(|s| s.actor == "KobAsk").unwrap();
    ask.abi = bid.abi.clone();
    // the silverc artifact names its contract KobBid, so KobAsk is missing in it
    let err = transplant(&mut art, &sources).unwrap_err();
    assert!(err.contains("no contract `KobAsk`"), "{err}");

    // Rename the contract inside the artifact to get past that check: the state differs.
    let mut art = orders();
    let mut abi = bid.abi;
    let c = abi.contracts.remove("KobBid").unwrap();
    abi.contracts.insert("KobAsk".into(), c);
    let mut sources: Vec<ActorSource> = order_names().iter().map(|n| source(n)).collect();
    sources.iter_mut().find(|s| s.actor == "KobAsk").unwrap().abi = abi;
    let err = transplant(&mut art, &sources).unwrap_err();
    assert!(err.contains("runtime state") || err.contains("entry ABI"), "{err}");
}

#[test]
fn transplant_refuses_an_unbacked_actor() {
    let mut art = orders();
    let sources: Vec<ActorSource> = order_names().iter().filter(|n| *n != "KobPair").map(|n| source(n)).collect();
    let err = transplant(&mut art, &sources).unwrap_err();
    assert!(err.contains("KobPair") && err.contains("no hand-written contract"), "{err}");
}

#[test]
fn verify_refuses_a_tampered_handle_or_id() {
    let sources: Vec<ActorSource> = order_names().iter().map(|n| source(n)).collect();
    let mut art = orders();
    art.argent.template_plan.templates.iter_mut().find(|t| t.actor == "KobAsk").unwrap().actor_type_handle.template.hash[0] ^= 1;
    assert!(verify(&art, &sources).is_err(), "tampered handle");
    let mut art = orders();
    art.id = "11".repeat(32);
    assert!(verify(&art, &sources).is_err(), "tampered id");
    // an embedded contract whose bytecode differs from the silverc artifact is refused
    let mut art = orders();
    art.sil_abi.contracts.get_mut("KobAsk").unwrap().compiled.bytecode.push(0x51);
    assert!(verify(&art, &sources).is_err(), "tampered bytecode");
}

#[test]
fn docs_pin_the_published_artifacts() {
    // docs/argent.md quotes the artifact ids and handles; they must be the committed ones.
    let doc = std::fs::read_to_string(root().join("docs/argent.md")).expect("docs/argent.md");
    let hex = |b: &[u8]| b.iter().map(|x| format!("{x:02x}")).collect::<String>();
    let router: Artifact = read("contracts/argent/router/artifact.json");
    let token: Artifact = read("contracts/argent/KOBToken/artifact.json");
    for art in [orders(), orders_kron(), token, router] {
        assert!(doc.contains(&art.id), "docs/argent.md does not pin {} {}", art.app, art.id);
        if art.app != "KobRouter" {
            for t in &art.argent.template_plan.templates {
                assert!(
                    doc.contains(&hex(&t.actor_type_handle.template.hash)),
                    "docs/argent.md does not list the handle of {}",
                    t.actor
                );
            }
        }
    }
}

#[test]
fn docs_list_every_router_entry() {
    // docs/argent.md documents the fill shapes as entries; a shape added to (or dropped from)
    // kob_router.ag without the page is caught here.
    let doc = std::fs::read_to_string(root().join("docs/argent.md")).expect("docs/argent.md");
    let manifest: serde_json::Value = read("contracts/argent/router/manifest.json");
    let mut n = 0;
    for actor in manifest["actors"].as_array().expect("actors") {
        let name = actor["name"].as_str().expect("actor name");
        // one actor per fill shape: `<Intent>_<shape>` (`TokenToKasKron_sell`: the KRON twin); the page names the intent and
        // `_<shape>`
        let (intent, shape) = name.split_once('_').expect("actor name <Intent>_<shape>");
        assert!(doc.contains(&format!("`{intent}`")), "docs/argent.md does not mention the intent {intent}");
        assert!(doc.contains(&format!("`_{shape}`")), "docs/argent.md does not list the actor {name}");
        for entry in actor["entries"].as_array().expect("entries") {
            let e = entry["name"].as_str().expect("entry name");
            assert!(doc.contains(&format!("`{e}(")), "docs/argent.md does not list {name}::{e}");
            n += 1;
        }
    }
    assert_eq!(
        n,
        30 * 3,
        "router: 30 fill shapes (six per intent, two KRON intents), each an actor with its entry, expire and cancel"
    );
}

/// The published KOBOrders id is a pinned constant of the repo (the router and every example import
/// it with `id "..."`, which the patched argentc enforces), and `sil2argent verify --id` refuses any other id.
#[test]
fn kob_orders_id_is_pinned_and_verify_checks_the_pin() {
    let art = orders();
    let pin_line = format!("import \"./KOBOrders/artifact.json\" id \"{}\";", art.id);
    for (file, line) in [
        ("tools/router-gen/router_head.ag", pin_line.clone()),
        ("contracts/argent/kob_router.ag", pin_line.clone()),
        ("contracts/argent/examples/closed_icc_gate.ag", pin_line.replace("./KOBOrders", "../KOBOrders")),
    ] {
        let text = std::fs::read_to_string(root().join(file)).expect(file);
        assert!(text.lines().any(|l| l == line), "{file} does not pin KOBOrders {}", art.id);
    }
    let bin = env!("CARGO_BIN_EXE_sil2argent");
    let run = |id: &str| {
        let mut c = std::process::Command::new(bin);
        c.current_dir(root()).args(["verify", "contracts/argent/KOBOrders/artifact.json", "--id", id]);
        for n in order_names() {
            c.arg(format!("{n}=contracts/artifacts/{n}.json"));
        }
        let o = c.output().expect("run sil2argent");
        (o.status.success(), String::from_utf8_lossy(&o.stderr).to_string())
    };
    assert!(run(&art.id).0, "the published id verifies");
    let (ok, err) = run(&"11".repeat(32));
    assert!(!ok && err.contains("is not the pinned id"), "{err}");
}
