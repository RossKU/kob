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

/// Every template the library knows. The names are the artifact file names.
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
    #[serde(rename = "KCC20Ref")]
    Kcc20Ref,
    #[serde(rename = "KCC20Ref_4x5")]
    Kcc20Ref4x5,
    #[serde(rename = "KCC20Ref_8x8")]
    Kcc20Ref8x8,
    #[serde(rename = "KCC20Ref_16x16")]
    Kcc20Ref16x16,
    #[serde(rename = "KCC20P2")]
    Kcc20P2,
    /// KaspaCom's KCC20 0.2.5 (`contracts/third-party/kaspacom-kcc20`, Apache-2.0): a third-party
    /// KCC-20-compatible program (112-byte state, draft tags, owner scheme 0x04, 8 / 8 slots, 25.5 KB)
    /// accepted like every other program: through the strict template list of the registry.
    #[serde(rename = "KCC20KaspaCom_0_2_5")]
    Kcc20KaspaCom025,
    // ---- KRON family (46-byte token state)
    KobAskKron,
    KobBidKron,
    KobCondAskKron,
    KobCondBidKron,
    KobIfdBidKron,
    KobIfdAskKron,
    /// The common KRON token program (2,433 B).
    KronToken2433,
    /// The newer KRON token program (2,732 B).
    KronToken2732,
}

impl TemplateId {
    /// All embedded templates (artifact-backed order kinds and token programs, raw KRON programs).
    pub const ALL: [TemplateId; 23] = [
        TemplateId::KobAsk,
        TemplateId::KobBid,
        TemplateId::KobCondAsk,
        TemplateId::KobCondBid,
        TemplateId::KobIfdBid,
        TemplateId::KobIfdAsk,
        TemplateId::KobPair,
        TemplateId::KobCondPair,
        TemplateId::KobIfdPair,
        TemplateId::Kcc20Ref,
        TemplateId::Kcc20Ref4x5,
        TemplateId::Kcc20Ref8x8,
        TemplateId::Kcc20Ref16x16,
        TemplateId::Kcc20P2,
        TemplateId::Kcc20KaspaCom025,
        TemplateId::KobAskKron,
        TemplateId::KobBidKron,
        TemplateId::KobCondAskKron,
        TemplateId::KobCondBidKron,
        TemplateId::KobIfdBidKron,
        TemplateId::KobIfdAskKron,
        TemplateId::KronToken2433,
        TemplateId::KronToken2732,
    ];

