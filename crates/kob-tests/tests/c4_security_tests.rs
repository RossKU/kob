//! C4 security verification (internal review of KOB's own covenants, 2026-10-02): regression tests for the property
//! cells the other suites did not cover. The review itself (property x contract matrix, findings) is the internal
//! report `c4_security.md`; the properties are numbered as there.
//!
//! * **P8 resource bounds** (static): every hand-unrolled bounded scan of the 15 order templates (the stray guards)
//!   visits EVERY slot below its cap, each line indexing its own slot. Since the cost pass the
//!   scans are written out one line per slot (`if (cnt > k) { ... OpCovInputIdx(token, k) ... }`): a missing or
//!   duplicated line would leave a token input or output unscanned behind `require(cnt <= CAP)`, which no engine test
//!   of a small batch notices.
//! * **P5 time** (engine, every builder shape of both families and every pair order shape of the four family mixes): each CLTV rule
//!   (`tx.daa >= x`, SilverScript `OpWithin(0, LOCK_TIME_THRESHOLD)` + `OP_CHECKLOCKTIMEVERIFY`) is "not earlier than"
//!   only and cannot be met by a builder that (a) switches the lock time to the other type (a unix-ms time lock that
//!   is numerically above every DAA bound), or (b) finalises the order input (sequence `u64::MAX`, which makes
//!   consensus ignore the lock time); and each CSV rule (`this.ageDaa >= x`: TWAP / DCA slices, the trailing wait)
//!   cannot be met with the sequence-lock disable bit or a shorter relative lock. The router's `tx.time >= deadline`
//!   expiry is checked the same way (a DAA lock time, a finalised intent input).
//!
//! Every negative is checked input by input: exactly the inputs whose script carries the rule fail, everything else
//! (P2PK funding re-signed, token programs) still passes, so a failure is the time rule and not a broken transaction.
//! Run: cargo test -p kob-tests --test c4_security_tests -- --nocapture --test-threads=1

mod common;
#[path = "../../kob-protocol/tests/common/mod.rs"]
mod fx;

use std::collections::{BTreeMap, BTreeSet};

use kaspa_consensus_core::tx::{Transaction, UtxoEntry};
use kaspa_txscript::{LOCK_TIME_THRESHOLD, MAX_TX_IN_SEQUENCE_NUM, SEQUENCE_LOCK_TIME_DISABLED};
use kob_protocol::artifacts::TemplateId;
use kob_protocol::build::{build, build_expire_intent, intent_budgets, Action, ExpireIntent};
use kob_protocol::family::Family;
use kob_protocol::router::{Actor, IntentState};
use kob_protocol::state::{AnyState, TokenState};
use kob_protocol::tx::{finalize, sighash, sign_digest, sign_locally, Arg, BuiltTx, FinalizeOptions, SigPlan, TokenUtxo, Utxo};
use kob_protocol::verify::execute;

// ================================================================================================ P8: unrolled scans

/// The 15 order templates (the KAS-quoted kinds of both families; one template each for the pair kinds, serving both
/// sides and both families): every bounded scan they run.
const ORDER_SOURCES: [&str; 15] = [
    "contracts/v2/KobAsk.sil",
    "contracts/v2/KobBid.sil",
    "contracts/v2/KobCondAsk.sil",
    "contracts/v2/KobCondBid.sil",
    "contracts/v2/KobIfdBid.sil",
    "contracts/v2/KobIfdAsk.sil",
    "contracts/v2/KobPair.sil",
    "contracts/v2/KobCondPair.sil",
    "contracts/v2/KobIfdPair.sil",
    "contracts/adapters/kron/v2/KobAskKron.sil",
    "contracts/adapters/kron/v2/KobBidKron.sil",
    "contracts/adapters/kron/v2/KobCondAskKron.sil",
    "contracts/adapters/kron/v2/KobCondBidKron.sil",
    "contracts/adapters/kron/v2/KobIfdBidKron.sil",
    "contracts/adapters/kron/v2/KobIfdAskKron.sil",
];

/// One bounded scan: `require(cnt <= CAP);` followed by its unrolled slot lines.
#[derive(Debug)]
struct Scan {
    cap_name: String,
    cap: usize,
    slots: Vec<usize>,
    /// Problems of the slot lines (wrong index, foreign slot variable, no guard).
    problems: Vec<String>,
}

