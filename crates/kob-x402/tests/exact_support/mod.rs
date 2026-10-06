//! Shared fixtures for the exact / standard-native integration tests (not a test target itself).
#![allow(dead_code)]

use kaspa_addresses::{Address, Version};
use kaspa_consensus_core::tx::{ScriptPublicKey, Transaction, UtxoEntry};
use kob_protocol::tx::{KeyUtxo, Utxo};
use kob_x402::canonical::{http_request_hash, requirements_hash};
use kob_x402::chain::{FixedClock, Outpoint};
use kob_x402::client::native::{native_requirements, pay_native, PayOptions};
use kob_x402::common::{iso_from_ms, requirements_hash_hex, signed_auth_digest, SignedAuth};
use kob_x402::error::{Diag, Reason, X402Error};
use kob_x402::policy::Policy;
use kob_x402::safe_tx::{spk_to_hex, SafeTx};
use kob_x402::testkit::{p2pk_spk, pubkey, secret, MockChain};
use kob_x402::verify::{verify_payment, Verified, VerifyCtx};
use kob_x402::wire::{hex, parse_hash32, Finality, Network, PaymentPayload, PaymentRequirements, Profile};

pub const PAYER: u8 = 7;
pub const MERCHANT: u8 = 8;
pub const ATTACKER: u8 = 9;
pub const NOW_MS: u64 = 1_800_000_000_000;
pub const AMOUNT: u64 = 20_000_000;
pub const TIMEOUT: u64 = 60;

pub fn addr(n: u8) -> String {
    Address::new(Network::Testnet10.prefix(), Version::PubKey, &pubkey(n)).to_string()
}

pub struct Env {
    pub chain: MockChain,
    pub clock: FixedClock,
    pub policy: Policy,
}

impl Env {
    pub fn new() -> Env {
        Env { chain: MockChain::new(), clock: FixedClock::new(NOW_MS), policy: Policy::new(Network::Testnet10) }
    }
    pub fn ctx(&self) -> VerifyCtx<'_> {
        VerifyCtx { chain: &self.chain, clock: &self.clock, policy: &self.policy }
    }
}

impl Default for Env {
    fn default() -> Self {
        Self::new()
    }
}

/// Funds `key` with `amount` and returns the wallet view of the new coin.
pub fn fund(chain: &MockChain, key: u8, amount: u64) -> KeyUtxo {
    let op = chain.add_p2pk(&pubkey(key), amount);
    let u = chain.utxo(&op).unwrap();
    KeyUtxo {
        utxo: Utxo { transaction_id: op.txid, index: op.index, amount, block_daa_score: u.block_daa_score, covenant_id: None },
        pubkey: pubkey(key),
    }
}

pub fn offer(amount: u64) -> PaymentRequirements {
    native_requirements(Network::Testnet10, amount, &addr(MERCHANT), TIMEOUT, Finality::Accepted).unwrap()
}

pub fn request_hash_for(offer: &PaymentRequirements) -> String {
    let rh = requirements_hash_hex(offer).unwrap();
    hex(&http_request_hash("GET", "https://api.example.test/file", None, &rh).unwrap())
}

/// A funded payer, an offer and a valid signed payment.
pub struct Fixture {
    pub env: Env,
    pub offer: PaymentRequirements,
    pub request_hash: String,
    pub coins: Vec<KeyUtxo>,
    pub payload: PaymentPayload,
}

pub fn fixture_with(funds: &[u64], fee_rate: u64) -> Fixture {
    let env = Env::new();
    let coins: Vec<KeyUtxo> = funds.iter().map(|a| fund(&env.chain, PAYER, *a)).collect();
    let offer = offer(AMOUNT);
    let request_hash = request_hash_for(&offer);
    let mut opts = PayOptions::new(u64::MAX);
    opts.fee_rate = fee_rate;
    let payload = pay_native(&offer, &request_hash, &secret(PAYER), &coins, NOW_MS, &opts).unwrap();
    Fixture { env, offer, request_hash, coins, payload }
}

pub fn fixture() -> Fixture {
    fixture_with(&[500_000_000], 100)
}

impl Fixture {
    pub fn verify(&self) -> Result<Verified, X402Error> {
        self.verify_payload(&self.payload)
    }
    pub fn verify_payload(&self, p: &PaymentPayload) -> Result<Verified, X402Error> {
        verify_payment(&self.env.ctx(), &self.offer, p, &self.request_hash)
    }
    pub fn tx(&self) -> (Transaction, Vec<UtxoEntry>) {
        decode(&self.payload)
    }
}

