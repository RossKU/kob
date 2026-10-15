//! The market of one token (the web app's `TokenMarket`, `web/src/kob/plan-types.ts`): program, family, template identity,
//! extension commitment, order scale, tick and the program's default tips; resolved from the registry and / or the indexer's
//! token list, which must agree.

use kob_protocol::artifacts::{token_template, token_template_by_hash, TemplateId};
use kob_protocol::defaults::{default_scale, tips};
use kob_protocol::json::to_hex;
use kob_protocol::Family;

use super::plan::{Issue, Severity};
use super::units::TokenUnit;

/// How the registry knows a token (the web app's `tradable` / `untradableReason` and its `token-unlisted` finding).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Listing {
    /// A tradable registry token.
    Listed,
    /// In the registry but not tradable there (delisted, or its token program not reviewed): the reason.
    NotTradable(String),
    /// Not in the registry: the ticker is the indexer's, the decimals the indexer's, its orders' scale or `--decimals`.
    Unlisted,
}

/// Where a KCC-20 market's extension commitment comes from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExtSource {
    /// The registry and / or the indexer's token list (they agree); a KRON token's (none) too.
    Lists,
    /// No source names it (a token without a registry entry the indexer names none for: an older indexer, or placements that
    /// named more than one): `ext` is a placeholder nothing plans with. A signing command takes it from the wallet's own
    /// node-verified holdings ([`TokenMarket::take_ext_from_holdings`]) or from the live order's own state; a view needs none.
    Unknown,
    /// The wallet's own token UTXOs of it, their states confirmed on the node.
    Holdings,
    /// The live order's own state (read from the node).
    Order,
}

/// Where a token's decimals (and the scale of its orders) come from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DecimalsFrom {
    /// The registry and / or the indexer's token list (they agree).
    Lists,
    /// The scale of the token's market: the scale of its first order as the indexer reports it (`market_scale`, open or ended),
    /// else (an older indexer) the one most of its open orders use.
    Market,
    /// `--decimals` for the token's first order: neither the registry, the indexer nor an order of the token tells them.
    Entered,
    /// The decimals `--decimals` named for this machine's own first order of the token, which the indexer has not seen yet
    /// (`crate::recent`).
    Remembered,
    /// A view of a token without orders and without decimals: nothing tells them; amounts are base units (scale 1).
    Unknown,
}

/// Everything a planner needs of one token.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TokenMarket {
    /// Covenant id.
    pub cov: [u8; 32],
    /// Ticker (empty: not in a registry).
    pub ticker: String,
    /// Decimals.
    pub decimals: u32,
    /// Token program.
    pub program: TemplateId,
    /// Family.
    pub family: Family,
    /// Template hash, prefix and suffix lengths (order states carry them).
    pub tpl_hash: [u8; 32],
    /// Template prefix length.
    pub prefix_len: i64,
    /// Template suffix length.
    pub suffix_len: i64,
    /// KCC-20 extension commitment (zero for KRON).
    pub ext: [u8; 32],
    /// Where `ext` comes from.
    pub ext_source: ExtSource,
    /// Token inputs / outputs one transfer may carry.
    pub slots: (usize, usize),
    /// Base units per whole token of new orders (`10^min(decimals, 9)`).
    pub scale: i64,
    /// Price step, sompi per whole token (1: none).
    pub tick: i64,
    /// The program's default refund tip and keeper tip (sompi).
    pub refund_tip: i64,
    /// Keeper tip.
    pub keeper_tip: i64,
    /// The registry checked every genesis output.
    pub genesis_verified: bool,
    /// How the registry knows it ([`resolve`] sets it; a market built by [`TokenMarket::new`] alone is unlisted).
    pub listing: Listing,
    /// Where the decimals (and the scale) come from ([`resolve_with`] sets it).
    pub decimals_from: DecimalsFrom,
}

impl TokenMarket {
    /// The market of a token of `program` (`ext`: required for KCC-20, ignored for KRON).
    pub fn new(
        cov: [u8; 32],
        ticker: &str,
        decimals: u32,
        program: TemplateId,
        ext: Option<[u8; 32]>,
        tick: Option<i64>,
    ) -> Result<TokenMarket, String> {
        if !program.is_token() {
            return Err(format!("{} is not a token program", program.name()));
        }
        if decimals > 18 {
            return Err(format!("{ticker}: decimals {decimals} out of range"));
        }
        let t = token_template(program);
        let family = program.family();
        let ext = match family {
            Family::Kron => [0; 32],
            Family::Kcc20 => ext.ok_or_else(|| format!("{}: a KCC-20 token needs its extension commitment", show_id(&cov, ticker)))?,
        };
        let tip = tips(program).map_err(|e| e.to_string())?;
        let tick = tick.unwrap_or(1);
        if tick <= 0 {
            return Err(format!("{ticker}: the tick must be positive"));
        }
        Ok(TokenMarket {
            cov,
            ticker: ticker.to_string(),
            decimals,
            program,
            family,
            tpl_hash: t.hash,
            prefix_len: t.prefix.len() as i64,
            suffix_len: t.suffix.len() as i64,
            ext,
            ext_source: ExtSource::Lists,
            slots: t.slots,
            scale: default_scale(decimals),
            tick,
            refund_tip: tip.refund_tip as i64,
            keeper_tip: tip.keeper_tip as i64,
            genesis_verified: false,
            listing: Listing::Unlisted,
            decimals_from: DecimalsFrom::Lists,
        })
    }

