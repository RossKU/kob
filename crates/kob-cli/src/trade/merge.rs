//! `kob merge <TOKEN>`: merge the wallet's own plain token UTXOs of one token into one (the answer to
//! `TOKEN_UTXOS_FRAGMENTED`), a port of `web/src/kob/merge.ts`:
//!
//! * the inputs: token UTXOs the key owns by itself, plain, of the token's extension commitment (never an order's custody
//!   or stray), each confirmed on the node; smallest first, ties by outpoint;
//! * a CHAIN of transfers to the key: the first takes up to the program's token inputs, every next one the previous
//!   output plus up to inputs - 1 more, at most 10 transactions; each is a plain transfer (`sendTokens`) valid on its own;
//! * the merged output keeps the largest input carrier (at most 2 KAS, at least the program's floor); the freed carriers
//!   pay the fee, a KAS UTXO of the key is added only when they do not cover it (and always for KRON, whose transfer
//!   needs a P2PK input of the owner).
//!
//! The whole chain is built before anything is signed (each later link against the previous link's transaction id);
//! after one confirmation every link is signed, validated in the script engine and submitted in order, a later link
//! rebuilt on the node's view of the previous output once the node shows it (else on the submitted transaction).

use std::collections::BTreeSet;
use std::time::{Duration, Instant};

use kob_protocol::artifacts::token_template;
use kob_protocol::build::{build, Action, SendTokens, TokenRecipient, TokenRef};
use kob_protocol::defaults::DEFAULT_ORDER_CARRIER;
use kob_protocol::json::to_hex;
use kob_protocol::state::TokenState;
use kob_protocol::tx::{spk_to_string, BuiltTx, FeeOptions, KeyUtxo, TokenUtxo, Utxo};
use kob_protocol::Family;
use serde_json::json;

use super::data;
use super::market::{self, TokenMarket};
use super::opts::{confirm, parse, Common, COMMON_HELP};
use super::plan::{token_carrier_floor, usable_token_utxos, MAX_TOKEN_INPUTS};
use super::show::{self, Units};
use super::units;
use crate::common::{sign_validate_send, spk_bytes, Signer};

/// Help of `kob merge`.
pub const MERGE_USAGE: &str = "\
kob merge: merge the key's own token UTXOs of one token into one (a chain of transfers to the key itself)

USAGE
  kob merge <TOKEN> [--wait-secs N] [options]

  Takes the key's plain UTXOs of TOKEN (smallest first): the first transfer up to the program's token inputs, every next
  one the previous output plus up to inputs - 1 more, at most 10 transactions per run (run again for more). The merged
  output keeps the largest input carrier (at most 2 KAS); the freed carriers pay the fee. One confirmation signs the
  whole chain; each later link waits up to --wait-secs (default 30) for the node to show the previous output, then is
  rebuilt on it (else it spends the submitted, still unconfirmed output).
";

/// Most transactions one merge runs.
pub const MAX_MERGE_TXS: usize = 10;
const ONE_KAS: u64 = 100_000_000;
const FUNDING_HEADROOM: u64 = 500_000;

/// One link of the chain.
#[derive(Debug, Clone)]
pub struct Link {
    /// Token inputs (from the second link on, the previous link's output first).
    pub inputs: Vec<TokenUtxo>,
    /// Inputs of the key's original set this link merges.
    pub fresh: usize,
    /// The transaction.
    pub built: BuiltTx,
    /// KAS UTXOs of the key spent.
    pub funding: Vec<KeyUtxo>,
    /// The merged output: index, carrier, state.
    pub output: (u32, u64, TokenState),
}

fn outpoint(u: &Utxo) -> ([u8; 32], u32) {
    (u.transaction_id, u.index)
}

/// The UTXOs a merge may spend: plain UTXOs of `m` the key owns by itself (its extension commitment), positive; smallest
/// first, ties by outpoint (`mergeCandidates`).
pub fn candidates(m: &TokenMarket, maker: &[u8; 32], utxos: &[TokenUtxo]) -> Vec<TokenUtxo> {
    let mut v: Vec<TokenUtxo> = usable_token_utxos(m, maker, utxos).into_iter().filter(|u| u.state.amount() > 0).collect();
    v.sort_by(|a, b| a.state.amount().cmp(&b.state.amount()).then(outpoint(&a.utxo).cmp(&outpoint(&b.utxo))));
    v
}

