//! Kozmic on Kaspa mainnet (`contracts/third-party/kozmic/`): tokens of the published public-mint app's `KCC20` program whose
//! genesis holds one `BridgeMint`, Kaspero Labs' ZK bridge minter, which KOB pins by its template hash as the `zk-bridge-minter`
//! companion of `KCC20PublicMint` (`crates/kob-protocol/data/programs.json`). Fixture: `contracts/third-party/kozmic/observed.json`
//! (kzUSDC `cc9957fe...`, the older kzUSDC instance `9753d77b...` and the Sepolia rehearsal Sepia.k `13af6019...`, all three on
//! mainnet; a kzUSDC cell at the burn owner).
//!
//! * the pin is the observed `BridgeMint`'s cut, its context field names this program's `KCC20`, and its state names the bridge;
//! * each genesis recomputes to its covenant id and verifies with the pin (the open check of the apps and the registry's check),
//!   lists the minter as a mint authority without an allowance and with its bridge, and fails as contaminated without the pin;
//! * the pin recognises the build, not the bridge: every instance verifies, whatever bridge it names (the apps say so);
//! * a changed `BridgeMint`, one of another program, or one whose mints would carry another class than the token's is refused;
//! * the cell at `Blake2b(6a)` is burned.

use kaspa_consensus_core::hashing::covenant_id::covenant_id;
use kaspa_consensus_core::tx::{ScriptPublicKey, TransactionId, TransactionOutpoint, TransactionOutput};

use kob_protocol::artifacts::{token_template, TemplateId};
use kob_protocol::genesis_open::{check_open_genesis, OpenGenesis, OpenOutcome, OpenOutput};
use kob_protocol::json::{from_hex, to_hex};
use kob_protocol::kozmic::{bridge_mint_state, burn_owner, is_burned, BRIDGE_MINT_STATE_LEN};
use kob_protocol::profile::{match_companion, CompanionMatch, CompanionRole};
use kob_protocol::public_mint::KCC20_SIL_HASH;
use kob_protocol::registry::{verify_genesis_with, BridgeFacts, GenesisError, GenesisOutput, GenesisReport};
use kob_protocol::script::p2sh_spk;
use kob_protocol::state::{Kcc20State, TokenState};

/// The pin: the template hash of the KCC-1 cut (prefix 34 B with the `gen__kcc20_template` push, state 99 B, suffix 1,340 B).
const PIN: &str = "92c69c7f4f652fed2e66f2b4ac402b9725df21ecc268c1bedd8d69950c0f28e0";
/// The Sil template hash (prefix 1 B) the issuer publishes for the build (`minter_template` of the KCC-23 declarations).
const SIL: &str = "dc96afcde4874e0750e85ecffe91a845365ff16d952a4c1a92f49f7a0da70102";
const SUFFIX_LEN: usize = 1_340;
const KEYS: [&str; 3] = ["kzusdc", "kzusdc_instance_1", "sepia"];

fn observed() -> serde_json::Value {
    let p = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../contracts/third-party/kozmic/observed.json");
    serde_json::from_str(&std::fs::read_to_string(p).unwrap()).unwrap()
}

fn deployment(key: &str) -> serde_json::Value {
    observed()["deployments"].as_array().unwrap().iter().find(|d| d["key"] == key).unwrap().clone()
}

fn hex(v: &serde_json::Value) -> Vec<u8> {
    from_hex(v.as_str().unwrap()).unwrap()
}

fn h32(v: &serde_json::Value) -> [u8; 32] {
    hex(v).try_into().unwrap()
}

fn profile() -> &'static kob_protocol::profile::ProgramProfile {
    TemplateId::Kcc20PublicMint.profile().unwrap()
}

/// The deployment's `BridgeMint`: as its first spend revealed it, or (Sepia.k, whose script the source left out) kzUSDC's bytes
/// with Sepia.k's declared bridge covenant id in the state, which the output's hash then proves.
fn bridge_mint(key: &str) -> Vec<u8> {
    let d = deployment(key);
    if d["bridge_mint"]["redeem_script"].is_string() {
        return hex(&d["bridge_mint"]["redeem_script"]);
    }
    let mut r = bridge_mint("kzusdc");
    r[34 + 34..34 + 66].copy_from_slice(&h32(&d["token"]["declared_bridge_covenant_id"]));
    r
}

fn genesis_spk(key: &str) -> Vec<u8> {
    hex(&deployment(key)["genesis"]["group"][0]["script_public_key"])
}

/// The token's genesis group as a data source gives it: the one output, with the `BridgeMint`.
fn open_genesis(key: &str) -> OpenGenesis {
    let d = deployment(key);
    let g = &d["genesis"];
    OpenGenesis {
        covenant_id: d["token"]["covenant_id"].as_str().unwrap().into(),
        genesis_txid: g["txid"].as_str().unwrap().into(),
        authorizing_outpoint: (h32(&g["authorizing_outpoint"]["txid"]), g["authorizing_outpoint"]["index"].as_u64().unwrap() as u32),
        outputs: g["group"]
            .as_array()
            .unwrap()
            .iter()
            .map(|o| OpenOutput {
                index: o["index"].as_u64().unwrap() as u32,
                value: o["value"].as_u64().unwrap(),
                spk_version: 0,
                spk: hex(&o["script_public_key"]),
                redeem: Some(bridge_mint(key)),
            })
            .collect(),
        program_hint: None,
        owner_hints: vec![],
    }
}

