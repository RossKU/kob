//! Token registry: the pinned snapshot of facts behind a build (`registry/tokens.json`): the token programs it supports and
//! facts about tokens that anyone can re-derive from the chain.
//!
//! A *family* is a token program lineage KOB can escrow (`kcc20`: the draft KCC-20 layout, `kron`: KRON's 46-byte layout).
//! The registry pins, per family, the exact token programs (`templates`: template hash, sizes, slot limits and the escrow
//! owner types the order contracts use) and records facts about known tokens (`tokens`: identity, decimals, the genesis check).
//! A token's identity is `(family, covenant id, template hash, extension commitment)` (a token is its KIP-20
//! covenant id plus template hash, and fungibility only holds among equal extension commitments). Display is
//! `TICKER (abcd…1234)`; tickers are ASCII uppercase alphanumerics. Tickers are not unique: two entries may share a ticker or
//! use look-alike ones (`KRON`, `KR0N`), and neither is refused, because choosing which of two tokens may carry a ticker is a
//! curation decision KOB does not make (the covenant id beside the ticker tells them apart).
//!
//! **Two lists.** `templates` is the STRICT list, the program allowlist: only the token programs this build supports for trading
//! (`review_status: reviewed`: their engineering review is done and the order contracts escrow them) can carry a tradable token,
//! and a template that can freeze, seize or blacklist holders says so in `capabilities`. `tokens` is the OPEN list: every token
//! whose program is on the strict list is tradable and shown, whether or not it is in `tokens`. An entry adds facts only (the
//! decimals the chain does not state, the identity and genesis checks); KOB makes no judgement about a token: no listing, no
//! delisting, no official badge, no warning text written by people. Curation (official status, anti-impersonation, logos) belongs
//! to external community lists the user chooses. The warnings a token carries are derived from the recorded facts
//! ([`TokenWarning`]). Identity is always the covenant id plus the template hash, never the ticker.
//!
//! The format is specified by `registry/tokens.schema.json` (structure) and this validator (structure plus the cross-field
//! rules a schema cannot express). Everything is hand-validated on top of serde with `deny_unknown_fields`.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

use serde::{Deserialize, Serialize};

/// The registry shipped with this build (`registry/tokens.json`).
pub const DEFAULT_REGISTRY_JSON: &str = include_str!("../../../registry/tokens.json");
/// The JSON Schema of the format (`registry/tokens.schema.json`).
pub const SCHEMA_JSON: &str = include_str!("../../../registry/tokens.schema.json");

/// Lowercase hex sha256 of the embedded default registry ([`DEFAULT_REGISTRY_JSON`]) over its LF-normalised bytes: the hash
/// the mainnet deployment record pins (`contracts/deploy/mainnet/deployment.json` `registry.sha256`
/// and `GET /v1/tokens` `registry.sha256` of an executor that loads it).
pub fn default_registry_sha256() -> String {
    use sha2::{Digest, Sha256};
    let lf = DEFAULT_REGISTRY_JSON.replace("\r\n", "\n");
    crate::json::to_hex(&Sha256::digest(lf.as_bytes()))
}

/// Format version this module reads and writes.
pub const SCHEMA_VERSION: u32 = 1;
/// Token keys of older files that carried a per-token judgement (the listing state, the official badge, a maintainer's warning
/// text): [`Registry::parse`] reads and ignores them, whatever they hold, and nothing writes them. KOB makes no such judgement;
/// curation belongs to external community lists.
pub const LEGACY_TOKEN_KEYS: [&str; 3] = ["status", "official", "warning"];
/// Networks a registry can describe.
pub const NETWORKS: [&str; 3] = ["mainnet", "testnet-10", "devnet"];
/// Largest `decimals` a token may declare.
pub const MAX_DECIMALS: u8 = 18;
/// KRON's token program rejects any output above this amount (a custody, a delivery, a fill of a KRON order).
pub const KRON_MAX_OUTPUT_AMOUNT: i64 = 1_000_000_000;

/// KCC-20 state span length (`amount owner owner_scheme borrow_scheme borrow_guard extension_commitment`).
pub const KCC20_STATE_LEN: u32 = 112;
/// KRON token state span length (`owner id_type amount is_minter`).
pub const KRON_STATE_LEN: u32 = 46;

/// Token program family.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Family {
    /// Draft KCC-20 layout (112-byte state).
    Kcc20,
    /// KRON 46-byte layout.
    Kron,
}
impl Family {
    /// Every family, as spelled in the schema.
    pub const ALL: [&'static str; 2] = ["kcc20", "kron"];
    /// Schema spelling.
    pub fn as_str(self) -> &'static str {
        match self {
            Family::Kcc20 => "kcc20",
            Family::Kron => "kron",
        }
    }
    /// Required state span length of the family's token program.
    pub fn state_len(self) -> u32 {
        match self {
            Family::Kcc20 => KCC20_STATE_LEN,
            Family::Kron => KRON_STATE_LEN,
        }
    }
}

/// Review state of a pinned token program: whether this build supports trading it (the program allowlist). A statement about a
/// token PROGRAM (its bytes, owner types and limits), never about a token that runs it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ReviewStatus {
    /// Pinned, semantic review not done: this build does not trade it.
    PendingReview,
    /// Semantic review done (owner types, no admin path, conservation, limits): this build supports trading it.
    Reviewed,
}
impl ReviewStatus {
    /// Schema spellings.
    pub const ALL: [&'static str; 2] = ["pending-review", "reviewed"];
}

/// What a token program lets someone do beyond moving balances by their owners (the labels a strict-list template declares).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Capability {
    /// A mint authority (signature) can create supply on minter UTXOs.
    MintAuthority,
    /// Anyone can mint while the public-mint switch is on.
    PublicMint,
    /// Holders (or the program) can burn supply.
    Burn,
    /// An authority can freeze holder balances: spends of a frozen balance fail.
    Freeze,
    /// An authority can seize (move) holder balances without the owner's authorisation.
    Seize,
    /// An authority can blacklist addresses so transfers to or from them fail.
    Blacklist,
}
impl Capability {
    /// Schema spellings.
    pub const ALL: [&'static str; 6] = ["mint-authority", "public-mint", "burn", "freeze", "seize", "blacklist"];
    /// Schema spelling.
    pub fn as_str(self) -> &'static str {
        match self {
            Capability::MintAuthority => "mint-authority",
            Capability::PublicMint => "public-mint",
            Capability::Burn => "burn",
            Capability::Freeze => "freeze",
            Capability::Seize => "seize",
            Capability::Blacklist => "blacklist",
        }
    }
    /// True for the capabilities that can make escrowed tokens unspendable or take them: the maker (and the taker) then
    /// bear the issuer's discretion, which a UI must say before an order is placed.
    pub fn restricts_holders(self) -> bool {
        matches!(self, Capability::Freeze | Capability::Seize | Capability::Blacklist)
    }
}

/// Extension-commitment class of a KCC-20 token (KRON has none).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ExtensionClass {
    /// No extension: KCC-20 tokens carry an all-zero commitment, KRON tokens have no field.
    None,
    /// KOB fixed-supply standard token: one fixed commitment.
    FixedSupplyStandard,
    /// Anything else (needs its own review).
    Other,
}
impl ExtensionClass {
    /// Schema spellings.
    pub const ALL: [&'static str; 3] = ["none", "fixed-supply-standard", "other"];
}

/// Owner types the order contracts use for this program.
///
/// kcc20: `owner_scheme` 4 (covenant id), `borrow_scheme` 0. kron: `id_type` 2 (covenant id), `is_minter` 0, deliveries to
/// makers with `delivery_id_type` 3 (address presence, KRON-wallet compatible) or 0 (pubkey).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Escrow {
    /// KCC-20 owner scheme used for escrow.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub owner_scheme: Option<u8>,
    /// KCC-20 borrow scheme required on covenant-held states.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub borrow_scheme: Option<u8>,
    /// KRON owner id type used for escrow.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id_type: Option<u8>,
    /// KRON minter flag required on escrowed states.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub is_minter: Option<u8>,
    /// KRON owner id type of maker deliveries.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub delivery_id_type: Option<u8>,
}

/// Where a pinned program's bytes come from.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Source {
    /// Repo-relative path of the pinned artifact (`.json` silverc artifact) or raw program (`.bin`).
    pub path: String,
    /// Upstream provenance (URL or reference), if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub upstream: Option<String>,
    /// Free text.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

/// A pinned token program.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Template {
    /// Registry-local id, referenced by tokens.
    pub id: String,
    /// Family.
    pub family: Family,
    /// Template hash (silverscript `template_hash(prefix, suffix)`, blake3), lowercase hex, 32 bytes.
    pub template_hash: String,
    /// Template prefix length in bytes (bytes before the state span).
    pub prefix_len: u32,
    /// Template suffix length in bytes (bytes after the state span).
    pub suffix_len: u32,
    /// State span length in bytes (112 for kcc20, 46 for kron).
    pub state_len: u32,
    /// Maximum token inputs per transfer.
    pub max_token_inputs: u32,
    /// Maximum token outputs per transfer.
    pub max_token_outputs: u32,
    /// Owner types the order contracts use.
    pub escrow: Escrow,
    /// Review state (only `reviewed` templates are on the strict list: the programs this build supports for trading).
    pub review_status: ReviewStatus,
    /// What the program's authorities can do beyond owner-authorised transfers (see [`Capability`]); empty: nothing found.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub capabilities: Vec<Capability>,
    /// Declared risks of the program that are not a capability (missing hardening, signing assumptions), shown next to the
    /// capabilities; empty: none declared.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub risks: Vec<String>,
    /// Provenance.
    pub source: Source,
}

impl Template {
    /// True when the template is on the strict list (semantic review done: this build supports trading the program).
    pub fn is_strict(&self) -> bool {
        self.review_status == ReviewStatus::Reviewed
    }
    /// True when an authority can freeze, seize or blacklist holders (UIs label such tokens: orders can fail).
    pub fn restricts_holders(&self) -> bool {
        self.capabilities.iter().any(|c| c.restricts_holders())
    }
}

/// Display metadata (off-chain).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Display {
    /// One-line description.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// `https://` website.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub website: Option<String>,
    /// `https://` or `ipfs://` icon URL.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub icon: Option<String>,
    /// `https://` address of the token's X (Twitter) account.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub x: Option<String>,
    /// `https://` address of the token's Telegram.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub telegram: Option<String>,
    /// KCC-23 metadata JSON object (verbatim, optional; KOB does not depend on it).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kcc23: Option<serde_json::Value>,
}

/// A token KOB knows about.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Token {
    /// Ticker: 2-12 ASCII uppercase letters or digits.
    pub ticker: String,
    /// Display name.
    pub name: String,
    /// Family.
    pub family: Family,
    /// KIP-20 covenant id, lowercase hex, 32 bytes.
    pub covenant_id: String,
    /// Id of the pinned program this token runs.
    pub template_id: String,
    /// Extension commitment, lowercase hex, 32 bytes (kcc20 only; null for kron).
    pub extension_commitment: Option<String>,
    /// Extension-commitment class.
    pub extension_class: ExtensionClass,
    /// Decimals (<= 18).
    pub decimals: u8,
    /// Legacy (the protocol v2 lots): read from older registry files and ignored. Protocol v3 orders have no lot (any amount
    /// in base units) and no price tick; nothing reads or validates these, and the shipped registry no longer carries them.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lot_size: Option<i64>,
    /// Legacy (the protocol v2 price tick): read and ignored, like `lot_size`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tick: Option<i64>,
    /// Redundant slot limits; when present they must equal the template's.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_token_inputs: Option<u32>,
    /// Redundant slot limits; when present they must equal the template's.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_token_outputs: Option<u32>,
    /// The covenant id and template hash were checked against a chain UTXO of the token (the UTXO is an instance of the
    /// template, carrying the covenant id). A fact anyone can re-derive from the chain, not an endorsement of the token; the name
    /// is the format's since schema version 1.
    pub verified: bool,
    /// Genesis check: `true` = EVERY output of the token's genesis group was revealed and is an instance of its
    /// program ([`verify_genesis`]); `false` = checked and NOT clean (a hidden non-template output); absent = not checked.
    /// The covenant id commits only to the P2SH hashes of the genesis outputs, so an unchecked genesis may hide an output
    /// that later mints look-alike tokens or feeds a fake balance into a transfer. Any token without `true` carries the API
    /// warning [`TokenWarning::GenesisUnverified`]; trading never depends on it unless an operator opts in
    /// (`require_genesis_verified` of the executor).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub genesis_verified: Option<bool>,
    /// What the genesis check found on chain (the record behind `genesis_verified` and the live-mint-authority check);
    /// re-derived from the committed chain evidence by `kob registry verify-genesis`. Live mint-authority cells in it give the
    /// warning [`TokenWarning::LiveMintAuthority`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub genesis: Option<GenesisRecord>,
    /// Display metadata: a snapshot of what the token declares about itself (KCC-23) or of neutral facts, not KOB's word.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display: Option<Display>,
}