/// Consensus transaction and entries (rebuilt from the payload's own hints) of a payload.
pub fn decode(p: &PaymentPayload) -> (Transaction, Vec<UtxoEntry>) {
    let parsed = SafeTx::parse(&p.payload.transaction, 1 << 20).unwrap().to_consensus().unwrap();
    let entries = parsed
        .hints
        .iter()
        .map(|h| {
            let h = h.as_ref().unwrap();
            UtxoEntry::new(h.amount, h.script_public_key.clone(), 0, false, h.covenant_id.map(kaspa_consensus_core::Hash::from_bytes))
        })
        .collect();
    (parsed.tx, entries)
}

/// How to repackage a mutated transaction.
#[derive(Clone)]
pub struct Repack {
    /// Recompute the storage mass commitment.
    pub fix_mass: bool,
    /// Re-sign every input (payer key) after the mutation.
    pub resign: bool,
    /// Key that signs the authorization digest.
    pub auth_key: u8,
    pub auth_input: u32,
    /// Authorization expiry (unix ms).
    pub expires_ms: u64,
    pub payment_output_index: u32,
    /// Sign a fresh authorization for the new transaction (otherwise the old one is kept).
    pub reauthorize: bool,
}

impl Default for Repack {
    fn default() -> Self {
        Repack {
            fix_mass: true,
            resign: true,
            auth_key: PAYER,
            auth_input: 0,
            expires_ms: NOW_MS + TIMEOUT * 1000,
            payment_output_index: 0,
            reauthorize: true,
        }
    }
}

/// Signs every input of `tx` with `key` (P2PK, SIGHASH_ALL).
pub fn resign(tx: &mut Transaction, entries: &[UtxoEntry], key: u8) {
    for i in 0..tx.inputs.len() {
        let d = kob_protocol::tx::sighash(tx, entries, i);
        let sig = kob_protocol::tx::sign_digest(&secret(key), &d).unwrap();
        tx.inputs[i].signature_script = kob_protocol::script::push_data(&sig);
    }
}

/// Signs the authorization for `tx` into `payload` (digest and signature).
pub fn authorize(payload: &mut PaymentPayload, offer: &PaymentRequirements, request_hash: &str, tx: &Transaction, r: &Repack) {
    let a = &mut payload.payload.authorization;
    a.input_index = Some(r.auth_input);
    a.expires_at = iso_from_ms(r.expires_ms);
    let txid = tx.id().as_bytes();
    let rh = parse_hash32(request_hash).unwrap();
    let reqh = requirements_hash(offer).unwrap();
    let pay_addr = Address::try_from(offer.pay_to.as_str()).unwrap();
    let pay_spk = kaspa_txscript::pay_to_address_script(&pay_addr);
    let digest = signed_auth_digest(&SignedAuth {
        network: Network::Testnet10,
        profile: Profile::StandardNative,
        transaction_id: &txid,
        payment_output_index: r.payment_output_index,
        amount: &offer.amount,
        pay_to: &offer.pay_to,
        pay_to_spk_hex: &spk_to_hex(&pay_spk),
        requirements_hash: &reqh,
        request_hash: &rh,
        challenge_id: None,
        input_index: r.auth_input,
        expires_at: &a.expires_at,
    })
    .unwrap();
    a.digest = hex(&digest);
    let mut sig = kob_protocol::tx::sign_digest(&secret(r.auth_key), &digest).unwrap();
    sig.truncate(64);
    a.signature = Some(hex(&sig));
    payload.payload.payment_output_index = r.payment_output_index;
}

/// Applies `f` to the payment's transaction and repackages it as a new payload.
pub fn mutate(
    fx: &Fixture,
    r: &Repack,
    f: impl FnOnce(&mut Transaction, &mut Vec<UtxoEntry>),
) -> (PaymentPayload, Transaction, Vec<UtxoEntry>) {
    let (mut tx, mut entries) = fx.tx();
    f(&mut tx, &mut entries);
    tx.finalize();
    if r.fix_mass {
        tx.set_storage_mass(kob_protocol::tx::masses(&tx, &entries).storage);
    }
    if r.resign {
        resign(&mut tx, &entries, PAYER);
    }
    let mut p = fx.payload.clone();
    p.payload.transaction = SafeTx::from_consensus(&tx, &entries).to_text();
    if r.reauthorize {
        authorize(&mut p, &fx.offer, &fx.request_hash, &tx, r);
    }
    (p, tx, entries)
}

pub fn outpoint_of_coin(k: &KeyUtxo) -> Outpoint {
    Outpoint::new(k.utxo.transaction_id, k.utxo.index)
}

pub fn expect_err(r: Result<Verified, X402Error>, reason: Reason, diag: Diag) {
    match r {
        Ok(_) => panic!("expected {reason:?}/{diag:?}, but the payment verified"),
        Err(e) => assert_eq!((e.reason, e.diag), (reason, diag), "got: {e}"),
    }
}

pub fn spk_of(key: u8) -> ScriptPublicKey {
    p2pk_spk(&pubkey(key))
}