/// `int constant NAME = N;` declarations of a source.
fn int_constants(src: &str) -> BTreeMap<String, usize> {
    src.lines()
        .filter_map(|l| {
            let rest = l.trim().strip_prefix("int constant ")?;
            let (name, value) = rest.split_once(" = ")?;
            Some((name.trim().to_string(), value.trim_end_matches(';').trim().parse().ok()?))
        })
        .collect()
}

/// The arguments of every `...Idx(` call in `body` (the text between the parentheses).
fn idx_calls(body: &str) -> Vec<String> {
    let mut out = vec![];
    let mut rest = body;
    while let Some(p) = rest.find("Idx(") {
        let after = &rest[p + 4..];
        let mut depth = 1;
        let mut end = after.len();
        for (i, c) in after.char_indices() {
            match c {
                '(' => depth += 1,
                ')' => {
                    depth -= 1;
                    if depth == 0 {
                        end = i;
                        break;
                    }
                }
                _ => {}
            }
        }
        out.push(after[..end].to_string());
        rest = &after[end..];
    }
    out
}

/// Slot variables of a line (`x3`, `e3`): identifier tokens made of one of `x` / `e` and digits.
fn slot_vars(body: &str) -> Vec<(char, usize)> {
    body.split(|c: char| !c.is_ascii_alphanumeric())
        .filter_map(|t| {
            let mut cs = t.chars();
            let first = cs.next()?;
            let digits: String = cs.collect();
            ((first == 'x' || first == 'e') && !digits.is_empty() && digits.chars().all(|c| c.is_ascii_digit()))
                .then(|| (first, digits.parse().unwrap()))
        })
        .collect()
}

/// Every bounded scan of a source.
fn scans(src: &str) -> Vec<Scan> {
    let consts = int_constants(src);
    let lines: Vec<&str> = src.lines().collect();
    let mut out = vec![];
    for (i, l) in lines.iter().enumerate() {
        let Some(cap_name) = l.trim().strip_prefix("require(cnt <= ").and_then(|r| r.strip_suffix(");")) else { continue };
        let cap = *consts.get(cap_name).unwrap_or_else(|| panic!("unknown cap {cap_name}"));
        let mut slots = vec![];
        let mut problems = vec![];
        for line in &lines[i + 1..] {
            let t = line.trim();
            let Some(rest) = t.strip_prefix("if (cnt > ") else {
                if slots.is_empty() {
                    continue; // set-up lines between the bound and the first slot (offset, family selection)
                }
                break;
            };
            let (k, body) = rest.split_once(") {").unwrap_or_else(|| panic!("slot line: {t}"));
            let k: usize = k.parse().unwrap_or_else(|_| panic!("slot index: {t}"));
            slots.push(k);
            let calls = idx_calls(body);
            if calls.len() != 1 || calls[0].rsplit(',').next().map(str::trim) != Some(&k.to_string()) {
                problems.push(format!("slot {k}: the line must read exactly its own slot (Idx calls {calls:?})"));
            }
            if let Some((v, n)) = slot_vars(body).into_iter().find(|(_, n)| *n != k) {
                problems.push(format!("slot {k}: uses the variable of slot {n} ({v}{n})"));
            }
            if !(body.contains("!= me") || body.contains("bSource(") || body.contains("aOut(")) {
                problems.push(format!("slot {k}: no guard (stray check, bSource or aOut)"));
            }
        }
        out.push(Scan { cap_name: cap_name.to_string(), cap, slots, problems });
    }
    out
}

/// Problems of every scan of a source (empty = every scan visits slots 0..cap, each line its own slot, guarded).
fn scan_problems(src: &str) -> Vec<String> {
    let mut v = vec![];
    for s in scans(src) {
        let want: Vec<usize> = (0..s.cap).collect();
        if s.slots != want {
            v.push(format!("{} = {}: slots {:?}, want {:?}", s.cap_name, s.cap, s.slots, want));
        }
        v.extend(s.problems.iter().map(|p| format!("{}: {p}", s.cap_name)));
    }
    v
}

