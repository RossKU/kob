//! Contract artifacts with template-hash pinning.
//!
//! Every KOB order kind (in each token family) and every supported token program is one template:
//! a fixed prefix and suffix around a fixed-size state span. KCC-20 programs and the KOB order
//! kinds are SilverScript artifacts; the KRON token programs are raw pinned bytes
//! (`contracts/adapters/kron/templates/*.bin`, [`TokenTemplate`]). The committed `silverc` artifacts under
//! `contracts/artifacts` are embedded at compile time (the wasm build needs no I/O) and checked on
//! first use:
//!
//! * the artifact's recorded template hash equals `template_hash(prefix, suffix)` recomputed from
//!   its bytecode (blake3 over `len(prefix) ‖ prefix ‖ len(suffix) ‖ suffix`, the rule the
//!   covenants use to authenticate each other);
//! * that hash equals the hash pinned in [`PINNED`] (a changed artifact is a changed protocol);
//! * the build constants each template inlines (the evidence templates KobAsk / KobBid of the
//!   conditional and if-done kinds, the exit templates, the pair orders' evidence and exit templates) match the other
//!   embedded templates.
//!
//! [`load_artifact`] applies the same checks to an artifact supplied at run time.
//!
//! No template depends on a network or a genesis (a stop is armed by a fill in the same
//! transaction, there is no receipt covenant), so these artifacts are the same on every network.
//! The `deploy-tn10` and `deploy-mainnet` features only record the network the binaries are built for
//! ([`deployment_network`], `contracts/deploy/testnet-10`, `contracts/deploy/mainnet`); the mainnet record also pins
//! the token registry the build embeds ([`deployment_registry_sha256`]).

use std::collections::BTreeMap;
use std::sync::OnceLock;

use kaspa_consensus_core::tx::ScriptPublicKey;
use kaspa_txscript::pay_to_script_hash_script;
use serde::{Deserialize, Serialize};
use silverscript_abi::{ArtifactValue, SilAbiArtifact, SilContractArtifact};

use crate::family::Family;
use crate::json::to_hex;
pub use crate::profile::EmbeddedSource;

/// Errors raised while loading or pinning a compiled artifact.
#[derive(Debug, thiserror::Error)]
pub enum ArtifactError {
    /// The artifact JSON did not match the `SilAbiArtifact` schema.
    #[error("invalid artifact json: {0}")]
    Json(#[from] serde_json::Error),
    /// The artifact must contain exactly one contract.
    #[error("expected exactly one contract in the artifact, found {0}")]
    ContractCount(usize),
    /// The state span lies outside the bytecode.
    #[error("state span {offset}+{len} outside bytecode of {size} bytes")]
    Span { offset: usize, len: usize, size: usize },
    /// The recorded template hash does not match the bytecode.
    #[error("{name}: recorded template hash {recorded} != recomputed {computed}")]
    HashMismatch { name: String, recorded: String, computed: String },
    /// The template hash is not the pinned one.
    #[error("{name}: template hash {got} is not the pinned {pinned}")]
    NotPinned { name: String, got: String, pinned: String },
    /// A network constant inlined in a template disagrees with the embedded templates.
    #[error("network constant mismatch: {0}")]
    Constant(String),
}

/// Adversarial tests only (feature `adversarial`, never in a release build): a per-thread record of every P2SH script
/// public key this library derives (order, token and router templates) with the template and the state span it was
/// derived from, so a test that mutates transactions can name the owner and the state of every output it sees.
#[cfg(feature = "adversarial")]
pub mod spk_trace {
    use std::cell::RefCell;
    use std::collections::HashMap;

    use kaspa_consensus_core::tx::ScriptPublicKey;

    use super::TemplateId;

    /// The template a recorded script public key is an instance of.
    #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
    pub enum Origin {
        Template(TemplateId),
        Router(&'static str),
    }

    type Map = HashMap<Vec<u8>, (Origin, Vec<u8>)>;

    thread_local! {
        static MAP: RefCell<Map> = RefCell::new(HashMap::new());
    }

    pub(crate) fn record(spk: &ScriptPublicKey, origin: Origin, state: &[u8]) {
        MAP.with(|m| {
            m.borrow_mut().entry(spk.script().to_vec()).or_insert_with(|| (origin, state.to_vec()));
        });
    }

    /// Template and state span of a script public key derived on this thread, if any.
    pub fn lookup(spk: &ScriptPublicKey) -> Option<(Origin, Vec<u8>)> {
        MAP.with(|m| m.borrow().get(spk.script()).cloned())
    }

    /// Number of script public keys recorded on this thread.
    pub fn len() -> usize {
        MAP.with(|m| m.borrow().len())
    }

    /// True when nothing is recorded on this thread.
    pub fn is_empty() -> bool {
        len() == 0
    }

