//! The issuance actors of upstream's published `KCC20PublicMint` app (argent-lang/kcc20-reference, `contracts/public_mint.ag`
//! and `contracts/token_seed.ag`), as the genesis check of a token of that app reads them, in both upstream builds KOB
//! pins ([`IssuerVersion`]): `c8a0871` and `27f8403`.
//!
//! The app has three actors in one covenant family: `KCC20` (the holders, pinned by KOB as
//! [`TemplateId::Kcc20PublicMint`]), `PublicMint` and `TokenSeed`. Upstream recommends issuing a token by a genesis that
//! holds one `PublicMint` (the advertised supply as its allowance) and at least one `TokenSeed`, and no pre-minted
//! balances. KOB takes their programs from upstream's published artifacts (`contracts/third-party/kcc20-reference/
//! public-mint.artifact.json` for `c8a0871` and `public-mint.27f8403.artifact.json`, vendored unmodified and pinned by
//! sha256 in `crates/kob-tests/tests/kcc20_reference_tests.rs`) and checks the Sil template hash of each against the
//! constants below. The `KCC20` holder program is the same bytes in both builds (Sil hash [`KCC20_SIL_HASH`]); only the two
//! issuers differ, so a token of either build is the same token program to every order.
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
//! * `27f8403` adds one entry to each issuer, `rebalance_value(owner_signature)`: the deposit owner signs a transaction that
//!   spends exactly this issuer cell (no other input of the covenant id) and recreates exactly one output with the identical
//!   script public key, so the whole state (allowance, per-mint limit, owner, extension commitment) is kept and only the
//!   cell's KAS value may change. It mints nothing, creates no holder and no other issuer, and changes no allowance: it is
//!   not a supply or mint-authority power (a lone seed's deposit can now be withdrawn through it, which `reclaim` forbids).
//!   A reader that follows a minter's lineage must follow such a spend to the identical successor cell.
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
/// Sil template hash of the `PublicMint` actor of the published app at `c8a0871` (`gen__public_mint_template`).
pub const PUBLIC_MINT_SIL_HASH: &str = "14b0f06bc176b78c94f19292e409c622636f31dae6bb31ff3d4d3308a6053b49";
/// Sil template hash of the `TokenSeed` actor of the published app at `c8a0871` (`gen__token_seed_template`).
pub const TOKEN_SEED_SIL_HASH: &str = "f79a3fccf6f87d7076662410a17e54d1531889d9b60f630f9fbb8465a384e62d";
/// Sil template hash of the `PublicMint` actor of the published app at `27f8403` (with `rebalance_value`).
pub const PUBLIC_MINT_SIL_HASH_27F8403: &str = "8c87ce6e88755277c4d243cf4690e43d05eb4c4213d4a1824dcbefd13061ac53";
/// Sil template hash of the `TokenSeed` actor of the published app at `27f8403` (with `rebalance_value`).
pub const TOKEN_SEED_SIL_HASH_27F8403: &str = "6ea2c53a45cc4dcd6156bc7d135ca9b6466bd691465206d660d0ef29ebc6c2ec";

const APP_ARTIFACT_C8A0871: &str = include_str!("../../../contracts/third-party/kcc20-reference/public-mint.artifact.json");
const APP_ARTIFACT_27F8403: &str = include_str!("../../../contracts/third-party/kcc20-reference/public-mint.27f8403.artifact.json");

/// An upstream build of the issuance actors (`PublicMint`, `TokenSeed`) that KOB pins. Both build the same `KCC20` holder
/// program; they differ in the issuers' entries (`27f8403` adds `rebalance_value`), so their template hashes differ and a
/// genesis is read against the build its actors are instances of.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum IssuerVersion {
    /// argent-lang/kcc20-reference `c8a0871` (2026-10-07).
    C8a0871,
    /// argent-lang/kcc20-reference `27f8403` (2026-10-21): the issuers also have the owner-signed `rebalance_value`.
    F27f8403,
}

impl IssuerVersion {
    /// Every pinned build, oldest first.
    pub const ALL: [IssuerVersion; 2] = [IssuerVersion::C8a0871, IssuerVersion::F27f8403];

