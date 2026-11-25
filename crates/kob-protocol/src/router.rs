//! The KOB router (`contracts/argent/kob_router.ag`): payment intents any keeper can execute.
//!
//! # Intents
//!
//! An intent is one covenant UTXO of one router actor. The payer creates it once, with one
//! signature in the creation transaction, and can always cancel it (`cancel`, SIGHASH_ALL). Any
//! keeper may execute it against the current book in the fill shape the actor was chosen for,
//! without the payer. From its `deadline` (unix ms) on, anyone may `expire` it. That returns its
//! KAS, less at most [`EXPIRE_MAX_FEE`], and its locked tokens to the payer. See
//! `docs/argent/argent.md`, "The router".
//!
//! - `KasToToken_<shape>`: the payer locks KAS, and the merchant receives exactly `amount` of token
//!   B, bought from 1-3 `KobAsk`s.
//! - `TokenToKas_<shape>`: the payer locks token A, owned by the intent's covenant id. The merchant
//!   receives at least `merchant_kas`, from selling into 1-3 `KobBid`s.
//! - `TokenSwap_<shape>`: the payer locks token A as above. The merchant receives exactly
//!   `amount_b` of token B, with A sold into 1-2 bids and the KAS spent on 1-2 asks.
//! - `TokenToKasKron_<shape>` and `TokenSwapKron_<shape>`: the payer locks a KRON token A. They
//!   work as `TokenToKas` and `TokenSwap`, with A sold into `KobBidKron`s.
//!
//! # Orders and token programs
//!
//! The router links the orders (`KobAsk`, `KobBid`, `KobBidKron`) by closed ICC. The token programs
//! are open ICC handles in the intent's state. [`IntentState`] names the program of every token it
//! moves, and the router reads and pins the tokens under it. One actor therefore serves every
//! KCC-20 program of the 112-byte state and every KRON program, and [`intent_program`] says which
//! programs an intent may name. A KRON token is only paid with, never received, since the merchant
//! receives KAS or a KCC-20 token.
//!
//! This module embeds the router's SilverScript ABI and pins every actor's template hash
//! ([`ROUTER_PINNED`]), so a different router is a different protocol and fails to load. The ABI is
//! the `sil_abi` member of the committed Argent artifact `contracts/argent/router/artifact.json`,
//! extracted into `data/router_sil_abi.json`, and a test keeps the copy equal to the artifact.

use std::collections::BTreeMap;
use std::sync::OnceLock;

use kaspa_consensus_core::tx::ScriptPublicKey;
use kaspa_txscript::pay_to_script_hash_script;
use serde::{Deserialize, Serialize};
use silverscript_abi::{encode_contract_entry_sig_script, encode_runtime_state_script, ArtifactValue, SilAbiArtifact};

use crate::artifacts::{template, TemplateId};
use crate::error::{Error, Result};
use crate::family::Family;
use crate::json::to_hex;
use crate::script::push_data;

/// Id of the committed router artifact (`contracts/argent/router/artifact.json`, field `id`).
pub const ROUTER_ARTIFACT_ID: &str = "01301f9a63ae2b4fce47add08f4d63755879c0eb8477d981adf30976eac85258";

/// Most sompi an `expire` may keep of the intent's KAS.
///
/// This is `EXPIRE_MAX_FEE` of `tools/router-gen/router_head.ag`.
pub const EXPIRE_MAX_FEE: u64 = 10_000_000;

/// Least KAS an intent UTXO may hold, twice [`EXPIRE_MAX_FEE`].
///
/// Its cancel and its expiry take their fee from the intent's own KAS, since they have no funding
/// input. The payer's output they leave must itself stay a standard output (KIP-9 storage mass). An
/// intent below this could be neither cancelled nor expired, and the builder and the verifier
/// refuse it.
pub const MIN_INTENT_VALUE: u64 = 2 * EXPIRE_MAX_FEE;

/// Checks that `p` is a token program an intent may name.
///
/// That is a program whose profile allows router intents (`router_intents` of
/// `data/programs.json`). The router observes tokens by open ICC under the program the intent's
/// state names, so a KCC-20 program needs the 112-byte state. KaspaCom's KCC20 0.2.5, a 25.5 KB
/// program with a 0.5 KAS token output floor, is refused while it is pending review.
pub fn intent_program(p: TemplateId) -> Result<()> {
    let Some(profile) = p.profile() else {
        return Err(Error::Invalid(format!("{} is not a token program", p.name())));
    };
    if !profile.router_intents {
        return Err(Error::Invalid(format!("{} is pending review: no router intent trades it", p.name())));
    }
    if p.family() == Family::Kcc20 && crate::artifacts::token_template(p).state_len != 112 {
        return Err(Error::Invalid(format!("{} has no 112-byte KCC-20 state", p.name())));
    }
    Ok(())
}

/// True when the token slots of `p` (per-transaction inputs and outputs) hold `ins` / `outs`.
fn slots_fit(p: TemplateId, ins: usize, outs: usize) -> bool {
    p.token_slots().is_some_and(|(i, o)| ins <= i && outs <= o)
}

/// Template hash of every router actor (hex).
///
/// A changed router is a changed protocol.
pub const ROUTER_PINNED: [(&str, &str); 42] = [
    ("KasToToken_buy", "76dbb99e083e6fc594e2a436a4a36681340f7703837986f38ca4b50b73d14114"),
    ("KasToToken_buy_out", "fe486d08b506bba6df7b8684a5d35720d215b6fcfd0dc7dddbd81ad39b3f21f8"),
    ("KasToToken_buy2", "98617bf7358221ca68cd54f66dd3fd8a9690f6bca8a0b1eb38987cadecd29cab"),
    ("KasToToken_buy2_out", "0cc9a7e3b6779bb1c4440bab47449142d0be373bf865e2a5be57a68f4f44b8be"),
    ("KasToToken_buy3", "7815df6a07d4569a0e67c9b8db3a3f77c309a5e94ad2b83e6f25130c50347b7a"),
    ("KasToToken_buy3_out", "3190550c858e0b357602bea32c33ccb58a8f001c43b842d7c5ac1b9e68c0e610"),
    ("TokenToKas_sell", "6ae3e9234619e1e2ff8725362fe35d8ff2be024aed7ec583359398b6ab3ce189"),
    ("TokenToKas_sell_out", "94b2de343934538c6243ccd4180082f63748b99f412a4688201b5ad0f0e959d0"),
    ("TokenToKas_sell2", "c95bf35902d29ed6ecbb00e33b02f2273aeed19938a498afe0f1a7e4154034ff"),
    ("TokenToKas_sell2_out", "84b2f8b14e45a83d0a7804f22143d5c923fd9befa64efaf7c6f144e368c02c9e"),
    ("TokenToKas_sell3", "96197f1892026734159a96b42b6b9c695b77df5843e199feb911cf65412fc9d0"),
    ("TokenToKas_sell3_out", "614d6376ec0de4dbb1960983b6954616ee5fcfdbf1439299bf1a6485b6a4220d"),
    ("TokenToKas_sell_c", "878baecab8c4288c2ad9678c6a53d7550d01983f29eaaadf85373bd846a5b24d"),
    ("TokenToKas_sell_c_out", "9d8ba48eea416a1ea666eec09b82b9b675e2cee8753e5109908fd895a1be2d46"),
    ("TokenToKas_sell2_c", "76558d34f98aa04cf790c844c5581430968bd69aa3b3995a5ee98d275799b3f8"),
    ("TokenToKas_sell2_c_out", "ab87cf630014a60a7a9768d9166e96eb9e88a638008d2493441ce6ffad619819"),
    ("TokenToKas_sell2_cc", "1c6bd6095aa8369a922d6bcfb4d7c9fd2d4d2393c2cbe692494ce6748b9ca4c2"),
    ("TokenToKas_sell2_cc_out", "c4e16977e1cd88e2beaed5a8345a890a2fce2d673abfb70d5a319f7817cc1f6b"),
    ("TokenSwap_swap", "c7260618d5ba4706f091d0744cc31d2835812f02804c931173de569470823f6d"),
    ("TokenSwap_swap_bid_out", "088bcee5e01297c01d1aa1096ce690f28afb1bf83b425d402612c207a194f3bd"),
    ("TokenSwap_swap_ask_out", "82faf4508517d362a6203bdf193572f7b1d7d6baa6dc7e03d0b952026d522904"),
    ("TokenSwap_swap_out", "dc2104eeb6fc2787e9de59227f8dd185442143935850b8694f418c3b7f3cafc8"),
    ("TokenSwap_swap2", "08d730c574389c0c8ee14e9ee8955b415d0e5062415d6c6a9f6e461cee202f48"),
    ("TokenSwap_swap2_out", "2e458a19e9ced7af1605a04fa5363d0cdc2dffe64e708432aec66051336d3b1f"),
    ("TokenSwap_swap_c", "96e29b16c175b1e48422bfae0e746988a24d280fc028497f2f565e2c9137152a"),
    ("TokenSwap_swap_bid_out_c", "995a0d42b7b0356f127af662cedd026ace0dbeb607ba745672ce775da2acd91d"),
    ("TokenSwap_swap_ask_out_c", "11146e113725e7864e26e1a54a55826c01b192c44ba01b6f9de382f296e145c9"),
    ("TokenSwap_swap_out_c", "21132df94b9f38caf51669c6d4bfd25d64d761c9dfbf4c027a85395e31de5e37"),
    ("TokenSwap_swap2_c", "11712ec1a1026af93ca6fce4fb21f1330139829c2261e2c2f2a3f2307eec3e1c"),
    ("TokenSwap_swap2_out_c", "aa014b2bbd9f563f4fada833425c845410039b7814322ab60ec2f106d9414b61"),
    ("TokenToKasKron_sell", "6ebb5afcdce240a51e5b9e932191ac00fc07612f26b80c7fc089e1a0ceb20d79"),
    ("TokenToKasKron_sell_out", "1531dc60e6f7a16bc6d32bd5a70fc26bf13e69bdb6f75c4350b9b122f4a6f103"),
    ("TokenToKasKron_sell2", "08d05ab206f4751c0d6dbb0adc965d65c7451c5f2acc9cbb2006ae521a7d4d31"),
    ("TokenToKasKron_sell2_out", "3c3f4de380aae4be0543f775f29a151d66a369b5f78ec9458db098b4ac252e48"),
    ("TokenToKasKron_sell3", "97dbacbe2c74359c1e65bbab015acfedbe233cd0dfca725661d4a3d69872f317"),
    ("TokenToKasKron_sell3_out", "19cc871a91fbb428b93936ff98c59e8479c2a9248607b9c6ee11397117688676"),
    ("TokenSwapKron_swap", "4d78246d2dc60efa5fcbd40ec304231e62c87b893b04c3424557ea52971743f1"),
    ("TokenSwapKron_swap_bid_out", "0ff058d8b7a3949e74d0cbe1cb6ce2a5d4df9c645deb05ef56aa952bd5ac2583"),
    ("TokenSwapKron_swap_ask_out", "556c8661bfeb3535f6b3d0d244041a7154e6ef32d6ca3a1565a97713a809ae9b"),
    ("TokenSwapKron_swap_out", "e17c3e0e101a25f83f2a8dd7ce49799b61fcf9be7189326b40cec1d1877a0329"),
    ("TokenSwapKron_swap2", "97e12c5dcdb779b30cb96993a2882133ceb59551e491f95a847a443d25b2d1a1"),
    ("TokenSwapKron_swap2_out", "ae1cdd9cb10a74e23a62ecc173d5876f69c67d6eb6b88aad46823ffb4c033f39"),
];

