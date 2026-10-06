//! The KOB router (`contracts/argent/kob_router.ag`): payment intents any keeper can execute.
//!
//! An intent is one covenant UTXO of one router actor. The payer creates it once (one signature,
//! the creation transaction) and can always cancel it (`cancel`, SIGHASH_ALL); any keeper may
//! execute it against the current book in the fill shape the actor was chosen for, without the
//! payer; from its `deadline` (unix ms) on, anyone may `expire` it, which returns its KAS (less at
//! most [`EXPIRE_MAX_FEE`]) and its locked tokens to the payer (`docs/argent.md`, "The router"):
//!
//! | Intent | Payer locks | Merchant receives |
//! |---|---|---|
//! | `KasToToken_<shape>` | KAS | exactly `amount` of token B, bought from 1-3 `KobAsk`s |
//! | `TokenToKas_<shape>` | token A (owner = the intent's covenant id) | at least `merchant_kas`, from selling into 1-3 `KobBid`s |
//! | `TokenSwap_<shape>` | token A (as above) | exactly `amount_b` of token B: A into 1-2 bids, the KAS into 1-2 asks |
//! | `TokenToKasKron_<shape>`, `TokenSwapKron_<shape>` | a KRON token A | as `TokenToKas` / `TokenSwap`, A sold into `KobBidKron`s |
//!
//! The router links the orders (`KobAsk`, `KobBid`, `KobBidKron`) by closed ICC. The token programs are open
//! ICC handles in the intent's state ([`IntentState`] names the program of every token it moves, and the
//! router reads and pins the tokens under it): one actor serves every KCC-20 program of the 112-byte state
//! and every KRON program ([`intent_program`] says which programs an intent may name). A KRON token is paid
//! with, never received (the merchant receives KAS or a KCC-20 token).
//!
//! This module embeds the router's SilverScript ABI (the `sil_abi` member of the committed Argent
//! artifact `contracts/argent/router/artifact.json`, extracted into `data/router_sil_abi.json`; a test
//! keeps the copy equal to the artifact) and pins every actor's template hash ([`ROUTER_PINNED`]):
//! a different router is a different protocol and fails to load.

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
pub const ROUTER_ARTIFACT_ID: &str = "a85ee4b6c560818e056db2008d06050b503a93f5d194095f4585e5b82f451f5d";

/// Most sompi an `expire` may keep of the intent's KAS (`EXPIRE_MAX_FEE` of `tools/router-gen/router_head.ag`).
pub const EXPIRE_MAX_FEE: u64 = 10_000_000;

/// Least KAS an intent UTXO may hold: twice [`EXPIRE_MAX_FEE`]. Its cancel and its expiry take their fee from the intent's own
/// KAS (they have no funding input), and the payer's output they leave must itself stay a standard output (KIP-9 storage
/// mass): an intent below this could be neither cancelled nor expired. The builder and the verifier refuse it.
pub const MIN_INTENT_VALUE: u64 = 2 * EXPIRE_MAX_FEE;

/// Checks that `p` is a token program an intent may name: a KCC-20 program of the 112-byte state or a KRON
/// program (the router observes tokens by open ICC under the program the intent's state names). KaspaCom's
/// KCC20 0.2.5 is refused while it is pending review (a 25.5 KB program with a 0.5 KAS token output floor).
pub fn intent_program(p: TemplateId) -> Result<()> {
    if !p.is_token() {
        return Err(Error::Invalid(format!("{} is not a token program", p.name())));
    }
    if p == TemplateId::Kcc20KaspaCom025 {
        return Err(Error::Invalid(format!("{} is pending review: no router intent trades it", p.name())));
    }
    if p.family() == Family::Kcc20 && crate::artifacts::token_template(p).state_len != 112 {
        return Err(Error::Invalid(format!("{} has no 112-byte KCC-20 state", p.name())));
    }
    Ok(())
}

/// True when the token slots of `p` (inputs, outputs of one token per transaction) hold `ins` / `outs`.
fn slots_fit(p: TemplateId, ins: usize, outs: usize) -> bool {
    p.token_slots().is_some_and(|(i, o)| ins <= i && outs <= o)
}

