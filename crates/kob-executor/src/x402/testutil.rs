//! Test support for the facilitator: a deterministic fixture over [`MockChain`], a KAS payment
//! builder (consensus-valid version-0 P2PK spends the mock chain validates with the script engine) and
//! a verifier that runs the shared verification checks of `kob_x402::common` (envelope, bounded parse,
//! trusted UTXO resolution, economics and scripts) without the profile's authorization step, so tests
//! can control the authorization expiry and the swap order inputs.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use kaspa_addresses::{Address, Prefix, Version};
use kaspa_consensus_core::subnets::SUBNETWORK_ID_NATIVE;
use kaspa_consensus_core::tx::{ScriptPublicKey, Transaction, TransactionInput, TransactionOutpoint, TransactionOutput, UtxoEntry};
use kaspa_consensus_core::Hash;
use kob_x402::chain::{ChainView, FixedClock, Outpoint};
use kob_x402::common;
use kob_x402::error::{Diag, Result, X402Error};
use kob_x402::policy::Policy;
use kob_x402::safe_tx::{spk_to_hex, SafeTx};
use kob_x402::testkit::{p2pk_spk, pubkey, secret, MockChain};
use kob_x402::verify::{PaymentKind, Verified, VerifyCtx, WatchedOutput};
use kob_x402::wire::{
    hex, Authorization, ExactPayload, FacilitatorRequest, Finality, Network, PaymentPayload, PaymentRequirements, Profile, ASSET_KAS,
    AUTH_VERSION_SIGNED, BINDING_EXACT, PAYLOAD_EXACT_TX, SCHEME_EXACT, TX_ENCODING, X402_VERSION,
};
use serde_json::{json, Map};

use super::facilitator::{Facilitator, FacilitatorConfig, PaymentVerifier};
use super::ledger::Ledger;

/// Fixture key of the payer.
pub const PAYER_KEY: u8 = 1;
/// Fixture key of the merchant.
pub const MERCHANT_KEY: u8 = 2;
/// The fixture clock's start (unix ms).
pub const START_MS: u64 = 1_700_000_000_000;

/// True when `again` is the repeat answer of `first`: the same settlement, marked `extensions.kob.replayed = true`.
pub fn is_repeat_of(again: &kob_x402::wire::SettlementResponse, first: &kob_x402::wire::SettlementResponse) -> bool {
    let mut stripped = again.clone();
    let marked = stripped
        .extensions
        .as_mut()
        .and_then(|x| x.get_mut("kob"))
        .and_then(|k| k.as_object_mut())
        .and_then(|k| k.remove("replayed"))
        .is_some_and(|v| v == json!(true));
    let unmarked_first = first.extensions.as_ref().and_then(|x| x.pointer("/kob/replayed")).is_none();
    marked && unmarked_first && &stripped == first
}

/// Address of a fixture key on TN10.
pub fn address_of_key(n: u8) -> String {
    Address::new(Prefix::Testnet, Version::PubKey, &pubkey(n)).to_string()
}

/// A standard-native offer for `amount` sompi to the merchant fixture key.
pub fn kas_requirements(amount: u64, finality: Finality, max_timeout_seconds: u64) -> PaymentRequirements {
    let mut extra = Map::new();
    extra.insert("binding".into(), json!(BINDING_EXACT));
    extra.insert("profile".into(), json!(Profile::StandardNative.as_str()));
    extra.insert("finality".into(), json!(finality.as_str()));
    extra.insert("transactionEncoding".into(), json!(TX_ENCODING));
    extra.insert("payToScriptPublicKey".into(), json!(spk_to_hex(&p2pk_spk(&pubkey(MERCHANT_KEY)))));
    PaymentRequirements {
        scheme: SCHEME_EXACT.into(),
        network: Network::Testnet10.as_str().into(),
        amount: amount.to_string(),
        asset: ASSET_KAS.into(),
        pay_to: address_of_key(MERCHANT_KEY),
        max_timeout_seconds,
        extra,
        other: Map::new(),
    }
}