const SIL_ABI_JSON: &str = include_str!("../data/router_sil_abi.json");

/// The three payment intents.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum IntentKind {
    /// The payer locks KAS; the merchant receives exactly `amount` of token B.
    KasToToken,
    /// The payer locks token A; the merchant receives at least `merchant_kas` KAS.
    TokenToKas,
    /// The payer locks token A; the merchant receives exactly `amount_b` of token B.
    TokenSwap,
}

impl IntentKind {
    /// The kind's name.
    pub fn as_str(self) -> &'static str {
        match self {
            IntentKind::KasToToken => "KasToToken",
            IntentKind::TokenToKas => "TokenToKas",
            IntentKind::TokenSwap => "TokenSwap",
        }
    }
    /// True when the payer locks token A in the intent (owner = the intent's covenant id).
    pub fn locks_tokens(self) -> bool {
        !matches!(self, IntentKind::KasToToken)
    }
    /// True when the merchant receives a token (KasToToken, TokenSwap), false for KAS (TokenToKas).
    pub fn merchant_gets_token(self) -> bool {
        !matches!(self, IntentKind::TokenToKas)
    }
    /// The output the router pins for the merchant in an execution that spends the intent at
    /// input `j`.
    ///
    /// The intent owns outputs `j` and `j + 1` (the positional anti-aliasing rule). The merchant's
    /// token B is at `j + 1` for KasToToken. The merchant's KAS (TokenToKas) and token B
    /// (TokenSwap) are at `j`.
    pub fn merchant_output(self, j: usize) -> usize {
        match self {
            IntentKind::KasToToken => j + 1,
            IntentKind::TokenToKas | IntentKind::TokenSwap => j,
        }
    }
}

/// The fill shape of one router actor.
///
/// It says how many asks and bids the keeper fills, and whether the last order of each leg rests
/// (continues, GTC) or ends (sold out, exhausted, IOC or FOK). Every order of a leg but the last
/// ends.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Shape {
    pub kind: IntentKind,
    pub asks: usize,
    pub bids: usize,
    pub last_ask_rests: bool,
    pub last_bid_rests: bool,
    /// Family of token A, which a token intent locks and sells (to `KobBidKron`s for KRON).
    ///
    /// It is KCC-20 for KasToToken.
    #[serde(default)]
    pub a_family: Family,
}

impl Shape {
    /// Token slots the execution of this shape needs: (inputs, outputs) of token A, then token B.
    pub fn token_slots(&self) -> ((usize, usize), (usize, usize)) {
        let a = if self.kind.locks_tokens() { (1, self.bids + 1) } else { (0, 0) };
        let b = if self.kind.merchant_gets_token() { (self.asks, 1 + usize::from(self.last_ask_rests)) } else { (0, 0) };
        (a, b)
    }
    /// True when the programs of token A (`a`) and token B (`b`) can run this shape.
    ///
    /// The families must match, i.e. token A of `a_family` and token B KCC-20. The programs' slots
    /// must hold its token inputs and outputs. The 3/3 reference program, for example, has no room
    /// for a three-bid sell, which has four token outputs.
    pub fn fits(&self, a: Option<TemplateId>, b: Option<TemplateId>) -> bool {
        let ((ai, ao), (bi, bo)) = self.token_slots();
        let a_ok = match (self.kind.locks_tokens(), a) {
            (true, Some(p)) => p.family() == self.a_family && intent_program(p).is_ok() && slots_fit(p, ai, ao),
            (false, None) => true,
            _ => false,
        };
        let b_ok = match (self.kind.merchant_gets_token(), b) {
            (true, Some(p)) => p.family() == Family::Kcc20 && intent_program(p).is_ok() && slots_fit(p, bi, bo),
            (false, None) => true,
            _ => false,
        };
        a_ok && b_ok
    }
}

/// One router actor (= one template): its name, its fill entry, its shape and its custody legs.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Actor {
    pub name: &'static str,
    /// The fill entry (the actor's other entries are `expire` and `cancel`).
    pub entry: &'static str,
    pub shape: Shape,
    /// How many leading bid legs sell into a bid that already holds a bought-custody (0 to 2).
    ///
    /// A mode-1 bid with `bought > 0` keeps what it bought in a token UTXO owned by its own
    /// covenant id (`docs/design/bid-delivery-spec.md`, section 1). The fill spends it and
    /// delivers `bought + n`, so the custody is a token input of token A. The router observes
    /// every token input, so each count of custody legs is its own actor (`_c`, `_cc`). The
    /// custody legs are the first bids, and their custodies the token A inputs after the lock.
    pub custody: usize,
}

const fn sh(kind: IntentKind, asks: usize, bids: usize, last_ask_rests: bool, last_bid_rests: bool) -> Shape {
    Shape { kind, asks, bids, last_ask_rests, last_bid_rests, a_family: Family::Kcc20 }
}
const fn shk(kind: IntentKind, asks: usize, bids: usize, last_ask_rests: bool, last_bid_rests: bool) -> Shape {
    Shape { kind, asks, bids, last_ask_rests, last_bid_rests, a_family: Family::Kron }
}

use IntentKind::{KasToToken as K2T, TokenSwap as SW, TokenToKas as T2K};

