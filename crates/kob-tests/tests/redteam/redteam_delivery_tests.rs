//! Tests delivery-mode attacks (docs/design/bid-delivery-spec.md) on every honest delivery shape of
//! `shapes.rs`: mode-1 fills (first, later, ending, IOC / FOK ends), top-ups (form 2) and new
//! receive UTXOs (form 1), refunds that release a bought-custody, claims (CL1 to CL3), pair
//! delivery fills and refunds, and the router's bought-custody legs.
//!
//! The attacks, by id:
//!
//! - DLV-01 custody stand-in: another order's custody, the attacker's own UTXO, a wrong amount,
//!   borrowing on, another extension
//! - DLV-02 look-alike receive state: the attacker's, a lower threshold, a non-canonical guard,
//!   another borrow scheme or extension
//! - DLV-03 cross-mode delivery: a delivery shaped for another mode or form, the order's mode or
//!   `form` argument changed
//! - DLV-04 tip over-take: one more sompi (or a carrier) out of the escrow, the delivery's KAS
//!   below its due
//! - DLV-05 claim by a non-maker: the attacker signs the claim, or a keeper refunds before it is
//!   due
//! - DLV-06 `bought` inflated or deflated in the continuation (custody unchanged, or both by one
//!   unit)
//! - DLV-07 a second top-up of the same receive UTXO by a second mode-2 order in one transaction
//! - DLV-08 the receive UTXO's successor moved off the delivery slot
//! - DLV-09 stray-guard bypass with a fake custody (an exact look-alike owned by the order, named
//!   or swept)
//! - DLV-10 a fill or claim that keeps `bought` while the custody leaves, or releases it while
//!   continuing
//! - DLV-11 a refund that drops the custody, sends it elsewhere, cuts its KAS or moves its slot
//! - DLV-12 an ending fill that leaves the custody behind
//!
//! Each attempt must be refused, or is a designed note by an explicit rule (`rt::classify`,
//! `rt::ALLOW`). Maker-signed claims attacked from the maker's own seat (DLV-10) are notes: the
//! maker's `cancel` signature authorises any outputs.
//!
//! The existing tests per attack id (hand-built scenarios, ablation, pre-sign) are mapped in
//! docs/security/redteam-coverage.md.
//!
//! Run:
//!
//! ```text
//! cargo test -p kob-tests --test redteam_delivery_tests -- --nocapture --test-threads=2
//! ```
//!
//! The pair tests are ignored by default. Run them with `--ignored`.

#[path = "../../../kob-protocol/tests/common/mod.rs"]
mod fx;
mod rt;
mod shapes;

use kob_protocol::artifacts::{template, TemplateId};
use kob_protocol::state::*;
use kob_protocol::tx::*;
use rt::*;

// ---------------------------------------------------------------- order state access

/// (mode, bought, recvThreshold, topupTip, deliveryCarrier) of a delivering kind.
fn dlv(s: &AnyState) -> Option<(i64, i64, i64, i64, i64)> {
    match s {
        AnyState::KobBid(x) => Some((x.mode, x.bought, x.recv_threshold, x.topup_tip, x.delivery_carrier)),
        AnyState::KobCondBid(x) => Some((x.mode, x.bought, x.recv_threshold, x.topup_tip, x.delivery_carrier)),
        AnyState::KobPair(x) => Some((x.mode, x.bought, x.recv_threshold, x.topup_tip, x.delivery_carrier)),
        AnyState::KobCondPair(x) => Some((x.mode, x.bought, x.recv_threshold, x.topup_tip, x.delivery_carrier)),
        _ => None,
    }
}

/// The state with `mode` and `bought` replaced (None keeps).
fn with_dlv(s: &AnyState, mode: Option<i64>, bought: Option<i64>) -> AnyState {
    let mut s = s.clone();
    macro_rules! set {
        ($x:expr) => {{
            if let Some(m) = mode {
                $x.mode = m;
            }
            if let Some(b) = bought {
                $x.bought = b;
            }
        }};
    }
    match &mut s {
        AnyState::KobBid(x) => set!(x),
        AnyState::KobCondBid(x) => set!(x),
        AnyState::KobPair(x) => set!(x),
        AnyState::KobCondPair(x) => set!(x),
        _ => {}
    }
    s
}

/// The token the order buys (delivers to its maker): a bid kind's token, a pair order's T.
fn bought_token(s: &AnyState) -> [u8; 32] {
    match s {
        AnyState::KobPair(x) => x.t_cov_id,
        AnyState::KobCondPair(x) => x.t_cov_id,
        other => other.token_cov_id(),
    }
}

fn encode(t: TemplateId, s: &AnyState) -> Option<Vec<u8>> {
    let _ = t;
    s.try_encode().ok()
}

/// Replaces the state of order input `i` (its plan and its UTXO's script).
fn set_order_in(m: &mut Mtx, i: usize, s: &AnyState) -> bool {
    let SigPlan::Entry { template: t, state, .. } = &mut m.plans[i] else { return false };
    let Some(b) = encode(*t, s) else { return false };
    m.tx.inputs[i].utxo.script_public_key = spk_to_string(&template(*t).spk(&b));
    *state = b;
    new_outpoint(m, i);
    true
}

/// The continuation of order input `i`: the output bound to its covenant id that it authorises.
fn continuation(m: &Mtx, i: usize) -> Option<usize> {
    let c = m.cov_in(i)?;
    (0..m.tx.outputs.len())
        .find(|j| m.tx.outputs[*j].covenant.as_ref().is_some_and(|x| x.covenant_id == c && x.authorizing_input as usize == i))
}

