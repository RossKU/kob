//! Shared fixtures of the multi-family swap-and-pay tests: KRON, KaspaCom-template and reference
//! KCC-20 tokens with real KOB order UTXOs (bids, asks with their custody) on a `MockChain` that runs
//! every submitted transaction through the rusty-kaspa engine.
//!
//! Included with `#[path]` by `kob-x402/tests/swap_families.rs` and by the facilitator's end-to-end
//! test in `kob-executor`; the includer must also declare `mod common;` (the protocol's fixtures,
//! `kob-protocol/tests/common/mod.rs`) at its crate root.
#![allow(dead_code)]

use kaspa_addresses::{Address, Prefix, Version};
use std::sync::Arc;

use kaspa_consensus_core::tx::ScriptPublicKey;
use kob_protocol::artifacts::{token_template, TemplateId};
use kob_protocol::registry::Family;
use kob_protocol::script::p2pk_spk;
use kob_protocol::state::{AskState, BidState, OrderState, TokenState};
use kob_protocol::tx::{KeyUtxo, OrderUtxo, TokenUtxo, Utxo};
use kob_x402::chain::{ChainUtxo, ChainView, FixedClock, Outpoint, OutputStatus};
use kob_x402::client::swap::{
    pay_swap, swap_requirements, MerchantGain, OrderRef, PayAssetSpec, PayerFunds, Quote, SwapOfferParams, SwapOptions, SwapPayment,
};
use kob_x402::error::X402Error;
use kob_x402::policy::{AllowedToken, Custody, Policy};
use kob_x402::testkit::MockChain;
use kob_x402::verify::{verify_payment, Verified, VerifyCtx};
use kob_x402::wire::{Finality, Network, PaymentPayload, PaymentRequirements};

use crate::common::{self, keys, pk, CARRIER, DC, KAS, NOW, P245, P250, WHOLE};

/// Fails loudly when the test binary and the `kob-x402` library it runs against were compiled from different checkouts.
///
/// Worktrees that share one `CARGO_TARGET_DIR` (parallel agents, a coordinator building main next to feature branches)
/// share artifacts: cargo's metadata hash is relative to the workspace root and freshness is decided by file mtimes, so a
/// build in one checkout can link (or reuse) a test binary / library compiled from another. The symptom is a test that
/// fails in one invocation and passes in another with no code difference (for instance the payer's carrier ceiling
/// showing up in a checkout that does not have it). `own_manifest_dir` is the includer's `env!("CARGO_MANIFEST_DIR")`
/// (a sibling crate of `kob-x402`, or `kob-x402` itself).
pub fn assert_single_checkout(own_manifest_dir: &str) {
    use std::path::{Path, PathBuf};
    let canon = |p: &Path| -> Option<PathBuf> { std::fs::canonicalize(p).ok() };
    let compiled_lib = canon(Path::new(kob_x402::SOURCE_ROOT));
    let running_lib = canon(&Path::new(own_manifest_dir).join("../kob-x402"));
    // the manifest dir cargo hands to the test process at run time is the checkout that started it
    let runtime_own = std::env::var("CARGO_MANIFEST_DIR").ok().and_then(|d| canon(Path::new(&d)));
    let compiled_own = canon(Path::new(own_manifest_dir));
    let hint = "worktrees share one CARGO_TARGET_DIR and cargo mixed their artifacts: use a per-checkout target dir (or `cargo clean -p kob-x402 -p kob-protocol` and rebuild)";
    assert!(
        compiled_lib.is_some() && compiled_lib == running_lib,
        "the kob-x402 library was compiled from {:?}, this test runs in {:?}: {hint}",
        kob_x402::SOURCE_ROOT,
        running_lib
    );
    if runtime_own.is_some() {
        assert_eq!(compiled_own, runtime_own, "this test binary was compiled from another checkout: {hint}");
    }
}

pub const NOW_MS: u64 = 1_800_000_000_000;
pub const PAYER: u8 = common::TAKER;
pub const MERCHANT: u8 = common::MERCHANT;
pub const RH: &str = "a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1";

/// A token of the fixtures: covenant id, program and ticker.
#[derive(Clone, Copy, Debug)]
pub struct Tok {
    pub cov: [u8; 32],
    pub prog: TemplateId,
    pub ticker: &'static str,
}

