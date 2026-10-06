//! Structural mutation operators over [`MTx`]. Every operator is deterministic in its RNG, returns a short
//! description of what it did (None when it does not apply), and has a "smart" variant that keeps the transaction's
//! internal references consistent (binding authorising inputs, the index arguments of the order entries, the order of
//! the KCC-20 / KRON next states) so the mutant gets past the plumbing and reaches the economic checks.

use kaspa_consensus_core::tx::{CovenantBinding, ScriptPublicKey, TransactionId, TransactionOutpoint, TransactionOutput, UtxoEntry};
use kaspa_consensus_core::Hash;
use kob_protocol::artifacts::{try_template, TemplateId};
use kob_protocol::script::p2pk_spk;
use kob_protocol::state::{Kcc20State, KronState, TokenState};
use kob_protocol::tx::{Arg, SigPlan, Witness};
use rand::rngs::StdRng;
use rand::Rng;

use super::{p2pk_of, MIn, MTx, ATTACKER};
use crate::common::pk;

/// Role of an entry argument.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Role {
    N,
    In,
    /// An input index or a negative flag (`tk`).
    InOrNeg,
    Out,
    Time,
    Leg,
    Amount,
    Bytes,
    Sig,
}

/// Roles of an order entry's arguments (`contracts/v2/*.sil`).
pub fn roles(t: TemplateId, entry: &str) -> &'static [Role] {
    use Role::*;
    let base = t.base();
    match (base, entry) {
        (TemplateId::KobAsk, "settle") => &[N, In, Out, Time],
        (TemplateId::KobBid, "fill") => &[N, In, Time],
        (TemplateId::KobCondAsk, "settle") => &[N, In, Out, Leg, In, InOrNeg, Time],
        (TemplateId::KobCondBid, "settle") => &[N, In, Leg, In, Time],
        (TemplateId::KobCondAsk | TemplateId::KobCondBid | TemplateId::KobIfdAsk, "update") => &[In, InOrNeg],
        (TemplateId::KobIfdBid, "update") => &[In],
        // settle(nb, custIn, tTplIn, t, sOut, tOut)
        (TemplateId::KobPair, "settle") => &[N, In, In, Time, Amount, Amount],
        // settle(nb, custIn, tTplIn, t, sOut, tOut, leg, evA, evB, tk, evMode, upd, k)
        (TemplateId::KobCondPair, "settle") => &[N, In, In, Time, Amount, Amount, Leg, In, In, In, Leg, Leg, Amount],
        // fill(nb, aIn, bIn, aTplIn, bTplIn, exitOut, amt, cPre, cSuf, evA, evB, tk, evMode, t, aOut, bOut, xc, upd)
        (TemplateId::KobIfdPair, "fill") => &[N, In, In, In, In, Out, Amount, Bytes, Bytes, In, In, In, Leg, Time, Out, Out, In, Leg],
        (TemplateId::KobIfdAsk, "settle") => &[N, In, Out, Out, Bytes, Bytes, In, InOrNeg, Time],
        (TemplateId::KobIfdBid, "fill") => &[N, In, Out, Bytes, Bytes, In, Time],
        (_, "cancel") => &[Sig],
        _ => &[],
    }
}

fn entry_roles(p: &SigPlan) -> Vec<Role> {
    match p {
        SigPlan::Entry { template, entry, args, .. } => {
            let r = roles(*template, entry);
            if r.len() == args.len() {
                r.to_vec()
            } else {
                vec![Role::Amount; args.len()]
            }
        }
        _ => vec![],
    }
}

fn fresh_outpoint(r: &mut StdRng) -> TransactionOutpoint {
    let mut id = [0u8; 32];
    r.fill(&mut id[..]);
    id[0] = 0xc6;
    TransactionOutpoint { transaction_id: TransactionId::from_bytes(id), index: r.gen_range(0..4) }
}

/// Remaps every input reference after a permutation of the inputs (`new_of[old] = new`, None = dropped).
pub fn remap_inputs(m: &mut MTx, new_of: &[Option<usize>]) {
    for o in &mut m.outs {
        if let Some(b) = &mut o.covenant {
            if let Some(Some(n)) = new_of.get(b.authorizing_input as usize) {
                b.authorizing_input = *n as u16;
            }
        }
    }
    for i in &mut m.ins {
        let rs = entry_roles(&i.plan);
        match &mut i.plan {
            SigPlan::Entry { args, .. } => {
                for (a, r) in args.iter_mut().zip(rs) {
                    if let (Arg::Int(v), Role::In | Role::InOrNeg) = (a, r) {
                        if *v >= 0 {
                            if let Some(Some(n)) = new_of.get(*v as usize) {
                                *v = *n as i64;
                            }
                        }
                    }
                }
            }
            SigPlan::KronToken { witnesses, .. } => {
                for w in witnesses.iter_mut() {
                    if let Some(Some(n)) = new_of.get(*w as usize) {
                        *w = *n as u8;
                    }
                }
            }
            _ => {}
        }
    }
}

