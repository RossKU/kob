//! The default fee is exactly the relay floor of a rusty-kaspa v2.1.0 node, never below it.
//!
//! [`node_minimum_fee`] transcribes the node's check (it is `pub(crate)` in `kaspa-mining`, which this
//! workspace does not depend on) from the v2.1.0 sources, with rusty-kaspa's own types:
//!
//! * `mining/src/mempool/validate_and_insert_transaction.rs:33`: the non-contextual masses come from
//!   `consensus.calculate_transaction_non_contextual_masses`, i.e. (`consensus/src/consensus/mod.rs:689`,
//!   `consensus/src/consensus/services.rs:133`) `MassCalculator::new(params.mass_per_tx_byte,
//!   params.mass_per_script_pub_key_byte, params.storage_mass_parameter).calc_non_contextual_masses`;
//! * `mining/src/mempool/config.rs`: `mempool_mass_cofactors = mempool_block_mass_limits.cofactors()`,
//!   the node passing `config.block_mass_limits` (`kaspad/src/daemon.rs:651`), and
//!   `DEFAULT_MINIMUM_RELAY_TRANSACTION_FEE = 100_000` sompi per kilogram;
//! * `mining/src/mempool/check_transaction_standard.rs:67-86`: `fee_mass = max(compute_mass,
//!   normalized_transient)`, storage mass is not part of it, and the fee must be at least
//!   `minimum_required_transaction_relay_fee(fee_mass)` (`:95-110`: `mass * fee_per_kg / 1000`, the base
//!   fee when that is 0, capped at `MAX_SOMPI`).
//!
//! The comparison runs on thousands of pseudo-random transaction shapes (versions 0 and 1, P2PK and
//! P2SH inputs with large signature scripts, sig-op counts and compute budgets, covenant outputs,
//! payloads, tiny and huge output values so storage mass ranges from nothing to far above the
//! compute mass) and on every builder scenario of the golden vectors, for mainnet and testnet-10.

mod common;

use kaspa_consensus_core::config::params::{Params, MAINNET_PARAMS, TESTNET_PARAMS};
use kaspa_consensus_core::constants::MAX_SOMPI;
use kaspa_consensus_core::mass::{MassCalculator, NonContextualMasses};
use kaspa_consensus_core::subnets::SUBNETWORK_ID_NATIVE;
use kaspa_consensus_core::tx::{
    CovenantBinding, ScriptPublicKey, Transaction, TransactionId, TransactionInput, TransactionOutpoint, TransactionOutput, UtxoEntry,
};
use kaspa_consensus_core::Hash;
use kob_protocol::build::build;
use kob_protocol::tx::{
    finalize, masses, min_fee, priority_fee, relay_fee_for_mass, sign_locally, target_fee, FeeMode, FinalizeOptions,
    BLOCK_MASS_LIMITS, MIN_FEE_RATE,
};

/// `DEFAULT_MINIMUM_RELAY_TRANSACTION_FEE` (sompi per kilogram), `mining/src/mempool/config.rs:20`.
const NODE_MINIMUM_RELAY_TRANSACTION_FEE: u64 = 100_000;

/// The node's minimum fee for `tx` (see the module docs), at `per_kg` sompi per kilogram.
fn node_minimum_fee(params: &Params, tx: &Transaction, per_kg: u64) -> u64 {
    let masses: NonContextualMasses =
        MassCalculator::new(params.mass_per_tx_byte, params.mass_per_script_pub_key_byte, params.storage_mass_parameter)
            .calc_non_contextual_masses(tx);
    let cofactors = params.block_mass_limits.cofactors();
    let normalized_transient_mass = masses.normalized_transient(&cofactors);
    let fee_mass = masses.compute_mass.max(normalized_transient_mass);
    // minimum_required_transaction_relay_fee
    let mut minimum_fee = fee_mass.saturating_mul(per_kg) / 1000;
    if minimum_fee == 0 {
        minimum_fee = per_kg;
    }
    minimum_fee.min(MAX_SOMPI)
}

/// `check_transaction_standard_in_context`'s fee rule: does the node relay `tx` paying `fee`?
fn node_relays(params: &Params, tx: &Transaction, fee: u64) -> bool {
    fee >= node_minimum_fee(params, tx, NODE_MINIMUM_RELAY_TRANSACTION_FEE)
}