/// A KRON token (46-byte state, address presence, no extension commitment).
pub const K: Tok = Tok { cov: [0x72; 32], prog: TemplateId::KronToken2433, ticker: "KRN" };
/// A KCC-20 token on the 8 / 8 reference program.
pub const A8: Tok = Tok { cov: common::TOKEN_COV, prog: TemplateId::Kcc20Ref8x8, ticker: "AAA" };
/// A KCC-20 token on KaspaCom's third-party 0.2.5 program (25.5 KB, 8 / 8 slots).
pub const KC: Tok = Tok { cov: common::TOKEN_B, prog: TemplateId::Kcc20KaspaCom025, ticker: "KCM" };
/// A KCC-20 token on the 3 / 3 reference program.
pub const B3: Tok = Tok { cov: [0x73; 32], prog: TemplateId::Kcc20Ref, ticker: "BBB" };

impl Tok {
    pub fn fam(&self) -> Family {
        self.prog.family()
    }
    /// Extension commitment of the fixture token (KRON tokens have none).
    pub fn ext(&self) -> [u8; 32] {
        common::ext_for(self.prog)
    }
    pub fn allowed(&self) -> AllowedToken {
        AllowedToken::new(self.cov, self.prog, self.ext(), Custody::Unconditional, self.ticker, 3)
    }
    /// SPK of a token UTXO in `state` on this token's program.
    pub fn spk(&self, state: &TokenState) -> ScriptPublicKey {
        state.spk_with(token_template(self.prog))
    }
}

pub fn op(u: &Utxo) -> Outpoint {
    Outpoint::new(u.transaction_id, u.index)
}

pub fn addr(key: u8) -> String {
    Address::new(Prefix::Testnet, Version::PubKey, &pk(key)).to_string()
}

pub fn put(chain: &MockChain, u: &Utxo, spk: ScriptPublicKey) {
    chain.insert_utxo(
        op(u),
        ChainUtxo {
            amount: u.amount,
            script_public_key: spk,
            block_daa_score: u.block_daa_score,
            is_coinbase: false,
            covenant_id: u.covenant_id,
        },
    );
}

/// A mock chain with the payer's KAS funding UTXO, and a policy that allowlists `toks`.
pub struct World {
    pub chain: Arc<MockChain>,
    pub clock: Arc<FixedClock>,
    pub policy: Policy,
    pub funding: KeyUtxo,
}

impl World {
    pub fn new(toks: &[Tok]) -> World {
        assert_single_checkout(env!("CARGO_MANIFEST_DIR"));
        let chain = Arc::new(MockChain::new());
        chain.advance_daa(NOW);
        let mut policy = Policy::new(Network::Testnet10);
        for t in toks {
            policy.tokens.insert(t.allowed()).unwrap();
        }
        policy.limits.min_carrier_sompi = KAS;
        // these worlds quote the 10 KAS test carrier; the payer ceiling (default 2 KAS) is covered by tests/carrier_ceiling.rs
        policy.limits.max_carrier_sompi = CARRIER;
        let funding = common::key_utxo(12, PAYER, 20 * KAS);
        put(&chain, &funding.utxo, p2pk_spk(&funding.pubkey));
        World { chain, clock: Arc::new(FixedClock::new(NOW_MS)), policy, funding }
    }