/// The decoded state of the continuation of order input `i`.
fn cont_state(m: &Mtx, i: usize) -> Option<(usize, AnyState)> {
    let j = continuation(m, i)?;
    let SigPlan::Entry { template: t, .. } = &m.plans[i] else { return None };
    let tpl = template(*t);
    let spk = &m.tx.outputs[j].script_public_key;
    // the continuation's state: the order's state with spliced windows; read it back by trying the decoded input state
    // with the fields a fill changes (the tests only rewrite `mode` / `bought`)
    let (_, s) = order_state(m, i)?;
    let (_, bought, ..) = dlv(&s)?;
    for b in candidates(bought) {
        for st in variants_of(&s, b) {
            if let Ok(bytes) = st.try_encode() {
                if &spk_to_string(&tpl.spk(&bytes)) == spk {
                    return Some((j, st));
                }
            }
        }
    }
    None
}
fn candidates(bought: i64) -> Vec<i64> {
    let mut v = vec![bought, 0];
    for n in 1..=12 {
        v.push(bought + n * fx::WHOLE);
    }
    v
}
/// Possible continuation states: the input state with `bought` = `b`, and with the fill's other mutable fields as the
/// builder writes them (amountLeft for the kinds that count it) read from the honest output by brute force.
fn variants_of(s: &AnyState, b: i64) -> Vec<AnyState> {
    let base = with_dlv(s, None, Some(b));
    let mut v = vec![base.clone()];
    match &base {
        AnyState::KobCondBid(x) => {
            for n in 0..=12 {
                v.push(AnyState::KobCondBid(CondBidState { amount_left: x.amount_left - n * fx::WHOLE, ..x.clone() }));
            }
        }
        AnyState::KobPair(x) => {
            for n in 0..=12 {
                for c in 0..=12 {
                    v.push(AnyState::KobPair(PairState {
                        amount_left: x.amount_left - n * fx::WHOLE,
                        custody: x.custody - c * fx::WHOLE,
                        ..x.clone()
                    }));
                }
            }
        }
        _ => {}
    }
    v
}

/// Rewrites the continuation of order input `i` to `s`.
fn set_cont(m: &mut Mtx, i: usize, j: usize, s: &AnyState) -> bool {
    let SigPlan::Entry { template: t, .. } = &m.plans[i] else { return false };
    let Some(b) = encode(*t, s) else { return false };
    m.tx.outputs[j].script_public_key = spk_to_string(&template(*t).spk(&b));
    true
}

/// (order input, bought-custody input) pairs: a mode-1 order holding `bought > 0` and the token input of its id with
/// exactly `bought` units.
fn custody_pairs(m: &Mtx) -> Vec<(usize, usize)> {
    let mut v = vec![];
    for i in targets(m) {
        let Some((_, s)) = order_state(m, i) else { continue };
        let Some((mode, bought, ..)) = dlv(&s) else { continue };
        if mode != MODE_ACCUMULATE || bought <= 0 {
            continue;
        }
        let Some(c) = m.cov_in(i) else { continue };
        let bt = bought_token(&s);
        for k in 0..m.plans.len() {
            if let Some((_, st)) = token_in_state(m, k) {
                if m.cov_in(k) == Some(bt) && st.is_covenant_owned() && st.owner() == c && st.amount() == bought {
                    v.push((i, k));
                }
            }
        }
    }
    v
}

/// The receive inputs (a KCC-20 leader on the borrow path) and the orders whose fill names them.
fn receive_pairs(m: &Mtx) -> Vec<(usize, usize)> {
    let mut v = vec![];
    for i in targets(m) {
        let Some((_, s)) = order_state(m, i) else { continue };
        if dlv(&s).map(|d| d.0) != Some(MODE_TOPUP) || get_int_arg(m, i, "form") != Some(2) {
            continue;
        }
        let r = ["tokIn", "tTplIn"].iter().find_map(|n| get_int_arg(m, i, n));
        if let Some(r) = r {
            if matches!(m.plans.get(r as usize), Some(SigPlan::TokenLeader { witness: Witness::Borrow, .. })) {
                v.push((i, r as usize));
            }
        }
    }
    v
}

/// The token output that is input `i`'s delivery slot (output `i`), if it is a token output.
fn delivery(m: &Mtx, i: usize) -> Option<TokOut> {
    token_outputs(m).into_iter().find(|t| t.j == i)
}

fn tok_att(id: &'static str, label: String, m: Mtx) -> Attempt {
    let mut a = Attempt::new(id, label, m);
    a.token_change = true;
    a
}

// ---------------------------------------------------------------- the attacks