    /// Forgets every script public key recorded on this thread.
    pub fn clear() {
        MAP.with(|m| m.borrow_mut().clear());
    }
}

// The token-program variants of `TemplateId` and their embedded sources come from the profile table `data/programs.json`
// (`build.rs`, `crate::profile`): adding a program of an existing codec family is a profile entry, not code.
include!(concat!(env!("OUT_DIR"), "/token_programs.rs"));

/// Number of KOB order kinds (`TemplateId` variants that are not token programs).
pub const ORDER_KIND_COUNT: usize = 15;

macro_rules! define_template_id {
    (
        kcc20: [$(($kv:ident, $kn:literal, $kd:literal)),* $(,)?],
        kron: [$(($rv:ident, $rn:literal, $rd:literal)),* $(,)?]
    ) => {
        /// Every template the library knows: the KOB order kinds (artifact file names) and the token programs of the profile
        /// table (`data/programs.json`; serde and [`TemplateId::name`] use the profile `name`). Variant order: the KCC-20 order
        /// kinds and pair kinds, the KCC-20-codec programs, the KRON order kinds, the KRON-codec programs (each in table order).
        #[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
        pub enum TemplateId {
            KobAsk,
            KobBid,
            KobCondAsk,
            KobCondBid,
            KobIfdBid,
            KobIfdAsk,
            /// Plain order of a token/token pair A/B (ask or bid, IOC / FOK, decay, TWAP / DCA): ONE template for both sides and
            /// both families of A and B (state fields `side`, `sFamily`, `tFamily`). Kind code 0x08.
            KobPair,
            /// Conditional order of a pair (stop, stop-limit, trailing, take-profit / limit leg, OCO; the exits of
            /// `KobIfdPair`): one template for both sides and families. Kind code 0x09.
            KobCondPair,
            /// If-done entry of a pair (IFD / IFO / bracket / repeat; buy-first and sell-first): one template for both sides and
            /// families. Kind code 0x0a.
            KobIfdPair,
            $( #[doc = $kd] #[serde(rename = $kn)] $kv, )*
            // ---- KRON family (46-byte token state)
            KobAskKron,
            KobBidKron,
            KobCondAskKron,
            KobCondBidKron,
            KobIfdBidKron,
            KobIfdAskKron,
            $( #[doc = $rd] #[serde(rename = $rn)] $rv, )*
        }

        impl TemplateId {
            /// All embedded templates (order kinds and token programs), in variant order.
            pub const ALL: [TemplateId; ORDER_KIND_COUNT + TOKEN_PROGRAM_COUNT] = [
                TemplateId::KobAsk,
                TemplateId::KobBid,
                TemplateId::KobCondAsk,
                TemplateId::KobCondBid,
                TemplateId::KobIfdBid,
                TemplateId::KobIfdAsk,
                TemplateId::KobPair,
                TemplateId::KobCondPair,
                TemplateId::KobIfdPair,
                $( TemplateId::$kv, )*
                TemplateId::KobAskKron,
                TemplateId::KobBidKron,
                TemplateId::KobCondAskKron,
                TemplateId::KobCondBidKron,
                TemplateId::KobIfdBidKron,
                TemplateId::KobIfdAskKron,
                $( TemplateId::$rv, )*
            ];

            /// The token programs (both codec families), in variant order: one per profile of `data/programs.json`.
            pub const TOKEN_PROGRAMS: [TemplateId; TOKEN_PROGRAM_COUNT] = [$( TemplateId::$kv, )* $( TemplateId::$rv, )*];

            /// Name: the artifact file stem of an order kind, the profile `name` of a token program.
            pub fn name(self) -> &'static str {
                match self {
                    TemplateId::KobAsk => "KobAsk",
                    TemplateId::KobBid => "KobBid",
                    TemplateId::KobCondAsk => "KobCondAsk",
                    TemplateId::KobCondBid => "KobCondBid",
                    TemplateId::KobIfdBid => "KobIfdBid",
                    TemplateId::KobIfdAsk => "KobIfdAsk",
                    TemplateId::KobPair => "KobPair",
                    TemplateId::KobCondPair => "KobCondPair",
                    TemplateId::KobIfdPair => "KobIfdPair",
                    TemplateId::KobAskKron => "KobAskKron",
                    TemplateId::KobBidKron => "KobBidKron",
                    TemplateId::KobCondAskKron => "KobCondAskKron",
                    TemplateId::KobCondBidKron => "KobCondBidKron",
                    TemplateId::KobIfdBidKron => "KobIfdBidKron",
                    TemplateId::KobIfdAskKron => "KobIfdAskKron",
                    $( TemplateId::$kv => $kn, )*
                    $( TemplateId::$rv => $rn, )*
                }
            }

            /// True for the token programs (both codec families).
            pub fn is_token(self) -> bool {
                matches!(self, $( TemplateId::$kv )|* $( | TemplateId::$rv )*)
            }

            /// Token family of this template (order kinds: the family whose tokens they escrow; token programs: the profile
            /// `codec`). The pair kinds (`KobPair`, `KobCondPair`, `KobIfdPair`) serve both families (an instance's family is
            /// the family of its base token A, a state field): they report KCC-20 here, see [`TemplateId::serves`].
            pub fn family(self) -> Family {
                match self {
                    TemplateId::KobAskKron
                    | TemplateId::KobBidKron
                    | TemplateId::KobCondAskKron
                    | TemplateId::KobCondBidKron
                    | TemplateId::KobIfdBidKron
                    | TemplateId::KobIfdAskKron
                    $( | TemplateId::$rv )* => Family::Kron,
                    _ => Family::Kcc20,
                }
            }
        }
    };
}

with_token_programs!(define_template_id);

impl TemplateId {
    /// The KCC-20 counterpart of a KRON order kind (identity for everything else).
    pub fn base(self) -> TemplateId {
        match self {
            TemplateId::KobAskKron => TemplateId::KobAsk,
            TemplateId::KobBidKron => TemplateId::KobBid,
            TemplateId::KobCondAskKron => TemplateId::KobCondAsk,
            TemplateId::KobCondBidKron => TemplateId::KobCondBid,
            TemplateId::KobIfdBidKron => TemplateId::KobIfdBid,
            TemplateId::KobIfdAskKron => TemplateId::KobIfdAsk,
            other => other,
        }
    }

    /// The same order kind in `family` (`KobAsk` in `Kron` is `KobAskKron`); token programs and the pair kinds (one
    /// template for both families) are returned unchanged.
    pub fn in_family(self, family: Family) -> TemplateId {
        let b = self.base();
        if family == Family::Kcc20 {
            return b;
        }
        match b {
            TemplateId::KobAsk => TemplateId::KobAskKron,
            TemplateId::KobBid => TemplateId::KobBidKron,
            TemplateId::KobCondAsk => TemplateId::KobCondAskKron,
            TemplateId::KobCondBid => TemplateId::KobCondBidKron,
            TemplateId::KobIfdBid => TemplateId::KobIfdBidKron,
            TemplateId::KobIfdAsk => TemplateId::KobIfdAskKron,
            other => other,
        }
    }

    /// True when instances of this order template may escrow tokens of `family` (the pair kinds: both families).
    pub fn serves(self, family: Family) -> bool {
        self.kind_code().is_some() && self.in_family(family) == self
    }

    /// The token program's profile (`data/programs.json`); `None` for an order kind.
    pub fn profile(self) -> Option<&'static crate::profile::ProgramProfile> {
        if self.is_token() {
            Some(crate::profile::by_name(self.name()).unwrap_or_else(|| panic!("{}: no profile", self.name())))
        } else {
            None
        }
    }

    /// True for templates compiled from a SilverScript source (the order kinds, the token programs pinned as `silverc`
    /// artifacts); false for a token program pinned as a raw redeem script (the KRON programs).
    pub fn is_artifact(self) -> bool {
        self.profile().is_none_or(|p| p.source.kind == crate::profile::SourceKind::SilvercArtifact)
    }

    /// Parses an artifact name.
    pub fn from_name(name: &str) -> Option<TemplateId> {
        TemplateId::ALL.into_iter().find(|t| t.name() == name)
    }

    /// Compiler-owned context fields at the head of the program's state (Argent `runtime_states` field roles), which the
    /// KCC-1 actor-type handle fixes in the template prefix (profile `source.context_fields`: `gen__kcc20_template` of the
    /// `KCC20` actor of the published `KCC20PublicMint` app, which holds the program's own Sil template hash in every UTXO).
    /// Empty for every other template.
    pub fn context_fields(self) -> &'static [String] {
        self.profile().map(|p| p.source.context_fields.as_slice()).unwrap_or(&[])
    }

    /// Per-transaction token input / output slots of a token program (KCC-20: leader included; profile `slots`).
    pub fn token_slots(self) -> Option<(usize, usize)> {
        self.profile().map(|p| (p.slots.inputs, p.slots.outputs))
    }