/// Every actor of the router.
///
/// The KCC-20 `TokenToKas` and `TokenSwap` shapes have custody twins: `sell_c`, `sell2_c`,
/// `sell2_cc` and their `_out` twins, and one `_c` twin of each swap shape. There are no
/// `sell3` custody twins, and the KRON routers have none (a KRON delivery is mode 0).
pub const ACTORS: [Actor; 42] = [
    Actor { name: "KasToToken_buy", entry: "buy", shape: sh(K2T, 1, 0, true, false), custody: 0 },
    Actor { name: "KasToToken_buy_out", entry: "buy_out", shape: sh(K2T, 1, 0, false, false), custody: 0 },
    Actor { name: "KasToToken_buy2", entry: "buy2", shape: sh(K2T, 2, 0, true, false), custody: 0 },
    Actor { name: "KasToToken_buy2_out", entry: "buy2_out", shape: sh(K2T, 2, 0, false, false), custody: 0 },
    Actor { name: "KasToToken_buy3", entry: "buy3", shape: sh(K2T, 3, 0, true, false), custody: 0 },
    Actor { name: "KasToToken_buy3_out", entry: "buy3_out", shape: sh(K2T, 3, 0, false, false), custody: 0 },
    Actor { name: "TokenToKas_sell", entry: "sell", shape: sh(T2K, 0, 1, false, true), custody: 0 },
    Actor { name: "TokenToKas_sell_out", entry: "sell_out", shape: sh(T2K, 0, 1, false, false), custody: 0 },
    Actor { name: "TokenToKas_sell2", entry: "sell2", shape: sh(T2K, 0, 2, false, true), custody: 0 },
    Actor { name: "TokenToKas_sell2_out", entry: "sell2_out", shape: sh(T2K, 0, 2, false, false), custody: 0 },
    Actor { name: "TokenToKas_sell3", entry: "sell3", shape: sh(T2K, 0, 3, false, true), custody: 0 },
    Actor { name: "TokenToKas_sell3_out", entry: "sell3_out", shape: sh(T2K, 0, 3, false, false), custody: 0 },
    Actor { name: "TokenToKas_sell_c", entry: "sell_c", shape: sh(T2K, 0, 1, false, true), custody: 1 },
    Actor { name: "TokenToKas_sell_c_out", entry: "sell_c_out", shape: sh(T2K, 0, 1, false, false), custody: 1 },
    Actor { name: "TokenToKas_sell2_c", entry: "sell2_c", shape: sh(T2K, 0, 2, false, true), custody: 1 },
    Actor { name: "TokenToKas_sell2_c_out", entry: "sell2_c_out", shape: sh(T2K, 0, 2, false, false), custody: 1 },
    Actor { name: "TokenToKas_sell2_cc", entry: "sell2_cc", shape: sh(T2K, 0, 2, false, true), custody: 2 },
    Actor { name: "TokenToKas_sell2_cc_out", entry: "sell2_cc_out", shape: sh(T2K, 0, 2, false, false), custody: 2 },
    Actor { name: "TokenSwap_swap", entry: "swap", shape: sh(SW, 1, 1, true, true), custody: 0 },
    Actor { name: "TokenSwap_swap_bid_out", entry: "swap_bid_out", shape: sh(SW, 1, 1, true, false), custody: 0 },
    Actor { name: "TokenSwap_swap_ask_out", entry: "swap_ask_out", shape: sh(SW, 1, 1, false, true), custody: 0 },
    Actor { name: "TokenSwap_swap_out", entry: "swap_out", shape: sh(SW, 1, 1, false, false), custody: 0 },
    Actor { name: "TokenSwap_swap2", entry: "swap2", shape: sh(SW, 2, 2, true, true), custody: 0 },
    Actor { name: "TokenSwap_swap2_out", entry: "swap2_out", shape: sh(SW, 2, 2, false, false), custody: 0 },
    Actor { name: "TokenSwap_swap_c", entry: "swap_c", shape: sh(SW, 1, 1, true, true), custody: 1 },
    Actor { name: "TokenSwap_swap_bid_out_c", entry: "swap_bid_out_c", shape: sh(SW, 1, 1, true, false), custody: 1 },
    Actor { name: "TokenSwap_swap_ask_out_c", entry: "swap_ask_out_c", shape: sh(SW, 1, 1, false, true), custody: 1 },
    Actor { name: "TokenSwap_swap_out_c", entry: "swap_out_c", shape: sh(SW, 1, 1, false, false), custody: 1 },
    Actor { name: "TokenSwap_swap2_c", entry: "swap2_c", shape: sh(SW, 2, 2, true, true), custody: 1 },
    Actor { name: "TokenSwap_swap2_out_c", entry: "swap2_out_c", shape: sh(SW, 2, 2, false, false), custody: 1 },
    Actor { name: "TokenToKasKron_sell", entry: "sell", shape: shk(T2K, 0, 1, false, true), custody: 0 },
    Actor { name: "TokenToKasKron_sell_out", entry: "sell_out", shape: shk(T2K, 0, 1, false, false), custody: 0 },
    Actor { name: "TokenToKasKron_sell2", entry: "sell2", shape: shk(T2K, 0, 2, false, true), custody: 0 },
    Actor { name: "TokenToKasKron_sell2_out", entry: "sell2_out", shape: shk(T2K, 0, 2, false, false), custody: 0 },
    Actor { name: "TokenToKasKron_sell3", entry: "sell3", shape: shk(T2K, 0, 3, false, true), custody: 0 },
    Actor { name: "TokenToKasKron_sell3_out", entry: "sell3_out", shape: shk(T2K, 0, 3, false, false), custody: 0 },
    Actor { name: "TokenSwapKron_swap", entry: "swap", shape: shk(SW, 1, 1, true, true), custody: 0 },
    Actor { name: "TokenSwapKron_swap_bid_out", entry: "swap_bid_out", shape: shk(SW, 1, 1, true, false), custody: 0 },
    Actor { name: "TokenSwapKron_swap_ask_out", entry: "swap_ask_out", shape: shk(SW, 1, 1, false, true), custody: 0 },
    Actor { name: "TokenSwapKron_swap_out", entry: "swap_out", shape: shk(SW, 1, 1, false, false), custody: 0 },
    Actor { name: "TokenSwapKron_swap2", entry: "swap2", shape: shk(SW, 2, 2, true, true), custody: 0 },
    Actor { name: "TokenSwapKron_swap2_out", entry: "swap2_out", shape: shk(SW, 2, 2, false, false), custody: 0 },
];

impl Actor {
    /// The actor of this name.
    pub fn by_name(name: &str) -> Option<&'static Actor> {
        ACTORS.iter().find(|a| a.name == name)
    }
    /// The actor of a shape whose bids hold no bought-custody.
    ///
    /// It serves bids of delivery modes 0 and 2 and the first fill of a mode-1 bid.
    pub fn by_shape(shape: &Shape) -> Option<&'static Actor> {
        Self::by_shape_custody(shape, 0)
    }
    /// The actor of a shape whose first `custody` bids hold a bought-custody.
    pub fn by_shape_custody(shape: &Shape, custody: usize) -> Option<&'static Actor> {
        ACTORS.iter().find(|a| a.shape == *shape && a.custody == custody)
    }
    /// Token slots the execution of this actor needs: (inputs, outputs) of token A, then token B.
    ///
    /// They are the shape's, plus one token A input per custody leg.
    pub fn token_slots(&self) -> ((usize, usize), (usize, usize)) {
        let ((ai, ao), b) = self.shape.token_slots();
        ((ai + self.custody, ao), b)
    }
    /// True when the programs of token A (`a`) and token B (`b`) can run this actor.
    ///
    /// See [`Shape::fits`]; the custody legs add token A inputs.
    pub fn fits(&self, a: Option<TemplateId>, b: Option<TemplateId>) -> bool {
        let ((ai, ao), _) = self.token_slots();
        self.shape.fits(a, b) && a.is_none_or(|p| slots_fit(p, ai, ao))
    }
    /// The loaded, pinned template.
    pub fn template(&self) -> &'static RouterTemplate {
        &loaded().templates[self.name]
    }
    /// The dispatch tag of the actor's entry `entry`: `self.entry`, `expire` or `cancel`.
    ///
    /// A signature script of the actor pushes it right before the redeem script.
    pub fn dispatch_tag(&self, entry: &str) -> Option<[u8; 4]> {
        loaded().abi.contracts.get(self.name)?.entries.get(entry).map(|e| e.dispatch_tag.into_bytes())
    }
}

/// One router actor's compiled template.
#[derive(Clone, Debug)]
pub struct RouterTemplate {
    pub name: &'static str,
    pub prefix: Vec<u8>,
    pub suffix: Vec<u8>,
    pub state_len: usize,
    pub hash: [u8; 32],
}

impl RouterTemplate {
    /// Redeem script of an instance: `prefix ‖ state ‖ suffix`.
    pub fn redeem(&self, state: &[u8]) -> Result<Vec<u8>> {
        if state.len() != self.state_len {
            return Err(Error::Invalid(format!("{}: state span must be {} bytes, got {}", self.name, self.state_len, state.len())));
        }
        Ok([self.prefix.as_slice(), state, self.suffix.as_slice()].concat())
    }
    /// P2SH script public key of an instance.
    pub fn spk(&self, state: &[u8]) -> Result<ScriptPublicKey> {
        let spk = pay_to_script_hash_script(&self.redeem(state)?);
        #[cfg(feature = "adversarial")]
        crate::artifacts::spk_trace::record(&spk, crate::artifacts::spk_trace::Origin::Router(self.name), state);
        Ok(spk)
    }
    /// Hex of the template hash.
    pub fn hash_hex(&self) -> String {
        to_hex(&self.hash)
    }
}