/// DLV-01: the bought-custody input replaced by a stand-in; the outputs as the honest fill builds them.
fn dlv01(h: &Mtx) -> Vec<Attempt> {
    let mut v = vec![];
    // two orders with custodies in one transaction: each names the other's custody (both orders are spent, so the
    // token program authorises both custodies)
    let pairs = custody_pairs(h);
    if pairs.len() >= 2 && pairs[0].0 != pairs[1].0 {
        let ((i1, k1), (i2, k2)) = (pairs[0], pairs[1]);
        let mut p = h.clone();
        let mut ok = false;
        for (i, k) in [(i1, k2), (i2, k1)] {
            for arg in ["tokIn", "tTplIn"] {
                if get_int_arg(&p, i, arg).is_some() {
                    ok |= set_int_arg(&mut p, i, arg, k as i64);
                }
            }
        }
        if ok {
            v.push(tok_att("DLV-01", format!("orders {i1} and {i2} name each other's custody"), p));
        }
    }
    for (i, k) in custody_pairs(h) {
        let (program, st) = token_in_state(h, k).unwrap();
        let mut stand = |label: &str, new: TokenState, witness: Option<Witness>| {
            let mut p = h.clone();
            set_token_in_state(&mut p, k, new.clone());
            if let Some(w) = witness {
                match &mut p.plans[k] {
                    SigPlan::TokenLeader { witness, .. } | SigPlan::TokenDelegator { witness, .. } => *witness = w,
                    _ => {}
                }
                p.sigs[k] = SigSrc::Fresh;
            }
            let mut a = tok_att("DLV-01", format!("order {i} ({}): {label}", program.name()), p);
            a.must_reach_target = true;
            v.push(a);
        };
        let other = TokenState::custody(st.family(), st.amount(), [0xb9; 32], st.extension());
        stand("another order's custody of the same amount", other, None);
        let own = st.with_holder_owner(program, attacker());
        stand("the attacker's own UTXO of the same amount", own, Some(Witness::P2pk(attacker())));
        if st.as_kcc20().is_some() {
            stand("the custody with borrowing on", st.clone().with_borrow((1, [0; 32])), None);
            if let TokenState::Kcc20(x) = &st {
                stand(
                    "the custody of another extension",
                    TokenState::Kcc20(Kcc20State { extension_commitment: [0xdd; 32], ..x.clone() }),
                    None,
                );
                stand("the order's id under owner scheme 0", TokenState::Kcc20(Kcc20State { owner_scheme: 0, ..x.clone() }), None);
            }
        }
        // a custody of one unit more (the extra unit to the attacker) and of one unit less (the continuation follows)
        let mut p = h.clone();
        set_token_in_state(&mut p, k, st.with_amount(st.amount() + 1));
        if give_attacker_tokens(&mut p, h.cov_in(k).unwrap(), 1, &st) {
            let mut a = tok_att("DLV-01", format!("order {i}: a custody of bought + 1, the unit to the attacker"), p);
            a.must_reach_target = true;
            v.push(a);
        }
        let mut p = h.clone();
        set_token_in_state(&mut p, k, st.with_amount(st.amount() - 1));
        if let Some(d) = token_outputs(h).into_iter().find(|t| t.state.owner() == st.owner() || t.j == i) {
            set_token_out(&mut p, &d, d.state.with_amount(d.state.amount() - 1));
            let mut a = tok_att("DLV-01", format!("order {i}: a custody of bought - 1, the delivery one unit short"), p);
            a.must_reach_target = true;
            v.push(a);
        }
    }
    v
}

/// DLV-02: the receive UTXO replaced by a look-alike (its successor follows).
fn dlv02(h: &Mtx) -> Vec<Attempt> {
    let mut v = vec![];
    for (i, r) in receive_pairs(h) {
        let (program, st) = token_in_state(h, r).unwrap();
        let TokenState::Kcc20(x) = &st else { continue };
        let Some(d) = token_outputs(h).into_iter().find(|t| t.leader == r && t.pos == 0) else { continue };
        let TokenState::Kcc20(dx) = &d.state else { continue };
        let thr = i64::from_le_bytes(x.borrow_guard[..8].try_into().unwrap());
        let mut look = |label: &str, f: &dyn Fn(&Kcc20State) -> Kcc20State| {
            let mut p = h.clone();
            set_token_in_state(&mut p, r, TokenState::Kcc20(f(x)));
            set_token_out(&mut p, &d, TokenState::Kcc20(f(dx)));
            let mut a = tok_att("DLV-02", format!("order {i} ({}): {label}", program.name()), p);
            a.must_reach_target = true;
            v.push(a);
        };
        let attacker_pk = attacker();
        look("the attacker's receive UTXO", &|s| Kcc20State { owner: attacker_pk, ..s.clone() });
        let low = {
            let mut g = [0u8; 32];
            g[..8].copy_from_slice(&(thr - 1).to_le_bytes());
            g
        };
        look("a threshold one below recvThreshold", &|s| Kcc20State { borrow_guard: low, ..s.clone() });
        let mut nc = x.borrow_guard;
        nc[8] = 1;
        look("a non-canonical guard", &|s| Kcc20State { borrow_guard: nc, ..s.clone() });
        look("another extension commitment", &|s| Kcc20State { extension_commitment: [0xdd; 32], ..s.clone() });
        look("owner scheme 4 (a covenant-owned look-alike)", &|s| Kcc20State { owner_scheme: 4, ..s.clone() });
        for scheme in [2u8, 3] {
            look(&format!("borrow scheme {scheme}"), &|s| Kcc20State { borrow_scheme: scheme, ..s.clone() });
        }
    }
    v
}