    /// Smallest KAS value (sompi) a token program accepts on a token output it authorises (profile `carrier_floor`; `None`
    /// for a template that is not a token program). Measured in the script engine for every program (`kob-tests`
    /// `c6_diff`, `c6_7_token_output_floors_are_measured`): KaspaCom's KCC20 0.2.5 refuses a token output below 0.5 KAS;
    /// the reference programs, P2 and the KRON programs take any value. Every builder refuses a token output below it, and
    /// [`crate::build::check_new_order`] refuses order terms whose token outputs (delivery and exit carriers) would be.
    /// The storage-mass bound of a small output is separate ([`crate::tx::DUST_OUTPUT_MIN`]).
    pub fn min_token_output(self) -> Option<u64> {
        self.profile().map(|p| p.carrier_floor)
    }

    /// The executor's one-byte record-log code of a token program (profile `record_code`).
    pub fn record_code(self) -> Option<u8> {
        self.profile().map(|p| p.record_code)
    }

    /// Inverse of [`TemplateId::record_code`].
    pub fn from_record_code(code: u8) -> Option<TemplateId> {
        crate::profile::by_record_code(code).and_then(|p| TemplateId::from_name(&p.name))
    }

    /// The constant pushes a token input of this program carries after its witness column (profile
    /// `witness.trailing_pushes`, `kron-columns` only; empty for every other template).
    pub fn witness_trailing(self) -> Vec<Vec<u8>> {
        self.profile().map(|p| p.witness.trailing()).unwrap_or_default()
    }

    /// One-byte code used by the `KOB1` payload for the KOB order templates. A KRON kind has the
    /// code of its KCC-20 counterpart (the payload's family byte tells them apart). `0x07` (the retired
    /// trade receipt) stays reserved: no kind will ever reuse it ([`RETIRED_KIND_CODES`]).
    pub fn kind_code(self) -> Option<u8> {
        Some(match self.base() {
            TemplateId::KobAsk => 0x01,
            TemplateId::KobBid => 0x02,
            TemplateId::KobCondAsk => 0x03,
            TemplateId::KobCondBid => 0x04,
            TemplateId::KobIfdBid => 0x05,
            TemplateId::KobIfdAsk => 0x06,
            TemplateId::KobPair => 0x08,
            TemplateId::KobCondPair => 0x09,
            TemplateId::KobIfdPair => 0x0a,
            _ => return None,
        })
    }

    /// True for the pair kinds (one template for both sides and both families, two tokens per order).
    pub fn is_pair(self) -> bool {
        matches!(self, TemplateId::KobPair | TemplateId::KobCondPair | TemplateId::KobIfdPair)
    }

    /// Inverse of [`TemplateId::kind_code`] within a family over the pinned templates (`None` for a reserved code; `0x08`,
    /// `0x09`, `0x0a` are the pair kinds in both families).
    pub fn from_kind_code(family: Family, code: u8) -> Option<TemplateId> {
        TemplateId::ALL.into_iter().find(|t| t.kind_code() == Some(code) && t.serves(family))
    }
}

/// `KOB1` order-kind codes that are retired and reserved forever: `0x07` was the trade receipt
/// (`KobReceipt` / `KobReceiptKron`), which no longer exists.
pub const RETIRED_KIND_CODES: [u8; 1] = [0x07];

/// Template hashes of the KOB order kinds pinned by this protocol version (hex). A different artifact is a different
/// protocol and fails to load. The token programs are pinned by their profiles (`template_hash` of `data/programs.json`).
pub const PINNED: [(TemplateId, &str); ORDER_KIND_COUNT] = [
    (TemplateId::KobAsk, "070bb3b2425cc800c02e2e9465241a8b94abdce93e712fd3baf19474b63b887c"),
    (TemplateId::KobBid, "b995661f8b17c7c558b85975e361c26b00a2d57214cf631083a464b835e54fa3"),
    (TemplateId::KobCondAsk, "1e1285bf5a02448fdcfbc1623017a59fa64e61a95a4ea93693a42515a62ff1d1"),
    (TemplateId::KobCondBid, "2dcca63888b1ae85828a8cea5f1a934c8f1a256959982a3d4654a13abed327fa"),
    (TemplateId::KobIfdBid, "8ba34b33ef4780eb6b1928b37df582cda0daa57f3a76645cc7f6bbcbbaeff55d"),
    (TemplateId::KobIfdAsk, "ce1ccf8e84f61cc2cd42f43e5dec9106e709e258c3110209c052ef344c28333f"),
    (TemplateId::KobPair, "c95c92344f08c42992699b0e467cf32fe573dda9b879250438d60a13cab4a9c6"),
    (TemplateId::KobCondPair, "98b137f64c552ccbbd11f66e55e2b5ffe18ff8500c924903b900fe1c4a779374"),
    (TemplateId::KobIfdPair, "a9c04066e3b332a8108b7caf9b17b59966e5ce62d4eaff57f8a202654ee67c5a"),
    (TemplateId::KobAskKron, "f7274b79b081fbbf05d14b006359883c144304adb0ec0c6f9b8741feaef8f76d"),
    (TemplateId::KobBidKron, "6ec1a3dd4a287b73295a08db5f75fedcac4966539d793e9d1a659711ad888efc"),
    (TemplateId::KobCondAskKron, "27fa0ee2b17e75aabb6cccf6cd68f3f56568caeb4170031e44e3cd954947d3b7"),
    (TemplateId::KobCondBidKron, "296a2ffb289ee74abd936c2e1bc25692638d7efc5accfcd8cc2c9b9e96774dd3"),
    (TemplateId::KobIfdBidKron, "b705e2c3db279403276d0d0ccfe1e26dd0775620e383d13c98a2dca00892a335"),
    (TemplateId::KobIfdAskKron, "4797dc300d40ce968d8f26000c890e74f85f1c0d24ee227dac936a69ec034d12"),
];

/// The embedded artifact of an artifact-backed template (an order kind, or a token program whose profile source is a
/// `silverc` artifact).
fn embedded_json(id: TemplateId) -> &'static str {
    match id {
        TemplateId::KobAsk => include_str!("../../../contracts/artifacts/KobAsk.json"),
        TemplateId::KobBid => include_str!("../../../contracts/artifacts/KobBid.json"),
        TemplateId::KobCondAsk => include_str!("../../../contracts/artifacts/KobCondAsk.json"),
        TemplateId::KobCondBid => include_str!("../../../contracts/artifacts/KobCondBid.json"),
        TemplateId::KobIfdBid => include_str!("../../../contracts/artifacts/KobIfdBid.json"),
        TemplateId::KobIfdAsk => include_str!("../../../contracts/artifacts/KobIfdAsk.json"),
        TemplateId::KobPair => include_str!("../../../contracts/artifacts/KobPair.json"),
        TemplateId::KobCondPair => include_str!("../../../contracts/artifacts/KobCondPair.json"),
        TemplateId::KobIfdPair => include_str!("../../../contracts/artifacts/KobIfdPair.json"),
        TemplateId::KobAskKron => include_str!("../../../contracts/artifacts/KobAskKron.json"),
        TemplateId::KobBidKron => include_str!("../../../contracts/artifacts/KobBidKron.json"),
        TemplateId::KobCondAskKron => include_str!("../../../contracts/artifacts/KobCondAskKron.json"),
        TemplateId::KobCondBidKron => include_str!("../../../contracts/artifacts/KobCondBidKron.json"),
        TemplateId::KobIfdBidKron => include_str!("../../../contracts/artifacts/KobIfdBidKron.json"),
        TemplateId::KobIfdAskKron => include_str!("../../../contracts/artifacts/KobIfdAskKron.json"),
        program => match embedded_program_source(program.name()) {
            Some(EmbeddedSource::Artifact(json)) => json,
            _ => unreachable!("{} is pinned as raw bytes and has no artifact", program.name()),
        },
    }
}

