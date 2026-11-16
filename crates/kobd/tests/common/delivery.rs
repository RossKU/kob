#![allow(dead_code)]
//! Bid delivery fixtures (`docs/design/bid-delivery-spec.md`): the transactions of a mode-1 or mode-2 bid, built with the protocol
//! builders as a mode-0 fill, refund or cancel and patched to the delivery-mode shape of spec sections 4, 5 and 7 (the outputs and
//! states the covenant pins, the KAS each part carries, and the entry arguments). The indexer runs no script, so a patched
//! transaction is signed again without engine validation; what the indexer reads of it is exactly what a valid one shows.
//!
//! Included with `#[path = "common/delivery.rs"] mod delivery;` next to `mod common;`.

use crate::common::*;
use kob_protocol::artifacts::{token_template, TemplateId};
use kob_protocol::build::*;
use kob_protocol::state::*;
use kob_protocol::tx::*;
use kobd::hex::Hash32;
use kobd::testkit::*;

/// The script public key of a fixture token state (`T3`), as the transaction JSON writes it.
pub fn tok_spk(s: &TokenState) -> String {
    spk_to_string(&s.spk_with(token_template(T3)))
}

/// A plain token state of `n` base units for key `k`, as a mode-0 delivery is.
pub fn plain(n: i64, k: u8) -> TokenState {
    TokenState::user(T3, n, pk(k), EXT)
}

/// The bought-custody of order `cov`: `n` base units owned by the order id, borrowing disabled.
pub fn bought(n: i64, cov: &Hash32) -> TokenState {
    TokenState::custody(kob_protocol::family::Family::Kcc20, n, cov.0, EXT)
}

/// The guard of a receive UTXO: the threshold as 8 bytes little-endian, then 24 zero bytes.
pub fn thr_guard(t: i64) -> [u8; 32] {
    let mut g = [0u8; 32];
    g[..8].copy_from_slice(&t.to_le_bytes());
    g
}

/// A receive UTXO state of key `k` (owner scheme 0x00, borrow scheme 0x01, guard = threshold `t`).
pub fn receive(n: i64, k: u8, t: i64) -> TokenState {
    TokenState::Kcc20(Kcc20State {
        amount: n,
        owner: pk(k),
        owner_scheme: SCHEME_P2PK,
        borrow_scheme: 1,
        borrow_guard: thr_guard(t),
        extension_commitment: EXT,
    })
}

/// Index of the output with script public key `spk`.
pub fn out_at(built: &BuiltTx, spk: &str) -> usize {
    built.tx.outputs.iter().position(|o| o.script_public_key == spk).unwrap_or_else(|| panic!("no output {spk}"))
}

/// Re-targets the token output holding `from` to `to`, in the outputs and in the leader's next states.
pub fn retarget(built: &mut BuiltTx, from: &TokenState, to: &TokenState) -> usize {
    let j = out_at(built, &tok_spk(from));
    built.tx.outputs[j].script_public_key = tok_spk(to);
    let (TokenState::Kcc20(f), TokenState::Kcc20(t)) = (from, to) else { panic!("KCC-20 fixtures") };
    let mut patched = false;
    for p in built.plans.iter_mut() {
        if let SigPlan::TokenLeader { next_states, .. } = p {
            for s in next_states.iter_mut().filter(|s| *s == f) {
                *s = t.clone();
                patched = true;
            }
        }
    }
    assert!(patched, "the leader authorises the delivery");
    j
}

/// The input of the token leader.
pub fn leader(built: &BuiltTx) -> usize {
    built.plans.iter().position(|p| matches!(p, SigPlan::TokenLeader { .. })).expect("a token leader")
}

/// Adds a token output of state `s` holding `value` sompi, authorised by the leader; returns its index.
pub fn add_token_output(built: &mut BuiltTx, s: &TokenState, value: u64) -> usize {
    let l = leader(built);
    if let (SigPlan::TokenLeader { next_states, .. }, TokenState::Kcc20(k)) = (&mut built.plans[l], s) {
        next_states.push(k.clone());
    }
    built.tx.outputs.push(TxOutputJson {
        value,
        script_public_key: tok_spk(s),
        covenant: Some(CovenantJson { authorizing_input: l as u16, covenant_id: TOKEN_COV }),
    });
    built.tx.outputs.len() - 1
}