/// DLV-03: deliveries shaped for another mode or form.
fn dlv03(h: &Mtx) -> Vec<Attempt> {
    let mut v = vec![];
    for i in targets(h) {
        let Some((_, s)) = order_state(h, i) else { continue };
        let Some((mode, bought, thr, ..)) = dlv(&s) else { continue };
        let Some(c) = h.cov_in(i) else { continue };
        let Some(d) = delivery(h, i) else { continue };
        let maker = s.maker();
        let fam = d.state.family();
        if fam != kob_protocol::Family::Kcc20 {
            continue;
        }
        let ext = d.state.extension();
        let n = d.state.amount();
        let custody = TokenState::custody(fam, n, c, ext);
        let plain = TokenState::user(d.program, n, maker, ext);
        let recv = plain.clone().with_borrow(kob_protocol::build::receive_borrow(thr.max(1)));
        let mut shape = |label: String, st: TokenState| {
            if st == d.state {
                return;
            }
            let mut p = h.clone();
            set_token_out(&mut p, &d, st);
            v.push(tok_att("DLV-03", format!("order {i} mode {mode}: delivery as {label}"), p));
        };
        shape("a custody of the order (mode 1)".into(), custody.clone());
        shape("a plain UTXO of the maker (mode 0)".into(), plain.clone());
        shape("a new receive UTXO (form 1)".into(), recv);
        // the order's mode changed (input and continuation), the transaction as built
        for m2 in [0i64, 1, 2, 3] {
            if m2 == mode {
                continue;
            }
            let mut p = h.clone();
            let ns = with_dlv(&s, Some(m2), None);
            if !set_order_in(&mut p, i, &ns) {
                continue;
            }
            if let Some((j, cs)) = cont_state(h, i) {
                set_cont(&mut p, i, j, &with_dlv(&cs, Some(m2), None));
            }
            let mut a = tok_att("DLV-03", format!("order {i}: mode {mode} -> {m2}, transaction as built"), p);
            // the spec's delivery is the same in both modes here (bid-delivery-spec section 4): a plain UTXO of n to the
            // maker in mode 0, in mode 1 when the order ends holding nothing, in mode 2 form 0
            let ends = continuation(h, i).is_none();
            let form = get_int_arg(h, i, "form").unwrap_or(0);
            let plain_now = d.state == plain && bought == 0;
            let plain_then = match m2 {
                0 => true,
                1 => ends,
                2 => form == 0,
                _ => false,
            };
            if is_refund(h, i) {
                a.benign = Some(format!(
                    "a refund (mode {m2}) releases the escrow and the bought-custody to the maker whatever the mode, outputs as built; a mode other than 1 with bought > 0 is a state no placement or fill reaches"
                ));
            } else if plain_now && plain_then {
                a.benign =
                    Some("same delivery in both modes: a plain UTXO of n to the maker (bid-delivery-spec section 4)".to_string());
            }
            v.push(a);
        }
        // the form argument changed
        if let Some(f) = get_int_arg(h, i, "form") {
            for f2 in [0i64, 1, 2, 3, -1] {
                if f2 == f {
                    continue;
                }
                let mut p = h.clone();
                set_int_arg(&mut p, i, "form", f2);
                let mut a = tok_att("DLV-03", format!("order {i} mode {mode}: form {f} -> {f2}"), p);
                if mode != MODE_TOPUP {
                    a.benign = Some("form is read in mode 2 only (bid-delivery-spec section 3)".into());
                }
                v.push(a);
            }
        }
    }
    v
}

/// DLV-04: more KAS out of the escrow than the fill releases, or less on the delivery.
fn dlv04(h: &Mtx) -> Vec<Attempt> {
    let mut v = vec![];
    for i in targets(h) {
        let Some((_, s)) = order_state(h, i) else { continue };
        let Some((mode, _, _, tip, carrier)) = dlv(&s) else { continue };
        if mode == 0 && get_int_arg(h, i, "form").is_none() {
            continue;
        }
        let deltas = [1u64, tip.max(1) as u64, carrier.max(1) as u64];
        if let Some(j) = continuation(h, i) {
            for dlt in deltas {
                if h.tx.outputs[j].value <= dlt {
                    continue;
                }
                let mut p = h.clone();
                p.tx.outputs[j].value -= dlt;
                give_attacker_kas(&mut p, dlt);
                let mut a = Attempt::new("DLV-04", format!("order {i} mode {mode}: {dlt} sompi more out of the escrow"), p);
                a.gain = dlt;
                v.push(a);
            }
        }
        if let Some(d) = delivery(h, i) {
            for dlt in deltas {
                if h.tx.outputs[d.j].value <= dlt + 1 {
                    continue;
                }
                let mut p = h.clone();
                p.tx.outputs[d.j].value -= dlt;
                give_attacker_kas(&mut p, dlt);
                let mut a = Attempt::new("DLV-04", format!("order {i} mode {mode}: the delivery's KAS {dlt} sompi short"), p);
                a.gain = dlt;
                v.push(a);
            }
        }
    }
    v
}

/// DLV-05: a claim signed by a non-maker (the custody to the attacker), and a keeper's refund before it is due.
fn dlv05(h: &Mtx) -> Vec<Attempt> {
    let mut v = vec![];
    let claim = targets(h).into_iter().any(|i| entry_name(h, i) == "cancel") && !custody_inputs(h).is_empty();
    if claim {
        for i in targets(h) {
            if entry_name(h, i) != "cancel" {
                continue;
            }
            let mut p = h.clone();
            if let SigPlan::Entry { args, .. } = &mut p.plans[i] {
                for a in args.iter_mut() {
                    if let Arg::Sig(_) = a {
                        *a = Arg::Sig(attacker());
                    }
                }
            }
            p.sigs[i] = SigSrc::Fresh;
            // every key-owned token output and plain output of the maker to the attacker
            let maker = order_state(h, i).map(|(_, s)| s.maker());
            for t in token_outputs(&p.clone()) {
                if t.state.is_user() && Some(t.state.owner()) == maker {
                    set_token_out(&mut p, &t, t.state.with_holder_owner(t.program, attacker()));
                }
            }
            v.push(tok_att("DLV-05", format!("claim of order {i} signed by the attacker, the tokens to the attacker"), p.clone()));
            let mut q = h.clone();
            q.plans[i] = p.plans[i].clone();
            q.sigs[i] = SigSrc::Fresh;
            v.push(tok_att("DLV-05", format!("claim of order {i} signed by the attacker, outputs as built"), q));
        }
    }
    // refunds of an order holding its custody, before they are due
    for i in targets(h) {
        if entry_name(h, i) == "refund" && !custody_pairs(h).is_empty() {
            for lt in [0u64, h.tx.lock_time.saturating_sub(1_000), h.tx.lock_time.saturating_sub(1)] {
                let mut p = h.clone();
                p.tx.lock_time = lt;
                v.push(tok_att("DLV-05", format!("refund of order {i} at lock time {lt} (built at {})", h.tx.lock_time), p));
            }
        }
    }
    v
}