/// Builds and signs a version-0 P2PK payment of `amount` sompi to `pay_to` funded by `inputs` (payer
/// key `payer_key`), with change back to the payer and the relay-floor fee.
pub fn build_kas_payment(
    chain: &MockChain,
    payer_key: u8,
    inputs: &[Outpoint],
    pay_to: &ScriptPublicKey,
    amount: u64,
) -> (Transaction, Vec<UtxoEntry>) {
    let entries: Vec<UtxoEntry> = inputs.iter().map(|o| chain.utxo(o).expect("funding utxo").to_entry()).collect();
    let total: u64 = entries.iter().map(|e| e.amount).sum();
    let payer = p2pk_spk(&pubkey(payer_key));
    let mk = |fee: u64, sigs: Option<&[Vec<u8>]>| {
        let ins = inputs
            .iter()
            .enumerate()
            .map(|(i, o)| {
                let script = match sigs {
                    Some(s) => s[i].clone(),
                    None => vec![0x41; 66],
                };
                TransactionInput::new(TransactionOutpoint::new(Hash::from_bytes(o.txid), o.index), script, 0, 1)
            })
            .collect();
        let change = total - amount - fee;
        let mut outs = vec![TransactionOutput::new(amount, pay_to.clone())];
        if change > 0 {
            outs.push(TransactionOutput::new(change, payer.clone()));
        }
        Transaction::new(0, ins, outs, 0, SUBNETWORK_ID_NATIVE, 0, vec![])
    };
    let mut fee = 20_000u64;
    for _ in 0..6 {
        let tx = mk(fee, None);
        tx.set_storage_mass(kob_protocol::tx::masses(&tx, &entries).storage);
        let need = kob_protocol::tx::min_fee(&kob_protocol::tx::masses(&tx, &entries), kob_protocol::tx::MIN_FEE_RATE);
        if fee >= need {
            break;
        }
        fee = need + 1_000;
    }
    let unsigned = mk(fee, None);
    let sigs: Vec<Vec<u8>> = (0..inputs.len())
        .map(|i| {
            let digest = kob_protocol::tx::sighash(&unsigned, &entries, i);
            let sig = kob_protocol::tx::sign_digest(&secret(payer_key), &digest).expect("sign");
            let mut s = vec![0x41];
            s.extend_from_slice(&sig);
            s
        })
        .collect();
    let mut tx = mk(fee, Some(&sigs));
    tx.finalize();
    tx.set_storage_mass(kob_protocol::tx::masses(&tx, &entries).storage);
    (tx, entries)
}

/// A facilitator request for `tx` under `reqs`, with `request_hash` and a `payment-identifier`.
pub fn request_for(
    tx: &Transaction,
    entries: &[UtxoEntry],
    reqs: &PaymentRequirements,
    request_hash: [u8; 32],
    payment_id: &str,
) -> FacilitatorRequest {
    let payload = PaymentPayload {
        x402_version: X402_VERSION,
        accepted: reqs.clone(),
        payload: ExactPayload {
            kind: PAYLOAD_EXACT_TX.into(),
            profile: Profile::StandardNative.as_str().into(),
            payer_address: Some(address_of_key(PAYER_KEY)),
            transaction: SafeTx::from_consensus(tx, entries).to_text(),
            transaction_encoding: TX_ENCODING.into(),
            payment_output_index: 0,
            request_hash: hex(&request_hash),
            challenge_id: None,
            authorization: Authorization {
                version: AUTH_VERSION_SIGNED.into(),
                input_index: Some(0),
                expires_at: common::iso_from_ms(START_MS + 60_000),
                digest: "00".repeat(32),
                signature: Some("00".repeat(64)),
            },
            route: None,
        },
        resource: None,
        extensions: Some(json!({ "payment-identifier": { "info": { "required": true, "id": payment_id } } })),
    };
    FacilitatorRequest {
        x402_version: X402_VERSION,
        payment_payload: payload,
        payment_requirements: reqs.clone(),
        request_hash: Some(hex(&request_hash)),
        resource: None,
    }
}