    /// An unknown extension commitment ([`ExtSource::Unknown`]) from the commitments of the wallet's own token UTXOs of the
    /// token (`held`, their states confirmed on the node): the one they share, with the finding that says so; refused when
    /// they name none (nothing to take it from) or more than one (the token has look-alike units). A known commitment is
    /// left as it is (`Ok(None)`: the caller compares it with the holdings).
    pub fn take_ext_from_holdings(&mut self, held: &[[u8; 32]]) -> Result<Option<Issue>, String> {
        if self.ext_source != ExtSource::Unknown {
            return Ok(None);
        }
        let distinct: std::collections::BTreeSet<[u8; 32]> = held.iter().copied().collect();
        let one = match distinct.len() {
            0 => {
                return Err(format!(
                    "{}: its extension commitment is unknown: no registry entry, the indexer names none, and this wallet holds none \
                     of the token to take it from (an order pins it: only tokens of that commitment fill it)",
                    self.name()
                ))
            }
            1 => *distinct.first().expect("one"),
            n => {
                return Err(format!(
                    "{}: your token UTXOs of it (checked on the node) have {n} different extension commitments and no source names \
                     the token's: refused (check the token's genesis)",
                    self.name()
                ))
            }
        };
        self.ext = one;
        self.ext_source = ExtSource::Holdings;
        Ok(Some(Issue {
            code: "TOKEN_EXTENSION_FROM_HOLDINGS",
            severity: Severity::Info,
            message: format!(
                "{}: the indexer names no extension commitment; {} is the one your {} token UTXO(s) of it have (checked on the node)",
                self.name(),
                to_hex(&one),
                held.len()
            ),
        }))
    }

    /// The finding a signing command raises for a token it trades (`trades`) or sends: a registry token that is not
    /// tradable is refused for trading (a warning for a plain send); a token the registry does not list is a warning (its
    /// ticker and decimals, which read every amount and price, come from the indexer, its orders or `--decimals`).
    pub fn listing_issue(&self, trades: bool) -> Option<Issue> {
        match &self.listing {
            Listing::Listed => None,
            Listing::NotTradable(why) => Some(Issue {
                code: "TOKEN_NOT_TRADABLE",
                severity: if trades { Severity::Error } else { Severity::Warning },
                message: format!("{} is not tradable in the registry: {why}", self.name()),
            }),
            Listing::Unlisted => Some(Issue {
                code: "TOKEN_UNLISTED",
                severity: Severity::Warning,
                message: format!(
                    "{} is not in the registry: its ticker comes from the indexer and its decimals ({}, which read every amount and \
                     price) {}; check its covenant id {} yourself",
                    self.name(),
                    self.decimals,
                    match self.decimals_from {
                        DecimalsFrom::Entered => "are the ones --decimals names for its first order",
                        DecimalsFrom::Remembered => "are the ones --decimals named for your first order of it (kept on this machine)",
                        DecimalsFrom::Unknown => "are unknown (no order of it yet)",
                        DecimalsFrom::Lists | DecimalsFrom::Market => "come from the indexer or the scale of its orders",
                    },
                    to_hex(&self.cov)
                ),
            }),
        }
    }

    /// Ticker and decimals.
    pub fn unit(&self) -> TokenUnit {
        TokenUnit { ticker: self.ticker.clone(), decimals: self.decimals }
    }

    /// `TICKER (abcd…1234)`.
    pub fn name(&self) -> String {
        show_id(&self.cov, &self.ticker)
    }
}

/// `TICKER (abcd..1234)` or the short id alone.
pub fn show_id(cov: &[u8; 32], ticker: &str) -> String {
    let h = to_hex(cov);
    let short = format!("{}..{}", &h[..8], &h[56..]);
    if ticker.is_empty() {
        short
    } else {
        format!("{ticker} ({short})")
    }
}

/// One token as a source describes it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TokenFacts {
    /// Covenant id.
    pub cov: [u8; 32],
    /// Ticker.
    pub ticker: String,
    /// Decimals, when known.
    pub decimals: Option<u32>,
    /// Template hash, when known.
    pub tpl_hash: Option<[u8; 32]>,
    /// Extension commitment.
    pub ext: Option<[u8; 32]>,
    /// Tick.
    pub tick: Option<i64>,
    /// Genesis verified.
    pub genesis_verified: bool,
    /// A registry token that is not tradable there: why (delisted, its token program pending review).
    pub not_tradable: Option<String>,
    /// The indexer's `market_scale` of a token without decimals: the scale of its earliest order, open or ended (`Some(None)`:
    /// no order of it was ever seen; `None`: the source does not report it, an older indexer or the registry).
    pub market_scale: Option<Option<i64>>,
}

