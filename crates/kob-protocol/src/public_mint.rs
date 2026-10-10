//! The issuance actors of upstream's published `KCC20PublicMint` app (argent-lang/kcc20-reference `c8a0871`,
//! `contracts/public_mint.ag` and `contracts/token_seed.ag`), as the genesis check of a token of that app reads them.
//!
//! The app has three actors in one covenant family: `KCC20` (the holders, pinned by KOB as
//! [`TemplateId::Kcc20PublicMint`]), `PublicMint` and `TokenSeed`. Upstream recommends issuing a token by a genesis that
//! holds one `PublicMint` (the advertised supply as its allowance) and at least one `TokenSeed`, and no pre-minted
//! balances. KOB takes their programs from upstream's published artifact (`contracts/third-party/kcc20-reference/
//! public-mint.artifact.json`, vendored unmodified and pinned by sha256 in `crates/kob-tests/tests/kcc20_reference_tests.rs`)
//! and checks the Sil template hash of each against the constants below.
//!
//! What the actors can do (the supply rules the genesis check rests on):
//!
//! * `PublicMint { remaining, mint_amount, owner, extension_commitment }`: anyone mints `0 < amount <= min(mint_amount,
//!   remaining)` into ONE new `KCC20` holder of the minter's extension commitment, and the minter continues with
//!   `remaining - amount`; anyone splits `0 < take <= remaining / 2` off into a second minter; two minters of the same
//!   `mint_amount` and commitment merge (`reclaim_into`, the retiring owner signs); an exhausted minter (`remaining == 0`)
//!   is closed by its owner. Every path keeps `Σ remaining (live minters) + Σ amount (holders)` constant, and the only
//!   source of a `PublicMint` is another `PublicMint` or the genesis.
//! * `TokenSeed { owner, extension_commitment }`: anyone creates a zero-amount `KCC20` holder of the seed's commitment, or
//!   another seed; seeds never carry or create an amount.
//! * `KCC20`: transfers conserve the amount and create only `KCC20` holders.
//!
//! So a token's supply is bounded forever by its genesis: its **maximum supply** is the holders' amounts of the genesis plus
//! the `remaining` allowance of its genesis minters ([`GenesisSupply`]), and at any later time the amount minted so far is
//! that maximum less the `remaining` of the live minters. A genesis without a minter has a fixed supply.
//!
//! Each actor's state starts with compiler-owned template fields (`gen__kcc20_template`, then the actor's own), which
//! decide which program the actor recognises as `KCC20` and as itself: a minter whose `gen__kcc20_template` named another
//! program would mint outputs of that program. The genesis check therefore requires them to be exactly the Sil template
//! hashes of the published app ([`decode_companion`]).

use std::sync::OnceLock;

use serde_json::Value;

use crate::artifacts::{template, TemplateId};
use crate::kcc1::{int_from_state_payload, int_state_payload};

/// Sil template hash of the `KCC20` actor of the published app (`gen__kcc20_template` in every UTXO of the app).
pub const KCC20_SIL_HASH: &str = "9703112ee6e3555107cd168858992b77d3b74f655205b2b463b1f9ec2ec73cf7";
/// Sil template hash of the `PublicMint` actor of the published app (`gen__public_mint_template`).
pub const PUBLIC_MINT_SIL_HASH: &str = "14b0f06bc176b78c94f19292e409c622636f31dae6bb31ff3d4d3308a6053b49";
/// Sil template hash of the `TokenSeed` actor of the published app (`gen__token_seed_template`).
pub const TOKEN_SEED_SIL_HASH: &str = "f79a3fccf6f87d7076662410a17e54d1531889d9b60f630f9fbb8465a384e62d";

const APP_ARTIFACT: &str = include_str!("../../../contracts/third-party/kcc20-reference/public-mint.artifact.json");

