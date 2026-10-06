//! KOB protocol v2 on the real KaspaCom KCC20 program (kcc20-tx-builder 0.2.5, Apache-2.0, vendored under
//! `contracts/third-party/kaspacom-kcc20/`, embedded in `kob-protocol` as `TemplateId::Kcc20KaspaCom025`:
//! 112-byte KCC-20 state, 8 token inputs / 8 token outputs, 25.5 KB program that every token input reveals).
//!
//! The v2 orders (KobAsk / KobBid) are driven through the `kob-protocol` builders exactly as a client does
//! (build -> sign with fixture keys -> finalize with budgets tightened to the engine-measured minimum ->
//! `verify::validate_signed`: scripts under the enforced compute budgets, covenant context, storage-mass
//! commitment, relay fee floor, sig-op cap, block mass limits) and every transaction is checked with its
//! exact budgets (budget - 1 rejects on every metered input).
//!
//! The committed compute-budget table (`data/compute_budgets.json`) has no rows for this program yet, so the
//! budgets are measured here per transaction: pass 1 builds with a generous budget, the engine measures every
//! input, pass 2 rebuilds with the measured per-role budgets (the way the table generator works).
//!
//! Scenarios: ask create + partial fill, sweeps of 1..=8 asks (the largest sweep the token slots and block
//! mass allow, measured), sell into two bids, cancel (ask, bid), refund after expiry (ask, bid).
//! Run: cargo test --release -p kob-tests --test kob_kaspacom_v2_tests -- --nocapture --test-threads=1

use std::collections::BTreeMap;

use kob_protocol::artifacts::{token_template, TemplateId};
use kob_protocol::build::{build_with, Action, Batch, CancelOrder, CreateOrder, Leg, RefundOrder};
use kob_protocol::defaults::tip_for_fee;
use kob_protocol::state::*;
use kob_protocol::tx::*;
use kob_protocol::verify::{execute, validate_signed, Validation};
use kob_protocol::Error;

const TP: TemplateId = TemplateId::Kcc20KaspaCom025;
const KAS: u64 = 100_000_000;
const CARRIER: u64 = 10 * KAS;
const DC: i64 = 10 * KAS as i64;
/// Base units per whole token (a 3-decimal token): prices and tips are sompi per whole token.
const SCALE: i64 = 1_000;
/// One whole token in base units.
const WHOLE: i64 = SCALE;
/// Default minimum fill: one whole token.
const MIN_FILL: i64 = WHOLE;
const TIP: i64 = 100_000;
const EXPIRY: i64 = 400_000_000;
const NOW: u64 = 1_000_000;
const TOKEN_COV: [u8; 32] = [0x70; 32];
const EXT: [u8; 32] = [0xee; 32];
const P250: i64 = 250_000_000;
const P245: i64 = 245_000_000;
const MAKER_A: u8 = 1;
const MAKER_B: u8 = 2;
const MAKER_C: u8 = 3;
const TAKER: u8 = 4;
/// Budget of the first pass (units of 10,000 script units): above what any KaspaCom input needs here (<= 100).
const GENEROUS: u16 = 100;

// ---------------------------------------------------------------- fixtures