struct Loaded {
    abi: SilAbiArtifact,
    templates: BTreeMap<&'static str, RouterTemplate>,
}

fn load() -> std::result::Result<Loaded, String> {
    let abi: SilAbiArtifact = serde_json::from_str(SIL_ABI_JSON).map_err(|e| format!("router abi json: {e}"))?;
    let mut templates = BTreeMap::new();
    for a in &ACTORS {
        let c = abi.contracts.get(a.name).ok_or_else(|| format!("router abi has no actor {}", a.name))?;
        if !c.entries.contains_key(a.entry)
            || !c.entries.contains_key("expire")
            || !c.entries.contains_key("cancel")
            || c.entries.len() != 3
        {
            return Err(format!("router actor {} must have exactly the entries {}, expire and cancel", a.name, a.entry));
        }
        let span = c.compiled.state_span;
        let code = &c.compiled.bytecode;
        if span.offset + span.len > code.len() {
            return Err(format!("{}: state span outside the bytecode", a.name));
        }
        let prefix = code[..span.offset].to_vec();
        let suffix = code[span.offset + span.len..].to_vec();
        let hash = silverscript_abi::template_hash(&prefix, &suffix);
        if hash != c.compiled.template_hash {
            return Err(format!("{}: recorded template hash differs from the bytecode", a.name));
        }
        let pinned = ROUTER_PINNED.iter().find(|(n, _)| *n == a.name).map(|(_, h)| *h).unwrap_or("");
        if to_hex(&hash) != pinned {
            return Err(format!("{}: template hash {} is not the pinned {pinned}", a.name, to_hex(&hash)));
        }
        templates.insert(a.name, RouterTemplate { name: a.name, prefix, suffix, state_len: span.len, hash });
    }
    Ok(Loaded { abi, templates })
}

fn loaded() -> &'static Loaded {
    static L: OnceLock<Loaded> = OnceLock::new();
    L.get_or_init(|| load().unwrap_or_else(|e| panic!("embedded router: {e}")))
}

/// Loads and checks the embedded router (template hashes against [`ROUTER_PINNED`]).
pub fn check_router() -> Result<()> {
    load().map(|_| ()).map_err(Error::Invalid)
}

/// The actor whose template hash is `hash`.
pub fn actor_by_hash(hash: &[u8; 32]) -> Option<&'static Actor> {
    ACTORS.iter().find(|a| &a.template().hash == hash)
}

/// The actor (and the decoded state span) of a redeem script, if it is a router instance.
pub fn instance_of(redeem: &[u8]) -> Option<(&'static Actor, Vec<u8>)> {
    ACTORS.iter().find_map(|a| {
        let t = a.template();
        (redeem.len() == t.prefix.len() + t.state_len + t.suffix.len() && redeem.starts_with(&t.prefix) && redeem.ends_with(&t.suffix))
            .then(|| (a, redeem[t.prefix.len()..t.prefix.len() + t.state_len].to_vec()))
    })
}

// ------------------------------------------------------------------------------------------- state

fn default_program() -> TemplateId {
    TemplateId::Kcc20Ref8x8
}

/// The terms of an intent, i.e. the actor's state.
///
/// Keys are x-only public keys, and token ids are covenant ids. Every token is named twice: by its
/// covenant id and by its program, whose template hash the router's open ICC handle carries (the
/// `program*` fields). A JSON without those fields means `KCC20Ref_8x8`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum IntentState {
    #[serde(rename_all = "camelCase")]
    KasToToken {
        /// Receives the KAS change; signs `cancel`.
        #[serde(with = "crate::json::field")]
        payer: [u8; 32],
        /// P2PK owner of the delivered token.
        #[serde(with = "crate::json::field")]
        merchant: [u8; 32],
        /// Token B.
        #[serde(with = "crate::json::field")]
        token: [u8; 32],
        /// Token B's program (KCC-20).
        #[serde(default = "default_program")]
        program: TemplateId,
        /// Exact token base units the merchant receives.
        #[serde(with = "crate::json::field")]
        amount: i64,
        /// Most sompi the asks may demand at their quotes (summed over the sweep).
        #[serde(with = "crate::json::field")]
        max_pay: i64,
        /// Most sompi carriers, fillers and the network fee may take besides.
        #[serde(with = "crate::json::field")]
        max_extra: i64,
        /// B PIN: extension_commitment of token B, the offer's token.
        ///
        /// Every ask escrow the intent buys from must carry it, and the merchant's delivery is
        /// pinned to it.
        #[serde(with = "crate::json::field")]
        b_extension: [u8; 32],
        /// Unix ms from which anyone may expire the intent (`expire`, CLTV `tx.time`).
        ///
        /// It is the payer's authorization expiry.
        #[serde(with = "crate::json::field")]
        deadline: i64,
    },
    #[serde(rename_all = "camelCase")]
    TokenToKas {
        #[serde(with = "crate::json::field")]
        payer: [u8; 32],
        /// Receives the KAS (P2PK).
        #[serde(with = "crate::json::field")]
        merchant: [u8; 32],
        /// Token A.
        #[serde(with = "crate::json::field")]
        token: [u8; 32],
        /// Token A's program (KCC-20 or KRON: the actor's family).
        #[serde(default = "default_program")]
        program: TemplateId,
        /// Least sompi the merchant receives.
        #[serde(with = "crate::json::field")]
        merchant_kas: i64,
        /// Most token A base units sold (summed over the sweep).
        #[serde(with = "crate::json::field")]
        max_sell: i64,
        /// LOCK PIN: exact token A base units of the lock, the creation's `lock_amount`.
        ///
        /// Every entry that spends the lock requires it, so no other token UTXO owned by the
        /// intent's id can stand in for the lock.
        #[serde(with = "crate::json::field")]
        lock_amount: i64,
        /// LOCK PIN: extension_commitment of the lock.
        ///
        /// It is that of a KCC-20 token A, and all zero for a KRON token A, which has none.
        #[serde(with = "crate::json::field")]
        lock_extension: [u8; 32],
        /// Unix ms from which anyone may expire the intent (`expire`, CLTV `tx.time`).
        ///
        /// It is the payer's authorization expiry.
        #[serde(with = "crate::json::field")]
        deadline: i64,
    },
    #[serde(rename_all = "camelCase")]
    TokenSwap {
        #[serde(with = "crate::json::field")]
        payer: [u8; 32],
        #[serde(with = "crate::json::field")]
        merchant: [u8; 32],
        #[serde(with = "crate::json::field")]
        token_a: [u8; 32],
        /// Token A's program (KCC-20 or KRON: the actor's family).
        #[serde(default = "default_program")]
        program_a: TemplateId,
        #[serde(with = "crate::json::field")]
        token_b: [u8; 32],
        /// Token B's program (KCC-20).
        #[serde(default = "default_program")]
        program_b: TemplateId,
        /// Most token A base units sold (summed over the sweep).
        #[serde(with = "crate::json::field")]
        max_sell_a: i64,
        /// Exact token B base units the merchant receives.
        #[serde(with = "crate::json::field")]
        amount_b: i64,
        /// LOCK PIN: exact token A base units of the lock, the creation's `lock_amount`.
        ///
        /// Every entry that spends the lock requires it, so no other token UTXO owned by the
        /// intent's id can stand in for the lock.
        #[serde(with = "crate::json::field")]
        lock_amount: i64,
        /// LOCK PIN: extension_commitment of the lock.
        ///
        /// It is that of a KCC-20 token A, and all zero for a KRON token A, which has none.
        #[serde(with = "crate::json::field")]
        lock_extension: [u8; 32],
        /// B PIN: extension_commitment of token B, the offer's token.
        ///
        /// Every ask escrow the intent buys from must carry it, and the merchant's delivery is
        /// pinned to it.
        #[serde(with = "crate::json::field")]
        b_extension: [u8; 32],
        /// Unix ms from which anyone may expire the intent (`expire`, CLTV `tx.time`).
        ///
        /// It is the payer's authorization expiry.
        #[serde(with = "crate::json::field")]
        deadline: i64,
    },
}

fn b32(v: &[u8; 32]) -> ArtifactValue {
    ArtifactValue::Bytes(v.to_vec())
}

/// The open ICC handle of a token program: its template hash.
fn handle(p: TemplateId) -> ArtifactValue {
    b32(&crate::artifacts::token_template(p).hash)
}

