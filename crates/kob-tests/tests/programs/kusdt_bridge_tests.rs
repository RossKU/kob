//! kUSDT on testnet-10 (`contracts/third-party/kusdt/`): a token of the published public-mint app's `KCC20` program whose genesis
//! holds a `BridgeMint`, the federated bridge's minter, which KOB pins by its template hash as the `bridge-minter` companion of
//! `KCC20PublicMint` (`crates/kob-protocol/data/programs.json`), and whose redemptions burn kUSDT to an `OP_RETURN` script hash.
//! Fixture: `contracts/third-party/kusdt/observed.json` (the genesis group, the revealed `BridgeMint`, a redemption), and under
//! `recovery_build` the deployment that superseded it (covenant `a1d9fd13...`: a newer `BridgeMint` with the signer-recovery path,
//! a 250-byte state span, a second pin).
//!
//! * each pin is its observed covenant's cut, and its context field names this program's `KCC20`;
//! * each genesis group verifies with its pin (the web app's open check: the unspent `TokenSeed` rebuilt from the creator's key),
//!   lists it as a mint authority without an allowance, and fails as contaminated without the pin;
//! * a `BridgeMint` that names another program is not recognised, and one whose mints would carry another extension
//!   commitment than the token's is refused;
//! * the observed redemption's output 0 is the burn state its payload names.

use kaspa_consensus_core::hashing::covenant_id::covenant_id;
use kaspa_consensus_core::tx::{ScriptPublicKey, TransactionId, TransactionOutpoint, TransactionOutput};

use kob_protocol::artifacts::{token_template, TemplateId};
use kob_protocol::genesis_open::{check_open_genesis, OpenGenesis, OpenOutcome, OpenOutput};
use kob_protocol::json::{from_hex, to_hex};
use kob_protocol::kusdt::{
    bridge_mint_extension, burn_owner, is_burned, redemption_entries, RedemptionEntry, BRIDGE_MINT_STATE_LEN_RECOVERY,
    BRIDGE_MINT_STATE_LEN_V1,
};
use kob_protocol::profile::{match_companion, CompanionMatch, CompanionRole};
use kob_protocol::public_mint::KCC20_SIL_HASH;
use kob_protocol::registry::{verify_genesis_with, GenesisError, GenesisOutput};
use kob_protocol::script::p2sh_spk;
use kob_protocol::state::{Kcc20State, TokenState};

/// A `BridgeMint` build KOB pins: where its deployment is in the fixture, its handle (prefix 34 B with the context push) and cut,
/// its Sil template hash (prefix 1 B).
struct Build {
    /// the fixture's key of the deployment, `None` for the top level (the first build)
    key: Option<&'static str>,
    hash: &'static str,
    sil_hash: &'static str,
    state_len: usize,
    suffix_len: usize,
}

/// The first build KOB observed (covenant `6711958e...`, superseded); its Sil hash as the issuer's published core reports it.
const V1: Build = Build {
    key: None,
    hash: "15e5532d4115550df60b7f9b1af2a0c996463cc4ba243bcdf9c478174fb6da40",
    sil_hash: "11ae39c524e45cfe477e53dd15715e35dd61a241c864a6537d1d4c509649cc25",
    state_len: BRIDGE_MINT_STATE_LEN_V1,
    suffix_len: 4_151,
};
/// The build with the signer-recovery path (covenant `a1d9fd13...`); its Sil hash as the issuer's artifact records it.
const RECOVERY: Build = Build {
    key: Some("recovery_build"),
    hash: "d8b48be4cf93156a40f0ac443da5eddf293e79b862a89c2e1c96b21d8c4a3446",
    sil_hash: "1bcabab63c4906ab3cb08a7233bf18cb813b248450fc400538c89d233f014964",
    state_len: BRIDGE_MINT_STATE_LEN_RECOVERY,
    suffix_len: 8_215,
};
const BUILDS: [&Build; 2] = [&V1, &RECOVERY];