/// What a clean genesis check found (see [`verify_genesis`]); the registry records the result as `genesis_verified: true`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct GenesisReport {
    /// Transaction that created the token's covenant (the genesis group), lowercase hex.
    pub txid: String,
    /// Number of outputs in the genesis group (all checked).
    pub outputs: u32,
    /// Total token amount the genesis created (sum of the genesis holders' amounts).
    pub supply: i64,
    /// Genesis outputs that carry a mint authority, by output index: KRON outputs with `is_minter != 0`, and the
    /// `PublicMint` outputs of a token of the published `KCC20PublicMint` app ([`crate::public_mint`]). A KRON transaction
    /// can create a minter output only when it spends a minter (every non-minter token input refuses minter outputs), and
    /// only a `PublicMint` creates a `PublicMint`, so an empty list proves the token can never have a live minter. KaspaCom's
    /// KCC-20 program keeps its minter lanes behind the extension commitment, which this check cannot see: always empty
    /// for it.
    pub minter_outputs: Vec<u32>,
    /// Sum of the `remaining` allowance of the genesis `PublicMint` outputs (0 without one): what anyone may still mint.
    /// The token's maximum supply is `supply + mint_allowance`, fixed at genesis ([`crate::public_mint::GenesisSupply`]).
    #[serde(default)]
    pub mint_allowance: i64,
    /// Genesis `TokenSeed` outputs (zero-amount holder factories of the published app), by output index.
    #[serde(default)]
    pub seed_outputs: Vec<u32>,
    /// Genesis outputs that are opaque companions the program's profile declares (a launchpad's curve, pool or vault bound
    /// to the token's covenant id; `genesis.companions` of `data/programs.json`): no amount, no mint authority.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub companion_outputs: Vec<CompanionOutput>,
    /// The upstream build of the published public-mint app whose `PublicMint` / `TokenSeed` actors the genesis holds
    /// ([`crate::public_mint::IssuerVersion::label`], e.g. `kcc20-reference@27f8403`); absent when the genesis holds none of
    /// them. A genesis never mixes builds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub issuer: Option<String>,
}

/// An opaque companion output of a genesis group (see [`GenesisReport::companion_outputs`]).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CompanionOutput {
    /// Output index in the genesis transaction.
    pub index: u32,
    /// The role the profile declares (`curve`, `pool`, `vault`, `seed`).
    pub role: String,
}

impl GenesisReport {
    /// What the genesis created and may still mint ([`crate::public_mint::GenesisSupply`]).
    pub fn genesis_supply(&self) -> crate::public_mint::GenesisSupply {
        crate::public_mint::GenesisSupply { holders: self.supply, mint_allowance: self.mint_allowance }
    }
}

/// The registry's record of a token's genesis check (condition C1) and live-mint-authority check (condition C2), as
/// `kob registry verify-genesis` derives it from chain evidence (`registry/evidence/`).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GenesisRecord {
    /// Genesis transaction id (recomputed from the recorded transaction), lowercase hex.
    pub txid: String,
    /// DAA score of the block that accepted the genesis transaction.
    pub daa_score: u64,
    /// Output indices of the genesis group (every one checked against the program).
    pub outputs: Vec<u32>,
    /// Token amount the genesis created.
    pub supply: i64,
    /// Genesis outputs carrying a mint authority (see [`GenesisReport::minter_outputs`]).
    pub minter_outputs: Vec<u32>,
    /// Allowance of the genesis `PublicMint` outputs (see [`GenesisReport::mint_allowance`]); absent: the genesis holds no
    /// public minter. The maximum supply is `supply + mint_allowance`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mint_allowance: Option<i64>,
    /// The upstream build of the published public-mint app the genesis's `PublicMint` / `TokenSeed` actors are instances of
    /// (see [`GenesisReport::issuer`]); absent: the genesis holds none.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub issuer: Option<String>,
    /// Live mint-authority cells (`txid:index`) at `checked_at_daa`: empty = none (condition C2 holds), absent = not
    /// determined. A token with a live minter carries the warning [`TokenWarning::LiveMintAuthority`] (its supply can still grow).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub live_minters: Option<Vec<String>>,
    /// Virtual DAA score of the node the check read liveness from.
    pub checked_at_daa: u64,
    /// Where the bytes came from (node, explorer, issuer) and how they were checked.
    pub source: String,
}

/// A warning a token carries in the registry data and the API (`warnings`, snake_case strings). UIs show it next to the token.
/// Every code is derived from recorded chain facts by code, never written by people: KOB states what the chain shows about a
/// token, not what it thinks of it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TokenWarning {
    /// The registry does not say `genesis_verified: true`: not every genesis output of the token was checked
    /// against its program, so the issuer may hold a hidden non-template output able to mint look-alike tokens or feed a fake
    /// balance into a transfer. Every token on the open list without a registry entry carries it.
    GenesisUnverified,
    /// The token's genesis record lists live mint-authority cells (`genesis.live_minters` non-empty): whoever holds them can
    /// still mint, so the supply can still grow.
    LiveMintAuthority,
}
impl TokenWarning {
    /// All warning codes (API `warnings`).
    pub const ALL: [&'static str; 2] = ["genesis_unverified", "live_mint_authority"];
    /// API spelling (the executor's `/v1/tokens` `warnings`).
    pub fn as_str(self) -> &'static str {
        match self {
            TokenWarning::GenesisUnverified => "genesis_unverified",
            TokenWarning::LiveMintAuthority => "live_mint_authority",
        }
    }
}

/// Warnings of a token that has NO registry entry (a token of an open-list program the executor found on chain).
pub const OPEN_TOKEN_WARNINGS: [TokenWarning; 1] = [TokenWarning::GenesisUnverified];

impl Token {
    /// Warnings this token carries (API `warnings`), derived from its recorded facts: [`TokenWarning::GenesisUnverified`] unless
    /// `genesis_verified: true`, and [`TokenWarning::LiveMintAuthority`] when its genesis record lists a live minter.
    pub fn warnings(&self) -> Vec<TokenWarning> {
        let mut w = vec![];
        if self.genesis_verified != Some(true) {
            w.push(TokenWarning::GenesisUnverified);
        }
        if self.has_live_minter() {
            w.push(TokenWarning::LiveMintAuthority);
        }
        w
    }

    /// True when the genesis record lists a live mint-authority cell (`genesis.live_minters` non-empty).
    pub fn has_live_minter(&self) -> bool {
        self.genesis.as_ref().and_then(|g| g.live_minters.as_ref()).is_some_and(|l| !l.is_empty())
    }
}

/// The registry document.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Registry {
    /// Optional pointer to the schema.
    #[serde(rename = "$schema", default, skip_serializing_if = "Option::is_none")]
    pub schema: Option<String>,
    /// Format version (1).
    pub schema_version: u32,
    /// `mainnet`, `testnet-10` or `devnet`.
    pub network: String,
    /// Pinned token programs.
    pub templates: Vec<Template>,
    /// Known tokens.
    pub tokens: Vec<Token>,
}

/// Identity of a token: what a KOB order pins (covenant id, template hash, extension commitment) plus the family.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct AllowlistKey {
    /// Family.
    pub family: Family,
    /// KIP-20 covenant id.
    pub covenant_id: [u8; 32],
    /// Template hash of the token program.
    pub template_hash: [u8; 32],
    /// Extension commitment (kcc20 only).
    pub extension_commitment: Option<[u8; 32]>,
}

/// One row of the launch allowlist (plan section 2.3): what the listing rules and the executor need.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AllowlistEntry {
    /// Identity.
    pub key: AllowlistKey,
    /// Template suffix length.
    pub suffix_len: u32,
    /// Template prefix length.
    pub prefix_len: u32,
    /// Decimals.
    pub decimals: u8,
    /// Maximum token inputs per transfer.
    pub max_token_inputs: u32,
    /// Maximum token outputs per transfer.
    pub max_token_outputs: u32,
}

/// One validation finding.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ValidationError {
    /// `schema_version` is not [`SCHEMA_VERSION`].
    #[error("unsupported schema_version {0} (this build reads {SCHEMA_VERSION})")]
    UnsupportedSchemaVersion(u32),
    /// `network` is not one of [`NETWORKS`].
    #[error("unknown network `{0}`")]
    UnknownNetwork(String),
    /// The registry is for another network than the caller expects.
    #[error("registry is for network `{found}`, expected `{expected}`")]
    WrongNetwork {
        /// Network the caller expects.
        expected: String,
        /// Network the registry declares.
        found: String,
    },
    /// A hex field is not 32 bytes of lowercase hex.
    #[error("{path}: expected 64 lowercase hex characters (32 bytes), got `{value}`")]
    BadHex {
        /// Field path.
        path: String,
        /// Offending value (truncated).
        value: String,
    },
    /// Two templates share an id.
    #[error("duplicate template id `{0}`")]
    DuplicateTemplateId(String),
    /// Two templates share a template hash.
    #[error("templates `{a}` and `{b}` have the same template hash")]
    DuplicateTemplateHash {
        /// First template id.
        a: String,
        /// Second template id.
        b: String,
    },
    /// A template is internally inconsistent.
    #[error("template `{id}`: {reason}")]
    TemplateInvalid {
        /// Template id.
        id: String,
        /// What is wrong.
        reason: String,
    },
    /// A token references a template that does not exist.
    #[error("token {ticker}: unknown template `{template_id}`")]
    UnknownTemplate {
        /// Token ticker.
        ticker: String,
        /// Referenced template id.
        template_id: String,
    },
    /// A token's family differs from its template's.
    #[error("token {ticker}: family {token} but template `{template}` is family {template_family}")]
    FamilyMismatch {
        /// Token ticker.
        ticker: String,
        /// Token family.
        token: &'static str,
        /// Template id.
        template: String,
        /// Template family.
        template_family: &'static str,
    },
    /// Extension commitment / class rules violated.
    #[error("token {ticker}: {reason}")]
    Extension {
        /// Token ticker.
        ticker: String,
        /// What is wrong.
        reason: String,
    },
    /// Two tokens have the same `(family, covenant id, template hash, extension commitment)`.
    #[error("tokens {a} and {b} have the same identity (family, covenant id, template hash, extension commitment)")]
    DuplicateIdentity {
        /// First ticker.
        a: String,
        /// Second ticker.
        b: String,
    },
    /// Two tokens share a covenant id.
    #[error("tokens {a} and {b} share covenant id {covenant_id}")]
    DuplicateCovenantId {
        /// First ticker.
        a: String,
        /// Second ticker.
        b: String,
        /// The covenant id.
        covenant_id: String,
    },
    /// A ticker is malformed.
    #[error("ticker `{ticker}`: {reason}")]
    BadTicker {
        /// Offending ticker.
        ticker: String,
        /// What is wrong.
        reason: &'static str,
    },
    /// A display name or metadata field is malformed.
    #[error("token {ticker}: {reason}")]
    BadDisplay {
        /// Token ticker.
        ticker: String,
        /// What is wrong.
        reason: String,
    },
    /// `decimals` above [`MAX_DECIMALS`].
    #[error("token {ticker}: decimals {decimals} exceeds {MAX_DECIMALS}")]
    Decimals {
        /// Token ticker.
        ticker: String,
        /// Declared decimals.
        decimals: u8,
    },
    /// A token's own slot limits differ from its template's.
    #[error("token {ticker}: slot limits {token_in}/{token_out} differ from template `{template}` ({template_in}/{template_out})")]
    SlotLimitMismatch {
        /// Token ticker.
        ticker: String,
        /// Template id.
        template: String,
        /// Token's max inputs.
        token_in: u32,
        /// Token's max outputs.
        token_out: u32,
        /// Template max inputs.
        template_in: u32,
        /// Template max outputs.
        template_out: u32,
    },
    /// A template lists a capability twice.
    #[error("template `{id}`: capability `{capability}` listed twice")]
    DuplicateCapability {
        /// Template id.
        id: String,
        /// The capability.
        capability: &'static str,
    },
    /// A declared risk that is empty, longer than 512 characters or carries control / invisible characters (the schema's and
    /// the web client's bound).
    #[error("template `{id}`: risks must be texts of 1..=512 printable characters")]
    BadRisk {
        /// Template id.
        id: String,
    },
    /// `genesis_verified` / `genesis` record rules.
    #[error("token {ticker}: {reason}")]
    Genesis {
        /// Token ticker.
        ticker: String,
        /// What is wrong.
        reason: &'static str,
    },
}

