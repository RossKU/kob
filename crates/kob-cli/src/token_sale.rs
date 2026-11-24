//! "After creation" of `kob token create`, as the web app's Create token has it: keep everything
//! (the default), sell at one price or sell on a curve, with `--keep` percent of the share kept in
//! the wallet.
//!
//! The sale takes the funding key's share of the supply (its first scheme-0 holder). A curve is a
//! `KobCurveAsk` created in the genesis (unbound, its custody a scheme-0x04 holder). One price is a
//! limit sell (`KobAsk`, GTC) placed right after the genesis, spending the genesis output for sale
//! and the preparation's change. Everything is planned and signed before the one confirmation.

use kob_protocol::artifacts::token_template_by_hash;
use kob_protocol::defaults::{curve_min_fill, curve_table, default_scale, CurveSpec, CurveTable};
use kob_protocol::issue::{p2pk_script, CreationPlan, GenesisCurve, Holder, IssueSpec, Program, SCHEME_COVENANT_ID};
use kob_protocol::state::{CurveAskState, Kcc20State, TokenState};
use kob_protocol::tx::{BuiltTx, KeyUtxo, TokenUtxo, Utxo};

use crate::trade::market::{DecimalsFrom, TokenMarket};
use crate::trade::plan::{plan_order, Clock, Crossing, Env, Intent, Lifetime, LimitIntent, Plan, Severity, Side};
use crate::trade::units::{parse_kas, parse_kas_price, TokenUnit};

/// The flags of the sale, each taking a value.
pub const SALE_FLAGS: [&str; 8] = ["--sell", "--keep", "--price", "--levels", "--total", "--multiple", "--shape", "--octaves"];

/// What a sell-out realises by default, and what a one-price sale asks for its amount when no
/// `--price` is given: 10,000 KAS (the web's defaults).
const DEFAULT_TOTAL_SOMPI: i128 = 1_000_000_000_000;

/// The sale flags as given.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SaleArgs {
    /// `--sell price|curve` (none: keep everything).
    pub mode: Option<String>,
    /// `--keep PERCENT` of the share kept in the wallet (default 0).
    pub keep: Option<String>,
    /// `--price P`: KAS per whole token of a one-price sale.
    pub price: Option<String>,
    /// `--levels N` of a curve.
    pub levels: Option<String>,
    /// `--total KAS` a curve's sell-out realises.
    pub total: Option<String>,
    /// `--multiple M`: a curve's last level over its first.
    pub multiple: Option<String>,
    /// `--shape linear|chromatic` and `--octaves N` of a curve.
    pub shape: Option<String>,
    pub octaves: Option<String>,
    /// `--lock-until`, resolved to unix ms: the kept share is locked until then
    /// (`crate::token_lock`).
    pub lock_until: Option<u64>,
    /// `--lock-manual`: the lock opens to a signature of the key (`kob token unlock` signs), not by
    /// itself (the default: anyone may release it to the key, `kob_protocol::lock_auto`).
    pub lock_manual: bool,
}

impl SaleArgs {
    /// Stores `value` of `flag`, one of [`SALE_FLAGS`].
    pub fn set(&mut self, flag: &str, value: String) {
        let slot = match flag {
            "--sell" => &mut self.mode,
            "--keep" => &mut self.keep,
            "--price" => &mut self.price,
            "--levels" => &mut self.levels,
            "--total" => &mut self.total,
            "--shape" => &mut self.shape,
            "--octaves" => &mut self.octaves,
            _ => &mut self.multiple,
        };
        *slot = Some(value);
    }