/// Token output indices of token `t` in output order.
fn token_outputs(m: &MTx, t: &[u8; 32]) -> Vec<usize> {
    (0..m.outs.len()).filter(|&j| m.outs[j].covenant.is_some_and(|b| b.covenant_id.as_bytes() == *t)).collect()
}

/// Remaps every output reference after a permutation of the outputs (`new_of[old] = new`, None = dropped). The
/// next states of each token follow their outputs.
pub fn remap_outputs(m: &mut MTx, before: &[TransactionOutput], new_of: &[Option<usize>]) {
    for i in &mut m.ins {
        let rs = entry_roles(&i.plan);
        if let SigPlan::Entry { args, .. } = &mut i.plan {
            for (a, r) in args.iter_mut().zip(rs) {
                if let (Arg::Int(v), Role::Out) = (a, r) {
                    if *v >= 0 {
                        if let Some(Some(n)) = new_of.get(*v as usize) {
                            *v = *n as i64;
                        }
                    }
                }
            }
        }
    }
    // next states per token, in the order of their outputs
    let tokens: Vec<[u8; 32]> = m.ins.iter().filter_map(|i| i.entry.covenant_id.map(|h| h.as_bytes())).collect();
    for t in tokens {
        let old: Vec<usize> =
            (0..before.len()).filter(|&j| before[j].covenant.is_some_and(|b| b.covenant_id.as_bytes() == t)).collect();
        for i in &mut m.ins {
            if i.entry.covenant_id.map(|h| h.as_bytes()) != Some(t) {
                continue;
            }
            match &mut i.plan {
                SigPlan::TokenLeader { next_states, .. } if next_states.len() == old.len() => {
                    *next_states = reorder(next_states, &old, new_of);
                }
                SigPlan::KronToken { next_states, .. } if next_states.len() == old.len() => {
                    *next_states = reorder(next_states, &old, new_of);
                }
                _ => {}
            }
        }
    }
}

fn reorder<T: Clone>(v: &[T], old: &[usize], new_of: &[Option<usize>]) -> Vec<T> {
    let mut pairs: Vec<(usize, T)> =
        old.iter().zip(v).filter_map(|(o, s)| new_of.get(*o).copied().flatten().map(|n| (n, s.clone()))).collect();
    pairs.sort_by_key(|p| p.0);
    pairs.into_iter().map(|p| p.1).collect()
}

/// Context shared by the operators.
pub struct Cx {
    /// Donor transactions (the seeds) for splices and foreign script public keys.
    pub donors: Vec<MTx>,
}

fn pick<T>(r: &mut StdRng, v: &[T]) -> Option<usize> {
    (!v.is_empty()).then(|| r.gen_range(0..v.len()))
}

fn delta(r: &mut StdRng, v: u64) -> u64 {
    match r.gen_range(0..8) {
        0 => 1,
        1 => r.gen_range(1..1_000),
        2 => 100_000,
        3 => 10_000_000,
        4 => 100_000_000,
        5 => (v / 2).max(1),
        6 => v.saturating_sub(1).max(1),
        _ => r.gen_range(1..=v.max(1)),
    }
}

/// The names of the operators, in [`apply`] order.
pub const OPS: [&str; 28] = [
    "swapInputs",
    "swapInputsSmart",
    "swapOutputs",
    "swapOutputsSmart",
    "dropInput",
    "dropOutputSmart",
    "dupOutput",
    "cloneInput",
    "valueShift",
    "valueSet",
    "redirect",
    "spkSwap",
    "binding",
    "tokenOut",
    "argInt",
    "argN",
    "argBytes",
    "evidence",
    "lockTime",
    "sequence",
    "utxoDaa",
    "utxoAmount",
    "addStray",
    "splice",
    "nextState",
    "attackerOut",
    "orderState",
    "tokenInState",
];

/// Relative weights of the operators.
pub const WEIGHTS: [u32; 28] = [2, 3, 2, 3, 2, 3, 2, 2, 6, 4, 4, 3, 3, 6, 6, 6, 2, 3, 4, 1, 3, 3, 3, 3, 3, 3, 8, 2];

