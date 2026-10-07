//! Token allowlist and listing rules.
//!
//! The allowlist is a config input (`--tokens registry/tokens.json`, produced by another branch).
//! Its exact schema is owned by that branch, so the loader is deliberately tolerant: it accepts a
//! bare array or an object with a `tokens` array and snake_case or camelCase keys (and their aliases); an unknown key
//! is refused, so a misspelt pin is never dropped silently. Only the covenant id is mandatory.
//!
//! **Strict templates, open tokens.** A registry document (`registry/tokens.json`) carries the STRICT template list (the reviewed token
//! programs) and the token entries. Every order whose token program (template hash and family) is on the strict list is listed,
//! whether or not its token has an entry (open token list); an entry adds metadata and the standing (`official` badge or
//! `unverified`). A delisted token is refused. The token is identified by covenant id plus template hash, never by ticker.
//!
//! Listing is a policy of the indexer, not of the chain: an order that fails a rule is still
//! indexed (status by covenant id keeps working, makers can cancel) but is marked `listed = false`
//! with a reason and stays out of the books.

use crate::hex::Hash32;
use kob_protocol::family::Family;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::path::Path;

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
pub struct TokenEntry {
    #[serde(alias = "symbol", alias = "name", default)]
    pub ticker: String,
    #[serde(alias = "covenantId", alias = "covenant_id", alias = "id")]
    pub covenant_id: Hash32,
    /// Template hash of the token program; when present, orders must quote the same hash.
    #[serde(alias = "templateHash", alias = "tokenTemplateHash", alias = "token_template_hash", default)]
    pub template_hash: Option<Hash32>,
    #[serde(alias = "extensionCommitment", default)]
    pub extension_commitment: Option<Hash32>,
    /// Decimals of the token: when present, an order is listed only if its `scale` (base units per whole token, the price
    /// denominator) is the standard one, `10^decimals` capped at `10^9` ([`TokenEntry::scale`]), so every listed order of the
    /// token quotes the same whole token.
    #[serde(default)]
    pub decimals: Option<u32>,
    /// Token program family (`kcc20`, `kron`); when present, orders must trade a token of that family.
    #[serde(default)]
    pub family: Option<Family>,
    #[serde(default = "yes")]
    pub enabled: bool,
    /// Confirmed genuine (registry `official`): the "official" badge. Every other token is "unverified".
    #[serde(default)]
    pub official: bool,
    /// Registry id of the token's program (`kron-2433`, ...), when known.
    #[serde(alias = "templateId", default)]
    pub template_id: Option<String>,
    /// Capabilities of the token's program that reach holder balances or supply (`freeze`, `seize`, `blacklist`, `mint-authority`, ...).
    #[serde(default)]
    pub powers: Vec<String>,
}

impl TokenEntry {
    /// The standard scale of the token's orders: `kob_protocol::defaults::default_scale(decimals)` (`10^decimals`, at most
    /// `10^9`); `None` without `decimals`.
    pub fn scale(&self) -> Option<i64> {
        self.decimals.map(kob_protocol::defaults::default_scale)
    }

    /// `official` or `unverified` (delisted tokens are not entries).
    pub fn standing(&self) -> &'static str {
        if self.official {
            "official"
        } else {
            "unverified"
        }
    }
}

/// A token program of the strict template list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StrictTemplate {
    /// Registry id (`kcc20-kaspacom-0-2-5`).
    pub id: String,
    pub family: Family,
    /// Capabilities of the program (registry `capabilities`).
    pub powers: Vec<String>,
}

fn yes() -> bool {
    true
}

/// Every key a [`TokenEntry`] of the list format may carry (its fields and their aliases, and the `genesis_verified` flag read
/// from the raw document). Anything else is refused.
const TOKEN_ENTRY_KEYS: &[&str] = &[
    "ticker",
    "symbol",
    "name",
    "covenant_id",
    "covenantId",
    "id",
    "template_hash",
    "templateHash",
    "tokenTemplateHash",
    "token_template_hash",
    "extension_commitment",
    "extensionCommitment",
    "decimals",
    "family",
    "enabled",
    "official",
    "template_id",
    "templateId",
    "powers",
    "genesis_verified",
    "genesisVerified",
];

#[derive(Debug, thiserror::Error)]
pub enum TokenLoadError {
    #[error("cannot read {0}: {1}")]
    Io(String, std::io::Error),
    #[error("cannot parse {0}: {1}")]
    Parse(String, serde_json::Error),
}

#[derive(Debug, Clone, Default)]
pub struct TokenAllowlist {
    entries: HashMap<Hash32, TokenEntry>,
    /// The strict template list by template hash (empty: a plain allowlist, only entries are listed).
    templates: HashMap<Hash32, StrictTemplate>,
    /// Covenant ids of delisted tokens: never listed, even on a strict template.
    delisted: HashSet<Hash32>,
    /// The registry's `genesis_verified` per token: whether EVERY genesis output of the token was checked to be an
    /// instance of the pinned program. A covenant id only commits to the genesis output hashes, so an unchecked genesis may hide an
    /// output that mints look-alike tokens later.
    genesis: HashMap<Hash32, bool>,
    /// Where the document came from and its SHA-256: served with every token so a client can check which registry the
    /// operator's judgements (`official`, `genesis_verified`) come from instead of trusting the operator's word.
    info: Option<RegistryInfo>,
}