/// Adds a token input (`TokenDelegator`, authorised by the covenant id of its owner); returns its index.
pub fn add_token_input(built: &mut BuiltTx, u: &TokenUtxo) -> usize {
    let TokenState::Kcc20(k) = &u.state else { panic!("KCC-20 fixtures") };
    built.tx.inputs.push(TxInputJson {
        transaction_id: u.utxo.transaction_id,
        index: u.utxo.index,
        sequence: 0,
        sig_op_count: 0,
        compute_budget: 0,
        signature_script: vec![],
        utxo: UtxoJson {
            address: None,
            amount: u.utxo.amount,
            script_public_key: tok_spk(&u.state),
            block_daa_score: u.utxo.block_daa_score,
            is_coinbase: false,
            covenant_id: Some(TOKEN_COV),
        },
    });
    built.plans.push(SigPlan::TokenDelegator { template: T3, state: k.clone(), witness: Witness::CovenantId });
    built.roles.push("test.token.delegator".into());
    built.tx.inputs.len() - 1
}

/// The continuation output of order `cov`.
pub fn cont_at(built: &BuiltTx, cov: &Hash32) -> usize {
    built.tx.outputs.iter().position(|o| o.covenant.as_ref().is_some_and(|c| c.covenant_id == cov.0)).expect("continuation")
}

/// Sets the continuation of `cov` to the P2SH of `s`, `dv` sompi more.
pub fn set_cont(built: &mut BuiltTx, cov: &Hash32, s: &AnyState, dv: i64) {
    let j = cont_at(built, cov);
    let o = &mut built.tx.outputs[j];
    o.script_public_key = spk_to_string(&s.spk());
    o.value = (o.value as i64 + dv) as u64;
}

/// Adds `dv` sompi to output `j`.
pub fn add_value(built: &mut BuiltTx, j: usize, dv: i64) {
    let o = &mut built.tx.outputs[j];
    o.value = (o.value as i64 + dv) as u64;
}

/// Takes `sompi` from the change output, for the mass the patch adds (and what it moves elsewhere).
pub fn take_change(built: &mut BuiltTx, sompi: i64) {
    let j = built.fee.change_output.expect("a change output") as usize;
    add_value(built, j, -sompi);
}

/// The entry plan of the order input (`template`), its arguments replaced by `f`.
pub fn set_args(built: &mut BuiltTx, template: TemplateId, f: impl FnOnce(&mut Vec<Arg>)) {
    let p = built
        .plans
        .iter_mut()
        .find(|p| matches!(p, SigPlan::Entry { template: t, .. } if *t == template))
        .expect("the order's entry plan");
    if let SigPlan::Entry { args, .. } = p {
        f(args);
    }
}

/// An empty unsigned transaction at lock time `lock`, signed with [`World::resign`] once filled. The fee rate is 0: each fixture
/// leaves 1 KAS of fee.
pub fn draft(lock: u64) -> BuiltTx {
    BuiltTx {
        tx: TxJson {
            id: [0; 32],
            version: 1,
            inputs: vec![],
            outputs: vec![],
            subnetwork_id: vec![0; 20],
            lock_time: lock,
            gas: 0,
            storage_mass: 0,
            payload: vec![],
        },
        plans: vec![],
        roles: vec![],
        sign: vec![],
        fee: FeeReport::default(),
        covenants: vec![],
    }
}

/// Adds an input spending `u` (script public key `spk`) by `plan`, signed by `signer`; returns its index.
pub fn add_input(b: &mut BuiltTx, u: &Utxo, spk: String, plan: SigPlan, signer: Option<[u8; 32]>) -> usize {
    let i = b.tx.inputs.len();
    b.tx.inputs.push(TxInputJson {
        transaction_id: u.transaction_id,
        index: u.index,
        sequence: 0,
        sig_op_count: 1,
        compute_budget: 0,
        signature_script: vec![],
        utxo: UtxoJson {
            address: None,
            amount: u.amount,
            script_public_key: spk,
            block_daa_score: u.block_daa_score,
            is_coinbase: false,
            covenant_id: u.covenant_id,
        },
    });
    b.plans.push(plan);
    b.roles.push("test".into());
    if let Some(pubkey) = signer {
        b.sign.push(SignRequest { input_index: i, pubkey, sighash_type: 1, sighash: [0; 32], redeem_script: None });
    }
    i
}

/// Adds an output; `cov`: its authorising input and covenant id. Returns its index.
pub fn add_output(b: &mut BuiltTx, value: i64, spk: String, cov: Option<(usize, [u8; 32])>) -> usize {
    assert!(value > 0, "output value {value}");
    b.tx.outputs.push(TxOutputJson {
        value: value as u64,
        script_public_key: spk,
        covenant: cov.map(|(i, c)| CovenantJson { authorizing_input: i as u16, covenant_id: c }),
    });
    b.tx.outputs.len() - 1
}