/// P8: no token input / output can sit in an unscanned slot. Every bounded scan of every order template (both
/// families) is unrolled over EXACTLY the slots 0..CAP-1 behind its `require(cnt <= CAP)`, each line reads its own slot,
/// and every line carries the guard. Negative: the checker flags a dropped line, a line that reads another slot and a
/// line without its guard (it is not vacuous).
#[test]
fn c4_p8_unrolled_scans_cover_every_slot() {
    let mut expected_scans = 0;
    for path in ORDER_SOURCES {
        let src = std::fs::read_to_string(common::repo_root().join(path)).unwrap_or_else(|e| panic!("{path}: {e}"));
        let found = scans(&src);
        assert!(!found.is_empty(), "{path}: no bounded scan found (the checker no longer matches the source)");
        for s in &found {
            println!("SCAN {path:<46} {:<12} cap {} slots {:?}", s.cap_name, s.cap, s.slots);
        }
        expected_scans += found.len();
        let p = scan_problems(&src);
        assert!(p.is_empty(), "{path}: {p:#?}");
    }
    // one stray guard per template (a pair template's guard is one function called for each of its two tokens)
    assert_eq!(expected_scans, 15, "bounded scans of the 15 order templates");

    // the checker is not vacuous
    let ask = std::fs::read_to_string(common::repo_root().join("contracts/v2/KobAsk.sil")).unwrap();
    let line5 = ask.lines().find(|l| l.trim_start().starts_with("if (cnt > 5)")).expect("slot 5").to_string();
    let dropped = ask.replace(&format!("{line5}\n"), "");
    assert!(!scan_problems(&dropped).is_empty(), "a dropped slot line must be flagged");
    let wrong = ask.replace(&line5, &line5.replace("OpCovInputIdx(tokenCovId, 5)", "OpCovInputIdx(tokenCovId, 4)"));
    assert!(!scan_problems(&wrong).is_empty(), "a slot line reading another slot must be flagged");
    let unguarded = ask.replace(&line5, &line5.replace("!= me", "== me || true"));
    assert!(!scan_problems(&unguarded).is_empty(), "a slot line without its guard must be flagged");
    let pair = std::fs::read_to_string(common::repo_root().join("contracts/v2/KobPair.sil")).unwrap();
    let x7 = pair.lines().find(|l| l.trim_start().starts_with("if (cnt > 7)")).expect("pair slot 7").to_string();
    assert!(!scan_problems(&pair.replace(&format!("{x7}\n"), "")).is_empty(), "a dropped pair slot must be flagged");
}

// ================================================================================================ P5: lock times

/// Every builder shape the golden vectors cover (reference 3/3 KCC-20 and the common KRON program) and the pair order
/// shapes of the four family mixes.
fn shapes() -> Vec<(String, Action)> {
    use TemplateId::{Kcc20Ref, KronToken2433, KronToken2732};
    let mut v: Vec<(String, Action)> = fx::scenarios().into_iter().chain(fx::scenarios_kron()).collect();
    for (tag, pa, pb) in [
        ("", Kcc20Ref, Kcc20Ref),
        ("kcc20-kron.", Kcc20Ref, KronToken2433),
        ("kron-kcc20.", KronToken2433, Kcc20Ref),
        ("kron-kron.", KronToken2433, KronToken2732),
    ] {
        for (n, a) in fx::pair::pair_scenarios(pa, pb) {
            v.push((format!("{tag}{n}"), a));
        }
    }
    v
}

/// What time rules an input's script runs.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct Rules {
    /// A CLTV rule (`tx.daa >= x`).
    cltv: bool,
    /// A CSV rule (`this.ageDaa >= x`) and its operand.
    csv: Option<i64>,
}

/// The time rules of an order input, read from its plan (template, decoded state, entry and arguments):
/// - every order entry but `cancel` and a repeat merge runs a CLTV rule (fill / settle `tx.daa >= activeFrom`,
///   refund / close / kill `tx.daa >= due`, update `tx.daa >= activeFrom`);
/// - a fill of a TWAP ask / DCA bid (`interval > 0`) runs `this.ageDaa >= interval`; a trailing update (`KobCondAsk`
///   on bid evidence, `tk < 0`; `KobCondBid` on ask evidence, `tk >= 0`; `KobCondPair` update with k > 0) runs
///   `this.ageDaa >= trailWait`; a `KobPair` fill with `interval > 0` runs `this.ageDaa >= interval`.
fn rules_of(plan: &SigPlan) -> Rules {
    let SigPlan::Entry { template, state, entry, args } = plan else { return Rules::default() };
    if template.is_token() || entry == "cancel" {
        return Rules::default();
    }
    let first_nb = match args.first() {
        Some(Arg::Bytes(b)) if b.len() == 8 => Some(b.clone()),
        _ => None,
    };
    let merge = first_nb.as_ref().is_some_and(|b| b[7] & 0x80 != 0);
    if merge {
        return Rules::default();
    }
    let n = first_nb.map(|b| i64::from_le_bytes(b.try_into().unwrap())).unwrap_or(0);
    let st = AnyState::decode(*template, state).unwrap_or_else(|e| panic!("{}: state: {e}", template.name()));
    let int = |i: usize| match args.get(i) {
        Some(Arg::Int(v)) => *v,
        a => panic!("{}.{entry}: argument {i} is {a:?}", template.name()),
    };
    let csv = match (&st, entry.as_str()) {
        (AnyState::KobAsk(a) | AnyState::KobAskKron(a), "settle") if n > 0 && a.interval > 0 => Some(a.interval),
        (AnyState::KobBid(b) | AnyState::KobBidKron(b), "fill") if n > 0 && b.interval > 0 => Some(b.interval),
        (AnyState::KobCondAsk(c) | AnyState::KobCondAskKron(c), "update") if int(1) < 0 => Some(c.trail_wait),
        (AnyState::KobCondBid(c) | AnyState::KobCondBidKron(c), "update") if int(1) >= 0 => Some(c.trail_wait),
        (AnyState::KobPair(p), "settle") if n > 0 && p.interval > 0 => Some(p.interval),
        // settle(nb, custIn, tTplIn, t, sOut, tOut, leg, evA, evB, tk, evMode, upd, k): a trailing update (upd 1, k > 0)
        (AnyState::KobCondPair(c), "settle") if n == 0 && int(11) == 1 && int(12) > 0 => Some(c.trail_wait),
        _ => None,
    };
    Rules { cltv: true, csv }
}