/// The raw bytes of a token program pinned as a raw redeem script (profile source `raw-redeem`: the redeem script of one
/// instance, `prefix ‖ state ‖ suffix`).
fn raw_program_bytes(id: TemplateId) -> &'static [u8] {
    match embedded_program_source(id.name()) {
        Some(EmbeddedSource::Raw(b)) => b,
        _ => unreachable!("{} is not pinned as a raw redeem script", id.name()),
    }
}

// Length of the KRON token state span (`0x20 owner | 0x01 id_type | 0x08 amount | 0x01 is_minter`).
pub const KRON_STATE_LEN: usize = 46;

/// Constructor files that carry build constants (checked against the templates).
const CTOR_COND_ASK: &str = include_str!("../../../contracts/v2/KobCondAsk.ctor.json");
const CTOR_COND_BID: &str = include_str!("../../../contracts/v2/KobCondBid.ctor.json");
const CTOR_IFD_BID: &str = include_str!("../../../contracts/v2/KobIfdBid.ctor.json");
const CTOR_IFD_ASK: &str = include_str!("../../../contracts/v2/KobIfdAsk.ctor.json");
const CTOR_COND_PAIR: &str = include_str!("../../../contracts/v2/KobCondPair.ctor.json");
const CTOR_IFD_PAIR: &str = include_str!("../../../contracts/v2/KobIfdPair.ctor.json");
const KRON_CTOR_COND_ASK: &str = include_str!("../../../contracts/adapters/kron/v2/KobCondAskKron.ctor.json");
const KRON_CTOR_COND_BID: &str = include_str!("../../../contracts/adapters/kron/v2/KobCondBidKron.ctor.json");
const KRON_CTOR_IFD_BID: &str = include_str!("../../../contracts/adapters/kron/v2/KobIfdBidKron.ctor.json");
const KRON_CTOR_IFD_ASK: &str = include_str!("../../../contracts/adapters/kron/v2/KobIfdAskKron.ctor.json");

/// A pinned token program of either family: the redeem script of a token UTXO is
/// `prefix ‖ state ‖ suffix`. KCC-20 programs come from their artifacts, KRON programs from the raw
/// pinned bytes.
#[derive(Clone, Debug)]
pub struct TokenTemplate {
    pub id: TemplateId,
    pub family: Family,
    pub prefix: Vec<u8>,
    pub suffix: Vec<u8>,
    pub state_len: usize,
    /// blake3 template hash (`template_hash(prefix, suffix)`).
    pub hash: [u8; 32],
    /// Token inputs / outputs one transfer may carry.
    pub slots: (usize, usize),
}

impl TokenTemplate {
    /// Redeem script of an instance: `prefix ‖ state ‖ suffix`.
    pub fn redeem(&self, state: &[u8]) -> Vec<u8> {
        assert_eq!(state.len(), self.state_len, "{}: state span must be {} bytes", self.id.name(), self.state_len);
        [self.prefix.as_slice(), state, self.suffix.as_slice()].concat()
    }

    /// P2SH script public key of an instance.
    pub fn spk(&self, state: &[u8]) -> ScriptPublicKey {
        let spk = pay_to_script_hash_script(&self.redeem(state));
        #[cfg(feature = "adversarial")]
        spk_trace::record(&spk, spk_trace::Origin::Template(self.id), state);
        spk
    }

    /// State span of a redeem script, if the script is an instance of this program.
    pub fn state_of<'a>(&self, redeem: &'a [u8]) -> Option<&'a [u8]> {
        if redeem.len() != self.prefix.len() + self.state_len + self.suffix.len()
            || !redeem.starts_with(&self.prefix)
            || !redeem.ends_with(&self.suffix)
        {
            return None;
        }
        Some(&redeem[self.prefix.len()..self.prefix.len() + self.state_len])
    }

    /// Hex of the template hash.
    pub fn hash_hex(&self) -> String {
        to_hex(&self.hash)
    }
}

/// A loaded, hash-checked template.
#[derive(Clone, Debug)]
pub struct Template {
    /// Which template this is (for [`load_artifact`] results: the pinned id it matched).
    pub id: TemplateId,
    /// Contract name inside the artifact.
    pub contract_name: String,
    /// The parsed artifact (entry ABIs, state layout, compiled example instance).
    pub artifact: SilAbiArtifact,
    /// Bytecode before the state span.
    pub prefix: Vec<u8>,
    /// Bytecode after the state span.
    pub suffix: Vec<u8>,
    /// Length of the encoded state span.
    pub state_len: usize,
    /// blake3 template hash (see the module docs).
    pub hash: [u8; 32],
    /// Template hash of the compiler's state cut (`compiled.template_hash` of the artifact). Equal to [`Template::hash`]
    /// except for a program whose state begins with compiler-owned context fields ([`TemplateId::context_fields`]):
    /// KOB moves them into the prefix (the KCC-1 actor-type handle), and `hash` is the handle's.
    pub sil_hash: [u8; 32],
}

impl Template {
    /// The single contract of the artifact.
    pub fn contract(&self) -> &SilContractArtifact {
        &self.artifact.contracts[&self.contract_name]
    }

    /// Redeem script of an instance: `prefix ‖ state ‖ suffix`.
    pub fn redeem(&self, state: &[u8]) -> Vec<u8> {
        assert_eq!(state.len(), self.state_len, "{}: state span must be {} bytes", self.id.name(), self.state_len);
        [self.prefix.as_slice(), state, self.suffix.as_slice()].concat()
    }