/// Template hash of every router actor (hex). A changed router is a changed protocol.
pub const ROUTER_PINNED: [(&str, &str); 30] = [
    ("KasToToken_buy", "73d68be7c72c651acd472770cf286b5de60b38405438d54f51c720ec9e570215"),
    ("KasToToken_buy_out", "e7752fa426ae34bdd44b09f573da3a9e21cee8ed2db0e1cc9a9c05e6774e03ed"),
    ("KasToToken_buy2", "9db4ee36cb47048d24c101b6450e33449e7f04a3634240436db204da6725a6af"),
    ("KasToToken_buy2_out", "20527396e25c3e25382e21a3544ae2a29afb3f3b8ae7d0ebcb4b6494132d6156"),
    ("KasToToken_buy3", "f3c70ee44aeee7882f4a0da1a66591d39cc4bcb0c8021fd62fa4ab683ab961c4"),
    ("KasToToken_buy3_out", "9873c0b1901eadbb20c4e51478ca6791803e17614cd8dfb7858b45c124c384ca"),
    ("TokenToKas_sell", "679a809cdabed073278262490214dab8971ae1a65ad1ae798ec4f4047655323f"),
    ("TokenToKas_sell_out", "d7f98ef34d8ce02ae2d716fcc2c4e3bf5c48633a45e7aee7538b47068468866a"),
    ("TokenToKas_sell2", "df80ad0eb522e2c7e6aa8ef843bf517ff8879a6e5856cee620e278d351998a09"),
    ("TokenToKas_sell2_out", "a3e339544a5b9ac05b6e0f8fae5f00f1e2477535f3538d9d7ddf964c414e98f6"),
    ("TokenToKas_sell3", "fd4c735d414c4721b3ea8585c18f6aab7b18da4ccbcb72036494effbfb0ad8b0"),
    ("TokenToKas_sell3_out", "905215a8975dff5dd8cc74454750fa4f0fc3106d92a35267b614f3b0b41e645a"),
    ("TokenSwap_swap", "21c66d3fec270ecd2af2dad23dcb48ff429f37fd4f43670368911d6b5db5f024"),
    ("TokenSwap_swap_bid_out", "901331e198dfef6d22518827aad96e8fbc854b6c445e2a661a00893426f38d52"),
    ("TokenSwap_swap_ask_out", "64d6c5d67f9255233ec4105fd9590927e8fdad53454d9a828a7644a56eb2ce75"),
    ("TokenSwap_swap_out", "ffdf3af018c987476e0a884d4915bbad9ae7293116199b36662a2f5b9f3e872a"),
    ("TokenSwap_swap2", "4729d6ae5c15b5557525ad44b40fcbccb1727f7d2b8a4681e7516a05715cd9fa"),
    ("TokenSwap_swap2_out", "88b71e2c21f3f95eb3f324d9e5c10c1bd49a709e21063e6e291495b1eb3125fc"),
    ("TokenToKasKron_sell", "b4b50e3f12670616c4d025a4306901406c8cdbf9997d9db142ab8bf2d45bdd41"),
    ("TokenToKasKron_sell_out", "50c518bfb409f2138bcc118a3da299021460930c028466938399965246ffa41c"),
    ("TokenToKasKron_sell2", "903ed88b81c4deecb6bee8dcf80fe9dd2630ecc187af3aa0ffe182287d45d8d9"),
    ("TokenToKasKron_sell2_out", "17a65a8d867b9f45c7592d42a9f6ee24b157dd2839564d667b5d640ffa7a70e4"),
    ("TokenToKasKron_sell3", "f135a653a8203e40bcfb35fe6aa8b50a086238d6811211637722f31eaa4947c4"),
    ("TokenToKasKron_sell3_out", "9465c0117c5f2a5325240184a33292d524a080e220678ad3fa1adeeafa8fbfc4"),
    ("TokenSwapKron_swap", "9490de6fe0512c44a7165c2ef1db22904c0403b78c94f4c9f2267f327665beee"),
    ("TokenSwapKron_swap_bid_out", "be593f30992a26df06dd959c7e78ed72e8c10769b508e770861031028d2cef9d"),
    ("TokenSwapKron_swap_ask_out", "bd037f70d0a01596353c3a7326f625247b731800cd64ed6ca1a29a5f7deec109"),
    ("TokenSwapKron_swap_out", "78c7781ca4b2c95da67010a2a0a0f9b98c2144f0f8e03752c25f601903fff9c2"),
    ("TokenSwapKron_swap2", "10482ffe6c83540a887447e2df802643f2f50aa97ba58de5fe014acff4b1df6d"),
    ("TokenSwapKron_swap2_out", "532794dbd066b0d952f09bf31d067d057fbc0ddba81bb8ee0997c2431094a013"),
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
}

