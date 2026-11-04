//! What `kob watch` knows of tokens: the registry and the indexer's token list (ticker, decimals), resolved the way `kob book`
//! resolves a token, cached per token. A token the lists do not name yet is asked again after a pause (a new token enters
//! the indexer's list with its first order).

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use kob_protocol::json::to_hex;
use kob_protocol::registry::parse_hex32;

use super::frames::TokenInfo;
use crate::trade::data;
use crate::trade::market::{self, DecimalsFrom, TokenFacts};

/// How long a token that did not resolve is left alone.
const RETRY_AFTER: Duration = Duration::from_secs(60);
/// How often the indexer's token list is read again for a token that did not resolve.
const REFRESH: Duration = Duration::from_secs(30);

/// The token lists and the per-token display facts.
pub struct Names {
    indexer: String,
    network: String,
    registry: Option<PathBuf>,
    /// The registry's tokens.
    pub reg: Vec<TokenFacts>,
    /// The indexer's tokens.
    pub idx: Vec<TokenFacts>,
    cache: BTreeMap<[u8; 32], (Option<TokenInfo>, Instant)>,
    refreshed: Instant,
}

impl Names {
    /// Read the lists.
    pub fn load(indexer: &str, network: &str, registry: Option<PathBuf>) -> Result<Names, String> {
        let (reg, idx) = data::token_lists(registry.as_deref(), network, Some(indexer))?;
        Ok(Names {
            indexer: indexer.to_string(),
            network: network.to_string(),
            registry,
            reg,
            idx,
            cache: BTreeMap::new(),
            refreshed: Instant::now(),
        })
    }

    /// Read the lists again (after a reconnect: tokens may have appeared) and forget what did not resolve.
    pub fn reload(&mut self) -> Result<(), String> {
        let (reg, idx) = data::token_lists(self.registry.as_deref(), &self.network, Some(&self.indexer))?;
        self.reg = reg;
        self.idx = idx;
        self.refreshed = Instant::now();
        self.cache.retain(|_, (info, _)| info.is_some());
        Ok(())
    }

    /// A token named by a ticker or a covenant id.
    pub fn find(&self, key: &str) -> Result<[u8; 32], String> {
        if let Some(c) = parse_hex32(key) {
            return Ok(c);
        }
        market::unique_cov(&self.reg, &self.idx, key)?
            .ok_or_else(|| format!("unknown token `{key}` (a ticker the registry or the indexer lists, or a covenant id)"))
    }

    /// The display facts of a token, `None` when its decimals are not known (its fills are shown in base units).
    pub fn info(&mut self, cov: &[u8; 32]) -> Option<TokenInfo> {
        if let Some((info, at)) = self.cache.get(cov) {
            if info.is_some() || at.elapsed() < RETRY_AFTER {
                return info.clone();
            }
        }
        let info = self.resolve(cov);
        self.cache.insert(*cov, (info.clone(), Instant::now()));
        info
    }

    fn resolve(&mut self, cov: &[u8; 32]) -> Option<TokenInfo> {
        if let Some(info) = self.try_resolve(cov) {
            return Some(info);
        }
        // a token that entered the indexer's list after the lists were read: read it again, at most every REFRESH
        if self.refreshed.elapsed() >= REFRESH {
            self.refreshed = Instant::now();
            if let Ok(idx) = data::token_lists(None, &self.network, Some(&self.indexer)).map(|l| l.1) {
                self.idx = idx;
                return self.try_resolve(cov);
            }
        }
        None
    }

    fn try_resolve(&self, cov: &[u8; 32]) -> Option<TokenInfo> {
        let order_scale = |c: &[u8; 32]| data::order_scale(&self.indexer, c);
        let remembered = |c: &[u8; 32]| crate::recent::first_order_decimals(&self.network, c);
        let open =
            market::OpenToken { remembered: Some(&remembered), view: true, ..market::OpenToken::new(&self.network, &order_scale) };
        let m = market::resolve_with(&to_hex(cov), &self.reg, &self.idx, Some(&open)).ok()?;
        (m.decimals_from != DecimalsFrom::Unknown).then(|| TokenInfo {
            ticker: m.ticker.clone(),
            decimals: m.decimals,
            scale: m.scale,
        })
    }
}