/// The P2PK script public key of key `k`.
pub fn key_spk(k: u8) -> String {
    spk_to_string(&kob_protocol::script::p2pk_spk(&pk(k)))
}

fn kcc(s: &TokenState) -> Kcc20State {
    match s {
        TokenState::Kcc20(k) => k.clone(),
        TokenState::Kron(_) => panic!("KCC-20 fixtures"),
    }
}

/// The plain (mode-0) fill of `n` base units of `order` (at input 0) by the taker selling a fresh synthetic token coin, in the shape
/// `kob_protocol::build` gives it: `fill(nb, tokIn, t, form)`; the delivery at output 0 (a carrier plus `used(n) - spend`); the
/// continuation, `left - deliveryCarrier`, while the bid can continue, else the unspent KAS beyond a carrier at the refund slot
/// `M - 1` (from REFUND_MIN up; below it rides on the delivery); the taker's change, paid `spend`.
pub fn bid_fill(c: &Ctx, order: &OrderUtxo<BidState>, n: i64) -> BuiltTx {
    let s = &order.state;
    let cov = order.utxo.covenant_id.expect("an order UTXO");
    let v = order.utxo.amount as i64;
    let (used, spend) = (s.used(n).unwrap(), s.spend(n, 0, order.utxo.block_daa_score as i64).unwrap());
    let (left, dc) = (v - used, s.delivery_carrier);
    let tok = c.w.token(TAKER, n);
    let coin = c.w.coin(TAKER, 100);
    let state = AnyState::KobBid(s.clone());
    let mut b = draft(c.daa());
    let entry = SigPlan::Entry {
        template: TemplateId::KobBid,
        state: state.encode(),
        entry: "fill".into(),
        args: vec![Arg::Bytes(n.to_le_bytes().to_vec()), Arg::Int(1), Arg::Int(0), Arg::Int(0)],
    };
    add_input(&mut b, &order.utxo, spk_to_string(&state.spk()), entry, None);
    let lead = SigPlan::TokenLeader {
        template: T3,
        state: kcc(&tok.state),
        next_states: vec![kcc(&plain(n, MAKER_A))],
        witness: Witness::P2pk(pk(TAKER)),
    };
    add_input(&mut b, &tok.utxo, tok_spk(&tok.state), lead, Some(pk(TAKER)));
    add_input(&mut b, &coin.utxo, key_spk(TAKER), SigPlan::P2pk { pubkey: pk(TAKER) }, Some(pk(TAKER)));
    let cont = s.tif == TIF_GTC && s.can_continue(left);
    let (d, refund) = if cont {
        (dc + used - spend, None)
    } else if v - spend - dc >= REFUND_MIN as i64 {
        (dc, Some(v - spend - dc))
    } else {
        (v - spend, None)
    };
    add_output(&mut b, d, tok_spk(&plain(n, MAKER_A)), Some((1, TOKEN_COV)));
    if cont {
        add_output(&mut b, left - dc, spk_to_string(&state.spk()), Some((0, cov)));
    }
    let change = add_output(&mut b, (coin.utxo.amount + tok.utxo.amount) as i64 + spend - KAS as i64, key_spk(TAKER), None);
    b.fee.change_output = Some(change as u32);
    if let Some(r) = refund {
        add_output(&mut b, r, key_spk(MAKER_A), None);
    }
    b
}

/// Mode 1, `bought = 0`: the fill of `n` creates the bought-custody (`bought` <- n; one delivery carrier released, as mode 0).
pub fn fill_first(c: &Ctx, order: &OrderUtxo<BidState>, cov: &Hash32, n: i64) -> SignedTx {
    let s = &order.state;
    assert_eq!((s.mode, s.bought), (MODE_ACCUMULATE, 0));
    let mut built = bid_fill(c, order, n);
    retarget(&mut built, &plain(n, MAKER_A), &bought(n, cov));
    set_cont(&mut built, cov, &AnyState::KobBid(BidState { bought: n, ..s.clone() }), 0);
    take_change(&mut built, KAS as i64);
    c.w.resign(built, false)
}