/// The fill shape of one router actor: how many asks and bids the keeper fills, and whether the
/// last order of each leg rests (continues, GTC) or ends (sold out / exhausted / IOC / FOK). Every
/// order of a leg but the last ends.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Shape {
    pub kind: IntentKind,
    pub asks: usize,
    pub bids: usize,
    pub last_ask_rests: bool,
    pub last_bid_rests: bool,
    /// Family of token A, the token a token intent locks and sells (`KobBidKron`s for KRON); KasToToken: KCC-20.
    #[serde(default)]
    pub a_family: Family,
}

impl Shape {
    /// Token slots the execution of this shape needs: (inputs, outputs) of token A, then of token B.
    pub fn token_slots(&self) -> ((usize, usize), (usize, usize)) {
        let a = if self.kind.locks_tokens() { (1, self.bids + 1) } else { (0, 0) };
        let b = if self.kind.merchant_gets_token() { (self.asks, 1 + usize::from(self.last_ask_rests)) } else { (0, 0) };
        (a, b)
    }
    /// True when the programs of token A (`a`) and token B (`b`) can run this shape: the families match (token A of
    /// `a_family`, token B KCC-20) and the programs' slots hold its token inputs and outputs (the 3/3 reference program
    /// has no room for a three-bid sell: four token outputs).
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

/// One router actor (= one template): its name, its fill entry and its shape.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Actor {
    pub name: &'static str,
    /// The fill entry (the actor's other entries are `expire` and `cancel`).
    pub entry: &'static str,
    pub shape: Shape,
}

const fn sh(kind: IntentKind, asks: usize, bids: usize, last_ask_rests: bool, last_bid_rests: bool) -> Shape {
    Shape { kind, asks, bids, last_ask_rests, last_bid_rests, a_family: Family::Kcc20 }
}
const fn shk(kind: IntentKind, asks: usize, bids: usize, last_ask_rests: bool, last_bid_rests: bool) -> Shape {
    Shape { kind, asks, bids, last_ask_rests, last_bid_rests, a_family: Family::Kron }
}

use IntentKind::{KasToToken as K2T, TokenSwap as SW, TokenToKas as T2K};