/// A refund: the `refund` entry, or a pair order's zero fill (`settle` with `nb = 0`, bid-delivery-spec section 5).
fn is_refund(m: &Mtx, i: usize) -> bool {
    if entry_name(m, i) == "refund" {
        return true;
    }
    let (Some(k), SigPlan::Entry { args, .. }) = (arg_index(m, i, "nb"), &m.plans[i]) else { return false };
    matches!(args.get(k), Some(Arg::Bytes(b)) if b.iter().all(|x| *x == 0))
}

/// Token inputs owned by a target's id.
fn custody_inputs(m: &Mtx) -> Vec<usize> {
    let ids: Vec<[u8; 32]> = targets(m).into_iter().filter_map(|i| m.cov_in(i)).collect();
    (0..m.plans.len())
        .filter(|k| token_in_state(m, *k).is_some_and(|(_, s)| s.is_covenant_owned() && ids.contains(&s.owner())))
        .collect()
}

/// DLV-06: `bought` inflated or deflated in the continuation.
fn dlv06(h: &Mtx) -> Vec<Attempt> {
    let mut v = vec![];
    for i in targets(h) {
        let Some((_, s)) = order_state(h, i) else { continue };
        let Some((mode, ..)) = dlv(&s) else { continue };
        let Some((j, cs)) = cont_state(h, i) else { continue };
        let Some((_, b, ..)) = dlv(&cs) else { continue };
        for (label, nb) in [("bought + 1", b + 1), ("bought + 1000 units", b + 1_000), ("bought - 1", b - 1), ("bought = 0", 0)] {
            if nb == b || nb < 0 {
                continue;
            }
            let mut p = h.clone();
            if !set_cont(&mut p, i, j, &with_dlv(&cs, None, Some(nb))) {
                continue;
            }
            v.push(tok_att("DLV-06", format!("order {i} mode {mode}: continuation {label}, custody unchanged"), p));
        }
        // both the continuation and its custody one unit more (the taker pays the unit)
        if mode == MODE_ACCUMULATE {
            if let Some(d) = delivery(h, i).filter(|d| d.state.is_covenant_owned()) {
                let mut p = h.clone();
                if set_cont(&mut p, i, j, &with_dlv(&cs, None, Some(b + 1))) {
                    set_token_out(&mut p, &d, d.state.with_amount(d.state.amount() + 1));
                    // the extra unit from the seat's token change, if there is one
                    if let Some(t) =
                        token_outputs(&p).into_iter().find(|t| t.token == d.token && seat_output(&p, t.j) && t.state.amount() > 1)
                    {
                        set_token_out(&mut p, &t, t.state.with_amount(t.state.amount() - 1));
                    }
                    v.push(tok_att("DLV-06", format!("order {i}: continuation and custody both one unit more"), p));
                }
            }
        }
    }
    v
}

/// DLV-07: a second mode-2 order tops up the same receive UTXO (built from a two-bid top-up match, see `second_topup`).
fn dlv07(h: &Mtx) -> Vec<Attempt> {
    let mut v = vec![];
    let rp = receive_pairs(h);
    let Some(&(i1, r)) = rp.first() else { return v };
    // the other mode-2 order of the transaction that delivers a new receive UTXO (form 1)
    for i2 in targets(h) {
        if i2 == i1 || get_int_arg(h, i2, "form") != Some(1) {
            continue;
        }
        let Some(d2) = delivery(h, i2) else { continue };
        let (_, rs) = token_in_state(h, r).unwrap();
        let mut p = h.clone();
        set_int_arg(&mut p, i2, "form", 2);
        set_int_arg(&mut p, i2, "tokIn", r as i64);
        // its delivery: the receive UTXO's state topped up by its fill, with the receive UTXO's KAS
        let n2 = d2.state.amount();
        set_token_out(&mut p, &d2, rs.with_amount(rs.amount() + n2));
        p.tx.outputs[d2.j].value = p.tx.outputs[d2.j].value.max(h.tx.inputs[r].utxo.amount);
        // the extra units from the seat's token input
        if let Some(k) = (0..p.plans.len()).find(|k| {
            token_in_state(&p, *k).is_some_and(|(_, s)| s.is_user() && seat_pks().contains(&s.owner())) && p.cov_in(*k) == p.cov_in(r)
        }) {
            let (_, ts) = token_in_state(&p, k).unwrap();
            set_token_in_state(&mut p, k, ts.with_amount(ts.amount() + rs.amount()));
        }
        // the escrow releases the tip, not the carrier
        if let (Some(j2), Some((_, s2))) = (continuation(h, i2), order_state(h, i2)) {
            let (_, _, _, tip, carrier) = dlv(&s2).unwrap();
            p.tx.outputs[j2].value += (carrier - tip).max(0) as u64;
        }
        fund_deficit(&mut p);
        let mut a = tok_att("DLV-07", format!("orders {i1} and {i2} both top up the receive UTXO at input {r}"), p);
        a.must_reach_target = false;
        v.push(a);
    }
    v
}