/// The verifier of the fixture: the shared checks of `kob_x402::common` plus configurable knobs.
pub struct TestVerifier {
    /// Authorization expiry (unix ms) reported in `Verified`.
    pub expires_at_ms: AtomicU64,
    /// Input outpoints reported as swap order inputs.
    pub orders: Mutex<Vec<Outpoint>>,
    /// Advances this clock by the given ms while verifying (an expiry that lapses during awaited work).
    pub advance_clock: Mutex<Option<(Arc<FixedClock>, u64)>>,
    /// Number of `verify` calls.
    pub calls: AtomicU64,
    /// Runs inside `verify` (something that happens while a settle is past its first checks).
    #[allow(clippy::type_complexity)]
    pub during_verify: Mutex<Option<Box<dyn Fn() + Send + Sync>>>,
}

impl TestVerifier {
    pub fn new() -> TestVerifier {
        TestVerifier {
            expires_at_ms: AtomicU64::new(START_MS + 60_000),
            orders: Mutex::new(vec![]),
            advance_clock: Mutex::new(None),
            calls: AtomicU64::new(0),
            during_verify: Mutex::new(None),
        }
    }
}

impl Default for TestVerifier {
    fn default() -> Self {
        Self::new()
    }
}

impl PaymentVerifier for TestVerifier {
    fn verify(
        &self,
        ctx: &VerifyCtx,
        offered: &PaymentRequirements,
        payload: &PaymentPayload,
        request_hash: &str,
    ) -> Result<Verified> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        let env = common::check_envelope(ctx, offered, payload, request_hash)?;
        let parsed = common::parse_tx(ctx, payload)?;
        let entries = common::resolve_entries(ctx, &parsed)?;
        let fee = common::check_economics_and_scripts(ctx, &parsed.tx, &entries)?;
        let pid = common::payment_identifier(ctx, payload)?;
        let outs: Vec<usize> = parsed
            .tx
            .outputs
            .iter()
            .enumerate()
            .filter(|(_, o)| o.script_public_key == env.pay_to_spk && o.value == env.amount)
            .map(|(i, _)| i)
            .collect();
        if outs.len() != 1 {
            return Err(X402Error::payload(Diag::InvalidKaspaExactPaymentOutput, "exactly one merchant output is required"));
        }
        if let Some(f) = self.during_verify.lock().unwrap().as_ref() {
            f();
        }
        if let Some((clock, d)) = self.advance_clock.lock().unwrap().as_ref() {
            clock.set(kob_x402::chain::Clock::now_ms(&**clock) + d);
        }
        let txid = parsed.tx.id().as_bytes();
        let consumed: Vec<Outpoint> = parsed.tx.inputs.iter().map(common::outpoint_of).collect();
        let mut ext = Map::new();
        ext.insert("binding".into(), json!(BINDING_EXACT));
        ext.insert("profile".into(), json!(Profile::StandardNative.as_str()));
        ext.insert("paymentOutputIndex".into(), json!(outs[0]));
        ext.insert("finality".into(), json!(env.finality.as_str()));
        ext.insert("transactionEncoding".into(), json!(TX_ENCODING));
        Ok(Verified {
            kind: PaymentKind::Native,
            profile: Profile::StandardNative,
            txid,
            merchant_output: WatchedOutput {
                outpoint: Outpoint::new(txid, outs[0] as u32),
                script_public_key: env.pay_to_spk.clone(),
                amount: env.amount,
            },
            payer_address: common::address_of(&entries[0].script_public_key, env.network),
            amount: env.amount,
            payment_output_index: outs[0] as u32,
            consumed,
            order_inputs: self.orders.lock().unwrap().clone(),
            fee,
            finality: common::effective_finality(env.finality, ctx.policy),
            custody: None,
            authorization_expires_at_ms: self.expires_at_ms.load(Ordering::SeqCst),
            request_hash: env.request_hash,
            requirements_hash: env.requirements_hash,
            payment_identifier: pid,
            response_extension: ext,
            tx: parsed.tx,
            entries,
        })
    }
}