/// Every actor of the router.
pub const ACTORS: [Actor; 30] = [
    Actor { name: "KasToToken_buy", entry: "buy", shape: sh(K2T, 1, 0, true, false) },
    Actor { name: "KasToToken_buy_out", entry: "buy_out", shape: sh(K2T, 1, 0, false, false) },
    Actor { name: "KasToToken_buy2", entry: "buy2", shape: sh(K2T, 2, 0, true, false) },
    Actor { name: "KasToToken_buy2_out", entry: "buy2_out", shape: sh(K2T, 2, 0, false, false) },
    Actor { name: "KasToToken_buy3", entry: "buy3", shape: sh(K2T, 3, 0, true, false) },
    Actor { name: "KasToToken_buy3_out", entry: "buy3_out", shape: sh(K2T, 3, 0, false, false) },
    Actor { name: "TokenToKas_sell", entry: "sell", shape: sh(T2K, 0, 1, false, true) },
    Actor { name: "TokenToKas_sell_out", entry: "sell_out", shape: sh(T2K, 0, 1, false, false) },
    Actor { name: "TokenToKas_sell2", entry: "sell2", shape: sh(T2K, 0, 2, false, true) },
    Actor { name: "TokenToKas_sell2_out", entry: "sell2_out", shape: sh(T2K, 0, 2, false, false) },
    Actor { name: "TokenToKas_sell3", entry: "sell3", shape: sh(T2K, 0, 3, false, true) },
    Actor { name: "TokenToKas_sell3_out", entry: "sell3_out", shape: sh(T2K, 0, 3, false, false) },
    Actor { name: "TokenSwap_swap", entry: "swap", shape: sh(SW, 1, 1, true, true) },
    Actor { name: "TokenSwap_swap_bid_out", entry: "swap_bid_out", shape: sh(SW, 1, 1, true, false) },
    Actor { name: "TokenSwap_swap_ask_out", entry: "swap_ask_out", shape: sh(SW, 1, 1, false, true) },
    Actor { name: "TokenSwap_swap_out", entry: "swap_out", shape: sh(SW, 1, 1, false, false) },
    Actor { name: "TokenSwap_swap2", entry: "swap2", shape: sh(SW, 2, 2, true, true) },
    Actor { name: "TokenSwap_swap2_out", entry: "swap2_out", shape: sh(SW, 2, 2, false, false) },
    Actor { name: "TokenToKasKron_sell", entry: "sell", shape: shk(T2K, 0, 1, false, true) },
    Actor { name: "TokenToKasKron_sell_out", entry: "sell_out", shape: shk(T2K, 0, 1, false, false) },
    Actor { name: "TokenToKasKron_sell2", entry: "sell2", shape: shk(T2K, 0, 2, false, true) },
    Actor { name: "TokenToKasKron_sell2_out", entry: "sell2_out", shape: shk(T2K, 0, 2, false, false) },
    Actor { name: "TokenToKasKron_sell3", entry: "sell3", shape: shk(T2K, 0, 3, false, true) },
    Actor { name: "TokenToKasKron_sell3_out", entry: "sell3_out", shape: shk(T2K, 0, 3, false, false) },
    Actor { name: "TokenSwapKron_swap", entry: "swap", shape: shk(SW, 1, 1, true, true) },
    Actor { name: "TokenSwapKron_swap_bid_out", entry: "swap_bid_out", shape: shk(SW, 1, 1, true, false) },
    Actor { name: "TokenSwapKron_swap_ask_out", entry: "swap_ask_out", shape: shk(SW, 1, 1, false, true) },
    Actor { name: "TokenSwapKron_swap_out", entry: "swap_out", shape: shk(SW, 1, 1, false, false) },
    Actor { name: "TokenSwapKron_swap2", entry: "swap2", shape: shk(SW, 2, 2, true, true) },
    Actor { name: "TokenSwapKron_swap2_out", entry: "swap2_out", shape: shk(SW, 2, 2, false, false) },
];

impl Actor {
    /// The actor of this name.
    pub fn by_name(name: &str) -> Option<&'static Actor> {
        ACTORS.iter().find(|a| a.name == name)
    }
    /// The actor of a shape.
    pub fn by_shape(shape: &Shape) -> Option<&'static Actor> {
        ACTORS.iter().find(|a| a.shape == *shape)
    }
    /// The loaded, pinned template.
    pub fn template(&self) -> &'static RouterTemplate {
        &loaded().templates[self.name]
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

// ---------------------------------------------------------------------------------------------- state

fn default_program() -> TemplateId {
    TemplateId::Kcc20Ref8x8
}