/// xorshift64*: deterministic shapes without a dev-dependency.
struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_f491_4f6c_dd1d)
    }
    fn below(&mut self, n: u64) -> u64 {
        self.next() % n.max(1)
    }
    /// Below `rare` with probability 1/`one_in`, else below `usual`.
    fn below_mix(&mut self, one_in: u64, rare: u64, usual: u64) -> u64 {
        let n = if self.below(one_in) == 0 { rare } else { usual };
        self.below(n)
    }
    fn bytes(&mut self, n: usize) -> Vec<u8> {
        (0..n).map(|_| self.next() as u8).collect()
    }
    /// A value spread over every magnitude, 1 sompi to about 10^13.
    fn value(&mut self) -> u64 {
        let digits = self.below(13) as u32;
        1 + self.below(10u64.pow(digits + 1))
    }
}

fn random_tx(r: &mut Rng) -> (Transaction, Vec<UtxoEntry>) {
    let version: u16 = if r.below(3) == 0 { 0 } else { 1 };
    let n_in = 1 + r.below_mix(4, 40, 4) as usize;
    let n_out = 1 + r.below_mix(4, 20, 3) as usize;
    let mut inputs = vec![];
    let mut entries = vec![];
    let mut total_in = 0u64;
    for i in 0..n_in {
        let op = TransactionOutpoint::new(TransactionId::from_bytes(r.bytes(32).try_into().unwrap()), i as u32);
        // P2PK (66-byte push), or P2SH with a redeem script of up to ~3 KB
        let sig_len = match r.below(3) {
            0 => 66,
            1 => 66 + r.below(3_000) as usize,
            _ => r.below(200) as usize,
        };
        let sig = r.bytes(sig_len);
        let input = if version == 0 {
            let sigops = if r.below(2) == 0 { 0 } else { r.below(16) as u8 };
            TransactionInput::new(op, sig, u64::MAX, sigops)
        } else {
            let seq = r.below(2);
            let budget = if r.below(2) == 0 { 0 } else { r.below_mix(5, 400, 30) as u16 };
            TransactionInput::new_with_compute_budget(op, sig, seq, budget)
        };
        inputs.push(input);
        let amount = r.value().saturating_add(1_000_000_000);
        total_in = total_in.saturating_add(amount);
        let spk_len = if r.below(2) == 0 { 34 } else { 35 };
        entries.push(UtxoEntry::new(amount, ScriptPublicKey::new(0, r.bytes(spk_len).into()), 0, false, None));
    }
    let mut outputs = vec![];
    for i in 0..n_out {
        let spk_len = match r.below(4) {
            0 => 34,
            1 => 35,
            2 => 1 + r.below(64) as usize,
            _ => 34 + r.below(400) as usize,
        };
        let value = match r.below(6) {
            0 => 1 + r.below(10_000),
            1 | 2 => r.value(),
            _ => 1_000_000_000 + r.below(100_000_000_000),
        }
        .min(total_in / (n_out as u64 + 1))
        .max(1);
        let covenant = (version == 1 && r.below(3) == 0).then(|| CovenantBinding {
            authorizing_input: (i % n_in) as u16,
            covenant_id: Hash::from_bytes(r.bytes(32).try_into().unwrap()),
        });
        outputs.push(TransactionOutput { value, script_public_key: ScriptPublicKey::new(0, r.bytes(spk_len).into()), covenant });
    }
    let payload_len = if r.below(3) == 0 { r.below(6_000) as usize } else { 0 };
    let payload = r.bytes(payload_len);
    let tx = Transaction::new(version, inputs, outputs, r.below(1_000_000), SUBNETWORK_ID_NATIVE, 0, payload);
    tx.set_storage_mass(masses(&tx, &entries).storage);
    (tx, entries)
}

#[test]
fn our_mass_parameters_are_the_nodes_on_mainnet_and_testnet_10() {
    for p in [&MAINNET_PARAMS, &TESTNET_PARAMS] {
        assert_eq!(p.block_mass_limits, BLOCK_MASS_LIMITS);
        assert_eq!((p.mass_per_tx_byte, p.mass_per_script_pub_key_byte, p.storage_mass_parameter), (1, 10, 1_000_000_000_000));
    }
    assert_eq!(NODE_MINIMUM_RELAY_TRANSACTION_FEE, MIN_FEE_RATE * 1000);
}