/// A shape built, signed and finalised (budgets untightened: the engine runs with an unlimited meter below).
struct Shape {
    built: BuiltTx,
    tx: Transaction,
    entries: Vec<UtxoEntry>,
}

fn shape(name: &str, action: &Action, keys: &BTreeMap<[u8; 32], [u8; 32]>) -> Shape {
    let built = build(action).unwrap_or_else(|e| panic!("{name}: build: {e}"));
    let sigs = sign_locally(&built, keys).unwrap();
    let signed = finalize(&built, &sigs, FinalizeOptions { tighten_budgets: false }).unwrap();
    let (tx, entries) = signed.tx.to_tx().unwrap();
    Shape { built, tx, entries }
}

/// Signs every input that carries a signature again (after a lock-time or sequence edit: SIGHASH_ALL covers both).
fn resign(s: &Shape, tx: &mut Transaction, keys: &BTreeMap<[u8; 32], [u8; 32]>) {
    for (i, plan) in s.built.plans.iter().enumerate() {
        if let Some(k) = plan.signer() {
            let sig = sign_digest(&keys[&k], &sighash(tx, &s.entries, i)).unwrap();
            tx.inputs[i].signature_script = plan.sigscript(Some(&sig)).unwrap();
        }
    }
}

/// Indices of the inputs the engine refuses.
fn failing(tx: &Transaction, entries: &[UtxoEntry]) -> BTreeSet<usize> {
    execute(tx, entries, false).unwrap().iter().enumerate().filter(|(_, r)| r.is_err()).map(|(i, _)| i).collect()
}