/// The terms of an intent (the actor's state). Keys are x-only public keys, token ids covenant ids. Every token
/// is named twice: its covenant id and its program, whose template hash the router's open ICC handle carries
/// (`program*` fields; a JSON without them means `KCC20Ref_8x8`, the only program of the routers before them).
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
        /// Unix ms from which anyone may expire the intent (`expire`, CLTV `tx.time`): the payer's authorization expiry.
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
        /// Unix ms from which anyone may expire the intent (`expire`, CLTV `tx.time`): the payer's authorization expiry.
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
        /// Unix ms from which anyone may expire the intent (`expire`, CLTV `tx.time`): the payer's authorization expiry.
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
    pub fn kind(&self) -> IntentKind {
        match self {
            IntentState::KasToToken { .. } => IntentKind::KasToToken,
            IntentState::TokenToKas { .. } => IntentKind::TokenToKas,
            IntentState::TokenSwap { .. } => IntentKind::TokenSwap,
        }
    }
    pub fn payer(&self) -> [u8; 32] {
        match self {
            IntentState::KasToToken { payer, .. } | IntentState::TokenToKas { payer, .. } | IntentState::TokenSwap { payer, .. } => {
                *payer
            }
        }
    }
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
    /// The family of the locked token A (KasToToken: KCC-20, the family of every actor without a token A).
    pub fn a_family(&self) -> Family {
        self.locked_program().map(|p| p.family()).unwrap_or(Family::Kcc20)
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
            IntentState::KasToToken { payer, merchant, token, program, amount, max_pay, max_extra, deadline } => {
                put("payer", b32(payer));
                put("merchant", b32(merchant));
                put("token_covid", b32(token));
                put("token_type", handle(*program));
                put("amount", ArtifactValue::Int(*amount));
                put("max_pay", ArtifactValue::Int(*max_pay));
                put("max_extra", ArtifactValue::Int(*max_extra));
                put("deadline", ArtifactValue::Int(*deadline));
            }
            IntentState::TokenToKas { payer, merchant, token, program, merchant_kas, max_sell, deadline } => {
                put("payer", b32(payer));
                put("merchant", b32(merchant));
                put("token_covid", b32(token));
                put("token_type", handle(*program));
                put("merchant_kas", ArtifactValue::Int(*merchant_kas));
                put("max_sell", ArtifactValue::Int(*max_sell));
                put("deadline", ArtifactValue::Int(*deadline));
            }
            IntentState::TokenSwap { payer, merchant, token_a, program_a, token_b, program_b, max_sell_a, amount_b, deadline } => {
                put("payer", b32(payer));
                put("merchant", b32(merchant));
                put("token_a", b32(token_a));
                put("token_a_type", handle(*program_a));
                put("token_b", b32(token_b));
                put("token_b_type", handle(*program_b));
                put("max_sell_a", ArtifactValue::Int(*max_sell_a));
                put("amount_b", ArtifactValue::Int(*amount_b));
                put("deadline", ArtifactValue::Int(*deadline));
            }
        }
        m
    }

    /// Checks the terms: keys on the curve, positive amounts and bounds, distinct tokens, programs an intent may
    /// name ([`intent_program`]; the merchant's token KCC-20), a deadline that is a time lock (unix ms, at least
    /// [`kaspa_txscript::LOCK_TIME_THRESHOLD`]; a smaller value could never be met by `expire`).
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
        Ok(())
    }

    /// Checks that `actor` can carry these terms: its kind, the family of token A, and the token slots of the
    /// programs (a shape a program cannot run is refused here, not after the payer locked its funds).
    pub fn check_actor(&self, actor: &Actor) -> Result<()> {
        if actor.shape.kind != self.kind() {
            return Err(Error::Invalid(format!("{} is not a {} actor", actor.name, self.kind().as_str())));
        }
        if actor.shape.a_family != self.a_family() {
            return Err(Error::Invalid(format!("{} does not trade a {:?} token A", actor.name, self.a_family())));
        }
        if !actor.shape.fits(self.locked_program(), self.merchant_program()) {
            return Err(Error::Invalid(format!("{}: the token programs have no room for its token inputs and outputs", actor.name)));
        }
        Ok(())
    }

    /// The encoded state span under `actor` (the actor must carry these terms, [`IntentState::check_actor`]).
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
        // KCC-1 3.7.1: the consumed bytes must be exactly the canonical encoding of the decoded values (no negative zero,
        // no over-long push prefix); silverscript-abi's decoder alone accepts both.
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
        // a handle names a known token program, or the instance is not one KOB can build or check
        let prog = |k: &str| -> Result<TemplateId> {
            let h = key(k)?;
            crate::artifacts::token_template_by_hash(&h)
                .map(|t| t.id)
                .ok_or_else(|| Error::Invalid(format!("{}: state field {k} names no known token program", actor.name)))
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
                deadline: int("deadline")?,
            },
            IntentKind::TokenToKas => IntentState::TokenToKas {
                payer: key("payer")?,
                merchant: key("merchant")?,
                token: key("token_covid")?,
                program: prog("token_type")?,
                merchant_kas: int("merchant_kas")?,
                max_sell: int("max_sell")?,
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

// ---------------------------------------------------------------------------------------------- entries

/// The fill arguments of one execution: the orders in transaction order and the base units sold into
/// each bid. The hidden witnesses (handle prefix / suffix lengths of KobAsk, KobBid / KobBidKron and of
/// the token programs the intent's state names) are added by [`fill_args`].
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

/// A hidden witness of an entry that observes a template: the prefix or suffix length of a closed ICC handle
/// (an imported order) or of an open one (a token program the intent's `state` names).
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
        // KasToToken / TokenToKas name one token (`token_type`), a swap two (`token_a_type`, `token_b_type`)
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

/// The ABI arguments of `actor`'s `expire` entry: none for a KasToToken intent, the token handle's hidden
/// witnesses for a token intent (it observes the locked tokens).
pub fn expire_args(actor: &Actor, state: &IntentState) -> Result<Vec<ArtifactValue>> {
    hidden_args(actor, state, "expire", 0)
}

/// The ABI arguments of `actor`'s `cancel` entry after the payer's signature: none for a KasToToken intent, the
/// token handle's hidden witnesses for a token intent (it observes the lock it must spend).
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
            assert_eq!(Actor::by_shape(&a.shape).unwrap().name, a.name, "one actor per shape");
            let prefix = a.name.split('_').next().unwrap();
            let kron = a.shape.a_family == Family::Kron;
            assert_eq!(prefix, if kron { format!("{}Kron", a.shape.kind.as_str()) } else { a.shape.kind.as_str().to_string() });
            assert_eq!(actor_by_hash(&a.template().hash).unwrap().name, a.name);
        }
    }

    /// Which programs run which shapes: every shape on the 8/8 program and on KRON's 4/5; the 3/3 reference program
    /// runs everything but the three-bid sells (4 token outputs); a KRON token is never the merchant's; KaspaCom's
    /// program is pending review.
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
            assert!(s.fits(pa, pb), "{}", a.name);
            let three = |p: Option<TemplateId>| p.map(|_| Kcc20Ref);
            let fits3 = s.fits(if s.a_family == Family::Kcc20 { three(pa) } else { pa }, three(pb));
            // (a KRON token A keeps its program: only token B moves to the 3/3 program, and a KRON sell has no token B)
            assert_eq!(fits3, !(s.a_family == Family::Kcc20 && s.bids == 3), "{} on the 3/3 program", a.name);
            if pa.is_some() {
                assert!(!s.fits(Some(Kcc20KaspaCom025), pb), "{}", a.name);
            }
            if s.kind.merchant_gets_token() {
                assert!(!s.fits(pa, Some(KronToken2433)), "{}: a KRON merchant token", a.name);
                assert!(!s.fits(pa, Some(Kcc20KaspaCom025)), "{}", a.name);
            }
            if let Some(p) = pa {
                let other = if p.family() == Family::Kron { Kcc20Ref8x8 } else { KronToken2433 };
                assert!(!s.fits(Some(other), pb), "{}: token A of the other family", a.name);
            }
        }
        assert!(intent_program(Kcc20KaspaCom025).is_err());
        assert!(intent_program(KobAsk).is_err());
        for p in [Kcc20Ref, Kcc20Ref4x5, Kcc20Ref8x8, Kcc20Ref16x16, Kcc20P2, KronToken2433, KronToken2732] {
            intent_program(p).unwrap();
        }
    }

    /// The embedded ABI is the `sil_abi` member of the committed Argent artifact, byte for byte in
    /// content (`KOB_WRITE_ROUTER_ABI=1` rewrites the copy).
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
                deadline: D,
            },
            IntentState::TokenToKas {
                payer: k,
                merchant: k,
                token: [7; 32],
                program: p8,
                merchant_kas: 9,
                max_sell: 3,
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
                deadline: D + 2,
            },
            IntentState::TokenToKas {
                payer: k,
                merchant: k,
                token: [7; 32],
                program: TemplateId::KronToken2433,
                merchant_kas: 9,
                max_sell: 3,
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
                deadline: D + 4,
            },
        ];
        for a in &ACTORS {
            for st in states.iter().filter(|s| s.kind() == a.shape.kind && s.a_family() == a.shape.a_family) {
                // the 3/3 program has no room for a three-bid sell
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
            deadline: 1_800_000_000_000,
        };
        let f = FillArgs { asks: vec![[1; 32], [2; 32]], bids: vec![[3; 32], [4; 32]], bid_amounts: vec![5, 6] };
        let v = fill_args(a, &st, &f).unwrap();
        assert_eq!(v[0], ArtifactValue::Bytes(vec![3; 32]));
        assert_eq!(v[2], ArtifactValue::Bytes(vec![1; 32]));
        assert_eq!(v[4], ArtifactValue::Int(5));
        assert_eq!(v[5], ArtifactValue::Int(6));
        // the hidden witnesses: KobAsk, KobBid, then the handles of A (the 3/3 program) and B (the 8/8 one)
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