fn sk(n: u8) -> [u8; 32] {
    [n; 32]
}
fn pk(n: u8) -> [u8; 32] {
    pubkey_of(&sk(n)).unwrap()
}
fn keys() -> BTreeMap<[u8; 32], [u8; 32]> {
    (1..=40u8).map(|n| (pk(n), sk(n))).collect()
}
fn utxo(tag: u8, amount: u64, daa: u64, cov: Option<[u8; 32]>) -> Utxo {
    Utxo { transaction_id: [tag; 32], index: tag as u32, amount, block_daa_score: daa, covenant_id: cov }
}
fn key_utxo(tag: u8, key: u8, amount: u64) -> KeyUtxo {
    KeyUtxo { utxo: utxo(tag, amount, 500, None), pubkey: pk(key) }
}
fn cov(b: u8) -> [u8; 32] {
    [b; 32]
}
fn order<S>(tag: u8, amount: u64, c: [u8; 32], daa: u64, state: S) -> OrderUtxo<S> {
    OrderUtxo { utxo: utxo(tag, amount, daa, Some(c)), state }
}
fn user_tok(tag: u8, amount: i64, owner: [u8; 32]) -> TokenUtxo {
    TokenUtxo { utxo: utxo(tag, CARRIER, 500, Some(TOKEN_COV)), state: TokenState::user(TP.family(), amount, owner, EXT) }
}
fn custody(tag: u8, amount: i64, order: [u8; 32], daa: u64) -> TokenUtxo {
    TokenUtxo { utxo: utxo(tag, CARRIER, daa, Some(TOKEN_COV)), state: TokenState::custody(TP.family(), amount, order, EXT) }
}

/// The keeper tip of refunds; set by `refund_tip` from a measured refund transaction.
fn tok_fields() -> ([u8; 32], i64, i64) {
    let t = token_template(TP);
    (t.hash, t.prefix.len() as i64, t.suffix.len() as i64)
}

/// Limit ask of `amount` base units at `price` (`refund_tip` per the caller).
fn ask(maker: u8, price: i64, amount: i64, refund_tip: i64) -> AskState {
    let (h, p, s) = tok_fields();
    AskState {
        maker: pk(maker),
        token_cov_id: TOKEN_COV,
        token_tpl_hash: h,
        tpl_prefix_len: p,
        tpl_suffix_len: s,
        scale: SCALE,
        min_fill: MIN_FILL,
        price,
        tip: TIP,
        tif: 0,
        active_from: 0,
        expiry_daa: EXPIRY,
        refund_tip,
        interval: 0,
        max_fill: 0,
        slope: 0,
        price_end: 0,
        decay_step: 1_000,
        amount_left: amount,
    }
}
fn bid(maker: u8, price: i64, refund_tip: i64) -> BidState {
    let (h, p, s) = tok_fields();
    BidState {
        maker: pk(maker),
        token_cov_id: TOKEN_COV,
        token_tpl_hash: h,
        tpl_prefix_len: p,
        tpl_suffix_len: s,
        extension_commitment: EXT,
        scale: SCALE,
        min_fill: MIN_FILL,
        price,
        tip: TIP,
        tif: 0,
        active_from: 0,
        expiry_daa: EXPIRY,
        refund_tip,
        reserve: 0,
        delivery_carrier: DC,
        interval: 0,
        max_fill: 0,
        slope: 0,
        price_end: 0,
        decay_step: 1_000,
    }
}
fn batch(legs: Vec<Leg>) -> Batch {
    Batch {
        lock_time: NOW,
        legs,
        updates: vec![],
        taker_tokens: vec![],
        taker: Some(pk(TAKER)),
        taker_token_carrier: CARRIER,
        receivers: vec![],
        payments: vec![],
        funding: vec![],
        change: Some(pk(TAKER)),
        records: vec![],
        fee: FeeOptions::default(),
    }
}
fn refund_action(order: OrderUtxo<AnyState>, custody: Option<TokenUtxo>) -> RefundOrder {
    RefundOrder {
        order,
        custody,
        prefund: None,
        foreign: vec![],
        lock_time: EXPIRY as u64,
        funding: vec![],
        change: None,
        fee: FeeOptions::default(),
    }
}

// ---------------------------------------------------------------- run: build, sign, tighten, validate

struct Out {
    signed: SignedTx,
    v: Validation,
}