/// A facilitator over a mock chain with fast polling.
pub struct Fixture {
    pub chain: Arc<MockChain>,
    pub clock: Arc<FixedClock>,
    pub ledger: Arc<Ledger>,
    pub verifier: Arc<TestVerifier>,
    pub fac: Arc<Facilitator>,
}

impl Fixture {
    pub fn new() -> Fixture {
        Self::with_ledger(Arc::new(Ledger::in_memory()), Arc::new(MockChain::new()), Arc::new(FixedClock::new(START_MS)))
    }

    /// A facilitator over an existing chain and ledger (restart simulations).
    pub fn with_ledger(ledger: Arc<Ledger>, chain: Arc<MockChain>, clock: Arc<FixedClock>) -> Fixture {
        let view: Arc<dyn ChainView> = chain.clone();
        Self::with_view(ledger, chain, view, clock)
    }

    /// Like `with_ledger`, with the facilitator reading through `view` (a wrapper around `chain`).
    pub fn with_view(ledger: Arc<Ledger>, chain: Arc<MockChain>, view: Arc<dyn ChainView>, clock: Arc<FixedClock>) -> Fixture {
        let verifier = Arc::new(TestVerifier::new());
        let mut policy = Policy::new(Network::Testnet10);
        policy.confirmations_daa = 100;
        let fac = Facilitator::new(
            policy,
            view,
            clock.clone(),
            ledger.clone(),
            FacilitatorConfig {
                settle_wait: Duration::from_secs(5),
                poll_interval: Duration::from_millis(5),
                reorg_watch_daa: 10_000,
                kill_switch_file: None,
                ..Default::default()
            },
        )
        .with_verifier(verifier.clone());
        Fixture { chain, clock, ledger, verifier, fac: Arc::new(fac) }
    }

    /// [`Fixture::new`] with invoices served (volatile store, one day of lifetime).
    pub fn with_invoices() -> Fixture {
        let mut fx = Self::new();
        let fac =
            Facilitator::new(fx.fac.policy.clone(), fx.chain.clone(), fx.clock.clone(), fx.ledger.clone(), fx.fac.config.clone())
                .with_verifier(fx.verifier.clone())
                .with_invoices(super::facilitator::InvoiceRuntime {
                    store: Arc::new(super::facilitator::InvoiceStore::in_memory()),
                    max_lifetime_ms: 86_400_000,
                    public_url: None,
                    max_open_per_merchant: 100,
                    max_extra_payments: 16,
                });
        fx.fac = Arc::new(fac);
        fx
    }

    /// A payer UTXO of `amount` sompi.
    pub fn fund(&self, amount: u64) -> Outpoint {
        self.chain.add_p2pk(&pubkey(PAYER_KEY), amount)
    }

    /// Builds a payment of `amount` funded by `inputs` and the facilitator request for it.
    pub fn payment(&self, inputs: &[Outpoint], amount: u64, payment_id: &str, request_hash: u8) -> (FacilitatorRequest, Transaction) {
        self.payment_with(inputs, amount, payment_id, request_hash, Finality::Accepted)
    }

    pub fn payment_with(
        &self,
        inputs: &[Outpoint],
        amount: u64,
        payment_id: &str,
        request_hash: u8,
        finality: Finality,
    ) -> (FacilitatorRequest, Transaction) {
        let reqs = kas_requirements(amount, finality, 60);
        let (tx, entries) = build_kas_payment(&self.chain, PAYER_KEY, inputs, &p2pk_spk(&pubkey(MERCHANT_KEY)), amount);
        (request_for(&tx, &entries, &reqs, [request_hash; 32], payment_id), tx)
    }
}

impl Default for Fixture {
    fn default() -> Self {
        Self::new()
    }
}
