//! Genesis check of a token that no registry lists (the web app's "Genesis verified" fact): the same rules as
//! [`crate::registry::verify_genesis_with`], fed with the genesis group a data source supplied.
//!
//! The registry path ([`crate::genesis_evidence`]) starts from a registry entry that names the token's program and
//! extension commitment. A token outside the registry has neither, so this module finds the program from the bytes
//! themselves and lets the genesis group fix the extension class (every output must carry the first output's):
//!
//! * The caller gives the covenant id, the genesis transaction id, the authorising outpoint and the complete group
//!   (index, value, script public key and, where known, the redeem script of every output bound to the covenant id). The
//!   KIP-20 covenant id commits to the outpoint and to the index, value and script public key of every group output, so a
//!   source that leaves an output out, adds one or changes a byte can only make the check fail; it cannot make a wrong group
//!   pass. A redeem script is checked against its output's P2SH hash. The source is a supplier of bytes, never an authority.
//! * Every pinned token program is tried as the group's program; the outputs themselves decide which one it is (an output
//!   that is an instance of a program's holder state names the program, a genesis of the published public-mint app that
//!   holds only its issuance actors is named by them). When the data does not single a program out, the caller's hint (a
//!   template hash from a third party) only picks among programs that verify.
//! * An output whose redeem script no spend revealed is rebuilt when it can be: a `TokenSeed` of the published public-mint
//!   app is fully determined by an owner key and an extension commitment that the group's other cells carry
//!   ([`rebuild_seeds`]). The rebuild is a candidate for the hash check, never a state taken on trust: a candidate that does
//!   not hash to the output's script public key proves nothing and is dropped.
//!
//! The outcome has three kinds. `Verified`: every output checked and clean (the report says what the genesis created and the
//! allowance of any public minter). `Failed`: the data is internally consistent and the group is NOT clean, or the data does
//! not match the covenant id or a hash (a wrong or tampered source). `Unavailable`: the check could not be done (a redeem
//! script nobody revealed, a program this build does not pin, malformed input): never a verdict on the token.

use serde::Serialize;

use kaspa_consensus_core::tx::ScriptPublicKey;

use crate::artifacts::{try_token_template, TemplateId};
use crate::public_mint::{self, Companion, IssuerVersion, SeedState};
use crate::registry::{parse_hex32, verify_genesis_with, GenesisError, GenesisOutput, GenesisReport};

/// One output of the genesis group as a data source gave it.
#[derive(Clone, Debug)]
pub struct OpenOutput {
    /// Output index in the genesis transaction.
    pub index: u32,
    /// Value (sompi).
    pub value: u64,
    /// Script public key version (0 on every network so far).
    pub spk_version: u16,
    /// Script public key bytes.
    pub spk: Vec<u8>,
    /// Redeem script behind the P2SH script public key, when known.
    pub redeem: Option<Vec<u8>>,
}

/// The input of [`check_open_genesis`].
#[derive(Clone, Debug)]
pub struct OpenGenesis {
    /// The token's covenant id, 64 lowercase hex characters.
    pub covenant_id: String,
    /// The genesis transaction id, 64 lowercase hex characters (identity only: the covenant id is what authenticates).
    pub genesis_txid: String,
    /// Previous outpoint of the input that authorised the group.
    pub authorizing_outpoint: ([u8; 32], u32),
    /// The complete genesis group, by strictly increasing index.
    pub outputs: Vec<OpenOutput>,
    /// A template hash a third party named for the token's program (only breaks a tie between programs that verify).
    pub program_hint: Option<[u8; 32]>,
    /// Keys the genesis transaction itself pays to (the x-only keys of its P2PK outputs: the creator's change): further
    /// owners a `TokenSeed` may carry ([`rebuild_seeds`]). Candidates only: a rebuilt script must hash to its output.
    pub owner_hints: Vec<[u8; 32]>,
}

/// Why a check failed (the data is wrong, or the genesis is not clean).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum FailCode {
    /// The group does not recompute to the covenant id, or a redeem script does not hash to its output: the supplied data is
    /// not the chain's (or is incomplete).
    Mismatch,
    /// The covenant id recomputed and the redeem scripts match their hashes, but an output is not an instance of the
    /// program (or the group holds a second extension class, or an impossible state): the genesis is not clean.
    Contaminated,
}