/// The registry's genesis check of the group (`kob registry verify-genesis`) under `p`.
fn verify(key: &str, p: &kob_protocol::profile::ProgramProfile) -> Result<GenesisReport, GenesisError> {
    let g = open_genesis(key);
    let group: Vec<GenesisOutput> = g
        .outputs
        .iter()
        .map(|o| GenesisOutput {
            index: o.index,
            value: o.value,
            script_public_key: ScriptPublicKey::new(0, o.spk.clone().into()),
            redeem_script: o.redeem.clone(),
        })
        .collect();
    let tpl = kob_protocol::profile::registry_template(profile(), None);
    verify_genesis_with(&tpl, p, &g.covenant_id, &g.genesis_txid, g.authorizing_outpoint, &group, None)
}

#[test]
fn the_pin_is_the_observed_bridge_mint() {
    let pins: Vec<_> = profile().genesis.companions.iter().filter(|c| c.role == CompanionRole::ZkBridgeMinter).collect();
    assert_eq!(pins.len(), 1);
    let rule = pins[0];
    assert_eq!(rule.template_hash.as_deref(), Some(PIN));
    let l = rule.layout.unwrap();
    assert_eq!((l.prefix_len, l.state_len, l.suffix_len), (34, BRIDGE_MINT_STATE_LEN, SUFFIX_LEN));
    for key in KEYS {
        let r = bridge_mint(key);
        assert_eq!(r.len(), 34 + BRIDGE_MINT_STATE_LEN + SUFFIX_LEN);
        // the redeem script of the genesis output (revealed, or rebuilt and proven by the same hash)
        assert_eq!(p2sh_spk(&r).script(), genesis_spk(key).as_slice(), "{key}");
        assert_eq!(to_hex(&silverscript_abi::template_hash(&r[..34], &r[34 + BRIDGE_MINT_STATE_LEN..])), PIN);
        assert_eq!(to_hex(&silverscript_abi::template_hash(&r[..1], &r[34 + BRIDGE_MINT_STATE_LEN..])), SIL);
        // the context field the prefix pins is the published app's KCC20: what the pinned bytes mint is this program
        assert_eq!((r[0], r[1]), (0x6b, 0x20));
        assert_eq!(to_hex(&r[2..34]), KCC20_SIL_HASH);
        assert!(matches!(match_companion(profile(), &r), Ok(Some(CompanionMatch::Opaque(CompanionRole::ZkBridgeMinter)))));
        // the state: its own Sil template, the bridge the issuer declares, and the zero class
        let s = bridge_mint_state(rule.opaque_state(&r).unwrap()).unwrap();
        assert_eq!(to_hex(&s.template), SIL);
        assert_eq!(s.bridge_covenant_id, h32(&deployment(key)["token"]["declared_bridge_covenant_id"]));
        assert_eq!(s.extension_commitment, [0; 32]);
        // kUSDT's layouts do not read it, and no other program declares it
        assert_eq!(kob_protocol::kusdt::bridge_mint_extension(rule.opaque_state(&r).unwrap()), None);
        for id in TemplateId::ALL.into_iter().filter(|t| t.is_token() && *t != TemplateId::Kcc20PublicMint) {
            assert!(matches!(match_companion(id.profile().unwrap(), &r), Ok(None)), "{}", id.name());
        }
    }
    // the builds are one: the instances differ in the bridge covenant id only
    let (a, b) = (bridge_mint("kzusdc"), bridge_mint("kzusdc_instance_1"));
    assert!(a.iter().zip(&b).enumerate().all(|(i, (x, y))| x == y || (68..100).contains(&i)));
}

#[test]
fn each_genesis_verifies_with_the_pin() {
    for key in KEYS {
        let d = deployment(key);
        let OpenOutcome::Verified(v) = check_open_genesis(&open_genesis(key)) else {
            panic!("{key}: {:?}", check_open_genesis(&open_genesis(key)))
        };
        assert_eq!(v.program, "KCC20PublicMint", "{key}");
        let rep = &v.report;
        assert_eq!(rep.txid, d["genesis"]["txid"].as_str().unwrap());
        // nothing minted at creation; the BridgeMint is a mint authority without an allowance, the only output
        assert_eq!((rep.outputs, rep.supply, rep.mint_allowance), (1, 0, 0));
        assert_eq!((rep.minter_outputs.clone(), rep.seed_outputs.clone(), rep.issuer.clone()), (vec![1], vec![], None));
        let c = &rep.companion_outputs;
        assert_eq!(c.iter().map(|c| (c.index, c.role.as_str())).collect::<Vec<_>>(), [(1, "zk-bridge-minter")]);
        let bridge = d["token"]["declared_bridge_covenant_id"].as_str().unwrap().to_string();
        assert_eq!(c[0].bridge, Some(BridgeFacts { bridge_covenant_id: Some(bridge), ..Default::default() }));
        assert!(rep.is_bridge_minter(1));
        assert!(v.rebuilt.is_empty());
        // the registry's check (the command line) gives the same report
        assert_eq!(verify(key, profile()).as_ref(), Ok(rep), "{key}");
    }
}

