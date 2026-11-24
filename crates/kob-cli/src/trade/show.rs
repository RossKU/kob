//! What a transaction does, decoded from the unsigned transaction and its signing plan alone.
//!
//! The decoding is like the confirm screen of the web app, `web/src/kob/decode.ts`. It lists
//! every input and output with its owner, the net effect on the wallet (KAS and each token), the
//! fee and the signatures asked for.

use std::collections::BTreeMap;

use kob_protocol::json::to_hex;
use kob_protocol::script::p2pk_spk;
use kob_protocol::state::TokenState;
use kob_protocol::tx::{spk_to_string, BuiltTx, FeeMode, SigPlan};
use serde_json::{json, Value};

use super::market::show_id;
use super::units::{fmt_decimal, kas, TokenUnit};

/// The display facts of the tokens that a transaction may touch.
pub type Units = BTreeMap<[u8; 32], TokenUnit>;

fn tok(units: &Units, cov: &[u8; 32], amount: i128) -> String {
    match units.get(cov) {
        Some(u) => {
            format!("{} {}", fmt_decimal(amount, u.decimals), if u.ticker.is_empty() { show_id(cov, "") } else { u.ticker.clone() })
        }
        None => format!("{amount} base units of {}", show_id(cov, "")),
    }
}

fn owner_of(st: &TokenState, me: &[u8; 32]) -> String {
    if st.is_user() {
        if st.owner() == *me {
            "you".into()
        } else {
            format!("key {}", show_id(&st.owner(), ""))
        }
    } else {
        format!("covenant {}", show_id(&st.owner(), ""))
    }
}

/// The decoded transaction, from the point of view of one key.
#[derive(Debug, Clone)]
pub struct Decoded {
    /// One line per input.
    pub inputs: Vec<String>,
    /// One line per output.
    pub outputs: Vec<String>,
    /// KAS into (+) or out of (-) the wallet's plain UTXOs, fee included.
    pub kas_net: i128,
    /// Tokens into (+) or out of (-) the token UTXOs that the wallet key owns.
    pub token_net: BTreeMap<[u8; 32], i128>,
    /// The network fee.
    pub fee: u64,
    /// Whether the fee is the storage-inclusive priority fee (`--priority-fee`), not the relay
    /// floor.
    pub priority_fee: bool,
    /// The number of inputs that the wallet key signs.
    pub signatures: usize,
    /// The number of signatures of other keys, which the signer refuses.
    pub foreign_signatures: usize,
    /// Inputs that no signature of yours covers (covenant-spent custodies, strays, the orders it
    /// fills), and the KAS they hold as the node reports it. A signature commits to its own input's
    /// amount only, so the fee shown holds as far as the node reports these truly (R8F3-1).
    pub unsigned_inputs: usize,
    pub unsigned_kas: i128,
}