/// DLV-08: the receive UTXO's successor moved off the delivery slot (swapped with the next output).
fn dlv08(h: &Mtx) -> Vec<Attempt> {
    let mut v = vec![];
    for (i, r) in receive_pairs(h) {
        let Some(d) = token_outputs(h).into_iter().find(|t| t.leader == r && t.pos == 0) else { continue };
        let j2 = d.j + 1;
        if j2 >= h.tx.outputs.len() {
            continue;
        }
        let mut p = h.clone();
        p.tx.outputs.swap(d.j, j2);
        v.push(tok_att("DLV-08", format!("order {i}: the receive successor at output {j2}, output {j2} at {}", d.j), p));
    }
    v
}

/// DLV-09: an exact look-alike of the bought-custody owned by the order, next to the real one: the fill names the real
/// one and the look-alike goes to the attacker, or the fill names the look-alike and the real one goes to the attacker.
fn dlv09(h: &Mtx) -> Vec<Attempt> {
    let mut v = vec![];
    for (i, k) in custody_pairs(h) {
        let (program, st) = token_in_state(h, k).unwrap();
        let token = h.cov_in(k).unwrap();
        let value = h.tx.inputs[k].utxo.amount;
        for name_fake in [false, true] {
            let mut p = h.clone();
            let f = add_token_input(&mut p, 0xf7, program, token, st.clone(), value);
            if name_fake {
                for arg in ["tokIn", "tTplIn", "custIn"] {
                    if get_int_arg(&p, i, arg) == Some(k as i64) {
                        set_int_arg(&mut p, i, arg, f as i64);
                    }
                }
            }
            if !give_attacker_tokens(&mut p, token, st.amount(), &st) {
                continue;
            }
            let mut a = tok_att(
                "DLV-09",
                format!(
                    "order {i}: a look-alike custody at input {f}, the fill names the {}",
                    if name_fake { "look-alike" } else { "real one" }
                ),
                p,
            );
            a.must_reach_target = true;
            v.push(a);
        }
    }
    v
}

/// DLV-10: a fill that releases the custody to the maker while continuing with `bought`, and a maker-signed claim that
/// keeps `bought` (attacked from the maker's own seat: a note when accepted).
fn dlv10(h: &Mtx) -> Vec<Attempt> {
    let mut v = vec![];
    for i in targets(h) {
        let Some((_, s)) = order_state(h, i) else { continue };
        let Some((mode, ..)) = dlv(&s) else { continue };
        if mode != MODE_ACCUMULATE {
            continue;
        }
        let Some((j, cs)) = cont_state(h, i) else { continue };
        let Some((_, b, ..)) = dlv(&cs) else { continue };
        let maker = s.maker();
        if entry_name(h, i) == "cancel" {
            // the claim continues with bought = 0 (CL1): keep the old bought instead, from the maker's seat
            let (_, ib, ..) = dlv(&s).unwrap();
            let mut p = h.clone();
            if set_cont(&mut p, i, j, &with_dlv(&cs, None, Some(ib))) {
                p.sigs[i] = SigSrc::Fresh;
                let mut a = tok_att("DLV-10", format!("claim of order {i} keeps bought = {ib} (maker's seat)"), p);
                a.benign = Some(format!("signed-by-maker: {}", ALLOW[1].1));
                v.push(a);
            }
            continue;
        }
        // a fill: the custody delivered to the maker as a plain UTXO while the continuation keeps `bought`
        if let Some(d) = delivery(h, i).filter(|d| d.state.is_covenant_owned()) {
            let mut p = h.clone();
            set_token_out(&mut p, &d, d.state.with_holder_owner(d.program, maker));
            v.push(tok_att("DLV-10", format!("order {i}: the custody to the maker, the continuation keeps bought = {b}"), p));
        }
    }
    v
}

/// DLV-11: refunds of an order with a bought-custody: the custody dropped, sent to the attacker, its KAS cut, its slot moved.
fn dlv11(h: &Mtx) -> Vec<Attempt> {
    let mut v = vec![];
    for i in targets(h) {
        if entry_name(h, i) != "refund" {
            continue;
        }
        for (oi, k) in custody_pairs(h) {
            if oi != i {
                continue;
            }
            let rel = token_outputs(h)
                .into_iter()
                .find(|t| t.state.is_user() && t.state.amount() == token_in_state(h, k).unwrap().1.amount());
            // dropped: the custody input and its release removed (the release's KAS stays as fee)
            let mut p = h.clone();
            if let Some(rel) = &rel {
                p.tx.outputs.remove(rel.j);
            }
            p.remove_input(k);
            for arg in ["custIn"] {
                if get_int_arg(&p, i, arg).is_some() {
                    set_int_arg(&mut p, i, arg, 0);
                }
            }
            v.push(tok_att("DLV-11", format!("refund of order {i} without its custody (left behind)"), p));
            let Some(rel) = rel else { continue };
            let mut p = h.clone();
            set_token_out(&mut p, &rel, rel.state.with_holder_owner(rel.program, attacker()));
            v.push(tok_att("DLV-11", format!("refund of order {i}: the custody to the attacker"), p));
            let mut p = h.clone();
            let kas = p.tx.outputs[rel.j].value / 2;
            p.tx.outputs[rel.j].value -= kas;
            give_attacker_kas(&mut p, kas);
            let mut a = Attempt::new("DLV-11", format!("refund of order {i}: the custody's release with half its KAS"), p);
            a.gain = kas;
            v.push(a);
            for other in [0usize, h.tx.outputs.len() - 1] {
                if other == rel.j {
                    continue;
                }
                let mut p = h.clone();
                p.tx.outputs.swap(rel.j, other);
                v.push(tok_att("DLV-11", format!("refund of order {i}: the custody's release at output {other}"), p));
            }
            // a custody of another order standing in
            let mut p = h.clone();
            let (_, st) = token_in_state(h, k).unwrap();
            set_token_in_state(&mut p, k, TokenState::custody(st.family(), st.amount(), [0xb9; 32], st.extension()));
            let mut a = tok_att("DLV-11", format!("refund of order {i} with another order's custody"), p);
            a.must_reach_target = true;
            v.push(a);
        }
    }
    v
}