#[test]
fn the_default_fee_is_the_nodes_relay_floor_on_random_shapes() {
    let mut r = Rng(0x9e37_79b9_7f4a_7c15);
    let (mut storage_dominated, mut compute_dominated, mut transient_dominated) = (0, 0, 0);
    for i in 0..4_000 {
        let (tx, entries) = random_tx(&mut r);
        let m = masses(&tx, &entries);
        let ours = min_fee(&m, MIN_FEE_RATE);
        for params in [&MAINNET_PARAMS, &TESTNET_PARAMS] {
            let node = node_minimum_fee(params, &tx, NODE_MINIMUM_RELAY_TRANSACTION_FEE);
            assert_eq!(ours, node, "shape {i}: ours {ours} != node {node} ({m:?})");
            // the node relays our floor and not one sompi less
            assert!(node_relays(params, &tx, ours), "shape {i}");
            assert!(!node_relays(params, &tx, ours - 1), "shape {i}");
            // other rates: the node's formula with minimumRelayTransactionFee = rate x 1000
            let rate = 1 + r.below(10_000);
            assert_eq!(relay_fee_for_mass(m.fee_mass, rate), node_minimum_fee(params, &tx, rate * 1000), "shape {i} at rate {rate}");
        }
        // storage mass is not part of the floor; the priority fee counts it and is never below the floor
        assert_eq!(m.fee_mass, m.compute.max(m.transient_normalized));
        assert_eq!(m.transient_normalized, 2 * m.size);
        assert_eq!(m.priority_mass, m.fee_mass.max(m.storage));
        assert_eq!(priority_fee(&m, MIN_FEE_RATE), m.priority_mass * MIN_FEE_RATE);
        assert_eq!(target_fee(&m, MIN_FEE_RATE, FeeMode::Relay), ours);
        assert!(target_fee(&m, MIN_FEE_RATE, FeeMode::Priority) >= ours);
        if m.storage > m.fee_mass {
            storage_dominated += 1;
        } else if m.compute >= m.transient_normalized {
            compute_dominated += 1;
        } else {
            transient_dominated += 1;
        }
    }
    // the generator really covers every regime
    assert!(
        storage_dominated > 100 && compute_dominated > 100 && transient_dominated > 100,
        "{storage_dominated} {compute_dominated} {transient_dominated}"
    );
}

#[test]
fn every_builder_scenario_pays_exactly_the_nodes_floor() {
    let keys = common::keys();
    let (mut exact, mut all) = (0, 0);
    for (name, action) in common::scenarios().into_iter().chain(common::scenarios_kron()) {
        let built = build(&action).unwrap_or_else(|e| panic!("{name}: {e}"));
        let sigs = sign_locally(&built, &keys).unwrap();
        // the signed transaction has the measured size of the built one (placeholder signatures of the
        // same length): the builder's fee is the node's floor of what is broadcast
        let signed = finalize(&built, &sigs, FinalizeOptions { tighten_budgets: false }).unwrap();
        let (tx, entries) = signed.tx.to_tx().unwrap();
        let fee = entries.iter().map(|e| e.amount).sum::<u64>() - tx.outputs.iter().map(|o| o.value).sum::<u64>();
        let node = node_minimum_fee(&TESTNET_PARAMS, &tx, NODE_MINIMUM_RELAY_TRANSACTION_FEE);
        assert!(fee >= node, "{name}: fee {fee} below the node floor {node}");
        assert_eq!(signed.fee.min_fee, node, "{name}");
        if built.fee.change_output.is_some() {
            assert_eq!(fee, node, "{name}: a transaction with change pays exactly the floor");
            exact += 1;
        }
        // tightening the budgets only lowers the floor
        let tight = finalize(&built, &sigs, FinalizeOptions { tighten_budgets: true }).unwrap();
        let (ttx, _) = tight.tx.to_tx().unwrap();
        assert!(node_relays(&TESTNET_PARAMS, &ttx, fee), "{name}");
        all += 1;
    }
    assert!(exact > all / 3, "{exact} of {all} scenarios have change");
}

/// The rc.1 vector of elldeeone's binding (`vectors/kaspa-x402-rc1`, standard-native) pays 200000 sompi;
/// the node's floor for it is 203600 (normalized transient 2036 grams): that vector is below the relay
/// floor of a v2.1.0 node, which the live TN10 probe confirms (see `crates/kob-x402/tests/interop_vectors.rs`).
#[test]
fn relay_fee_arithmetic_matches_the_node() {
    assert_eq!(relay_fee_for_mass(2_036, MIN_FEE_RATE), 203_600);
    assert_eq!(relay_fee_for_mass(0, MIN_FEE_RATE), NODE_MINIMUM_RELAY_TRANSACTION_FEE);
    // the node saturates the product before dividing (so its MAX_SOMPI cap never binds)
    assert_eq!(relay_fee_for_mass(u64::MAX, MIN_FEE_RATE), u64::MAX / 1000);
}