fn observed() -> serde_json::Value {
    let p = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../contracts/third-party/kusdt/observed.json");
    serde_json::from_str(&std::fs::read_to_string(p).unwrap()).unwrap()
}

/// The fixture's record of a build's deployment (token, genesis, bridge_mint).
fn deployment(b: &Build) -> serde_json::Value {
    let o = observed();
    match b.key {
        Some(k) => o[k].clone(),
        None => o,
    }
}

fn hex(v: &serde_json::Value) -> Vec<u8> {
    from_hex(v.as_str().unwrap()).unwrap()
}

fn h32(v: &serde_json::Value) -> [u8; 32] {
    hex(v).try_into().unwrap()
}

fn bridge_mint(b: &Build) -> Vec<u8> {
    hex(&deployment(b)["bridge_mint"]["redeem_script"])
}

fn profile() -> &'static kob_protocol::profile::ProgramProfile {
    TemplateId::Kcc20PublicMint.profile().unwrap()
}

/// The token's genesis group as a data source gives it: output 0 with the `BridgeMint` its first spend revealed, output 1 (the
/// unspent `TokenSeed`) unrevealed; the keys the transaction pays to as owner hints.
fn open_genesis(b: &Build) -> OpenGenesis {
    let obs = deployment(b);
    let g = &obs["genesis"];
    let token = obs["token"]["covenant_id"].as_str().unwrap().to_string();
    let all = g["outputs"].as_array().unwrap();
    let outputs = all
        .iter()
        .filter(|o| o["covenant"]["covenant_id"].as_str() == Some(token.as_str()))
        .map(|o| OpenOutput {
            index: o["index"].as_u64().unwrap() as u32,
            value: o["value"].as_u64().unwrap(),
            spk_version: 0,
            spk: hex(&o["script_public_key"]),
            redeem: (o["index"] == 0).then(|| bridge_mint(b)),
        })
        .collect();
    let owner_hints = all
        .iter()
        .map(|o| hex(&o["script_public_key"]))
        .filter(|s| s.len() == 34 && s[0] == 0x20 && s[33] == 0xac)
        .map(|s| s[1..33].try_into().unwrap())
        .collect();
    OpenGenesis {
        covenant_id: token,
        genesis_txid: g["txid"].as_str().unwrap().into(),
        authorizing_outpoint: (h32(&g["authorizing_outpoint"]["txid"]), g["authorizing_outpoint"]["index"].as_u64().unwrap() as u32),
        outputs,
        program_hint: None,
        owner_hints,
    }
}

#[test]
fn each_bridge_mint_pin_is_its_observed_covenant() {
    let pins: Vec<_> = profile().genesis.companions.iter().filter(|c| c.role == CompanionRole::BridgeMinter).collect();
    assert_eq!(pins.len(), BUILDS.len(), "one pin per build");
    for b in BUILDS {
        let rule = pins.iter().find(|c| c.template_hash.as_deref() == Some(b.hash)).expect("the pin");
        let l = rule.layout.unwrap();
        assert_eq!((l.prefix_len, l.state_len, l.suffix_len), (34, b.state_len, b.suffix_len));
        let r = bridge_mint(b);
        assert_eq!(r.len(), 34 + b.state_len + b.suffix_len);
        // the redeem script of genesis output 0, as its first spend revealed it
        assert_eq!(p2sh_spk(&r).script(), hex(&deployment(b)["genesis"]["outputs"][0]["script_public_key"]).as_slice());
        assert_eq!(to_hex(&silverscript_abi::template_hash(&r[..34], &r[34 + b.state_len..])), b.hash);
        assert_eq!(to_hex(&silverscript_abi::template_hash(&r[..1], &r[34 + b.state_len..])), b.sil_hash);
        // the context field the prefix pins is the published app's KCC20: what the pinned bytes mint is this program
        assert_eq!(r[1], 0x20);
        assert_eq!(to_hex(&r[2..34]), KCC20_SIL_HASH);
        assert!(rule.matches_opaque(&r));
        // the other build's pin does not match it
        assert_eq!(pins.iter().filter(|c| c.matches_opaque(&r)).count(), 1);
        assert!(matches!(match_companion(profile(), &r), Ok(Some(CompanionMatch::Opaque(CompanionRole::BridgeMinter)))));
        // what it mints carries the zero extension commitment
        assert_eq!(bridge_mint_extension(rule.opaque_state(&r).unwrap()), Some([0; 32]));
        // no other program declares it
        for id in TemplateId::ALL.into_iter().filter(|t| t.is_token() && *t != TemplateId::Kcc20PublicMint) {
            assert!(matches!(match_companion(id.profile().unwrap(), &r), Ok(None)), "{}", id.name());
        }
    }
}