/// DLV-12: an ending fill (no continuation) of an order holding its custody that leaves the custody behind: the
/// custody input removed, the delivery only `n` (from the taker).
fn dlv12(h: &Mtx) -> Vec<Attempt> {
    let mut v = vec![];
    for (i, k) in custody_pairs(h) {
        if continuation(h, i).is_some() || entry_name(h, i) == "cancel" || entry_name(h, i) == "refund" {
            continue;
        }
        let Some(d) = delivery(h, i) else { continue };
        let (_, st) = token_in_state(h, k).unwrap();
        let mut p = h.clone();
        p.remove_input(k);
        let tokin = ["tokIn", "tTplIn"].iter().find(|n| get_int_arg(&p, i, n).is_some()).copied();
        // the token's leader moved: name the first remaining token input
        let Some(lead) = (0..p.plans.len()).find(|x| p.cov_in(*x) == h.cov_in(k)) else { continue };
        if let SigPlan::TokenDelegator { template: t, state, witness } = p.plans[lead].clone() {
            let outs: Vec<Kcc20State> =
                token_outputs(h).into_iter().filter(|o| o.token == d.token).map(|o| o.state.as_kcc20().cloned().unwrap()).collect();
            p.plans[lead] = SigPlan::TokenLeader { template: t, state, next_states: outs, witness };
            for o in p.tx.outputs.iter_mut() {
                if let Some(c) = o.covenant.as_mut() {
                    if c.covenant_id == d.token {
                        c.authorizing_input = lead as u16;
                    }
                }
            }
        }
        if let Some(n) = tokin {
            set_int_arg(&mut p, i, n, lead as i64);
        }
        let dd = token_outputs(&p).into_iter().find(|t| t.j == d.j);
        if let Some(dd) = dd {
            set_token_out(&mut p, &dd, dd.state.with_amount(dd.state.amount() - st.amount()));
        }
        fund_deficit(&mut p);
        let mut a = tok_att("DLV-12", format!("order {i}: ends, its custody of {} left behind", st.amount()), p);
        a.must_reach_target = false;
        v.push(a);
    }
    v
}

fn delivery_attempts(h: &Mtx) -> Vec<Attempt> {
    let mut v = vec![];
    v.extend(dlv01(h));
    v.extend(dlv02(h));
    v.extend(dlv03(h));
    v.extend(dlv04(h));
    v.extend(dlv05(h));
    v.extend(dlv06(h));
    v.extend(dlv07(h));
    v.extend(dlv08(h));
    v.extend(dlv09(h));
    v.extend(dlv10(h));
    v.extend(dlv11(h));
    v.extend(dlv12(h));
    v
}

/// Shapes with a delivering order (a delivery block in its state).
fn is_delivery_shape(h: &Mtx) -> bool {
    targets(h).into_iter().any(|i| order_state(h, i).and_then(|(_, s)| dlv(&s)).is_some_and(|d| d.0 != 0 || d.1 != 0))
}

fn run(title: &str, shapes: Vec<shapes::Shape>, only_delivery: bool) -> Run {
    let mut r = Run::default();
    for (name, b) in shapes {
        let h = honest(&name, &b);
        if only_delivery && !is_delivery_shape(&h) {
            continue;
        }
        let a = delivery_attempts(&h);
        r.attack(&name, &h, a);
    }
    r.report(title);
    r
}

/// Two mode-2 bids of one maker against the taker: the first tops up the maker's receive UTXO, the second delivers a new
/// receive UTXO (the shape DLV-07 turns into a second top-up).
fn second_topup_shapes() -> Vec<shapes::Shape> {
    use fx::*;
    use kob_protocol::build::*;
    let mut v = vec![];
    for p in PROGRAMS.into_iter().filter(|p| kob_protocol::budget::topup_extra(*p).is_some()) {
        fx::with_carriers(Carriers::order_side(kob_protocol::defaults::DEFAULT_ORDER_CARRIER), || {
            let m2 = mode_bid(bid(MAKER_B, P250, p), MODE_TOPUP, 0);
            let m2b = mode_bid(bid(MAKER_B, P245, p), MODE_TOPUP, 0);
            let recv = recv_tok(24, 2 * WHOLE, pk(MAKER_B), RECV_T, 1_500);
            let mut b = take_bid_valued(m2.clone(), 3, 0, None);
            b.legs = vec![
                Leg::Bid {
                    order: order(20, m2.escrow(10 * WHOLE, 3).unwrap() as u64, cov(0xb1), 1_000, m2.clone()),
                    amount: 3 * WHOLE,
                    t: None,
                    delivery: bid_dlv(None, Some(recv), None),
                },
                Leg::Bid {
                    order: order(21, m2b.escrow(10 * WHOLE, 3).unwrap() as u64, cov(0xb2), 1_000, m2b),
                    amount: 2 * WHOLE,
                    t: None,
                    delivery: bid_dlv(None, None, Some(1)),
                },
            ];
            b.taker_tokens = vec![tok(22, 7 * WHOLE, pk(TAKER), SCHEME_P2PK, 1_000)];
            b.change = Some(pk(TAKER));
            // the shape has no measured budget role (leader.borrow.i2.o3): any budget, the judge meters without limit
            let any = |r: &str| kob_protocol::budget::lookup(r).or(Ok(400));
            match build_with(&Action::Batch(b), &any) {
                Ok(x) => v.push((format!("{}/take.sell.twoBids.topup", p.name()), x)),
                Err(e) => println!("{}: the two-bid top-up shape does not build: {e}", p.name()),
            }
        });
    }
    v
}