/// Parse or validation failure.
#[derive(Debug, thiserror::Error)]
pub enum RegistryError {
    /// Not valid JSON for this format (includes unknown fields and wrong types).
    #[error("invalid registry json: {0}")]
    Json(#[from] serde_json::Error),
    /// Parsed, but one or more rules are violated.
    #[error("registry invalid: {}", .0.iter().map(|e| e.to_string()).collect::<Vec<_>>().join("; "))]
    Invalid(Vec<ValidationError>),
}

fn short(s: &str) -> String {
    if s.chars().count() > 24 {
        format!("{}…", s.chars().take(24).collect::<String>())
    } else {
        s.to_string()
    }
}

/// `<64 lowercase hex>:<u32>`
fn is_outpoint(s: &str) -> bool {
    s.split_once(':')
        .is_some_and(|(t, i)| parse_hex32(t).is_some() && !i.is_empty() && !i.starts_with('+') && i.parse::<u32>().is_ok())
}

/// Decode 32 bytes of lowercase hex.
pub fn parse_hex32(s: &str) -> Option<[u8; 32]> {
    if s.len() != 64 || !s.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)) {
        return None;
    }
    let mut out = [0u8; 32];
    for (i, o) in out.iter_mut().enumerate() {
        *o = u8::from_str_radix(&s[2 * i..2 * i + 2], 16).ok()?;
    }
    Some(out)
}

fn has_bad_char(s: &str) -> bool {
    s.chars().any(|c| {
        c.is_control()
            || matches!(c, '\u{200B}'..='\u{200F}' | '\u{202A}'..='\u{202E}' | '\u{2060}'..='\u{2064}' | '\u{2066}'..='\u{2069}' | '\u{FEFF}')
    })
}

impl Registry {
    /// Parse and fully validate (network not constrained).
    pub fn parse(json: &str) -> Result<Registry, RegistryError> {
        let r = Self::parse_unvalidated(json)?;
        r.validate().map_err(RegistryError::Invalid)?;
        Ok(r)
    }

    /// Parse only (unknown fields and wrong types are still refused). The per-token judgements older files carry
    /// ([`LEGACY_TOKEN_KEYS`]: `status`, `official`, `warning`) are read and ignored, whatever they hold: a registry of the
    /// earlier shape (an operator's testnet file, say) still loads and trades exactly what it would trade without them, and
    /// nothing of them is ever honoured or written back.
    pub fn parse_unvalidated(json: &str) -> Result<Registry, RegistryError> {
        let mut doc: serde_json::Value = serde_json::from_str(json)?;
        if let Some(tokens) = doc.get_mut("tokens").and_then(|t| t.as_array_mut()) {
            for t in tokens.iter_mut().filter_map(|t| t.as_object_mut()) {
                for k in LEGACY_TOKEN_KEYS {
                    t.remove(k);
                }
            }
        }
        Ok(serde_json::from_value(doc)?)
    }

    /// The registry shipped with this build.
    pub fn default_registry() -> Registry {
        Self::parse(DEFAULT_REGISTRY_JSON).expect("registry/tokens.json is valid (checked by tests)")
    }

    /// Validate and require a specific network.
    pub fn validate_for_network(&self, expected: &str) -> Result<(), Vec<ValidationError>> {
        let mut errs = self.validate().err().unwrap_or_default();
        if self.network != expected {
            errs.push(ValidationError::WrongNetwork { expected: expected.to_string(), found: self.network.clone() });
        }
        if errs.is_empty() {
            Ok(())
        } else {
            Err(errs)
        }
    }

    /// Check every rule; returns all findings.
    pub fn validate(&self) -> Result<(), Vec<ValidationError>> {
        let mut e: Vec<ValidationError> = vec![];
        if self.schema_version != SCHEMA_VERSION {
            e.push(ValidationError::UnsupportedSchemaVersion(self.schema_version));
        }
        if !NETWORKS.contains(&self.network.as_str()) {
            e.push(ValidationError::UnknownNetwork(self.network.clone()));
        }

        // ---- templates
        let mut by_id: BTreeMap<&str, &Template> = BTreeMap::new();
        let mut by_hash: BTreeMap<&str, &str> = BTreeMap::new();
        for t in &self.templates {
            let bad = |reason: String| ValidationError::TemplateInvalid { id: t.id.clone(), reason };
            if by_id.insert(&t.id, t).is_some() {
                e.push(ValidationError::DuplicateTemplateId(t.id.clone()));
            }
            if t.id.is_empty() || !t.id.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-' || b == b'_') {
                e.push(bad("id must be non-empty lowercase [a-z0-9_-]".into()));
            }
            if parse_hex32(&t.template_hash).is_none() {
                e.push(ValidationError::BadHex { path: format!("templates[{}].template_hash", t.id), value: short(&t.template_hash) });
            } else if let Some(prev) = by_hash.insert(&t.template_hash, &t.id) {
                e.push(ValidationError::DuplicateTemplateHash { a: prev.to_string(), b: t.id.clone() });
            }
            if t.state_len != t.family.state_len() {
                e.push(bad(format!("state_len {} but family {} has {}", t.state_len, t.family.as_str(), t.family.state_len())));
            }
            if t.suffix_len == 0 {
                e.push(bad("suffix_len must be > 0".into()));
            }
            // a program this build embeds: the entry states its profile's facts (data/programs.json). A KRON-codec program may
            // have a prefix (an entry selector): the order templates take the prefix length as a parameter.
            if let Err(m) = crate::profile::check_registry_template(t) {
                e.push(bad(format!("disagrees with the embedded program profile: {m}")));
            }
            if t.max_token_inputs == 0 || t.max_token_outputs == 0 || t.max_token_inputs > 64 || t.max_token_outputs > 64 {
                e.push(bad(format!("slot limits {}/{} must be within 1..=64", t.max_token_inputs, t.max_token_outputs)));
            }
            let s = &t.escrow;
            match t.family {
                Family::Kcc20 => {
                    if s.owner_scheme != Some(4) || s.borrow_scheme != Some(0) {
                        e.push(bad("kcc20 escrow must be owner_scheme 4 (covenant id) with borrow_scheme 0".into()));
                    }
                    if s.id_type.is_some() || s.is_minter.is_some() || s.delivery_id_type.is_some() {
                        e.push(bad("kcc20 escrow must not carry kron fields".into()));
                    }
                }
                Family::Kron => {
                    if s.id_type != Some(2) || s.is_minter != Some(0) {
                        e.push(bad("kron escrow must be id_type 2 (covenant id) with is_minter 0".into()));
                    }
                    if !matches!(s.delivery_id_type, Some(0) | Some(3)) {
                        e.push(bad("kron delivery_id_type must be 3 (address presence) or 0 (pubkey)".into()));
                    }
                    if s.owner_scheme.is_some() || s.borrow_scheme.is_some() {
                        e.push(bad("kron escrow must not carry kcc20 fields".into()));
                    }
                }
            }
            if t.source.path.is_empty() {
                e.push(bad("source.path is empty".into()));
            }
            let mut seen = BTreeSet::new();
            for c in &t.capabilities {
                if !seen.insert(*c) {
                    e.push(ValidationError::DuplicateCapability { id: t.id.clone(), capability: c.as_str() });
                }
            }
            if t.risks.iter().any(|r| r.is_empty() || r.chars().count() > 512 || has_bad_char(r)) {
                e.push(ValidationError::BadRisk { id: t.id.clone() });
            }
        }