/// Decodes `built` from the point of view of the key `me`.
pub fn decode(built: &BuiltTx, me: &[u8; 32], units: &Units) -> Decoded {
    let my_spk = spk_to_string(&p2pk_spk(me));
    let mut d = Decoded {
        inputs: vec![],
        outputs: vec![],
        kas_net: 0,
        token_net: BTreeMap::new(),
        fee: built.fee.fee,
        priority_fee: built.fee.fee_mode == FeeMode::Priority,
        signatures: built.sign.iter().filter(|s| s.pubkey == *me).count(),
        foreign_signatures: built.sign.iter().filter(|s| s.pubkey != *me).count(),
        unsigned_inputs: 0,
        unsigned_kas: 0,
    };
    // Token output states per token, in output order. The leader and KRON plans carry the next
    // states.
    let mut next: BTreeMap<[u8; 32], Vec<TokenState>> = BTreeMap::new();
    for (i, (inp, plan)) in built.tx.inputs.iter().zip(&built.plans).enumerate() {
        let value = inp.utxo.amount as i128;
        if !built.sign.iter().any(|s| s.input_index == i && s.pubkey == *me) {
            d.unsigned_inputs += 1;
            d.unsigned_kas += value;
        }
        let line = match plan {
            SigPlan::P2pk { pubkey } => {
                if pubkey == me {
                    d.kas_net -= value;
                    format!("in  #{i}  your KAS {}", kas(value))
                } else {
                    format!("in  #{i}  KAS of key {} {}", show_id(pubkey, ""), kas(value))
                }
            }
            SigPlan::CreatorLock { until, key } => {
                if key == me {
                    d.kas_net -= value;
                }
                format!("in  #{i}  creator lock coin of key {} (opens at {until} ms) {}", show_id(key, ""), kas(value))
            }
            SigPlan::CreatorLockAuto { until, key, .. } => {
                format!(
                    "in  #{i}  automatic creator lock coin of key {} (opens at {until} ms, back to the key) {}",
                    show_id(key, ""),
                    kas(value)
                )
            }
            SigPlan::Entry { template, entry, .. } => format!(
                "in  #{i}  order {} {} `{entry}` ({} on it)",
                template.name(),
                inp.utxo.covenant_id.map(|c| show_id(&c, "")).unwrap_or_default(),
                kas(value)
            ),
            SigPlan::TokenLeader { state, next_states, .. } => {
                let cov = inp.utxo.covenant_id.unwrap_or_default();
                next.insert(cov, next_states.iter().cloned().map(TokenState::Kcc20).collect());
                token_in(&mut d, i, &TokenState::Kcc20(state.clone()), cov, value, me, units)
            }
            SigPlan::TokenDelegator { state, .. } => {
                let cov = inp.utxo.covenant_id.unwrap_or_default();
                token_in(&mut d, i, &TokenState::Kcc20(state.clone()), cov, value, me, units)
            }
            SigPlan::KronToken { state, next_states, .. } => {
                let cov = inp.utxo.covenant_id.unwrap_or_default();
                next.entry(cov).or_insert_with(|| next_states.iter().cloned().map(TokenState::Kron).collect());
                token_in(&mut d, i, &TokenState::Kron(state.clone()), cov, value, me, units)
            }
            SigPlan::Router { actor, entry, .. } => format!("in  #{i}  router {actor} `{entry}` ({})", kas(value)),
            SigPlan::RetiredCancel { template, retired, .. } => format!(
                "in  #{i}  order {} {} `cancel` (older contract version {}, cancel only; {} on it)",
                template.name(),
                inp.utxo.covenant_id.map(|c| show_id(&c, "")).unwrap_or_default(),
                kob_protocol::json::to_hex(retired),
                kas(value)
            ),
        };
        d.inputs.push(line);
    }
    // Orders spent by the transaction. An output under one of their covenant ids continues it.
    let spent_orders: BTreeMap<[u8; 32], String> = built
        .tx
        .inputs
        .iter()
        .zip(&built.plans)
        .filter_map(|(i, p)| match p {
            SigPlan::Entry { template, .. } | SigPlan::RetiredCancel { template, .. } => {
                i.utxo.covenant_id.map(|c| (c, template.name().to_string()))
            }
            _ => None,
        })
        .collect();
    let created: BTreeMap<u32, (&kob_protocol::tx::NewCovenant, Option<String>)> = built
        .covenants
        .iter()
        .flat_map(|c| c.outputs.iter().map(move |o| (*o, (c, c.template.map(|t| t.name().to_string())))))
        .collect();
    let mut seen: BTreeMap<[u8; 32], usize> = BTreeMap::new();
    for (o, out) in built.tx.outputs.iter().enumerate() {
        let value = out.value as i128;
        let line = if out.script_public_key == my_spk && out.covenant.is_none() {
            d.kas_net += value;
            format!("out #{o}  {} to you", kas(value))
        } else if let Some(c) = &out.covenant {
            let k = seen.entry(c.covenant_id).or_default();
            let st = next.get(&c.covenant_id).and_then(|v| v.get(*k)).cloned();
            *k += 1;
            match (created.get(&(o as u32)), st) {
                (Some((nc, Some(t))), _) if !t.starts_with("KCC20") && !t.starts_with("Kron") => {
                    format!("out #{o}  new order {t} {} ({} on it)", show_id(&nc.covenant_id, ""), kas(value))
                }
                (_, Some(st)) => {
                    let who = owner_of(&st, me);
                    if st.is_user() && st.owner() == *me {
                        *d.token_net.entry(c.covenant_id).or_default() += st.amount() as i128;
                    }
                    format!("out #{o}  {} to {who} (carrier {})", tok(units, &c.covenant_id, st.amount() as i128), kas(value))
                }
                _ => match spent_orders.get(&c.covenant_id) {
                    Some(t) => format!("out #{o}  order {t} {} continues ({} on it)", show_id(&c.covenant_id, ""), kas(value)),
                    None => format!("out #{o}  covenant {} ({})", show_id(&c.covenant_id, ""), kas(value)),
                },
            }
        } else {
            format!("out #{o}  {} to script {}", kas(value), out.script_public_key)
        };
        d.outputs.push(line);
    }
    // The price each placed or amended plain order trades at, from its recorded state: a fixed price, or an auction
    // with its start and its limit, never the `price` field alone (R9FC2-1).
    let placed =
        kob_protocol::payload::recover_orders(&built.tx).map(|v| v.into_iter().map(|r| (r.output, r.order)).collect::<Vec<_>>());
    let amended = kob_protocol::payload::recover_amends_planned(&built.tx, &built.plans)
        .map(|v| v.into_iter().map(|r| (r.output, r.order)).collect::<Vec<_>>());
    for (o, st) in placed.unwrap_or_default().into_iter().chain(amended.unwrap_or_default()) {
        if let (Some(line), Some(t)) = (price_text(&st, units), d.outputs.get_mut(o as usize)) {
            t.push_str(&format!("; {line}"));
        }
    }
    d.token_net.retain(|_, v| *v != 0);
    d
}

