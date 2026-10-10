//! Token registry: the data behind KOB's token allowlist (`registry/tokens.json`).
//!
//! A *family* is a token program lineage KOB can escrow (`kcc20`: the draft KCC-20 layout, `kron`: KRON's 46-byte layout).
//! The registry pins, per family, the exact token programs (`templates`: template hash, sizes, slot limits and the escrow
//! owner types the order contracts use) and lists the tokens KOB knows about (`tokens`).
//! A token's identity is `(family, covenant id, template hash, extension commitment)` (a token is its KIP-20
//! covenant id plus template hash, and fungibility only holds among equal extension commitments). Display is
//! `TICKER (abcd…1234) [verified]`; tickers are ASCII uppercase alphanumerics, and two tickers that collide after
//! homoglyph normalisation (`0`/`O`, `1`/`I`/`L`) are refused.
//!
//! **Two lists.** `templates` is the STRICT list: only token programs KOB has reviewed (`review_status: reviewed`) can carry a
//! tradable token, and a template that can freeze, seize or blacklist holders says so in `capabilities`. `tokens` is the OPEN list:
//! every token whose program is on the strict list is tradable and shown, whether or not it is in `tokens`; a token KOB confirmed
//! genuine is `official` (badge), any other is `unverified`; an entry that is still `pending-review` stays tradable and is shown as such
//! (`[unverified, pending review]`), only `delisted` blocks a token. Identity is always the covenant id plus the template hash, never the ticker.
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
/// the mainnet deployment record pins (`contracts/deploy/mainnet/deployment.json` `registry.sha256`, the web build's
/// `web/registry-pin.mjs`, and `GET /v1/tokens` `registry.sha256` of an executor that loads it).
pub fn default_registry_sha256() -> String {
    use sha2::{Digest, Sha256};
    let lf = DEFAULT_REGISTRY_JSON.replace("\r\n", "\n");
    crate::json::to_hex(&Sha256::digest(lf.as_bytes()))
}

/// Format version this module reads and writes.
pub const SCHEMA_VERSION: u32 = 1;
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

/// Review state of a pinned token program (a program must be `reviewed` before any token on it can be `listed`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ReviewStatus {
    /// Pinned, semantic review not done.
    PendingReview,
    /// Semantic review done (owner types, no admin path, conservation, limits).
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

/// How KOB regards a token: shown next to every token in the UIs and the API.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Standing {
    /// Confirmed genuine by KOB (covenant id and program checked against the chain and the issuer's own listing).
    Official,
    /// On a strict-list program, not confirmed genuine (tickers collide: check the covenant id).
    Unverified,
    /// Was listed, no longer: orders are not listed.
    Delisted,
}
impl Standing {
    /// Schema / API spelling.
    pub fn as_str(self) -> &'static str {
        match self {
            Standing::Official => "official",
            Standing::Unverified => "unverified",
            Standing::Delisted => "delisted",
        }
    }
}