impl IntentState {
    /// The intent's kind.
    pub fn kind(&self) -> IntentKind {
        match self {
            IntentState::KasToToken { .. } => IntentKind::KasToToken,
            IntentState::TokenToKas { .. } => IntentKind::TokenToKas,
            IntentState::TokenSwap { .. } => IntentKind::TokenSwap,
        }
    }
    /// The payer's key.
    pub fn payer(&self) -> [u8; 32] {
        match self {
            IntentState::KasToToken { payer, .. } | IntentState::TokenToKas { payer, .. } | IntentState::TokenSwap { payer, .. } => {
                *payer
            }
        }
    }
    /// The merchant's key.
    pub fn merchant(&self) -> [u8; 32] {
        match self {
            IntentState::KasToToken { merchant, .. }
            | IntentState::TokenToKas { merchant, .. }
            | IntentState::TokenSwap { merchant, .. } => *merchant,
        }
    }
    /// Unix ms from which anyone may expire the intent.
    pub fn deadline(&self) -> i64 {
        match self {
            IntentState::KasToToken { deadline, .. }
            | IntentState::TokenToKas { deadline, .. }
            | IntentState::TokenSwap { deadline, .. } => *deadline,
        }
    }
    /// The token the payer locks (TokenToKas, TokenSwap).
    pub fn locked_token(&self) -> Option<[u8; 32]> {
        match self {
            IntentState::KasToToken { .. } => None,
            IntentState::TokenToKas { token, .. } => Some(*token),
            IntentState::TokenSwap { token_a, .. } => Some(*token_a),
        }
    }
    /// The program of the token the payer locks (TokenToKas, TokenSwap).
    pub fn locked_program(&self) -> Option<TemplateId> {
        match self {
            IntentState::KasToToken { .. } => None,
            IntentState::TokenToKas { program, .. } => Some(*program),
            IntentState::TokenSwap { program_a, .. } => Some(*program_a),
        }
    }
    /// The token the merchant receives (KasToToken, TokenSwap).
    pub fn merchant_token(&self) -> Option<[u8; 32]> {
        match self {
            IntentState::KasToToken { token, .. } => Some(*token),
            IntentState::TokenToKas { .. } => None,
            IntentState::TokenSwap { token_b, .. } => Some(*token_b),
        }
    }
    /// The program of the token the merchant receives (KasToToken, TokenSwap).
    pub fn merchant_program(&self) -> Option<TemplateId> {
        match self {
            IntentState::KasToToken { program, .. } => Some(*program),
            IntentState::TokenToKas { .. } => None,
            IntentState::TokenSwap { program_b, .. } => Some(*program_b),
        }
    }
    /// The family of the locked token A.
    ///
    /// It is KCC-20 for KasToToken, the family of every actor without a token A.
    pub fn a_family(&self) -> Family {
        self.locked_program().map(|p| p.family()).unwrap_or(Family::Kcc20)
    }
    /// The lock pin (TokenToKas, TokenSwap): exact base units and extension commitment of the lock.
    pub fn lock_pin(&self) -> Option<(i64, [u8; 32])> {
        match self {
            IntentState::KasToToken { .. } => None,
            IntentState::TokenToKas { lock_amount, lock_extension, .. }
            | IntentState::TokenSwap { lock_amount, lock_extension, .. } => Some((*lock_amount, *lock_extension)),
        }
    }
    /// The B pin (KasToToken, TokenSwap): the extension commitment of the merchant's token.
    pub fn b_extension(&self) -> Option<[u8; 32]> {
        match self {
            IntentState::KasToToken { b_extension, .. } | IntentState::TokenSwap { b_extension, .. } => Some(*b_extension),
            IntentState::TokenToKas { .. } => None,
        }
    }
    /// Most token units the intent may sell (TokenToKas, TokenSwap).
    pub fn max_sell(&self) -> Option<i64> {
        match self {
            IntentState::KasToToken { .. } => None,
            IntentState::TokenToKas { max_sell, .. } => Some(*max_sell),
            IntentState::TokenSwap { max_sell_a, .. } => Some(*max_sell_a),
        }
    }

    fn values(&self) -> BTreeMap<String, ArtifactValue> {
        let mut m = BTreeMap::new();
        let mut put = |k: &str, v: ArtifactValue| {
            m.insert(k.to_string(), v);
        };
        match self {
            IntentState::KasToToken { payer, merchant, token, program, amount, max_pay, max_extra, b_extension, deadline } => {
                put("payer", b32(payer));
                put("merchant", b32(merchant));
                put("token_covid", b32(token));
                put("token_type", handle(*program));
                put("amount", ArtifactValue::Int(*amount));
                put("max_pay", ArtifactValue::Int(*max_pay));
                put("max_extra", ArtifactValue::Int(*max_extra));
                put("b_extension", b32(b_extension));
                put("deadline", ArtifactValue::Int(*deadline));
            }
            IntentState::TokenToKas {
                payer,
                merchant,
                token,
                program,
                merchant_kas,
                max_sell,
                lock_amount,
                lock_extension,
                deadline,
            } => {
                put("payer", b32(payer));
                put("merchant", b32(merchant));
                put("token_covid", b32(token));
                put("token_type", handle(*program));
                put("merchant_kas", ArtifactValue::Int(*merchant_kas));
                put("max_sell", ArtifactValue::Int(*max_sell));
                put("lock_amount", ArtifactValue::Int(*lock_amount));
                if program.family() == Family::Kcc20 {
                    put("lock_extension", b32(lock_extension));
                }
                put("deadline", ArtifactValue::Int(*deadline));
            }
            IntentState::TokenSwap {
                payer,
                merchant,
                token_a,
                program_a,
                token_b,
                program_b,
                max_sell_a,
                amount_b,
                lock_amount,
                lock_extension,
                b_extension,
                deadline,
            } => {
                put("payer", b32(payer));
                put("merchant", b32(merchant));
                put("token_a", b32(token_a));
                put("token_a_type", handle(*program_a));
                put("token_b", b32(token_b));
                put("token_b_type", handle(*program_b));
                put("max_sell_a", ArtifactValue::Int(*max_sell_a));
                put("amount_b", ArtifactValue::Int(*amount_b));
                put("lock_amount", ArtifactValue::Int(*lock_amount));
                if program_a.family() == Family::Kcc20 {
                    put("lock_extension", b32(lock_extension));
                }
                put("b_extension", b32(b_extension));
                put("deadline", ArtifactValue::Int(*deadline));
            }
        }
        m
    }

    /// Checks the terms.
    ///
    /// - The keys are on the curve.
    /// - The amounts and bounds are positive, and the tokens distinct.
    /// - The programs are ones an intent may name ([`intent_program`]), and the merchant's token is
    ///   KCC-20.
    /// - The deadline is a time lock: unix ms, at least [`kaspa_txscript::LOCK_TIME_THRESHOLD`]. A
    ///   smaller value could never be met by `expire`.
    pub fn check(&self) -> Result<()> {
        crate::tx::check_key(&self.payer(), "intent payer")?;
        crate::tx::check_key(&self.merchant(), "intent merchant")?;
        if self.deadline() < kaspa_txscript::LOCK_TIME_THRESHOLD as i64 {
            return Err(Error::Invalid(format!(
                "intent deadline {} is not a unix-ms time lock (at least {})",
                self.deadline(),
                kaspa_txscript::LOCK_TIME_THRESHOLD
            )));
        }
        if let Some(p) = self.locked_program() {
            intent_program(p)?;
        }
        if let Some(p) = self.merchant_program() {
            intent_program(p)?;
            if p.family() != Family::Kcc20 {
                return Err(Error::Invalid(format!("the merchant receives KCC-20 tokens only ({} is a KRON program)", p.name())));
            }
        }
        let pos = |v: i64, what: &str| if v > 0 { Ok(()) } else { Err(Error::Invalid(format!("intent {what} must be positive"))) };
        match self {
            IntentState::KasToToken { amount, max_pay, max_extra, .. } => {
                pos(*amount, "amount")?;
                pos(*max_pay, "max_pay")?;
                if *max_extra < 0 {
                    return Err(Error::Invalid("intent max_extra must not be negative".into()));
                }
            }
            IntentState::TokenToKas { merchant_kas, max_sell, .. } => {
                pos(*merchant_kas, "merchant_kas")?;
                pos(*max_sell, "max_sell")?;
            }
            IntentState::TokenSwap { token_a, token_b, max_sell_a, amount_b, .. } => {
                pos(*max_sell_a, "max_sell_a")?;
                pos(*amount_b, "amount_b")?;
                if token_a == token_b {
                    return Err(Error::Invalid("a swap intent sells and buys two different tokens".into()));
                }
            }
        }
        if let (Some((lock, ext)), Some(max_sell)) = (self.lock_pin(), self.max_sell()) {
            if lock < max_sell {
                return Err(Error::Invalid(format!("the intent may sell {max_sell} units but its lock_amount is {lock}")));
            }
            // A KRON output holds 1..=1e9 units, so the payer's change of a sale of max_sell must
            // hold a unit.
            if self.a_family() == Family::Kron {
                if lock <= max_sell {
                    return Err(Error::Invalid(format!(
                        "a KRON intent locks more than it may sell (its token change after a sale of max_sell {max_sell} must hold a unit)"
                    )));
                }
                if ext != [0u8; 32] {
                    return Err(Error::Invalid("a KRON lock has no extension commitment (lock_extension must be zero)".into()));
                }
            }
        }
        Ok(())
    }