    /// P2SH script public key of an instance.
    pub fn spk(&self, state: &[u8]) -> ScriptPublicKey {
        let spk = pay_to_script_hash_script(&self.redeem(state));
        #[cfg(feature = "adversarial")]
        spk_trace::record(&spk, spk_trace::Origin::Template(self.id), state);
        spk
    }

    /// Size of every redeem script of this template.
    pub fn redeem_len(&self) -> usize {
        self.prefix.len() + self.state_len + self.suffix.len()
    }

    /// State span of a redeem script, if the script is an instance of this template.
    pub fn state_of<'a>(&self, redeem: &'a [u8]) -> Option<&'a [u8]> {
        if redeem.len() != self.redeem_len() || !redeem.starts_with(&self.prefix) || !redeem.ends_with(&self.suffix) {
            return None;
        }
        Some(&redeem[self.prefix.len()..self.prefix.len() + self.state_len])
    }

    /// Hex of the template hash.
    pub fn hash_hex(&self) -> String {
        to_hex(&self.hash)
    }

    /// Dispatch tags of the entries (name -> 4-byte tag hex).
    pub fn entries(&self) -> BTreeMap<String, String> {
        self.contract().entries.iter().map(|(k, e)| (k.clone(), e.dispatch_tag.to_hex())).collect()
    }
}

/// Splits and checks a parsed artifact (single contract, span in range, recorded hash).
pub fn template_from_artifact(id: TemplateId, artifact: SilAbiArtifact) -> Result<Template, ArtifactError> {
    if artifact.contracts.len() != 1 {
        return Err(ArtifactError::ContractCount(artifact.contracts.len()));
    }
    let (contract_name, contract) = artifact.contracts.iter().next().expect("one contract");
    let bytecode = &contract.compiled.bytecode;
    let span = contract.compiled.state_span;
    let (offset, len) = (span.offset, span.len);
    if offset + len > bytecode.len() {
        return Err(ArtifactError::Span { offset, len, size: bytecode.len() });
    }
    let mut prefix = bytecode[..offset].to_vec();
    let suffix = bytecode[offset + len..].to_vec();
    let computed = silverscript_abi::template_hash(&prefix, &suffix);
    let recorded = contract.compiled.template_hash;
    if computed != recorded {
        return Err(ArtifactError::HashMismatch { name: id.name().into(), recorded: to_hex(&recorded), computed: to_hex(&computed) });
    }
    let mut state_len = len;
    let context = id.context_fields();
    if !context.is_empty() {
        // The actor-type handle (KCC-1 template view): the leading compiler-owned fields, each a `byte[32]` template
        // commitment whose canonical value is the program's own Sil template hash, move into the prefix in their canonical
        // encoding (`0x20 ‖ hash`); the rest of the state stays open.
        let fields = &contract.runtime_state.fields;
        let ctx_err = |m: String| ArtifactError::Constant(format!("{}: {m}", id.name()));
        let mut at = offset;
        for (k, want) in context.iter().enumerate() {
            let f = fields.get(k).ok_or_else(|| ctx_err(format!("no runtime field {k}")))?;
            if f.name != *want || !matches!(f.ty, silverscript_abi::TypeArtifact::FixedBytes { len: 32 }) {
                return Err(ctx_err(format!("runtime field {k} is {} ({:?}), not the context field byte[32] {want}", f.name, f.ty)));
            }
            let enc = [&[0x20u8][..], &computed[..]].concat();
            if bytecode.get(at..at + enc.len()) != Some(enc.as_slice()) {
                return Err(ctx_err(format!("{want} of the compiled instance is not the canonical 0x20 ‖ {}", to_hex(&computed))));
            }
            prefix.extend_from_slice(&enc);
            at += enc.len();
        }
        state_len = offset + len - at;
    }
    let hash = silverscript_abi::template_hash(&prefix, &suffix);
    Ok(Template {
        id,
        contract_name: contract_name.clone(),
        prefix,
        suffix,
        state_len,
        hash,
        sil_hash: computed,
        artifact: artifact.clone(),
    })
}

/// Parse an artifact produced by `silverc` (see `scripts/build-contracts.sh`), or an Argent app-bundle artifact (see
/// [`parse_sil_abi`]).
pub fn parse_artifact(json: &str) -> Result<SilAbiArtifact, ArtifactError> {
    Ok(parse_sil_abi(json)?.0)
}

/// Which artifact schema a program file is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ArtifactSchema {
    /// The compiler's own ABI artifact (`schema_version, compiler_version, structs, contracts`).
    Silverc,
    /// An Argent app bundle (`schema_version, id, generator, app, ..., argent, sil_abi`): the compiler artifact is its
    /// `sil_abi` member (the bundle records the compiler in `generator`, not in a `compiler_version` field).
    Argent,
}

/// Parses a program artifact of either schema and returns the compiler artifact and the schema found. An object with a
/// `sil_abi` object and an `argent` member and no `contracts` of its own is an Argent bundle; anything else is read as a
/// silverc artifact (so a malformed one still reports its own missing field).
pub fn parse_sil_abi(json: &str) -> Result<(SilAbiArtifact, ArtifactSchema), serde_json::Error> {
    let value: serde_json::Value = serde_json::from_str(json)?;
    let is_bundle = value
        .as_object()
        .is_some_and(|o| !o.contains_key("contracts") && o.contains_key("argent") && o.get("sil_abi").is_some_and(|s| s.is_object()));
    if is_bundle {
        let inner = value.get("sil_abi").cloned().expect("checked above");
        return Ok((serde_json::from_value(inner)?, ArtifactSchema::Argent));
    }
    Ok((serde_json::from_value(value)?, ArtifactSchema::Silverc))
}

/// Loads an artifact supplied at run time and requires its template hash to be the pinned hash
/// of `id` (hash pinning for artifacts not taken from this build).
pub fn load_artifact(id: TemplateId, json: &str) -> Result<Template, ArtifactError> {
    let t = template_from_artifact(id, parse_artifact(json)?)?;
    let pinned = pinned_hash(id);
    if t.hash != pinned {
        return Err(ArtifactError::NotPinned { name: id.name().into(), got: to_hex(&t.hash), pinned: to_hex(&pinned) });
    }
    Ok(t)
}

/// Pinned hash of a template.
pub fn pinned_hash(id: TemplateId) -> [u8; 32] {
    if let Some(p) = id.profile() {
        return p.hash();
    }
    let hex = PINNED.iter().find(|(t, _)| *t == id).expect("every order kind is pinned").1;
    crate::json::hex32(hex).expect("pinned hex")
}

struct Registry {
    templates: BTreeMap<TemplateId, Template>,
    tokens: BTreeMap<TemplateId, TokenTemplate>,
}

fn ctor(json: &str) -> Vec<ArtifactValue> {
    serde_json::from_str(json).expect("embedded ctor json")
}