#[test]
fn without_the_pin_each_genesis_is_contaminated() {
    // the profile as it was before the pin (kUSDT's two pins only)
    let mut before = profile().clone();
    before.genesis.companions.retain(|c| c.role != CompanionRole::ZkBridgeMinter);
    for key in KEYS {
        assert!(verify(key, profile()).is_ok(), "{key}");
        assert!(matches!(verify(key, &before), Err(GenesisError::NotTemplate(1, _))), "{key}: {:?}", verify(key, &before));
    }
}

/// A synthetic genesis of `minter` and one holder of this program of class `class` (3 tokens); the registry's check.
fn synthetic(minter: Vec<u8>, class: [u8; 32]) -> Result<GenesisReport, GenesisError> {
    let holder = TokenState::Kcc20(Kcc20State::p2pk(3, [9; 32], class));
    let redeems = [minter, token_template(TemplateId::Kcc20PublicMint).redeem(&holder.encode())];
    let outs: Vec<TransactionOutput> =
        redeems.iter().map(|r| TransactionOutput { value: 100_000_000, script_public_key: p2sh_spk(r), covenant: None }).collect();
    let outpoint = TransactionOutpoint { transaction_id: TransactionId::from_bytes([3; 32]), index: 0 };
    let id = covenant_id(outpoint, outs.iter().enumerate().map(|(i, o)| (i as u32, o)));
    let group: Vec<GenesisOutput> = outs
        .iter()
        .zip(redeems)
        .enumerate()
        .map(|(i, (o, r))| GenesisOutput {
            index: i as u32,
            value: o.value,
            script_public_key: o.script_public_key.clone(),
            redeem_script: Some(r),
        })
        .collect();
    let tpl = kob_protocol::profile::registry_template(profile(), None);
    verify_genesis_with(&tpl, profile(), &to_hex(id.as_bytes().as_slice()), &"ab".repeat(32), ([3; 32], 0), &group, None)
}

#[test]
fn the_pin_recognises_the_build_not_the_bridge() {
    // any bridge covenant id in the state: the same build, so it verifies and names that bridge (what the apps show, unchecked)
    let mut r = bridge_mint("kzusdc");
    r[68..100].copy_from_slice(&[0x42; 32]);
    let ok = synthetic(r, [0; 32]).expect("the same build");
    assert_eq!((ok.supply, ok.minter_outputs.clone(), ok.mint_allowance), (3, vec![0], 0));
    assert_eq!(ok.companion_outputs[0].bridge.as_ref().unwrap().bridge_covenant_id.as_deref(), Some("42".repeat(32).as_str()));
}

#[test]
fn a_changed_bridge_mint_or_another_class_is_refused() {
    let r = bridge_mint("kzusdc");
    assert!(synthetic(r.clone(), [0; 32]).is_ok());
    // a byte of the program changed (the suffix, the prefix's context field naming another program, a push of the state): not the
    // pinned build, so the genesis is contaminated
    for at in [0, 2, 33, 34, 67, 133, 700, r.len() - 1] {
        let mut t = r.clone();
        t[at] ^= 1;
        assert!(matches!(match_companion(profile(), &t), Ok(None) | Err(_)) || at == 34 || at == 67, "{at}");
        assert!(synthetic(t, [0; 32]).is_err(), "{at}");
    }
    // the state's push opcodes are in the span: the pin still matches, but the state is no BridgeMint state
    let mut t = r.clone();
    t[34] = 0x21;
    assert!(matches!(synthetic(t, [0; 32]), Err(GenesisError::BadState(0, _))));
    // its mints would carry another class than the token's
    let mut t = r.clone();
    t[101..133].copy_from_slice(&[7; 32]);
    assert!(matches!(synthetic(t.clone(), [0; 32]), Err(GenesisError::ExtensionCommitment(1, _, _))));
    assert!(synthetic(t, [7; 32]).is_ok());
}

#[test]
fn the_cell_at_the_burn_owner_is_burned() {
    let b = &observed()["burn"];
    let tpl = token_template(TemplateId::Kcc20PublicMint);
    let state = TokenState::decode_with(tpl, &hex(&b["state"])).unwrap();
    // the state the indexer read is the one the output commits to
    assert_eq!(tpl.spk(&state.encode()).script(), hex(&b["script_public_key"]).as_slice());
    let k = state.as_kcc20().unwrap();
    assert_eq!((k.amount, k.owner_scheme, k.owner), (10_000_000, kob_protocol::kcc2::P2SH, burn_owner()));
    assert!(is_burned(&state));
}
