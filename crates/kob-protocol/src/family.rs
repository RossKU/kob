//! Token families.
//!
//! KOB escrows tokens of more than one program lineage. A **family** is what changes between them,
//! and nothing else: the token state codec, the owner types the order contracts use, how an input
//! is authorised (signature script columns), the slot limits and the order templates (one adapter
//! per family). The order logic, the builders' transaction layouts and the entry arguments are the
//! same in every family. `contracts/orders/kron/README.md` lists the small differences.
//!
//! # `kcc20`
//!
//! - Token state: 112 bytes, `amount owner owner_scheme borrow_scheme borrow_guard
//!   extension_commitment`.
//! - Order custody: `owner_scheme 0x04` (covenant id), borrow disabled.
//! - Maker and taker held tokens, deliveries: `owner_scheme 0x00` (P2PK, token-level signature).
//! - Authorisation: a leader (`transfer`) and delegators, with the owner witness inside the
//!   sigscript.
//! - Token inputs and outputs per transaction: 3 / 3 for the reference program (what KOB issues),
//!   8 / 8 for a prototype or third-party program (KaspaCom KCC20 0.2.5, a 25.5 KB program whose
//!   sweeps are bounded by block mass), 4 / 5, or 16 / 16.
//! - Token input bound of the orders (`MAX_TOK_IN`): 8.
//! - Output amount: any positive amount.
//! - Extension commitment: yes, a token sub-type.
//!
//! # `kron`
//!
//! - Token state: 46 bytes, `owner id_type amount is_minter`.
//! - Order custody: `id_type 2` (covenant id), `is_minter 0`.
//! - Maker and taker held tokens, deliveries: `id_type 3` (address presence: a P2PK input of the
//!   owner is in the transaction).
//! - Authorisation: one entry per input, shared next-state columns, and one witness byte per token
//!   input that names the authorising input.
//! - Token inputs and outputs per transaction: 4 / 5.
//! - Token input bound of the orders (`MAX_TOK_IN`): 4.
//! - Output amount: `1 ..= 1e9`.
//! - Extension commitment: none.
//!
//! [`Family`] is the registry's family type (`registry/tokens.json` spells it `kcc20` / `kron`).
//! Everything else that depends on the family hangs off the
//! [`TemplateId`](crate::artifacts::TemplateId) of the token program
//! ([`TemplateId::family`](crate::artifacts::TemplateId::family)) or off the order kind
//! ([`AnyState::family`](crate::state::AnyState::family)).

pub use crate::registry::Family;

/// KRON `id_type`: owner is an x-only public key (token-level signature). Not used by the builders.
pub const KRON_TYPE_PUBKEY: u8 = 0;
/// KRON `id_type`: owner is a script hash. Not used by the builders.
pub const KRON_TYPE_SCRIPT_HASH: u8 = 1;
/// KRON `id_type`: owner is a covenant id (KOB order custody).
pub const KRON_TYPE_COVID: u8 = 2;
/// KRON `id_type`: address presence, the P2PK input of the owner is in the transaction.
///
/// Used for wallet balances and maker deliveries.
pub const KRON_TYPE_ADDR: u8 = 3;

/// Most token inputs a transaction that spends KOB KCC-20 orders may carry.
pub const MAX_TOK_IN_KCC20: usize = 8;
/// Most token inputs a transaction that spends KOB KRON orders may carry (KRON's own limit).
pub const MAX_TOK_IN_KRON: usize = 4;

impl Default for Family {
    /// KCC-20, the family of every request that does not name one.
    fn default() -> Self {
        Family::Kcc20
    }
}

impl Family {
    /// True for KCC-20 (serde `skip_serializing_if` of the request `family` fields).
    pub fn is_kcc20(&self) -> bool {
        *self == Family::Kcc20
    }

    /// Family byte of the `KOB1` placement record.
    pub fn code(self) -> u8 {
        match self {
            Family::Kcc20 => 0x01,
            Family::Kron => 0x02,
        }
    }

    /// Inverse of [`Family::code`].
    pub fn from_code(code: u8) -> Option<Family> {
        match code {
            0x01 => Some(Family::Kcc20),
            0x02 => Some(Family::Kron),
            _ => None,
        }
    }

    /// Most token inputs of one token that a transaction spending KOB orders may carry.
    ///
    /// Every order refuses more, and the covenants scan that many inputs for strays.
    pub fn max_tok_in(self) -> usize {
        match self {
            Family::Kcc20 => MAX_TOK_IN_KCC20,
            Family::Kron => MAX_TOK_IN_KRON,
        }
    }

    /// Suffix appended to an order template name in this family (`KobAsk` -> `KobAskKron`).
    pub fn suffix(self) -> &'static str {
        match self {
            Family::Kcc20 => "",
            Family::Kron => "Kron",
        }
    }

    /// Name of the order kind `base` (`"KobAsk"`, ...) in this family, as used in roles and
    /// artifacts.
    pub fn kind_name(self, base: &str) -> String {
        format!("{base}{}", self.suffix())
    }
}