/// How a plain order prices (R9FC2-1): `trades at P (fixed)`, or an auction from its start to its limit.
fn price_text(s: &kob_protocol::state::AnyState, units: &Units) -> Option<String> {
    use kob_protocol::state::AnyState;
    let (x_price, slope, end, step, from, interval, scale, cov) = match s {
        AnyState::KobAsk(x) | AnyState::KobAskKron(x) => {
            (x.price, x.slope, x.price_end, x.decay_step, x.active_from, x.interval, x.scale, x.token_cov_id)
        }
        AnyState::KobBid(x) | AnyState::KobBidKron(x) => {
            (x.price, x.slope, x.price_end, x.decay_step, x.active_from, x.interval, x.scale, x.token_cov_id)
        }
        _ => return None,
    };
    let u = units.get(&cov).cloned().unwrap_or(TokenUnit { ticker: String::new(), decimals: 0 });
    let px = |p: i64| super::units::price_per_whole(p, &u, scale, super::units::KAS_DECIMALS, "KAS");
    let slices = if interval > 0 { format!(", in slices every {interval} DAA") } else { String::new() };
    Some(if slope == 0 {
        format!("trades at {} (a fixed price{slices})", px(x_price))
    } else {
        format!("an auction from {} to its limit {} ({} per {step} DAA from DAA {from}{slices})", px(x_price), px(end), px(slope))
    })
}

fn token_in(d: &mut Decoded, i: usize, st: &TokenState, cov: [u8; 32], value: i128, me: &[u8; 32], units: &Units) -> String {
    if st.is_user() && st.owner() == *me {
        *d.token_net.entry(cov).or_default() -= st.amount() as i128;
    }
    format!("in  #{i}  {} of {} (carrier {})", tok(units, &cov, st.amount() as i128), owner_of(st, me), kas(value))
}

/// The lines of the decoded transaction and its net effect.
pub fn lines(d: &Decoded, units: &Units, txid: &[u8; 32]) -> Vec<String> {
    let mut v = vec![format!("transaction {} (unsigned): {} inputs, {} outputs", to_hex(txid), d.inputs.len(), d.outputs.len())];
    v.extend(d.inputs.iter().map(|l| format!("  {l}")));
    v.extend(d.outputs.iter().map(|l| format!("  {l}")));
    v.push(format!(
        "network fee: {}{}",
        kas(d.fee as i128),
        if d.priority_fee { " (priority fee, storage mass included)" } else { "" }
    ));
    if d.unsigned_inputs > 0 {
        v.push(format!(
            "  {} input{} that no signature of yours covers hold {} as the node reports it: the fee above is exact if the node is (a node \n             that reports less makes the true fee higher by the difference)",
            d.unsigned_inputs,
            if d.unsigned_inputs == 1 { "" } else { "s" },
            kas(d.unsigned_kas)
        ));
    }
    let mut net = vec![format!("{}{}", if d.kas_net >= 0 { "+" } else { "" }, kas(d.kas_net))];
    for (cov, n) in &d.token_net {
        net.push(format!("{}{}", if *n >= 0 { "+" } else { "" }, tok(units, cov, *n)));
    }
    v.push(format!("net effect on your wallet: {}", net.join(", ")));
    v.push(format!(
        "signatures: {} by your key{}",
        d.signatures,
        if d.foreign_signatures > 0 { format!(", {} by other keys", d.foreign_signatures) } else { String::new() }
    ));
    v
}

/// The decoded transaction as JSON.
pub fn json_of(d: &Decoded, built: &BuiltTx) -> Value {
    let mut v = json!({
        "txid": to_hex(&built.tx.id),
        "inputs": d.inputs,
        "outputs": d.outputs,
        "fee": d.fee.to_string(),
        "net": {
            "kas": d.kas_net.to_string(),
            "tokens": d.token_net.iter().map(|(c, n)| (to_hex(c), Value::String(n.to_string()))).collect::<serde_json::Map<_, _>>(),
        },
        "signatures": d.signatures,
    });
    // Only a priority fee is named, so the document of a relay-floor plan is unchanged.
    if d.priority_fee {
        v["fee_mode"] = json!("priority");
    }
    v
}