/// Provenance of the registry / allowlist an operator loaded.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RegistryInfo {
    /// The path the operator loaded (`inline` for a document parsed from text).
    pub source: String,
    /// Lowercase hex SHA-256 of the exact bytes of the document.
    pub sha256: String,
}

fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    crate::hex::encode(&Sha256::digest(bytes))
}

/// Warning code the API attaches to a token whose genesis has not been verified.
pub const WARN_GENESIS_UNVERIFIED: &str = "genesis_unverified";

impl TokenAllowlist {
    pub fn from_entries(entries: impl IntoIterator<Item = TokenEntry>) -> Self {
        TokenAllowlist {
            entries: entries.into_iter().filter(|e| e.enabled).map(|e| (e.covenant_id, e)).collect(),
            ..Default::default()
        }
    }

    /// Adds the strict template list: from now on every token whose program is on it is listed (open token list).
    pub fn with_strict_templates(mut self, templates: impl IntoIterator<Item = (Hash32, StrictTemplate)>) -> Self {
        self.templates.extend(templates);
        self
    }

    /// Records the registry's `genesis_verified` flags.
    pub fn with_genesis(mut self, flags: impl IntoIterator<Item = (Hash32, bool)>) -> Self {
        self.genesis.extend(flags);
        self
    }

    /// The registry's `genesis_verified` of a token (`None`: the registry does not say).
    pub fn genesis_verified(&self, id: &Hash32) -> Option<bool> {
        self.genesis.get(id).copied()
    }