    /// Artifact name (file stem under `contracts/artifacts`).
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
            TemplateId::Kcc20Ref => "KCC20Ref",
            TemplateId::Kcc20Ref4x5 => "KCC20Ref_4x5",
            TemplateId::Kcc20Ref8x8 => "KCC20Ref_8x8",
            TemplateId::Kcc20Ref16x16 => "KCC20Ref_16x16",
            TemplateId::Kcc20P2 => "KCC20P2",
            TemplateId::Kcc20KaspaCom025 => "KCC20KaspaCom_0_2_5",
            TemplateId::KobAskKron => "KobAskKron",
            TemplateId::KobBidKron => "KobBidKron",
            TemplateId::KobCondAskKron => "KobCondAskKron",
            TemplateId::KobCondBidKron => "KobCondBidKron",
            TemplateId::KobIfdBidKron => "KobIfdBidKron",
            TemplateId::KobIfdAskKron => "KobIfdAskKron",
            TemplateId::KronToken2433 => "KronToken2433",
            TemplateId::KronToken2732 => "KronToken2732",
        }
    }

    /// Token family of this template (order kinds: the family whose tokens they escrow). The pair kinds (`KobPair`,
    /// `KobCondPair`, `KobIfdPair`) serve both families (an instance's family is the family of its base token A, a state
    /// field): they report KCC-20 here, see [`TemplateId::serves`].
    pub fn family(self) -> Family {
        match self {
            TemplateId::KobAskKron
            | TemplateId::KobBidKron
            | TemplateId::KobCondAskKron
            | TemplateId::KobCondBidKron
            | TemplateId::KobIfdBidKron
            | TemplateId::KobIfdAskKron
            | TemplateId::KronToken2433
            | TemplateId::KronToken2732 => Family::Kron,
            _ => Family::Kcc20,
        }
    }

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

    /// True for templates compiled from a SilverScript source (an artifact under
    /// `contracts/artifacts`); false for the raw KRON token programs.
    pub fn is_artifact(self) -> bool {
        !matches!(self, TemplateId::KronToken2433 | TemplateId::KronToken2732)
    }

    /// Parses an artifact name.
    pub fn from_name(name: &str) -> Option<TemplateId> {
        TemplateId::ALL.into_iter().find(|t| t.name() == name)
    }

    /// True for the KCC-20 token programs.
    pub fn is_token(self) -> bool {
        matches!(
            self,
            TemplateId::Kcc20Ref
                | TemplateId::Kcc20Ref4x5
                | TemplateId::Kcc20Ref8x8
                | TemplateId::Kcc20Ref16x16
                | TemplateId::Kcc20P2
                | TemplateId::Kcc20KaspaCom025
                | TemplateId::KronToken2433
                | TemplateId::KronToken2732
        )
    }

    /// Per-transaction token input / output slots of a token program (KCC-20: leader included).
    pub fn token_slots(self) -> Option<(usize, usize)> {
        match self {
            TemplateId::Kcc20Ref | TemplateId::Kcc20P2 => Some((3, 3)),
            TemplateId::Kcc20Ref4x5 => Some((4, 5)),
            TemplateId::Kcc20Ref8x8 | TemplateId::Kcc20KaspaCom025 => Some((8, 8)),
            TemplateId::Kcc20Ref16x16 => Some((16, 16)),
            TemplateId::KronToken2433 | TemplateId::KronToken2732 => Some((4, 5)),
            _ => None,
        }
    }

    /// Smallest KAS value (sompi) a token program accepts on a token output it authorises (`None` for a template that is
    /// not a token program). Measured in the script engine for every program (`kob-tests` `c6_diff`,
    /// `c6_7_token_output_floors_are_measured`): KaspaCom's KCC20 0.2.5 refuses a token output below 0.5 KAS; the reference
    /// programs, P2 and the KRON programs take any value. Every builder refuses a token output below it, and
    /// [`crate::build::check_new_order`] refuses order terms whose token outputs (delivery and exit carriers) would be.
    /// The storage-mass bound of a small output is separate ([`crate::tx::DUST_OUTPUT_MIN`]).
    pub fn min_token_output(self) -> Option<u64> {
        match self {
            TemplateId::Kcc20KaspaCom025 => Some(50_000_000),
            t if t.is_token() => Some(1),
            _ => None,
        }
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

/// Template hashes pinned by this protocol version (hex). A different artifact is a different
/// protocol and fails to load.
pub const PINNED: [(TemplateId, &str); 23] = [
    (TemplateId::KobAsk, "126ff059375b459674df139dc644f6518cc26e831e998e9be7a4008c8c8e0aa1"),
    (TemplateId::KobBid, "b995661f8b17c7c558b85975e361c26b00a2d57214cf631083a464b835e54fa3"),
    (TemplateId::KobCondAsk, "40a8eb7321e9dc157ca05683fcf01c1967d0c2be91fb8644e02025b0df087b74"),
    (TemplateId::KobCondBid, "076e7bd8bfb6e1b59a8f2a19a6cad5961c7978368a0816a2b9df5f39d805bdfb"),
    (TemplateId::KobIfdBid, "50b58e7b26e0bdc1a0eed7f7d7e5bcee88f95ed6ff367be32b8e74979a9de292"),
    (TemplateId::KobIfdAsk, "bbcfc226d41f2dbf9db2f4835a55ed87b568fe43c46ac24ceee6831aa7b3f0b2"),
    (TemplateId::KobPair, "107388074abe4d223d6fc1e17f6d5aa09bd76a7b739aa2e82774516ddbef7d16"),
    (TemplateId::KobCondPair, "37ab667f662779e97e75ea712e855c073b4d2c9a5183285fa7ec11daf8ae0820"),
    (TemplateId::KobIfdPair, "9975b2f3131179217050165f51b844700cf9f99f47c5b7954dabf00cf0441d5c"),
    (TemplateId::Kcc20Ref, "f4ac029d2c3c74dd3dcaeb64245f7d0a0977e27c2956f3977540a11dc7c45b1f"),
    (TemplateId::Kcc20Ref4x5, "6bef67391fd2eb94c9d155591229b6d3676a8b7e8e609063bce1b308559ff12f"),
    (TemplateId::Kcc20Ref8x8, "40fef59a59bd76991f4d4e2101d1e3e34860997b89fe7714532637cec482a9d7"),
    (TemplateId::Kcc20Ref16x16, "8319cdc429d7f1b31045120c168e951a316c98fcde0c95c385e5157994ba9ab3"),
    (TemplateId::Kcc20P2, "16032d0d301c5d8cc7e5a737ac99af8265c6b6956e1e6785585daab566bc1a9c"),
    (TemplateId::Kcc20KaspaCom025, "911f0638ccb7368bf36d117f1725073ae7ee487ce8b58ca3e8375051c2d40f6c"),
    (TemplateId::KobAskKron, "f7274b79b081fbbf05d14b006359883c144304adb0ec0c6f9b8741feaef8f76d"),
    (TemplateId::KobBidKron, "6ec1a3dd4a287b73295a08db5f75fedcac4966539d793e9d1a659711ad888efc"),
    (TemplateId::KobCondAskKron, "6c4f92cee1613899b5d680e784b11fd567b1bfac63a5006018cc6c54cfd35839"),
    (TemplateId::KobCondBidKron, "fb392f88a136f14708841895b2efae2a5070749ddeb0168c7405806aea7ed953"),
    (TemplateId::KobIfdBidKron, "e5cff7ede2faeb58706cfd28e095dcefad552f1e3e92d8a12bd05a22c053496d"),
    (TemplateId::KobIfdAskKron, "d523d794bcb15adbfb4b212e92f48ca65a1276de808aecc34e9b82c6f9f014f6"),
    (TemplateId::KronToken2433, "2ed46a7edf5b168e67dba56998c58255235bebac436940a85115ca31d5c559f2"),
    (TemplateId::KronToken2732, "8097c96fe586a785b3ffb62ddd2a9b3012593421d605d136d26153806e28053e"),
];

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
        TemplateId::Kcc20Ref => include_str!("../../../contracts/artifacts/KCC20Ref.json"),
        TemplateId::Kcc20Ref4x5 => include_str!("../../../contracts/artifacts/KCC20Ref_4x5.json"),
        TemplateId::Kcc20Ref8x8 => include_str!("../../../contracts/artifacts/KCC20Ref_8x8.json"),
        TemplateId::Kcc20Ref16x16 => include_str!("../../../contracts/artifacts/KCC20Ref_16x16.json"),
        TemplateId::Kcc20P2 => include_str!("../../../contracts/artifacts/KCC20P2.json"),
        TemplateId::Kcc20KaspaCom025 => include_str!("../../../contracts/third-party/kaspacom-kcc20/KCC20.placeholder.json"),
        TemplateId::KobAskKron => include_str!("../../../contracts/artifacts/KobAskKron.json"),
        TemplateId::KobBidKron => include_str!("../../../contracts/artifacts/KobBidKron.json"),
        TemplateId::KobCondAskKron => include_str!("../../../contracts/artifacts/KobCondAskKron.json"),
        TemplateId::KobCondBidKron => include_str!("../../../contracts/artifacts/KobCondBidKron.json"),
        TemplateId::KobIfdBidKron => include_str!("../../../contracts/artifacts/KobIfdBidKron.json"),
        TemplateId::KobIfdAskKron => include_str!("../../../contracts/artifacts/KobIfdAskKron.json"),
        TemplateId::KronToken2433 | TemplateId::KronToken2732 => unreachable!("raw KRON programs have no artifact"),
    }
}