    /// The upstream commit (7 characters).
    pub fn commit(self) -> &'static str {
        match self {
            IssuerVersion::C8a0871 => "c8a0871",
            IssuerVersion::F27f8403 => "27f8403",
        }
    }

    /// The name a genesis check reports (`GenesisReport::issuer`, the registry genesis record's `issuer`).
    pub fn label(self) -> &'static str {
        match self {
            IssuerVersion::C8a0871 => "kcc20-reference@c8a0871",
            IssuerVersion::F27f8403 => "kcc20-reference@27f8403",
        }
    }

    /// The build named by [`IssuerVersion::label`].
    pub fn from_label(label: &str) -> Option<IssuerVersion> {
        IssuerVersion::ALL.into_iter().find(|v| v.label() == label)
    }

    /// Sil template hash of this build's `PublicMint`.
    pub fn public_mint_hash(self) -> &'static str {
        match self {
            IssuerVersion::C8a0871 => PUBLIC_MINT_SIL_HASH,
            IssuerVersion::F27f8403 => PUBLIC_MINT_SIL_HASH_27F8403,
        }
    }

    /// Sil template hash of this build's `TokenSeed`.
    pub fn token_seed_hash(self) -> &'static str {
        match self {
            IssuerVersion::C8a0871 => TOKEN_SEED_SIL_HASH,
            IssuerVersion::F27f8403 => TOKEN_SEED_SIL_HASH_27F8403,
        }
    }

    fn artifact(self) -> &'static str {
        match self {
            IssuerVersion::C8a0871 => APP_ARTIFACT_C8A0871,
            IssuerVersion::F27f8403 => APP_ARTIFACT_27F8403,
        }
    }

    fn idx(self) -> usize {
        self as usize
    }
}

/// `PublicMint` state span: `0x20 kcc20 tpl | 0x20 own tpl | 0x08 remaining | 0x08 mint_amount | 0x20 owner | 0x20 ext`.
pub const PUBLIC_MINT_STATE_LEN: usize = 150;
/// `TokenSeed` state span: `0x20 kcc20 tpl | 0x20 own tpl | 0x20 owner | 0x20 ext`.
pub const TOKEN_SEED_STATE_LEN: usize = 132;

/// One issuance actor's program: the Sil cut of the published artifact.
#[derive(Clone, Debug)]
pub struct AppProgram {
    /// The upstream build this program is from.
    pub version: IssuerVersion,
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

fn load(version: IssuerVersion, contract: &str, actor: &'static str, state_len: usize, pinned: &str) -> AppProgram {
    let doc: Value = serde_json::from_str(version.artifact()).expect("vendored public-mint artifact parses");
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
    AppProgram { version, actor, prefix, suffix, state_len, sil_hash }
}

/// The `PublicMint` program of the `c8a0871` build of the published app.
pub fn public_mint() -> &'static AppProgram {
    public_mint_of(IssuerVersion::C8a0871)
}

/// The `TokenSeed` program of the `c8a0871` build of the published app.
pub fn token_seed() -> &'static AppProgram {
    token_seed_of(IssuerVersion::C8a0871)
}

/// The `PublicMint` program of the given build of the published app.
pub fn public_mint_of(v: IssuerVersion) -> &'static AppProgram {
    static P: [OnceLock<AppProgram>; 2] = [OnceLock::new(), OnceLock::new()];
    P[v.idx()].get_or_init(|| load(v, "PublicMint", "PublicMint", PUBLIC_MINT_STATE_LEN, v.public_mint_hash()))
}

/// The `TokenSeed` program of the given build of the published app.
pub fn token_seed_of(v: IssuerVersion) -> &'static AppProgram {
    static P: [OnceLock<AppProgram>; 2] = [OnceLock::new(), OnceLock::new()];
    P[v.idx()].get_or_init(|| load(v, "TokenSeed", "TokenSeed", TOKEN_SEED_STATE_LEN, v.token_seed_hash()))
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

/// Canonical state span of a `PublicMint` of the `c8a0871` build (the context fields set to the app's template hashes).
pub fn minter_state_bytes(m: &MinterState) -> Option<Vec<u8>> {
    minter_state_bytes_of(IssuerVersion::C8a0871, m)
}

/// Canonical state span of a `PublicMint` of the given build.
pub fn minter_state_bytes_of(version: IssuerVersion, m: &MinterState) -> Option<Vec<u8>> {
    let mut v = Vec::with_capacity(PUBLIC_MINT_STATE_LEN);
    push32(&mut v, &hex32(KCC20_SIL_HASH));
    push32(&mut v, &hex32(version.public_mint_hash()));
    push_int(&mut v, m.remaining)?;
    push_int(&mut v, m.mint_amount)?;
    push32(&mut v, &m.owner);
    push32(&mut v, &m.extension_commitment);
    Some(v)
}