    /// `official` only when the registry confirms the token AND has not found its genesis contaminated (`genesis_verified: false`
    /// withdraws the badge); every other standing is `unverified`.
    pub fn standing_of(&self, e: &TokenEntry) -> &'static str {
        if e.official && self.genesis_verified(&e.covenant_id) != Some(false) {
            "official"
        } else {
            "unverified"
        }
    }

    /// Warnings a client must show for a token: its genesis is not verified (absent flag included: fail closed in the display).
    pub fn warnings_for(&self, id: &Hash32) -> Vec<&'static str> {
        if self.genesis_verified(id) == Some(true) {
            vec![]
        } else {
            vec![WARN_GENESIS_UNVERIFIED]
        }
    }

    /// Refuses these covenant ids whatever their program.
    pub fn with_delisted(mut self, ids: impl IntoIterator<Item = Hash32>) -> Self {
        self.delisted.extend(ids);
        self
    }

    /// True when a strict template list is loaded: tokens outside the entries are listed if their program is on it.
    pub fn is_open(&self) -> bool {
        !self.templates.is_empty()
    }

    /// The strict template with this hash.
    pub fn strict_template(&self, hash: &Hash32) -> Option<&StrictTemplate> {
        self.templates.get(hash)
    }

    pub fn is_delisted(&self, covenant_id: &Hash32) -> bool {
        self.delisted.contains(covenant_id)
    }

    /// Parses an allowlist. A full token registry document (`schema_version` + `templates`, i.e.
    /// `registry/tokens.json`) is validated by `kob_protocol::registry` and only its `listed` tokens enter the list
    /// ([`TokenAllowlist::from_registry`]); anything else is the tolerant list format.
    pub fn parse(json: &str) -> Result<Self, serde_json::Error> {
        use serde::de::Error as _;
        let raw: serde_json::Value = serde_json::from_str(json)?;
        if raw.get("schema_version").is_some() && raw.get("templates").is_some() {
            let reg =
                kob_protocol::registry::Registry::parse(json).map_err(|e| serde_json::Error::custom(format!("registry: {e}")))?;
            let flags = genesis_flags(raw.get("tokens").and_then(|t| t.as_array()).map(Vec::as_slice).unwrap_or(&[]));
            return Ok(Self::from_registry(&reg).with_genesis(flags));
        }
        let entries = match &raw {
            serde_json::Value::Array(a) => a.as_slice(),
            v => v.get("tokens").and_then(|t| t.as_array()).map(Vec::as_slice).unwrap_or(&[]),
        };
        // a misspelt key (`templatHash`, `decimalz`) would silently drop the check it names: every key must be a known one
        for (i, e) in entries.iter().enumerate() {
            if let Some(obj) = e.as_object() {
                if let Some(k) = obj.keys().find(|k| !TOKEN_ENTRY_KEYS.contains(&k.as_str())) {
                    return Err(serde_json::Error::custom(format!(
                        "token entry {i}: unknown key {k:?} (known: {})",
                        TOKEN_ENTRY_KEYS.join(", ")
                    )));
                }
            }
        }
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum Doc {
            List(Vec<TokenEntry>),
            Obj { tokens: Vec<TokenEntry> },
        }
        let doc: Doc = serde_json::from_str(json)?;
        let list = match doc {
            Doc::List(l) => Self::from_entries(l),
            Doc::Obj { tokens } => Self::from_entries(tokens),
        };
        Ok(list.with_genesis(genesis_flags(entries)))
    }

    /// The tokens of a registry (delisted ones aside: they are refused): identity, program template hash, family, decimals (the
    /// standard scale), standing; plus the registry's strict template list, which opens the list to every token on those
    /// programs.
    pub fn from_registry(reg: &kob_protocol::registry::Registry) -> Self {
        use kob_protocol::registry::Status;
        let parse = |s: &str| kob_protocol::registry::parse_hex32(s).map(Hash32);
        let entries = reg.tokens.iter().filter(|t| t.status != Status::Delisted).filter_map(|t| {
            let tpl = reg.template(&t.template_id)?;
            Some(TokenEntry {
                ticker: t.ticker.clone(),
                covenant_id: parse(&t.covenant_id)?,
                template_hash: parse(&tpl.template_hash),
                extension_commitment: t.extension_commitment.as_deref().and_then(parse),
                decimals: Some(t.decimals as u32),
                family: Some(t.family),
                enabled: true,
                official: t.official && t.status == Status::Listed,
                template_id: Some(tpl.id.clone()),
                powers: tpl.capabilities.iter().map(|c| c.as_str().to_string()).collect(),
            })
        });
        let templates = reg.strict_templates().filter_map(|t| {
            Some((
                parse(&t.template_hash)?,
                StrictTemplate {
                    id: t.id.clone(),
                    family: t.family,
                    powers: t.capabilities.iter().map(|c| c.as_str().to_string()).collect(),
                },
            ))
        });
        let delisted = reg.tokens.iter().filter(|t| t.status == Status::Delisted).filter_map(|t| parse(&t.covenant_id));
        Self::from_entries(entries).with_strict_templates(templates).with_delisted(delisted)
    }

    pub fn load(path: &Path) -> Result<Self, TokenLoadError> {
        let p = path.display().to_string();
        let s = std::fs::read_to_string(path).map_err(|e| TokenLoadError::Io(p.clone(), e))?;
        let mut list = Self::parse(&s).map_err(|e| TokenLoadError::Parse(p.clone(), e))?;
        list.info = Some(RegistryInfo { source: p, sha256: sha256_hex(s.as_bytes()) });
        Ok(list)
    }

    /// The registry the build embeds (`registry/tokens.json`, the mainnet registry), parsed like a loaded file. Its
    /// provenance is `embedded:registry/tokens.json` with the sha256 of its LF-normalised bytes (the hash the mainnet
    /// deployment record pins).
    pub fn embedded_default() -> Result<Self, TokenLoadError> {
        let text = kob_protocol::registry::DEFAULT_REGISTRY_JSON;
        let source = "embedded:registry/tokens.json".to_string();
        let mut list = Self::parse(text).map_err(|e| TokenLoadError::Parse(source.clone(), e))?;
        list.info = Some(RegistryInfo { source, sha256: kob_protocol::registry::default_registry_sha256() });
        Ok(list)
    }

    /// The registry document this list was loaded from (`None`: built in memory).
    pub fn info(&self) -> Option<&RegistryInfo> {
        self.info.as_ref()
    }

    pub fn get(&self, covenant_id: &Hash32) -> Option<&TokenEntry> {
        self.entries.get(covenant_id)
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn list(&self) -> Vec<&TokenEntry> {
        let mut v: Vec<_> = self.entries.values().collect();
        v.sort_by(|a, b| a.ticker.cmp(&b.ticker).then(a.covenant_id.cmp(&b.covenant_id)));
        v
    }
}

/// The `genesis_verified` (or `genesisVerified`) booleans of raw token entries, by covenant id. Read from the raw JSON so the
/// flag is honoured whatever typed schema the registry loader has.
fn genesis_flags(entries: &[serde_json::Value]) -> Vec<(Hash32, bool)> {
    entries
        .iter()
        .filter_map(|e| {
            let flag = ["genesis_verified", "genesisVerified"].into_iter().find_map(|k| e.get(k).and_then(|v| v.as_bool()))?;
            let id = ["covenant_id", "covenantId", "id"].into_iter().find_map(|k| e.get(k).and_then(|v| v.as_str()))?;
            Some((Hash32::parse(id).ok()?, flag))
        })
        .collect()
}