/// `PublicMint` state span: `0x20 kcc20 tpl | 0x20 own tpl | 0x08 remaining | 0x08 mint_amount | 0x20 owner | 0x20 ext`.
pub const PUBLIC_MINT_STATE_LEN: usize = 150;
/// `TokenSeed` state span: `0x20 kcc20 tpl | 0x20 own tpl | 0x20 owner | 0x20 ext`.
pub const TOKEN_SEED_STATE_LEN: usize = 132;

/// One issuance actor's program: the Sil cut of the published artifact.
#[derive(Clone, Debug)]
pub struct AppProgram {
    /// Actor name (`PublicMint`, `TokenSeed`).
    pub actor: &'static str,
    /// Bytecode before the state span.
    pub prefix: Vec<u8>,
    /// Bytecode after the state span.
    pub suffix: Vec<u8>,
    /// Length of the state span.
    pub state_len: usize,
    /// Sil template hash (`template_hash(prefix, suffix)`), equal to the pinned constant.
    pub sil_hash: [u8; 32],
}

impl AppProgram {
    /// Redeem script of an instance in `state`.
    pub fn redeem(&self, state: &[u8]) -> Vec<u8> {
        assert_eq!(state.len(), self.state_len, "{}: state span must be {} bytes", self.actor, self.state_len);
        [self.prefix.as_slice(), state, self.suffix.as_slice()].concat()
    }

    /// State span of a redeem script, if it is an instance of this program.
    pub fn state_of<'a>(&self, redeem: &'a [u8]) -> Option<&'a [u8]> {
        if redeem.len() != self.prefix.len() + self.state_len + self.suffix.len()
            || !redeem.starts_with(&self.prefix)
            || !redeem.ends_with(&self.suffix)
        {
            return None;
        }
        Some(&redeem[self.prefix.len()..self.prefix.len() + self.state_len])
    }
}

/// State of a `PublicMint` (the minter).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MinterState {
    /// Allowance left to mint (base units).
    pub remaining: i64,
    /// Largest amount one `mint` issues.
    pub mint_amount: i64,
    /// Schnorr key that reclaims the minter's deposit.
    pub owner: [u8; 32],
    /// Extension commitment of every holder it mints.
    pub extension_commitment: [u8; 32],
}

/// State of a `TokenSeed`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SeedState {
    /// Schnorr key that reclaims the seed's deposit.
    pub owner: [u8; 32],
    /// Extension commitment of every zero-amount holder it creates.
    pub extension_commitment: [u8; 32],
}

/// A genesis output of the app that is not a `KCC20` holder.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Companion {
    /// A `PublicMint`.
    Minter(MinterState),
    /// A `TokenSeed`.
    Seed(SeedState),
}

impl Companion {
    /// Extension commitment of the actor.
    pub fn extension_commitment(&self) -> [u8; 32] {
        match self {
            Companion::Minter(m) => m.extension_commitment,
            Companion::Seed(s) => s.extension_commitment,
        }
    }
}

/// What a genesis of the app created: the supply the genesis check reports (see the module docs).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct GenesisSupply {
    /// Sum of the genesis holders' amounts (minted at genesis).
    pub holders: i64,
    /// Sum of the genesis minters' `remaining` (the allowance anyone may still mint).
    pub mint_allowance: i64,
}

impl GenesisSupply {
    /// The most that can ever exist: holders plus allowance (`None` beyond `i64::MAX`).
    pub fn max_supply(&self) -> Option<i64> {
        self.holders.checked_add(self.mint_allowance)
    }

    /// Minted so far, given the `remaining` of every live minter (`None` when they exceed the genesis allowance, which no
    /// transaction of the app can cause).
    pub fn minted(&self, live_remaining: &[i64]) -> Option<i64> {
        let live = live_remaining.iter().try_fold(0i64, |s, r| if *r < 0 { None } else { s.checked_add(*r) })?;
        if live > self.mint_allowance {
            return None;
        }
        self.max_supply()?.checked_sub(live)
    }
}