fn ctor_bytes(v: &ArtifactValue) -> Vec<u8> {
    match v {
        ArtifactValue::Bytes(b) => b.clone(),
        other => panic!("expected bytes constant, got {other:?}"),
    }
}

fn ctor_int(v: &ArtifactValue) -> i64 {
    match v {
        ArtifactValue::Int(i) => *i,
        other => panic!("expected int constant, got {other:?}"),
    }
}

fn check_constants(t: &BTreeMap<TemplateId, Template>, fam: Family) -> Result<(), ArtifactError> {
    fn err<T>(m: String) -> Result<T, ArtifactError> {
        Err(ArtifactError::Constant(m))
    }
    // The templates of this family, by their KCC-20 name.
    let id = |base: TemplateId| base.in_family(fam);
    let (c_cond_ask, c_cond_bid, c_ifd_bid, c_ifd_ask) = match fam {
        Family::Kcc20 => (CTOR_COND_ASK, CTOR_COND_BID, CTOR_IFD_BID, CTOR_IFD_ASK),
        Family::Kron => (KRON_CTOR_COND_ASK, KRON_CTOR_COND_BID, KRON_CTOR_IFD_BID, KRON_CTOR_IFD_ASK),
    };
    let tpl_ref =
        |args: &[ArtifactValue], at: usize, want_id: TemplateId, with_lens: bool, who: TemplateId| -> Result<(), ArtifactError> {
            let want = &t[&want_id];
            if ctor_bytes(&args[at]) != want.hash {
                return err(format!("{} inlines a {} template hash that is not the embedded one", who.name(), want_id.name()));
            }
            if with_lens
                && (ctor_int(&args[at + 1]) != want.prefix.len() as i64 || ctor_int(&args[at + 2]) != want.suffix.len() as i64)
            {
                return err(format!("{} inlines {} prefix/suffix lengths that do not match", who.name(), want_id.name()));
            }
            Ok(())
        };
    // Conditional orders: the evidence templates (the plain ask, then the plain bid of their family).
    let ca = ctor(c_cond_ask);
    let cb = ctor(c_cond_bid);
    for (args, who) in [(&ca, id(TemplateId::KobCondAsk)), (&cb, id(TemplateId::KobCondBid))] {
        tpl_ref(args, 0, id(TemplateId::KobAsk), true, who)?;
        tpl_ref(args, 3, id(TemplateId::KobBid), true, who)?;
    }
    // If-done entries: exit template (hash, prefix, suffix), then the evidence template of their stop
    // entry (a buy-stop entry reads a resting bid, a sell-stop entry a resting ask).
    let ib = ctor(c_ifd_bid);
    let ia = ctor(c_ifd_ask);
    tpl_ref(&ib, 0, id(TemplateId::KobCondAsk), true, id(TemplateId::KobIfdBid))?;
    tpl_ref(&ia, 0, id(TemplateId::KobCondBid), true, id(TemplateId::KobIfdAsk))?;
    tpl_ref(&ib, 3, id(TemplateId::KobBid), true, id(TemplateId::KobIfdBid))?;
    tpl_ref(&ia, 3, id(TemplateId::KobAsk), true, id(TemplateId::KobIfdAsk))?;
    // Pair kinds (one template for both families, checked once). KobCondPair: the trigger evidence templates ASK
    // (KobAsk), ASK_KRON, BID (KobBid), BID_KRON, PAIR (KobPair); KobIfdPair: COND (the exit KobCondPair), then the same five.
    let evidence_refs = [TemplateId::KobAsk, TemplateId::KobAskKron, TemplateId::KobBid, TemplateId::KobBidKron, TemplateId::KobPair];
    let cp = ctor(CTOR_COND_PAIR);
    let ip = ctor(CTOR_IFD_PAIR);
    let mut pair_needles: Vec<(TemplateId, TemplateId)> = vec![];
    if fam == Family::Kcc20 {
        for (k, want) in evidence_refs.into_iter().enumerate() {
            tpl_ref(&cp, 3 * k, want, true, TemplateId::KobCondPair)?;
            tpl_ref(&ip, 3 + 3 * k, want, true, TemplateId::KobIfdPair)?;
            pair_needles.push((TemplateId::KobCondPair, want));
            pair_needles.push((TemplateId::KobIfdPair, want));
        }
        tpl_ref(&ip, 0, TemplateId::KobCondPair, true, TemplateId::KobIfdPair)?;
        pair_needles.push((TemplateId::KobIfdPair, TemplateId::KobCondPair));
    }
    // The inlined constants must really be inside the template bytes (ctor file == artifact).
    let h = |x: TemplateId| t[&x].hash.to_vec();
    for (tid, needle) in [
        (id(TemplateId::KobCondAsk), h(id(TemplateId::KobAsk))),
        (id(TemplateId::KobCondAsk), h(id(TemplateId::KobBid))),
        (id(TemplateId::KobCondBid), h(id(TemplateId::KobAsk))),
        (id(TemplateId::KobCondBid), h(id(TemplateId::KobBid))),
        (id(TemplateId::KobIfdBid), h(id(TemplateId::KobCondAsk))),
        (id(TemplateId::KobIfdAsk), h(id(TemplateId::KobCondBid))),
        (id(TemplateId::KobIfdBid), h(id(TemplateId::KobBid))),
        (id(TemplateId::KobIfdAsk), h(id(TemplateId::KobAsk))),
    ]
    .into_iter()
    .chain(pair_needles.into_iter().map(|(who, want)| (who, h(want))))
    {
        let tp = &t[&tid];
        let hay = [tp.prefix.as_slice(), tp.suffix.as_slice()].concat();
        if !hay.windows(32).any(|w| w == needle.as_slice()) {
            return err(format!("{} does not contain its build constant {}", tid.name(), to_hex(&needle)));
        }
    }
    Ok(())
}

fn raw_token_template(id: TemplateId) -> Result<TokenTemplate, ArtifactError> {
    let p = id.profile().expect("a token program");
    let loaded = p.check_bytes(&EmbeddedSource::Raw(raw_program_bytes(id))).map_err(|e| ArtifactError::Constant(e.to_string()))?;
    let pinned = pinned_hash(id);
    if loaded.hash != pinned {
        return Err(ArtifactError::NotPinned { name: id.name().into(), got: to_hex(&loaded.hash), pinned: to_hex(&pinned) });
    }
    Ok(TokenTemplate {
        id,
        family: p.codec,
        prefix: loaded.prefix,
        suffix: loaded.suffix,
        state_len: p.layout.state_len,
        hash: loaded.hash,
        slots: (p.slots.inputs, p.slots.outputs),
    })
}