/// The tokens of a registry file (`registry/tokens.json` format) of `network`: the registry must pass every rule of the
/// validator (`kob registry validate`) and be of that network (the web app's `parseRegistry`).
pub fn registry_tokens(json: &str, network: &str) -> Result<Vec<TokenFacts>, String> {
    use kob_protocol::registry::{Registry, ReviewStatus, Status};
    let r = Registry::parse_unvalidated(json).map_err(|e| format!("registry: {e}"))?;
    r.validate_for_network(network).map_err(|errs| {
        let list: Vec<String> = errs.iter().take(5).map(|e| e.to_string()).collect();
        format!("the registry is not valid for {network} ({} findings): {}", errs.len(), list.join("; "))
    })?;
    let mut out = vec![];
    for t in &r.tokens {
        let not_tradable = if t.status == Status::Delisted {
            Some("delisted".to_string())
        } else {
            match r.template(&t.template_id) {
                Some(x) if x.review_status == ReviewStatus::Reviewed => None,
                Some(x) => Some(format!("its token program {} is pending review", x.id)),
                None => Some(format!("unknown template {}", t.template_id)),
            }
        };
        let cov = kob_protocol::registry::parse_hex32(&t.covenant_id).ok_or(format!("{}: bad covenant id", t.ticker))?;
        let tpl = r
            .template(&t.template_id)
            .and_then(|x| kob_protocol::registry::parse_hex32(&x.template_hash))
            .or_else(|| kob_protocol::profile::by_registry_id(&t.template_id).map(|p| p.hash()));
        out.push(TokenFacts {
            cov,
            ticker: t.ticker.clone(),
            decimals: Some(u32::from(t.decimals)),
            tpl_hash: tpl,
            ext: t.extension_commitment.as_deref().and_then(kob_protocol::registry::parse_hex32),
            tick: t.tick,
            genesis_verified: t.genesis_verified == Some(true),
            not_tradable,
            market_scale: None,
        });
    }
    Ok(out)
}

/// The tokens of the indexer's `GET /v1/tokens`.
pub fn indexer_tokens(v: &serde_json::Value) -> Result<Vec<TokenFacts>, String> {
    let items = v.get("tokens").and_then(|t| t.as_array()).ok_or("indexer /v1/tokens: no `tokens` array")?;
    let h = |x: Option<&serde_json::Value>| x.and_then(|s| s.as_str()).and_then(kob_protocol::registry::parse_hex32);
    items
        .iter()
        .map(|t| {
            Ok(TokenFacts {
                cov: h(t.get("covenant_id")).ok_or("indexer token without covenant_id")?,
                ticker: t.get("ticker").and_then(|s| s.as_str()).unwrap_or("").to_string(),
                decimals: t.get("decimals").and_then(|d| d.as_u64()).map(|d| d as u32),
                tpl_hash: h(t.get("template_hash")),
                ext: h(t.get("extension_commitment")),
                tick: None,
                genesis_verified: false,
                not_tradable: None,
                market_scale: match t.get("market_scale") {
                    None => None,
                    Some(serde_json::Value::Null) => Some(None),
                    Some(v) => Some(Some(v.as_i64().ok_or("indexer /v1/tokens: a `market_scale` that is not an integer")?)),
                },
            })
        })
        .collect()
}

/// Find a token by ticker (case-insensitive) or 64-hex covenant id.
pub fn find<'a>(list: &'a [TokenFacts], key: &str) -> Vec<&'a TokenFacts> {
    match kob_protocol::registry::parse_hex32(key) {
        Some(c) => list.iter().filter(|t| t.cov == c).collect(),
        None => list.iter().filter(|t| !t.ticker.is_empty() && t.ticker.eq_ignore_ascii_case(key)).collect(),
    }
}

/// Most decimals `--decimals` names: the scale of an order is `10^decimals`, at most 10^9 (`MAX_SCALE`).
pub const MAX_ENTERED_DECIMALS: u32 = 9;

/// The standard scale of a token's orders, `None` without orders ([`OpenToken::order_scale`]).
pub type OrderScale<'a> = &'a dyn Fn(&[u8; 32]) -> Result<Option<i64>, String>;

/// The decimals `--decimals` named for this machine's own first order of a token ([`OpenToken::remembered`]).
pub type Remembered<'a> = &'a dyn Fn(&[u8; 32]) -> Option<u32>;

/// What resolves a token without a registry entry (the open token list): its network, and its decimals when neither the
/// registry nor the indexer names them.
pub struct OpenToken<'a> {
    /// The network: on mainnet a token without a registry entry must be of a `reviewed` program (prototypes are not traded).
    pub network: &'a str,
    /// The standard scale of the token's open orders ([`standard_scale`] of the indexer's book), `None` without orders: read
    /// when the indexer's token list reports no market scale (an older indexer, or a token without orders).
    pub order_scale: OrderScale<'a>,
    /// `--decimals N` for the token's first order (0..=9): used only when no order of the token tells its scale.
    pub entered: Option<u32>,
    /// The decimals `--decimals` named for this machine's own first order of the token (`crate::recent`): within the indexer's
    /// lag they stand in for `--decimals`, and a `--decimals` of another scale is refused.
    pub remembered: Option<Remembered<'a>>,
    /// A view (`kob book`): a token whose decimals nothing tells resolves with none ([`DecimalsFrom::Unknown`]: base units)
    /// instead of being refused.
    pub view: bool,
}

impl<'a> OpenToken<'a> {
    /// The open token of a command on `network`, its orders' scale read with `order_scale`.
    pub fn new(network: &'a str, order_scale: OrderScale<'a>) -> OpenToken<'a> {
        OpenToken { network, order_scale, entered: None, remembered: None, view: false }
    }
}

/// The scale shared by most of the order rows of a book view (`asks` and `bids`; ties: the smaller), only powers of ten up
/// to 10^9 counted; `None` when no row tells one (the web app's `standardScaleOf`).
pub fn standard_scale(book: &serde_json::Value) -> Option<i64> {
    let mut count: std::collections::BTreeMap<i64, usize> = std::collections::BTreeMap::new();
    for side in ["asks", "bids"] {
        for r in book.get(side).and_then(|s| s.as_array()).map(|a| a.as_slice()).unwrap_or_default() {
            if let Some(s) = r.get("scale").and_then(|s| s.as_i64()).filter(|s| kob_protocol::state::check_scale(*s).is_ok()) {
                *count.entry(s).or_default() += 1;
            }
        }
    }
    // ascending scales: the first of the highest count is the smaller on a tie
    let most = count.values().copied().max()?;
    count.into_iter().find(|(_, n)| *n == most).map(|(s, _)| s)
}