    pub fn ctx(&self) -> VerifyCtx<'_> {
        VerifyCtx { chain: &*self.chain, clock: &*self.clock, policy: &self.policy }
    }

    pub fn allowed(&self, t: Tok) -> &AllowedToken {
        self.policy.tokens.find(&t.cov).expect("allowlisted in this world")
    }

    /// A bid of `t` funded for `whole` whole tokens (on chain under the order kind of the token's family).
    pub fn put_bid(&self, t: Tok, tag: u8, covb: u8, maker: u8, whole: i64) -> OrderUtxo<BidState> {
        let bs = BidState { token_cov_id: t.cov, ..common::bid(maker, P245, t.prog) };
        let v = (bs.used(whole * WHOLE).expect("budget") + DC) as u64;
        let o = common::order(tag, v, common::cov(covb), 1_000, bs);
        put(&self.chain, &o.utxo, o.state.spk_for(t.fam()));
        o
    }

    /// An ask of 5 whole tokens of `t` with its exact custody (both on chain).
    pub fn put_ask(&self, t: Tok, tag: u8, tag_c: u8, covb: u8, maker: u8) -> (OrderUtxo<AskState>, TokenUtxo) {
        let a = AskState { token_cov_id: t.cov, ..common::ask_n(maker, P250, t.prog, 5) };
        let c = common::cov(covb);
        let order = common::order(tag, CARRIER, c, 1_000, a);
        let custody = TokenUtxo {
            utxo: common::utxo(tag_c, CARRIER, 1_000, Some(t.cov)),
            state: TokenState::custody(t.fam(), 5 * WHOLE, c, t.ext()),
        };
        put(&self.chain, &order.utxo, order.state.spk_for(t.fam()));
        put(&self.chain, &custody.utxo, t.spk(&custody.state));
        (order, custody)
    }

    /// A key-owned token UTXO of `t` (KCC-20 P2PK, KRON address presence).
    pub fn put_holding(&self, t: Tok, tag: u8, amount: i64, owner: [u8; 32]) -> TokenUtxo {
        let h = TokenUtxo {
            utxo: common::utxo(tag, CARRIER, 1_000, Some(t.cov)),
            state: TokenState::user(t.fam(), amount, owner, t.ext()),
        };
        put(&self.chain, &h.utxo, t.spk(&h.state));
        h
    }

    /// The payer's assets: the tokens, and the KAS funding UTXO when `with_kas` (a KRON payer always
    /// needs it: address presence).
    pub fn funds(&self, tokens: Vec<TokenUtxo>, with_kas: bool) -> PayerFunds {
        PayerFunds { tokens, funding: if with_kas { vec![self.funding.clone()] } else { vec![] }, change: pk(PAYER) }
    }

    fn specs(&self, pay: &[Tok]) -> Vec<PayAssetSpec<'_>> {
        pay.iter().map(|t| PayAssetSpec::Token(self.allowed(*t))).collect()
    }

    /// An offer of `amount` sompi to the merchant, payable with `pay`.
    pub fn kas_offer(&self, amount: u64, pay: &[Tok]) -> PaymentRequirements {
        swap_requirements(&SwapOfferParams {
            network: Network::Testnet10,
            amount,
            pay_to: &addr(MERCHANT),
            max_timeout_seconds: 600,
            finality: Finality::Accepted,
            gain: MerchantGain::Kas,
            pay_assets: self.specs(pay),
        })
        .unwrap()
    }

    /// An offer of `amount` units of the KCC-20 token `merchant`, payable with `pay`.
    pub fn token_offer(&self, merchant: Tok, amount: u64, pay: &[Tok]) -> PaymentRequirements {
        swap_requirements(&SwapOfferParams {
            network: Network::Testnet10,
            amount,
            pay_to: &addr(MERCHANT),
            max_timeout_seconds: 600,
            finality: Finality::Accepted,
            gain: MerchantGain::Token { token: self.allowed(merchant), carrier: CARRIER },
            pay_assets: self.specs(pay),
        })
        .unwrap()
    }

    pub fn try_pay(&self, offer: &PaymentRequirements, quote: &Quote, funds: &PayerFunds) -> Result<SwapPayment, X402Error> {
        pay_swap(&self.policy, offer, quote, RH, &keys(), funds, NOW_MS, &SwapOptions::default())
    }

    pub fn pay(&self, offer: &PaymentRequirements, quote: &Quote, funds: &PayerFunds) -> SwapPayment {
        self.try_pay(offer, quote, funds).unwrap_or_else(|e| panic!("payer SDK: {e}"))
    }

    pub fn verify(&self, offer: &PaymentRequirements, p: &PaymentPayload) -> Result<Verified, X402Error> {
        verify_payment(&self.ctx(), offer, p, RH)
    }

    /// Verifies, checks the engine, submits, mines and asserts the merchant output is in the UTXO set.
    pub fn accept(&self, offer: &PaymentRequirements, p: &PaymentPayload) -> Verified {
        let v = self.verify(offer, p).unwrap_or_else(|e| panic!("verification failed: {e}"));
        kob_protocol::verify::validate(&v.tx, &v.entries).expect("engine validation");
        self.chain.submit(&v.tx).expect("submit");
        self.chain.mine(1);
        assert!(self.chain.is_accepted(&v.txid));
        let st = self.chain.output_status(&v.merchant_output.outpoint, &v.merchant_output.script_public_key).unwrap();
        assert!(matches!(st, OutputStatus::Accepted { .. }), "merchant output must be in the UTXO set");
        v
    }
}

pub fn quote(orders: Vec<OrderRef>) -> Quote {
    Quote { lock_time: NOW, orders }
}