fn build_registry() -> Result<Registry, ArtifactError> {
    // the profile table (shape and uniqueness)
    crate::profile::ProfileFile::parse(crate::profile::PROGRAMS_JSON).map_err(|e| ArtifactError::Constant(e.to_string()))?;
    let mut templates = BTreeMap::new();
    let mut tokens = BTreeMap::new();
    for id in TemplateId::ALL {
        if id.is_artifact() {
            templates.insert(id, load_artifact(id, embedded_json(id))?);
        } else {
            tokens.insert(id, raw_token_template(id)?);
        }
    }
    for (id, t) in &templates {
        if let Some(p) = id.profile() {
            // every profile fact against the bytes (layout, hash, state, entries)
            p.check_bytes(&EmbeddedSource::Artifact(embedded_json(*id))).map_err(|e| ArtifactError::Constant(e.to_string()))?;
            tokens.insert(
                *id,
                TokenTemplate {
                    id: *id,
                    family: p.codec,
                    prefix: t.prefix.clone(),
                    suffix: t.suffix.clone(),
                    state_len: t.state_len,
                    hash: t.hash,
                    slots: (p.slots.inputs, p.slots.outputs),
                },
            );
        }
    }
    check_constants(&templates, Family::Kcc20)?;
    check_constants(&templates, Family::Kron)?;
    Ok(Registry { templates, tokens })
}

fn registry() -> &'static Registry {
    static REG: OnceLock<Registry> = OnceLock::new();
    REG.get_or_init(|| build_registry().unwrap_or_else(|e| panic!("embedded artifacts: {e}")))
}

/// Checks every embedded artifact (hash pinning and build constants) without panicking, so
/// front ends can report a broken build.
pub fn self_check() -> Result<(), String> {
    build_registry().map(|_| ()).map_err(|e| e.to_string())
}

/// An embedded artifact-backed template (order kinds, KCC-20 programs). The raw KRON token
/// programs have no artifact: use [`token_template`].
pub fn template(id: TemplateId) -> &'static Template {
    registry().templates.get(&id).unwrap_or_else(|| panic!("{} is not an artifact-backed template (see token_template)", id.name()))
}

/// [`template`] that returns `None` instead of panicking for a template without an artifact.
pub fn try_template(id: TemplateId) -> Option<&'static Template> {
    registry().templates.get(&id)
}

/// [`token_template`] that returns `None` instead of panicking for a non-token id.
pub fn try_token_template(id: TemplateId) -> Option<&'static TokenTemplate> {
    registry().tokens.get(&id)
}

/// Looks up an embedded artifact-backed template by its hash.
pub fn template_by_hash(hash: &[u8; 32]) -> Option<&'static Template> {
    registry().templates.values().find(|t| &t.hash == hash)
}

/// An embedded token program of either family.
pub fn token_template(id: TemplateId) -> &'static TokenTemplate {
    registry().tokens.get(&id).unwrap_or_else(|| panic!("{} is not a token program", id.name()))
}

/// Looks up an embedded token program (either family) by its template hash.
pub fn token_template_by_hash(hash: &[u8; 32]) -> Option<&'static TokenTemplate> {
    registry().tokens.values().find(|t| &t.hash == hash)
}

#[cfg(all(feature = "deploy-tn10", feature = "deploy-mainnet"))]
compile_error!("the deployment features deploy-tn10 and deploy-mainnet are exclusive: build one network per binary");

/// Network of the deployment build compiled in (`Some("testnet-10")` with `deploy-tn10`, `Some("mainnet")` with
/// `deploy-mainnet`), `None` for the reference build. The templates are the same in all (nothing depends on a network).
pub fn deployment_network() -> Option<&'static str> {
    #[cfg(any(feature = "deploy-tn10", feature = "deploy-mainnet"))]
    return Some(deployment::NETWORK);
    #[cfg(not(any(feature = "deploy-tn10", feature = "deploy-mainnet")))]
    None
}

/// The registry the deployment build pins (`registry` of its `deployment.json`): its lowercase hex sha256 over the
/// LF-normalised bytes. `Some` only in a build whose record pins one (`deploy-mainnet`); the embedded default registry
/// ([`crate::registry::DEFAULT_REGISTRY_JSON`]) hashes to it (checked by this module's tests).
pub fn deployment_registry_sha256() -> Option<&'static str> {
    #[cfg(feature = "deploy-mainnet")]
    return Some(deployment::REGISTRY_SHA256);
    #[cfg(not(feature = "deploy-mainnet"))]
    None
}

/// The testnet-10 deployment record (`contracts/deploy/testnet-10`, written and verified by
/// `scripts/build-deploy.sh testnet-10`): the network and the template hashes deployed there, which are
/// the reference ones (no template depends on a genesis).
#[cfg(feature = "deploy-tn10")]
pub mod deployment {
    /// Network id of the deployment.
    pub const NETWORK: &str = "testnet-10";
    /// `deployment.json` of the build (network, template hashes; checked against the embedded templates).
    pub const MANIFEST: &str = include_str!("../../../contracts/deploy/testnet-10/deployment.json");
}

/// The mainnet deployment record (`contracts/deploy/mainnet`, written and verified by `scripts/build-deploy.sh mainnet`):
/// the network, the template hashes (the reference ones) and the pinned token registry (`registry/tokens.json`, which
/// [`crate::registry::DEFAULT_REGISTRY_JSON`] embeds). There is no genesis step: the build is reproducible from source.
#[cfg(feature = "deploy-mainnet")]
pub mod deployment {
    /// Network id of the deployment.
    pub const NETWORK: &str = "mainnet";
    /// `deployment.json` of the build (network, template hashes, registry pin; checked against what the build embeds).
    pub const MANIFEST: &str = include_str!("../../../contracts/deploy/mainnet/deployment.json");
    /// sha256 (lowercase hex, LF-normalised bytes) of the registry the record pins: the `registry.sha256` of [`MANIFEST`]
    /// and [`crate::registry::default_registry_sha256`] (checked by the tests).
    pub const REGISTRY_SHA256: &str = "dc3bd15389e548862a956ccded01cb6ecee51946d76241d7da1e028711322d10";
}

/// Summary of one template for front ends and documentation.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TemplateInfo {
    pub name: String,
    #[serde(with = "crate::json::field")]
    pub hash: [u8; 32],
    pub prefix_len: usize,
    pub state_len: usize,
    pub suffix_len: usize,
    pub entries: BTreeMap<String, String>,
    pub kind_code: Option<u8>,
    pub token_slots: Option<(usize, usize)>,
    /// Token programs: the smallest KAS value of a token output ([`TemplateId::min_token_output`]).
    #[serde(with = "crate::json::field", default, skip_serializing_if = "Option::is_none")]
    pub min_token_output: Option<u64>,
}