fn decimals_of_scale(scale: i64) -> u32 {
    scale.max(1).ilog10()
}

/// The market of `key` from a registry list and an indexer list: every source that names the token must agree on its
/// template, extension commitment and decimals ([`resolve_with`] without an open token's facts).
#[cfg(test)]
pub fn resolve(key: &str, registry: &[TokenFacts], indexer: &[TokenFacts]) -> Result<TokenMarket, String> {
    resolve_with(key, registry, indexer, None)
}

/// [`resolve`], and for a token without a registry entry (`open`): the network's program review, and its decimals from the
/// indexer, else from the scale of its orders, else from `--decimals` (its first order). The chain records no decimals, so
/// they are never assumed: without any of these the token is refused.
pub fn resolve_with(
    key: &str,
    registry: &[TokenFacts],
    indexer: &[TokenFacts],
    open: Option<&OpenToken>,
) -> Result<TokenMarket, String> {
    let r = find(registry, key);
    let i = find(indexer, key);
    if r.len() > 1 || i.len() > 1 {
        return Err(format!("`{key}` names more than one token: give its covenant id"));
    }
    // a ticker found in the registry pins the covenant id; the indexer entry must then be the same token
    let cov = match (r.first(), i.first()) {
        (Some(a), _) => a.cov,
        (None, Some(b)) => b.cov,
        (None, None) => {
            return Err(format!(
                "unknown token `{key}` (not in the registry nor in the indexer's token list: the indexer lists a token once an \
                 order of it was placed; a token without one opens its market from the web app, with a wallet that holds it)"
            ))
        }
    };
    let i = indexer.iter().find(|t| t.cov == cov);
    let r = r.first().copied();
    let pick = |f: &dyn Fn(&TokenFacts) -> Option<String>, what: &str| -> Result<Option<String>, String> {
        let a = r.and_then(f);
        let b = i.and_then(f);
        match (a, b) {
            (Some(a), Some(b)) if a != b => Err(format!("{what} of `{key}` differs between the registry ({a}) and the indexer ({b})")),
            (a, b) => Ok(a.or(b)),
        }
    };
    let tpl =
        pick(&|t| t.tpl_hash.map(|h| to_hex(&h)), "the token program")?.ok_or(format!("`{key}`: the token program is unknown"))?;
    let tpl = kob_protocol::registry::parse_hex32(&tpl).unwrap_or_default();
    let program =
        token_template_by_hash(&tpl).map(|t| t.id).ok_or(format!("`{key}`: its token program is not one this build supports"))?;
    if let (None, Some(o)) = (r, open) {
        let review = kob_protocol::profile::by_hash(&tpl).map(|p| p.review.status);
        if o.network == "mainnet" && review != Some(kob_protocol::profile::ProgramReview::Reviewed) {
            return Err(format!("`{key}`: its token program {} is not reviewed: it is not traded on mainnet", program.name()));
        }
    }
    // a program whose tokens exist on other networks only (profile `networks`; KForge: testnet-10)
    if let (Some(p), Some(o)) = (kob_protocol::profile::by_hash(&tpl), open) {
        if !p.on_network(o.network) {
            return Err(format!("`{key}`: its token program {} exists on {} only", program.name(), p.networks.join(", ")));
        }
    }
    // an all-zero commitment is a commitment (the standard fixed-supply class: every token `kob token issue` makes), not a
    // missing one; a KRON token has none, whatever a source says
    let ext = if program.family() == Family::Kron {
        None
    } else {
        pick(&|t| t.ext.map(|h| to_hex(&h)), "the extension commitment")?.and_then(|e| kob_protocol::registry::parse_hex32(&e))
    };
    let known: Option<u32> = pick(&|t| t.decimals.map(|d| d.to_string()), "decimals")?.and_then(|d| d.parse().ok());
    let entered = open.and_then(|o| o.entered);
    if let Some(e) = entered.filter(|e| *e > MAX_ENTERED_DECIMALS) {
        return Err(format!("--decimals {e}: a first order names 0 to {MAX_ENTERED_DECIMALS} decimals (the scale is at most 10^9)"));
    }
    let (decimals, from) = match (known, open) {
        (Some(d), _) => {
            if let Some(e) = entered.filter(|e| default_scale(*e) != default_scale(d)) {
                return Err(format!("--decimals {e} differs from the decimals of `{key}` ({d})"));
            }
            (d, DecimalsFrom::Lists)
        }
        (None, Some(o)) => {
            // the market's scale: its first order's, open or ended, as the indexer reports it (TN10 2026-10-15 O-3: once every
            // order had ended, a first order with other decimals would have put two scales on one market); an older indexer,
            // or a token without orders: the scale of its open orders
            let market = i.and_then(|t| t.market_scale).flatten();
            if let Some(s) = market.filter(|s| kob_protocol::state::check_scale(*s).is_err()) {
                return Err(format!("`{key}`: the indexer reports a market scale of {s}, which no order can have"));
            }
            let scale = match market {
                Some(s) => Some(s),
                None => (o.order_scale)(&cov)?,
            };
            let remembered = o.remembered.and_then(|f| f(&cov)).filter(|d| *d <= MAX_ENTERED_DECIMALS);
            match (scale, entered) {
                (Some(s), e) => {
                    if let Some(e) = e.filter(|e| default_scale(*e) != s) {
                        return Err(format!(
                            "--decimals {e} differs from the scale of the market of `{key}` ({s} base units per whole token, {} \
                             decimals: the scale its first order set, which every order of it uses)",
                            decimals_of_scale(s)
                        ));
                    }
                    (decimals_of_scale(s), DecimalsFrom::Market)
                }
                (None, Some(e)) => {
                    if let Some(r) = remembered.filter(|r| default_scale(*r) != default_scale(e)) {
                        return Err(format!(
                            "--decimals {e} differs from the decimals your first order of `{key}` named ({r}, kept on this machine \
                             until the indexer lists the order): its market's scale is {}",
                            default_scale(r)
                        ));
                    }
                    (e, DecimalsFrom::Entered)
                }
                (None, None) => match remembered {
                    Some(r) => (r, DecimalsFrom::Remembered),
                    None if o.view => (0, DecimalsFrom::Unknown),
                    None => {
                        return Err(format!(
                            "`{key}`: its decimals are unknown: no registry entry and no order of it yet, and the chain does not \
                             record decimals. Its first order names the decimals the issuer chose: `kob place ... --decimals N` (0..=9)"
                        ))
                    }
                },
            }
        }
        (None, None) => return Err(format!("`{key}`: its decimals are unknown")),
    };
    let ticker = r.map(|t| t.ticker.clone()).or_else(|| i.map(|t| t.ticker.clone())).unwrap_or_default();
    let ext = if program.family() == Family::Kron { None } else { ext };
    // a token without a registry entry the indexer names no commitment for: a view needs none, and a signing command takes it
    // from what it can check without the indexer (the wallet's node-verified holdings, the order's own state)
    let unknown = ext.is_none() && r.is_none() && program.family() == Family::Kcc20;
    let mut m = TokenMarket::new(cov, &ticker, decimals, program, if unknown { Some([0; 32]) } else { ext }, r.and_then(|t| t.tick))?;
    if unknown {
        m.ext_source = ExtSource::Unknown;
    }
    m.genesis_verified = r.is_some_and(|t| t.genesis_verified);
    m.listing = match r {
        Some(t) => t.not_tradable.clone().map_or(Listing::Listed, Listing::NotTradable),
        None => Listing::Unlisted,
    };
    m.decimals_from = from;
    Ok(m)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn facts(cov: u8, ticker: &str, decimals: u32, program: TemplateId, ext: u8) -> TokenFacts {
        TokenFacts {
            cov: [cov; 32],
            ticker: ticker.into(),
            decimals: Some(decimals),
            tpl_hash: Some(token_template(program).hash),
            ext: Some([ext; 32]),
            tick: None,
            genesis_verified: false,
            not_tradable: None,
            market_scale: None,
        }
    }

    fn open_facts(cov: u8, program: TemplateId) -> TokenFacts {
        TokenFacts { decimals: None, ticker: String::new(), ..facts(cov, "", 0, program, 0xdd) }
    }

    #[test]
    fn an_open_token_takes_its_decimals_from_its_orders_else_from_decimals_for_its_first_order() {
        let idx = vec![open_facts(3, TemplateId::Kcc20Ref)];
        let key = to_hex(&[3; 32]);
        let none = |_: &[u8; 32]| -> Result<Option<i64>, String> { Ok(None) };
        let thousand = |_: &[u8; 32]| -> Result<Option<i64>, String> { Ok(Some(1000)) };
        fn open<'a>(f: OrderScale<'a>, entered: Option<u32>, network: &'a str) -> OpenToken<'a> {
            OpenToken { entered, ..OpenToken::new(network, f) }
        }
        // no orders, no --decimals: refused, never assumed
        let e = resolve_with(&key, &[], &idx, Some(&open(&none, None, "testnet-10"))).unwrap_err();
        assert!(e.contains("--decimals"), "{e}");
        assert!(resolve(&key, &[], &idx).unwrap_err().contains("decimals are unknown"));
        // the first order: the entered decimals, marked as such
        let m = resolve_with(&key, &[], &idx, Some(&open(&none, Some(6), "testnet-10"))).unwrap();
        assert_eq!((m.decimals, m.scale, m.decimals_from, m.ext), (6, 1_000_000, DecimalsFrom::Entered, [0xdd; 32]));
        assert!(resolve_with(&key, &[], &idx, Some(&open(&none, Some(10), "testnet-10"))).unwrap_err().contains("0 to 9"));
        // orders exist: their scale decides; a --decimals that disagrees is refused, one that agrees is fine
        let m = resolve_with(&key, &[], &idx, Some(&open(&thousand, None, "testnet-10"))).unwrap();
        assert_eq!((m.decimals, m.scale, m.decimals_from), (3, 1000, DecimalsFrom::Market));
        assert!(resolve_with(&key, &[], &idx, Some(&open(&thousand, Some(3), "testnet-10"))).is_ok());
        assert!(resolve_with(&key, &[], &idx, Some(&open(&thousand, Some(8), "testnet-10"))).unwrap_err().contains("differs"));
        // decimals the token list names (an operator's registry entry) win, and --decimals must agree with them
        let named = vec![facts(3, "", 2, TemplateId::Kcc20Ref, 0xdd)];
        assert_eq!(resolve_with(&key, &[], &named, Some(&open(&none, None, "mainnet"))).unwrap().decimals, 2);
        assert!(resolve_with(&key, &[], &named, Some(&open(&none, Some(4), "mainnet"))).unwrap_err().contains("differs"));
        // the reviewed programs on mainnet; a prototype is not traded there without a registry entry
        assert!(resolve_with(&key, &[], &idx, Some(&open(&none, Some(0), "mainnet"))).is_ok());
        let pm = vec![open_facts(3, TemplateId::Kcc20PublicMint)];
        assert!(resolve_with(&key, &[], &pm, Some(&open(&none, Some(0), "mainnet"))).is_ok());
        let proto = vec![open_facts(3, TemplateId::Kcc20Ref8x8)];
        assert!(resolve_with(&key, &[], &proto, Some(&open(&none, Some(0), "mainnet"))).unwrap_err().contains("not reviewed"));
        assert!(resolve_with(&key, &[], &proto, Some(&open(&none, Some(0), "testnet-10"))).is_ok());
        // a program on testnet-10 only (KForge, holders id_type 0): refused on mainnet even with a registry entry
        let forge = vec![open_facts(5, TemplateId::KForgeToken1785)];
        let fkey = to_hex(&[5; 32]);
        let m = resolve_with(&fkey, &[], &forge, Some(&open(&none, Some(0), "testnet-10"))).unwrap();
        assert_eq!((m.program, m.family), (TemplateId::KForgeToken1785, Family::Kron));
        assert!(resolve_with(&fkey, &[], &forge, Some(&open(&none, Some(0), "mainnet"))).is_err());
        let listed = vec![facts(5, "FRG", 0, TemplateId::KForgeToken1785, 0xdd)];
        assert!(resolve_with(&fkey, &listed, &[], Some(&open(&none, None, "mainnet"))).unwrap_err().contains("testnet-10 only"));
        // a covenant id no source names
        assert!(resolve_with(&to_hex(&[4; 32]), &[], &idx, Some(&open(&none, Some(0), "mainnet")))
            .unwrap_err()
            .contains("unknown token"));
    }

    /// TN10 2026-10-15 O-3: the market of a token without decimals keeps the scale its first order set after every order of
    /// it has ended: the indexer's `market_scale` decides (the book, empty, tells nothing), a --decimals of another scale is
    /// refused, and the same one is accepted. An older indexer (no `market_scale`): the scale of the open orders, as before.
    #[test]
    fn the_market_scale_of_an_open_token_stays_after_its_orders_ended() {
        let key = to_hex(&[3; 32]);
        let ended = vec![TokenFacts { market_scale: Some(Some(1_000_000)), ..open_facts(3, TemplateId::Kcc20Ref) }];
        let empty_book = |_: &[u8; 32]| -> Result<Option<i64>, String> { Ok(None) };
        let unread =
            |_: &[u8; 32]| -> Result<Option<i64>, String> { Err("the book is not read when the market scale is known".into()) };
        let open = |f, entered| OpenToken { entered, ..OpenToken::new("testnet-10", f) };
        let m = resolve_with(&key, &[], &ended, Some(&open(&unread, None))).unwrap();
        assert_eq!((m.decimals, m.scale, m.decimals_from), (6, 1_000_000, DecimalsFrom::Market));
        let e = resolve_with(&key, &[], &ended, Some(&open(&empty_book, Some(2)))).unwrap_err();
        assert!(e.contains("--decimals 2 differs from the scale of the market") && e.contains("1000000 base units"), "{e}");
        assert_eq!(resolve_with(&key, &[], &ended, Some(&open(&empty_book, Some(6)))).unwrap().decimals, 6);
        // the market's scale wins over the open orders' (orders of another scale placed before it was kept)
        let other = |_: &[u8; 32]| -> Result<Option<i64>, String> { Ok(Some(100)) };
        assert_eq!(resolve_with(&key, &[], &ended, Some(&open(&other, None))).unwrap().scale, 1_000_000);
        // no order ever (`null`), or an older indexer (absent): the open orders, else the first order's --decimals
        for ms in [Some(None), None] {
            let idx = vec![TokenFacts { market_scale: ms, ..open_facts(3, TemplateId::Kcc20Ref) }];
            assert_eq!(resolve_with(&key, &[], &idx, Some(&open(&other, None))).unwrap().scale, 100);
            assert_eq!(resolve_with(&key, &[], &idx, Some(&open(&empty_book, Some(2)))).unwrap().decimals_from, DecimalsFrom::Entered);
        }
        // a scale no order can have is refused, not used
        let bad = vec![TokenFacts { market_scale: Some(Some(500)), ..open_facts(3, TemplateId::Kcc20Ref) }];
        assert!(resolve_with(&key, &[], &bad, Some(&open(&empty_book, Some(2)))).unwrap_err().contains("no order can have"));
        // a registry token's decimals win; its row carries no market scale
        let reg = vec![facts(3, "TOK", 8, TemplateId::Kcc20Ref, 0xdd)];
        assert_eq!(resolve_with("TOK", &reg, &ended, Some(&open(&unread, None))).unwrap().decimals, 8);
        // the indexer's token list reads it: absent, null, a number; anything else is refused
        // (`Null` here: the field left out)
        let v = |ms: serde_json::Value| {
            let mut t = serde_json::json!({"covenant_id": key, "ticker": ""});
            if !ms.is_null() {
                t["market_scale"] = ms;
            }
            indexer_tokens(&serde_json::json!({ "tokens": [t] }))
        };
        assert_eq!(v(serde_json::Value::Null).unwrap()[0].market_scale, None);
        assert_eq!(v(serde_json::json!(1000)).unwrap()[0].market_scale, Some(Some(1000)));
        let null = indexer_tokens(&serde_json::json!({"tokens": [{"covenant_id": key, "market_scale": null}]})).unwrap();
        assert_eq!(null[0].market_scale, Some(None));
        assert!(v(serde_json::json!("1000")).is_err());
    }

    /// TN10 2026-10-15 O-1 and O-2: the decimals of this machine's own first order stand in for --decimals while the indexer
    /// has not seen it (a --decimals of another scale is refused), and a view of a token nothing tells the decimals of
    /// resolves with none instead of asking for a --decimals it does not take.
    #[test]
    fn remembered_first_order_decimals_and_a_view_without_decimals() {
        let key = to_hex(&[3; 32]);
        let idx = vec![TokenFacts { market_scale: Some(None), ..open_facts(3, TemplateId::Kcc20Ref) }];
        let none = |_: &[u8; 32]| -> Result<Option<i64>, String> { Ok(None) };
        let two = |c: &[u8; 32]| (*c == [3; 32]).then_some(2u32);
        let open = |entered, view| OpenToken { entered, remembered: Some(&two), view, ..OpenToken::new("testnet-10", &none) };
        let m = resolve_with(&key, &[], &idx, Some(&open(None, false))).unwrap();
        assert_eq!((m.decimals, m.scale, m.decimals_from), (2, 100, DecimalsFrom::Remembered));
        assert!(m.listing_issue(true).unwrap().message.contains("kept on this machine"));
        assert_eq!(resolve_with(&key, &[], &idx, Some(&open(Some(2), false))).unwrap().decimals_from, DecimalsFrom::Entered);
        let e = resolve_with(&key, &[], &idx, Some(&open(Some(4), false))).unwrap_err();
        assert!(e.contains("--decimals 4 differs from the decimals your first order") && e.contains("(2,"), "{e}");
        // once the indexer reports the market's scale it decides (another order may have come first)
        let seen = vec![TokenFacts { market_scale: Some(Some(1000)), ..open_facts(3, TemplateId::Kcc20Ref) }];
        assert_eq!(resolve_with(&key, &[], &seen, Some(&open(None, false))).unwrap().decimals, 3);
        // nothing tells them: a signing command is told how the first order names them; a view resolves in base units
        let e = resolve_with(&key, &[], &idx, Some(&OpenToken::new("testnet-10", &none))).unwrap_err();
        assert!(e.contains("`kob place ... --decimals N`"), "{e}");
        let m = resolve_with(&key, &[], &idx, Some(&OpenToken { view: true, ..OpenToken::new("testnet-10", &none) })).unwrap();
        assert_eq!((m.decimals, m.scale, m.decimals_from), (0, 1, DecimalsFrom::Unknown));
    }

    #[test]
    fn the_standard_scale_is_the_one_most_orders_use() {
        let v = |rows: serde_json::Value| serde_json::json!({ "asks": rows, "bids": [] });
        assert_eq!(standard_scale(&v(serde_json::json!([{"scale": 1000}, {"scale": 100}, {"scale": 1000}]))), Some(1000));
        assert_eq!(standard_scale(&v(serde_json::json!([{"scale": 100}, {"scale": 10}]))), Some(10));
        assert_eq!(standard_scale(&v(serde_json::json!([{"scale": 500}, {"scale": 10_000_000_000i64}, {}]))), None);
        assert_eq!(standard_scale(&serde_json::json!({ "asks": [], "bids": [{"scale": 1}] })), Some(1));
        assert_eq!(standard_scale(&serde_json::json!({})), None);
    }

    #[test]
    fn sources_must_agree() {
        let reg = vec![facts(1, "TUSD", 8, TemplateId::Kcc20Ref, 0xee)];
        let idx = vec![facts(1, "TUSD", 8, TemplateId::Kcc20Ref, 0xee), facts(2, "", 3, TemplateId::Kcc20Ref, 0xdd)];
        let m = resolve("tusd", &reg, &idx).unwrap();
        assert_eq!((m.cov, m.decimals, m.scale, m.ext, m.program), ([1; 32], 8, 100_000_000, [0xee; 32], TemplateId::Kcc20Ref));
        // an unlisted token by covenant id from the indexer alone
        let m = resolve(&to_hex(&[2; 32]), &[], &idx).unwrap();
        assert_eq!((m.decimals, m.scale), (3, 1000));
        // the all-zero commitment of a standard fixed-supply token is its commitment (it was dropped as missing, and every
        // trading command refused the token); a zero against a non-zero one is a disagreement
        let zero = vec![facts(5, "TZERO", 8, TemplateId::Kcc20Ref, 0)];
        assert_eq!(resolve("TZERO", &zero, &zero).unwrap().ext, [0; 32]);
        assert_eq!(resolve(&to_hex(&[5; 32]), &[], &zero).unwrap().ext, [0; 32]);
        let other = vec![facts(5, "TZERO", 8, TemplateId::Kcc20Ref, 0xee)];
        assert!(resolve("TZERO", &zero, &other).unwrap_err().contains("extension commitment"));
        // a disagreement is refused
        let bad = vec![facts(1, "TUSD", 6, TemplateId::Kcc20Ref, 0xee)];
        assert!(resolve("TUSD", &reg, &bad).unwrap_err().contains("decimals"));
        let bad = vec![facts(1, "TUSD", 8, TemplateId::Kcc20Ref, 0xef)];
        assert!(resolve("TUSD", &reg, &bad).unwrap_err().contains("extension"));
        assert!(resolve("NOPE", &reg, &idx).is_err());
    }

    /// A token without a registry entry the indexer names no commitment for (an older indexer reported none for every token
    /// of the standard zero commitment, TN10 2026-10-14 B-4): a view resolves it, and a signing command takes the commitment
    /// from the wallet's node-verified holdings, refused when they name none or several. A registry token still needs one.
    #[test]
    fn an_unknown_commitment_comes_from_the_wallets_holdings_never_from_nothing() {
        let no_ext = vec![TokenFacts { ext: None, ..facts(6, "", 0, TemplateId::Kcc20Ref, 0) }];
        let mut m = resolve(&to_hex(&[6; 32]), &[], &no_ext).unwrap();
        assert_eq!(m.ext_source, ExtSource::Unknown);
        assert!(resolve("TZ", &[TokenFacts { ext: None, ..facts(6, "TZ", 0, TemplateId::Kcc20Ref, 0) }], &[])
            .unwrap_err()
            .contains("needs its extension commitment"));
        let e = m.clone().take_ext_from_holdings(&[]).unwrap_err();
        assert!(e.contains("holds none of the token to take it from"), "{e}");
        let e = m.clone().take_ext_from_holdings(&[[0; 32], [0xee; 32]]).unwrap_err();
        assert!(e.contains("2 different extension commitments and no source names the token's: refused"), "{e}");
        let i = m.take_ext_from_holdings(&[[0; 32], [0; 32]]).unwrap().unwrap();
        assert_eq!((m.ext, m.ext_source, i.code), ([0; 32], ExtSource::Holdings, "TOKEN_EXTENSION_FROM_HOLDINGS"));
        // a commitment a source names is left to the caller's comparison
        let mut named = resolve(&to_hex(&[5; 32]), &[], &[facts(5, "", 0, TemplateId::Kcc20Ref, 0)]).unwrap();
        assert_eq!(named.ext_source, ExtSource::Lists);
        assert_eq!(named.take_ext_from_holdings(&[[0xee; 32]]).unwrap(), None);
        assert_eq!(named.ext, [0; 32]);
        // KRON has none to look for
        let kron = vec![TokenFacts { ext: None, ..facts(7, "", 0, TemplateId::KronToken2433, 0) }];
        assert_eq!(resolve(&to_hex(&[7; 32]), &[], &kron).unwrap().ext_source, ExtSource::Lists);
    }

    /// A registry file passes the validator for the command's network (as the web app's `parseRegistry`), and a registry
    /// token that is not tradable there (delisted, or on a program pending review) is refused for trading, like the web
    /// app's `assess`; a token only the indexer knows is flagged.
    #[test]
    fn the_registry_decides_what_is_tradable() {
        let ex = include_str!("../../../../registry/tokens.example.json");
        let e = registry_tokens(ex, "mainnet").unwrap_err();
        assert!(e.contains("not valid for mainnet"), "{e}");
        assert_eq!(resolve("EXKCC", &registry_tokens(ex, "testnet-10").unwrap(), &[]).unwrap().listing, Listing::Listed);
        // a token of the 8/8 prototype while that program is pending review
        let mut v: serde_json::Value = serde_json::from_str(ex).unwrap();
        assert_eq!(v["templates"][1]["id"], "kcc20-ref-8x8");
        v["templates"][1]["review_status"] = "pending-review".into();
        let reg = registry_tokens(&v.to_string(), "testnet-10").unwrap();
        let m = resolve("EXKCC", &reg, &[]).unwrap();
        assert!(matches!(&m.listing, Listing::NotTradable(w) if w.contains("pending review")), "{:?}", m.listing);
        assert_eq!(m.listing_issue(true).unwrap().severity, Severity::Error);
        assert_eq!(m.listing_issue(false).unwrap().severity, Severity::Warning);
        // a pending-review token of a reviewed program is tradable
        assert_eq!(resolve("EXKRON", &reg, &[]).unwrap().listing, Listing::Listed);
        // delisted
        let mut v: serde_json::Value = serde_json::from_str(ex).unwrap();
        v["tokens"][1]["status"] = "delisted".into();
        let reg = registry_tokens(&v.to_string(), "testnet-10").unwrap();
        assert_eq!(resolve("EXKRON", &reg, &[]).unwrap().listing, Listing::NotTradable("delisted".into()));
        // a registry the validator refuses (a token listed on a program pending review) is not read at all
        let mut v: serde_json::Value = serde_json::from_str(ex).unwrap();
        v["templates"][1]["review_status"] = "pending-review".into();
        v["tokens"][0]["status"] = "listed".into();
        assert!(registry_tokens(&v.to_string(), "testnet-10").unwrap_err().contains("not valid"));
        // a token only the indexer knows
        let idx = vec![facts(2, "TOK", 8, TemplateId::Kcc20Ref, 0xdd)];
        let m = resolve(&to_hex(&[2; 32]), &[], &idx).unwrap();
        assert_eq!(m.listing, Listing::Unlisted);
        let i = m.listing_issue(true).unwrap();
        assert_eq!((i.code, i.severity), ("TOKEN_UNLISTED", Severity::Warning));
        assert!(i.message.contains(&to_hex(&[2; 32])), "{}", i.message);
    }
}