macro_rules! program_test {
    ($name:ident, $p:expr) => {
        #[test]
        fn $name() {
            let p: TemplateId = $p;
            let r = run(&format!("delivery/{}", p.name()), shapes::program_shapes(p), true);
            r.assert_clean();
        }
    };
}

program_test!(delivery_kcc20ref, TemplateId::Kcc20Ref);
program_test!(delivery_kcc20ref_4x5, TemplateId::Kcc20Ref4x5);
program_test!(delivery_kcc20ref_8x8, TemplateId::Kcc20Ref8x8);
program_test!(delivery_kcc20ref_16x16, TemplateId::Kcc20Ref16x16);
program_test!(delivery_kcc20p2, TemplateId::Kcc20P2);
program_test!(delivery_kcc20kaspacom, TemplateId::Kcc20KaspaCom025);
program_test!(delivery_kcc20publicmint, TemplateId::Kcc20PublicMint);
program_test!(delivery_kforge7214, TemplateId::KForgeToken7214);
program_test!(delivery_kaspa_rocket, TemplateId::KaspaRocketToken11458);

macro_rules! pair_test {
    ($name:ident, $i:expr) => {
        #[test]
        #[ignore = "heavy (minutes per program pair): cargo test -p kob-tests --test redteam_delivery_tests -- --ignored"]
        fn $name() {
            let (pa, pb) = shapes::PAIRS[$i];
            let r = run(&format!("delivery/pair/{}", fx::pair::pair_name(pa, pb)), shapes::pair_shapes(pa, pb), true);
            r.assert_clean();
        }
    };
}

pair_test!(delivery_pair_0, 0);
pair_test!(delivery_pair_1, 1);
pair_test!(delivery_pair_2, 2);
pair_test!(delivery_pair_3, 3);
pair_test!(delivery_pair_4, 4);
pair_test!(delivery_pair_5, 5);
pair_test!(delivery_pair_6, 6);
pair_test!(delivery_pair_7, 7);

#[test]
fn delivery_claims() {
    let r = run("delivery/claims", shapes::claim_shapes(), false);
    r.assert_clean();
}

#[test]
fn delivery_router() {
    let r = run("delivery/router", shapes::router_shapes(), true);
    r.assert_clean();
}

/// DLV-07 (docs/security/redteam-coverage.md): a second mode-2 bid names the receive UTXO another bid tops up and delivers a
/// copy of its state, not its successor. Refused by that bid since a top-up's delivery must be the receive UTXO's successor
/// (`OpAuthOutputIdx(tokIn, 0) == i`); ablation: kcc20-sell `DM-31` (scenario NO07).
#[test]
fn delivery_second_topup() {
    let s = second_topup_shapes();
    assert!(!s.is_empty(), "the two-bid top-up shape builds");
    let n = s.len() as u64;
    let r = run("delivery/second-topup", s, false);
    r.assert_clean();
    let c: Vec<&Cell> = r.cells.iter().filter(|((a, _, _), _)| a == "DLV-07").map(|(_, c)| c).collect();
    let (judged, by_target) = (c.iter().map(|c| c.judged).sum::<u64>(), c.iter().map(|c| c.by_target).sum::<u64>());
    assert!(judged >= n && by_target == judged, "DLV-07: {judged} judged, {by_target} refused by an order (one per shape: {n})");
}

/// Prints every attempt on the shapes whose name contains `RT_DESCRIBE` with its verdict (all failing inputs).
#[test]
#[ignore = "debug: RT_DESCRIBE=<name part> RT_ATTACK=<id part> cargo test --test redteam_delivery_tests explain -- --ignored --nocapture"]
fn explain() {
    let pat = std::env::var("RT_DESCRIBE").unwrap_or_default();
    let att = std::env::var("RT_ATTACK").unwrap_or_default();
    let mut all = shapes::program_shapes(TemplateId::Kcc20Ref);
    all.extend(shapes::pair_shapes(TemplateId::Kcc20Ref, TemplateId::Kcc20Ref));
    all.extend(shapes::router_shapes());
    for (n, b) in all.iter().filter(|(n, _)| n.contains(&pat)) {
        let h = honest(n, b);
        println!("{n}\n{}", describe(&h));
        let mut a = delivery_attempts(&h);
        a.extend(library_attempts(&h));
        for x in a.into_iter().filter(|x| x.id.contains(&att)) {
            let v = judge(&x.m);
            let detail = match &v {
                Verdict::Refused { inputs, rule } => format!(
                    "{rule:?} {}",
                    inputs
                        .iter()
                        .map(|(i, e)| format!("[{i} {}: {}]", role_name(&x.m, *i), e.chars().take(140).collect::<String>()))
                        .collect::<Vec<_>>()
                        .join(" ")
                ),
                other => other.short(),
            };
            println!("  {} {} -> {detail}", x.id, x.label);
        }
    }
}