/// Listing state of a token.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Status {
    /// Known, not yet confirmed genuine by the registry maintainers. Still tradable, like any token on a reviewed template
    /// (the token list is open: what gates trading is the template, and only an explicit `delisted` blocks a token); shown as
    /// `[unverified, pending review]` or `[verified, pending review]`.
    PendingReview,
    /// Listed: tradable, and eligible for the `official` badge once its genesis check passes.
    Listed,
    /// Was listed, no longer.
    Delisted,
}
impl Status {
    /// Schema spellings.
    pub const ALL: [&'static str; 3] = ["pending-review", "listed", "delisted"];
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
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
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
    /// Review state (only `reviewed` templates are on the strict list).
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
    /// True when the template is on the strict list (semantic review done).
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
    /// Listing state.
    pub status: Status,
    /// KOB checked covenant id and template hash against chain (a UTXO of the token is an instance of the template).
    pub verified: bool,
    /// Genesis check: `true` = EVERY output of the token's genesis group was revealed and is an instance of its
    /// program ([`verify_genesis`]); `false` = checked and NOT clean (a hidden non-template output); absent = not checked.
    /// The covenant id commits only to the P2SH hashes of the genesis outputs, so an unchecked genesis may hide an output
    /// that later mints look-alike tokens or feeds a fake balance into a transfer. `official` needs `true` (a listed token without it trades with the warning); any
    /// token without `true` carries the API warning [`TokenWarning::GenesisUnverified`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub genesis_verified: Option<bool>,
    /// What the genesis check found on chain (the record behind `genesis_verified` and the live-mint-authority check);
    /// re-derived from the committed chain evidence by `kob registry verify-genesis`. `official` needs it with no live minter.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub genesis: Option<GenesisRecord>,
    /// A warning UIs show next to the token (free text from the registry maintainer, e.g. what the genesis check found).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub warning: Option<String>,
    /// Confirmed genuine (the real token of its issuer, not a lookalike): shown with an "official" badge. Needs `verified` and
    /// `status: listed`. Every other token is shown as "unverified".
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub official: bool,
    /// Display metadata.
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
    /// Total token amount the genesis created (sum of the genesis outputs' amounts).
    pub supply: i64,
    /// Genesis outputs that carry a mint authority (KRON `is_minter != 0`), by output index. A KRON transaction can create a
    /// minter output only when it spends a minter (every non-minter token input refuses minter outputs), so an empty list
    /// proves the token can never have a live minter. KCC-20 programs keep their minter lanes behind the extension
    /// commitment, which this check cannot see: always empty for the kcc20 family.
    pub minter_outputs: Vec<u32>,
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
    /// Live mint-authority cells (`txid:index`) at `checked_at_daa`: empty = none (condition C2 holds), absent = not
    /// determined. A token with a live minter needs a `warning` and cannot be official.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub live_minters: Option<Vec<String>>,
    /// Virtual DAA score of the node the check read liveness from.
    pub checked_at_daa: u64,
    /// Where the bytes came from (node, explorer, issuer) and how they were checked.
    pub source: String,
}

/// A warning a token carries in the registry data and the API (`warnings`, kebab-case strings). UIs show it next to the token.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TokenWarning {
    /// The registry does not say `genesis_verified: true`: not every genesis output of the token was checked
    /// against its program, so the issuer may hold a hidden non-template output able to mint look-alike tokens or feed a fake
    /// balance into a transfer. Every token on the open list without a registry entry carries it.
    GenesisUnverified,
}
impl TokenWarning {
    /// All warning codes (API `warnings`).
    pub const ALL: [&'static str; 1] = ["genesis_unverified"];
    /// API spelling (the executor's `/v1/tokens` `warnings`).
    pub fn as_str(self) -> &'static str {
        match self {
            TokenWarning::GenesisUnverified => "genesis_unverified",
        }
    }
}

/// Warnings of a token that has NO registry entry (a token of an open-list program the executor found on chain).
pub const OPEN_TOKEN_WARNINGS: [TokenWarning; 1] = [TokenWarning::GenesisUnverified];

impl Token {
    /// Warnings this token carries (API `warnings`): [`TokenWarning::GenesisUnverified`] unless `genesis_verified: true`.
    pub fn warnings(&self) -> Vec<TokenWarning> {
        if self.genesis_verified == Some(true) {
            vec![]
        } else {
            vec![TokenWarning::GenesisUnverified]
        }
    }