    /// Refuses a mode other than `price` and `curve`, and a flag that does not apply to the mode.
    pub fn check(&self) -> Result<(), String> {
        let curve_only = [
            ("--levels", &self.levels),
            ("--total", &self.total),
            ("--multiple", &self.multiple),
            ("--shape", &self.shape),
            ("--octaves", &self.octaves),
        ];
        let given = |xs: &[(&'static str, &Option<String>)]| xs.iter().find(|(_, v)| v.is_some()).map(|(k, _)| *k);
        match self.mode.as_deref() {
            None => {
                let keep = if self.lock_until.is_some() { None } else { given(&[("--keep", &self.keep)]) };
                match keep.or_else(|| given(&[("--price", &self.price)])).or_else(|| given(&curve_only)) {
                    Some(k) => Err(format!(
                        "{k} goes with --sell price|curve{} (without --sell the wallet keeps everything)",
                        if k == "--keep" { " or --lock-until" } else { "" }
                    )),
                    None => Ok(()),
                }
            }
            Some("price") => match given(&curve_only) {
                Some(k) => Err(format!("{k} is for --sell curve")),
                None => Ok(()),
            },
            Some("curve") => match self.price {
                Some(_) => Err("--price is for --sell price (a curve's prices come from --total and --multiple)".into()),
                None => Ok(()),
            },
            Some(o) => Err(format!("--sell must be price or curve, got `{o}`")),
        }
    }
}

/// A planned sale of a creation.
#[derive(Debug, Clone)]
pub struct Sale {
    /// The holder (genesis output) that holds the part for sale: the curve's custody, or the
    /// wallet output the limit sell spends.
    pub at: usize,
    /// Base units for sale and kept in the wallet.
    pub sell: u64,
    pub keep: u64,
    /// A one-price sale: sompi per `scale` base units.
    pub price: Option<i64>,
    /// A curve: its table and state.
    pub curve: Option<(CurveTable, CurveAskState)>,
}

/// `--keep` as basis points: a percent from 0 to 100, at most two decimals (`12.5`, `12.5%`).
pub fn parse_keep(v: &str) -> Result<u64, String> {
    let bad = || format!("--keep is a percent from 0 to 100 with at most two decimals, got `{v}`");
    let t = v.strip_suffix('%').unwrap_or(v);
    let (int, frac) = t.split_once('.').unwrap_or((t, ""));
    if int.is_empty() || frac.len() > 2 || !int.bytes().chain(frac.bytes()).all(|b| b.is_ascii_digit()) || int.len() > 3 {
        return Err(bad());
    }
    let bps = int.parse::<u64>().map_err(|_| bad())? * 100 + format!("{frac:0<2}").parse::<u64>().map_err(|_| bad())?;
    if bps > 10_000 {
        return Err(bad());
    }
    Ok(bps)
}

/// Applies the sale of `s` to `spec`: splits the share of `funder` into the part for sale and
/// the part kept, and puts a curve into the genesis. `None` keeps everything.
pub fn apply(spec: &mut IssueSpec, s: &SaleArgs, funder: [u8; 32]) -> Result<Option<Sale>, String> {
    s.check()?;
    let Some(mode) = s.mode.as_deref() else { return Ok(None) };
    let at = spec
        .holders
        .iter()
        .position(|h| h.owner == funder && h.owner_scheme == 0)
        .ok_or("--sell: the funding key receives no tokens to sell (give it a share: no --holder, or --holder self:0:AMOUNT)")?;
    let share = spec.holders[at].clone();
    if share.borrow_scheme != 0 {
        return Err("--sell: the funding key's share has borrowing enabled; a sale takes a share with borrowing disabled".into());
    }
    let bps = s.keep.as_deref().map(parse_keep).transpose()?.unwrap_or(0);
    let sell = (share.amount as u128 * (10_000 - bps) as u128 / 10_000) as u64;
    let keep = share.amount - sell;
    if sell == 0 {
        return Err("--sell: nothing is left for sale: lower --keep".into());
    }
    let amount = i64::try_from(sell).map_err(|_| "--sell: the amount for sale is too large".to_string())?;
    let decimals = u32::from(spec.decimals);
    let scale = default_scale(decimals);
    let kept = (keep > 0).then(|| Holder { amount: keep, ..share.clone() });
    let (holder, price, curve) = match mode {
        "price" => {
            let unit = TokenUnit { ticker: spec.ticker.clone(), decimals };
            let price = match &s.price {
                Some(v) => parse_kas_price(v, &unit, scale, "--price", false)?,
                // 10,000 KAS over the amount, at least 1 sompi (the web's default price).
                None => (DEFAULT_TOTAL_SOMPI * scale as i128 / sell as i128).clamp(1, i64::MAX as i128) as i64,
            };
            (Holder { amount: sell, ..share }, Some(price), None)
        }
        _ => {
            let mut c = CurveSpec::defaults(amount, scale);
            if let Some(v) = &s.levels {
                c.levels = v.parse().map_err(|_| format!("--levels must be a whole number from 1 to 1000, got `{v}`"))?;
            }
            if let Some(v) = &s.total {
                c.total = i64::try_from(parse_kas(v, "--total", false)?).map_err(|_| "--total is too large".to_string())?;
            }
            if let Some(v) = &s.multiple {
                c.multiple_milli = crate::trade::curve::parse_multiple(v)?;
            }
            let linear = [("--levels", &s.levels), ("--multiple", &s.multiple)].into_iter().find(|(_, v)| v.is_some()).map(|(f, _)| f);
            crate::trade::curve::apply_shape(&mut c, s.shape.as_deref(), s.octaves.as_deref(), linear)?;
            let t = curve_table(&c).map_err(|e| e.to_string())?;
            let program = Program::of(spec.program).map_err(|e| e.to_string())?;
            let state = CurveAskState {
                maker: funder,
                token_cov_id: [0; 32],
                token_tpl_hash: program.template_hash,
                tpl_prefix_len: program.prefix.len() as i64,
                tpl_suffix_len: program.suffix.len() as i64,
                scale,
                min_fill: curve_min_fill(amount, t.table[0], t.level_size, scale),
                tip: 0,
                active_from: 0,
                level_size: t.level_size,
                total: amount,
                amount_left: amount,
                extension_commitment: spec.extension_commitment,
                table: t.table.clone(),
            };
            state.check_numbers()?;
            spec.curve = Some(GenesisCurve { state: state.clone(), custody: at, carrier: spec.carrier });
            (Holder::new([0; 32], SCHEME_COVENANT_ID, sell), None, Some((t, state)))
        }
    };
    spec.holders[at] = holder;
    if let Some(k) = kept {
        spec.holders.insert(at + 1, k);
    }
    Ok(Some(Sale { at, sell, keep, price, curve }))
}

/// The market of the token that `plan` issues, with the decimals it declares.
pub fn market_of(plan: &CreationPlan) -> Result<TokenMarket, String> {
    let g = &plan.genesis;
    let id =
        token_template_by_hash(&g.program.template_hash).map(|t| t.id).ok_or("the issued program is not one this build trades")?;
    let mut m = TokenMarket::new(
        g.covenant_id.as_bytes(),
        &g.spec.ticker,
        u32::from(g.spec.decimals),
        id,
        Some(g.spec.extension_commitment),
        None,
    )?;
    m.decimals_from = DecimalsFrom::Declared;
    Ok(m)
}

/// The genesis output `at` as a token UTXO of its state (not yet on the node).
fn genesis_utxo(plan: &CreationPlan, at: usize) -> Result<TokenUtxo, String> {
    let g = &plan.genesis;
    let s = g.states.get(at).ok_or("the sale's genesis output is missing")?;
    let value = g.tx.outputs.get(at).ok_or("the sale's genesis output is missing")?.value;
    let k = Kcc20State {
        amount: s.amount as i64,
        owner: s.owner,
        owner_scheme: s.owner_scheme,
        borrow_scheme: s.borrow_scheme,
        borrow_guard: s.borrow_guard,
        extension_commitment: s.extension_commitment,
    };
    Ok(TokenUtxo {
        utxo: Utxo {
            transaction_id: g.txid().as_bytes(),
            index: at as u32,
            amount: value,
            block_daa_score: 0,
            covenant_id: Some(g.covenant_id.as_bytes()),
        },
        state: TokenState::Kcc20(k),
    })
}

/// The preparation's change to `funder`, which funds the limit sell, when there is one.
fn prep_change(plan: &CreationPlan, funder: &[u8; 32]) -> Option<KeyUtxo> {
    let (i, value) = plan.prep.change?;
    let out = plan.prep.tx.outputs.get(i)?;
    (out.script_public_key == p2pk_script(funder)).then(|| KeyUtxo {
        utxo: Utxo {
            transaction_id: plan.prep.txid().as_bytes(),
            index: i as u32,
            amount: value,
            block_daa_score: 0,
            covenant_id: None,
        },
        pubkey: *funder,
    })
}

/// The limit sell of a one-price sale: GTC at `price`, funded by the preparation's change and
/// `extra` (KAS UTXOs of the key the creation does not spend). The book of a new token is empty.
pub fn plan_ask(
    plan: &CreationPlan,
    sale: &Sale,
    funder: [u8; 32],
    extra: &[KeyUtxo],
    clock: Clock,
    fee_rate: u64,
) -> Result<(TokenMarket, Plan), String> {
    let m = market_of(plan)?;
    let price = sale.price.ok_or("not a one-price sale")?;
    let funding: Vec<KeyUtxo> = prep_change(plan, &funder).into_iter().chain(extra.iter().cloned()).collect();
    let env = Env {
        market: m.clone(),
        maker: funder,
        clock,
        book: Default::default(),
        own: vec![],
        guards_unavailable: vec![],
        guards_acknowledged: false,
        last_fill: None,
        market_start_ack: false,
        book_verified: true,
        funding,
        token_utxos: vec![genesis_utxo(plan, sale.at)?],
        fee_rate: Some(fee_rate),
        fee_mode: Default::default(),
        carrier: None,
        pair: None,
        delivery: Default::default(),
    };
    let intent = Intent::Limit(LimitIntent {
        side: Side::Sell,
        amount: sale.sell as i64,
        price,
        tip: 0,
        min_fill: None,
        lifetime: Lifetime::Gtc,
        active_from: None,
        crossing: Crossing::Auction,
        max_fills: None,
    });
    Ok((m, plan_order(&env, &intent)))
}

/// The built transaction of an ask plan, or why it has none.
pub fn ask_built(p: &Plan) -> Result<&BuiltTx, String> {
    let errors: Vec<String> =
        p.issues.iter().filter(|i| i.severity == Severity::Error).map(|i| format!("{} [{}]", i.message, i.code)).collect();
    match (&p.built, errors.is_empty()) {
        (Some(b), true) => Ok(b),
        _ => Err(format!("--sell price: the sell order could not be planned: {}", errors.join("; "))),
    }
}

/// Whether the ask plan failed for lack of KAS, so that one more funding UTXO may help.
pub fn short_of_kas(p: &Plan) -> bool {
    p.built.is_none() && p.issues.iter().any(|i| i.code == "INSUFFICIENT_KAS")
}

#[cfg(test)]
mod tests {
    use super::*;
    use kob_protocol::issue::{build_creation, FundingUtxo};

    fn spec(supply: u64, decimals: u8) -> (IssueSpec, [u8; 32]) {
        let pk = kob_protocol::tx::pubkey_of(&[0x33; 32]).unwrap();
        let outpoint = kaspa_consensus_core::tx::TransactionOutpoint {
            transaction_id: kaspa_consensus_core::tx::TransactionId::from_bytes([7; 32]),
            index: 0,
        };
        let funding = FundingUtxo { outpoint, amount: 500 * 100_000_000, owner_pubkey: pk };
        let mut s = IssueSpec::new("Sale", "SALE", decimals, supply, vec![Holder::new(pk, 0, supply)], vec![funding]);
        s.network = "testnet-10".into();
        (s, pk)
    }

    fn args(mode: Option<&str>, keep: Option<&str>) -> SaleArgs {
        SaleArgs { mode: mode.map(str::to_string), keep: keep.map(str::to_string), ..Default::default() }
    }

    #[test]
    fn keep_parses_a_percent_with_two_decimals() {
        assert_eq!(parse_keep("0"), Ok(0));
        assert_eq!(parse_keep("12.5"), Ok(1_250));
        assert_eq!(parse_keep("12.34%"), Ok(1_234));
        assert_eq!(parse_keep("100"), Ok(10_000));
        for bad in ["100.01", "1.234", "-1", "x", "", ".5"] {
            assert!(parse_keep(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn flags_that_do_not_apply_are_refused() {
        assert!(args(None, Some("10")).check().unwrap_err().contains("goes with --sell"));
        assert!(args(Some("swap"), None).check().is_err());
        let mut a = args(Some("price"), None);
        a.levels = Some("10".into());
        assert!(a.check().unwrap_err().contains("--sell curve"));
        let mut a = args(Some("curve"), None);
        a.price = Some("1KAS".into());
        assert!(a.check().unwrap_err().contains("--sell price"));
        assert!(args(None, None).check().is_ok());
    }

    /// The web's defaults: 100 levels, 10,000 KAS if sold out, the last level 5 times the first; the
    /// curve is in the genesis and the kept part is a separate output of the wallet.
    #[test]
    fn a_curve_with_a_kept_share_is_created_in_the_genesis() {
        let (mut s, pk) = spec(1_000_000_000, 4);
        let sale = apply(&mut s, &args(Some("curve"), Some("10")), pk).unwrap().unwrap();
        assert_eq!((sale.at, sale.sell, sale.keep), (0, 900_000_000, 100_000_000));
        let (t, st) = sale.curve.clone().unwrap();
        assert_eq!((st.levels(), st.total, st.scale, st.active_from), (100, 900_000_000, 10_000, 0));
        assert!(t.realised <= 1_000_000_000_000 && t.realised > 999_000_000_000);
        let ratio = *st.table.last().unwrap() as f64 / st.table[0] as f64;
        assert!((ratio - 5.0).abs() < 0.01, "x{ratio}");
        assert_eq!((s.holders[0].owner_scheme, s.holders[1].owner, s.holders[1].amount), (SCHEME_COVENANT_ID, pk, 100_000_000));
        let plan = build_creation(&s).unwrap();
        let placed = plan.genesis.curve.as_ref().expect("the genesis places the curve");
        assert_eq!(placed.custody, 0);
        assert_eq!(plan.genesis.states[0].owner, placed.covenant_id.as_bytes(), "the custody is owned by the curve");
    }

    #[test]
    fn a_one_price_sale_splits_the_share_and_defaults_to_ten_thousand_kas() {
        let (mut s, pk) = spec(1_000, 0);
        let sale = apply(&mut s, &args(Some("price"), Some("25")), pk).unwrap().unwrap();
        assert_eq!((sale.sell, sale.keep, sale.price), (750, 250, Some(1_000_000_000_000 / 750)));
        assert_eq!(s.holders.iter().map(|h| (h.owner_scheme, h.amount)).collect::<Vec<_>>(), vec![(0, 750), (0, 250)]);
        let mut a = args(Some("price"), None);
        a.price = Some("2KAS".into());
        let (mut s, _) = spec(1_000, 0);
        assert_eq!(apply(&mut s, &a, pk).unwrap().unwrap().price, Some(200_000_000));
        // Keeping everything leaves nothing for sale.
        let (mut s, _) = spec(1_000, 0);
        assert!(apply(&mut s, &args(Some("price"), Some("100")), pk).unwrap_err().contains("nothing is left"));
        // No share of the funding key: nothing to sell.
        let (mut s, _) = spec(1_000, 0);
        s.holders = vec![Holder::new([9; 32], 0, 1_000)];
        assert!(apply(&mut s, &args(Some("price"), None), pk).unwrap_err().contains("receives no tokens"));
    }

    /// The limit sell spends the genesis output for sale and the preparation's change, and plans
    /// on the empty book of the new token.
    #[test]
    fn the_one_price_ask_is_planned_on_the_genesis_output() {
        let (mut s, pk) = spec(1_000_000, 2);
        let sale = apply(&mut s, &args(Some("price"), Some("50")), pk).unwrap().unwrap();
        let plan = build_creation(&s).unwrap();
        let clock = Clock { daa: 1_000_000, unix: 1_800_000_000, rate_milli: 10_000, disputed: false };
        let (m, p) = plan_ask(&plan, &sale, pk, &[], clock, 100).unwrap();
        let built = ask_built(&p).unwrap();
        assert_eq!(m.cov, plan.genesis.covenant_id.as_bytes());
        let spent: Vec<([u8; 32], u32)> = built.tx.inputs.iter().map(|i| (i.transaction_id, i.index)).collect();
        assert!(spent.contains(&(plan.genesis.txid().as_bytes(), 0)), "the sale output");
        assert!(spent.iter().any(|(t, _)| *t == plan.prep.txid().as_bytes()), "the preparation's change");
        assert!(matches!(p.states.first(), Some(kob_protocol::state::AnyState::KobAsk(a)) if a.price == sale.price.unwrap()));
    }
}
