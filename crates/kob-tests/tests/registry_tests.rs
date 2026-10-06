//! `registry/tokens.json` cross-checked against the pinned programs it describes: every template's hash, prefix/suffix/state
//! lengths, slot limits and escrow owner types must equal what the committed artifacts, the pinned KRON program bytes and the
//! order contracts say. The validator itself is unit-tested in `kob_protocol::registry`.

mod common;

use kob_protocol::registry::{Escrow, Family, Registry, Template};
use silverscript_lang::template::template_hash;

fn hexs(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

fn read(rel: &str) -> Vec<u8> {
    std::fs::read(common::repo_root().join(rel)).unwrap_or_else(|e| panic!("read {rel}: {e}"))
}

/// Value of `int constant NAME = N;` in a source file.
fn source_int(src: &str, name: &str) -> i64 {
    let key = format!("int constant {name} = ");
    let start = src.find(&key).unwrap_or_else(|| panic!("{name} not found")) + key.len();
    src[start..].split(';').next().unwrap().trim().parse().unwrap()
}
/// Value of `byte constant NAME = 0xNN;`.
fn source_byte(src: &str, name: &str) -> u8 {
    let key = format!("byte constant {name} = 0x");
    let start = src.find(&key).unwrap_or_else(|| panic!("{name} not found")) + key.len();
    u8::from_str_radix(src[start..].split(';').next().unwrap().trim(), 16).unwrap()
}

fn check_kcc20(t: &Template) {
    let json = String::from_utf8(read(&t.source.path)).unwrap();
    let art = kob_protocol::artifacts::parse_artifact(&json).expect("artifact loads");
    let c = art.contracts.values().next().expect("one contract");
    let off = c.compiled.state_span.offset;
    let len = c.compiled.state_span.len;
    let total = c.compiled.bytecode.len();
    assert_eq!(t.template_hash, hexs(&c.compiled.template_hash), "{}: template hash", t.id);
    assert_eq!(t.prefix_len as usize, off, "{}: prefix_len", t.id);
    assert_eq!(t.state_len as usize, len, "{}: state_len", t.id);
    assert_eq!(t.suffix_len as usize, total - off - len, "{}: suffix_len", t.id);
    // the hash really is the template hash of prefix and suffix of the artifact's own bytecode
    let bc = &c.compiled.bytecode;
    assert_eq!(hexs(&template_hash(&bc[..off], &bc[off + len..])), t.template_hash, "{}: recomputed template hash", t.id);
    // slot limits: from the compiled program's source, or (third-party programs, no source here) from the artifact's own
    // program information, which must also carry the draft's transfer entries and owner schemes
    if t.source.path.starts_with("contracts/third-party/") {
        let v: serde_json::Value = serde_json::from_str(&json).unwrap();
        let info = &v["program_information"];
        assert_eq!(info["max_token_inputs"], t.max_token_inputs, "{}: max token inputs (artifact program_information)", t.id);
        assert_eq!(info["max_token_outputs"], t.max_token_outputs, "{}: max token outputs (artifact program_information)", t.id);
        assert!(info["owner_schemes"].as_array().unwrap().contains(&serde_json::json!(4)), "{}: owner scheme 4 (covenant id)", t.id);
        let tags = c.entries.iter().map(|(n, e)| (n.as_str(), e.dispatch_tag.to_hex())).collect::<std::collections::BTreeMap<_, _>>();
        assert_eq!((tags["transfer"].as_str(), tags["transfer_delegator"].as_str()), ("79c71c23", "fd3ef14a"), "{}: draft tags", t.id);
    } else {
        let stem = std::path::Path::new(&t.source.path).file_stem().unwrap().to_string_lossy().to_string();
        let src = common::contract_source(&stem);
        assert_eq!(
            t.max_token_inputs as i64,
            source_int(&src, "MAX_DELEGATES") + 1,
            "{}: max token inputs = leader + MAX_DELEGATES",
            t.id
        );
        assert_eq!(t.max_token_outputs as i64, source_int(&src, "MAX_TOKEN_OUTPUTS"), "{}: max token outputs", t.id);
    }
    // escrow owner types = what KobAsk custodies
    let ask = String::from_utf8(read("contracts/v2/KobAsk.sil")).unwrap();
    assert_eq!(t.escrow.owner_scheme, Some(source_byte(&ask, "SCHEME_COVID")), "{}: escrow owner scheme", t.id);
    assert_eq!(t.escrow.borrow_scheme, Some(source_byte(&ask, "BORROW_DISABLED")), "{}: escrow borrow scheme", t.id);
}

fn check_kron(t: &Template) {
    let b = read(&t.source.path);
    assert_eq!(t.state_len as usize, 46);
    assert_eq!(t.prefix_len, 0);
    assert_eq!(t.suffix_len as usize, b.len() - 46, "{}: suffix_len", t.id);
    assert_eq!(hexs(&template_hash(&[], &b[46..])), t.template_hash, "{}: template hash", t.id);
    // state span opens with the KRON state pushes: 0x20 owner | 0x01 id_type | 0x08 amount | 0x01 is_minter
    assert_eq!((b[0], b[33], b[35], b[44]), (0x20, 0x01, 0x08, 0x01), "{}: KRON state layout at offset 0", t.id);
    let ask = String::from_utf8(read("contracts/adapters/kron/AskOrderKron.sil")).unwrap();
    assert_eq!(t.escrow.id_type, Some(source_byte(&ask, "TYPE_COVID")), "{}: escrow id_type", t.id);
    assert_eq!(t.escrow.is_minter, Some(source_byte(&ask, "NOT_MINTER")), "{}: escrow is_minter", t.id);
    assert_eq!(t.escrow.delivery_id_type, Some(source_byte(&ask, "TYPE_ADDRESS")), "{}: default delivery id_type", t.id);
    assert_eq!((t.max_token_inputs, t.max_token_outputs), (4, 5), "{}: KRON token limits (1-4 in, 1-5 out)", t.id);
}

#[test]
fn shipped_registry_templates_match_the_pinned_programs() {
    let r: Registry = Registry::parse(&String::from_utf8(read("registry/tokens.json")).unwrap()).expect("tokens.json validates");
    assert_eq!(r.templates.len(), 5);
    for t in &r.templates {
        match t.family {
            Family::Kcc20 => check_kcc20(t),
            Family::Kron => check_kron(t),
        }
        println!(
            "REGISTRY template {:<14} {} suffix={} limits={}/{}",
            t.id, t.template_hash, t.suffix_len, t.max_token_inputs, t.max_token_outputs
        );
    }
    // the ids the order/issue tooling refers to
    for id in ["kcc20-ref-3x3", "kcc20-ref-8x8", "kron-2433", "kron-2732", "kcc20-kaspacom-0-2-5"] {
        assert!(r.template(id).is_some(), "template {id} missing");
    }
    // every template of the file is a program this build embeds (same hash, same slots): the strict list is the pinned list
    for t in &r.templates {
        let hash = kob_protocol::registry::parse_hex32(&t.template_hash).unwrap();
        let p = kob_protocol::artifacts::token_template_by_hash(&hash).unwrap_or_else(|| panic!("{}: not an embedded program", t.id));
        assert_eq!((p.slots.0 as u32, p.slots.1 as u32), (t.max_token_inputs, t.max_token_outputs), "{}: slots", t.id);
        assert_eq!(p.family, t.family, "{}: family", t.id);
    }
    // the KRON family of the 2026-09-29 census (templates A and B): seven graduated tokens and KDIST, identity verified;
    // listing verification 2026-10-03: genesis verified on mainnet data (C1), no live mint authority (C2), all eight listed; six
    // official, the two test tokens (PEPE "The Ultimate test", DNBT "dont buy this is test") not official, with a warning (founder 2026-10-03)
    let kron: Vec<_> = r.tokens.iter().filter(|t| t.family == Family::Kron).collect();
    assert_eq!(kron.len(), 8);
    assert!(kron.iter().all(|t| t.verified
        && t.status == kob_protocol::registry::Status::Listed
        && t.genesis_verified == Some(true)
        && t.genesis.as_ref().is_some_and(|g| g.live_minters == Some(vec![]))));
    let not_official: Vec<&str> = kron.iter().filter(|t| !t.official).map(|t| t.ticker.as_str()).collect();
    assert_eq!(not_official, ["PEPE", "DNBT"]);
    for t in kron.iter().filter(|t| !t.official) {
        let w = t.warning.as_deref().unwrap_or_default();
        assert!(w.starts_with("Not official: a test token per its own name"), "{}: {w}", t.ticker);
    }
    assert!(r.tokens.iter().find(|t| t.ticker == "PEPE").unwrap().warning.as_deref().unwrap().contains("well-known PEPE"));
    assert!(kron.iter().filter(|t| t.official).all(|t| t.warning.is_none()));
    let a = "2ed46a7edf5b168e67dba56998c58255235bebac436940a85115ca31d5c559f2";
    let b = "8097c96fe586a785b3ffb62ddd2a9b3012593421d605d136d26153806e28053e";
    assert_eq!(r.template("kron-2433").unwrap().template_hash, a);
    assert_eq!(r.template("kron-2732").unwrap().template_hash, b);
    for t in &kron {
        let want = if t.ticker == "KDIST" { "kron-2732" } else { "kron-2433" };
        assert_eq!(t.template_id, want, "{}: template", t.ticker);
    }
}

#[test]
fn registry_default_matches_the_file_and_example_validates() {
    let file = String::from_utf8(read("registry/tokens.json")).unwrap();
    assert_eq!(file.replace("\r\n", "\n"), kob_protocol::registry::DEFAULT_REGISTRY_JSON.replace("\r\n", "\n"));
    let ex = Registry::parse(&String::from_utf8(read("registry/tokens.example.json")).unwrap()).expect("example validates");
    ex.validate_for_network("testnet-10").unwrap();
    assert_eq!(ex.tokens.len(), 2);
    let fams: Vec<Family> = ex.tokens.iter().map(|t| t.family).collect();
    assert_eq!(fams, [Family::Kcc20, Family::Kron]);
    for t in &ex.tokens {
        // fictional covenant ids must not be mistaken for real ones
        assert!(t.covenant_id.starts_with("e5") || t.covenant_id.starts_with("e6"));
        println!("REGISTRY example {}", kob_protocol::registry::display_name(t));
    }
    // keep Escrow in scope for docs: an escrow block round-trips
    let e = Escrow { owner_scheme: Some(4), borrow_scheme: Some(0), id_type: None, is_minter: None, delivery_id_type: None };
    assert_eq!(serde_json::from_str::<Escrow>(&serde_json::to_string(&e).unwrap()).unwrap(), e);
}

/// Genesis contamination: a covenant id commits only to the P2SH hashes of its genesis outputs, so a genesis
/// can hide a non-template output (a "backdoor" script that later mints look-alike tokens or feeds a forged balance into a
/// genuine transfer). `verify_genesis`, the step `verified` requires, checks EVERY output of the group.
#[test]
fn genesis_check_refuses_contaminated_genesis() {
    use kaspa_consensus_core::hashing::covenant_id::covenant_id;
    use kaspa_consensus_core::tx::{TransactionId, TransactionOutpoint, TransactionOutput};
    use kaspa_txscript::pay_to_script_hash_script;
    use kob_protocol::registry::{verify_genesis, GenesisError, GenesisOutput};
    use kob_protocol::state::TokenState;

    let r = Registry::default_registry();
    let outpoint = ([0x99; 32], 0u32);
    let txid = "ab".repeat(32);
    let id_of = |outs: &[GenesisOutput]| -> String {
        let op = TransactionOutpoint { transaction_id: TransactionId::from_bytes(outpoint.0), index: outpoint.1 };
        let tx: Vec<TransactionOutput> = outs
            .iter()
            .map(|o| TransactionOutput { value: o.value, script_public_key: o.script_public_key.clone(), covenant: None })
            .collect();
        hexs(&covenant_id(op, outs.iter().zip(&tx).map(|(o, t)| (o.index, t))).as_bytes())
    };
    let output = |index: u32, redeem: Vec<u8>| GenesisOutput {
        index,
        value: 100_000_000,
        script_public_key: pay_to_script_hash_script(&redeem),
        redeem_script: Some(redeem),
    };
    for (id, states) in [
        (
            "kcc20-ref-8x8",
            vec![
                TokenState::user(Family::Kcc20, 600, [0x11; 32], [0; 32]).encode(),
                TokenState::user(Family::Kcc20, 400, [0x22; 32], [0; 32]).encode(),
            ],
        ),
        (
            "kron-2433",
            vec![
                TokenState::user(Family::Kron, 600, [0x11; 32], [0; 32]).encode(),
                TokenState::user(Family::Kron, 400, [0x22; 32], [0; 32]).encode(),
            ],
        ),
    ] {
        let tpl = r.template(id).unwrap();
        let prog = kob_protocol::artifacts::token_template_by_hash(&kob_protocol::registry::parse_hex32(&tpl.template_hash).unwrap())
            .unwrap();
        let clean: Vec<GenesisOutput> = states.iter().enumerate().map(|(i, s)| output(i as u32, prog.redeem(s))).collect();
        let rec = verify_genesis(tpl, &id_of(&clean), &txid, outpoint, &clean).expect("clean genesis");
        assert_eq!((rec.outputs, rec.supply, rec.txid.as_str()), (2, 1000, txid.as_str()), "{id}");

        // a hidden backdoor output (OP_TRUE behind P2SH) in the same group
        let mut dirty = clean.clone();
        dirty.push(output(2, vec![0x51]));
        let cid = id_of(&dirty);
        assert_eq!(verify_genesis(tpl, &cid, &txid, outpoint, &dirty), Err(GenesisError::NotTemplate(2, id.to_string())), "{id}");
        // a backdoor with the program's size and a template-looking state is not an instance either
        let mut forged = prog.redeem(&states[0]);
        let last = forged.len() - 1;
        forged[last] ^= 1;
        let mut dirty2 = clean.clone();
        dirty2.push(output(2, forged));
        let cid2 = id_of(&dirty2);
        assert!(matches!(verify_genesis(tpl, &cid2, &txid, outpoint, &dirty2), Err(GenesisError::NotTemplate(2, _))), "{id}");
        // leaving the backdoor out of the check does not help: the covenant id commits to the whole group
        assert_eq!(verify_genesis(tpl, &cid, &txid, outpoint, &clean), Err(GenesisError::NotTheGroup(2)), "{id}");
        // an output whose redeem script was never revealed cannot be checked
        let mut hidden = dirty.clone();
        hidden[2].redeem_script = None;
        assert_eq!(verify_genesis(tpl, &cid, &txid, outpoint, &hidden), Err(GenesisError::Unrevealed(2)), "{id}");
        // a redeem script that is not the output's preimage
        let mut wrong = clean.clone();
        wrong[1].redeem_script = Some(prog.redeem(&states[0]));
        assert_eq!(verify_genesis(tpl, &id_of(&clean), &txid, outpoint, &wrong), Err(GenesisError::WrongRedeem(1)), "{id}");
        // a program of the other family is not this token's program
        let other = r.template(if id == "kron-2433" { "kcc20-ref-8x8" } else { "kron-2433" }).unwrap();
        assert!(matches!(verify_genesis(other, &id_of(&clean), &txid, outpoint, &clean), Err(GenesisError::NotTemplate(0, _))));
    }
}
