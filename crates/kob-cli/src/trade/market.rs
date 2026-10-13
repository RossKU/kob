//! The market of one token (the web app's `TokenMarket`, `web/src/kob/plan-types.ts`): program, family, template identity,
//! extension commitment, order scale, tick and the program's default tips; resolved from the registry and / or the indexer's
//! token list, which must agree.

use kob_protocol::artifacts::{token_template, token_template_by_hash, TemplateId};
use kob_protocol::defaults::{default_scale, tips};
use kob_protocol::json::to_hex;
use kob_protocol::Family;

use super::units::TokenUnit;

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
            slots: t.slots,
            scale: default_scale(decimals),
            tick,
            refund_tip: tip.refund_tip as i64,
            keeper_tip: tip.keeper_tip as i64,
            genesis_verified: false,
        })
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
}

/// The tokens of a registry file (`registry/tokens.json` format).
pub fn registry_tokens(json: &str) -> Result<Vec<TokenFacts>, String> {
    let r = kob_protocol::registry::Registry::parse_unvalidated(json).map_err(|e| e.to_string())?;
    let mut out = vec![];
    for t in &r.tokens {
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

/// The market of `key` from a registry list and an indexer list: every source that names the token must agree on its
/// template, extension commitment and decimals.
pub fn resolve(key: &str, registry: &[TokenFacts], indexer: &[TokenFacts]) -> Result<TokenMarket, String> {
    let r = find(registry, key);
    let i = find(indexer, key);
    if r.len() > 1 || i.len() > 1 {
        return Err(format!("`{key}` names more than one token: give its covenant id"));
    }
    // a ticker found in the registry pins the covenant id; the indexer entry must then be the same token
    let cov = match (r.first(), i.first()) {
        (Some(a), _) => a.cov,
        (None, Some(b)) => b.cov,
        (None, None) => return Err(format!("unknown token `{key}` (not in the registry nor in the indexer's token list)")),
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
    let ext = pick(&|t| t.ext.filter(|e| *e != [0; 32]).map(|h| to_hex(&h)), "the extension commitment")?
        .and_then(|e| kob_protocol::registry::parse_hex32(&e));
    let decimals = pick(&|t| t.decimals.map(|d| d.to_string()), "decimals")?
        .and_then(|d| d.parse().ok())
        .ok_or(format!("`{key}`: its decimals are unknown"))?;
    let ticker = r.map(|t| t.ticker.clone()).or_else(|| i.map(|t| t.ticker.clone())).unwrap_or_default();
    let ext = if program.family() == Family::Kron { None } else { ext };
    let mut m = TokenMarket::new(cov, &ticker, decimals, program, ext, r.and_then(|t| t.tick))?;
    m.genesis_verified = r.is_some_and(|t| t.genesis_verified);
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
        }
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
        // a disagreement is refused
        let bad = vec![facts(1, "TUSD", 6, TemplateId::Kcc20Ref, 0xee)];
        assert!(resolve("TUSD", &reg, &bad).unwrap_err().contains("decimals"));
        let bad = vec![facts(1, "TUSD", 8, TemplateId::Kcc20Ref, 0xef)];
        assert!(resolve("TUSD", &reg, &bad).unwrap_err().contains("extension"));
        assert!(resolve("NOPE", &reg, &idx).is_err());
    }
}