/// Applies operator `op`.
pub fn apply(op: usize, m: &mut MTx, r: &mut StdRng, cx: &Cx) -> Option<String> {
    match OPS[op] {
        "swapInputs" | "swapInputsSmart" => {
            if m.ins.len() < 2 {
                return None;
            }
            let a = r.gen_range(0..m.ins.len());
            let b = r.gen_range(0..m.ins.len());
            if a == b {
                return None;
            }
            m.ins.swap(a, b);
            if OPS[op] == "swapInputsSmart" {
                let new_of: Vec<Option<usize>> = (0..m.ins.len())
                    .map(|i| {
                        Some(if i == a {
                            b
                        } else if i == b {
                            a
                        } else {
                            i
                        })
                    })
                    .collect();
                remap_inputs(m, &new_of);
            }
            Some(format!("{} {a}<->{b}", OPS[op]))
        }
        "swapOutputs" | "swapOutputsSmart" => {
            if m.outs.len() < 2 {
                return None;
            }
            let a = r.gen_range(0..m.outs.len());
            let b = r.gen_range(0..m.outs.len());
            if a == b {
                return None;
            }
            let before = m.outs.clone();
            m.outs.swap(a, b);
            if OPS[op] == "swapOutputsSmart" {
                let new_of: Vec<Option<usize>> = (0..m.outs.len())
                    .map(|i| {
                        Some(if i == a {
                            b
                        } else if i == b {
                            a
                        } else {
                            i
                        })
                    })
                    .collect();
                remap_outputs(m, &before, &new_of);
            }
            Some(format!("{} {a}<->{b}", OPS[op]))
        }
        "dropInput" => {
            let i = pick(r, &m.ins)?;
            m.ins.remove(i);
            let new_of: Vec<Option<usize>> = (0..=m.ins.len())
                .map(|k| {
                    if k == i {
                        None
                    } else if k > i {
                        Some(k - 1)
                    } else {
                        Some(k)
                    }
                })
                .collect();
            remap_inputs(m, &new_of);
            Some(format!("dropInput {i}"))
        }
        "dropOutputSmart" => {
            let j = pick(r, &m.outs)?;
            let before = m.outs.clone();
            m.outs.remove(j);
            let new_of: Vec<Option<usize>> = (0..before.len())
                .map(|k| {
                    if k == j {
                        None
                    } else if k > j {
                        Some(k - 1)
                    } else {
                        Some(k)
                    }
                })
                .collect();
            remap_outputs(m, &before, &new_of);
            Some(format!("dropOutput {j}"))
        }
        "dupOutput" => {
            let j = pick(r, &m.outs)?;
            let o = m.outs[j].clone();
            m.outs.push(o);
            // a duplicated token output also needs its next state
            if let Some(b) = m.outs[j].covenant {
                let t = b.covenant_id.as_bytes();
                let pos = token_outputs(m, &t).iter().position(|&k| k == j);
                for i in &mut m.ins {
                    if i.entry.covenant_id.map(|h| h.as_bytes()) != Some(t) {
                        continue;
                    }
                    match &mut i.plan {
                        SigPlan::TokenLeader { next_states, .. } => {
                            if let Some(s) = pos.and_then(|p| next_states.get(p).cloned()) {
                                next_states.push(s);
                            }
                        }
                        SigPlan::KronToken { next_states, .. } => {
                            if let Some(s) = pos.and_then(|p| next_states.get(p).cloned()) {
                                next_states.push(s);
                            }
                        }
                        _ => {}
                    }
                }
            }
            Some(format!("dupOutput {j}"))
        }
        "cloneInput" => {
            let i = pick(r, &m.ins)?;
            let mut c = m.ins[i].clone();
            c.op = fresh_outpoint(r);
            m.ins.push(c);
            Some(format!("cloneInput {i}"))
        }
        "valueShift" => {
            if m.outs.len() < 2 {
                return None;
            }
            let a = r.gen_range(0..m.outs.len());
            let b = r.gen_range(0..m.outs.len());
            if a == b {
                return None;
            }
            let d = delta(r, m.outs[a].value).min(m.outs[a].value.saturating_sub(1));
            if d == 0 {
                return None;
            }
            m.outs[a].value -= d;
            m.outs[b].value = m.outs[b].value.saturating_add(d);
            Some(format!("valueShift {d} from {a} to {b}"))
        }
        "valueSet" => {
            let j = pick(r, &m.outs)?;
            let v = m.outs[j].value;
            let nv = match r.gen_range(0..6) {
                0 => v.saturating_sub(1).max(1),
                1 => v.saturating_add(1),
                2 => (v / 2).max(1),
                3 => v.saturating_mul(2),
                4 => v.saturating_sub(delta(r, v)).max(1),
                _ => v.saturating_add(delta(r, v)),
            };
            m.outs[j].value = nv;
            Some(format!("valueSet {j}: {v} -> {nv}"))
        }
        "redirect" => {
            let j = pick(r, &m.outs)?;
            m.outs[j].script_public_key = p2pk_spk(&pk(ATTACKER));
            if r.gen_bool(0.5) {
                m.outs[j].covenant = None;
            }
            Some(format!("redirect {j} to attacker"))
        }
        "spkSwap" => {
            let j = pick(r, &m.outs)?;
            let src: ScriptPublicKey = if r.gen_bool(0.6) || cx.donors.is_empty() {
                let k = r.gen_range(0..m.outs.len());
                m.outs[k].script_public_key.clone()
            } else {
                let d = &cx.donors[r.gen_range(0..cx.donors.len())];
                let k = pick(r, &d.outs)?;
                d.outs[k].script_public_key.clone()
            };
            m.outs[j].script_public_key = src;
            Some(format!("spkSwap {j}"))
        }
        "binding" => {
            let j = pick(r, &m.outs)?;
            let covs: Vec<(usize, Hash)> = m.ins.iter().enumerate().filter_map(|(i, x)| x.entry.covenant_id.map(|c| (i, c))).collect();
            match r.gen_range(0..3) {
                0 => {
                    m.outs[j].covenant = None;
                    Some(format!("binding {j}: none"))
                }
                1 => {
                    let k = pick(r, &covs)?;
                    m.outs[j].covenant = Some(CovenantBinding { authorizing_input: covs[k].0 as u16, covenant_id: covs[k].1 });
                    Some(format!("binding {j}: input {} cov", covs[k].0))
                }
                _ => {
                    let b = m.outs[j].covenant.as_mut()?;
                    let k = pick(r, &covs)?;
                    b.authorizing_input = covs[k].0 as u16;
                    Some(format!("binding {j}: authorizing input {}", covs[k].0))
                }
            }
        }
        "tokenOut" => token_out(m, r),
        "argInt" => {
            let cands: Vec<(usize, usize)> = m
                .ins
                .iter()
                .enumerate()
                .flat_map(|(i, x)| match &x.plan {
                    SigPlan::Entry { args, .. } | SigPlan::Router { args, .. } => {
                        args.iter().enumerate().filter(|(_, a)| matches!(a, Arg::Int(_))).map(|(k, _)| (i, k)).collect::<Vec<_>>()
                    }
                    _ => vec![],
                })
                .collect();
            let (i, k) = cands[pick(r, &cands)?];
            let n_in = m.ins.len() as i64;
            let n_out = m.outs.len() as i64;
            let lock = m.lock_time as i64;
            let args = match &mut m.ins[i].plan {
                SigPlan::Entry { args, .. } | SigPlan::Router { args, .. } => args,
                _ => unreachable!(),
            };
            let Arg::Int(v) = &mut args[k] else { unreachable!() };
            let old = *v;
            *v = match r.gen_range(0..10) {
                0 => old.saturating_add(1),
                1 => old.saturating_sub(1),
                2 => 0,
                3 => -1,
                4 => r.gen_range(0..n_in.max(1)),
                5 => r.gen_range(0..n_out.max(1)),
                6 => lock,
                7 => lock + 1,
                8 => old.saturating_add(r.gen_range(-1000..1000)),
                _ => [i64::MAX, i64::MIN, 1 << 32, -(1 << 32), 1 << 53, -(1 << 53)][r.gen_range(0..6)],
            };
            Some(format!("argInt input {i} arg {k}: {old} -> {v}"))
        }
        "argN" => {
            let cands: Vec<usize> = m
                .ins
                .iter()
                .enumerate()
                .filter(|(_, x)| matches!(&x.plan, SigPlan::Entry { args, .. } if matches!(args.first(), Some(Arg::Bytes(b)) if b.len() == 8)))
                .map(|(i, _)| i)
                .collect();
            let i = cands[pick(r, &cands)?];
            let SigPlan::Entry { args, .. } = &mut m.ins[i].plan else { unreachable!() };
            let Arg::Bytes(b) = &mut args[0] else { unreachable!() };
            let old = super::snum8(b);
            if r.gen_bool(0.08) {
                // the same number pushed minimally (not the fixed 8-byte push the merge cross-checks read)
                args[0] = Arg::Int(old);
                return Some(format!("argN input {i}: {old} pushed minimally"));
            }
            if r.gen_bool(0.15) {
                // non-canonical encodings: negative zero, the sign bit on a positive count
                let mut x = b.clone();
                x[7] ^= 0x80;
                *b = x;
                return Some(format!("argN input {i}: sign bit flipped on {old}"));
            }
            let nv = match r.gen_range(0..8) {
                0 => old.saturating_add(1),
                1 => old.saturating_sub(1),
                2 => 0,
                3 => old.saturating_mul(2),
                4 => -old,
                5 => old.saturating_add(r.gen_range(1..20)),
                // a repeat merge argument -(k * 2^53 + m) naming another input k
                6 => -((r.gen_range(0..8i64) << 53) + r.gen_range(1..12_000)),
                _ => [i64::MAX, -i64::MAX, -1, 1][r.gen_range(0..4)],
            };
            *b = super::enc8(nv);
            Some(format!("argN input {i}: {old} -> {nv}"))
        }
        "argBytes" => {
            let cands: Vec<(usize, usize)> = m
                .ins
                .iter()
                .enumerate()
                .flat_map(|(i, x)| match &x.plan {
                    SigPlan::Entry { args, .. } => args
                        .iter()
                        .enumerate()
                        .skip(1)
                        .filter(|(_, a)| matches!(a, Arg::Bytes(_)))
                        .map(|(k, _)| (i, k))
                        .collect::<Vec<_>>(),
                    _ => vec![],
                })
                .collect();
            let (i, k) = cands[pick(r, &cands)?];
            let ids = [
                TemplateId::KobCondPair,
                TemplateId::KobPair,
                TemplateId::KobCondAsk,
                TemplateId::KobCondBid,
                TemplateId::KobCondAskKron,
                TemplateId::KobCondBidKron,
                TemplateId::KobAsk,
            ];
            let SigPlan::Entry { args, .. } = &mut m.ins[i].plan else { unreachable!() };
            let Arg::Bytes(b) = &mut args[k] else { unreachable!() };
            let what = match r.gen_range(0..4) {
                0 => {
                    b.clear();
                    "empty".to_string()
                }
                1 => {
                    if !b.is_empty() {
                        let x = r.gen_range(0..b.len());
                        b[x] ^= 1 << r.gen_range(0..8);
                    }
                    "bitflip".to_string()
                }
                2 => {
                    let t = try_template(ids[r.gen_range(0..ids.len())])?;
                    *b = if r.gen_bool(0.5) { t.prefix.clone() } else { t.suffix.clone() };
                    "other template part".to_string()
                }
                _ => {
                    b.truncate(b.len() / 2);
                    "truncate".to_string()
                }
            };
            Some(format!("argBytes input {i} arg {k}: {what}"))
        }
        "evidence" => {
            let cands: Vec<(usize, usize)> = m
                .ins
                .iter()
                .enumerate()
                .flat_map(|(i, x)| {
                    entry_roles(&x.plan)
                        .into_iter()
                        .enumerate()
                        .filter(|(_, r)| matches!(r, Role::In | Role::InOrNeg))
                        .map(move |(k, _)| (i, k))
                })
                .collect();
            let (i, k) = cands[pick(r, &cands)?];
            let to = r.gen_range(0..m.ins.len()) as i64;
            let SigPlan::Entry { args, .. } = &mut m.ins[i].plan else { unreachable!() };
            let Arg::Int(v) = &mut args[k] else { return None };
            let old = *v;
            *v = to;
            Some(format!("evidence input {i} arg {k}: {old} -> {to}"))
        }
        "lockTime" => {
            let old = m.lock_time;
            let d = [1u64, 10, 50, 100, 300, 600, 10_000, 1_000_000, 77_760_000][r.gen_range(0..9)];
            m.lock_time = if r.gen_bool(0.5) { old.saturating_add(d) } else { old.saturating_sub(d) };
            Some(format!("lockTime {old} -> {}", m.lock_time))
        }
        "sequence" => {
            let i = pick(r, &m.ins)?;
            let old = m.ins[i].seq;
            m.ins[i].seq = [0u64, 1, 50, 600, u64::MAX, old.saturating_add(1)][r.gen_range(0..6)];
            Some(format!("sequence input {i}: {old} -> {}", m.ins[i].seq))
        }
        "utxoDaa" => {
            let i = pick(r, &m.ins)?;
            let e = &m.ins[i].entry;
            let old = e.block_daa_score;
            let d = [1u64, 50, 100, 600, 10_000, 1_000_000][r.gen_range(0..6)];
            let nv = if r.gen_bool(0.5) { old.saturating_add(d) } else { old.saturating_sub(d) };
            m.ins[i].entry = UtxoEntry::new(e.amount, e.script_public_key.clone(), nv, e.is_coinbase, e.covenant_id);
            Some(format!("utxoDaa input {i}: {old} -> {nv}"))
        }
        "utxoAmount" => {
            let i = pick(r, &m.ins)?;
            let e = &m.ins[i].entry;
            let old = e.amount;
            let d = delta(r, old);
            let nv = if r.gen_bool(0.5) { old.saturating_add(d) } else { old.saturating_sub(d).max(1) };
            m.ins[i].entry = UtxoEntry::new(nv, e.script_public_key.clone(), e.block_daa_score, e.is_coinbase, e.covenant_id);
            Some(format!("utxoAmount input {i}: {old} -> {nv}"))
        }
        "addStray" => add_stray(m, r),
        "orderState" => order_state(m, r),
        "tokenInState" => token_in_state(m, r),
        "splice" => {
            if cx.donors.is_empty() {
                return None;
            }
            let d = cx.donors[r.gen_range(0..cx.donors.len())].clone();
            splice(m, d, r);
            Some("splice".into())
        }
        "nextState" => {
            let cands: Vec<usize> = m
                .ins
                .iter()
                .enumerate()
                .filter(|(_, x)| {
                    matches!(&x.plan, SigPlan::TokenLeader { next_states, .. } if !next_states.is_empty())
                        || matches!(&x.plan, SigPlan::KronToken { next_states, .. } if !next_states.is_empty())
                })
                .map(|(i, _)| i)
                .collect();
            let i = cands[pick(r, &cands)?];
            let what = match &mut m.ins[i].plan {
                SigPlan::TokenLeader { next_states, .. } => {
                    let k = r.gen_range(0..next_states.len());
                    let d = delta(r, next_states[k].amount.max(1) as u64) as i64;
                    next_states[k].amount += if r.gen_bool(0.5) { d } else { -d };
                    format!("kcc20 next state {k} amount {}", next_states[k].amount)
                }
                SigPlan::KronToken { next_states, .. } => {
                    let k = r.gen_range(0..next_states.len());
                    let d = delta(r, next_states[k].amount.max(1) as u64) as i64;
                    next_states[k].amount += if r.gen_bool(0.5) { d } else { -d };
                    format!("kron next state {k} amount {}", next_states[k].amount)
                }
                _ => unreachable!(),
            };
            Some(format!("nextState input {i}: {what}"))
        }
        "attackerOut" => {
            // an extra attacker output taking value from a P2PK output (change, maker payment)
            let ks: Vec<usize> = (0..m.outs.len()).filter(|&j| p2pk_of(&m.outs[j].script_public_key).is_some()).collect();
            let j = ks[pick(r, &ks)?];
            let d = delta(r, m.outs[j].value).min(m.outs[j].value.saturating_sub(1));
            if d == 0 {
                return None;
            }
            m.outs[j].value -= d;
            let at = r.gen_range(0..=m.outs.len());
            let before = m.outs.clone();
            m.outs.insert(at, TransactionOutput { value: d, script_public_key: p2pk_spk(&pk(ATTACKER)), covenant: None });
            if r.gen_bool(0.7) {
                let new_of: Vec<Option<usize>> = (0..before.len()).map(|k| Some(if k >= at { k + 1 } else { k })).collect();
                remap_outputs(m, &before, &new_of);
            }
            Some(format!("attackerOut {d} from {j} at {at}"))
        }
        _ => None,
    }
}

