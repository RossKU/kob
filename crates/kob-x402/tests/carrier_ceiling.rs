//! Regression: a kcc20 offer's `extra.token.carrier` (the KAS value of the merchant token output) is
//! funded by the PAYER. It has a floor (`min_carrier_sompi`) AND a ceiling (`max_carrier_sompi`): the payer's builder
//! and its preflight (the verifier's logic under the payer's policy) both refuse a merchant that quotes one base unit
//! with a 500 KAS carrier, and the payer's own token-change carrier is capped independently.
use std::collections::BTreeMap;

use kaspa_addresses::{Address, Version};
use kob_protocol::artifacts::{template, TemplateId};
use kob_protocol::state::Kcc20State;
use kob_protocol::tx::{KeyUtxo, TokenUtxo, Utxo};
use kob_x402::canonical::sha256;
use kob_x402::chain::{FixedClock, Outpoint};
use kob_x402::client::token::{kcc20_requirements, pay_kcc20_with, Kcc20Options};
use kob_x402::error::Diag;
use kob_x402::policy::{AllowedToken, Custody, Policy};
use kob_x402::safe_tx::SafeTx;
use kob_x402::testkit::{pubkey, secret, MockChain};
use kob_x402::verify::{verify_payment, VerifyCtx};
use kob_x402::wire::{hex, Finality, Network};

const KAS: u64 = 100_000_000;
const COV: [u8; 32] = [0x70; 32];
const EXT: [u8; 32] = [0xee; 32];
const NOW_MS: u64 = 1_800_000_000_000;
const NET: Network = Network::Testnet10;

fn utxo_of(chain: &MockChain, op: Outpoint) -> Utxo {
    let u = chain.utxo(&op).unwrap();
    Utxo { transaction_id: op.txid, index: op.index, amount: u.amount, block_daa_score: u.block_daa_score, covenant_id: u.covenant_id }
}

struct Rig {
    tok: AllowedToken,
    policy: Policy,
    chain: MockChain,
    keys: BTreeMap<[u8; 32], [u8; 32]>,
    merchant: String,
    token: TokenUtxo,
    funding: KeyUtxo,
    rh: String,
}

fn rig() -> Rig {
    let keys: BTreeMap<[u8; 32], [u8; 32]> = (1..=10u8).map(|n| (pubkey(n), secret(n))).collect();
    let tok = AllowedToken::new(COV, TemplateId::Kcc20Ref8x8, EXT, Custody::Unconditional, "USDx", 6);
    let mut policy = Policy::new(NET);
    policy.tokens.insert(tok.clone()).unwrap();
    let chain = MockChain::new();
    let merchant = Address::new(NET.prefix(), Version::PubKey, &pubkey(8)).to_string();
    // payer: 1000 units of the token in a 10 KAS carrier UTXO, plus 2000 KAS of funding
    let st = Kcc20State::p2pk(1000, pubkey(1), EXT);
    let op = chain.add_utxo(10 * KAS, st.spk_with(template(TemplateId::Kcc20Ref8x8)), Some(COV));
    let token = TokenUtxo { utxo: utxo_of(&chain, op), state: st.into() };
    let fop = chain.add_p2pk(&pubkey(1), 2000 * KAS);
    let funding = KeyUtxo { utxo: utxo_of(&chain, fop), pubkey: pubkey(1) };
    Rig { tok, policy, chain, keys, merchant, token, funding, rh: hex(&sha256(b"GET /x")) }
}

fn opts() -> Kcc20Options {
    Kcc20Options { payment_id: Some("pay-0123456789abcdef".into()), ..Kcc20Options::default() }
}

#[test]
fn payer_refuses_a_merchant_quoted_500_kas_carrier() {
    let r = rig();
    let offer = kcc20_requirements(NET, &r.tok, 1, &r.merchant, 500 * KAS, 120, Finality::Accepted).unwrap();
    let e = pay_kcc20_with(&offer, &r.rh, &r.keys, vec![r.token.clone()], vec![r.funding.clone()], NOW_MS, &opts())
        .expect_err("a 500 KAS carrier must be refused by the payer builder");
    assert_eq!(e.diag, Diag::CarrierMismatch, "{e}");
}

#[test]
fn preflight_and_verifier_refuse_a_payment_with_an_oversized_carrier_even_if_the_builder_was_loosened() {
    let r = rig();
    let offer = kcc20_requirements(NET, &r.tok, 1, &r.merchant, 500 * KAS, 120, Finality::Accepted).unwrap();
    // a payer that raises its own builder bound (or a hostile payer SDK) still meets the policy ceiling at preflight
    let loose = Kcc20Options { max_carrier_sompi: 1_000 * KAS, ..opts() };
    let p = pay_kcc20_with(&offer, &r.rh, &r.keys, vec![r.token.clone()], vec![r.funding.clone()], NOW_MS, &loose)
        .expect("built with a loosened bound");
    let clock = FixedClock::new(NOW_MS);
    let e = verify_payment(&VerifyCtx { chain: &r.chain, clock: &clock, policy: &r.policy }, &offer, &p, &r.rh)
        .expect_err("preflight must refuse");
    assert_eq!(e.diag, Diag::CarrierMismatch, "{e}");
    // an explicit policy that accepts big carriers is the payer's own decision
    let mut wide = r.policy.clone();
    wide.limits.max_carrier_sompi = 1_000 * KAS;
    verify_payment(&VerifyCtx { chain: &r.chain, clock: &clock, policy: &wide }, &offer, &p, &r.rh)
        .expect("accepted under an explicit wide ceiling");
}

#[test]
fn a_carrier_within_the_ceiling_still_pays_and_the_change_carrier_is_capped() {
    let r = rig();
    let offer = kcc20_requirements(NET, &r.tok, 1, &r.merchant, 2 * KAS, 120, Finality::Accepted).unwrap();
    let p = pay_kcc20_with(&offer, &r.rh, &r.keys, vec![r.token.clone()], vec![r.funding.clone()], NOW_MS, &opts())
        .expect("2 KAS is within the default ceiling");
    let clock = FixedClock::new(NOW_MS);
    verify_payment(&VerifyCtx { chain: &r.chain, clock: &clock, policy: &r.policy }, &offer, &p, &r.rh).expect("preflight ok");
    let tx = SafeTx::parse(&p.payload.transaction, 1 << 22).unwrap().to_consensus().unwrap().tx;
    assert_eq!(tx.outputs[0].value, 2 * KAS);
    // the payer's own token change carrier cannot be inflated either
    let big_change = Kcc20Options { token_change_carrier: Some(400 * KAS), ..opts() };
    let e = pay_kcc20_with(&offer, &r.rh, &r.keys, vec![r.token.clone()], vec![r.funding.clone()], NOW_MS, &big_change)
        .expect_err("change carrier capped");
    assert_eq!(e.diag, Diag::CarrierMismatch, "{e}");
}