    /// Checks that `actor` can carry these terms: its kind, the family of token A, and the
    /// programs' token slots.
    ///
    /// A shape a program cannot run is refused here, not after the payer locked its funds.
    pub fn check_actor(&self, actor: &Actor) -> Result<()> {
        if actor.shape.kind != self.kind() {
            return Err(Error::Invalid(format!("{} is not a {} actor", actor.name, self.kind().as_str())));
        }
        if actor.shape.a_family != self.a_family() {
            return Err(Error::Invalid(format!("{} does not trade a {:?} token A", actor.name, self.a_family())));
        }
        if !actor.fits(self.locked_program(), self.merchant_program()) {
            return Err(Error::Invalid(format!("{}: the token programs have no room for its token inputs and outputs", actor.name)));
        }
        Ok(())
    }

    /// The encoded state span under `actor`.
    ///
    /// The actor must carry these terms, see [`IntentState::check_actor`].
    pub fn encode(&self, actor: &Actor) -> Result<Vec<u8>> {
        self.check_actor(actor)?;
        let l = loaded();
        let c = &l.abi.contracts[actor.name];
        let s = encode_runtime_state_script(&l.abi, &c.runtime_state, &self.values())
            .map_err(|e| Error::Invalid(format!("{}: state encoding: {e}", actor.name)))?;
        if s.len() != actor.template().state_len {
            return Err(Error::Invalid(format!("{}: encoded state is {} bytes", actor.name, s.len())));
        }
        Ok(s)
    }

    /// Redeem script of the intent under `actor`.
    pub fn redeem(&self, actor: &Actor) -> Result<Vec<u8>> {
        actor.template().redeem(&self.encode(actor)?)
    }

    /// P2SH script public key of the intent under `actor`.
    pub fn spk(&self, actor: &Actor) -> Result<ScriptPublicKey> {
        actor.template().spk(&self.encode(actor)?)
    }

    /// Decodes the state span of `actor`.
    pub fn decode(actor: &Actor, state: &[u8]) -> Result<IntentState> {
        let l = loaded();
        let c = &l.abi.contracts[actor.name];
        let v = silverscript_abi::decode_runtime_state_script(&l.abi, &c.runtime_state, state)
            .map_err(|e| Error::Invalid(format!("{}: state decoding: {e}", actor.name)))?;
        // KCC-1 3.7.1: the consumed bytes must be exactly the canonical encoding of the decoded
        // values, with no negative zero and no over-long push prefix. silverscript-abi's decoder
        // alone accepts both.
        let again = encode_runtime_state_script(&l.abi, &c.runtime_state, &v)
            .map_err(|e| Error::Invalid(format!("{}: state re-encoding: {e}", actor.name)))?;
        if again != state {
            return Err(Error::Invalid(format!("{}: non-canonical state encoding", actor.name)));
        }
        let key = |k: &str| -> Result<[u8; 32]> {
            match v.get(k) {
                Some(ArtifactValue::Bytes(b)) if b.len() == 32 => Ok(b.as_slice().try_into().expect("32")),
                _ => Err(Error::Invalid(format!("{}: state field {k} is not 32 bytes", actor.name))),
            }
        };
        let int = |k: &str| -> Result<i64> {
            match v.get(k) {
                Some(ArtifactValue::Int(i)) => Ok(*i),
                _ => Err(Error::Invalid(format!("{}: state field {k} is not an int", actor.name))),
            }
        };
        // A handle names a known token program, or the instance is not one KOB can build or check.
        let prog = |k: &str| -> Result<TemplateId> {
            let h = key(k)?;
            crate::artifacts::token_template_by_hash(&h)
                .map(|t| t.id)
                .ok_or_else(|| Error::Invalid(format!("{}: state field {k} names no known token program", actor.name)))
        };
        // The lock's extension commitment: a KCC-20 token A pins it, a KRON one has none.
        let ext = || -> Result<[u8; 32]> {
            if actor.shape.a_family == Family::Kcc20 {
                key("lock_extension")
            } else {
                Ok([0u8; 32])
            }
        };
        let st = match actor.shape.kind {
            IntentKind::KasToToken => IntentState::KasToToken {
                payer: key("payer")?,
                merchant: key("merchant")?,
                token: key("token_covid")?,
                program: prog("token_type")?,
                amount: int("amount")?,
                max_pay: int("max_pay")?,
                max_extra: int("max_extra")?,
                b_extension: key("b_extension")?,
                deadline: int("deadline")?,
            },
            IntentKind::TokenToKas => IntentState::TokenToKas {
                payer: key("payer")?,
                merchant: key("merchant")?,
                token: key("token_covid")?,
                program: prog("token_type")?,
                merchant_kas: int("merchant_kas")?,
                max_sell: int("max_sell")?,
                lock_amount: int("lock_amount")?,
                lock_extension: ext()?,
                deadline: int("deadline")?,
            },
            IntentKind::TokenSwap => IntentState::TokenSwap {
                payer: key("payer")?,
                merchant: key("merchant")?,
                token_a: key("token_a")?,
                program_a: prog("token_a_type")?,
                token_b: key("token_b")?,
                program_b: prog("token_b_type")?,
                max_sell_a: int("max_sell_a")?,
                amount_b: int("amount_b")?,
                lock_amount: int("lock_amount")?,
                lock_extension: ext()?,
                b_extension: key("b_extension")?,
                deadline: int("deadline")?,
            },
        };
        if st.locked_program().is_some_and(|p| p.family() != actor.shape.a_family)
            || st.merchant_program().is_some_and(|p| p.family() != Family::Kcc20)
        {
            return Err(Error::Invalid(format!("{}: a token handle names a program of another family", actor.name)));
        }
        Ok(st)
    }
}

// ----------------------------------------------------------------------------------------- entries

/// The fill arguments of one execution.
///
/// They are the orders in transaction order and the base units sold into each bid. [`fill_args`]
/// adds the hidden witnesses: the handle prefix and suffix lengths of KobAsk, KobBid or KobBidKron,
/// and of the token programs the intent's state names.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct FillArgs {
    /// Covenant ids of the asks, in transaction order.
    pub asks: Vec<[u8; 32]>,
    /// Covenant ids of the bids, in transaction order.
    pub bids: Vec<[u8; 32]>,
    /// Base units sold into each bid (TokenToKas, TokenSwap).
    pub bid_amounts: Vec<i64>,
}

fn handle_lens(id: TemplateId) -> (i64, i64) {
    let t = template(id);
    (t.prefix.len() as i64, t.suffix.len() as i64)
}

/// The ABI arguments of `actor`'s fill entry for an intent of `state`, by parameter name.
pub fn fill_args(actor: &Actor, state: &IntentState, f: &FillArgs) -> Result<Vec<ArtifactValue>> {
    state.check_actor(actor)?;
    let s = actor.shape;
    if f.asks.len() != s.asks || f.bids.len() != s.bids || f.bid_amounts.len() != s.bids {
        return Err(Error::Invalid(format!("{} fills {} asks and {} bids", actor.name, s.asks, s.bids)));
    }
    let l = loaded();
    let entry = &l.abi.contracts[actor.name].entries[actor.entry];
    let idx = |name: &str, prefix: &str| -> Option<usize> { name.strip_prefix(prefix)?.parse::<usize>().ok()?.checked_sub(1) };
    let mut out = Vec::with_capacity(entry.params.len());
    for p in &entry.params {
        let n = p.name.as_str();
        let v = if let Some(i) = n.strip_suffix("_covid").and_then(|x| idx(x, "ask")) {
            b32(f.asks.get(i).ok_or_else(|| Error::Invalid(format!("{}: no ask {}", actor.name, i + 1)))?)
        } else if let Some(i) = n.strip_suffix("_covid").and_then(|x| idx(x, "bid")) {
            b32(f.bids.get(i).ok_or_else(|| Error::Invalid(format!("{}: no bid {}", actor.name, i + 1)))?)
        } else if let Some(i) = idx(n, "n_a").or_else(|| idx(n, "n")) {
            ArtifactValue::Int(
                *f.bid_amounts.get(i).ok_or_else(|| Error::Invalid(format!("{}: no amount for bid {}", actor.name, i + 1)))?,
            )
        } else {
            ArtifactValue::Int(
                hidden_arg(n, state).ok_or_else(|| Error::Invalid(format!("{}: unknown entry parameter {n}", actor.name)))?,
            )
        };
        out.push(v);
    }
    Ok(out)
}