/// The raw KRON token programs (redeem script of the reference state: 46-byte state, then the suffix).
fn kron_program_bytes(id: TemplateId) -> &'static [u8] {
    match id {
        TemplateId::KronToken2433 => include_bytes!("../../../contracts/adapters/kron/templates/kron_token_2433.bin"),
        TemplateId::KronToken2732 => include_bytes!("../../../contracts/adapters/kron/templates/kron_token_2732.bin"),
        _ => unreachable!("not a KRON program"),
    }
}

/// Length of the KRON token state span (`0x20 owner | 0x01 id_type | 0x08 amount | 0x01 is_minter`).
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
    let prefix = bytecode[..offset].to_vec();
    let suffix = bytecode[offset + len..].to_vec();
    let computed = silverscript_abi::template_hash(&prefix, &suffix);
    let recorded = contract.compiled.template_hash;
    if computed != recorded {
        return Err(ArtifactError::HashMismatch { name: id.name().into(), recorded: to_hex(&recorded), computed: to_hex(&computed) });
    }
    Ok(Template {
        id,
        contract_name: contract_name.clone(),
        prefix,
        suffix,
        state_len: len,
        hash: computed,
        artifact: artifact.clone(),
    })
}

/// Parse an artifact produced by `silverc` (see `scripts/build-contracts.sh`).
pub fn parse_artifact(json: &str) -> Result<SilAbiArtifact, ArtifactError> {
    Ok(serde_json::from_str(json)?)
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
    let hex = PINNED.iter().find(|(t, _)| *t == id).expect("every template is pinned").1;
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

fn kron_token_template(id: TemplateId) -> Result<TokenTemplate, ArtifactError> {
    let bin = kron_program_bytes(id);
    if bin.len() < KRON_STATE_LEN {
        return Err(ArtifactError::Span { offset: 0, len: KRON_STATE_LEN, size: bin.len() });
    }
    let suffix = bin[KRON_STATE_LEN..].to_vec();
    let hash = silverscript_abi::template_hash(&[], &suffix);
    let pinned = pinned_hash(id);
    if hash != pinned {
        return Err(ArtifactError::NotPinned { name: id.name().into(), got: to_hex(&hash), pinned: to_hex(&pinned) });
    }
    Ok(TokenTemplate {
        id,
        family: Family::Kron,
        prefix: vec![],
        suffix,
        state_len: KRON_STATE_LEN,
        hash,
        slots: id.token_slots().expect("KRON program slots"),
    })
}

fn build_registry() -> Result<Registry, ArtifactError> {
    let mut templates = BTreeMap::new();
    let mut tokens = BTreeMap::new();
    for id in TemplateId::ALL {
        if id.is_artifact() {
            templates.insert(id, load_artifact(id, embedded_json(id))?);
        } else {
            tokens.insert(id, kron_token_template(id)?);
        }
    }
    for (id, t) in &templates {
        if id.is_token() {
            tokens.insert(
                *id,
                TokenTemplate {
                    id: *id,
                    family: Family::Kcc20,
                    prefix: t.prefix.clone(),
                    suffix: t.suffix.clone(),
                    state_len: t.state_len,
                    hash: t.hash,
                    slots: id.token_slots().expect("token program slots"),
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
    pub const REGISTRY_SHA256: &str = "6baf815424fb69c07499ccb7a28d9cc1a9d0d8ece736ac0a84f87f0d93c93a8d";
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
                assert_eq!((t.prefix.len(), t.state_len), (0, KRON_STATE_LEN));
                assert_eq!(t.redeem(&kron_program_bytes(id)[..KRON_STATE_LEN]), kron_program_bytes(id));
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