/// Measures the exact per-role compute budgets of `action` (pass 1), rebuilds with them (pass 2), signs,
/// finalizes with every budget tightened to the engine minimum, validates with enforced budgets and checks
/// that budget - 1 rejects on every metered input.
fn run(name: &str, action: &Action) -> Out {
    let keys = keys();
    let pass1 = build_with(action, &|_| Ok(GENEROUS)).unwrap_or_else(|e| panic!("{name}: build (pass 1): {e}"));
    let signed1 = finalize(&pass1, &sign_locally(&pass1, &keys).unwrap(), FinalizeOptions { tighten_budgets: true })
        .unwrap_or_else(|e| panic!("{name}: finalize (pass 1): {e}"));
    let v1 = validate_signed(&signed1).unwrap_or_else(|e| panic!("{name}: engine rejected (pass 1): {e}"));
    let mut table: BTreeMap<String, u16> = BTreeMap::new();
    for (role, b) in pass1.roles.iter().zip(&v1.budgets) {
        let e = table.entry(role.clone()).or_insert(0);
        *e = (*e).max(*b);
    }

    let built = build_with(action, &|r| table.get(r).copied().ok_or_else(|| Error::MissingBudget(r.to_string())))
        .unwrap_or_else(|e| panic!("{name}: build (pass 2): {e}"));
    let sigs = sign_locally(&built, &keys).unwrap();
    let signed = finalize(&built, &sigs, FinalizeOptions::default()).unwrap_or_else(|e| panic!("{name}: finalize: {e}"));
    validate_signed(&signed).unwrap_or_else(|e| panic!("{name}: engine rejected (measured role budgets): {e}"));
    let tight = finalize(&built, &sigs, FinalizeOptions { tighten_budgets: true }).unwrap();
    let v = validate_signed(&tight).unwrap_or_else(|e| panic!("{name}: engine rejected (exact budgets): {e}"));
    assert_eq!(tight.tx.id, built.tx.id, "{name}: signatures and budgets must not change the transaction id");
    assert!(v.fee >= v.min_fee, "{name}: fee below the floor");
    for (i, exact) in v.budgets.iter().enumerate() {
        if *exact > 0 {
            let (mut tx, entries) = tight.tx.to_tx().unwrap();
            tx.inputs[i].compute_commit = kaspa_consensus_core::mass::ComputeBudget::from(exact - 1).into();
            let r = execute(&tx, &entries, true).unwrap();
            assert!(r[i].is_err(), "{name}: input {i} ({}): budget - 1 unexpectedly passed", built.roles[i]);
        }
    }
    let m = &v.mass;
    println!(
        "TX {name}: {} in / {} out, {} B, compute {} storage {} transient {} (4x size) | floor {} sompi, paid {} | roles/budgets: {}",
        built.tx.inputs.len(),
        built.tx.outputs.len(),
        m.size,
        m.compute,
        m.storage,
        m.transient,
        v.min_fee,
        v.fee,
        built.roles.iter().zip(&v.budgets).map(|(r, b)| format!("{r}={b}")).collect::<Vec<_>>().join(" ")
    );
    Out { signed: tight, v }
}

/// Keeper tip that covers a refund: twice the measured floor of the refund transaction (as the tip table does).
fn refund_tip() -> i64 {
    let big = 2 * KAS as i64;
    let o = order(10, CARRIER, cov(0xa1), 1_000, AnyState::KobAsk(ask(MAKER_A, P250, 10 * WHOLE, big)));
    let probe = refund_action(o, Some(custody(11, 10 * WHOLE, cov(0xa1), 1_000)));
    let v = run("probe.refund.ask", &Action::RefundOrder(probe)).v;
    tip_for_fee(v.min_fee) as i64
}

// ---------------------------------------------------------------- scenarios

/// Taker buys `amount` base units from one ask of 10 whole tokens.
fn take_ask(rtip: i64, amount: i64) -> Action {
    let c = cov(0xa1);
    let mut b = batch(vec![Leg::Ask {
        order: order(10, CARRIER, c, 1_000, ask(MAKER_A, P250, 10 * WHOLE, rtip)),
        custody: custody(11, 10 * WHOLE, c, 1_000),
        amount,
        t: None,
    }]);
    b.funding = vec![key_utxo(100, TAKER, 1_000 * KAS)];
    Action::Batch(b)
}