/// Mutates a token output: amount, owner (attacker / another covenant id), keeping the next states consistent
/// (smart) or not.
fn token_out(m: &mut MTx, r: &mut StdRng) -> Option<String> {
    use kob_protocol::artifacts::spk_trace;
    let cands: Vec<(usize, TemplateId, TokenState)> = m
        .outs
        .iter()
        .enumerate()
        .filter_map(|(j, o)| match spk_trace::lookup(&o.script_public_key) {
            Some((spk_trace::Origin::Template(t), st)) if t.is_token() => {
                let tt = kob_protocol::artifacts::try_token_template(t)?;
                TokenState::decode_with(tt, &st).ok().map(|s| (j, t, s))
            }
            _ => None,
        })
        .collect();
    let (j, t, s) = cands[pick(r, &cands)?].clone();
    let covs: Vec<[u8; 32]> = m.ins.iter().filter_map(|i| i.entry.covenant_id.map(|h| h.as_bytes())).collect();
    let (ns, what) = match r.gen_range(0..5) {
        0 | 1 => {
            let d = delta(r, s.amount().max(1) as u64) as i64;
            let a = if r.gen_bool(0.5) { s.amount() + d } else { s.amount() - d };
            (s.with_amount(a), format!("amount {} -> {a}", s.amount()))
        }
        2 => (s.with_user_owner(pk(ATTACKER)), "owner -> attacker".to_string()),
        3 => {
            let c = covs.get(r.gen_range(0..covs.len().max(1)))?;
            (TokenState::custody(s.family(), s.amount(), *c, s.extension()), "owner -> covenant".to_string())
        }
        _ => {
            let a = s.amount();
            (s.with_user_owner(pk(ATTACKER)).with_amount(a), "owner -> attacker (same amount)".to_string())
        }
    };
    let tt = kob_protocol::artifacts::try_token_template(t)?;
    m.outs[j].script_public_key = ns.spk_with(tt);
    let smart = r.gen_bool(0.8);
    if smart {
        if let Some(b) = m.outs[j].covenant {
            let tok = b.covenant_id.as_bytes();
            let pos = token_outputs(m, &tok).iter().position(|&k| k == j)?;
            for i in &mut m.ins {
                if i.entry.covenant_id.map(|h| h.as_bytes()) != Some(tok) {
                    continue;
                }
                match (&mut i.plan, &ns) {
                    (SigPlan::TokenLeader { next_states, .. }, TokenState::Kcc20(x)) if pos < next_states.len() => {
                        next_states[pos] = x.clone()
                    }
                    (SigPlan::KronToken { next_states, .. }, TokenState::Kron(x)) if pos < next_states.len() => {
                        next_states[pos] = x.clone()
                    }
                    _ => {}
                }
            }
        }
    }
    Some(format!("tokenOut {j}: {what}{}", if smart { " (next states)" } else { "" }))
}