        // ---- tokens
        let mut identities: BTreeMap<AllowlistKey, &str> = BTreeMap::new();
        let mut covs: BTreeMap<&str, &str> = BTreeMap::new();
        for (i, tok) in self.tokens.iter().enumerate() {
            let tk = tok.ticker.as_str();
            // ticker
            let tick_bad = |reason: &'static str| ValidationError::BadTicker { ticker: short(tk), reason };
            if tk.len() < 2 || tk.len() > 12 {
                e.push(tick_bad("length must be 2..=12"));
            } else if !tk.bytes().all(|b| b.is_ascii_uppercase() || b.is_ascii_digit()) {
                e.push(tick_bad("only ASCII uppercase letters and digits are allowed"));
            }
            // no uniqueness: a shared or look-alike ticker is not refused (the module documentation says why)
            // name / display
            let disp_bad = |reason: String| ValidationError::BadDisplay { ticker: short(tk), reason };
            if tok.name.trim().is_empty() || tok.name.chars().count() > 64 || has_bad_char(&tok.name) {
                e.push(disp_bad("name must be 1..=64 printable characters (no control, zero-width or bidi characters)".into()));
            }
            if let Some(d) = &tok.display {
                for (label, v) in [
                    ("description", &d.description),
                    ("website", &d.website),
                    ("icon", &d.icon),
                    ("x", &d.x),
                    ("telegram", &d.telegram),
                ] {
                    if let Some(v) = v {
                        if v.chars().count() > 512 || has_bad_char(v) {
                            e.push(disp_bad(format!("{label} is too long or has control characters")));
                        }
                    }
                }
                if let Some(w) = &d.website {
                    if !w.starts_with("https://") {
                        e.push(disp_bad("website must be an https:// URL".into()));
                    }
                }
                if let Some(i) = &d.icon {
                    if !(i.starts_with("https://") || i.starts_with("ipfs://")) {
                        e.push(disp_bad("icon must be an https:// or ipfs:// URL".into()));
                    }
                }
                for (label, v) in [("x", &d.x), ("telegram", &d.telegram)] {
                    if v.as_ref().is_some_and(|v| !v.starts_with("https://")) {
                        e.push(disp_bad(format!("{label} must be an https:// URL")));
                    }
                }
                if let Some(k) = &d.kcc23 {
                    if !k.is_object() {
                        e.push(disp_bad("kcc23 must be a JSON object".into()));
                    }
                }
            }
            // hex
            let cov = parse_hex32(&tok.covenant_id);
            if cov.is_none() {
                e.push(ValidationError::BadHex { path: format!("tokens[{tk}].covenant_id"), value: short(&tok.covenant_id) });
            }
            let ext = tok.extension_commitment.as_deref().map(parse_hex32);
            if let Some(None) = ext {
                e.push(ValidationError::BadHex {
                    path: format!("tokens[{tk}].extension_commitment"),
                    value: short(tok.extension_commitment.as_deref().unwrap_or("")),
                });
            }
            if let Some(prev) = covs.insert(&tok.covenant_id, tk) {
                e.push(ValidationError::DuplicateCovenantId {
                    a: prev.to_string(),
                    b: tk.to_string(),
                    covenant_id: short(&tok.covenant_id),
                });
            }
            // numbers
            if tok.decimals > MAX_DECIMALS {
                e.push(ValidationError::Decimals { ticker: tk.to_string(), decimals: tok.decimals });
            }
            // template
            let Some(tpl) = by_id.get(tok.template_id.as_str()) else {
                e.push(ValidationError::UnknownTemplate { ticker: tk.to_string(), template_id: tok.template_id.clone() });
                continue;
            };
            if tpl.family != tok.family {
                e.push(ValidationError::FamilyMismatch {
                    ticker: tk.to_string(),
                    token: tok.family.as_str(),
                    template: tpl.id.clone(),
                    template_family: tpl.family.as_str(),
                });
            }
            let ext_err = |reason: &str| ValidationError::Extension { ticker: tk.to_string(), reason: reason.to_string() };
            match tok.family {
                Family::Kron => {
                    if tok.extension_commitment.is_some() {
                        e.push(ext_err("kron tokens have no extension_commitment (must be null)"));
                    }
                    if tok.extension_class != ExtensionClass::None {
                        e.push(ext_err("kron tokens must have extension_class `none`"));
                    }
                }
                Family::Kcc20 => {
                    if tok.extension_commitment.is_none() {
                        e.push(ext_err(
                            "kcc20 tokens must carry an extension_commitment (fungibility only holds among equal commitments)",
                        ));
                    } else if tok.extension_class == ExtensionClass::None && ext.flatten().is_some_and(|x| x != [0u8; 32]) {
                        e.push(ext_err("extension_class `none` requires an all-zero extension_commitment"));
                    }
                }
            }
            let (tin, tout) = (tok.max_token_inputs, tok.max_token_outputs);
            if tin.is_some() || tout.is_some() {
                let (a, b) = (tin.unwrap_or(tpl.max_token_inputs), tout.unwrap_or(tpl.max_token_outputs));
                if (a, b) != (tpl.max_token_inputs, tpl.max_token_outputs) {
                    e.push(ValidationError::SlotLimitMismatch {
                        ticker: tk.to_string(),
                        template: tpl.id.clone(),
                        token_in: a,
                        token_out: b,
                        template_in: tpl.max_token_inputs,
                        template_out: tpl.max_token_outputs,
                    });
                }
            }
            // identity
            if let (Some(cov), Some(th)) = (cov, parse_hex32(&tpl.template_hash)) {
                let key =
                    AllowlistKey { family: tok.family, covenant_id: cov, template_hash: th, extension_commitment: ext.flatten() };
                if let Some(prev) = identities.insert(key, tk) {
                    e.push(ValidationError::DuplicateIdentity { a: prev.to_string(), b: tk.to_string() });
                }
            }
            // genesis check: a token without `genesis_verified: true` carries the warning (derived, `Token::warnings`)
            let gbad = |reason: &'static str| ValidationError::Genesis { ticker: tk.to_string(), reason };
            if tok.genesis_verified == Some(true) && !tok.verified {
                e.push(gbad("genesis_verified needs verified (the covenant id and template hash checked first)"));
            }
            // the genesis record (C1 evidence) and the live-mint-authority check (C2)
            if let Some(g) = &tok.genesis {
                if tok.genesis_verified.is_none() {
                    e.push(gbad("a genesis record needs genesis_verified (true: clean, false: not clean)"));
                }
                if parse_hex32(&g.txid).is_none() {
                    e.push(ValidationError::BadHex { path: format!("tokens[{i}].genesis.txid"), value: short(&g.txid) });
                }
                if g.outputs.is_empty() || !g.outputs.windows(2).all(|w| w[0] < w[1]) {
                    e.push(gbad("genesis.outputs must be a non-empty, strictly increasing list of output indices"));
                }
                if !g.minter_outputs.iter().all(|m| g.outputs.contains(m)) {
                    e.push(gbad("genesis.minter_outputs must be genesis outputs"));
                }
                if g.supply < 0 {
                    e.push(gbad("genesis.supply must not be negative"));
                }
                if let Some(a) = g.mint_allowance {
                    if a < 0 || g.supply.checked_add(a).is_none() {
                        e.push(gbad("genesis.mint_allowance must be non-negative and supply + mint_allowance must fit 64 bits"));
                    }
                    if g.minter_outputs.is_empty() {
                        e.push(gbad("genesis.mint_allowance needs the genesis minter outputs that hold it"));
                    }
                }
                if g.issuer.as_deref().is_some_and(|l| crate::public_mint::IssuerVersion::from_label(l).is_none()) {
                    e.push(gbad("genesis.issuer must name a pinned upstream build of the public-mint app (kcc20-reference@<commit>)"));
                }
                if g.source.is_empty() || g.source.chars().count() > 512 || has_bad_char(&g.source) {
                    e.push(gbad("genesis.source must be 1-512 characters without control, zero-width or bidi characters"));
                }
                // a live minter is a recorded fact: the token carries the derived warning `live_mint_authority`
                if g.live_minters.as_ref().is_some_and(|live| !live.iter().all(|o| is_outpoint(o))) {
                    e.push(gbad("genesis.live_minters entries must be <txid>:<index>"));
                }
            }
        }
        if e.is_empty() {
            Ok(())
        } else {
            Err(e)
        }
    }

    /// Template by id.
    pub fn template(&self, id: &str) -> Option<&Template> {
        self.templates.iter().find(|t| t.id == id)
    }

    /// Slot limits `(max inputs, max outputs)` of a token (from its template).
    pub fn slot_limits(&self, token: &Token) -> Option<(u32, u32)> {
        self.template(&token.template_id).map(|t| (t.max_token_inputs, t.max_token_outputs))
    }

    /// Identity of a token: `(family, covenant id, template hash, extension commitment)`.
    pub fn allowlist_key(&self, token: &Token) -> Result<AllowlistKey, ValidationError> {
        let tpl = self.template(&token.template_id).ok_or_else(|| ValidationError::UnknownTemplate {
            ticker: token.ticker.clone(),
            template_id: token.template_id.clone(),
        })?;
        let hex = |path: &str, s: &str| {
            parse_hex32(s).ok_or_else(|| ValidationError::BadHex { path: format!("tokens[{}].{path}", token.ticker), value: short(s) })
        };
        Ok(AllowlistKey {
            family: token.family,
            covenant_id: hex("covenant_id", &token.covenant_id)?,
            template_hash: hex("template.template_hash", &tpl.template_hash)?,
            extension_commitment: match &token.extension_commitment {
                Some(s) => Some(hex("extension_commitment", s)?),
                None => None,
            },
        })
    }

    /// Find a token by identity.
    pub fn find(&self, key: &AllowlistKey) -> Option<&Token> {
        self.tokens.iter().find(|t| self.allowlist_key(t).is_ok_and(|k| &k == key))
    }

    /// Allowlist rows of every token entry whose program is on the strict list, the programs this build supports (call after
    /// `validate`). Entries on a program pending review give no row: nothing of such a program is tradable. A row is facts only
    /// (identity, layout, decimals); a token of a supported program trades whether or not it has a row (the open list).
    pub fn allowlist(&self) -> Vec<AllowlistEntry> {
        self.tokens
            .iter()
            .filter_map(|t| {
                let tpl = self.template(&t.template_id).filter(|tpl| tpl.is_strict())?;
                Some(AllowlistEntry {
                    key: self.allowlist_key(t).ok()?,
                    suffix_len: tpl.suffix_len,
                    prefix_len: tpl.prefix_len,
                    decimals: t.decimals,
                    max_token_inputs: tpl.max_token_inputs,
                    max_token_outputs: tpl.max_token_outputs,
                })
            })
            .collect()
    }

    /// The strict template list: the templates whose review is done (a token on any of them is tradable).
    pub fn strict_templates(&self) -> impl Iterator<Item = &Template> {
        self.templates.iter().filter(|t| t.is_strict())
    }

    /// The strict template with this hash, if any.
    pub fn strict_template_by_hash(&self, hash: &[u8; 32]) -> Option<&Template> {
        self.strict_templates().find(|t| parse_hex32(&t.template_hash).as_ref() == Some(hash))
    }

    /// Set of distinct families in use.
    pub fn families(&self) -> BTreeSet<Family> {
        self.tokens.iter().map(|t| t.family).collect()
    }
}

/// `TICKER (abcd…1234)`: ticker and short covenant id, so tokens that share a ticker are told apart. No standing or badge: KOB
/// makes no judgement about a token. Mirrors `displayName` of the web registry.
pub fn display_name(token: &Token) -> String {
    let c = &token.covenant_id;
    let short_id = if c.len() >= 8 { format!("{}…{}", &c[..4], &c[c.len() - 4..]) } else { c.clone() };
    format!("{} ({})", token.ticker, short_id)
}

impl fmt::Display for Token {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&display_name(self))
    }
}

/// One output of a token's genesis group as the chain shows it, with the redeem script behind its P2SH script public key
/// (revealed when the output was spent, or published by the issuer; either way it is checked against the hash).
#[derive(Clone, Debug)]
pub struct GenesisOutput {
    /// Output index in the genesis transaction.
    pub index: u32,
    /// Output value (sompi).
    pub value: u64,
    /// Output script public key.
    pub script_public_key: kaspa_consensus_core::tx::ScriptPublicKey,
    /// Redeem script behind the P2SH script public key; `None`: not revealed (the check fails).
    pub redeem_script: Option<Vec<u8>>,
}

/// Why a genesis check failed (see [`verify_genesis`]).
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum GenesisError {
    /// The template is not a program this build embeds.
    #[error("template `{0}` is not a pinned token program of this build")]
    UnknownProgram(String),
    /// Malformed input (hex, empty or unordered outputs).
    #[error("{0}")]
    Malformed(String),
    /// The outputs given are not the whole genesis group of this covenant id.
    #[error("the covenant id recomputed from the outpoint and the {0} output(s) given is not the token's: they are not its complete genesis group")]
    NotTheGroup(usize),
    /// A genesis output whose redeem script is unknown.
    #[error("genesis output {0}: redeem script not revealed")]
    Unrevealed(u32),
    /// A redeem script that does not hash to its output.
    #[error("genesis output {0}: the redeem script does not match the P2SH script public key")]
    WrongRedeem(u32),
    /// A genesis output that is not an instance of the token's program (genesis contamination).
    #[error("genesis output {0}: not an instance of the token program `{1}` (hidden non-template output)")]
    NotTemplate(u32, String),
    /// A genesis output whose state does not decode, or a negative / overflowing amount.
    #[error("genesis output {0}: bad token state: {1}")]
    BadState(u32, String),
    /// A genesis output whose extension commitment is not the token's (the one named, or the group's first output's): a
    /// second class of token under the same covenant id, not fungible with the first and not part of its supply.
    #[error("genesis output {0}: extension commitment {1} is not the token's ({2})")]
    ExtensionCommitment(u32, String, String),
}