/// Taker sweeps `k` asks (2 whole tokens each at (25 + i) / 10 KAS per token): the first k-1 fully, the last one
/// for 1.5 tokens (an amount that is not a multiple of the scale).
fn sweep(rtip: i64, k: usize) -> Action {
    let mut legs = vec![];
    for i in 0..k {
        let c = cov(0xa1 + i as u8);
        let price = P250 + i as i64 * 10_000_000;
        let (tag_o, tag_c) = (10 + 2 * i as u8, 11 + 2 * i as u8);
        legs.push(Leg::Ask {
            order: order(tag_o, CARRIER, c, 1_000, ask([MAKER_A, MAKER_B, MAKER_C][i % 3], price, 2 * WHOLE, rtip)),
            custody: custody(tag_c, 2 * WHOLE, c, 1_000),
            amount: if i + 1 == k { 1_500 } else { 2 * WHOLE },
            t: None,
        });
    }
    let mut b = batch(legs);
    b.funding = vec![key_utxo(100, TAKER, 1_000 * KAS)];
    Action::Batch(b)
}

/// Taker sells 6.5 tokens into 2 bids: bid 1 partial (4.5 of 10 tokens), bid 2 exhausted (2 tokens).
fn sell_two_bids(rtip: i64) -> Action {
    let (b1, b2) = (bid(MAKER_B, P245, rtip), bid(MAKER_C, P250, rtip));
    let (v1, v2) = (b1.used(10 * WHOLE).unwrap() + 3 * DC, b2.used(2 * WHOLE).unwrap() + DC);
    let mut b = batch(vec![
        Leg::Bid { order: order(20, v1 as u64, cov(0xb1), 1_000, b1), amount: 4_500, t: None },
        Leg::Bid { order: order(21, v2 as u64, cov(0xb2), 1_000, b2), amount: 2 * WHOLE, t: None },
    ]);
    b.taker_tokens = vec![user_tok(22, 6_500, pk(TAKER))];
    Action::Batch(b)
}

fn cancel(o: OrderUtxo<AnyState>, custody: Option<TokenUtxo>) -> Action {
    Action::CancelOrder(CancelOrder {
        order: o,
        custody,
        prefund: None,
        foreign: vec![],
        strays: vec![],
        tokens: vec![],
        funding: vec![],
        change: None,
        replace: None,
        lock_time: 0,
        records: vec![],
        fee: FeeOptions::default(),
    })
}

// ---------------------------------------------------------------- tests

#[test]
fn kaspacom_program_is_the_registered_v2_token() {
    let t = token_template(TP);
    assert_eq!(kob_protocol::json::to_hex(&t.hash), "911f0638ccb7368bf36d117f1725073ae7ee487ce8b58ca3e8375051c2d40f6c");
    assert_eq!((t.prefix.len(), t.state_len, t.suffix.len()), (1, 112, 25_439));
    assert_eq!(t.slots, (8, 8));
}

#[test]
fn ask_create_and_partial_fill() {
    let rtip = refund_tip();
    println!("REFUND TIP {rtip} sompi (twice the measured refund floor)");
    let create = CreateOrder {
        order: AnyState::KobAsk(ask(MAKER_A, P250, 10 * WHOLE, rtip)),
        value: CARRIER,
        tokens: vec![user_tok(1, 12 * WHOLE, pk(MAKER_A))],
        token_carrier: CARRIER,
        funding: vec![key_utxo(2, MAKER_A, 30 * KAS)],
        change: Some(pk(MAKER_A)),
        lock_time: 0,
        deadline: None,
        records: vec![],
        fee: FeeOptions::default(),
    };
    let c = run("v2 create ask (10 tokens + 2 tokens token change)", &Action::CreateOrder(create));
    assert_eq!(c.signed.tx.outputs.len(), 4, "order, custody, token change, KAS change");
    // The maker's 12-token UTXO reveals the 25.5 KB program once.
    assert!(c.v.mass.size > 25_552 && c.v.mass.size < 32_000, "create size {}", c.v.mass.size);

    let f = run("v2 fill: taker buys 4.321/10 tokens from 1 ask", &take_ask(rtip, 4_321));
    // One token input (the custody) reveals the program: the transaction is dominated by it.
    assert!((27_500..29_500).contains(&f.v.mass.size), "1-ask fill size {} (measured 28,302 B)", f.v.mass.size);
    assert!((36_000..42_000).contains(&f.v.mass.compute), "1-ask fill compute mass {} (measured 39,032)", f.v.mass.compute);
    assert!(f.v.mass.within_block_limits());
    assert_eq!(f.v.budgets.len(), 3, "ask + custody + funding");
    println!(
        "MEASURED 1-ask fill: {} B, compute {}, storage {}, floor {} sompi",
        f.v.mass.size, f.v.mass.compute, f.v.mass.storage, f.v.min_fee
    );
}