/// The fresh inputs of each link over `sorted`: the first up to `max_inputs`, each next up to `max_inputs - 1`, at most
/// `max_txs` links, the merged amount never above `cap` (`mergeGroups`). Fewer than two candidates: no link.
pub fn groups(sorted: &[TokenUtxo], max_inputs: usize, max_txs: usize, cap: i64) -> Vec<Vec<TokenUtxo>> {
    let mut out: Vec<Vec<TokenUtxo>> = vec![];
    if max_inputs < 2 {
        return out;
    }
    let (mut total, mut usable) = (0i128, 0usize);
    for u in sorted {
        if total + u.state.amount() as i128 > cap as i128 {
            break;
        }
        total += u.state.amount() as i128;
        usable += 1;
    }
    let mut at = 0;
    while out.len() < max_txs {
        let first = out.is_empty();
        let k = (if first { max_inputs } else { max_inputs - 1 }).min(usable - at);
        if k < if first { 2 } else { 1 } {
            break;
        }
        out.push(sorted[at..at + k].to_vec());
        at += k;
    }
    out
}

/// KAS the merged output carries: the largest input carrier, at most the default carrier, at least the program's floor.
pub fn merged_carrier(m: &TokenMarket, inputs: &[TokenUtxo]) -> u64 {
    let largest = inputs.iter().map(|u| u.utxo.amount).max().unwrap_or(0);
    largest.min(DEFAULT_ORDER_CARRIER).max(token_carrier_floor(m) as u64)
}

/// One link: `inputs` (all of the key, one token) into ONE output of the key holding their whole amount; the freed
/// carriers pay the fee, else KAS UTXOs of the key not in `spent` (`buildMergeLink`).
pub fn build_link(
    m: &TokenMarket,
    maker: &[u8; 32],
    inputs: Vec<TokenUtxo>,
    fresh: usize,
    pool: &[KeyUtxo],
    spent: &BTreeSet<([u8; 32], u32)>,
    fee: &FeeOptions,
) -> Result<Link, String> {
    let total: i64 = inputs.iter().map(|u| u.state.amount()).sum();
    let carrier = merged_carrier(m, &inputs);
    let kron = m.family == Family::Kron;
    let mut pool: Vec<KeyUtxo> = pool
        .iter()
        .filter(|f| f.pubkey == *maker && f.utxo.covenant_id.is_none() && !spent.contains(&outpoint(&f.utxo)))
        .cloned()
        .collect();
    pool.sort_by(|a, b| b.utxo.amount.cmp(&a.utxo.amount));
    // KRON: the owner's address presence is a P2PK input (the smallest of at least 1 KAS, else the largest)
    let seed = if kron { pool.iter().rev().find(|f| f.utxo.amount >= ONE_KAS).or(pool.first()).cloned() } else { None };
    if kron && seed.is_none() {
        return Err("merging KRON tokens needs a KAS UTXO of the key in the transaction".into());
    }
    let ext = if kron { [0; 32] } else { inputs.first().map(|u| u.state.extension()).unwrap_or(m.ext) };
    let state = TokenState::user(m.program, total, *maker, ext);
    let spk = spk_to_string(&state.spk_with(token_template(m.program)));
    let mut used: Vec<KeyUtxo> = seed.into_iter().collect();
    for _ in 0..8 {
        let req = SendTokens {
            token: TokenRef { covenant_id: m.cov, program: m.program },
            tokens: inputs.clone(),
            recipients: vec![TokenRecipient { pubkey: *maker, amount: total, carrier }],
            token_change: None,
            token_change_carrier: carrier,
            funding: used.clone(),
            change: Some(*maker),
            records: vec![],
            fee: fee.clone(),
        };
        match build(&Action::SendTokens(req)) {
            Ok(built) => {
                let outs: Vec<(usize, &kob_protocol::tx::TxOutputJson)> = built
                    .tx
                    .outputs
                    .iter()
                    .enumerate()
                    .filter(|(_, o)| o.covenant.as_ref().is_some_and(|c| c.covenant_id == m.cov))
                    .collect();
                let [(index, out)] = outs.as_slice() else {
                    return Err("the built transfer does not end in one token output of the key".into());
                };
                if out.script_public_key != spk {
                    return Err("the built transfer does not end in one token output of the key holding the whole amount".into());
                }
                let o = (*index as u32, out.value, state.clone());
                return Ok(Link { inputs, fresh, built, funding: used, output: o });
            }
            Err(kob_protocol::Error::InsufficientFunds { need, have }) => {
                let deficit = need.saturating_sub(have) + FUNDING_HEADROOM;
                let rest: Vec<&KeyUtxo> = pool.iter().filter(|p| !used.contains(p)).collect();
                let Some(first) = rest.first() else {
                    return Err(format!(
                        "not enough KAS for the network fee: {} short",
                        units::kas((deficit - FUNDING_HEADROOM) as i128)
                    ));
                };
                let pick = rest.iter().rev().find(|p| p.utxo.amount >= deficit).copied().unwrap_or(first);
                used.push(pick.clone());
            }
            Err(e) => return Err(format!("the protocol refused the transfer: {e}")),
        }
    }
    Err("could not fund the network fee within 8 attempts".into())
}