/// P5 (orders, both families, every builder shape and every pair order shape). Baseline: the honest transaction passes on
/// every input. Then:
/// - TIME LOCK: the lock time becomes a unix-ms time lock numerically above every DAA bound (LOCK_TIME_THRESHOLD +
///   the honest lock time): exactly the inputs with a CLTV rule fail (fills, refunds, kills, closes and updates of
///   every order kind; cancels and repeat merges carry none), so no "not before" rule can be met by switching the lock
///   time's type;
/// - FINAL: one CLTV input at a time gets the final sequence (u64::MAX, which makes consensus ignore the lock time):
///   exactly that input fails;
/// - CSV OFF: the same input gets the sequence-lock disable bit (1 << 63) on its honest sequence: it fails exactly when
///   its script runs a CSV rule (TWAP / DCA slices, trailing waits), every other input is unaffected (control);
/// - CSV SHORT: an input with a CSV rule `>= w` (w > 0) gets the relative lock w - 1: it fails.
#[test]
fn c4_p5_order_time_rules_cannot_be_bypassed() {
    let keys = fx::keys();
    let (mut n_cltv, mut n_csv, mut n_shapes) = (0usize, 0usize, 0usize);
    for (name, action) in shapes() {
        let s = shape(&name, &action, &keys);
        let base = failing(&s.tx, &s.entries);
        assert!(base.is_empty(), "{name}: the honest transaction fails on inputs {base:?}");
        let rules: Vec<Rules> = s.built.plans.iter().map(rules_of).collect();
        let cltv: BTreeSet<usize> = rules.iter().enumerate().filter(|(_, r)| r.cltv).map(|(i, _)| i).collect();
        if cltv.is_empty() {
            continue; // creations, cancels, transfers: no order rule runs a lock time
        }
        n_shapes += 1;

        // TIME LOCK
        let mut tx = s.tx.clone();
        tx.lock_time = LOCK_TIME_THRESHOLD + s.tx.lock_time.max(1);
        resign(&s, &mut tx, &keys);
        let got = failing(&tx, &s.entries);
        assert_eq!(got, cltv, "{name}: a time-type lock time must fail exactly the CLTV inputs");

        for &i in &cltv {
            n_cltv += 1;
            // FINAL
            let mut tx = s.tx.clone();
            tx.inputs[i].sequence = MAX_TX_IN_SEQUENCE_NUM;
            resign(&s, &mut tx, &keys);
            assert_eq!(failing(&tx, &s.entries), BTreeSet::from([i]), "{name}: input {i} finalised must fail alone");

            // CSV OFF
            let mut tx = s.tx.clone();
            tx.inputs[i].sequence |= SEQUENCE_LOCK_TIME_DISABLED;
            resign(&s, &mut tx, &keys);
            let want = if rules[i].csv.is_some() { BTreeSet::from([i]) } else { BTreeSet::new() };
            assert_eq!(failing(&tx, &s.entries), want, "{name}: input {i} with the sequence-lock disable bit");

            // CSV SHORT
            if let Some(w) = rules[i].csv.filter(|w| *w > 0) {
                n_csv += 1;
                assert!(s.tx.inputs[i].sequence >= w as u64, "{name}: the builder sets the relative lock of input {i}");
                let mut tx = s.tx.clone();
                tx.inputs[i].sequence = w as u64 - 1;
                resign(&s, &mut tx, &keys);
                assert_eq!(failing(&tx, &s.entries), BTreeSet::from([i]), "{name}: input {i} one DAA short of its CSV rule");
            }
        }
    }
    println!("P5: {n_shapes} shapes, {n_cltv} CLTV inputs, {n_csv} CSV inputs with a positive wait");
    assert!(n_shapes >= 150 && n_cltv >= 250, "the shape set shrank ({n_shapes} shapes, {n_cltv} CLTV inputs)");
    assert!(n_csv >= 10, "TWAP / DCA / trailing shapes of both families ({n_csv})");
}

/// P5 (router intents). `expire` is `tx.time >= deadline` (`deadline >= LOCK_TIME_THRESHOLD` checked in script, then
/// CLTV). For each intent kind (KAS -> token, token -> KAS, token swap), the honest expiry (lock time = the deadline)
/// passes; with a DAA lock time (the largest one, LOCK_TIME_THRESHOLD - 1), with the intent input finalised
/// (sequence u64::MAX: consensus would ignore the lock time) or one millisecond early, the intent input fails and
/// nothing else does.
#[test]
fn c4_p5_router_expiry_cannot_be_moved_earlier() {
    const DEADLINE: i64 = 1_800_000_000_000;
    let (payer, merchant) = (fx::pk(11), fx::pk(fx::MERCHANT));
    let id = fx::cov(0xc1);
    let kinds = [
        (
            "KasToToken_buy",
            IntentState::KasToToken {
                payer,
                merchant,
                token: fx::TOKEN_B,
                program: TemplateId::Kcc20Ref8x8,
                amount: 5 * fx::WHOLE,
                max_pay: 13 * fx::KAS as i64,
                max_extra: 2 * fx::KAS as i64,
                b_extension: fx::EXT,
                deadline: DEADLINE,
            },
            0,
        ),
        (
            "TokenToKas_sell",
            IntentState::TokenToKas {
                payer,
                merchant,
                token: fx::TOKEN_COV,
                program: TemplateId::Kcc20Ref8x8,
                merchant_kas: 4 * fx::KAS as i64,
                max_sell: 2 * fx::WHOLE,
                lock_amount: 2 * fx::WHOLE,
                lock_extension: fx::EXT,
                deadline: DEADLINE,
            },
            2 * fx::WHOLE,
        ),
        (
            "TokenSwap_swap",
            IntentState::TokenSwap {
                payer,
                merchant,
                token_a: fx::TOKEN_COV,
                program_a: TemplateId::Kcc20Ref8x8,
                token_b: fx::TOKEN_B,
                program_b: TemplateId::Kcc20Ref8x8,
                max_sell_a: 2 * fx::WHOLE,
                amount_b: fx::WHOLE,
                lock_amount: 2 * fx::WHOLE,
                lock_extension: fx::EXT,
                b_extension: fx::EXT,
                deadline: DEADLINE,
            },
            2 * fx::WHOLE,
        ),
    ];
    for (actor, state, locked) in kinds {
        let a = Actor::by_name(actor).unwrap();
        let intent =
            Utxo { transaction_id: [0xc1; 32], index: 0, amount: 20 * fx::KAS, block_daa_score: 2_000, covenant_id: Some(id) };
        let lock = (locked > 0).then(|| TokenUtxo {
            utxo: Utxo {
                transaction_id: [0xc2; 32],
                index: 1,
                amount: fx::KAS,
                block_daa_score: 2_000,
                covenant_id: Some(fx::TOKEN_COV),
            },
            state: TokenState::custody(Family::Kcc20, locked, id, fx::EXT),
        });
        let r = ExpireIntent { actor: a.name.into(), state, intent, lock, fee: fx::fee() };
        let built = build_expire_intent(&r, &intent_budgets).unwrap_or_else(|e| panic!("{actor}: {e}"));
        let signed = finalize(&built, &[], FinalizeOptions { tighten_budgets: false }).unwrap();
        let (tx, entries) = signed.tx.to_tx().unwrap();
        assert_eq!(tx.lock_time, DEADLINE as u64, "{actor}: the expiry's lock time is the deadline");
        assert!(failing(&tx, &entries).is_empty(), "{actor}: the honest expiry must pass");
        let mut daa = tx.clone();
        daa.lock_time = LOCK_TIME_THRESHOLD - 1;
        let mut fin = tx.clone();
        fin.inputs[0].sequence = MAX_TX_IN_SEQUENCE_NUM;
        let mut early = tx.clone();
        early.lock_time = DEADLINE as u64 - 1;
        for (what, bad) in [("a DAA lock time", daa), ("the intent input finalised", fin), ("one millisecond early", early)] {
            assert_eq!(failing(&bad, &entries), BTreeSet::from([0]), "{actor}: an expiry with {what} must fail at the intent");
        }
    }
}