#[test]
fn sweeps_up_to_the_slot_limit_and_the_block_mass() {
    let rtip = refund_tip();
    let mut rows = vec![];
    for k in 1..=8usize {
        let o = run(&format!("v2 sweep of {k} asks ({} full + 1 partial)", k - 1), &sweep(rtip, k));
        let m = o.v.mass;
        assert!(m.within_block_limits(), "k={k}: {m:?} exceeds BLOCK_MASS_LIMITS");
        assert!(
            m.compute <= BLOCK_MASS_LIMITS.compute
                && m.storage <= BLOCK_MASS_LIMITS.storage
                && m.transient <= BLOCK_MASS_LIMITS.transient
        );
        // Every ask brings its custody token input, and every token input reveals the whole program.
        assert!(m.size > k as u64 * 25_552, "k={k}: size {} below k x program", m.size);
        rows.push((k, m, o.v.min_fee, o.v.fee));
    }
    // 3-ask sweep: assert the measured order of magnitude (3 program reveals + 3 order scripts).
    let (_, m3, _, _) = rows[2];
    assert!((81_000..86_000).contains(&m3.size), "3-ask sweep size {} (measured 83,414 B)", m3.size);
    assert!((96_000..106_000).contains(&m3.compute), "3-ask sweep compute mass {} (measured 101,064)", m3.compute);
    let (_, m8, _, _) = rows[7];
    assert!((215_000..228_000).contains(&m8.size), "8-ask sweep size {} (measured 221,194 B)", m8.size);

    // Linear growth: bytes per extra ask, and the sweep the mass limits alone would allow.
    let (_, m7, _, _) = rows[6];
    let per_ask_size = m8.size - m7.size;
    let per_ask_compute = m8.compute - m7.compute;
    let mut k_mass = 8u64;
    loop {
        let (sz, cm) = (m8.size + (k_mass + 1 - 8) * per_ask_size, m8.compute + (k_mass + 1 - 8) * per_ask_compute);
        if cm > BLOCK_MASS_LIMITS.compute || 4 * sz > BLOCK_MASS_LIMITS.transient {
            break;
        }
        k_mass += 1;
    }
    println!("SWEEP per extra ask: +{per_ask_size} B, +{per_ask_compute} compute mass");
    println!(
        "LARGEST SWEEP within BLOCK_MASS_LIMITS (compute {}, storage {}, transient {}): built and validated up to 8 asks \
         (size {} B, compute {}, transient {} = {}% of the transient limit); by extrapolation the mass limits alone would allow {k_mass}, \
         the token program's 8 token inputs cap the sweep at 8",
        BLOCK_MASS_LIMITS.compute,
        BLOCK_MASS_LIMITS.storage,
        BLOCK_MASS_LIMITS.transient,
        m8.size,
        m8.compute,
        m8.transient,
        100 * m8.transient / BLOCK_MASS_LIMITS.transient
    );
    for (k, m, floor, paid) in &rows {
        println!(
            "SWEEP k={k}: {} B, compute {}, storage {}, transient {}, floor {floor} sompi ({:.5} KAS), paid {paid}",
            m.size,
            m.compute,
            m.storage,
            m.transient,
            *floor as f64 / 1e8
        );
    }
    // The 8 token slots are the binding limit: a 9-ask sweep is refused by the builder, before any transaction.
    let err = build_with(&sweep(rtip, 9), &|_| Ok(GENEROUS)).expect_err("9 token inputs of one token exceed the program's 8 slots");
    println!("9-ask sweep refused by the builder: {err}");
    assert!(err.to_string().contains("8 token inputs"), "{err}");
}