/// Canonical state span of a `TokenSeed` of the `c8a0871` build (the context fields set to the app's template hashes).
pub fn seed_state_bytes(s: &SeedState) -> Vec<u8> {
    seed_state_bytes_of(IssuerVersion::C8a0871, s)
}

/// Canonical state span of a `TokenSeed` of the given build.
pub fn seed_state_bytes_of(version: IssuerVersion, s: &SeedState) -> Vec<u8> {
    let mut v = Vec::with_capacity(TOKEN_SEED_STATE_LEN);
    push32(&mut v, &hex32(KCC20_SIL_HASH));
    push32(&mut v, &hex32(version.token_seed_hash()));
    push32(&mut v, &s.owner);
    push32(&mut v, &s.extension_commitment);
    v
}

/// Redeem script of a `PublicMint` of the `c8a0871` build in state `m`.
pub fn minter_redeem(m: &MinterState) -> Option<Vec<u8>> {
    minter_redeem_of(IssuerVersion::C8a0871, m)
}

/// Redeem script of a `PublicMint` of the given build in state `m`.
pub fn minter_redeem_of(version: IssuerVersion, m: &MinterState) -> Option<Vec<u8>> {
    Some(public_mint_of(version).redeem(&minter_state_bytes_of(version, m)?))
}

/// Redeem script of a `TokenSeed` of the `c8a0871` build in state `s`.
pub fn seed_redeem(s: &SeedState) -> Vec<u8> {
    seed_redeem_of(IssuerVersion::C8a0871, s)
}

/// Redeem script of a `TokenSeed` of the given build in state `s`.
pub fn seed_redeem_of(version: IssuerVersion, s: &SeedState) -> Vec<u8> {
    token_seed_of(version).redeem(&seed_state_bytes_of(version, s))
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

/// Reads a redeem script as a `PublicMint` or `TokenSeed` of the published app, with the build it is an instance of.
///
/// `Ok(None)`: an instance of no pinned build's program. `Err`: an instance whose state is not canonical, or whose template
/// fields are not the app's (a minter or seed that would create outputs of another program).
pub fn decode_companion_versioned(redeem: &[u8]) -> Result<Option<(IssuerVersion, Companion)>, String> {
    for v in IssuerVersion::ALL {
        if let Some(st) = public_mint_of(v).state_of(redeem) {
            context(st, v.public_mint_hash(), "PublicMint")?;
            let m = MinterState {
                remaining: field_int(st, 66, "remaining")?,
                mint_amount: field_int(st, 75, "mint_amount")?,
                owner: field32(st, 84, "owner")?,
                extension_commitment: field32(st, 117, "extension_commitment")?,
            };
            if minter_state_bytes_of(v, &m).as_deref() != Some(st) {
                return Err("PublicMint: the state is not canonical".into());
            }
            return Ok(Some((v, Companion::Minter(m))));
        }
        if let Some(st) = token_seed_of(v).state_of(redeem) {
            context(st, v.token_seed_hash(), "TokenSeed")?;
            let s = SeedState { owner: field32(st, 66, "owner")?, extension_commitment: field32(st, 99, "extension_commitment")? };
            if seed_state_bytes_of(v, &s) != st {
                return Err("TokenSeed: the state is not canonical".into());
            }
            return Ok(Some((v, Companion::Seed(s))));
        }
    }
    Ok(None)
}

/// [`decode_companion_versioned`] without the build.
pub fn decode_companion(redeem: &[u8]) -> Result<Option<Companion>, String> {
    Ok(decode_companion_versioned(redeem)?.map(|(_, c)| c))
}

/// The ABI of the given build of the published app (`sil_abi` of the vendored artifact).
fn app_abi(v: IssuerVersion) -> &'static silverscript_abi::SilAbiArtifact {
    static A: [OnceLock<silverscript_abi::SilAbiArtifact>; 2] = [OnceLock::new(), OnceLock::new()];
    A[v.idx()].get_or_init(|| {
        let doc: Value = serde_json::from_str(v.artifact()).expect("vendored public-mint artifact parses");
        serde_json::from_value(doc["sil_abi"].clone()).expect("vendored public-mint ABI parses")
    })
}