/// Policy knobs. Defaults: 90-day maximum expiry, 1 KAS minimum order value.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(default, deny_unknown_fields)]
pub struct ListingRules {
    /// When true (default) an empty allowlist lists nothing.
    pub require_allowlist: bool,
    /// Minimum value of an order, in sompi: its amount at its quote, `quoteOf(amount, price, scale)` rounded down (a bid,
    /// whose quantity is its escrow: the KAS of its output).
    pub min_order_value_sompi: i64,
    /// Latest acceptable `expiryDaa` relative to the DAA score of the block that accepted the order.
    pub max_expiry_span_daa: i64,
    /// Slack on top of the span for wallets that computed the expiry from an older DAA score.
    pub expiry_slack_daa: i64,
    /// Minimum value of the order's covenant output, in sompi (0 = off).
    pub min_carrier_sompi: u64,
    /// List only tokens whose registry entry says `genesis_verified: true`. Off by default: the open token list
    /// trades unverified tokens too, with the `genesis_unverified` warning in the API.
    pub require_genesis_verified: bool,
}

impl Default for ListingRules {
    fn default() -> Self {
        ListingRules {
            require_allowlist: true,
            min_order_value_sompi: 100_000_000,
            max_expiry_span_daa: 77_760_000,
            expiry_slack_daa: 864_000,
            min_carrier_sompi: 0,
            require_genesis_verified: false,
        }
    }
}

/// The order fields the rules look at.
#[derive(Debug, Clone, Default)]
pub struct ListingInput {
    pub token_cov_id: Option<Hash32>,
    pub token_tpl_hash: Option<Hash32>,
    pub extension_commitment: Option<Hash32>,
    /// Family of the order's token program.
    pub family: Option<Family>,
    /// Base units per whole token of the order (the price denominator).
    pub scale: Option<i64>,
    /// The order's smallest fill, base units.
    pub min_fill: Option<i64>,
    /// The order's amount in base units (`amountLeft`); `None` for a bid, whose quantity is its escrow (`carrier`).
    pub amount: Option<i64>,
    /// The order's quote, sompi per whole token (a conditional order: its take-profit leg); `None` when it has no KAS quote
    /// (a pair order).
    pub price: Option<i64>,
    /// Priority tip, sompi per whole token.
    pub tip: Option<i64>,
    pub expiry_daa: Option<i64>,
    pub genesis_daa: u64,
    /// KAS of the order's output (a bid's escrow).
    pub carrier: u64,
}