fn hex32(h: &str) -> [u8; 32] {
    crate::json::hex32(h).expect("pinned hex")
}

fn load(contract: &str, actor: &'static str, state_len: usize, pinned: &str) -> AppProgram {
    let doc: Value = serde_json::from_str(APP_ARTIFACT).expect("vendored public-mint artifact parses");
    let c = &doc["sil_abi"]["contracts"][contract]["compiled"];
    let bytecode: Vec<u8> =
        c["bytecode"].as_array().expect("bytecode").iter().map(|b| u8::try_from(b.as_u64().expect("byte")).expect("byte")).collect();
    let offset = c["state_span"]["offset"].as_u64().expect("offset") as usize;
    let len = c["state_span"]["len"].as_u64().expect("len") as usize;
    assert_eq!(len, state_len, "{actor}: state span");
    let prefix = bytecode[..offset].to_vec();
    let suffix = bytecode[offset + len..].to_vec();
    let sil_hash = silverscript_abi::template_hash(&prefix, &suffix);
    let recorded: Vec<u8> =
        c["template_hash"].as_array().expect("template_hash").iter().map(|b| b.as_u64().expect("byte") as u8).collect();
    assert_eq!(recorded, sil_hash, "{actor}: the artifact's template hash");
    assert_eq!(sil_hash, hex32(pinned), "{actor}: the vendored artifact is not the pinned program");
    AppProgram { actor, prefix, suffix, state_len, sil_hash }
}

/// The `PublicMint` program of the published app.
pub fn public_mint() -> &'static AppProgram {
    static P: OnceLock<AppProgram> = OnceLock::new();
    P.get_or_init(|| load("PublicMint", "PublicMint", PUBLIC_MINT_STATE_LEN, PUBLIC_MINT_SIL_HASH))
}

/// The `TokenSeed` program of the published app.
pub fn token_seed() -> &'static AppProgram {
    static P: OnceLock<AppProgram> = OnceLock::new();
    P.get_or_init(|| load("TokenSeed", "TokenSeed", TOKEN_SEED_STATE_LEN, TOKEN_SEED_SIL_HASH))
}

fn push32(v: &mut Vec<u8>, b: &[u8; 32]) {
    v.push(0x20);
    v.extend_from_slice(b);
}

fn push_int(v: &mut Vec<u8>, x: i64) -> Option<()> {
    v.push(0x08);
    v.extend_from_slice(&int_state_payload(x)?);
    Some(())
}

/// Canonical state span of a `PublicMint` (the context fields set to the app's template hashes).
pub fn minter_state_bytes(m: &MinterState) -> Option<Vec<u8>> {
    let mut v = Vec::with_capacity(PUBLIC_MINT_STATE_LEN);
    push32(&mut v, &hex32(KCC20_SIL_HASH));
    push32(&mut v, &hex32(PUBLIC_MINT_SIL_HASH));
    push_int(&mut v, m.remaining)?;
    push_int(&mut v, m.mint_amount)?;
    push32(&mut v, &m.owner);
    push32(&mut v, &m.extension_commitment);
    Some(v)
}

/// Canonical state span of a `TokenSeed` (the context fields set to the app's template hashes).
pub fn seed_state_bytes(s: &SeedState) -> Vec<u8> {
    let mut v = Vec::with_capacity(TOKEN_SEED_STATE_LEN);
    push32(&mut v, &hex32(KCC20_SIL_HASH));
    push32(&mut v, &hex32(TOKEN_SEED_SIL_HASH));
    push32(&mut v, &s.owner);
    push32(&mut v, &s.extension_commitment);
    v
}

/// Redeem script of a `PublicMint` in state `m`.
pub fn minter_redeem(m: &MinterState) -> Option<Vec<u8>> {
    Some(public_mint().redeem(&minter_state_bytes(m)?))
}

/// Redeem script of a `TokenSeed` in state `s`.
pub fn seed_redeem(s: &SeedState) -> Vec<u8> {
    token_seed().redeem(&seed_state_bytes(s))
}