/// Checks EVERY output of a token's genesis group against its pinned program: the step behind
/// `genesis_verified: true` (without it a token carries the warning [`TokenWarning::GenesisUnverified`]).
///
/// A KIP-20 covenant id commits to the genesis outpoint and to the index, value and script public key of every output of the
/// genesis group, and nothing else: for P2SH outputs only to the hash of the redeem script. A token program reads balances
/// and authority from any input carrying its covenant id, so one hidden non-template output in the genesis (a "backdoor"
/// script) can later mint look-alike tokens or feed a fake balance into a genuine transfer, and the template alone cannot
/// show it. This function recomputes the covenant id from `authorizing_outpoint` (the previous outpoint of the input that
/// authorised the group) and `outputs` (so `outputs` must be the complete group), then requires every output to be a P2SH
/// of a revealed redeem script that is an instance of `template` (prefix, state span, suffix; template hash) with a
/// decodable state, all of one extension commitment (fungibility holds only among equal commitments: a second commitment
/// in the genesis group is a second class of token under the same covenant id). Returns what it found (record
/// `genesis_verified: true` for the token). [`verify_genesis_of`] also names the commitment the token must carry.
///
/// A token of the published `KCC20PublicMint` app (template `kcc20-ref-public-mint`) may also hold the app's issuance
/// actors in its genesis, as upstream recommends ([`crate::public_mint`]): `PublicMint` minters and `TokenSeed` seeds of the
/// app's exact programs, whose template fields name the app's own programs and whose extension commitment is the
/// token's. A minter must have `remaining >= 0` and `mint_amount >= 1`. The report then lists them
/// ([`GenesisReport::minter_outputs`], [`GenesisReport::seed_outputs`]) and sums their allowance
/// ([`GenesisReport::mint_allowance`]); anything else in the group is refused as for every program.
pub fn verify_genesis(
    template: &Template,
    covenant_id: &str,
    genesis_txid: &str,
    authorizing_outpoint: ([u8; 32], u32),
    outputs: &[GenesisOutput],
) -> Result<GenesisReport, GenesisError> {
    verify_genesis_of(template, covenant_id, genesis_txid, authorizing_outpoint, outputs, None)
}

/// [`verify_genesis`] for a token whose extension commitment is known (the registry entry's `extension_commitment`; all
/// zero for KRON, which has none): every output of the group must carry exactly `extension_commitment`. `None`: every
/// output must carry the first output's.
pub fn verify_genesis_of(
    template: &Template,
    covenant_id: &str,
    genesis_txid: &str,
    authorizing_outpoint: ([u8; 32], u32),
    outputs: &[GenesisOutput],
    extension_commitment: Option<[u8; 32]>,
) -> Result<GenesisReport, GenesisError> {
    let hash = parse_hex32(&template.template_hash).ok_or_else(|| GenesisError::Malformed("bad template hash".into()))?;
    let program = crate::artifacts::token_template_by_hash(&hash).ok_or_else(|| GenesisError::UnknownProgram(template.id.clone()))?;
    let profile = program.id.profile().ok_or_else(|| GenesisError::UnknownProgram(template.id.clone()))?;
    verify_genesis_with(template, profile, covenant_id, genesis_txid, authorizing_outpoint, outputs, extension_commitment)
}