impl ListingRules {
    /// `Ok(())` when the order may appear in the books, otherwise a short machine-readable reason.
    pub fn evaluate(&self, tokens: &TokenAllowlist, o: &ListingInput) -> Result<(), String> {
        let Some(tok_id) = o.token_cov_id else {
            return Err("no_token".into());
        };
        if tokens.is_delisted(&tok_id) {
            return Err("token_delisted".into());
        }
        // the strict template list (registry): the order's token program must be on it, for listed and unknown tokens alike
        if tokens.is_open() {
            match (o.token_tpl_hash.as_ref().and_then(|h| tokens.strict_template(h)), o.family) {
                (Some(t), Some(f)) if t.family == f => {}
                (Some(_), Some(_)) => return Err("token_family_mismatch".into()),
                _ => return Err("token_template_not_strict".into()),
            }
        }
        if self.require_genesis_verified && tokens.genesis_verified(&tok_id) != Some(true) {
            return Err("genesis_not_verified".into());
        }
        match tokens.get(&tok_id) {
            None if self.require_allowlist && !tokens.is_open() => return Err("token_not_allowlisted".into()),
            None => {}
            Some(t) => {
                if let (Some(want), Some(got)) = (t.template_hash, o.token_tpl_hash) {
                    if want != got {
                        return Err("token_template_mismatch".into());
                    }
                }
                if let (Some(want), Some(got)) = (t.family, o.family) {
                    if want != got {
                        return Err("token_family_mismatch".into());
                    }
                }
                if let (Some(want), Some(got)) = (t.extension_commitment, o.extension_commitment) {
                    if want != got {
                        return Err("extension_commitment_mismatch".into());
                    }
                }
                if let (Some(want), Some(scale)) = (t.scale(), o.scale) {
                    // every listed order of a token quotes the same whole token
                    if scale != want {
                        return Err("non_standard_scale".into());
                    }
                }
            }
        }
        // the order's value: its amount at its quote (rounded down, i128: never overflows), a bid's escrow; an order without a
        // KAS quote (a pair order) or a scale is not measured here
        let value: Option<i128> = match (o.amount, o.price, o.scale) {
            (Some(amount), Some(price), Some(scale)) => {
                Some(kob_protocol::state::quote_exact(amount, price, scale, kob_protocol::state::Round::Down).unwrap_or(0))
            }
            (None, Some(_), Some(_)) => Some(o.carrier as i128),
            _ => None,
        };
        if value.is_some_and(|v| v < self.min_order_value_sompi as i128) {
            return Err("order_value_below_minimum".into());
        }
        if let Some(exp) = o.expiry_daa {
            let limit = (o.genesis_daa as i64).saturating_add(self.max_expiry_span_daa).saturating_add(self.expiry_slack_daa);
            if exp > limit {
                return Err("expiry_beyond_maximum".into());
            }
        }
        if o.carrier < self.min_carrier_sompi {
            return Err("carrier_below_minimum".into());
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn h(b: u8) -> Hash32 {
        Hash32([b; 32])
    }

    #[test]
    fn parses_both_shapes_and_key_styles() {
        let a = format!(r#"[{{"ticker":"KRON","covenantId":"{}","decimals":3}}]"#, h(1));
        let b = format!(r#"{{"tokens":[{{"symbol":"KRON","covenant_id":"{}","decimals":3,"genesisVerified":true}}]}}"#, h(1));
        for s in [a, b] {
            let t = TokenAllowlist::parse(&s).unwrap();
            assert_eq!(t.len(), 1);
            assert_eq!(t.get(&h(1)).unwrap().decimals, Some(3));
            assert_eq!(t.get(&h(1)).unwrap().scale(), Some(1000));
        }
        let off = format!(r#"[{{"covenantId":"{}","enabled":false}}]"#, h(2));
        assert!(TokenAllowlist::parse(&off).unwrap().is_empty());
    }

    /// A misspelt key would drop the pin it names without a word: every key of an entry must be a known one.
    #[test]
    fn an_unknown_key_of_a_token_entry_is_refused() {
        for doc in [
            format!(r#"[{{"ticker":"TST","covenantId":"{}","templatHash":"{}","decimals":8}}]"#, h(1), h(2)),
            format!(r#"[{{"ticker":"TST","covenantId":"{}","decimalz":8}}]"#, h(1)),
            format!(r#"{{"tokens":[{{"covenantId":"{}","extra":1}}]}}"#, h(1)),
        ] {
            let e = TokenAllowlist::parse(&doc).unwrap_err().to_string();
            assert!(e.contains("unknown key"), "{e}");
        }
    }

    /// The standard scale of a token is `10^decimals`, capped at `10^9` (the largest scale the builders accept).
    #[test]
    fn the_standard_scale_is_ten_to_the_decimals_capped() {
        for (decimals, scale) in [(0u32, 1i64), (3, 1_000), (8, 100_000_000), (9, 1_000_000_000), (18, 1_000_000_000)] {
            let e = TokenEntry {
                ticker: "T".into(),
                covenant_id: h(1),
                template_hash: None,
                extension_commitment: None,
                decimals: Some(decimals),
                family: None,
                enabled: true,
                official: false,
                template_id: None,
                powers: vec![],
            };
            assert_eq!(e.scale(), Some(scale), "{decimals} decimals");
        }
    }

    /// The registry file itself is a valid allowlist: only `listed` tokens, with the template hash, family and decimals (the
    /// standard scale), and the listing rules work on it for both families.
    #[test]
    fn a_registry_document_is_an_allowlist_with_decimals_and_standard_scale() {
        let text = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/../../registry/tokens.example.json")).unwrap();
        // the example: two entries, not confirmed genuine (unverified), and the strict template list opens the list
        let example = TokenAllowlist::parse(&text).unwrap();
        assert_eq!(example.len(), 2);
        assert!(example.is_open() && example.list().iter().all(|t| t.standing() == "unverified"));
        // the shipped registry: the eight KRON-family tokens of the census, listed after the listing verification, six official and
        // the two test tokens (PEPE, DNBT: test tokens per their own names, founder 2026-10-03) unverified
        // (genesis verified on mainnet data, no live mint authority: `registry/evidence/`, `kob registry verify-genesis`); the two
        // KRON programs are reviewed with conditions (`crates/kob-tests/tests/review_b2_kron.rs`), which opens the list to every token on them
        let shipped = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/../../registry/tokens.json")).unwrap();
        let shipped = TokenAllowlist::parse(&shipped).unwrap();
        assert_eq!(shipped.len(), 8);
        assert!(shipped.is_open() && shipped.list().iter().all(|t| t.family == Some(Family::Kron)));
        let mut unverified: Vec<&str> =
            shipped.list().iter().filter(|t| t.standing() != "official").map(|t| t.ticker.as_str()).collect();
        unverified.sort_unstable();
        assert_eq!(unverified, ["DNBT", "PEPE"]);
        assert!(shipped.list().iter().all(|t| t.standing() == "official" || t.standing() == "unverified"));

        let mut doc: serde_json::Value = serde_json::from_str(&text).unwrap();
        for t in doc["templates"].as_array_mut().unwrap() {
            t["review_status"] = "reviewed".into();
        }
        for t in doc["tokens"].as_array_mut().unwrap() {
            t["status"] = "listed".into();
            t["verified"] = true.into();
        }
        // one token stays pending: still an entry (metadata), not official
        doc["tokens"][1]["status"] = "pending-review".into();
        doc["tokens"][1]["verified"] = false.into();
        let listed = TokenAllowlist::parse(&doc.to_string()).unwrap();
        assert_eq!(listed.len(), 2);
        let kcc = listed.list().into_iter().find(|t| t.ticker == "EXKCC").unwrap().clone();
        assert_eq!(
            (kcc.ticker.as_str(), kcc.family, kcc.decimals, kcc.scale()),
            ("EXKCC", Some(Family::Kcc20), Some(8), Some(100_000_000))
        );
        assert!(kcc.template_hash.is_some() && kcc.extension_commitment.is_some());

        // both listed: the KRON entry carries its own template, no extension commitment
        doc["tokens"][1]["status"] = "listed".into();
        doc["tokens"][1]["verified"] = true.into();
        let both = TokenAllowlist::parse(&doc.to_string()).unwrap();
        assert_eq!(both.len(), 2);
        let kron = both.list().into_iter().find(|t| t.ticker == "EXKRON").unwrap().clone();
        assert_eq!((kron.family, kron.scale(), kron.extension_commitment), (Some(Family::Kron), Some(100_000_000), None));
        let rules = ListingRules { max_expiry_span_daa: 1 << 40, ..ListingRules::default() };
        // one whole token at 10 KAS per whole token, any minimum fill
        let order = |tok: &TokenEntry, scale: i64, family: Family| ListingInput {
            token_cov_id: Some(tok.covenant_id),
            token_tpl_hash: tok.template_hash,
            extension_commitment: tok.extension_commitment,
            family: Some(family),
            scale: Some(scale),
            min_fill: Some(1),
            amount: Some(scale),
            price: Some(1_000_000_000),
            expiry_daa: Some(1),
            genesis_daa: 1,
            carrier: 1,
            ..Default::default()
        };
        // the standard scale of 8 decimals lists; any other scale is non-standard (prices would not be comparable)
        assert_eq!(rules.evaluate(&both, &order(&kcc, 100_000_000, Family::Kcc20)), Ok(()));
        for s in [1, 10_000_000, 1_000_000_000] {
            assert_eq!(rules.evaluate(&both, &order(&kcc, s, Family::Kcc20)), Err("non_standard_scale".into()), "{s}");
        }
        assert_eq!(rules.evaluate(&both, &order(&kcc, 100_000_000, Family::Kron)), Err("token_family_mismatch".into()));
        assert_eq!(rules.evaluate(&both, &order(&kron, 100_000_000, Family::Kron)), Ok(()));
        assert_eq!(rules.evaluate(&both, &order(&kron, 1_000_000, Family::Kron)), Err("non_standard_scale".into()));
        // an order quoting a program that is not on the strict list is refused
        let mut o = order(&kron, 100_000_000, Family::Kron);
        o.token_tpl_hash = Some(h(3));
        assert_eq!(rules.evaluate(&both, &o), Err("token_template_not_strict".into()));
        // a listed token quoting another strict program is a template mismatch
        let mut o = order(&kron, 100_000_000, Family::Kron);
        o.token_tpl_hash = kcc.template_hash;
        o.family = Some(Family::Kcc20);
        assert_eq!(rules.evaluate(&both, &o), Err("token_template_mismatch".into()));
        // a registry document that fails validation is a load error, not an empty list
        let mut bad = doc.clone();
        bad["tokens"][0]["decimals"] = 19.into();
        assert!(TokenAllowlist::parse(&bad.to_string()).is_err());
    }

    #[test]
    fn rules() {
        let toks = TokenAllowlist::from_entries([TokenEntry {
            ticker: "T".into(),
            covenant_id: h(1),
            template_hash: Some(h(9)),
            extension_commitment: None,
            decimals: Some(3),
            family: Some(Family::Kcc20),
            enabled: true,
            official: false,
            template_id: None,
            powers: vec![],
        }]);
        let rules = ListingRules::default();
        // one whole token (scale 1000) at 2.5 KAS
        let ok = ListingInput {
            token_cov_id: Some(h(1)),
            token_tpl_hash: Some(h(9)),
            family: Some(Family::Kcc20),
            scale: Some(1000),
            min_fill: Some(1000),
            amount: Some(1000),
            price: Some(250_000_000),
            tip: Some(0),
            expiry_daa: Some(1_000_000),
            genesis_daa: 900_000,
            carrier: 1,
            ..Default::default()
        };
        assert_eq!(rules.evaluate(&toks, &ok), Ok(()));
        let mut o = ok.clone();
        o.token_cov_id = Some(h(2));
        assert_eq!(rules.evaluate(&toks, &o), Err("token_not_allowlisted".into()));
        let mut o = ok.clone();
        o.token_tpl_hash = Some(h(8));
        assert_eq!(rules.evaluate(&toks, &o), Err("token_template_mismatch".into()));
        let mut o = ok.clone();
        o.family = Some(Family::Kron);
        assert_eq!(rules.evaluate(&toks, &o), Err("token_family_mismatch".into()));
        let mut o = ok.clone();
        o.scale = Some(100);
        assert_eq!(rules.evaluate(&toks, &o), Err("non_standard_scale".into()));
        // any amount and minimum fill at the standard scale is standard: the scale is the only geometry an order has
        for (amount, min_fill) in [(1000, 1), (5_000, 1000), (1_000_000, 10), (401, 401)] {
            let mut o = ok.clone();
            o.amount = Some(amount);
            o.min_fill = Some(min_fill);
            o.price = Some(250_000_000_000 / amount);
            assert_eq!(rules.evaluate(&toks, &o), Ok(()), "{amount} / {min_fill}");
        }
        for scale in [1, 10, 100, 10_000, 1_000_000_000] {
            let mut o = ok.clone();
            o.scale = Some(scale);
            assert_eq!(rules.evaluate(&toks, &o), Err("non_standard_scale".into()), "{scale}");
        }
        // no scale known: the scale rule cannot decide and does not fire (nor is the value measured)
        let mut o = ok.clone();
        o.scale = None;
        o.price = Some(1);
        assert_eq!(rules.evaluate(&toks, &o), Ok(()));
        // the order's value is its amount at its quote, rounded down
        let mut o = ok.clone();
        o.price = Some(1);
        assert_eq!(rules.evaluate(&toks, &o), Err("order_value_below_minimum".into()));
        let mut o = ok.clone();
        // 999 base units at 100 100 sompi per whole token: 99 999.9 sompi
        (o.amount, o.price) = (Some(999), Some(100_100));
        assert_eq!(rules.evaluate(&toks, &o), Err("order_value_below_minimum".into()));
        // at least 1 KAS: one base unit at 1000 KAS per whole token (exactly 1e8 sompi); 999 base units just above and just below
        (o.amount, o.price) = (Some(1), Some(100_000_000_000));
        assert_eq!(rules.evaluate(&toks, &o), Ok(()));
        (o.amount, o.price) = (Some(999), Some(100_100_200));
        assert_eq!(rules.evaluate(&toks, &o), Ok(()), "999 x 100_100_200 / 1000 = 100_000_099.8, floor 100_000_099");
        (o.amount, o.price) = (Some(999), Some(100_100_100));
        assert_eq!(rules.evaluate(&toks, &o), Err("order_value_below_minimum".into()), "99_999_999.9 rounds down");
        // a full fill worth more than an i64 never overflows the rule
        (o.amount, o.price) = (Some(i64::MAX), Some(i64::MAX));
        assert_eq!(rules.evaluate(&toks, &o), Ok(()));
        // a bid has no amount: its value is its escrow (the KAS of its output)
        let mut b = ok.clone();
        b.amount = None;
        b.carrier = 99_999_999;
        assert_eq!(rules.evaluate(&toks, &b), Err("order_value_below_minimum".into()));
        b.carrier = 100_000_000;
        assert_eq!(rules.evaluate(&toks, &b), Ok(()));
        let mut o = ok.clone();
        o.expiry_daa = Some(900_000 + 77_760_000 + 864_001);
        assert_eq!(rules.evaluate(&toks, &o), Err("expiry_beyond_maximum".into()));
        // the entries' own template hashes are not on a strict list here (a plain allowlist): unchanged behaviour
        let open = ListingRules { require_allowlist: false, ..Default::default() };
        let mut o = ok;
        o.token_cov_id = Some(h(2));
        assert_eq!(open.evaluate(&toks, &o), Ok(()));
    }

    /// The open token list: every token whose program is on the strict template list is listed (unknown covenant ids included),
    /// a program off the list and a delisted token are not.
    #[test]
    fn open_token_list_lists_every_token_of_a_strict_program() {
        let strict = |b: u8, f: Family, powers: &[&str]| {
            (h(b), StrictTemplate { id: format!("t{b}"), family: f, powers: powers.iter().map(|p| p.to_string()).collect() })
        };
        let entry = |c: u8, tpl: u8, official: bool| TokenEntry {
            ticker: format!("T{c}"),
            covenant_id: h(c),
            template_hash: Some(h(tpl)),
            extension_commitment: None,
            decimals: Some(0),
            family: Some(Family::Kron),
            enabled: true,
            official,
            template_id: Some(format!("t{tpl}")),
            powers: vec![],
        };
        let toks = TokenAllowlist::from_entries([entry(1, 8, true)])
            .with_strict_templates([strict(8, Family::Kron, &[]), strict(9, Family::Kcc20, &["freeze"])])
            .with_delisted([h(7)]);
        assert!(toks.is_open());
        assert_eq!(toks.get(&h(1)).unwrap().standing(), "official");
        let rules = ListingRules { max_expiry_span_daa: 1 << 40, min_order_value_sompi: 0, ..ListingRules::default() };
        let order = |cov: u8, tpl: u8, fam: Family| ListingInput {
            token_cov_id: Some(h(cov)),
            token_tpl_hash: Some(h(tpl)),
            family: Some(fam),
            scale: Some(1000),
            min_fill: Some(5),
            amount: Some(5),
            price: Some(1),
            expiry_daa: Some(1),
            genesis_daa: 1,
            carrier: 1,
            ..Default::default()
        };
        // an entry with its standard scale (0 decimals: 1), and an unknown token of a strict program with any scale
        assert_eq!(rules.evaluate(&toks, &order(1, 8, Family::Kron)), Err("non_standard_scale".into()));
        assert_eq!(rules.evaluate(&toks, &order(2, 8, Family::Kron)), Ok(()));
        assert_eq!(rules.evaluate(&toks, &order(3, 9, Family::Kcc20)), Ok(()));
        assert_eq!(rules.evaluate(&toks, &ListingInput { scale: Some(1), ..order(1, 8, Family::Kron) }), Ok(()));
        assert_eq!(toks.strict_template(&h(9)).unwrap().powers, vec!["freeze".to_string()]);
        // a program off the strict list, a family that is not the program's, a delisted token
        assert_eq!(rules.evaluate(&toks, &order(2, 5, Family::Kron)), Err("token_template_not_strict".into()));
        assert_eq!(rules.evaluate(&toks, &order(2, 8, Family::Kcc20)), Err("token_family_mismatch".into()));
        assert_eq!(rules.evaluate(&toks, &order(7, 8, Family::Kron)), Err("token_delisted".into()));
        // without a strict list the plain allowlist decides
        let plain = TokenAllowlist::from_entries([entry(1, 8, false)]);
        assert!(!plain.is_open());
        assert_eq!(rules.evaluate(&plain, &order(2, 8, Family::Kron)), Err("token_not_allowlisted".into()));
    }

    /// A covenant id only commits to the genesis output hashes, so the registry says per token whether every genesis
    /// output was verified. The flag is read from the raw document (whatever the typed registry schema), withdraws `official`
    /// when false, drives the `genesis_unverified` warning and the optional `require_genesis_verified` listing rule.
    #[test]
    fn the_registry_genesis_flag_is_honoured() {
        let doc = |flag: &str| {
            format!(
                r#"[{{"ticker":"A","covenantId":"{}","official":true{flag}}},{{"ticker":"B","covenantId":"{}","official":true}}]"#,
                h(1),
                h(2)
            )
        };
        let t = TokenAllowlist::parse(&doc(r#","genesis_verified":true"#)).unwrap();
        assert_eq!(t.genesis_verified(&h(1)), Some(true));
        assert_eq!(t.standing_of(t.get(&h(1)).unwrap()), "official");
        assert!(t.warnings_for(&h(1)).is_empty());
        // the registry does not say: still official (unchanged data), but the warning is shown (fail closed in the display)
        assert_eq!(t.genesis_verified(&h(2)), None);
        assert_eq!(t.standing_of(t.get(&h(2)).unwrap()), "official");
        assert_eq!(t.warnings_for(&h(2)), vec![WARN_GENESIS_UNVERIFIED]);
        // explicitly contaminated: the badge is withdrawn
        let t = TokenAllowlist::parse(&doc(r#","genesisVerified":false"#)).unwrap();
        assert_eq!(t.standing_of(t.get(&h(1)).unwrap()), "unverified");
        assert_eq!(t.warnings_for(&h(1)), vec![WARN_GENESIS_UNVERIFIED]);
        // the listing rule
        let rules = ListingRules {
            require_genesis_verified: true,
            min_order_value_sompi: 0,
            max_expiry_span_daa: 1 << 40,
            ..Default::default()
        };
        let order = |cov: u8| ListingInput {
            token_cov_id: Some(h(cov)),
            scale: Some(1),
            amount: Some(1),
            price: Some(1),
            expiry_daa: Some(1),
            genesis_daa: 1,
            carrier: 1,
            ..Default::default()
        };
        let t = TokenAllowlist::parse(&doc(r#","genesis_verified":true"#)).unwrap();
        assert_eq!(rules.evaluate(&t, &order(1)), Ok(()));
        assert_eq!(rules.evaluate(&t, &order(2)), Err("genesis_not_verified".into()));
        assert_eq!(rules.evaluate(&t, &order(9)), Err("genesis_not_verified".into()));
        // off by default
        assert!(!ListingRules::default().require_genesis_verified);
    }

    /// The list says which registry document it was read from (path and SHA-256), so a client can check the operator's judgements.
    #[test]
    fn a_loaded_registry_carries_its_source_and_hash() {
        let dir = std::env::temp_dir().join(format!("kob-reg-info-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("tokens.json");
        let text = format!(r#"[{{"ticker":"A","covenantId":"{}"}}]"#, h(1));
        std::fs::write(&p, &text).unwrap();
        let t = TokenAllowlist::load(&p).unwrap();
        let embedded = TokenAllowlist::embedded_default().unwrap();
        let e = embedded.info().expect("provenance");
        assert_eq!(
            (e.source.as_str(), e.sha256.clone()),
            ("embedded:registry/tokens.json", kob_protocol::registry::default_registry_sha256())
        );
        assert_eq!(embedded.len(), 8);
        let info = t.info().expect("provenance");
        assert!(info.source.ends_with("tokens.json"));
        use sha2::{Digest, Sha256};
        assert_eq!(info.sha256, crate::hex::encode(&Sha256::digest(text.as_bytes())));
        assert!(TokenAllowlist::parse(&text).unwrap().info().is_none(), "in-memory lists have none");
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