/// The merged output of `link` as the next link's input, once its transaction `txid` is known (`node`: the node's view
/// of it: amount and DAA score).
pub fn carry(link: &Link, m: &TokenMarket, txid: [u8; 32], node: Option<(u64, u64)>) -> TokenUtxo {
    TokenUtxo {
        utxo: Utxo {
            transaction_id: txid,
            index: link.output.0,
            amount: node.map(|n| n.0).unwrap_or(link.output.1),
            block_daa_score: node.map(|n| n.1).unwrap_or(0),
            covenant_id: Some(m.cov),
        },
        state: link.output.2.clone(),
    }
}

/// The whole chain over the key's UTXOs of `m`, every later link against the previous link's transaction id (`planMerge`).
pub fn plan(
    m: &TokenMarket,
    maker: &[u8; 32],
    utxos: &[TokenUtxo],
    funding: &[KeyUtxo],
    fee: &FeeOptions,
) -> Result<Vec<Link>, String> {
    let cands = candidates(m, maker, utxos);
    let cap = if m.family == Family::Kron { kob_protocol::registry::KRON_MAX_OUTPUT_AMOUNT } else { i64::MAX };
    let gs = groups(&cands, m.slots.0.min(MAX_TOKEN_INPUTS), MAX_MERGE_TXS, cap);
    if gs.is_empty() {
        return Err(format!("fewer than two token UTXOs of {} can be merged ({} found)", m.name(), cands.len()));
    }
    let mut links: Vec<Link> = vec![];
    let mut spent = BTreeSet::new();
    for g in gs {
        let n = g.len();
        let inputs = match links.last() {
            Some(prev) => std::iter::once(carry(prev, m, prev.built.tx.id, None)).chain(g).collect(),
            None => g,
        };
        let l = build_link(m, maker, inputs, n, funding, &spent, fee)?;
        spent.extend(l.funding.iter().map(|f| outpoint(&f.utxo)));
        links.push(l);
    }
    Ok(links)
}