/// Adds a token input: a stray owned by an order covenant id of the transaction, or the attacker's own tokens;
/// optionally with an output that takes it.
fn add_stray(m: &mut MTx, r: &mut StdRng) -> Option<String> {
    // a token of the transaction and one of its inputs as a template for program and extension
    let toks: Vec<usize> = (0..m.ins.len())
        .filter(|&i| matches!(m.ins[i].plan, SigPlan::TokenLeader { .. } | SigPlan::TokenDelegator { .. } | SigPlan::KronToken { .. }))
        .collect();
    let src = m.ins[toks[pick(r, &toks)?]].clone();
    let order_covs: Vec<[u8; 32]> = m
        .ins
        .iter()
        .filter(|i| matches!(i.plan, SigPlan::Entry { .. }))
        .filter_map(|i| i.entry.covenant_id.map(|h| h.as_bytes()))
        .collect();
    let amount = [1i64, 7, 1_000, 3_000, 10_000][r.gen_range(0..5)];
    let to_order = !order_covs.is_empty() && r.gen_bool(0.6);
    let owner_cov = if to_order { Some(order_covs[r.gen_range(0..order_covs.len())]) } else { None };
    let (plan, state): (SigPlan, TokenState) = match &src.plan {
        SigPlan::TokenLeader { template, state, .. } | SigPlan::TokenDelegator { template, state, .. } => {
            let s = match owner_cov {
                Some(c) => Kcc20State { amount, owner: c, owner_scheme: 0x04, ..state.clone() },
                None => Kcc20State { amount, owner: pk(ATTACKER), owner_scheme: 0x00, ..state.clone() },
            };
            let w = if owner_cov.is_some() { Witness::CovenantId } else { Witness::P2pk(pk(ATTACKER)) };
            (SigPlan::TokenDelegator { template: *template, state: s.clone(), witness: w }, TokenState::Kcc20(s))
        }
        SigPlan::KronToken { template, next_states, witnesses, .. } => {
            let s = match owner_cov {
                Some(c) => KronState::custody(amount, c),
                None => KronState::addr(amount, pk(ATTACKER)),
            };
            // the witness names the authorising input: the owner order, or a P2PK input of the attacker
            let wi = match owner_cov {
                Some(c) => m.ins.iter().position(|i| i.entry.covenant_id.map(|h| h.as_bytes()) == Some(c))?,
                None => {
                    let spk = p2pk_spk(&pk(ATTACKER));
                    m.ins.push(MIn {
                        op: fresh_outpoint(r),
                        entry: UtxoEntry::new(100_000_000, spk, 500, false, None),
                        plan: SigPlan::P2pk { pubkey: pk(ATTACKER) },
                        seq: 0,
                        budget: 0,
                    });
                    m.ins.len() - 1
                }
            };
            let mut w = witnesses.clone();
            w.push(wi as u8);
            (
                SigPlan::KronToken { template: *template, state: s.clone(), next_states: next_states.clone(), witnesses: w },
                TokenState::Kron(s),
            )
        }
        _ => return None,
    };
    let n = m.ins.len();
    let spk = plan.spk();
    let entry = UtxoEntry::new(src.entry.amount, spk, src.entry.block_daa_score, false, src.entry.covenant_id);
    m.ins.push(MIn { op: fresh_outpoint(r), entry, plan, seq: 0, budget: src.budget });
    // KRON: every token input of the token carries the witnesses of all of them
    if let TokenState::Kron(_) = state {
        let tok = src.entry.covenant_id;
        let w = match &m.ins[n].plan {
            SigPlan::KronToken { witnesses, .. } => witnesses.clone(),
            _ => vec![],
        };
        for i in m.ins.iter_mut().take(n) {
            if i.entry.covenant_id == tok {
                if let SigPlan::KronToken { witnesses, .. } = &mut i.plan {
                    *witnesses = w.clone();
                }
            }
        }
    }
    // the attacker also needs to be present for a key-held KRON token / signs a KCC-20 delegator
    Some(format!("addStray {amount} units owned by {}", if owner_cov.is_some() { "an order" } else { "the attacker" }))
}