/// Why a check could not be done.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum UnavailableCode {
    /// A genesis output's redeem script is not known (no spend revealed it and it cannot be rebuilt).
    Unrevealed,
    /// The group fits none of the token programs this build pins.
    UnsupportedProgram,
    /// The input is malformed (hex, order, empty group).
    Malformed,
}

/// What a clean check found.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OpenVerified {
    /// The token program the group is an instance of (`TemplateId` name).
    pub program: String,
    /// Its template hash, lowercase hex.
    pub program_hash: String,
    /// The report of the genesis check.
    pub report: GenesisReport,
    /// Outputs whose redeem script was rebuilt (a `TokenSeed`), by index.
    pub rebuilt: Vec<u32>,
}

/// The outcome of [`check_open_genesis`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum OpenOutcome {
    /// Every output checked and clean.
    Verified(OpenVerified),
    /// The data does not match the chain, or the genesis is not clean.
    Failed { code: FailCode, message: String },
    /// The check could not be done.
    Unavailable { code: UnavailableCode, message: String },
}

/// The owners and extension commitments a `TokenSeed` of the group may carry: those of every cell whose redeem script is known
/// (the app's `PublicMint` and `TokenSeed` actors and the holders of any pinned KCC-20 program), the `hints` (keys the genesis
/// transaction pays to) among the owners, and the zero class among the commitments (the class of a plain fixed-supply token).
fn seed_candidates(outputs: &[OpenOutput], hints: &[[u8; 32]]) -> (Vec<[u8; 32]>, Vec<[u8; 32]>) {
    let (mut owners, mut classes): (Vec<[u8; 32]>, Vec<[u8; 32]>) = (vec![], vec![[0; 32]]);
    let mut add = |o: [u8; 32], c: [u8; 32]| {
        if !owners.contains(&o) {
            owners.push(o);
        }
        if !classes.contains(&c) {
            classes.push(c);
        }
    };
    for r in outputs.iter().filter_map(|o| o.redeem.as_deref()) {
        if let Ok(Some(c)) = public_mint::decode_companion(r) {
            match c {
                Companion::Minter(m) => add(m.owner, m.extension_commitment),
                Companion::Seed(s) => add(s.owner, s.extension_commitment),
            }
            continue;
        }
        for id in TemplateId::ALL.into_iter().filter(|i| i.is_token()) {
            let Some(tpl) = try_token_template(id) else { continue };
            if let Some(state) = tpl.state_of(r) {
                if let Ok(st) = crate::state::TokenState::decode_with(tpl, state) {
                    if st.as_kcc20().is_some() {
                        add(st.owner(), st.extension());
                    }
                }
            }
        }
    }
    for h in hints {
        if !owners.contains(h) {
            owners.push(*h);
        }
    }
    (owners, classes)
}

/// Fills in the redeem script of every group output that no spend revealed and that is a `TokenSeed` of the published
/// public-mint app (either pinned build) in the owner and class of one of the group's known cells; returns the indices it
/// rebuilt. A rebuilt script is kept only when it hashes to the output's script public key.
pub fn rebuild_seeds(outputs: &mut [OpenOutput], owner_hints: &[[u8; 32]]) -> Vec<u32> {
    let (owners, classes) = seed_candidates(outputs, owner_hints);
    let mut rebuilt = vec![];
    for o in outputs.iter_mut().filter(|o| o.redeem.is_none()) {
        let spk = ScriptPublicKey::new(o.spk_version, o.spk.clone().into());
        'found: for owner in &owners {
            for ext in &classes {
                for v in IssuerVersion::ALL {
                    let redeem = public_mint::seed_redeem_of(v, &SeedState { owner: *owner, extension_commitment: *ext });
                    if crate::script::p2sh_spk(&redeem) == spk {
                        o.redeem = Some(redeem);
                        rebuilt.push(o.index);
                        break 'found;
                    }
                }
            }
        }
    }
    rebuilt
}