fn field32(st: &[u8], at: usize, what: &str) -> Result<[u8; 32], String> {
    match st.get(at..at + 33) {
        Some(b) if b[0] == 0x20 => Ok(b[1..].try_into().expect("32 bytes")),
        _ => Err(format!("{what} is not a 32-byte push")),
    }
}

fn field_int(st: &[u8], at: usize, what: &str) -> Result<i64, String> {
    match st.get(at..at + 9) {
        Some(b) if b[0] == 0x08 => int_from_state_payload(&b[1..]).ok_or_else(|| format!("{what} is not a canonical int")),
        _ => Err(format!("{what} is not an 8-byte int push")),
    }
}

fn context(st: &[u8], own: &str, actor: &str) -> Result<(), String> {
    if field32(st, 0, "gen__kcc20_template")? != hex32(KCC20_SIL_HASH) {
        return Err(format!("{actor}: gen__kcc20_template is not the KCC20 program of the published app ({KCC20_SIL_HASH})"));
    }
    if field32(st, 33, "the actor's own template field")? != hex32(own) {
        return Err(format!("{actor}: its own template field is not its program ({own})"));
    }
    Ok(())
}

/// Reads a redeem script as a `PublicMint` or `TokenSeed` of the published app.
///
/// `Ok(None)`: an instance of neither program. `Err`: an instance whose state is not canonical, or whose template fields
/// are not the app's (a minter or seed that would create outputs of another program).
pub fn decode_companion(redeem: &[u8]) -> Result<Option<Companion>, String> {
    if let Some(st) = public_mint().state_of(redeem) {
        context(st, PUBLIC_MINT_SIL_HASH, "PublicMint")?;
        let m = MinterState {
            remaining: field_int(st, 66, "remaining")?,
            mint_amount: field_int(st, 75, "mint_amount")?,
            owner: field32(st, 84, "owner")?,
            extension_commitment: field32(st, 117, "extension_commitment")?,
        };
        if minter_state_bytes(&m).as_deref() != Some(st) {
            return Err("PublicMint: the state is not canonical".into());
        }
        return Ok(Some(Companion::Minter(m)));
    }
    if let Some(st) = token_seed().state_of(redeem) {
        context(st, TOKEN_SEED_SIL_HASH, "TokenSeed")?;
        let s = SeedState { owner: field32(st, 66, "owner")?, extension_commitment: field32(st, 99, "extension_commitment")? };
        if seed_state_bytes(&s) != st {
            return Err("TokenSeed: the state is not canonical".into());
        }
        return Ok(Some(Companion::Seed(s)));
    }
    Ok(None)
}

/// True when `id` is the holder program of the published app (the template whose genesis may hold companions).
pub fn is_app_holder(id: TemplateId) -> bool {
    id == TemplateId::Kcc20PublicMint
}

/// The pinned `KCC20` handle names the same Sil program as [`KCC20_SIL_HASH`] (checked by the tests).
pub fn kcc20_sil_hash() -> [u8; 32] {
    template(TemplateId::Kcc20PublicMint).sil_hash
}

#[cfg(test)]
mod tests {
    use super::*;
    use silverscript_abi::{encode_runtime_state_script, ArtifactValue, SilAbiArtifact};
    use std::collections::BTreeMap;

    fn abi() -> SilAbiArtifact {
        let doc: Value = serde_json::from_str(APP_ARTIFACT).unwrap();
        serde_json::from_value(doc["sil_abi"].clone()).unwrap()
    }

    #[test]
    fn programs_are_the_published_ones() {
        assert_eq!(kcc20_sil_hash(), hex32(KCC20_SIL_HASH));
        let (m, s) = (public_mint(), token_seed());
        assert_eq!((m.prefix.len(), m.state_len, m.suffix.len()), (1, 150, 1_868));
        assert_eq!((s.prefix.len(), s.state_len, s.suffix.len()), (1, 132, 1_222));
    }