#[test]
fn the_recovery_build_state_is_the_issuers_initial_minter() {
    // the issuer's deployments/testnet.json launch.initialMinter.state, read from the revealed bytes: the extension commitment
    // (bytes 131..164 of the span) is zero and is not the last field; the recovery fields follow it
    let r = bridge_mint(&RECOVERY);
    let s = &r[34..34 + BRIDGE_MINT_STATE_LEN_RECOVERY];
    let int = |at: usize| {
        assert_eq!(s[at], 0x08);
        i64::from_le_bytes(s[at + 1..at + 9].try_into().unwrap())
    };
    assert_eq!((int(0), int(9), int(18), int(27)), (1_000_000_000_000_000, 500_000_000_000, 500_000_000_000, 578_703));
    assert_eq!((int(36), int(45), int(54)), (12, 14, 0));
    assert_eq!(&s[63..65], [0x01, 0x00], "not paused");
    assert_eq!(to_hex(&s[66..98]), "b563c26ffc9f94f977e9d580f02a0b8501c023a8455f2f35b74e79f8df3c0301");
    assert_eq!(to_hex(&s[99..131]), "3c4c0ef3d985fb93ebdb30c5dd8ff8d6ffffa1e411565a21cc33aa2365a89566");
    assert_eq!((s[131], &s[132..164]), (0x20, [0u8; 32].as_slice()));
    assert_eq!((int(164), int(173)), (36_000, 0), "recovery delay, recovery nonce");
    assert_eq!(&s[182..184], [0x01, 0x00], "no recovery pending");
    assert_eq!(bridge_mint_extension(s), Some([0; 32]));
}

#[test]
fn each_kusdt_genesis_verifies_with_its_bridge_mint() {
    for b in BUILDS {
        genesis_verifies(b);
    }
}

fn genesis_verifies(b: &Build) {
    let g = open_genesis(b);
    let OpenOutcome::Verified(v) = check_open_genesis(&g) else { panic!("{:?}", check_open_genesis(&g)) };
    assert_eq!(v.program, "KCC20PublicMint");
    let rep = &v.report;
    assert_eq!(rep.outputs, 2);
    // nothing minted at creation; the BridgeMint is a mint authority without an allowance KOB reads, the TokenSeed a seed
    assert_eq!((rep.supply, rep.mint_allowance), (0, 0));
    assert_eq!((rep.minter_outputs.clone(), rep.seed_outputs.clone()), (vec![0], vec![1]));
    assert_eq!(rep.companion_outputs.iter().map(|c| (c.index, c.role.as_str())).collect::<Vec<_>>(), [(0, "bridge-minter")]);
    assert!(rep.is_bridge_minter(0) && !rep.is_bridge_minter(1));
    assert_eq!(rep.issuer.as_deref(), Some("kcc20-reference@27f8403"));
    assert_eq!(v.rebuilt, vec![1], "the unspent TokenSeed, rebuilt from the creator's key");
}

#[test]
fn without_its_pin_each_genesis_is_contaminated() {
    for b in BUILDS {
        contaminated_without(b);
    }
}