/// A hidden witness of an entry that observes a template.
///
/// It is the prefix or suffix length of a closed ICC handle (an imported order), or of an open one
/// (a token program the intent's `state` names).
fn hidden_arg(name: &str, state: &IntentState) -> Option<i64> {
    let tok = |p: Option<TemplateId>, suffix: bool| -> Option<i64> {
        let t = crate::artifacts::token_template(p?);
        Some(if suffix { t.suffix.len() } else { t.prefix.len() } as i64)
    };
    Some(match name {
        "gen__kob_orders__kob_ask_prefix_len" => handle_lens(TemplateId::KobAsk).0,
        "gen__kob_orders__kob_ask_suffix_len" => handle_lens(TemplateId::KobAsk).1,
        "gen__kob_orders__kob_bid_prefix_len" => handle_lens(TemplateId::KobBid).0,
        "gen__kob_orders__kob_bid_suffix_len" => handle_lens(TemplateId::KobBid).1,
        "gen__kob_orders_kron__kob_bid_kron_prefix_len" => handle_lens(TemplateId::KobBidKron).0,
        "gen__kob_orders_kron__kob_bid_kron_suffix_len" => handle_lens(TemplateId::KobBidKron).1,
        // KasToToken and TokenToKas name one token (`token_type`), a swap two (`token_a_type`,
        // `token_b_type`).
        "gen__actor_type_self_token_type_prefix_len" => tok(state.merchant_program().or(state.locked_program()), false)?,
        "gen__actor_type_self_token_type_suffix_len" => tok(state.merchant_program().or(state.locked_program()), true)?,
        "gen__actor_type_self_token_a_type_prefix_len" => tok(state.locked_program(), false)?,
        "gen__actor_type_self_token_a_type_suffix_len" => tok(state.locked_program(), true)?,
        "gen__actor_type_self_token_b_type_prefix_len" => tok(state.merchant_program(), false)?,
        "gen__actor_type_self_token_b_type_suffix_len" => tok(state.merchant_program(), true)?,
        _ => return None,
    })
}

/// The hidden arguments of an entry (every parameter after the first `skip` visible ones).
fn hidden_args(actor: &Actor, state: &IntentState, entry: &str, skip: usize) -> Result<Vec<ArtifactValue>> {
    state.check_actor(actor)?;
    let l = loaded();
    let e = &l.abi.contracts[actor.name].entries[entry];
    e.params
        .iter()
        .skip(skip)
        .map(|p| {
            hidden_arg(&p.name, state)
                .map(ArtifactValue::Int)
                .ok_or_else(|| Error::Invalid(format!("{}.{entry}: unknown entry parameter {}", actor.name, p.name)))
        })
        .collect()
}

/// The ABI arguments of `actor`'s `expire` entry.
///
/// There are none for a KasToToken intent. A token intent observes the locked tokens and passes the
/// token handle's hidden witnesses.
pub fn expire_args(actor: &Actor, state: &IntentState) -> Result<Vec<ArtifactValue>> {
    hidden_args(actor, state, "expire", 0)
}

/// The ABI arguments of `actor`'s `cancel` entry after the payer's signature.
///
/// There are none for a KasToToken intent. A token intent observes the lock it must spend and
/// passes the token handle's hidden witnesses.
pub fn cancel_args(actor: &Actor, state: &IntentState) -> Result<Vec<ArtifactValue>> {
    hidden_args(actor, state, "cancel", 1)
}