    /// The hand codec equals the ABI encoder of the published artifact for both actors.
    #[test]
    fn state_codec_matches_the_artifact_abi() {
        let a = abi();
        let m = MinterState { remaining: 21_000_000, mint_amount: -5, owner: [7; 32], extension_commitment: [0xee; 32] };
        let fields = BTreeMap::from([
            ("gen__kcc20_template".to_string(), ArtifactValue::from(hex32(KCC20_SIL_HASH).to_vec())),
            ("gen__public_mint_template".to_string(), hex32(PUBLIC_MINT_SIL_HASH).to_vec().into()),
            ("remaining".to_string(), ArtifactValue::Int(m.remaining)),
            ("mint_amount".to_string(), ArtifactValue::Int(m.mint_amount)),
            ("owner".to_string(), m.owner.to_vec().into()),
            ("extension_commitment".to_string(), m.extension_commitment.to_vec().into()),
        ]);
        let c = &a.contracts["PublicMint"];
        let want = encode_runtime_state_script(&a, &c.runtime_state, &fields).unwrap();
        assert_eq!(minter_state_bytes(&m).unwrap(), want);
        let s = SeedState { owner: [9; 32], extension_commitment: [0; 32] };
        let fields = BTreeMap::from([
            ("gen__kcc20_template".to_string(), ArtifactValue::from(hex32(KCC20_SIL_HASH).to_vec())),
            ("gen__token_seed_template".to_string(), hex32(TOKEN_SEED_SIL_HASH).to_vec().into()),
            ("owner".to_string(), s.owner.to_vec().into()),
            ("extension_commitment".to_string(), s.extension_commitment.to_vec().into()),
        ]);
        let c = &a.contracts["TokenSeed"];
        assert_eq!(seed_state_bytes(&s), encode_runtime_state_script(&a, &c.runtime_state, &fields).unwrap());
        // round trips
        assert_eq!(decode_companion(&minter_redeem(&m).unwrap()).unwrap(), Some(Companion::Minter(m.clone())));
        assert_eq!(decode_companion(&seed_redeem(&s)).unwrap(), Some(Companion::Seed(s)));
    }

    #[test]
    fn foreign_template_fields_and_non_canonical_states_are_refused() {
        let m = MinterState { remaining: 10, mint_amount: 1, owner: [7; 32], extension_commitment: [0; 32] };
        let good = minter_redeem(&m).unwrap();
        for (at, what) in [(2, "gen__kcc20_template"), (35, "own template field")] {
            let mut bad = good.clone();
            bad[1 + at] ^= 1;
            let e = decode_companion(&bad).unwrap_err();
            assert!(e.contains("template"), "{what}: {e}");
        }
        // negative zero remaining
        let mut nz = good.clone();
        let at = 1 + 66 + 1;
        nz[at..at + 8].copy_from_slice(&[0, 0, 0, 0, 0, 0, 0, 0x80]);
        assert!(decode_companion(&nz).unwrap_err().contains("canonical"));
        // neither program
        assert_eq!(decode_companion(&[0x51]).unwrap(), None);
        let mut other = good.clone();
        let n = other.len();
        other[n - 1] ^= 1;
        assert_eq!(decode_companion(&other).unwrap(), None);
    }

    #[test]
    fn supply_accounting() {
        let g = GenesisSupply { holders: 100, mint_allowance: 900 };
        assert_eq!(g.max_supply(), Some(1_000));
        assert_eq!(g.minted(&[900]), Some(100));
        assert_eq!(g.minted(&[400, 0, 100]), Some(500));
        assert_eq!(g.minted(&[]), Some(1_000));
        assert_eq!(g.minted(&[901]), None);
        assert_eq!(g.minted(&[-1]), None);
        assert_eq!(GenesisSupply { holders: i64::MAX, mint_allowance: 1 }.max_supply(), None);
    }
}