/// Run `kob merge`.
pub fn run(args: &[String]) -> i32 {
    if args.iter().any(|a| a == "--help" || a == "-h") {
        print!("{MERGE_USAGE}{COMMON_HELP}");
        return 0;
    }
    let fail = |e: String, code: i32| {
        eprintln!("error: {e}");
        code
    };
    let p = match parse(args, &["--wait-secs"], &[]) {
        Ok(p) => p,
        Err(e) => return fail(e, 2),
    };
    let setup = || -> Result<(Common, Signer, u64), String> {
        let c = Common::from(&p)?;
        let s = c.signer(args)?;
        let wait = match p.get("--wait-secs") {
            Some(v) => v.parse::<u64>().ok().filter(|w| *w <= 3_600).ok_or(format!("--wait-secs must be 0..3600, got `{v}`"))?,
            None => 30,
        };
        Ok((c, s, wait))
    };
    let (c, signer, wait) = match setup() {
        Ok(x) => x,
        Err(e) => return fail(e, 2),
    };
    if let Err(e) = super::flow::check_network_first(&c) {
        return fail(e, 1);
    }
    let [key] = p.pos.as_slice() else { return fail("give exactly one token (ticker or covenant id)".into(), 2) };
    let r = (|| -> Result<i32, String> {
        let node = c.need_node()?;
        let indexer = c.need_indexer()?;
        let (reg, idx) = data::token_lists(c.registry.as_deref(), &c.network, Some(indexer))?;
        let order_scale = |cov: &[u8; 32]| data::order_scale(indexer, cov);
        let remembered = |cov: &[u8; 32]| crate::recent::first_order_decimals(&c.network, cov);
        let declaration = |cov: &[u8; 32]| data::declaration(indexer, cov);
        let held_program = |cov: &[u8; 32]| data::held_program(indexer, node, &c.network, cov, &signer.pubkey);
        let open = market::OpenToken {
            remembered: Some(&remembered),
            declaration: Some(&declaration),
            held_program: Some(&held_program),
            ..market::OpenToken::new(&c.network, &order_scale)
        };
        let m = market::resolve_with(key, &reg, &idx, Some(&open))?;
        let utxos = data::token_utxos(indexer, node, &c.network, &m, &signer.pubkey)?;
        let funding = data::funding(node, &c.network, &signer.pubkey)?;
        let fee = FeeOptions { fee_rate: c.fee_rate, ..Default::default() };
        let links = plan(&m, &signer.pubkey, &utxos, &funding, &fee)?;
        let mut units = Units::new();
        units.insert(m.cov, m.unit());
        let before = candidates(&m, &signer.pubkey, &utxos).len();
        let merged: usize = links.iter().map(|l| l.fresh).sum();
        let amount = links.last().map(|l| l.output.2.amount()).unwrap_or(0);
        let fee_total: u64 = links.iter().map(|l| l.built.fee.fee).sum();
        let summary = json!({
            "action": format!("merge {}", m.name()),
            "network": c.network,
            "token": to_hex(&m.cov),
            "utxos_before": before,
            "utxos_after": before - merged + 1,
            "transactions": links.len(),
            "merged_amount": amount.to_string(),
            "fee": fee_total.to_string(),
        });
        if c.json {
            println!("{}", serde_json::to_string_pretty(&summary).unwrap_or_default());
        } else {
            println!("merge {} of {}", units::tokens(amount as i128, &m.unit()), m.name());
            println!(
                "  {before} UTXOs of the key become {} in {} transaction(s); network fees {}",
                before - merged + 1,
                links.len(),
                units::kas(fee_total as i128)
            );
            for (k, l) in links.iter().enumerate() {
                println!(
                    "  link {}: {} input(s) ({} new), one output of {} carrying {}",
                    k + 1,
                    l.inputs.len(),
                    l.fresh,
                    units::tokens(l.output.2.amount() as i128, &m.unit()),
                    units::kas(l.output.1 as i128)
                );
            }
            let d = show::decode(&links[0].built, &signer.pubkey, &units);
            for l in show::lines(&d, &units, &links[0].built.tx.id) {
                println!("{l}");
            }
        }
        if c.dry_run {
            eprintln!("dry run: nothing signed or submitted");
            return Ok(0);
        }
        if let Err(e) = confirm(&c, &format!("{} merge transaction(s)", links.len())) {
            eprintln!("{e}");
            return Ok(1);
        }
        let mut prev: Option<(Link, [u8; 32])> = None;
        let mut spent: BTreeSet<([u8; 32], u32)> = BTreeSet::new();
        let total = links.len();
        for (k, planned) in links.into_iter().enumerate() {
            // a later link is rebuilt on the previous output as the node shows it (or as submitted)
            let link = match &prev {
                None => planned,
                Some((pl, txid)) => {
                    let seen = wait_for_output(&c, pl, &m, *txid, wait)?;
                    let mut inputs = vec![carry(pl, &m, *txid, seen)];
                    inputs.extend(planned.inputs.into_iter().skip(1));
                    build_link(&m, &signer.pubkey, inputs, planned.fresh, &funding, &spent, &fee)?
                }
            };
            spent.extend(link.funding.iter().map(|f| outpoint(&f.utxo)));
            let file = format!("{}.merge-{}.json", to_hex(&m.cov), k + 1);
            let sent = sign_validate_send(&link.built, &signer, c.out_dir.as_deref(), &file, summary.clone(), false, node, &c.network)
                .map_err(|e| format!("link {} of {total}: {e} (earlier links were submitted; run kob merge again)", k + 1))?;
            if c.json {
                println!("{}", json!({"link": k + 1, "status": "submitted", "txid": sent.txid, "fee": sent.fee.to_string()}));
            } else {
                println!("submitted link {} of {total}: txid {}", k + 1, sent.txid);
            }
            let txid = kob_protocol::registry::parse_hex32(&sent.txid).unwrap_or(link.built.tx.id);
            prev = Some((link, txid));
        }
        Ok(0)
    })();
    match r {
        Ok(code) => code,
        Err(e) => fail(e, 1),
    }
}