/// Mode 1, `bought > 0`: the fill of `n` adds to the bought-custody `custody` (its input `tokIn`). The escrow releases no carrier: the
/// continuation keeps it and the delivery carries the custody's KAS. `end`: the fill ends the order and delivers `bought + n` to the
/// maker as one plain UTXO, the unspent KAS to the refund slot.
pub fn fill_into(c: &Ctx, order: &OrderUtxo<BidState>, cov: &Hash32, custody: &TokenUtxo, n: i64) -> SignedTx {
    let s = &order.state;
    assert!(s.mode == MODE_ACCUMULATE && s.bought > 0);
    let total = s.bought + n;
    let mut built = bid_fill(c, order, n);
    let ends = !built.tx.outputs.iter().any(|o| o.covenant.as_ref().is_some_and(|x| x.covenant_id == cov.0));
    let to = if ends { plain(total, MAKER_A) } else { bought(total, cov) };
    let d = retarget(&mut built, &plain(n, MAKER_A), &to);
    let k = add_token_input(&mut built, custody);
    let dc = s.delivery_carrier;
    if ends {
        // `rem = inValue - allIn - released` with nothing released goes to the maker at the refund slot `M - 1 - i` from REFUND_MIN
        // up, else it rides on the delivery (the mode-0 shape's slot, a carrier less, is dropped)
        let key = key_spk(MAKER_A);
        let change = built.fee.change_output.unwrap() as usize;
        if let Some(r) = built.tx.outputs.iter().rposition(|o| o.covenant.is_none() && o.script_public_key == key) {
            assert!(r > change);
            built.tx.outputs.remove(r);
        }
        let rem = order.utxo.amount as i64 - s.spend(n, 0, order.utxo.block_daa_score as i64).unwrap();
        if rem >= REFUND_MIN as i64 {
            built.tx.outputs[d].value = custody.utxo.amount;
            add_output(&mut built, rem, key, None);
        } else {
            built.tx.outputs[d].value = custody.utxo.amount + rem as u64;
        }
    } else {
        set_cont(&mut built, cov, &AnyState::KobBid(BidState { bought: total, ..s.clone() }), dc);
        add_value(&mut built, d, custody.utxo.amount as i64 - dc);
        take_change(&mut built, KAS as i64);
    }
    set_args(&mut built, TemplateId::KobBid, |a| {
        a[1] = Arg::Int(k as i64);
        a[3] = Arg::Int(0);
    });
    c.w.resign(built, false)
}

/// A receive UTXO of `n` base units for key `k` at threshold `t`, sent on chain (a token transfer re-targeted at that state).
pub fn send_receive(c: &Ctx, n: i64, k: u8, t: i64) -> SignedTx {
    let req = SendTokens {
        token: TokenRef { covenant_id: TOKEN_COV, program: T3 },
        tokens: vec![c.w.token(30, n + 5)],
        recipients: vec![TokenRecipient { pubkey: pk(31), amount: n, carrier: CARRIER }],
        token_change: Some(pk(30)),
        token_change_carrier: CARRIER,
        funding: vec![c.w.coin(30, 100)],
        records: vec![],
        change: None,
        fee: FeeOptions::default(),
    };
    let mut built = build(&Action::SendTokens(req)).unwrap();
    retarget(&mut built, &plain(n, 31), &receive(n, k, t));
    c.w.resign(built, false)
}

/// Mode 2, form 2: the fill of `n` tops up the maker's receive UTXO `recv` (its input `tokIn`). The escrow releases `topupTip`, which
/// the filler keeps; the delivery carries the receive UTXO's KAS.
pub fn fill_topup(c: &Ctx, order: &OrderUtxo<BidState>, cov: &Hash32, recv: &TokenUtxo, n: i64) -> SignedTx {
    let s = &order.state;
    assert_eq!(s.mode, MODE_TOPUP);
    let mut built = bid_fill(c, order, n);
    let have = recv.state.amount();
    let d = retarget(&mut built, &plain(n, MAKER_A), &recv.state.clone().with_amount(have + n));
    let k = add_token_input(&mut built, recv);
    let (dc, tip) = (s.delivery_carrier, s.topup_tip);
    set_cont(&mut built, cov, &AnyState::KobBid(s.clone()), dc - tip);
    add_value(&mut built, d, recv.utxo.amount as i64 - dc);
    take_change(&mut built, KAS as i64 - tip);
    set_args(&mut built, TemplateId::KobBid, |a| {
        a[1] = Arg::Int(k as i64);
        a[3] = Arg::Int(2);
    });
    c.w.resign(built, false)
}