/// Maps a genesis error to the outcome kind (see the module docs).
fn classify(e: &GenesisError) -> Result<FailCode, UnavailableCode> {
    match e {
        GenesisError::NotTheGroup(_) | GenesisError::WrongRedeem(_) => Ok(FailCode::Mismatch),
        GenesisError::NotTemplate(..) | GenesisError::BadState(..) | GenesisError::ExtensionCommitment(..) => {
            Ok(FailCode::Contaminated)
        }
        GenesisError::Unrevealed(_) => Err(UnavailableCode::Unrevealed),
        GenesisError::Malformed(_) | GenesisError::UnknownProgram(_) => Err(UnavailableCode::Malformed),
    }
}

/// Whether the group's revealed outputs name `id` as their program: some output is an instance of its holder state, or one of
/// the companions its profile declares (the issuance actors of the published app, a launchpad's curve).
fn names_program(id: TemplateId, outputs: &[OpenOutput]) -> bool {
    let (Some(tpl), Some(profile)) = (try_token_template(id), id.profile()) else { return false };
    outputs
        .iter()
        .filter_map(|o| o.redeem.as_deref())
        .any(|r| tpl.state_of(r).is_some() || matches!(crate::profile::match_companion(profile, r), Ok(Some(_))))
}

/// Checks a token's genesis group against the pinned token programs (see the module docs). Pure and deterministic.
pub fn check_open_genesis(input: &OpenGenesis) -> OpenOutcome {
    let unavailable = |code, message: &str| OpenOutcome::Unavailable { code, message: message.to_string() };
    if parse_hex32(&input.covenant_id).is_none() || parse_hex32(&input.genesis_txid).is_none() {
        return unavailable(UnavailableCode::Malformed, "the covenant id and the genesis txid must be 64 lowercase hex characters");
    }
    if input.outputs.is_empty() || !input.outputs.windows(2).all(|w| w[0].index < w[1].index) {
        return unavailable(
            UnavailableCode::Malformed,
            "the genesis group must be a non-empty list of outputs in increasing index order",
        );
    }
    let mut outputs = input.outputs.clone();
    let rebuilt = rebuild_seeds(&mut outputs, &input.owner_hints);
    let group: Vec<GenesisOutput> = outputs
        .iter()
        .map(|o| GenesisOutput {
            index: o.index,
            value: o.value,
            script_public_key: ScriptPublicKey::new(o.spk_version, o.spk.clone().into()),
            redeem_script: o.redeem.clone(),
        })
        .collect();

    let programs: Vec<TemplateId> = TemplateId::ALL.into_iter().filter(|i| i.is_token()).collect();
    let named: Vec<TemplateId> = programs.iter().copied().filter(|id| names_program(*id, &outputs)).collect();
    // the programs to try, best first: those the outputs name; else all (a hint breaks ties among the passing)
    let order: Vec<TemplateId> = if named.is_empty() { programs.clone() } else { named.clone() };
    let mut passing: Vec<OpenVerified> = vec![];
    let mut errors: Vec<(TemplateId, GenesisError)> = vec![];
    for id in order {
        let profile = id.profile().expect("a token program has a profile");
        let tpl = crate::profile::registry_template(profile, None);
        match verify_genesis_with(&tpl, profile, &input.covenant_id, &input.genesis_txid, input.authorizing_outpoint, &group, None) {
            Ok(report) => passing.push(OpenVerified {
                program: id.name().to_string(),
                program_hash: profile.template_hash.clone(),
                report,
                rebuilt: rebuilt.clone(),
            }),
            Err(e) => errors.push((id, e)),
        }
    }
    if !passing.is_empty() {
        let pick = input.program_hint.and_then(|h| passing.iter().position(|p| parse_hex32(&p.program_hash) == Some(h))).unwrap_or(0);
        return OpenOutcome::Verified(passing.swap_remove(pick));
    }
    // nothing passes. The group's own bytes decide whose error is the verdict:
    //  * the covenant id does not recompute (the same under every program): the data is wrong;
    if let Some((_, e)) = errors.iter().find(|(_, e)| matches!(e, GenesisError::NotTheGroup(_))) {
        return OpenOutcome::Failed { code: FailCode::Mismatch, message: e.to_string() };
    }
    //  * a program the outputs name: its error (a redeem script that does not hash, a contaminated output, an unrevealed one);
    let pick = errors.iter().find(|(id, _)| named.contains(id)).or_else(|| {
        //  * no output names a program: only the issuance actors of the published app can single one out; the hint, else
        //    any error that is not "this output is not an instance" (that one is just "wrong program")
        input
            .program_hint
            .and_then(|h| errors.iter().find(|(id, _)| id.profile().is_some_and(|p| p.hash() == h)))
            .or_else(|| errors.iter().find(|(_, e)| !matches!(e, GenesisError::NotTemplate(..) | GenesisError::UnknownProgram(_))))
    });
    match pick {
        Some((_, e)) => match classify(e) {
            Ok(code) => OpenOutcome::Failed { code, message: e.to_string() },
            Err(code) => OpenOutcome::Unavailable { code, message: e.to_string() },
        },
        None => unavailable(
            UnavailableCode::UnsupportedProgram,
            "the genesis group is not an instance of any token program this build pins",
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::public_mint::{minter_redeem_of, MinterState};
    use kaspa_consensus_core::hashing::covenant_id::covenant_id as cov_id_of;
    use kaspa_consensus_core::tx::{TransactionId, TransactionOutpoint, TransactionOutput};

    fn owner() -> [u8; 32] {
        [7; 32]
    }
    fn hexs(b: &[u8]) -> String {
        crate::json::to_hex(b)
    }

    /// A genesis of the published app: one PublicMint (allowance 1000, 10 per mint) and one TokenSeed of the same owner and
    /// class; `reveal_seed` false leaves the seed unrevealed (as an unspent seed is).
    fn app_genesis(reveal_seed: bool) -> (OpenGenesis, Vec<u8>) {
        let ext = [9; 32];
        let v = IssuerVersion::F27f8403;
        let mint =
            minter_redeem_of(v, &MinterState { remaining: 1000, mint_amount: 10, owner: owner(), extension_commitment: ext }).unwrap();
        let seed = public_mint::seed_redeem_of(v, &SeedState { owner: owner(), extension_commitment: ext });
        let spk = |r: &[u8]| crate::script::p2sh_spk(r);
        let outpoint = ([3u8; 32], 0u32);
        let outs = [(0u32, 100_000_000u64, spk(&mint)), (1, 100_000_000, spk(&seed))];
        let txouts: Vec<TransactionOutput> =
            outs.iter().map(|(_, v, s)| TransactionOutput { value: *v, script_public_key: s.clone(), covenant: None }).collect();
        let id = cov_id_of(
            TransactionOutpoint { transaction_id: TransactionId::from_bytes(outpoint.0), index: outpoint.1 },
            outs.iter().zip(&txouts).map(|((i, _, _), t)| (*i, t)),
        );
        let output = |i: u32, value: u64, s: &ScriptPublicKey, r: Option<&Vec<u8>>| OpenOutput {
            index: i,
            value,
            spk_version: s.version(),
            spk: s.script().to_vec(),
            redeem: r.cloned(),
        };
        let g = OpenGenesis {
            covenant_id: hexs(&id.as_bytes()),
            genesis_txid: "22".repeat(32),
            authorizing_outpoint: outpoint,
            outputs: vec![
                output(0, 100_000_000, &outs[0].2, Some(&mint)),
                output(1, 100_000_000, &outs[1].2, reveal_seed.then_some(&seed)),
            ],
            program_hint: None,
            owner_hints: vec![],
        };
        (g, seed)
    }

    #[test]
    fn an_app_genesis_verifies_with_its_unspent_seed_rebuilt() {
        let (g, _) = app_genesis(false);
        let OpenOutcome::Verified(v) = check_open_genesis(&g) else { panic!("{:?}", check_open_genesis(&g)) };
        assert_eq!(v.program, "KCC20PublicMint");
        assert_eq!(
            (v.report.minter_outputs.clone(), v.report.seed_outputs.clone(), v.report.mint_allowance, v.report.supply),
            (vec![0], vec![1], 1000, 0)
        );
        assert_eq!(v.rebuilt, vec![1]);
        // with the seed revealed nothing is rebuilt and the verdict is the same
        let (g2, _) = app_genesis(true);
        let OpenOutcome::Verified(v2) = check_open_genesis(&g2) else { panic!() };
        assert!(v2.rebuilt.is_empty());
        assert_eq!(v2.report, v.report);
    }

    #[test]
    fn a_seed_of_the_creators_key_is_rebuilt_from_the_hint_the_genesis_transaction_gives() {
        // the seed belongs to a key no other cell of the group names (the creator's change key in the genesis transaction)
        let v = IssuerVersion::F27f8403;
        let ext = [9; 32];
        let mint =
            minter_redeem_of(v, &MinterState { remaining: 50, mint_amount: 5, owner: owner(), extension_commitment: ext }).unwrap();
        let creator = [0x42u8; 32];
        let seed = public_mint::seed_redeem_of(v, &SeedState { owner: creator, extension_commitment: ext });
        let outpoint = ([6u8; 32], 0u32);
        let spks = [crate::script::p2sh_spk(&mint), crate::script::p2sh_spk(&seed)];
        let txouts: Vec<TransactionOutput> =
            spks.iter().map(|s| TransactionOutput { value: 1, script_public_key: s.clone(), covenant: None }).collect();
        let id = cov_id_of(
            TransactionOutpoint { transaction_id: TransactionId::from_bytes(outpoint.0), index: 0 },
            txouts.iter().enumerate().map(|(i, t)| (i as u32, t)),
        );
        let out = |i: u32, s: &ScriptPublicKey, r: Option<&Vec<u8>>| OpenOutput {
            index: i,
            value: 1,
            spk_version: 0,
            spk: s.script().to_vec(),
            redeem: r.cloned(),
        };
        let mut g = OpenGenesis {
            covenant_id: hexs(&id.as_bytes()),
            genesis_txid: "55".repeat(32),
            authorizing_outpoint: outpoint,
            outputs: vec![out(0, &spks[0], Some(&mint)), out(1, &spks[1], None)],
            program_hint: None,
            owner_hints: vec![],
        };
        assert!(matches!(check_open_genesis(&g), OpenOutcome::Unavailable { code: UnavailableCode::Unrevealed, .. }));
        g.owner_hints = vec![creator];
        let OpenOutcome::Verified(r) = check_open_genesis(&g) else { panic!() };
        assert_eq!((r.rebuilt, r.report.seed_outputs, r.report.mint_allowance), (vec![1], vec![1], 50));
    }

    #[test]
    fn a_seed_that_cannot_be_rebuilt_leaves_the_genesis_unavailable() {
        let (mut g, _) = app_genesis(false);
        // a seed of another owner than any known cell: no candidate hashes to it
        let other = public_mint::seed_redeem_of(IssuerVersion::F27f8403, &SeedState { owner: [1; 32], extension_commitment: [2; 32] });
        let s = crate::script::p2sh_spk(&other);
        g.outputs[1].spk = s.script().to_vec();
        // the group changed, so the covenant id no longer matches: a mismatch, not an unrevealed seed
        assert!(matches!(check_open_genesis(&g), OpenOutcome::Failed { code: FailCode::Mismatch, .. }));
        // an honest group whose seed belongs to an unknown owner is unavailable
        let (g, _) = app_genesis(false);
        let mut lone = g.clone();
        lone.outputs[0].redeem = None;
        let r = check_open_genesis(&lone);
        assert!(matches!(r, OpenOutcome::Unavailable { code: UnavailableCode::Unrevealed, .. }), "{r:?}");
    }

    #[test]
    fn tampered_groups_fail() {
        let (g, _) = app_genesis(true);
        let tamper = |f: &dyn Fn(&mut OpenGenesis)| {
            let mut t = g.clone();
            f(&mut t);
            check_open_genesis(&t)
        };
        // a changed value, an extra output, a missing output, another outpoint: the covenant id no longer recomputes
        assert!(matches!(tamper(&|t| t.outputs[0].value += 1), OpenOutcome::Failed { code: FailCode::Mismatch, .. }));
        assert!(matches!(
            tamper(&|t| {
                t.outputs.pop();
            }),
            OpenOutcome::Failed { code: FailCode::Mismatch, .. }
        ));
        assert!(matches!(tamper(&|t| t.authorizing_outpoint.1 = 1), OpenOutcome::Failed { code: FailCode::Mismatch, .. }));
        // a flipped byte in a revealed script: it no longer hashes to its output
        let r = tamper(&|t| {
            let b = t.outputs[0].redeem.as_mut().unwrap();
            let mid = b.len() / 2;
            b[mid] ^= 1;
        });
        assert!(matches!(r, OpenOutcome::Failed { code: FailCode::Mismatch, .. }), "{r:?}");
        // malformed input is unavailable, never a verdict
        assert!(matches!(tamper(&|t| t.covenant_id = "zz".into()), OpenOutcome::Unavailable { code: UnavailableCode::Malformed, .. }));
        assert!(matches!(tamper(&|t| t.outputs.clear()), OpenOutcome::Unavailable { code: UnavailableCode::Malformed, .. }));
    }

    #[test]
    fn a_hidden_output_makes_the_genesis_fail_as_contaminated() {
        // an honest genesis plus one output whose script is not the program: the id commits to all of it, so the covenant
        // id recomputes and the verdict is "contaminated", not a data error
        let ext = [9; 32];
        let v = IssuerVersion::F27f8403;
        let mint =
            minter_redeem_of(v, &MinterState { remaining: 5, mint_amount: 1, owner: owner(), extension_commitment: ext }).unwrap();
        let rogue = vec![0x51u8]; // OP_TRUE
        let spk = |r: &[u8]| crate::script::p2sh_spk(r);
        let outpoint = ([4u8; 32], 1u32);
        let outs = [(0u32, 1u64, spk(&mint)), (1, 1, spk(&rogue))];
        let txouts: Vec<TransactionOutput> =
            outs.iter().map(|(_, v, s)| TransactionOutput { value: *v, script_public_key: s.clone(), covenant: None }).collect();
        let id = cov_id_of(
            TransactionOutpoint { transaction_id: TransactionId::from_bytes(outpoint.0), index: outpoint.1 },
            outs.iter().zip(&txouts).map(|((i, _, _), t)| (*i, t)),
        );
        let o = |i: u32, s: &ScriptPublicKey, r: &Vec<u8>| OpenOutput {
            index: i,
            value: 1,
            spk_version: 0,
            spk: s.script().to_vec(),
            redeem: Some(r.clone()),
        };
        let g = OpenGenesis {
            covenant_id: hexs(&id.as_bytes()),
            genesis_txid: "33".repeat(32),
            authorizing_outpoint: outpoint,
            outputs: vec![o(0, &outs[0].2, &mint), o(1, &outs[1].2, &rogue)],
            program_hint: None,
            owner_hints: vec![],
        };
        let r = check_open_genesis(&g);
        assert!(matches!(r, OpenOutcome::Failed { code: FailCode::Contaminated, .. }), "{r:?}");
        // and a group of unknown scripts only is "unsupported", not contaminated
        let mut u = g.clone();
        u.outputs.remove(0);
        // (the id no longer matches the reduced group, which is a mismatch; the unsupported case needs its own id)
        assert!(matches!(check_open_genesis(&u), OpenOutcome::Failed { code: FailCode::Mismatch, .. }));
    }

    #[test]
    fn a_group_of_no_known_program_is_unsupported() {
        let rogue = vec![0x51u8];
        let s = crate::script::p2sh_spk(&rogue);
        let outpoint = ([5u8; 32], 0u32);
        let t = TransactionOutput { value: 1, script_public_key: s.clone(), covenant: None };
        let id = cov_id_of(
            TransactionOutpoint { transaction_id: TransactionId::from_bytes(outpoint.0), index: 0 },
            [(0u32, &t)].into_iter(),
        );
        let g = OpenGenesis {
            covenant_id: hexs(&id.as_bytes()),
            genesis_txid: "44".repeat(32),
            authorizing_outpoint: outpoint,
            outputs: vec![OpenOutput { index: 0, value: 1, spk_version: 0, spk: s.script().to_vec(), redeem: Some(rogue) }],
            program_hint: None,
            owner_hints: vec![],
        };
        let r = check_open_genesis(&g);
        assert!(matches!(r, OpenOutcome::Unavailable { code: UnavailableCode::UnsupportedProgram, .. }), "{r:?}");
    }
}
