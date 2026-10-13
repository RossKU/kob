//! What a transaction does, decoded from the unsigned transaction and its signing plan alone (like the web app's confirm
//! screen, `web/src/kob/decode.ts`): every input and output with its owner, the net effect on the wallet (KAS and each
//! token), the fee and the signatures asked for.

use std::collections::BTreeMap;

use kob_protocol::json::to_hex;
use kob_protocol::script::p2pk_spk;
use kob_protocol::state::TokenState;
use kob_protocol::tx::{spk_to_string, BuiltTx, SigPlan};
use serde_json::{json, Value};

use super::market::show_id;
use super::units::{fmt_decimal, kas, TokenUnit};

/// Display facts of the tokens a transaction may touch.
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

/// The decoded transaction.
#[derive(Debug, Clone)]
pub struct Decoded {
    /// One line per input.
    pub inputs: Vec<String>,
    /// One line per output.
    pub outputs: Vec<String>,
    /// KAS into (+) or out of (-) the wallet's plain UTXOs, fee included.
    pub kas_net: i128,
    /// Tokens into (+) or out of (-) the wallet's key-owned token UTXOs.
    pub token_net: BTreeMap<[u8; 32], i128>,
    /// Network fee.
    pub fee: u64,
    /// Inputs the wallet key signs.
    pub signatures: usize,
    /// Signatures of other keys (refused by the signer).
    pub foreign_signatures: usize,
}

/// Decode `built` from the point of view of the key `me`.
pub fn decode(built: &BuiltTx, me: &[u8; 32], units: &Units) -> Decoded {
    let my_spk = spk_to_string(&p2pk_spk(me));
    let mut d = Decoded {
        inputs: vec![],
        outputs: vec![],
        kas_net: 0,
        token_net: BTreeMap::new(),
        fee: built.fee.fee,
        signatures: built.sign.iter().filter(|s| s.pubkey == *me).count(),
        foreign_signatures: built.sign.iter().filter(|s| s.pubkey != *me).count(),
    };
    // token output states per token, in output order (the leader / KRON plans carry the next states)
    let mut next: BTreeMap<[u8; 32], Vec<TokenState>> = BTreeMap::new();
    for (i, (inp, plan)) in built.tx.inputs.iter().zip(&built.plans).enumerate() {
        let value = inp.utxo.amount as i128;
        let line = match plan {
            SigPlan::P2pk { pubkey } => {
                if pubkey == me {
                    d.kas_net -= value;
                    format!("in  #{i}  your KAS {}", kas(value))
                } else {
                    format!("in  #{i}  KAS of key {} {}", show_id(pubkey, ""), kas(value))
                }
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
        };
        d.inputs.push(line);
    }
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
                _ => format!("out #{o}  covenant {} ({})", show_id(&c.covenant_id, ""), kas(value)),
            }
        } else {
            format!("out #{o}  {} to script {}", kas(value), short_script(&out.script_public_key))
        };
        d.outputs.push(line);
    }
    d.token_net.retain(|_, v| *v != 0);
    d
}

fn short_script(s: &str) -> String {
    if s.len() > 24 {
        format!("{}..{}", &s[..12], &s[s.len() - 8..])
    } else {
        s.to_string()
    }
}

fn token_in(d: &mut Decoded, i: usize, st: &TokenState, cov: [u8; 32], value: i128, me: &[u8; 32], units: &Units) -> String {
    if st.is_user() && st.owner() == *me {
        *d.token_net.entry(cov).or_default() -= st.amount() as i128;
    }
    format!("in  #{i}  {} of {} (carrier {})", tok(units, &cov, st.amount() as i128), owner_of(st, me), kas(value))
}

/// Lines of the decoded transaction and the net effect.
pub fn lines(d: &Decoded, units: &Units, txid: &[u8; 32]) -> Vec<String> {
    let mut v = vec![format!("transaction {} (unsigned): {} inputs, {} outputs", to_hex(txid), d.inputs.len(), d.outputs.len())];
    v.extend(d.inputs.iter().map(|l| format!("  {l}")));
    v.extend(d.outputs.iter().map(|l| format!("  {l}")));
    v.push(format!("network fee: {}", kas(d.fee as i128)));
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
    json!({
        "txid": to_hex(&built.tx.id),
        "inputs": d.inputs,
        "outputs": d.outputs,
        "fee": d.fee.to_string(),
        "net": {
            "kas": d.kas_net.to_string(),
            "tokens": d.token_net.iter().map(|(c, n)| (to_hex(c), Value::String(n.to_string()))).collect::<serde_json::Map<_, _>>(),
        },
        "signatures": d.signatures,
    })
}