/// The custody rules a decoded transaction must keep, the CLI's second line after the builder (R8FX2-6).
///
/// It reads only the transaction and its signing plan. For every order the transaction spends
/// through its maker's `cancel` entry (the one entry no covenant rule guards beyond the signature):
/// - a spend that ends the order releases exactly the custodies its state holds (token, owner,
///   amount, class), and its bought-custody;
/// - a spend that keeps the order alive (a sweep, an amend, a claim) spends no unit of exactly a
///   custody's state, since nothing re-creates it.
///
/// Every other entry (settle, fill, refund, update) is held by its covenant. Each string is one
/// broken rule; empty means the transaction keeps them.
pub fn custody_issues(built: &BuiltTx) -> Vec<String> {
    use kob_protocol::build::{check_releases_held, covenant_accepts_as, curve_bound, held_custodies};
    use kob_protocol::state::AnyState;
    use kob_protocol::tx::{TokenUtxo, Utxo};
    let tokens: Vec<TokenUtxo> = built
        .tx
        .inputs
        .iter()
        .zip(&built.plans)
        .filter_map(|(inp, plan)| {
            let state = match plan {
                SigPlan::TokenLeader { state, .. } | SigPlan::TokenDelegator { state, .. } => TokenState::Kcc20(state.clone()),
                SigPlan::KronToken { state, .. } => TokenState::Kron(state.clone()),
                _ => return None,
            };
            let utxo = Utxo {
                transaction_id: inp.transaction_id,
                index: inp.index,
                amount: inp.utxo.amount,
                block_daa_score: inp.utxo.block_daa_score,
                covenant_id: inp.utxo.covenant_id,
            };
            Some(TokenUtxo { utxo, state })
        })
        .collect();
    let mut issues = vec![];
    for (i, (inp, plan)) in built.tx.inputs.iter().zip(&built.plans).enumerate() {
        let (template, bytes) = match plan {
            SigPlan::Entry { template, state, entry, .. } if entry == "cancel" => (*template, state),
            SigPlan::RetiredCancel { template, state, .. } => (*template, state),
            _ => continue,
        };
        let Some(cov) = inp.utxo.covenant_id else { continue };
        let Ok(state) = AnyState::decode(template, bytes) else {
            issues.push(format!("input {i}: the order's state does not decode, so its custody cannot be checked"));
            continue;
        };
        // an unbound curve names its token from its custody, the output of its own genesis it owns
        let owned: Vec<TokenUtxo> = tokens.iter().filter(|t| t.state.is_covenant_owned() && t.state.owner() == cov).cloned().collect();
        let state = match &state {
            AnyState::KobCurveAsk(c) if c.is_unbound() => match owned.first() {
                Some(t) => AnyState::KobCurveAsk(curve_bound(c, t)),
                None => state,
            },
            _ => state,
        };
        let held = match held_custodies(&state, cov) {
            Ok(h) => h,
            Err(e) => {
                issues.push(format!("input {i}: the order's custodies cannot be named: {e}"));
                continue;
            }
        };
        // continued only by an output bound to the id from this very input, and not a genesis on chain (R9FB2-1)
        let continues = built.tx.outputs.iter().enumerate().any(|(k, o)| {
            o.covenant.as_ref().is_some_and(|c| c.covenant_id == cov && c.authorizing_input as usize == i)
                && kob_protocol::payload::check_continues(&built.tx, k, cov).is_ok()
        });
        if continues {
            for h in &held {
                // the covenant's comparison, the borrow guard of a sell custody ignored (R9FA-1)
                if !owned.iter().any(|t| covenant_accepts_as(h, t)) {
                    continue;
                }
                // a claim moves the bought-custody, and only with a continuation that holds none (R9FC-4)
                let claimed = h.bought
                    && state.with_bought(0).is_some_and(|after| {
                        let spk = spk_to_string(&after.spk());
                        built
                            .tx
                            .outputs
                            .iter()
                            .any(|o| o.script_public_key == spk && o.covenant.as_ref().is_some_and(|c| c.covenant_id == cov))
                    });
                if !claimed {
                    issues.push(format!(
                        "input {i}: order {} stays live but this transaction spends a unit its covenant takes as its {}: the order would be left without it",
                        show_id(&cov, ""),
                        if h.bought { "bought-custody" } else { "custody" }
                    ));
                }
            }
        } else if let Err(e) = check_releases_held(&state, cov, &owned) {
            issues.push(format!("input {i}: order {} ends: {e}", show_id(&cov, "")));
        }
    }
    issues
}