/// Signature script of an intent input: the entry arguments, the dispatch tag and the redeem push.
pub fn entry_sigscript(actor: &Actor, state: &[u8], entry: &str, args: &[ArtifactValue]) -> Result<Vec<u8>> {
    let l = loaded();
    let mut s = encode_contract_entry_sig_script(&l.abi, actor.name, entry, args)
        .map_err(|e| Error::Invalid(format!("{}.{entry}: {e}", actor.name)))?;
    s.extend_from_slice(&push_data(&actor.template().redeem(state)?));
    Ok(s)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn router_loads_and_is_pinned() {
        check_router().unwrap();
        for a in &ACTORS {
            assert_eq!(Actor::by_shape_custody(&a.shape, a.custody).unwrap().name, a.name, "one actor per shape and custody legs");
            // Custody legs: KCC-20 bid legs only, at most two, never a three-bid sell; the name says
            // how many (`_c`, `_cc`).
            let tail = a.name.rsplit('_').find(|p| p.chars().all(|c| c == 'c')).map_or(0, str::len);
            assert_eq!(a.custody, tail, "{}: custody legs in the name", a.name);
            if a.custody > 0 {
                assert!(a.shape.a_family == Family::Kcc20 && a.shape.kind.locks_tokens(), "{}", a.name);
                assert!(a.custody <= a.shape.bids.min(2) && a.shape.bids < 3, "{}", a.name);
                assert!(a.entry.contains("_c"), "{}", a.name);
                // The twin without custody legs exists.
                assert_eq!(Actor::by_shape(&a.shape).unwrap().custody, 0, "{}", a.name);
            }
            let prefix = a.name.split('_').next().unwrap();
            let kron = a.shape.a_family == Family::Kron;
            assert_eq!(prefix, if kron { format!("{}Kron", a.shape.kind.as_str()) } else { a.shape.kind.as_str().to_string() });
            assert_eq!(actor_by_hash(&a.template().hash).unwrap().name, a.name);
            // a spender's entry is told apart by its dispatch tag
            let tags: std::collections::BTreeSet<[u8; 4]> =
                [a.entry, "expire", "cancel"].iter().map(|e| a.dispatch_tag(e).unwrap()).collect();
            assert_eq!(tags.len(), 3, "{}: three distinct dispatch tags", a.name);
            assert_eq!(a.dispatch_tag("transfer"), None);
        }
    }

    /// Which programs run which shapes.
    ///
    /// Every shape runs on the 8/8 program and on KRON's 4/5. The 3/3 reference program runs
    /// everything but the three-bid sells, which have 4 token outputs. A KRON token is never the
    /// merchant's, and KaspaCom's program is pending review.
    #[test]
    fn programs_fit_the_shapes() {
        use TemplateId::*;
        for a in &ACTORS {
            let s = a.shape;
            let (pa, pb) = match (s.kind, s.a_family) {
                (IntentKind::KasToToken, _) => (None, Some(Kcc20Ref8x8)),
                (IntentKind::TokenToKas, Family::Kcc20) => (Some(Kcc20Ref8x8), None),
                (IntentKind::TokenToKas, Family::Kron) => (Some(KronToken2433), None),
                (IntentKind::TokenSwap, Family::Kcc20) => (Some(Kcc20Ref8x8), Some(Kcc20Ref8x8)),
                (IntentKind::TokenSwap, Family::Kron) => (Some(KronToken2732), Some(Kcc20Ref8x8)),
            };
            assert!(a.fits(pa, pb), "{}", a.name);
            // The 3/3 programs: the reference and its published public-mint build.
            for p3 in [Kcc20Ref, Kcc20PublicMint] {
                let three = |p: Option<TemplateId>| p.map(|_| p3);
                let fits3 = a.fits(if s.a_family == Family::Kcc20 { three(pa) } else { pa }, three(pb));
                // A KRON token A keeps its program. Only token B moves to the 3/3 program, and a
                // KRON sell has no token B.
                // The custody legs fit: the lock and two custodies are three token A inputs.
                assert_eq!(fits3, !(s.a_family == Family::Kcc20 && s.bids == 3), "{} on {}", a.name, p3.name());
                assert_eq!(a.token_slots().0 .0, usize::from(s.kind.locks_tokens()) + a.custody, "{}", a.name);
            }
            if pa.is_some() {
                assert!(!a.fits(Some(Kcc20KaspaCom025), pb), "{}", a.name);
            }
            if s.kind.merchant_gets_token() {
                assert!(!a.fits(pa, Some(KronToken2433)), "{}: a KRON merchant token", a.name);
                assert!(!a.fits(pa, Some(Kcc20KaspaCom025)), "{}", a.name);
            }
            if let Some(p) = pa {
                let other = if p.family() == Family::Kron { Kcc20Ref8x8 } else { KronToken2433 };
                assert!(!a.fits(Some(other), pb), "{}: token A of the other family", a.name);
            }
        }
        assert!(intent_program(Kcc20KaspaCom025).is_err());
        assert!(intent_program(KobAsk).is_err());
        for p in [Kcc20Ref, Kcc20Ref4x5, Kcc20Ref8x8, Kcc20Ref16x16, Kcc20P2, Kcc20PublicMint, KronToken2433, KronToken2732] {
            intent_program(p).unwrap();
        }
    }

    /// The embedded ABI is the `sil_abi` member of the committed Argent artifact, byte for byte in
    /// content.
    ///
    /// `KOB_WRITE_ROUTER_ABI=1` rewrites the copy.
    #[test]
    fn embedded_abi_is_the_committed_artifact() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        let art: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(root.join("contracts/argent/router/artifact.json")).unwrap()).unwrap();
        assert_eq!(art["id"], ROUTER_ARTIFACT_ID, "the pinned artifact id");
        let mine: serde_json::Value = serde_json::from_str(SIL_ABI_JSON).unwrap();
        if std::env::var_os("KOB_WRITE_ROUTER_ABI").is_some() {
            let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("data/router_sil_abi.json");
            std::fs::write(path, format!("{}\n", serde_json::to_string(&art["sil_abi"]).unwrap())).unwrap();
            return;
        }
        assert_eq!(mine, art["sil_abi"], "data/router_sil_abi.json is stale: rerun with KOB_WRITE_ROUTER_ABI=1");
    }

    /// The TypeScript x402 SDK carries its own copy of the router artifact id, which must be this
    /// one.
    ///
    /// The copy is in `packages/kob-x402/src/types.ts`.
    #[test]
    fn ts_sdk_names_the_same_router_artifact() {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../packages/kob-x402/src/types.ts");
        let src = std::fs::read_to_string(&path).unwrap();
        let ids: Vec<&str> = src
            .lines()
            .filter_map(|l| l.trim().strip_prefix("export const ROUTER_ARTIFACT_ID = '"))
            .filter_map(|rest| rest.strip_suffix("';"))
            .collect();
        assert_eq!(ids, [ROUTER_ARTIFACT_ID], "ROUTER_ARTIFACT_ID in {}", path.display());
    }

    #[test]
    fn state_roundtrip_and_lengths() {
        let k = [0x11; 32];
        const D: i64 = 1_800_000_000_000;
        let p8 = TemplateId::Kcc20Ref8x8;
        let states = [
            IntentState::KasToToken {
                payer: k,
                merchant: k,
                token: [7; 32],
                program: TemplateId::Kcc20Ref,
                amount: 5,
                max_pay: 6,
                max_extra: 7,
                b_extension: [0xbe; 32],
                deadline: D,
            },
            IntentState::TokenToKas {
                payer: k,
                merchant: k,
                token: [7; 32],
                program: p8,
                merchant_kas: 9,
                max_sell: 3,
                lock_amount: 5,
                lock_extension: [0xee; 32],
                deadline: D + 1,
            },
            IntentState::TokenSwap {
                payer: k,
                merchant: k,
                token_a: [7; 32],
                program_a: TemplateId::Kcc20Ref,
                token_b: [8; 32],
                program_b: p8,
                max_sell_a: 4,
                amount_b: 2,
                lock_amount: 6,
                lock_extension: [0xee; 32],
                b_extension: [0xbe; 32],
                deadline: D + 2,
            },
            IntentState::TokenToKas {
                payer: k,
                merchant: k,
                token: [7; 32],
                program: TemplateId::KronToken2433,
                merchant_kas: 9,
                max_sell: 3,
                lock_amount: 5,
                lock_extension: [0; 32],
                deadline: D + 3,
            },
            IntentState::TokenSwap {
                payer: k,
                merchant: k,
                token_a: [7; 32],
                program_a: TemplateId::KronToken2732,
                token_b: [8; 32],
                program_b: p8,
                max_sell_a: 4,
                amount_b: 2,
                lock_amount: 6,
                lock_extension: [0; 32],
                b_extension: [0xbe; 32],
                deadline: D + 4,
            },
        ];
        for a in &ACTORS {
            for st in states.iter().filter(|s| s.kind() == a.shape.kind && s.a_family() == a.shape.a_family) {
                // The 3/3 program has no room for a three-bid sell.
                if st.check_actor(a).is_err() {
                    assert_eq!((st.locked_program(), a.shape.bids), (Some(TemplateId::Kcc20Ref), 3), "{}", a.name);
                    continue;
                }
                let enc = st.encode(a).unwrap();
                assert_eq!(enc.len(), a.template().state_len);
                assert_eq!(&IntentState::decode(a, &enc).unwrap(), st);
                let (found, span) = instance_of(&st.redeem(a).unwrap()).unwrap();
                assert_eq!((found.name, span), (a.name, enc));
            }
            for other in states.iter().filter(|s| s.kind() != a.shape.kind || s.a_family() != a.shape.a_family) {
                assert!(other.encode(a).is_err(), "{}", a.name);
            }
        }
    }

    #[test]
    fn non_canonical_state_is_refused() {
        let st = IntentState::KasToToken {
            payer: [1; 32],
            merchant: [2; 32],
            token: [7; 32],
            program: TemplateId::Kcc20Ref8x8,
            amount: 5,
            max_pay: 6,
            max_extra: 0,
            b_extension: [0xbe; 32],
            deadline: 9,
        };
        let a = ACTORS.iter().find(|a| a.shape.kind == st.kind()).unwrap();
        let enc = st.encode(a).unwrap();
        let zero = [0x08, 0, 0, 0, 0, 0, 0, 0, 0];
        let at = enc.windows(9).position(|w| w == zero).expect("max_extra = 0 is encoded");
        let mut neg_zero = enc.clone();
        neg_zero[at + 8] = 0x80;
        assert!(IntentState::decode(a, &neg_zero).is_err(), "KCC-1 3.7.1: negative zero is not canonical");
        assert_eq!(IntentState::decode(a, &enc).unwrap(), st);
    }

    #[test]
    fn fill_args_follow_the_abi() {
        let a = Actor::by_name("TokenSwap_swap2").unwrap();
        let st = IntentState::TokenSwap {
            payer: [1; 32],
            merchant: [2; 32],
            token_a: [7; 32],
            program_a: TemplateId::Kcc20Ref,
            token_b: [8; 32],
            program_b: TemplateId::Kcc20Ref8x8,
            max_sell_a: 4,
            amount_b: 2,
            lock_amount: 6,
            lock_extension: [0xee; 32],
            b_extension: [0xbe; 32],
            deadline: 1_800_000_000_000,
        };
        let f = FillArgs { asks: vec![[1; 32], [2; 32]], bids: vec![[3; 32], [4; 32]], bid_amounts: vec![5, 6] };
        let v = fill_args(a, &st, &f).unwrap();
        assert_eq!(v[0], ArtifactValue::Bytes(vec![3; 32]));
        assert_eq!(v[2], ArtifactValue::Bytes(vec![1; 32]));
        assert_eq!(v[4], ArtifactValue::Int(5));
        assert_eq!(v[5], ArtifactValue::Int(6));
        // The hidden witnesses: KobAsk, KobBid, then the handles of A (the 3/3 program) and B (the
        // 8/8 one).
        let t3 = crate::artifacts::token_template(TemplateId::Kcc20Ref);
        let t8 = crate::artifacts::token_template(TemplateId::Kcc20Ref8x8);
        let lens = |x: [usize; 4]| x.map(|n| ArtifactValue::Int(n as i64));
        assert_eq!(v.len(), 14);
        assert_eq!(v[10..], lens([t3.prefix.len(), t3.suffix.len(), t8.prefix.len(), t8.suffix.len()]));
        assert!(fill_args(a, &st, &FillArgs::default()).is_err());
        let k = Actor::by_name("TokenToKasKron_sell").unwrap();
        let kst = IntentState::TokenToKas {
            payer: [1; 32],
            merchant: [2; 32],
            token: [7; 32],
            program: TemplateId::KronToken2433,
            merchant_kas: 9,
            max_sell: 3,
            lock_amount: 5,
            lock_extension: [0; 32],
            deadline: 1_800_000_000_000,
        };
        let kv = fill_args(k, &kst, &FillArgs { asks: vec![], bids: vec![[3; 32]], bid_amounts: vec![2] }).unwrap();
        let bk = crate::artifacts::template(TemplateId::KobBidKron);
        let tk = crate::artifacts::token_template(TemplateId::KronToken2433);
        assert_eq!(kv[2..], lens([bk.prefix.len(), bk.suffix.len(), tk.prefix.len(), tk.suffix.len()]));
        let two = [tk.prefix.len(), tk.suffix.len()].map(|n| ArtifactValue::Int(n as i64));
        assert_eq!(cancel_args(k, &kst).unwrap(), two);
        assert_eq!(expire_args(k, &kst).unwrap(), two);
        assert!(fill_args(a, &kst, &f).is_err(), "a KRON intent on a KCC-20 actor");
    }
}