/// The maker's cancel of `order`, sweeping `custody` (built as a stray) to the maker. With `claim`, the escrow output becomes the
/// order's continuation with `bought = 0` (shape CL1); without it the escrow goes back (CL4).
pub fn cancel_sweeping(
    c: &Ctx,
    order: &OrderUtxo<AnyState>,
    cov: &Hash32,
    custody: Option<&TokenUtxo>,
    cont: Option<AnyState>,
) -> SignedTx {
    let req = CancelOrder {
        retired: None,
        prefund: None,
        order: order.clone(),
        custody: None,
        foreign: vec![],
        strays: custody.cloned().into_iter().collect(),
        tokens: vec![],
        funding: vec![c.w.coin(MAKER_A, 10)],
        change: None,
        replace: None,
        lock_time: 0,
        records: vec![],
        fee: FeeOptions::default(),
    };
    let mut built = build(&Action::CancelOrder(req)).unwrap_or_else(|e| panic!("build: {e}"));
    if let Some(next) = cont {
        let v = order.utxo.amount;
        // the builder returns the escrow with the funding's change in one output of the maker: the escrow stays in the order
        let key = key_spk(MAKER_A);
        let j = built
            .tx
            .outputs
            .iter()
            .position(|o| o.covenant.is_none() && o.script_public_key == key && o.value > v + KAS)
            .unwrap_or_else(|| panic!("the escrow's return (outputs {:?})", built.tx.outputs));
        built.tx.outputs[j].value -= v + KAS; // and the mass of the continuation
        let i =
            built.tx.inputs.iter().position(|x| x.transaction_id == order.utxo.transaction_id && x.index == order.utxo.index).unwrap();
        add_output(&mut built, v as i64, spk_to_string(&next.spk()), Some((i, cov.0)));
    }
    c.w.resign(built, false)
}

/// A keeper's refund of a mode-1 bid (`refund(custIn)`, at input 0) that releases its bought-custody `custody` (input 1) as one plain
/// UTXO of the maker at output `custIn` = 1, the custody's own input index. The maker gets the escrow less the refund tip at output 0.
pub fn refund_releasing(c: &Ctx, order: &OrderUtxo<AnyState>, custody: &TokenUtxo, lock: u64) -> SignedTx {
    let AnyState::KobBid(s) = &order.state else { panic!("a bid") };
    let cov = order.utxo.covenant_id.unwrap();
    let mut b = draft(lock);
    let entry =
        SigPlan::Entry { template: TemplateId::KobBid, state: order.state.encode(), entry: "refund".into(), args: vec![Arg::Int(1)] };
    add_input(&mut b, &order.utxo, spk_to_string(&order.state.spk()), entry, None);
    let release = plain(custody.state.amount(), MAKER_A);
    let lead = SigPlan::TokenLeader {
        template: T3,
        state: kcc(&custody.state),
        next_states: vec![kcc(&release)],
        witness: Witness::CovenantId,
    };
    let k = add_input(&mut b, &custody.utxo, tok_spk(&custody.state), lead, None);
    assert_eq!(k, 1);
    let coin = c.w.coin(KEEPER, 10);
    add_input(&mut b, &coin.utxo, key_spk(KEEPER), SigPlan::P2pk { pubkey: pk(KEEPER) }, Some(pk(KEEPER)));
    let _ = cov;
    add_output(&mut b, order.utxo.amount as i64 - s.refund_tip, key_spk(MAKER_A), None);
    add_output(&mut b, custody.utxo.amount as i64, tok_spk(&release), Some((k, TOKEN_COV)));
    let ch = add_output(&mut b, coin.utxo.amount as i64 + s.refund_tip - KAS as i64, key_spk(KEEPER), None);
    b.fee.change_output = Some(ch as u32);
    c.w.resign(b, false)
}

/// The income of an executor's transaction from its own figures: every input less every output (the network fee), plus what
/// the sender `k` took out (its P2PK outputs; the fixtures' senders keep no tokens), less what it put in (its P2PK inputs and the
/// tokens its inputs reveal it owns).
pub fn income_of(t: &SignedTx, k: u8) -> i64 {
    let key = spk_to_string(&kob_protocol::script::p2pk_spk(&pk(k)));
    let ins: i128 = t.tx.inputs.iter().map(|i| i128::from(i.utxo.amount)).sum();
    let outs: i128 = t.tx.outputs.iter().map(|o| i128::from(o.value)).sum();
    let mut sender: i128 = 0;
    for i in &t.tx.inputs {
        let mine = match i.utxo.covenant_id {
            None => i.utxo.script_public_key == key,
            Some(_) => revealed_owner(&i.signature_script).as_deref() == Some(&pk(k)[..]),
        };
        if mine {
            sender -= i128::from(i.utxo.amount);
        }
    }
    for o in t.tx.outputs.iter().filter(|o| o.covenant.is_none() && o.script_public_key == key) {
        sender += i128::from(o.value);
    }
    (ins - outs + sender) as i64
}