/// The node's view (amount, DAA score) of a link's merged output, polled for up to `secs` seconds.
fn wait_for_output(c: &Common, link: &Link, m: &TokenMarket, txid: [u8; 32], secs: u64) -> Result<Option<(u64, u64)>, String> {
    let node = c.need_node()?;
    let spk = link.output.2.spk_with(token_template(m.program));
    let until = Instant::now() + Duration::from_secs(secs);
    loop {
        let at = crate::common::lookup(node, &c.network, std::slice::from_ref(&spk))?;
        if let Some(u) = at.get(&spk_bytes(&spk)).and_then(|v| v.iter().find(|u| u.txid == txid && u.index == link.output.0)) {
            return Ok(Some((u.amount, u.daa)));
        }
        if Instant::now() >= until {
            return Ok(None);
        }
        std::thread::sleep(Duration::from_secs(1));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use kob_protocol::artifacts::TemplateId;

    fn tu(m: &TokenMarket, n: u8, amount: i64, carrier: u64) -> TokenUtxo {
        TokenUtxo {
            utxo: Utxo { transaction_id: [n; 32], index: 0, amount: carrier, block_daa_score: 1, covenant_id: Some(m.cov) },
            state: TokenState::user(m.family, amount, [0x11; 32], m.ext),
        }
    }

    #[test]
    fn groups_and_carriers() {
        let m = TokenMarket::new([0x70; 32], "TST", 3, TemplateId::Kcc20Ref, Some([0xee; 32]), None).unwrap();
        let us: Vec<TokenUtxo> = (1..=8).map(|n| tu(&m, n, n as i64 * 10, 30_000_000 * n as u64)).collect();
        // 3 inputs, then the previous output + 2: 3 + 2 + 2 + 1
        let g = groups(&us, 3, 10, i64::MAX);
        assert_eq!(g.iter().map(Vec::len).collect::<Vec<_>>(), vec![3, 2, 2, 1]);
        assert_eq!(groups(&us, 3, 2, i64::MAX).iter().map(Vec::len).collect::<Vec<_>>(), vec![3, 2]);
        // the cap: only the prefix whose sum fits (10 + 20 + 30 + 40 = 100)
        assert_eq!(groups(&us, 3, 10, 100).iter().map(Vec::len).collect::<Vec<_>>(), vec![3, 1]);
        assert!(groups(&us[..1], 3, 10, i64::MAX).is_empty());
        // the largest input carrier, at most 2 KAS, at least the floor
        assert_eq!(merged_carrier(&m, &us[..2]), 60_000_000);
        assert_eq!(merged_carrier(&m, &us), DEFAULT_ORDER_CARRIER);
        assert_eq!(merged_carrier(&m, &[tu(&m, 9, 1, 1)]), token_carrier_floor(&m) as u64);
    }
}