/// The app's `KCC20` program (the Sil cut: its state span starts with `gen__kcc20_template`), what a `mint` / `create`
/// call passes as `gen__kcc20_prefix` / `gen__kcc20_suffix`. The same bytes in both builds (checked by the tests).
pub fn kcc20_program() -> &'static AppProgram {
    static P: OnceLock<AppProgram> = OnceLock::new();
    P.get_or_init(|| load(IssuerVersion::C8a0871, "KCC20", "KCC20", 145, KCC20_SIL_HASH))
}

/// Signature script of a `PublicMint::mint` call (minter `m` of the `c8a0871` build) creating the holder `recipient`
/// (wallets and tests; KOB itself only reads such calls, [`created_holder`]).
pub fn mint_sigscript(m: &MinterState, recipient: &crate::state::Kcc20State) -> Option<Vec<u8>> {
    mint_sigscript_of(IssuerVersion::C8a0871, m, recipient)
}

/// [`mint_sigscript`] for a minter of the given build.
pub fn mint_sigscript_of(version: IssuerVersion, m: &MinterState, recipient: &crate::state::Kcc20State) -> Option<Vec<u8>> {
    use crate::state::FieldMap;
    use silverscript_abi::ArtifactValue;
    let k = kcc20_program();
    let args = vec![ArtifactValue::Object(recipient.to_values()), k.prefix.clone().into(), k.suffix.clone().into()];
    let mut s = silverscript_abi::encode_contract_entry_sig_script(app_abi(version), "PublicMint", "mint", &args).ok()?;
    s.extend_from_slice(&crate::script::push_data(&minter_redeem_of(version, m)?));
    Some(s)
}

/// The holder a `PublicMint::mint` or `TokenSeed::create` call creates (its `recipient_state` argument), from the calling
/// input's signature script, with the holder program it is an output of ([`TemplateId::Kcc20PublicMint`]: the app's
/// `KCC20`, which every canonical minter and seed names in `gen__kcc20_template`).
///
/// `None` unless the redeem script is a canonical `PublicMint` / `TokenSeed` of either pinned build, the dispatch tag is that entry's
/// and the arguments decode strictly. The state is a claim of the signature script: a reader proves it by comparing the
/// P2SH it produces under the holder program with the output's script public key (as for a transfer's `next_states`).
pub fn created_holder(signature_script: &[u8]) -> Option<(TemplateId, crate::state::Kcc20State)> {
    use crate::state::FieldMap;
    let (args, tag, redeem) = crate::kcc1::split_invocation(signature_script).ok()?;
    let (version, companion) = decode_companion_versioned(&redeem).ok()??;
    let (contract, entry) = match companion {
        Companion::Minter(_) => ("PublicMint", "mint"),
        Companion::Seed(_) => ("TokenSeed", "create"),
    };
    let abi = app_abi(version);
    let c = abi.contracts.get(contract)?;
    let e = c.entry(entry)?;
    if e.dispatch_tag.into_bytes() != tag {
        return None;
    }
    let types: Vec<_> = e.params.iter().map(|p| p.ty.clone()).collect();
    let values = crate::kcc1::decode_arguments(&types, &crate::kcc1::contract_records(abi, c), args).ok()?;
    let silverscript_abi::ArtifactValue::Object(m) = values.first()? else { return None };
    Some((TemplateId::Kcc20PublicMint, crate::state::Kcc20State::from_values(m).ok()?))
}