/// Appends a donor transaction (outpoints re-tagged, its references shifted).
pub fn splice(m: &mut MTx, mut d: MTx, r: &mut StdRng) {
    let off_in = m.ins.len();
    let off_out = m.outs.len();
    let tag: u8 = r.gen_range(1..=255);
    for i in &mut d.ins {
        let mut id = i.op.transaction_id.as_bytes();
        id[31] ^= tag;
        i.op.transaction_id = TransactionId::from_bytes(id);
    }
    let new_in: Vec<Option<usize>> = (0..d.ins.len()).map(|k| Some(k + off_in)).collect();
    remap_inputs(&mut d, &new_in);
    let before = d.outs.clone();
    let new_out: Vec<Option<usize>> = (0..d.outs.len()).map(|k| Some(k + off_out)).collect();
    // shift output references only (next states keep their relative order)
    for i in &mut d.ins {
        let rs = entry_roles(&i.plan);
        if let SigPlan::Entry { args, .. } = &mut i.plan {
            for (a, rr) in args.iter_mut().zip(rs) {
                if let (Arg::Int(v), Role::Out) = (a, rr) {
                    if *v >= 0 {
                        if let Some(Some(n)) = new_out.get(*v as usize) {
                            *v = *n as i64;
                        }
                    }
                }
            }
        }
    }
    let _ = before;
    // one KCC-20 leader per token: the donor's leaders of a token the base already leads become delegators
    for i in &mut d.ins {
        let tok = i.entry.covenant_id;
        let base_leader = m.ins.iter_mut().find(|x| x.entry.covenant_id == tok && matches!(x.plan, SigPlan::TokenLeader { .. }));
        if let (Some(bl), SigPlan::TokenLeader { template, state, next_states, witness }) = (base_leader, &i.plan) {
            if let SigPlan::TokenLeader { next_states: ns, .. } = &mut bl.plan {
                ns.extend(next_states.iter().cloned());
            }
            i.plan = SigPlan::TokenDelegator { template: *template, state: state.clone(), witness: witness.clone() };
        }
    }
    m.ins.extend(d.ins);
    m.outs.extend(d.outs);
    m.lock_time = m.lock_time.max(d.lock_time);
}