fn contaminated_without(b: &Build) {
    let mut g = open_genesis(b);
    kob_protocol::genesis_open::rebuild_seeds(&mut g.outputs, &g.owner_hints);
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
    let verify = |p: &kob_protocol::profile::ProgramProfile| {
        verify_genesis_with(&tpl, p, &g.covenant_id, &g.genesis_txid, g.authorizing_outpoint, &group, None)
    };
    assert!(verify(profile()).is_ok());
    let mut bare = profile().clone();
    bare.genesis.companions.retain(|c| c.role != CompanionRole::BridgeMinter);
    assert!(matches!(verify(&bare), Err(GenesisError::NotTemplate(0, _))), "{:?}", verify(&bare));
    // the other build's pin alone does not verify it either
    let mut other = profile().clone();
    other.genesis.companions.retain(|c| c.role != CompanionRole::BridgeMinter || c.template_hash.as_deref() != Some(b.hash));
    assert!(matches!(verify(&other), Err(GenesisError::NotTemplate(0, _))), "{:?}", verify(&other));
}

/// A `BridgeMint` of the pinned bytes of a build with its state's extension commitment set to `ext`.
fn bridge_mint_with(b: &Build, ext: [u8; 32]) -> Vec<u8> {
    let mut r = bridge_mint(b);
    // the last field of the first build's span; bytes 132..164 of the recovery build's
    let end = 34 + if b.state_len == BRIDGE_MINT_STATE_LEN_V1 { BRIDGE_MINT_STATE_LEN_V1 } else { 164 };
    r[end - 32..end].copy_from_slice(&ext);
    r
}

#[test]
fn a_bridge_mint_of_another_program_or_class_is_refused() {
    for b in BUILDS {
        refused_of_another_program_or_class(b);
    }
}

fn refused_of_another_program_or_class(b: &Build) {
    // a context field naming another program: not the pinned covenant
    let mut other = bridge_mint(b);
    other[2] ^= 1;
    assert!(matches!(match_companion(profile(), &other), Ok(None)));
    // a synthetic genesis: a BridgeMint and a holder of this program, of one class or of two
    let holder = TokenState::Kcc20(Kcc20State::p2pk(5, [9; 32], [0; 32]));
    let tpl_program = token_template(TemplateId::Kcc20PublicMint);
    let genesis = |minter: Vec<u8>| {
        let redeems = [minter, tpl_program.redeem(&holder.encode())];
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
    };
    let ok = genesis(bridge_mint_with(b, [0; 32])).expect("one class");
    assert_eq!((ok.supply, ok.minter_outputs.clone(), ok.mint_allowance), (5, vec![0], 0));
    assert!(matches!(genesis(bridge_mint_with(b, [7; 32])), Err(GenesisError::ExtensionCommitment(1, _, _))));
}

#[test]
fn the_observed_redemption_burns_its_output_0() {
    let obs = observed();
    let r = &obs["redemption"];
    let entries = redemption_entries(&hex(&r["payload"])).expect("a redemption payload");
    let address: [u8; 20] = hex(&r["burn"]["address"]).try_into().unwrap();
    assert_eq!(entries, vec![RedemptionEntry { output: 0, chain: 2, address }]);
    // output 0 is the program's P2SH of the burn state the payload names
    let burned = TokenState::Kcc20(Kcc20State {
        owner_scheme: kob_protocol::kcc2::P2SH,
        ..Kcc20State::p2pk(r["burn"]["amount"].as_i64().unwrap(), burn_owner(2, &address), [0; 32])
    });
    let spk = token_template(TemplateId::Kcc20PublicMint).spk(&burned.encode());
    assert_eq!(spk.script(), hex(&r["outputs"][0]["script_public_key"]).as_slice());
    assert!(is_burned(&entries, 0, &burned));
    assert!(!is_burned(&entries, 1, &burned));
}