// ================================================================================================ P4: router cancel

/// P4 router token intents (finding F2 of the review, fixed 2026-10-02). A token intent's `cancel` must spend the locked
/// tokens in the same transaction (the cancel ends the intent covenant; tokens owned by its id could never move
/// afterwards). The router requires the payer's signature and the lock as the one token input of the locked token, at
/// input j + 1, read under the intent's token handle (open ICC): owned by this intent's id with KCC-20 owner_scheme 0x04
/// or KRON id_type 2. Run on the 8/8 and 3/3 KCC-20 programs and on both KRON programs (whose key-held tokens are
/// authorised by a P2PK input of the owner, which the forger adds):
/// - the honest cancel (`build_cancel_intent`, the lock at j + 1, tokens back to the payer) passes;
/// - KOB's builder refuses a "lock" that the intent does not own;
/// - a payer-signed cancel whose input j + 1 is another token UTXO of the same token (here the payer's own key-held
///   tokens), leaving the lock behind, is refused by the router (before the fix it passed: only the position was checked),
///   and accepted by the actor with the two owner lines of its cancel removed (ablation).
#[test]
fn c4_router_token_cancel_checks_the_lock_owner() {
    use kaspa_txscript::pay_to_script_hash_script;
    use kob_protocol::build::{build_cancel_intent, CancelIntent};
    use kob_protocol::state::{Kcc20State, KronState};
    use kob_protocol::tx::{spk_to_string, Witness};
    use silverscript_abi::ArtifactValue;
    let keys = fx::keys();
    let payer = fx::pk(11);
    let id = fx::cov(0xc1);
    const D: i64 = 1_800_000_000_000;
    // (actor, program of the locked token A)
    let cases = [
        ("TokenToKas_sell", TemplateId::Kcc20Ref8x8),
        ("TokenToKas_sell2", TemplateId::Kcc20Ref),
        ("TokenToKasKron_sell", TemplateId::KronToken2433),
        ("TokenSwapKron_swap", TemplateId::KronToken2732),
    ];
    for (actor, prog) in cases {
        let fam = prog.family();
        let a = Actor::by_name(actor).unwrap();
        let (merchant, max_sell) = (fx::pk(fx::MERCHANT), 2 * fx::WHOLE);
        let ext = if fam == Family::Kron { [0; 32] } else { fx::EXT };
        let units = 3 * fx::WHOLE;
        let state = if a.shape.kind == kob_protocol::router::IntentKind::TokenSwap {
            IntentState::TokenSwap {
                payer,
                merchant,
                token_a: fx::TOKEN_COV,
                program_a: prog,
                token_b: fx::TOKEN_B,
                program_b: TemplateId::Kcc20Ref8x8,
                max_sell_a: max_sell,
                amount_b: fx::WHOLE,
                lock_amount: units,
                lock_extension: ext,
                b_extension: fx::EXT,
                deadline: D,
            }
        } else {
            IntentState::TokenToKas {
                payer,
                merchant,
                token: fx::TOKEN_COV,
                program: prog,
                merchant_kas: 4 * fx::KAS as i64,
                max_sell,
                lock_amount: units,
                lock_extension: ext,
                deadline: D,
            }
        };
        let intent = Utxo { transaction_id: [0xc1; 32], index: 0, amount: 2 * fx::KAS, block_daa_score: 2_000, covenant_id: Some(id) };
        let tok_utxo = |tag: u8| Utxo {
            transaction_id: [tag; 32],
            index: 1,
            amount: fx::KAS,
            block_daa_score: 2_000,
            covenant_id: Some(fx::TOKEN_COV),
        };
        let lock = TokenUtxo { utxo: tok_utxo(0xc2), state: TokenState::custody(fam, units, id, ext) };
        let own = TokenUtxo { utxo: tok_utxo(0xc3), state: TokenState::user(fam, units, payer, ext) };
        let req = |l: &TokenUtxo| CancelIntent {
            actor: actor.into(),
            state: state.clone(),
            intent: intent.clone(),
            lock: Some(l.clone()),
            to: None,
            fee: fx::fee(),
        };

        // honest cancel
        let built = build_cancel_intent(&req(&lock), &intent_budgets).unwrap();
        let sigs = sign_locally(&built, &keys).unwrap();
        let (tx, entries) = finalize(&built, &sigs, FinalizeOptions { tighten_budgets: false }).unwrap().tx.to_tx().unwrap();
        assert!(failing(&tx, &entries).is_empty(), "{actor}: the honest cancel (lock at j + 1) must pass");
        assert_eq!(tx.inputs[1].previous_outpoint.transaction_id.as_bytes(), [0xc2; 32], "the lock is input j + 1");

        // KOB's builder refuses a cancel that does not spend the lock
        let e = build_cancel_intent(&req(&own), &intent_budgets).unwrap_err();
        assert!(e.to_string().contains("not owned by this intent"), "{e}");

        // residual: the same transaction with the payer's own tokens at j + 1 instead of the lock (signed by the payer).
        // A key-held KRON token is authorised by a P2PK input of its owner (address presence): the forger adds one.
        let mut b = built.clone();
        let spk = own.state.spk_with(kob_protocol::artifacts::token_template(prog));
        let inp = &mut b.tx.inputs[1];
        inp.transaction_id = [0xc3; 32];
        inp.utxo.script_public_key = spk_to_string(&spk);
        b.plans[1] = match b.plans[1].clone() {
            SigPlan::TokenLeader { template, next_states, .. } => SigPlan::TokenLeader {
                template,
                state: Kcc20State::p2pk(units, payer, ext),
                next_states,
                witness: Witness::P2pk(payer),
            },
            SigPlan::KronToken { template, next_states, .. } => {
                let mut fund = b.tx.inputs[1].clone();
                fund.transaction_id = [0xc4; 32];
                fund.index = 0;
                fund.utxo.covenant_id = None;
                fund.utxo.script_public_key = spk_to_string(&kob_protocol::script::p2pk_spk(&payer));
                b.tx.inputs.push(fund);
                b.plans.push(SigPlan::P2pk { pubkey: payer });
                SigPlan::KronToken { template, state: KronState::addr(units, payer), next_states, witnesses: vec![2] }
            }
            other => panic!("{actor}: input 1 is a token input, not {other:?}"),
        };
        let (mut tx, entries) = b.tx.to_tx().unwrap();
        for (i, plan) in b.plans.iter().enumerate() {
            let sig = plan.signer().map(|k| sign_digest(&keys[&k], &sighash(&tx, &entries, i)).unwrap());
            tx.inputs[i].signature_script = plan.sigscript(sig.as_deref()).unwrap();
        }
        assert!(
            !tx.inputs.iter().any(|i| i.previous_outpoint.transaction_id.as_bytes() == [0xc2; 32]),
            "the forged cancel leaves the lock behind"
        );
        let f = failing(&tx, &entries);
        assert_eq!(
            f,
            BTreeSet::from([0]),
            "{actor}: the router refuses a payer-signed cancel whose j + 1 is not the lock (F2), and only the intent input fails"
        );

        // Ablation: the same actor without the owner check of its observed lock (the owner and its owner type, two
        // lines of the generated cancel) accepts this forged cancel, so the owner check is what refuses it.
        let src = common::contract_source(actor).replace("\r\n", "\n");
        let grp = if a.shape.kind == kob_protocol::router::IntentKind::TokenSwap { "token_a_group" } else { "token" };
        let owner_type = if fam == Family::Kron { "id_type == KRON_ID_COVENANT" } else { "owner_scheme == OWNER_COVENANT_ID" };
        let mut ablated = src.clone();
        for line in [
            format!("        require(gen__{grp}_mine_state.owner == byte[32](OpInputCovenantId(this.activeInputIndex)));\n"),
            format!("        require(gen__{grp}_mine_state.{owner_type});\n"),
        ] {
            // the cancel is the last entry: its lines are the last occurrence (expire checks the lock owner too)
            let at = ablated.rfind(&line).unwrap_or_else(|| panic!("{actor}: generated router changed: {line}"));
            assert!(at > ablated.rfind("entry cancel(").unwrap(), "{actor}: {line} belongs to the cancel");
            ablated.replace_range(at..at + line.len(), "");
        }
        let handle = |p: TemplateId| ArtifactValue::Bytes(kob_protocol::artifacts::token_template(p).hash.to_vec());
        let bytes = |v: &[u8; 32]| ArtifactValue::Bytes(v.to_vec());
        let args = match &state {
            IntentState::TokenToKas { payer, merchant, token, program, merchant_kas, max_sell, lock_amount, deadline, .. } => vec![
                bytes(payer),
                bytes(merchant),
                bytes(token),
                handle(*program),
                ArtifactValue::Int(*merchant_kas),
                ArtifactValue::Int(*max_sell),
                ArtifactValue::Int(*lock_amount),
                ArtifactValue::Int(*deadline),
            ],
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
                deadline,
                ..
            } => {
                vec![
                    bytes(payer),
                    bytes(merchant),
                    bytes(token_a),
                    handle(*program_a),
                    bytes(token_b),
                    handle(*program_b),
                    ArtifactValue::Int(*max_sell_a),
                    ArtifactValue::Int(*amount_b),
                    ArtifactValue::Int(*lock_amount),
                    ArtifactValue::Int(*deadline),
                ]
            }
            _ => unreachable!(),
        };
        // a KCC-20 token A's state pins the lock's extension commitment too (before the deadline)
        let mut args = args;
        if fam == Family::Kcc20 {
            args.insert(args.len() - 1, bytes(&ext));
        }
        // a swap pins the extension commitment of token B (the B pin), the last field before the deadline
        if let IntentState::TokenSwap { b_extension, .. } = &state {
            args.insert(args.len() - 1, bytes(b_extension));
        }
        let compile =
            |s: &str| common::compile_contract(s, &args, Default::default()).unwrap_or_else(|e| panic!("compile {actor}: {e:?}"));
        let shipped = compile(&src);
        assert_eq!(
            pay_to_script_hash_script(&common::bytecode(&shipped)),
            entries[0].script_public_key,
            "the source is the pinned actor"
        );
        let abl = compile(&ablated);
        let mut entries2 = entries.clone();
        entries2[0].script_public_key = pay_to_script_hash_script(&common::bytecode(&abl));
        let t = kob_protocol::artifacts::token_template(prog);
        let mut tx2 = tx.clone();
        for (i, plan) in b.plans.iter().enumerate() {
            let sig = plan.signer().map(|k| sign_digest(&keys[&k], &sighash(&tx2, &entries2, i)).unwrap());
            tx2.inputs[i].signature_script = if i == 0 {
                let cancel_args = [
                    ArtifactValue::from(sig.expect("payer signs")),
                    ArtifactValue::Int(t.prefix.len() as i64),
                    ArtifactValue::Int(t.suffix.len() as i64),
                ];
                let mut ss = common::encode_entry_sig_script(&abl, "cancel", &cancel_args).unwrap();
                ss.extend(common::push_redeem_script(&common::bytecode(&abl)));
                ss
            } else {
                plan.sigscript(sig.as_deref()).unwrap()
            };
        }
        assert!(failing(&tx2, &entries2).is_empty(), "{actor}: ablated router (no owner check of the lock): the forged cancel passes");
        println!("{actor} on {}: the lock owner check refuses the forged cancel; without it, it passes", prog.name());
    }
}