/// Adds the attacker's funding when the outputs exceed the inputs (an attacker can always pay).
pub fn balance(m: &mut MTx, r: &mut StdRng) {
    let i: u64 = m.ins.iter().map(|x| x.entry.amount).sum();
    let o: u64 = m.outs.iter().map(|x| x.value).sum();
    if o > i {
        let need = o - i + 1_000_000;
        let spk = p2pk_spk(&pk(ATTACKER));
        m.ins.push(MIn {
            op: fresh_outpoint(r),
            entry: UtxoEntry::new(need.max(100_000_000), spk, 500, false, None),
            plan: SigPlan::P2pk { pubkey: pk(ATTACKER) },
            seq: 0,
            budget: 0,
        });
    }
}

fn perturb(r: &mut StdRng, v: i64) -> i64 {
    match r.gen_range(0..9) {
        0 => v.saturating_add(1),
        1 => v.saturating_sub(1),
        2 => 0,
        3 => v.saturating_mul(2),
        4 => v / 2,
        5 => v.saturating_add(delta(r, v.unsigned_abs().max(1)) as i64),
        6 => v.saturating_sub(delta(r, v.unsigned_abs().max(1)) as i64),
        7 => v.saturating_neg(),
        _ => [1, 10_000, 1 << 32, i64::MAX / 2][r.gen_range(0..4)],
    }
}