/// [`TemplateInfo`] of every embedded template.
pub fn template_infos() -> Vec<TemplateInfo> {
    TemplateId::ALL
        .into_iter()
        .map(|id| {
            if id.is_artifact() {
                let t = template(id);
                TemplateInfo {
                    name: id.name().into(),
                    hash: t.hash,
                    prefix_len: t.prefix.len(),
                    state_len: t.state_len,
                    suffix_len: t.suffix.len(),
                    entries: t.entries(),
                    kind_code: id.kind_code(),
                    token_slots: id.token_slots(),
                    min_token_output: id.min_token_output(),
                }
            } else {
                let t = token_template(id);
                TemplateInfo {
                    name: id.name().into(),
                    hash: t.hash,
                    prefix_len: t.prefix.len(),
                    state_len: t.state_len,
                    suffix_len: t.suffix.len(),
                    entries: BTreeMap::new(),
                    kind_code: None,
                    token_slots: Some(t.slots),
                    min_token_output: id.min_token_output(),
                }
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn embedded_artifacts_are_pinned_and_consistent() {
        self_check().unwrap();
        for id in TemplateId::ALL {
            if !id.is_artifact() {
                let t = token_template(id);
                assert_eq!(t.hash, pinned_hash(id), "{}", id.name());
                assert_eq!(token_template_by_hash(&t.hash).unwrap().id, id);
                assert_eq!(t.family, Family::Kron);
                let raw = raw_program_bytes(id);
                let (p, n) = (t.prefix.len(), t.state_len);
                assert_eq!((p, n), (id.profile().unwrap().layout.prefix_len, KRON_STATE_LEN));
                assert_eq!(t.redeem(&raw[p..p + n]), raw);
                continue;
            }
            let t = template(id);
            assert_eq!(t.hash, pinned_hash(id), "{}", id.name());
            assert_eq!(template_by_hash(&t.hash).unwrap().id, id);
            let c = t.contract();
            assert_eq!(t.redeem(&c.compiled.bytecode[t.prefix.len()..t.prefix.len() + t.state_len]), c.compiled.bytecode);
        }
        // The retired receipt kind code decodes to nothing in either family.
        for code in RETIRED_KIND_CODES {
            assert!(TemplateId::from_kind_code(Family::Kcc20, code).is_none());
            assert!(TemplateId::from_kind_code(Family::Kron, code).is_none());
        }
    }

    #[test]
    fn deployment_features_record_their_network() {
        let want = if cfg!(feature = "deploy-tn10") {
            Some("testnet-10")
        } else if cfg!(feature = "deploy-mainnet") {
            Some("mainnet")
        } else {
            None
        };
        assert_eq!(deployment_network(), want);
        assert_eq!(deployment_registry_sha256().is_some(), cfg!(feature = "deploy-mainnet"));
    }

    /// The mainnet record (checked in every build, whatever the features): it lists exactly the embedded order templates and
    /// pins the registry this crate embeds, so a template or registry change without `scripts/build-deploy.sh mainnet` fails here.
    #[test]
    fn mainnet_deployment_record_matches_the_embedded_templates_and_registry() {
        self_check().unwrap();
        let m: serde_json::Value = serde_json::from_str(include_str!("../../../contracts/deploy/mainnet/deployment.json")).unwrap();
        assert_eq!(m["network"], "mainnet");
        assert!(m.get("receipt_covenant_id").is_none(), "no receipt genesis since v2.6");
        let listed = m["templates"].as_array().unwrap();
        let kinds: Vec<TemplateId> = TemplateId::ALL.into_iter().filter(|t| t.kind_code().is_some()).collect();
        assert_eq!(listed.len(), kinds.len());
        for id in kinds {
            let e = listed.iter().find(|e| e["name"] == id.name()).unwrap_or_else(|| panic!("{} not listed", id.name()));
            assert_eq!(e["hash"], template(id).hash_hex(), "{}", id.name());
        }
        let reg = &m["registry"];
        assert_eq!(reg["path"], "registry/tokens.json");
        let pinned = crate::registry::default_registry_sha256();
        assert_eq!(reg["sha256"], pinned.as_str(), "registry/tokens.json changed: run scripts/build-deploy.sh mainnet");
        let d = crate::registry::Registry::default_registry();
        assert_eq!(d.network, "mainnet");
        let strict: Vec<&str> = reg["strict_templates"].as_array().unwrap().iter().map(|t| t["id"].as_str().unwrap()).collect();
        assert_eq!(strict, d.strict_templates().map(|t| t.id.as_str()).collect::<Vec<_>>());
        let tokens: Vec<(&str, bool)> = reg["listed"]
            .as_array()
            .unwrap()
            .iter()
            .map(|t| (t["ticker"].as_str().unwrap(), t["official"].as_bool().unwrap()))
            .collect();
        let want: Vec<(&str, bool)> =
            d.tokens.iter().filter(|t| t.status == crate::registry::Status::Listed).map(|t| (t.ticker.as_str(), t.official)).collect();
        assert_eq!(tokens, want);
        #[cfg(feature = "deploy-mainnet")]
        assert_eq!(deployment::REGISTRY_SHA256, pinned, "update deployment::REGISTRY_SHA256 with the record");
    }

    #[cfg(any(feature = "deploy-tn10", feature = "deploy-mainnet"))]
    #[test]
    fn deployment_record_lists_the_embedded_templates() {
        use deployment::*;
        self_check().unwrap();
        assert_eq!(deployment_network(), Some(NETWORK));
        // deployment.json (written by scripts/build-deploy.sh) lists exactly the embedded order templates.
        let m: serde_json::Value = serde_json::from_str(MANIFEST).unwrap();
        assert_eq!(m["network"], NETWORK);
        assert!(m.get("receipt_covenant_id").is_none(), "no receipt genesis since v2.6");
        let listed = m["templates"].as_array().unwrap();
        let kinds: Vec<TemplateId> = TemplateId::ALL.into_iter().filter(|t| t.kind_code().is_some()).collect();
        assert_eq!(listed.len(), kinds.len());
        for id in kinds {
            let e = listed.iter().find(|e| e["name"] == id.name()).unwrap_or_else(|| panic!("{} not listed", id.name()));
            assert_eq!(e["hash"], template(id).hash_hex(), "{}", id.name());
        }
    }

    #[test]
    fn tampered_artifact_is_rejected() {
        let json = embedded_json(TemplateId::KobAsk);
        // Pinning: a valid artifact under the wrong id is rejected.
        assert!(matches!(load_artifact(TemplateId::KobBid, json), Err(ArtifactError::NotPinned { .. })));
        // Integrity: change one bytecode byte outside the state span.
        let mut v: serde_json::Value = serde_json::from_str(json).unwrap();
        let bc = &mut v["contracts"]["KobAsk"]["compiled"]["bytecode"];
        let last = bc.as_array().unwrap().len() - 1;
        bc[last] = serde_json::Value::from(0x51);
        assert!(matches!(load_artifact(TemplateId::KobAsk, &v.to_string()), Err(ArtifactError::HashMismatch { .. })));
    }
}