/// [`verify_genesis_of`] under an explicit program profile: its `genesis.companions` are the outputs beside the token's own
/// that the group may hold (the embedded profile of the template's program is what [`verify_genesis_of`] passes; a review
/// passes a candidate profile to try a launchpad's genesis shape before the profile is committed).
pub fn verify_genesis_with(
    template: &Template,
    profile: &crate::profile::ProgramProfile,
    covenant_id: &str,
    genesis_txid: &str,
    authorizing_outpoint: ([u8; 32], u32),
    outputs: &[GenesisOutput],
    extension_commitment: Option<[u8; 32]>,
) -> Result<GenesisReport, GenesisError> {
    use kaspa_consensus_core::hashing::covenant_id::covenant_id as cov_id_of;
    use kaspa_consensus_core::tx::{TransactionId, TransactionOutpoint, TransactionOutput};

    let hash = parse_hex32(&template.template_hash).ok_or_else(|| GenesisError::Malformed("bad template hash".into()))?;
    if hash != profile.hash() {
        return Err(GenesisError::Malformed(format!("profile {} is not the program of template {}", profile.name, template.id)));
    }
    let program = crate::artifacts::token_template_by_hash(&hash).ok_or_else(|| GenesisError::UnknownProgram(template.id.clone()))?;
    let want =
        parse_hex32(covenant_id).ok_or_else(|| GenesisError::Malformed("covenant id must be 64 lowercase hex characters".into()))?;
    parse_hex32(genesis_txid).ok_or_else(|| GenesisError::Malformed("genesis txid must be 64 lowercase hex characters".into()))?;
    if outputs.is_empty() {
        return Err(GenesisError::Malformed("a genesis group has at least one output".into()));
    }
    if !outputs.windows(2).all(|w| w[0].index < w[1].index) {
        return Err(GenesisError::Malformed("genesis outputs must be in strictly increasing index order".into()));
    }
    let outpoint =
        TransactionOutpoint { transaction_id: TransactionId::from_bytes(authorizing_outpoint.0), index: authorizing_outpoint.1 };
    let txouts: Vec<TransactionOutput> = outputs
        .iter()
        .map(|o| TransactionOutput { value: o.value, script_public_key: o.script_public_key.clone(), covenant: None })
        .collect();
    let id = cov_id_of(outpoint, outputs.iter().zip(&txouts).map(|(o, t)| (o.index, t)));
    if id.as_bytes() != want {
        return Err(GenesisError::NotTheGroup(outputs.len()));
    }
    let mut supply: i64 = 0;
    let mut minter_outputs = vec![];
    let mut seed_outputs = vec![];
    let mut companion_outputs = vec![];
    let mut mint_allowance: i64 = 0;
    // the build of the public-mint app the issuer actors are from (a genesis never mixes the two)
    let mut issuer: Option<crate::public_mint::IssuerVersion> = None;
    // the token's one extension commitment: the one named, else the first output's
    let mut ext = extension_commitment;
    for o in outputs {
        let redeem = o.redeem_script.as_ref().ok_or(GenesisError::Unrevealed(o.index))?;
        if crate::script::p2sh_spk(redeem) != o.script_public_key {
            return Err(GenesisError::WrongRedeem(o.index));
        }
        let Some(state) = program.state_of(redeem) else {
            // a companion the program's profile declares (the issuance actors of the published public-mint app, a
            // launchpad's curve or pool bound to the token's id); anything else is genesis contamination
            let found = crate::profile::match_companion(profile, redeem).map_err(|e| GenesisError::BadState(o.index, e))?;
            let c = match found.ok_or_else(|| GenesisError::NotTemplate(o.index, template.id.clone()))? {
                crate::profile::CompanionMatch::Decoded(v, c) => {
                    if let Some(first) = issuer.filter(|f| *f != v) {
                        return Err(GenesisError::BadState(
                            o.index,
                            format!(
                                "the genesis mixes issuer builds ({} and {}): its actors must all be of one build",
                                first.label(),
                                v.label()
                            ),
                        ));
                    }
                    issuer = Some(v);
                    c
                }
                crate::profile::CompanionMatch::Opaque(role) => {
                    companion_outputs.push(CompanionOutput { index: o.index, role: role.as_str().to_string() });
                    continue;
                }
            };
            let want = *ext.get_or_insert(c.extension_commitment());
            if c.extension_commitment() != want {
                return Err(GenesisError::ExtensionCommitment(
                    o.index,
                    crate::json::to_hex(&c.extension_commitment()),
                    crate::json::to_hex(&want),
                ));
            }
            match c {
                crate::public_mint::Companion::Minter(m) => {
                    if m.remaining < 0 || m.mint_amount < 1 {
                        return Err(GenesisError::BadState(
                            o.index,
                            format!("PublicMint with remaining {} and mint_amount {}: not a minter", m.remaining, m.mint_amount),
                        ));
                    }
                    mint_allowance = mint_allowance
                        .checked_add(m.remaining)
                        .ok_or_else(|| GenesisError::BadState(o.index, "mint allowance overflows".into()))?;
                    minter_outputs.push(o.index);
                }
                crate::public_mint::Companion::Seed(_) => seed_outputs.push(o.index),
            }
            continue;
        };
        let st = crate::state::TokenState::decode_with(program, state).map_err(|e| GenesisError::BadState(o.index, e.to_string()))?;
        if st.amount() < 0 {
            return Err(GenesisError::BadState(o.index, "negative amount".into()));
        }
        let want = *ext.get_or_insert(st.extension());
        if st.extension() != want {
            return Err(GenesisError::ExtensionCommitment(o.index, crate::json::to_hex(&st.extension()), crate::json::to_hex(&want)));
        }
        if let crate::state::TokenState::Kron(k) = &st {
            if k.is_minter != 0 {
                minter_outputs.push(o.index);
            }
        }
        supply = supply.checked_add(st.amount()).ok_or_else(|| GenesisError::BadState(o.index, "supply overflows".into()))?;
    }
    if supply.checked_add(mint_allowance).is_none() {
        return Err(GenesisError::BadState(outputs[0].index, "supply plus mint allowance overflows".into()));
    }
    Ok(GenesisReport {
        txid: genesis_txid.to_string(),
        outputs: outputs.len() as u32,
        supply,
        minter_outputs,
        mint_allowance,
        seed_outputs,
        companion_outputs,
        issuer: issuer.map(|v| v.label().to_string()),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{json, Value};

    const EXAMPLE: &str = include_str!("../../../registry/tokens.example.json");

    fn example() -> Value {
        serde_json::from_str(EXAMPLE).unwrap()
    }
    fn check(mut f: impl FnMut(&mut Value)) -> Result<Registry, RegistryError> {
        let mut v = example();
        f(&mut v);
        Registry::parse(&v.to_string())
    }
    fn errs(r: Result<Registry, RegistryError>) -> Vec<ValidationError> {
        match r {
            Err(RegistryError::Invalid(e)) => e,
            Err(RegistryError::Json(e)) => panic!("expected validation errors, got json error: {e}"),
            Ok(_) => panic!("expected validation errors, registry was accepted"),
        }
    }
    fn tok(v: &mut Value, i: usize) -> &mut Value {
        &mut v["tokens"][i]
    }
    /// A genesis record that found no live mint authority (no derived warning besides what genesis_verified decides).
    fn genesis_rec() -> Value {
        json!({"txid": "ab".repeat(32), "daa_score": 10, "outputs": [1, 2], "supply": 1000, "minter_outputs": [],
            "live_minters": [], "checked_at_daa": 20, "source": "test"})
    }

    #[test]
    fn default_and_example_registries_validate() {
        let d = Registry::default_registry();
        assert_eq!(d.network, "mainnet");
        // the KRON family from the 2026-09-29 census: seven graduated tokens and KDIST, facts only: identity checked against a chain
        // UTXO, and the genesis check (2026-10-03, `registry/evidence/mainnet-genesis.json`, `genesis_evidence`): every genesis output
        // checked on mainnet data (C1), no genesis minter output so no live mint authority (C2), hence no derived warning. No entry
        // carries a judgement (no listing state, no official flag, no written warning: the format has none)
        assert_eq!(d.tokens.len(), 8);
        assert!(d.tokens.iter().all(|t| t.family == Family::Kron
            && t.verified
            && t.genesis_verified == Some(true)
            && t.genesis.as_ref().is_some_and(|g| g.minter_outputs.is_empty() && g.live_minters == Some(vec![]))
            && t.warnings().is_empty()));
        assert!(d.tokens.iter().all(|t| t
            .display
            .as_ref()
            .and_then(|x| x.description.as_deref())
            .is_some_and(|s| !s.contains("listed") && !s.contains("official"))));
        assert_eq!(d.allowlist().len(), 8);
        assert_eq!(d.templates.len(), 6);
        // internal review of the two KRON programs (2026-09-30, `review_b2_kron.rs`): reviewed with conditions (per-token genesis check and
        // no live minter, facts each token shows as derived warnings); the reference KCC-20 3/3 program, KOB's standard one, is reviewed
        // (internal review only, maintainer decision 2026-10-12: tradable on mainnet), and so is the reference's published public-mint
        // build (2026-10-13, the same permissionless KCC-20 policy); the 8/8 prototype and KaspaCom's are pending
        let strict: Vec<&str> = d.strict_templates().map(|t| t.id.as_str()).collect();
        assert_eq!(strict, ["kcc20-ref-3x3", "kron-2433", "kron-2732", "kcc20-ref-public-mint"]);
        let r3 = d.template("kcc20-ref-3x3").unwrap();
        assert!(r3.risks.iter().any(|r| r.starts_with("Reviewed (internal review only") && r.contains("verify_genesis")));
        for id in ["kcc20-ref-8x8", "kcc20-kaspacom-0-2-5"] {
            assert_eq!(d.template(id).unwrap().review_status, ReviewStatus::PendingReview, "{id}");
        }
        for id in ["kron-2433", "kron-2732"] {
            let t = d.template(id).unwrap();
            assert!(
                t.risks.iter().any(|r| r.starts_with("Reviewed with conditions")
                    && r.contains("verify_genesis")
                    && r.contains("live is_minter")),
                "{id}"
            );
        }
        let kc = d.template("kcc20-kaspacom-0-2-5").unwrap();
        assert_eq!(kc.review_status, ReviewStatus::PendingReview);
        assert!(kc
            .risks
            .iter()
            .any(|r| r.starts_with("set_public_mint_active admits other covenant inputs") && r.contains("r_kc_05")));
        assert!(
            kc.risks.iter().any(|r| r.starts_with("K-1 limits")),
            "K-1 confirmed by engine PoC: the template stays pending-review"
        );
        assert!(d.templates.iter().all(|t| !t.restricts_holders()), "no shipped program can freeze, seize or blacklist");
        // declared capabilities and risks
        for t in d.templates.iter().filter(|t| t.family == Family::Kron) {
            assert!(t.capabilities.contains(&Capability::MintAuthority), "{}: KRON's is_minter path is a mint authority", t.id);
            assert!(t.risks.iter().any(|r| r.contains("id_type 3")), "{}", t.id);
        }
        assert!(d.template("kron-2433").unwrap().risks.iter().any(|r| r.starts_with("Missing hardening")));
        assert!(!d.template("kron-2732").unwrap().risks.iter().any(|r| r.starts_with("Missing hardening")));
        assert!(d.templates.iter().all(|t| t.risks.iter().any(|r| r.starts_with("Genesis contamination"))));
        // the published public-mint build: its KCC-1 actor-type handle (34-byte prefix with the context field), reviewed (internal
        // review only, 2026-10-13): tradable on mainnet, a live PublicMint bounded by the genesis allowance
        let pm = d.template("kcc20-ref-public-mint").unwrap();
        assert_eq!(pm.review_status, ReviewStatus::Reviewed);
        assert!(pm
            .risks
            .iter()
            .any(|r| r.starts_with("Reviewed (internal review only") && r.contains("mint_allowance") && r.contains("verify_genesis")));
        assert!(d.strict_template_by_hash(&parse_hex32(&pm.template_hash).unwrap()).is_some());
        assert_eq!((pm.prefix_len, pm.state_len, pm.suffix_len, pm.max_token_inputs, pm.max_token_outputs), (34, 112, 3_885, 3, 3));
        assert_eq!(pm.capabilities, vec![Capability::PublicMint]);
        assert!(pm.risks.iter().any(|r| r.starts_with("Issuance lives beside the program") && r.contains("verify-genesis")));
        let ph = parse_hex32(&pm.template_hash).unwrap();
        assert_eq!(crate::artifacts::token_template_by_hash(&ph).map(|t| t.id), Some(crate::artifacts::TemplateId::Kcc20PublicMint));
        let kc = d.template("kcc20-kaspacom-0-2-5").unwrap();
        assert_eq!((kc.max_token_inputs, kc.suffix_len), (8, 25_439));
        assert_eq!(kc.capabilities, vec![Capability::MintAuthority, Capability::PublicMint, Capability::Burn]);
        let hash = parse_hex32(&kc.template_hash).unwrap();
        assert!(d.strict_template_by_hash(&hash).is_none(), "a pending template is not on the strict list");
        let ex = Registry::parse(EXAMPLE).unwrap();
        assert_eq!(ex.network, "testnet-10");
        assert_eq!(ex.tokens.len(), 2);
        // the example carries the same pinned templates, reviewed (a fixture of a reviewed strict list; the shipped file holds them)
        let reviewed: Vec<Template> =
            d.templates.iter().map(|t| Template { review_status: ReviewStatus::Reviewed, ..t.clone() }).collect();
        assert_eq!(ex.templates, reviewed, "example carries the same pinned templates");
        assert_eq!(ex.strict_templates().count(), 6);
        assert!(ex.validate_for_network("testnet-10").is_ok());
        // both example entries are on reviewed (supported) programs: both give an allowlist row, unchecked as they are
        assert_eq!(ex.allowlist().len(), 2);
    }

    #[test]
    fn strict_list_capabilities_and_warnings() {
        // a template that can freeze is labelled, and only reviewed templates are on the strict list
        let r = check(|v| {
            for t in v["templates"].as_array_mut().unwrap() {
                t["review_status"] = json!("reviewed");
            }
            v["templates"][0]["capabilities"] = json!(["freeze", "blacklist"]);
            v["templates"][1]["review_status"] = json!("pending-review");
        })
        .unwrap();
        assert!(r.templates[0].restricts_holders() && !r.templates[1].is_strict());
        assert_eq!(r.strict_templates().count(), r.templates.len() - 1);
        let h1 = parse_hex32(&r.templates[1].template_hash).unwrap();
        assert!(r.strict_template_by_hash(&h1).is_none());
        // unknown / duplicate capabilities are refused
        assert!(matches!(check(|v| v["templates"][0]["capabilities"] = json!(["teleport"])), Err(RegistryError::Json(_))));
        let e = errs(check(|v| v["templates"][0]["capabilities"] = json!(["burn", "burn"])));
        assert!(e.iter().any(|x| matches!(x, ValidationError::DuplicateCapability { .. })));
        // risks are 1..=512 printable characters (schema, web client)
        for bad in [json!([""]), json!(["x".repeat(513)]), json!(["a\u{202e}b"])] {
            let e = errs(check(|v| v["templates"][0]["risks"] = bad.clone()));
            assert!(e.iter().any(|x| matches!(x, ValidationError::BadRisk { .. })), "{bad}");
        }
        assert!(check(|v| v["templates"][0]["risks"] = json!(["x".repeat(512)])).is_ok());
        // the warnings are derived from the facts: a checked genesis without a live minter carries none, an entry without the check
        // carries what an open-list token without an entry carries
        let ok = check(|v| {
            tok(v, 0)["verified"] = json!(true);
            tok(v, 0)["genesis_verified"] = json!(true);
            tok(v, 0)["genesis"] = genesis_rec();
        })
        .unwrap();
        assert!(ok.tokens[0].warnings().is_empty() && ok.tokens[1].warnings() == OPEN_TOKEN_WARNINGS.to_vec());
        // the judgements older files carry are read and ignored, whatever they hold (an operator's file of the earlier shape still
        // loads): the result is the registry without them, and nothing of them is written back
        let plain = Registry::parse(EXAMPLE).unwrap();
        for (field, value) in [
            ("official", json!(true)),
            ("status", json!("delisted")),
            ("status", json!("no such state")),
            ("warning", json!("evil\u{202E}")),
            ("warning", json!(17)),
        ] {
            let r = check(|v| tok(v, 0)[field] = value.clone()).unwrap_or_else(|e| panic!("{field}: {e}"));
            assert_eq!(r, plain, "{field} = {value} is ignored");
            assert!(serde_json::to_value(&r.tokens[0]).unwrap().get(field).is_none());
        }
        // a direct token parse (not a registry document) knows no such field
        let mut t = example()["tokens"][0].clone();
        t["official"] = json!(true);
        assert!(serde_json::from_value::<Token>(t).is_err());
    }

    #[test]
    fn roundtrip_is_lossless() {
        let r = Registry::parse(EXAMPLE).unwrap();
        let again: Registry = serde_json::from_str(&serde_json::to_string(&r).unwrap()).unwrap();
        assert_eq!(r, again);
    }

    #[test]
    fn wrong_network() {
        let r = Registry::parse(EXAMPLE).unwrap();
        let e = r.validate_for_network("mainnet").unwrap_err();
        assert!(
            matches!(&e[..], [ValidationError::WrongNetwork { expected, found }] if expected == "mainnet" && found == "testnet-10")
        );
        let e = errs(check(|v| v["network"] = json!("moon")));
        assert!(e.contains(&ValidationError::UnknownNetwork("moon".into())));
        let e = errs(check(|v| v["schema_version"] = json!(2)));
        assert!(e.contains(&ValidationError::UnsupportedSchemaVersion(2)));
    }

    #[test]
    fn unknown_fields_are_refused() {
        for f in [
            |v: &mut Value| v["extra"] = json!(1),
            |v: &mut Value| v["tokens"][0]["extra"] = json!(1),
            |v: &mut Value| v["templates"][0]["extra"] = json!(1),
            |v: &mut Value| v["templates"][0]["escrow"]["extra"] = json!(1),
            |v: &mut Value| v["tokens"][0]["display"]["extra"] = json!(1),
        ] {
            match check(f) {
                Err(RegistryError::Json(e)) => assert!(e.to_string().contains("unknown field"), "{e}"),
                other => panic!("expected unknown-field error, got {other:?}"),
            }
        }
    }

    #[test]
    fn bad_hex() {
        for (field, val) in [
            ("covenant_id", json!("zz")),
            ("covenant_id", json!("AB".repeat(32))), // uppercase
            ("covenant_id", json!("ab".repeat(31))),
            ("extension_commitment", json!("0x".to_string() + &"ee".repeat(31))),
        ] {
            let e = errs(check(|v| tok(v, 0)[field] = val.clone()));
            assert!(e.iter().any(|x| matches!(x, ValidationError::BadHex { .. })), "{field}: {e:?}");
        }
        let e = errs(check(|v| v["templates"][0]["template_hash"] = json!("12")));
        assert!(e.iter().any(|x| matches!(x, ValidationError::BadHex { .. })));
    }

    #[test]
    fn unknown_template_and_family_mismatch() {
        let e = errs(check(|v| tok(v, 0)["template_id"] = json!("nope")));
        assert!(e.iter().any(|x| matches!(x, ValidationError::UnknownTemplate { .. })));
        let e = errs(check(|v| tok(v, 0)["template_id"] = json!("kron-2433")));
        assert!(e.iter().any(|x| matches!(x, ValidationError::FamilyMismatch { .. })), "{e:?}");
    }

    #[test]
    fn extension_rules() {
        // kron with an extension commitment
        let e = errs(check(|v| tok(v, 1)["extension_commitment"] = json!("ee".repeat(32))));
        assert!(e.iter().any(|x| matches!(x, ValidationError::Extension { .. })), "{e:?}");
        // kron with a non-none class
        let e = errs(check(|v| tok(v, 1)["extension_class"] = json!("other")));
        assert!(e.iter().any(|x| matches!(x, ValidationError::Extension { .. })));
        // kcc20 without one
        let e = errs(check(|v| tok(v, 0)["extension_commitment"] = Value::Null));
        assert!(e.iter().any(|x| matches!(x, ValidationError::Extension { .. })));
        // class none requires zero commitment
        let e = errs(check(|v| tok(v, 0)["extension_class"] = json!("none")));
        assert!(e.iter().any(|x| matches!(x, ValidationError::Extension { .. })));
        assert!(check(|v| {
            tok(v, 0)["extension_class"] = json!("none");
            tok(v, 0)["extension_commitment"] = json!("00".repeat(32));
        })
        .is_ok());
    }

    #[test]
    fn duplicates() {
        // same identity under another ticker (and the same covenant id)
        let e = errs(check(|v| {
            let mut t = v["tokens"][0].clone();
            t["ticker"] = json!("EXTWO");
            v["tokens"].as_array_mut().unwrap().push(t);
        }));
        assert!(e.iter().any(|x| matches!(x, ValidationError::DuplicateIdentity { .. })), "{e:?}");
        assert!(e.iter().any(|x| matches!(x, ValidationError::DuplicateCovenantId { .. })));
        // same covenant id, different extension commitment: still refused (one covenant id = one token)
        let e = errs(check(|v| {
            let mut t = v["tokens"][0].clone();
            t["ticker"] = json!("EXTWO");
            t["extension_commitment"] = json!("dd".repeat(32));
            v["tokens"].as_array_mut().unwrap().push(t);
        }));
        assert!(e.iter().any(|x| matches!(x, ValidationError::DuplicateCovenantId { .. })));
        assert!(!e.iter().any(|x| matches!(x, ValidationError::DuplicateIdentity { .. })));
        // duplicate template id / hash
        let e = errs(check(|v| {
            let t = v["templates"][0].clone();
            v["templates"].as_array_mut().unwrap().push(t);
        }));
        assert!(e.iter().any(|x| matches!(x, ValidationError::DuplicateTemplateId(_))));
        assert!(e.iter().any(|x| matches!(x, ValidationError::DuplicateTemplateHash { .. })));
    }

    #[test]
    fn tickers_are_checked_for_form_never_for_uniqueness() {
        for bad in ["kron", "K", "TOOLONGTICKER1", "KR N", "ΚRON", "KRÖN", "KR-N", ""] {
            let e = errs(check(|v| tok(v, 0)["ticker"] = json!(bad)));
            assert!(e.iter().any(|x| matches!(x, ValidationError::BadTicker { .. })), "{bad}: {e:?}");
        }
        // two tokens (two covenant ids) may share a ticker or use look-alike ones: choosing which may carry it is curation
        for (a, b) in [
            ("KRON", "KR0N"),
            ("PEPE", "PEPE"),
            ("ALIEN", "A1IEN"),
            ("KILO", "K1L0"),
            ("BOLT", "B01T"),
            ("BASS", "8A55"),
            ("MAIN", "RNAIN"),
        ] {
            let r = check(|v| {
                tok(v, 0)["ticker"] = json!(a);
                tok(v, 1)["ticker"] = json!(b);
            });
            assert!(r.is_ok(), "{a}/{b}: {r:?}");
        }
    }

    #[test]
    fn numbers() {
        let e = errs(check(|v| tok(v, 0)["decimals"] = json!(19)));
        assert!(e.iter().any(|x| matches!(x, ValidationError::Decimals { decimals: 19, .. })));
        assert!(check(|v| tok(v, 0)["decimals"] = json!(18)).is_ok());
        // the legacy lot and tick of older files are read and ignored, whatever they hold
        for lot in [0i64, -1, KRON_MAX_OUTPUT_AMOUNT + 1] {
            assert!(check(|v| {
                tok(v, 1)["lot_size"] = json!(lot);
                tok(v, 1)["tick"] = json!(lot);
            })
            .is_ok());
        }
        assert!(check(|v| {
            tok(v, 0).as_object_mut().unwrap().remove("lot_size");
            tok(v, 0).as_object_mut().unwrap().remove("tick");
        })
        .is_ok());
    }

    #[test]
    fn slot_limits_must_match_template() {
        let e = errs(check(|v| tok(v, 0)["max_token_inputs"] = json!(3)));
        assert!(e.iter().any(|x| matches!(x, ValidationError::SlotLimitMismatch { token_in: 3, template_in: 8, .. })), "{e:?}");
        let e = errs(check(|v| tok(v, 1)["max_token_outputs"] = json!(8)));
        assert!(e.iter().any(|x| matches!(x, ValidationError::SlotLimitMismatch { .. })));
        assert!(check(|v| {
            tok(v, 0)["max_token_inputs"] = json!(8);
            tok(v, 0)["max_token_outputs"] = json!(8);
        })
        .is_ok());
        // template-internal consistency
        let e = errs(check(|v| v["templates"][0]["max_token_inputs"] = json!(0)));
        assert!(e.iter().any(|x| matches!(x, ValidationError::TemplateInvalid { .. })));
        let e = errs(check(|v| v["templates"][0]["state_len"] = json!(46)));
        assert!(e.iter().any(|x| matches!(x, ValidationError::TemplateInvalid { .. })));
        let e = errs(check(|v| v["templates"][2]["escrow"]["id_type"] = json!(3)));
        assert!(e.iter().any(|x| matches!(x, ValidationError::TemplateInvalid { .. })));
        let e = errs(check(|v| v["templates"][0]["escrow"]["borrow_scheme"] = json!(1)));
        assert!(e.iter().any(|x| matches!(x, ValidationError::TemplateInvalid { .. })));
    }

    #[test]
    fn allowlist_rows() {
        // a row for every entry on a supported (reviewed) program, checked or not: an entry adds facts, it is no listing step
        // (no lot or tick since protocol v3)
        let none = check(|v| {
            for t in v["templates"].as_array_mut().unwrap() {
                t["review_status"] = json!("pending-review");
            }
        })
        .unwrap();
        assert!(none.allowlist().is_empty(), "nothing of a program pending review is tradable");
        let ok = check(|v| {
            for t in v["templates"].as_array_mut().unwrap() {
                t["review_status"] = json!("reviewed");
            }
            assert_eq!(v["templates"][2]["id"], "kron-2433");
            v["templates"][2]["review_status"] = json!("pending-review");
        })
        .unwrap();
        let rows = ok.allowlist();
        assert_eq!(rows.len(), 1, "the KRON entry's program is pending review");
        let row = rows[0];
        assert_eq!(row.key, ok.allowlist_key(&ok.tokens[0]).unwrap());
        assert_eq!((row.max_token_inputs, row.max_token_outputs), (8, 8));
        assert_eq!(row.suffix_len, ok.template("kcc20-ref-8x8").unwrap().suffix_len);
        assert!(!ok.tokens[0].verified, "an unchecked entry gives a row too");
    }

    #[test]
    fn genesis_check_rules_and_derived_warnings() {
        // genesis_verified: true needs verified; a token without it trades with the derived genesis_unverified warning (the
        // executor may require it: `require_genesis_verified`)
        let base = |f: &dyn Fn(&mut Value)| {
            check(|v| {
                tok(v, 0)["verified"] = json!(true);
                tok(v, 0)["genesis_verified"] = json!(true);
                tok(v, 0)["genesis"] = genesis_rec();
                f(v);
            })
        };
        assert!(base(&|_| {}).is_ok());
        // C2: a recorded live minter is a fact, not an error: the token carries the derived warning live_mint_authority; a
        // record without a liveness check (absent live_minters) or no record gives none (the program's `mint-authority`
        // capability still says what is possible)
        let lm = format!("{}:3", "cd".repeat(32));
        let live = base(&|v| tok(v, 0)["genesis"]["live_minters"] = json!([lm.clone()])).unwrap();
        assert!(live.tokens[0].has_live_minter());
        assert_eq!(live.tokens[0].warnings(), vec![TokenWarning::LiveMintAuthority]);
        let undetermined = base(&|v| {
            tok(v, 0)["genesis"].as_object_mut().unwrap().remove("live_minters");
        })
        .unwrap();
        assert!(undetermined.tokens[0].warnings().is_empty());
        // a record needs genesis_verified; malformed record fields
        for (name, f) in [
            ("no genesis_verified", &(|v: &mut Value| tok(v, 0)["genesis_verified"] = Value::Null) as &dyn Fn(&mut Value)),
            ("bad txid", &|v: &mut Value| tok(v, 0)["genesis"]["txid"] = json!("AB".repeat(32))),
            ("unordered outputs", &|v: &mut Value| tok(v, 0)["genesis"]["outputs"] = json!([2, 1])),
            ("no outputs", &|v: &mut Value| tok(v, 0)["genesis"]["outputs"] = json!([])),
            ("minter not a genesis output", &|v: &mut Value| tok(v, 0)["genesis"]["minter_outputs"] = json!([5])),
            ("negative supply", &|v: &mut Value| tok(v, 0)["genesis"]["supply"] = json!(-1)),
            ("empty source", &|v: &mut Value| tok(v, 0)["genesis"]["source"] = json!("")),
            ("bad outpoint", &|v: &mut Value| tok(v, 0)["genesis"]["live_minters"] = json!(["xyz:1"])),
        ] {
            assert!(base(f).is_err(), "{name}");
        }
        assert!(base(&|v| tok(v, 0)["genesis"]["extra"] = json!(1)).is_err(), "unknown genesis field");
        let e = errs(check(|v| tok(v, 0)["genesis_verified"] = json!(true)));
        assert!(e.iter().any(|x| matches!(x, ValidationError::Genesis { .. })), "genesis_verified without verified: {e:?}");
        let ok = check(|v| {
            tok(v, 0)["verified"] = json!(true);
            tok(v, 0)["genesis_verified"] = json!(true);
        })
        .unwrap();
        assert!(ok.tokens[0].warnings().is_empty());
        assert_eq!(ok.tokens[1].warnings(), vec![TokenWarning::GenesisUnverified]);
        // checked and NOT clean: still a valid entry (a fact), and it warns
        let r = check(|v| {
            tok(v, 0)["verified"] = json!(true);
            tok(v, 0)["genesis_verified"] = json!(false);
        })
        .unwrap();
        assert_eq!(r.tokens[0].warnings(), vec![TokenWarning::GenesisUnverified]);
        for (w, s) in
            [(TokenWarning::GenesisUnverified, "genesis_unverified"), (TokenWarning::LiveMintAuthority, "live_mint_authority")]
        {
            assert_eq!(w.as_str(), s);
            assert_eq!(serde_json::to_value(w).unwrap(), json!(s));
        }
    }

    #[test]
    fn display_rules() {
        let e = errs(check(|v| tok(v, 0)["name"] = json!("Evil\u{202E}Coin")));
        assert!(e.iter().any(|x| matches!(x, ValidationError::BadDisplay { .. })));
        let e = errs(check(|v| tok(v, 0)["name"] = json!("")));
        assert!(e.iter().any(|x| matches!(x, ValidationError::BadDisplay { .. })));
        let e = errs(check(|v| tok(v, 0)["display"]["website"] = json!("http://example.com")));
        assert!(e.iter().any(|x| matches!(x, ValidationError::BadDisplay { .. })));
        let e = errs(check(|v| tok(v, 0)["display"]["kcc23"] = json!([1])));
        assert!(e.iter().any(|x| matches!(x, ValidationError::BadDisplay { .. })));
        // the token's X and Telegram accounts: https:// addresses
        for key in ["x", "telegram"] {
            let e = errs(check(|v| tok(v, 0)["display"][key] = json!("@handle")));
            assert!(e.iter().any(|x| matches!(x, ValidationError::BadDisplay { .. })), "{key}");
        }
        let r = check(|v| {
            tok(v, 0)["display"]["x"] = json!("https://x.com/example");
            tok(v, 0)["display"]["telegram"] = json!("https://t.me/example_chat");
        });
        assert!(r.is_ok(), "{:?}", r.err());
    }

    #[test]
    fn identity_and_display_name() {
        let r = Registry::parse(EXAMPLE).unwrap();
        let t = &r.tokens[0];
        let k = r.allowlist_key(t).unwrap();
        assert_eq!(k.family, Family::Kcc20);
        assert_eq!(hex_of(&k.template_hash), r.template(&t.template_id).unwrap().template_hash);
        assert_eq!(k.extension_commitment, Some(parse_hex32(t.extension_commitment.as_ref().unwrap()).unwrap()));
        assert_eq!(r.find(&k).map(|x| x.ticker.as_str()), Some(t.ticker.as_str()));
        let kr = r.allowlist_key(&r.tokens[1]).unwrap();
        assert_eq!(kr.extension_commitment, None);
        assert_ne!(k, kr);
        let name = display_name(t);
        assert!(name.starts_with(&format!("{} (", t.ticker)) && !name.ends_with("…"));
        assert!(name.contains(&format!("{}…{}", &t.covenant_id[..4], &t.covenant_id[60..])));
        // ticker and short id, nothing else: no standing, whatever the facts say
        assert_eq!(name, format!("{} ({}…{})", t.ticker, &t.covenant_id[..4], &t.covenant_id[60..]));
        let mut v = t.clone();
        v.verified = true;
        v.genesis_verified = Some(true);
        assert_eq!(display_name(&v), name);
    }

    fn hex_of(b: &[u8; 32]) -> String {
        b.iter().map(|x| format!("{x:02x}")).collect()
    }

    /// The schema and the validator must agree on required fields, properties and enums.
    #[test]
    fn schema_stays_in_sync_with_the_types() {
        let schema: Value = serde_json::from_str(SCHEMA_JSON).unwrap();
        assert_eq!(schema["$schema"], "https://json-schema.org/draft/2020-12/schema");
        let defs = &schema["$defs"];
        let props = |def: &Value| -> BTreeSet<String> { def["properties"].as_object().unwrap().keys().cloned().collect() };
        let req = |def: &Value| -> BTreeSet<String> {
            def["required"].as_array().unwrap().iter().map(|x| x.as_str().unwrap().to_string()).collect()
        };
        let enum_of =
            |v: &Value| -> Vec<String> { v["enum"].as_array().unwrap().iter().map(|x| x.as_str().unwrap().to_string()).collect() };
        let keys = |v: &Value| -> BTreeSet<String> { v.as_object().unwrap().keys().cloned().collect() };

        // fully populated instances serialise every property the schema declares
        let ex = Registry::parse(EXAMPLE).unwrap();
        let mut full_tok = serde_json::to_value(&ex.tokens[0]).unwrap();
        full_tok["genesis_verified"] = json!(true);
        full_tok["genesis"] = json!({"txid": "11".repeat(32), "daa_score": 1, "outputs": [1, 2], "supply": 3, "minter_outputs": [],
            "mint_allowance": 0, "issuer": "kcc20-reference@27f8403", "live_minters": [], "checked_at_daa": 4, "source": "s"});
        // the legacy lot and tick (read and ignored) are declared optional by the schema
        full_tok["lot_size"] = json!(1);
        full_tok["tick"] = json!(1);
        full_tok["max_token_inputs"] = json!(8);
        full_tok["max_token_outputs"] = json!(8);
        full_tok["display"] = json!({"description": "d", "website": "https://e.x", "icon": "https://e.x/i.png", "x": "https://x.com/e", "telegram": "https://t.me/example", "kcc23": {}});
        let full_tok_typed: Token = serde_json::from_value(full_tok.clone()).unwrap();
        let full_tok = serde_json::to_value(&full_tok_typed).unwrap();
        // the schema also declares the legacy judgements of older files, which the parser reads and ignores (never written)
        let legacy: BTreeSet<String> = LEGACY_TOKEN_KEYS.iter().map(|s| s.to_string()).collect();
        assert!(keys(&full_tok).is_disjoint(&legacy));
        assert_eq!(keys(&full_tok).union(&legacy).cloned().collect::<BTreeSet<_>>(), props(&defs["token"]), "token properties");
        for k in LEGACY_TOKEN_KEYS {
            assert_eq!(defs["token"]["properties"][k]["deprecated"], true, "{k}");
        }
        assert_eq!(keys(&full_tok["display"]), props(&defs["display"]), "display properties");
        assert_eq!(keys(&full_tok["genesis"]), props(&defs["genesis_record"]), "genesis record properties");
        let optional_gen: BTreeSet<String> =
            ["live_minters".to_string(), "mint_allowance".to_string(), "issuer".to_string()].into_iter().collect();
        let expect: BTreeSet<String> = keys(&full_tok["genesis"]).difference(&optional_gen).cloned().collect();
        assert_eq!(req(&defs["genesis_record"]), expect, "genesis record required");
        assert_eq!(defs["genesis_record"]["additionalProperties"], false);
        let mut full_tpl = serde_json::to_value(&ex.templates[0]).unwrap();
        full_tpl["source"] = json!({"path": "p", "upstream": "u", "note": "n"});
        full_tpl["capabilities"] = json!(["burn"]);
        full_tpl["risks"] = json!(["r"]);
        let full_tpl_typed: Template = serde_json::from_value(full_tpl.clone()).unwrap();
        let full_tpl = serde_json::to_value(&full_tpl_typed).unwrap();
        assert_eq!(keys(&full_tpl), props(&defs["template"]), "template properties");
        assert_eq!(keys(&full_tpl["source"]), props(&defs["source"]), "source properties");
        let full_escrow = serde_json::to_value(Escrow {
            owner_scheme: Some(0),
            borrow_scheme: Some(0),
            id_type: Some(0),
            is_minter: Some(0),
            delivery_id_type: Some(0),
        })
        .unwrap();
        assert_eq!(keys(&full_escrow), props(&defs["escrow"]), "escrow properties");
        let mut root = serde_json::to_value(&ex).unwrap();
        root["$schema"] = json!("s");
        assert_eq!(keys(&root), props(&schema), "root properties");

        // required = the non-optional fields
        let optional_tok: BTreeSet<String> =
            ["max_token_inputs", "max_token_outputs", "display", "genesis_verified", "genesis", "lot_size", "tick"]
                .iter()
                .map(|s| s.to_string())
                .collect();
        let expect: BTreeSet<String> = keys(&full_tok).difference(&optional_tok).cloned().collect();
        assert_eq!(req(&defs["token"]), expect, "token required");
        let optional_tpl: BTreeSet<String> = ["capabilities".to_string(), "risks".to_string()].into_iter().collect();
        let expect: BTreeSet<String> = keys(&full_tpl).difference(&optional_tpl).cloned().collect();
        assert_eq!(req(&defs["template"]), expect, "template required");
        assert_eq!(req(&defs["source"]), ["path".to_string()].into_iter().collect());
        let expect: BTreeSet<String> = ["schema_version", "network", "templates", "tokens"].iter().map(|s| s.to_string()).collect();
        assert_eq!(req(&schema), expect, "root required");

        // enums
        assert_eq!(enum_of(&defs["family"]), Family::ALL);
        assert!(defs.get("status").is_none(), "no listing state: KOB makes no per-token judgement");
        assert_eq!(enum_of(&defs["extension_class"]), ExtensionClass::ALL);
        assert_eq!(enum_of(&defs["review_status"]), ReviewStatus::ALL);
        assert_eq!(enum_of(&defs["capability"]), Capability::ALL);
        assert_eq!(enum_of(&defs["warning_code"]), TokenWarning::ALL);
        for s in Capability::ALL {
            let c: Capability = serde_json::from_value(json!(s)).unwrap();
            assert_eq!(c.as_str(), s);
        }
        assert_eq!(enum_of(&schema["properties"]["network"]), NETWORKS);
        for s in Family::ALL {
            let f: Family = serde_json::from_value(json!(s)).unwrap();
            assert_eq!(f.as_str(), s);
        }
        for s in ExtensionClass::ALL {
            serde_json::from_value::<ExtensionClass>(json!(s)).unwrap();
        }
        for s in ReviewStatus::ALL {
            serde_json::from_value::<ReviewStatus>(json!(s)).unwrap();
        }
        // numeric bounds
        assert_eq!(schema["properties"]["schema_version"]["const"], SCHEMA_VERSION);
        assert_eq!(defs["token"]["properties"]["decimals"]["maximum"], MAX_DECIMALS);
        // the legacy lot and tick: optional, never required
        let required: Vec<&str> = defs["token"]["required"].as_array().unwrap().iter().map(|x| x.as_str().unwrap()).collect();
        assert!(!required.contains(&"lot_size") && !required.contains(&"tick"));
        assert_eq!(defs["token"]["additionalProperties"], false);
        assert_eq!(defs["template"]["additionalProperties"], false);
        assert_eq!(schema["additionalProperties"], false);
    }

    /// Generic genesis rule: a group may hold, beside the token's own outputs, exactly the companions its program's profile
    /// declares (here a launchpad's bonding curve bound to the token's id, as an opaque companion of the reference 3 / 3
    /// program); the same group is refused under the embedded profile, and an impostor of the curve is refused under both.
    #[test]
    fn a_genesis_group_holds_the_companions_its_profile_declares_and_nothing_else() {
        use crate::profile::{CompanionRole, CompanionRule, Layout};
        use crate::state::{Kcc20State, SCHEME_P2PK};
        use kaspa_consensus_core::hashing::covenant_id::covenant_id;
        use kaspa_consensus_core::tx::{TransactionId, TransactionOutpoint, TransactionOutput};

        let reg = Registry::default_registry();
        let tpl = reg.template("kcc20-ref-3x3").unwrap();
        let program = crate::artifacts::template(crate::artifacts::TemplateId::Kcc20Ref);
        let holder = Kcc20State {
            amount: 1_000,
            owner: [7; 32],
            owner_scheme: SCHEME_P2PK,
            borrow_scheme: 0,
            borrow_guard: [0; 32],
            extension_commitment: [0; 32],
        }
        .redeem_with(program);
        let (c_prefix, c_suffix) = (vec![0x6b, 0x51], vec![0x75, 0x51, 0x87, 0x69, 0x51]);
        let curve = |state: [u8; 9], suffix: &[u8]| [c_prefix.as_slice(), &state, suffix].concat();
        let rule = CompanionRule {
            role: CompanionRole::Curve,
            decoder: None,
            template_hash: Some(crate::json::to_hex(&silverscript_abi::template_hash(&c_prefix, &c_suffix))),
            layout: Some(Layout { prefix_len: c_prefix.len(), state_len: 9, suffix_len: c_suffix.len() }),
            note: None,
        };
        let embedded = crate::profile::by_name("KCC20Ref").unwrap();
        let mut launchpad = embedded.clone();
        launchpad.genesis.companions.push(rule);
        launchpad.check_shape().unwrap();

        let txid = [0x11; 32];
        let check = |profile: &crate::profile::ProgramProfile, redeems: &[Vec<u8>]| {
            let op = TransactionOutpoint { transaction_id: TransactionId::from_bytes(txid), index: 0 };
            let outs: Vec<TransactionOutput> = redeems
                .iter()
                .map(|r| TransactionOutput { value: 100_000_000, script_public_key: crate::script::p2sh_spk(r), covenant: None })
                .collect();
            let cov = covenant_id(op, outs.iter().enumerate().map(|(i, o)| (i as u32, o)));
            let group: Vec<GenesisOutput> = redeems
                .iter()
                .enumerate()
                .map(|(i, r)| GenesisOutput {
                    index: i as u32,
                    value: 100_000_000,
                    script_public_key: outs[i].script_public_key.clone(),
                    redeem_script: Some(r.clone()),
                })
                .collect();
            verify_genesis_with(tpl, profile, &crate::json::to_hex(&cov.as_bytes()), &"22".repeat(32), (txid, 0), &group, None)
        };
        let good = [holder.clone(), curve([0x08, 1, 2, 3, 4, 5, 6, 7, 8], &c_suffix)];
        let r = check(&launchpad, &good).expect("the declared curve is accepted");
        assert_eq!((r.supply, r.outputs), (1_000, 2));
        assert_eq!(r.companion_outputs, vec![CompanionOutput { index: 1, role: "curve".into() }]);
        assert!(r.minter_outputs.is_empty(), "an opaque companion is no mint authority");
        assert_eq!(check(embedded, &good).unwrap_err(), GenesisError::NotTemplate(1, "kcc20-ref-3x3".into()));
        let impostor = [holder, curve([0x08, 1, 2, 3, 4, 5, 6, 7, 8], &[0x75, 0x51])];
        assert_eq!(check(&launchpad, &impostor).unwrap_err(), GenesisError::NotTemplate(1, "kcc20-ref-3x3".into()));
        // the embedded entry point uses the embedded profile
        assert_eq!(
            verify_genesis(tpl, &"00".repeat(32), &"22".repeat(32), (txid, 0), &[]).unwrap_err(),
            GenesisError::Malformed("a genesis group has at least one output".into())
        );
    }
}