    /// Official (confirmed genuine), unverified, or delisted.
    pub fn standing(&self) -> Standing {
        if self.status == Status::Delisted {
            Standing::Delisted
        } else if self.official {
            Standing::Official
        } else {
            Standing::Unverified
        }
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
    /// Two tickers are equal after homoglyph normalisation.
    #[error("tickers {a} and {b} are confusable (both normalise to {normalised})")]
    ConfusableTicker {
        /// First ticker.
        a: String,
        /// Second ticker.
        b: String,
        /// Normalised form.
        normalised: String,
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
    /// `official` without what it needs.
    #[error("token {ticker}: cannot be official: {reason}")]
    NotOfficial {
        /// Token ticker.
        ticker: String,
        /// What is missing.
        reason: &'static str,
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
    /// `genesis_verified` rules, or a malformed `warning`.
    #[error("token {ticker}: {reason}")]
    Genesis {
        /// Token ticker.
        ticker: String,
        /// What is wrong.
        reason: &'static str,
    },
    /// A `listed` token lacks what listing needs.
    #[error("token {ticker}: cannot be listed: {reason}")]
    NotListable {
        /// Token ticker.
        ticker: String,
        /// What is missing.
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

/// Homoglyph normalisation used to detect confusable tickers: uppercase, the two-letter lookalikes
/// `RN`->`M` and `VV`->`W`, then `O`->`0`, `I`/`L`->`1`, `S`->`5`, `B`->`8`. Registry tickers are ASCII (validated), so this is the whole
/// skeleton for them; the web app's `normalizeTicker` (registry.ts) applies the same ASCII rules after folding Unicode lookalikes
/// (NFKC, Cyrillic / Greek / full-width letters) to ASCII, for UNTRUSTED tickers.
pub fn normalize_ticker(ticker: &str) -> String {
    ticker
        .chars()
        .flat_map(|c| c.to_uppercase())
        .collect::<String>()
        .replace("RN", "M")
        .replace("VV", "W")
        .chars()
        .map(|c| match c {
            'O' => '0',
            'I' | 'L' => '1',
            'S' => '5',
            'B' => '8',
            other => other,
        })
        .collect()
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

    /// Parse only (unknown fields and wrong types are still refused).
    pub fn parse_unvalidated(json: &str) -> Result<Registry, RegistryError> {
        Ok(serde_json::from_str(json)?)
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
            if t.family == Family::Kron && t.prefix_len != 0 {
                e.push(bad("kron templates have no prefix (state span at offset 0)".into()));
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
        let mut norm: BTreeMap<String, &str> = BTreeMap::new();
        for (i, tok) in self.tokens.iter().enumerate() {
            let tk = tok.ticker.as_str();
            // ticker
            let tick_bad = |reason: &'static str| ValidationError::BadTicker { ticker: short(tk), reason };
            if tk.len() < 2 || tk.len() > 12 {
                e.push(tick_bad("length must be 2..=12"));
            } else if !tk.bytes().all(|b| b.is_ascii_uppercase() || b.is_ascii_digit()) {
                e.push(tick_bad("only ASCII uppercase letters and digits are allowed"));
            } else {
                let n = normalize_ticker(tk);
                match norm.insert(n.clone(), tk) {
                    Some(prev) if prev == tk => e.push(tick_bad("duplicate ticker")),
                    Some(prev) => e.push(ValidationError::ConfusableTicker { a: prev.to_string(), b: tk.to_string(), normalised: n }),
                    None => {}
                }
            }
            // name / display
            let disp_bad = |reason: String| ValidationError::BadDisplay { ticker: short(tk), reason };
            if tok.name.trim().is_empty() || tok.name.chars().count() > 64 || has_bad_char(&tok.name) {
                e.push(disp_bad("name must be 1..=64 printable characters (no control, zero-width or bidi characters)".into()));
            }
            if let Some(d) = &tok.display {
                for (label, v) in [("description", &d.description), ("website", &d.website), ("icon", &d.icon)] {
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
            // genesis check: official needs every genesis output checked (a listed token without it carries the warning)
            let gbad = |reason: &'static str| ValidationError::Genesis { ticker: tk.to_string(), reason };
            if tok.genesis_verified == Some(true) && !tok.verified {
                e.push(gbad("genesis_verified needs verified (the covenant id and template hash checked first)"));
            }
            if tok.official && tok.genesis_verified != Some(true) {
                e.push(gbad("cannot be official without genesis_verified: true (every genesis output checked)"));
            }
            if let Some(w) = &tok.warning {
                if w.is_empty() || w.chars().count() > 512 || has_bad_char(w) {
                    e.push(gbad("warning must be 1-512 characters without control, zero-width or bidi characters"));
                }
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
                if g.source.is_empty() || g.source.chars().count() > 512 || has_bad_char(&g.source) {
                    e.push(gbad("genesis.source must be 1-512 characters without control, zero-width or bidi characters"));
                }
                if let Some(live) = &g.live_minters {
                    if !live.iter().all(|o| is_outpoint(o)) {
                        e.push(gbad("genesis.live_minters entries must be <txid>:<index>"));
                    }
                    if !live.is_empty() && tok.warning.is_none() {
                        e.push(gbad("a token with a live mint authority needs a warning (active mint authority)"));
                    }
                }
            }
            if tok.official && tok.genesis.as_ref().and_then(|g| g.live_minters.as_ref()).is_none_or(|l| !l.is_empty()) {
                e.push(gbad("cannot be official without a genesis record that found no live mint authority (live_minters: [])"));
            }
            // official
            if tok.official {
                let bad = |reason: &'static str| ValidationError::NotOfficial { ticker: tk.to_string(), reason };
                if !tok.verified {
                    e.push(bad("not verified against chain"));
                }
                if tok.status != Status::Listed {
                    e.push(bad("only a listed token can be official"));
                }
            }
            // listing
            if tok.status == Status::Listed {
                let bad = |reason: &'static str| ValidationError::NotListable { ticker: tk.to_string(), reason };
                if !tok.verified {
                    e.push(bad("not verified against chain"));
                }
                if tpl.review_status != ReviewStatus::Reviewed {
                    e.push(bad("its template is not reviewed"));
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

    /// Allowlist rows of every `listed` token (call after `validate`).
    pub fn listed_allowlist(&self) -> Vec<AllowlistEntry> {
        self.tokens
            .iter()
            .filter(|t| t.status == Status::Listed)
            .filter_map(|t| {
                let tpl = self.template(&t.template_id)?;
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

/// `TICKER (abcd…1234) [verified]`: ticker, short covenant id, state. `[delisted]` overrides `[official]`, which overrides `[verified]`;
/// a `pending-review` token says so (`[unverified, pending review]`, `[verified, pending review]`). Mirrors `displayName` of the web registry.
pub fn display_name(token: &Token) -> String {
    let c = &token.covenant_id;
    let short_id = if c.len() >= 8 { format!("{}…{}", &c[..4], &c[c.len() - 4..]) } else { c.clone() };
    let pending = token.status == Status::PendingReview;
    let base = if token.official && !pending {
        "official"
    } else if token.verified {
        "verified"
    } else {
        "unverified"
    };
    let state = match token.status {
        Status::Delisted => "[delisted]".to_string(),
        Status::PendingReview => format!("[{base}, pending review]"),
        Status::Listed => format!("[{base}]"),
    };
    format!("{} ({}) {}", token.ticker, short_id, state)
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
/// `genesis_verified: true`, which `official` requires.
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
    use kaspa_consensus_core::hashing::covenant_id::covenant_id as cov_id_of;
    use kaspa_consensus_core::tx::{TransactionId, TransactionOutpoint, TransactionOutput};

    let hash = parse_hex32(&template.template_hash).ok_or_else(|| GenesisError::Malformed("bad template hash".into()))?;
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
    // the token's one extension commitment: the one named, else the first output's
    let mut ext = extension_commitment;
    for o in outputs {
        let redeem = o.redeem_script.as_ref().ok_or(GenesisError::Unrevealed(o.index))?;
        if crate::script::p2sh_spk(redeem) != o.script_public_key {
            return Err(GenesisError::WrongRedeem(o.index));
        }
        let state = program.state_of(redeem).ok_or_else(|| GenesisError::NotTemplate(o.index, template.id.clone()))?;
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
    Ok(GenesisReport { txid: genesis_txid.to_string(), outputs: outputs.len() as u32, supply, minter_outputs })
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
    /// A genesis record that found no live mint authority (what official needs besides genesis_verified).
    fn genesis_rec() -> Value {
        json!({"txid": "ab".repeat(32), "daa_score": 10, "outputs": [1, 2], "supply": 1000, "minter_outputs": [],
            "live_minters": [], "checked_at_daa": 20, "source": "test"})
    }

    #[test]
    fn default_and_example_registries_validate() {
        let d = Registry::default_registry();
        assert_eq!(d.network, "mainnet");
        // the KRON family from the 2026-09-29 census: seven graduated tokens and KDIST, identity verified. Listing verification
        // (2026-10-03, `registry/evidence/mainnet-genesis.json`, `genesis_evidence`): every genesis verified on mainnet data (C1),
        // no genesis minter output so no live mint authority (C2): listed. Official except the two test tokens (founder 2026-10-03:
        // PEPE "The Ultimate test" and DNBT "dont buy this is test" are test tokens per their own names; PEPE also collides with the
        // well-known PEPE), which keep their genesis records and carry a maintainer warning instead
        assert_eq!(d.tokens.len(), 8);
        assert!(d.tokens.iter().all(|t| t.family == Family::Kron
            && t.verified
            && t.genesis_verified == Some(true)
            && t.genesis.as_ref().is_some_and(|g| g.minter_outputs.is_empty() && g.live_minters == Some(vec![]))
            && t.status == Status::Listed
            && t.warnings().is_empty()));
        for t in &d.tokens {
            let test_token = matches!(t.ticker.as_str(), "PEPE" | "DNBT");
            assert_eq!(t.official, !test_token, "{}", t.ticker);
            assert_eq!(t.standing(), if test_token { Standing::Unverified } else { Standing::Official }, "{}", t.ticker);
            let warned = t.warning.as_deref().is_some_and(|w| w.starts_with("Not official: a test token per its own name"));
            assert_eq!(warned, test_token, "{}", t.ticker);
        }
        assert!(d.tokens.iter().find(|t| t.ticker == "PEPE").unwrap().warning.as_deref().unwrap().contains("well-known PEPE"));
        assert_eq!(d.listed_allowlist().len(), 8);
        assert_eq!(d.templates.len(), 6);
        // internal review of the two KRON programs (2026-09-30, `review_b2_kron.rs`): reviewed with conditions (per-token genesis check and
        // no live minter before a token is listed or official); the reference KCC-20 programs and KaspaCom's are pending
        let strict: Vec<&str> = d.strict_templates().map(|t| t.id.as_str()).collect();
        assert_eq!(strict, ["kron-2433", "kron-2732"]);
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
        // the published public-mint build: its KCC-1 actor-type handle (34-byte prefix with the context field), pending review
        let pm = d.template("kcc20-ref-public-mint").unwrap();
        assert_eq!(pm.review_status, ReviewStatus::PendingReview);
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
        assert!(ex.listed_allowlist().is_empty());
    }

    #[test]
    fn strict_list_capabilities_official_and_standing() {
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
        // official needs verified and listed
        let e = errs(check(|v| tok(v, 0)["official"] = json!(true)));
        assert_eq!(e.iter().filter(|x| matches!(x, ValidationError::NotOfficial { .. })).count(), 2, "{e:?}");
        let ok = check(|v| {
            tok(v, 0)["official"] = json!(true);
            tok(v, 0)["verified"] = json!(true);
            tok(v, 0)["genesis_verified"] = json!(true);
            tok(v, 0)["genesis"] = genesis_rec();
            tok(v, 0)["status"] = json!("listed");
            v["templates"][1]["review_status"] = json!("reviewed");
        })
        .unwrap();
        assert!(ok.tokens[0].warnings().is_empty() && ok.tokens[1].warnings() == OPEN_TOKEN_WARNINGS.to_vec());
        assert_eq!(ok.tokens[0].standing(), Standing::Official);
        assert_eq!(ok.tokens[1].standing(), Standing::Unverified);
        assert!(display_name(&ok.tokens[0]).ends_with("[official]"));
        let mut d = ok.tokens[0].clone();
        d.status = Status::Delisted;
        assert_eq!(d.standing(), Standing::Delisted);
        assert_eq!(Standing::Unverified.as_str(), "unverified");
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
    fn tickers_and_confusables() {
        for bad in ["kron", "K", "TOOLONGTICKER1", "KR N", "ΚRON", "KRÖN", "KR-N", ""] {
            let e = errs(check(|v| tok(v, 0)["ticker"] = json!(bad)));
            assert!(e.iter().any(|x| matches!(x, ValidationError::BadTicker { .. })), "{bad}: {e:?}");
        }
        for (a, b) in [
            ("KRON", "KR0N"),
            ("PEPE", "PEPE"),
            ("ALIEN", "A1IEN"),
            ("KILO", "K1L0"),
            ("BOLT", "B01T"),
            ("BASS", "8A55"),
            ("MAIN", "RNAIN"),
        ] {
            let e = errs(check(|v| {
                tok(v, 0)["ticker"] = json!(a);
                tok(v, 1)["ticker"] = json!(b);
            }));
            assert!(
                e.iter().any(|x| matches!(x, ValidationError::ConfusableTicker { .. } | ValidationError::BadTicker { .. })),
                "{a}/{b}: {e:?}"
            );
        }
        assert_eq!(normalize_ticker("kr0n"), "KR0N");
        assert_eq!(normalize_ticker("LIO"), normalize_ticker("110"));
        // 5/S, 8/B, rn/m, vv/w
        assert_eq!(normalize_ticker("BASS"), normalize_ticker("8A55"));
        assert_eq!(normalize_ticker("KASPERN"), normalize_ticker("KASPEM"));
        assert_eq!(normalize_ticker("VVEB"), normalize_ticker("WE8"));
        assert_ne!(normalize_ticker("KRON"), normalize_ticker("KRAN"));
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
    fn listing_rules() {
        // listed needs verified and a reviewed template (no lot or tick since protocol v3)
        let e = errs(check(|v| {
            tok(v, 0)["status"] = json!("listed");
            for t in v["templates"].as_array_mut().unwrap() {
                t["review_status"] = json!("pending-review");
            }
        }));
        let n = e.iter().filter(|x| matches!(x, ValidationError::NotListable { .. })).count();
        assert!(n >= 2, "{e:?}");
        let ok = check(|v| {
            tok(v, 0)["status"] = json!("listed");
            tok(v, 0)["verified"] = json!(true);
            tok(v, 0)["genesis_verified"] = json!(true);
            for t in v["templates"].as_array_mut().unwrap() {
                t["review_status"] = json!("reviewed");
            }
        })
        .unwrap();
        assert_eq!(ok.listed_allowlist().len(), 1);
        let row = ok.listed_allowlist()[0];
        assert_eq!((row.max_token_inputs, row.max_token_outputs), (8, 8));
        assert_eq!(row.suffix_len, ok.template("kcc20-ref-8x8").unwrap().suffix_len);
        let e = errs(check(|v| {
            tok(v, 0)["status"] = json!("listed");
            tok(v, 0)["lot_size"] = Value::Null;
            v["templates"][1]["review_status"] = json!("reviewed");
        }));
        assert!(e.iter().any(|x| matches!(x, ValidationError::NotListable { reason: "not verified against chain", .. })));
    }

    #[test]
    fn official_needs_the_genesis_check() {
        // official needs genesis_verified: true, which needs verified; a listed token without it trades with the
        // genesis_unverified warning (the executor may require it: `require_genesis_verified`)
        let reviewed = |v: &mut Value| {
            for t in v["templates"].as_array_mut().unwrap() {
                t["review_status"] = json!("reviewed");
            }
        };
        let e = errs(check(|v| {
            reviewed(v);
            tok(v, 0)["verified"] = json!(true);
            tok(v, 0)["official"] = json!(true);
            tok(v, 0)["status"] = json!("listed");
        }));
        // no genesis_verified, no genesis record (C1 and C2 both missing)
        assert_eq!(e.iter().filter(|x| matches!(x, ValidationError::Genesis { .. })).count(), 2, "{e:?}");
        let official = |f: &dyn Fn(&mut Value)| {
            check(|v| {
                reviewed(v);
                tok(v, 0)["verified"] = json!(true);
                tok(v, 0)["official"] = json!(true);
                tok(v, 0)["status"] = json!("listed");
                tok(v, 0)["genesis_verified"] = json!(true);
                tok(v, 0)["genesis"] = genesis_rec();
                f(v);
            })
        };
        assert!(official(&|_| {}).is_ok());
        // C2: official needs a record that found no live mint authority
        let no_record = |v: &mut Value| {
            tok(v, 0).as_object_mut().unwrap().remove("genesis");
        };
        let undetermined = |v: &mut Value| {
            tok(v, 0)["genesis"].as_object_mut().unwrap().remove("live_minters");
        };
        let live = |v: &mut Value| {
            tok(v, 0)["genesis"]["live_minters"] = json!([format!("{}:3", "cd".repeat(32))]);
            tok(v, 0)["warning"] = json!("active mint authority");
        };
        for (name, f) in
            [("no record", &no_record as &dyn Fn(&mut Value)), ("liveness undetermined", &undetermined), ("live minter", &live)]
        {
            let e = errs(official(f));
            assert!(
                e.iter().any(|x| matches!(x, ValidationError::Genesis { reason, .. } if reason.contains("live mint authority"))),
                "{name}: {e:?}"
            );
        }
        // a live minter needs a warning; a record needs genesis_verified; malformed record fields
        let base = |f: &dyn Fn(&mut Value)| {
            check(|v| {
                tok(v, 0)["verified"] = json!(true);
                tok(v, 0)["genesis_verified"] = json!(true);
                tok(v, 0)["genesis"] = genesis_rec();
                f(v);
            })
        };
        assert!(base(&|_| {}).is_ok());
        let lm = format!("{}:3", "cd".repeat(32));
        let e = errs(base(&|v| tok(v, 0)["genesis"]["live_minters"] = json!([lm.clone()])));
        assert!(e.iter().any(|x| matches!(x, ValidationError::Genesis { reason, .. } if reason.contains("needs a warning"))), "{e:?}");
        assert!(base(&|v| {
            tok(v, 0)["genesis"]["live_minters"] = json!([lm.clone()]);
            tok(v, 0)["warning"] = json!("active mint authority: the issuer can mint without limit");
        })
        .is_ok());
        for (name, f) in [
            ("no genesis_verified", &(|v: &mut Value| tok(v, 0)["genesis_verified"] = Value::Null) as &dyn Fn(&mut Value)),
            ("bad txid", &|v: &mut Value| tok(v, 0)["genesis"]["txid"] = json!("AB".repeat(32))),
            ("unordered outputs", &|v: &mut Value| tok(v, 0)["genesis"]["outputs"] = json!([2, 1])),
            ("no outputs", &|v: &mut Value| tok(v, 0)["genesis"]["outputs"] = json!([])),
            ("minter not a genesis output", &|v: &mut Value| tok(v, 0)["genesis"]["minter_outputs"] = json!([5])),
            ("negative supply", &|v: &mut Value| tok(v, 0)["genesis"]["supply"] = json!(-1)),
            ("empty source", &|v: &mut Value| tok(v, 0)["genesis"]["source"] = json!("")),
            ("bad outpoint", &|v: &mut Value| {
                tok(v, 0)["genesis"]["live_minters"] = json!(["xyz:1"]);
                tok(v, 0)["warning"] = json!("w");
            }),
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
        // checked and NOT clean: still a valid entry, it warns, and it can never be official or listed
        let r = check(|v| {
            tok(v, 0)["verified"] = json!(true);
            tok(v, 0)["genesis_verified"] = json!(false);
            tok(v, 0)["warning"] = json!("genesis output 2 is not an instance of the token program");
        })
        .unwrap();
        assert_eq!(r.tokens[0].warnings(), vec![TokenWarning::GenesisUnverified]);
        assert!(r.tokens[0].warning.as_deref().unwrap().contains("genesis output 2"));
        let e = errs(check(|v| tok(v, 0)["warning"] = json!("evil\u{202E}")));
        assert!(e.iter().any(|x| matches!(x, ValidationError::Genesis { .. })), "{e:?}");
        assert_eq!(TokenWarning::GenesisUnverified.as_str(), "genesis_unverified");
        assert_eq!(serde_json::to_value(TokenWarning::GenesisUnverified).unwrap(), json!("genesis_unverified"));
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
        // the example entry is pending review: the name says so (it stays tradable, see `Status::PendingReview`)
        assert_eq!(t.status, Status::PendingReview);
        assert!(name.ends_with("[unverified, pending review]"));
        let mut v = t.clone();
        v.verified = true;
        assert!(display_name(&v).ends_with("[verified, pending review]"));
        v.status = Status::Listed;
        assert!(display_name(&v).ends_with("[verified]"));
        v.verified = false;
        assert!(display_name(&v).ends_with("[unverified]"));
        // a pending-review token is never shown as official, even with the flag set
        v.status = Status::PendingReview;
        v.official = true;
        assert!(display_name(&v).ends_with("[unverified, pending review]"));
        v.status = Status::Listed;
        v.verified = true;
        assert!(display_name(&v).ends_with("[official]"));
        v.status = Status::Delisted;
        assert!(display_name(&v).ends_with("[delisted]"));
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
        full_tok["official"] = json!(true);
        full_tok["genesis_verified"] = json!(true);
        full_tok["genesis"] = json!({"txid": "11".repeat(32), "daa_score": 1, "outputs": [1, 2], "supply": 3, "minter_outputs": [],
            "live_minters": [], "checked_at_daa": 4, "source": "s"});
        full_tok["warning"] = json!("w");
        // the legacy lot and tick (read and ignored) are declared optional by the schema
        full_tok["lot_size"] = json!(1);
        full_tok["tick"] = json!(1);
        full_tok["max_token_inputs"] = json!(8);
        full_tok["max_token_outputs"] = json!(8);
        full_tok["display"] = json!({"description": "d", "website": "https://e.x", "icon": "https://e.x/i.png", "kcc23": {}});
        let full_tok_typed: Token = serde_json::from_value(full_tok.clone()).unwrap();
        let full_tok = serde_json::to_value(&full_tok_typed).unwrap();
        assert_eq!(keys(&full_tok), props(&defs["token"]), "token properties");
        assert_eq!(keys(&full_tok["display"]), props(&defs["display"]), "display properties");
        assert_eq!(keys(&full_tok["genesis"]), props(&defs["genesis_record"]), "genesis record properties");
        let optional_gen: BTreeSet<String> = ["live_minters".to_string()].into_iter().collect();
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
        let optional_tok: BTreeSet<String> = [
            "max_token_inputs",
            "max_token_outputs",
            "display",
            "official",
            "genesis_verified",
            "genesis",
            "warning",
            "lot_size",
            "tick",
        ]
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
        assert_eq!(enum_of(&defs["status"]), Status::ALL);
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
        for s in Status::ALL {
            serde_json::from_value::<Status>(json!(s)).unwrap();
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
}