#[test]
fn sell_into_bids() {
    let rtip = refund_tip();
    let o = run("v2 taker sells 6.5 tokens into 2 bids (1 partial + 1 exhausted)", &sell_two_bids(rtip));
    assert!(o.v.mass.within_block_limits());
    // The taker's P2PK token input is the only token input: one program reveal.
    assert!(o.v.mass.size > 25_552 && o.v.mass.size < 35_000, "size {}", o.v.mass.size);
    // One bid, sold 4 tokens (partial fill).
    let b1 = bid(MAKER_B, P245, rtip);
    assert_eq!(b1.used(10 * WHOLE), Some(10 * (P245 + TIP)), "the budget of whole tokens is exact");
    let mut b = batch(vec![Leg::Bid {
        order: order(20, (10 * (P245 + TIP) + 3 * DC) as u64, cov(0xb1), 1_000, b1),
        amount: 4 * WHOLE,
        t: None,
    }]);
    b.taker_tokens = vec![user_tok(21, 4 * WHOLE, pk(TAKER))];
    run("v2 taker sells 4 tokens into 1 bid (partial)", &Action::Batch(b));
}

#[test]
fn cancel_ask_and_bid() {
    let rtip = refund_tip();
    let a = cov(0xa1);
    let ask_o = order(10, CARRIER, a, 1_000, AnyState::KobAsk(ask(MAKER_A, P250, 10 * WHOLE, rtip)));
    let c = run("v2 maker cancels ask (tokens + carriers back to the maker)", &cancel(ask_o, Some(custody(11, 10 * WHOLE, a, 1_000))));
    assert!(c.v.mass.size > 25_552);
    let bid_o = order(20, 25 * KAS, cov(0xb1), 1_000, AnyState::KobBid(bid(MAKER_B, P245, rtip)));
    let c = run("v2 maker cancels bid", &cancel(bid_o, None));
    assert!(c.v.mass.size < 3_000, "a bid cancel carries no token program: {} B", c.v.mass.size);
}

#[test]
fn refund_after_expiry() {
    let rtip = refund_tip();
    let a = cov(0xa1);
    let fresh = (EXPIRY - 1_000) as u64;
    let ask_o = order(10, CARRIER, a, fresh, AnyState::KobAsk(ask(MAKER_A, P250, 10 * WHOLE, rtip)));
    let r =
        run("v2 keeper refunds an expired ask", &Action::RefundOrder(refund_action(ask_o, Some(custody(11, 10 * WHOLE, a, fresh)))));
    assert!(r.v.min_fee <= rtip as u64, "the refund tip {rtip} must cover the floor {}", r.v.min_fee);
    let bid_o = order(20, 25 * KAS, cov(0xb1), fresh, AnyState::KobBid(bid(MAKER_B, P245, rtip)));
    let r = run("v2 keeper refunds an expired bid", &Action::RefundOrder(refund_action(bid_o, None)));
    assert!(r.v.min_fee <= rtip as u64);

    // Before the expiry the builder refuses (the contract would too).
    let ask_o = order(10, CARRIER, a, fresh, AnyState::KobAsk(ask(MAKER_A, P250, 10 * WHOLE, rtip)));
    let mut early = refund_action(ask_o, Some(custody(11, 10 * WHOLE, a, fresh)));
    early.lock_time = EXPIRY as u64 - 1;
    assert!(build_with(&Action::RefundOrder(early), &|_| Ok(GENEROUS)).is_err(), "refund before expiry must be refused");
}