/// True when `id` is a holder program of the published app: its profile declares the app's actors as genesis companions
/// (`genesis.companions` decoders `kcc20-public-mint/*` in `data/programs.json`), so its genesis may hold them and its
/// genesis record carries the minters' `mint_allowance`.
pub fn is_app_holder(id: TemplateId) -> bool {
    id.profile().is_some_and(|p| {
        p.genesis.companions.iter().any(|c| c.decoder.as_deref().is_some_and(|d| d.starts_with("kcc20-public-mint/")))
    })
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

    fn abi_of(v: IssuerVersion) -> SilAbiArtifact {
        let doc: Value = serde_json::from_str(v.artifact()).unwrap();
        serde_json::from_value(doc["sil_abi"].clone()).unwrap()
    }

    #[test]
    fn programs_are_the_published_ones() {
        assert_eq!(kcc20_sil_hash(), hex32(KCC20_SIL_HASH));
        let (m, s) = (public_mint(), token_seed());
        assert_eq!((m.prefix.len(), m.state_len, m.suffix.len()), (1, 150, 1_868));
        assert_eq!((s.prefix.len(), s.state_len, s.suffix.len()), (1, 132, 1_222));
    }

    /// The `27f8403` build: the same holder program, issuers with one more entry (so longer, with other hashes).
    #[test]
    fn the_27f8403_build_has_the_same_holder_program_and_new_issuers() {
        use IssuerVersion::{C8a0871, F27f8403};
        let (m, s) = (public_mint_of(F27f8403), token_seed_of(F27f8403));
        assert_eq!((m.prefix.len(), m.state_len, m.suffix.len()), (1, 150, 1_923));
        assert_eq!((s.prefix.len(), s.state_len, s.suffix.len()), (1, 132, 1_275));
        assert_eq!((hex32(PUBLIC_MINT_SIL_HASH_27F8403), hex32(TOKEN_SEED_SIL_HASH_27F8403)), (m.sil_hash, s.sil_hash));
        // four distinct issuer programs
        let hashes = [public_mint_of(C8a0871).sil_hash, token_seed_of(C8a0871).sil_hash, m.sil_hash, s.sil_hash];
        for (i, a) in hashes.iter().enumerate() {
            assert!(hashes.iter().skip(i + 1).all(|b| a != b));
        }
        // the KCC20 actor of the 27f8403 artifact is byte for byte the c8a0871 one (the holders do not change)
        let kcc20 = |v: IssuerVersion| {
            let doc: Value = serde_json::from_str(v.artifact()).unwrap();
            let c = doc["sil_abi"]["contracts"]["KCC20"]["compiled"].clone();
            (c["bytecode"].clone(), c["state_span"].clone(), c["template_hash"].clone())
        };
        assert_eq!(kcc20(C8a0871), kcc20(F27f8403));
        let recorded: Vec<u8> = kcc20(F27f8403).2.as_array().unwrap().iter().map(|b| b.as_u64().unwrap() as u8).collect();
        assert_eq!(recorded, hex32(KCC20_SIL_HASH).to_vec());
        // labels round trip
        for v in IssuerVersion::ALL {
            assert_eq!(IssuerVersion::from_label(v.label()), Some(v));
            assert!(v.label().ends_with(v.commit()));
        }
        assert_eq!(IssuerVersion::from_label("kcc20-reference@0000000"), None);
    }

    /// Each build decodes as itself; a program of one build holding the other build's own-template field is refused; the
    /// two builds never read as one another.
    #[test]
    fn both_builds_decode_as_themselves_and_not_as_each_other() {
        let m = MinterState { remaining: 5, mint_amount: 2, owner: [7; 32], extension_commitment: [0xee; 32] };
        let s = SeedState { owner: [9; 32], extension_commitment: [0xee; 32] };
        for v in IssuerVersion::ALL {
            let r = minter_redeem_of(v, &m).unwrap();
            assert_eq!(decode_companion_versioned(&r).unwrap(), Some((v, Companion::Minter(m.clone()))));
            let r = seed_redeem_of(v, &s);
            assert_eq!(decode_companion_versioned(&r).unwrap(), Some((v, Companion::Seed(s.clone()))));
            let other = IssuerVersion::ALL.into_iter().find(|o| *o != v).unwrap();
            let mut st = minter_state_bytes_of(v, &m).unwrap();
            st[34..66].copy_from_slice(&hex32(other.public_mint_hash()));
            assert!(decode_companion_versioned(&public_mint_of(v).redeem(&st)).unwrap_err().contains("template"));
            let mut st = seed_state_bytes_of(v, &s);
            st[34..66].copy_from_slice(&hex32(other.token_seed_hash()));
            assert!(decode_companion_versioned(&token_seed_of(v).redeem(&st)).unwrap_err().contains("template"));
        }
        // the same state under the other build is other bytes
        assert_ne!(minter_redeem_of(IssuerVersion::C8a0871, &m), minter_redeem_of(IssuerVersion::F27f8403, &m));
        assert_ne!(seed_redeem_of(IssuerVersion::C8a0871, &s), seed_redeem_of(IssuerVersion::F27f8403, &s));
        // the unversioned helpers are the c8a0871 build
        assert_eq!(minter_redeem(&m), minter_redeem_of(IssuerVersion::C8a0871, &m));
        assert_eq!(seed_redeem(&s), seed_redeem_of(IssuerVersion::C8a0871, &s));
    }

    /// The hand codec equals the ABI encoder of the published artifact for both actors, in both builds.
    #[test]
    fn state_codec_matches_the_artifact_abi() {
        for v in IssuerVersion::ALL {
            state_codec_matches_the_artifact_abi_of(v);
        }
    }

    fn state_codec_matches_the_artifact_abi_of(v: IssuerVersion) {
        let a = abi_of(v);
        let m = MinterState { remaining: 21_000_000, mint_amount: -5, owner: [7; 32], extension_commitment: [0xee; 32] };
        let fields = BTreeMap::from([
            ("gen__kcc20_template".to_string(), ArtifactValue::from(hex32(KCC20_SIL_HASH).to_vec())),
            ("gen__public_mint_template".to_string(), hex32(v.public_mint_hash()).to_vec().into()),
            ("remaining".to_string(), ArtifactValue::Int(m.remaining)),
            ("mint_amount".to_string(), ArtifactValue::Int(m.mint_amount)),
            ("owner".to_string(), m.owner.to_vec().into()),
            ("extension_commitment".to_string(), m.extension_commitment.to_vec().into()),
        ]);
        let c = &a.contracts["PublicMint"];
        let want = encode_runtime_state_script(&a, &c.runtime_state, &fields).unwrap();
        assert_eq!(minter_state_bytes_of(v, &m).unwrap(), want);
        let s = SeedState { owner: [9; 32], extension_commitment: [0; 32] };
        let fields = BTreeMap::from([
            ("gen__kcc20_template".to_string(), ArtifactValue::from(hex32(KCC20_SIL_HASH).to_vec())),
            ("gen__token_seed_template".to_string(), hex32(v.token_seed_hash()).to_vec().into()),
            ("owner".to_string(), s.owner.to_vec().into()),
            ("extension_commitment".to_string(), s.extension_commitment.to_vec().into()),
        ]);
        let c = &a.contracts["TokenSeed"];
        assert_eq!(seed_state_bytes_of(v, &s), encode_runtime_state_script(&a, &c.runtime_state, &fields).unwrap());
        // round trips
        assert_eq!(decode_companion(&minter_redeem_of(v, &m).unwrap()).unwrap(), Some(Companion::Minter(m.clone())));
        assert_eq!(decode_companion(&seed_redeem_of(v, &s)).unwrap(), Some(Companion::Seed(s)));
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
    fn mint_and_create_calls_reveal_the_holder_they_create() {
        for v in IssuerVersion::ALL {
            mint_and_create_calls_reveal_the_holder_they_create_of(v);
        }
    }

    fn mint_and_create_calls_reveal_the_holder_they_create_of(v: IssuerVersion) {
        use crate::state::{FieldMap, Kcc20State};
        let a = abi_of(v);
        let holder = Kcc20State::p2pk(250, [3; 32], [0xee; 32]);
        let tpl = template(TemplateId::Kcc20PublicMint);
        let (pre, suf) = (tpl.prefix.clone(), tpl.suffix.clone());
        let call = |contract: &str, entry: &str, redeem: &[u8]| {
            let args = vec![ArtifactValue::Object(holder.to_values()), pre.clone().into(), suf.clone().into()];
            let mut s = silverscript_abi::encode_contract_entry_sig_script(&a, contract, entry, &args).unwrap();
            s.extend_from_slice(&crate::script::push_data(redeem));
            s
        };
        let m = MinterState { remaining: 1_000, mint_amount: 500, owner: [7; 32], extension_commitment: [0xee; 32] };
        let minter = minter_redeem_of(v, &m).unwrap();
        let seed = seed_redeem_of(v, &SeedState { owner: [9; 32], extension_commitment: [0xee; 32] });
        assert_eq!(created_holder(&call("PublicMint", "mint", &minter)), Some((TemplateId::Kcc20PublicMint, holder.clone())));
        assert_eq!(created_holder(&mint_sigscript_of(v, &m, &holder).unwrap()), Some((TemplateId::Kcc20PublicMint, holder.clone())));
        assert_eq!(created_holder(&call("TokenSeed", "create", &seed)), Some((TemplateId::Kcc20PublicMint, holder.clone())));
        // the entry must be the actor's: a seed's `create` arguments under a minter redeem script carry the other dispatch tag
        assert_eq!(created_holder(&call("TokenSeed", "create", &minter)), None);
        // neither actor: a holder redeem script, or a minter whose template field names another program
        let mut foreign = minter.clone();
        foreign[2] ^= 1;
        assert_eq!(created_holder(&call("PublicMint", "mint", &foreign)), None);
        assert_eq!(created_holder(&call("PublicMint", "mint", &[0x51])), None);
        assert_eq!(created_holder(&[]), None);
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