/// A different world: the order spent at an input has other terms (one integer field of its state changed), the
/// transaction built for the original terms otherwise unchanged. The covenant must refuse every mutant that pays
/// the maker less than the new terms.
fn order_state(m: &mut MTx, r: &mut StdRng) -> Option<String> {
    use kob_protocol::state::AnyState;
    let cands: Vec<usize> = (0..m.ins.len()).filter(|&i| matches!(m.ins[i].plan, SigPlan::Entry { .. })).collect();
    let i = cands[pick(r, &cands)?];
    let SigPlan::Entry { template, state, .. } = &m.ins[i].plan else { unreachable!() };
    let s = AnyState::decode(*template, state).ok()?;
    let mut v = serde_json::to_value(&s).ok()?;
    let fields = v.get_mut("state")?.as_object_mut()?;
    let ints: Vec<String> = fields
        .iter()
        .filter(|(_, x)| x.as_i64().is_some() || x.as_str().is_some_and(|t| t.len() < 21 && t.parse::<i64>().is_ok()))
        .map(|(k, _)| k.clone())
        .collect();
    let k = ints[pick(r, &ints)?].clone();
    let x = fields.get_mut(&k)?;
    let old = x.as_i64().or_else(|| x.as_str().and_then(|t| t.parse().ok()))?;
    let nv = perturb(r, old);
    *x = if x.is_string() { serde_json::Value::String(nv.to_string()) } else { serde_json::Value::from(nv) };
    let s2: AnyState = serde_json::from_value(v).ok()?;
    let enc = s2.try_encode().ok()?;
    let spk = kob_protocol::artifacts::template(*template).spk(&enc);
    let e = &m.ins[i].entry;
    m.ins[i].entry = UtxoEntry::new(e.amount, spk, e.block_daa_score, e.is_coinbase, e.covenant_id);
    if let SigPlan::Entry { state, .. } = &mut m.ins[i].plan {
        *state = enc;
    }
    Some(format!("orderState input {i}: {k} {old} -> {nv}"))
}

/// A different world: a token input holds another amount (or is owned by another covenant id of the transaction).
fn token_in_state(m: &mut MTx, r: &mut StdRng) -> Option<String> {
    let cands: Vec<usize> = (0..m.ins.len())
        .filter(|&i| matches!(m.ins[i].plan, SigPlan::TokenLeader { .. } | SigPlan::TokenDelegator { .. } | SigPlan::KronToken { .. }))
        .collect();
    let i = cands[pick(r, &cands)?];
    let what;
    match &mut m.ins[i].plan {
        SigPlan::TokenLeader { state, .. } | SigPlan::TokenDelegator { state, .. } => {
            let old = state.amount;
            state.amount = perturb(r, old).max(0);
            what = format!("amount {old} -> {}", state.amount);
        }
        SigPlan::KronToken { state, .. } => {
            let old = state.amount;
            state.amount = perturb(r, old).max(0);
            what = format!("amount {old} -> {}", state.amount);
        }
        _ => unreachable!(),
    }
    let spk = m.ins[i].plan.spk();
    let e = &m.ins[i].entry;
    m.ins[i].entry = UtxoEntry::new(e.amount, spk, e.block_daa_score, e.is_coinbase, e.covenant_id);
    Some(format!("tokenInState input {i}: {what}"))
}
