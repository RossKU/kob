//! KCC-23 "MJ: Metadata JSON" (kaspanet/kccs `kcc-0023.md`, merged 2026-10-14 as #26, `main` 1516a6c, status Draft): JSON
//! metadata bound to a covenant id through the genesis's spent outpoint, and KOB's token-metadata convention on top of it.
//!
//! **Codec** ([`encode`], [`decode`], section 3.1). The payload of a version 1 *preparation transaction* is the codec byte `0x00`
//! followed by exactly one UTF-8 JSON object. The object must be I-JSON (RFC 7493: valid UTF-8, no unpaired surrogates, no
//! duplicate member names), carry no byte-order mark, convert every number to a finite IEEE-754 binary64 value and nest at most
//! 64 levels (the root object is depth 1). Any other codec byte is refused (KCC-23 defines no other). The commitment covers the
//! exact bytes, so [`encode`] never re-serialises: it validates and prefixes the text it is given.
//!
//! **Binding** ([`verify_binding`], section 3.3). The genesis spends an output of the preparation transaction; under KIP-20 the
//! covenant id commits to that outpoint and to the genesis output group, and the version 1 transaction id commits to the payload.
//! The check is the reference implementation's (`kcc-0023/implementation/src/lib.rs`) step for step: preparation version 1,
//! genesis version >= 1, payload valid, an input of the genesis spends an existing output of the recomputed preparation id, and
//! the covenant id recomputed from that outpoint and the group of outputs it authorises equals the expected one. It establishes
//! the binding only, not that either transaction was accepted.
//!
//! **Token convention** ([`TokenMetadata`], [`TokenDeclaration`]; `docs/spec/token-metadata.md`). KCC-23 leaves field names to
//! ecosystem conventions. KOB writes, in this order and only when set: `name`, `symbol`, `decimals` (the three of the KCC-23
//! example), then `description`, `logo`, `website`, `x`, `telegram`. A reader takes each field independently: a field of the wrong
//! type or out of range is dropped and named in [`TokenDeclaration::issues`], unknown fields are ignored, and a declaration
//! without a usable `decimals` leaves the token's decimals undeclared.

use kaspa_consensus_core::hashing::covenant_id::covenant_id;
use kaspa_consensus_core::hashing::tx::id as tx_id;
use kaspa_consensus_core::tx::Transaction;
use kaspa_consensus_core::Hash;
use serde::de::{self, DeserializeSeed, MapAccess, SeqAccess, Visitor};
use serde_json::{Map, Number, Value};
use std::collections::BTreeSet;
use std::fmt;

/// The codec byte of a UTF-8 JSON object (the only one KCC-23 defines).
pub const CODEC_JSON: u8 = 0x00;
/// Deepest object / array nesting KCC-23 allows (the root object is depth 1).
pub const MAX_DEPTH: usize = 64;
/// Largest preparation payload KOB's readers decode (codec byte included). A reader policy, not a KCC-23 rule: a token
/// declaration of KOB's convention is far smaller ([`MAX_TOKEN_PAYLOAD`]); larger payloads are left undecoded.
pub const MAX_READ_PAYLOAD: usize = 16 * 1024;
/// Largest token-metadata payload KOB writes (codec byte included): it is paid for in mass by the creator.
pub const MAX_TOKEN_PAYLOAD: usize = 1024;
/// Most decimals a declaration may state (the registry's limit, [`crate::registry::MAX_DECIMALS`]).
pub const MAX_DECIMALS: u8 = crate::registry::MAX_DECIMALS;
/// Longest `name` a reader takes, in characters.
pub const MAX_NAME_CHARS: usize = 64;
/// Longest `symbol` a reader takes, in characters (KOB itself writes 2..=12 of `A-Z 0-9`).
pub const MAX_SYMBOL_CHARS: usize = 32;
/// Longest `description` and link a reader takes, in characters (the registry's display limit).
pub const MAX_TEXT_CHARS: usize = 512;

/// Why a payload is not a KCC-23 declaration.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum Kcc23Error {
    /// The payload is empty: no codec byte.
    #[error("empty payload: no codec byte")]
    Empty,
    /// The codec byte is not one KCC-23 defines.
    #[error("unknown codec byte {0:#04x} (KCC-23 defines only 0x00, UTF-8 JSON)")]
    UnknownCodec(u8),
    /// Larger than the reader or writer limit.
    #[error("payload of {len} bytes exceeds {max}")]
    TooLarge {
        /// Payload length, codec byte included.
        len: usize,
        /// The limit.
        max: usize,
    },
    /// The bytes after the codec byte are not UTF-8.
    #[error("the JSON text is not valid UTF-8")]
    Utf8,
    /// The JSON text starts with a byte-order mark.
    #[error("the JSON text starts with a byte-order mark")]
    Bom,
    /// Not exactly one JSON value (syntax error, trailing data, lone surrogate, number out of range ...).
    #[error("invalid JSON: {0}")]
    Json(String),
    /// The value is not an object.
    #[error("the JSON value is not an object")]
    NotObject,
    /// An object repeats a member name (I-JSON forbids it).
    #[error("duplicate member name {0:?}")]
    DuplicateKey(String),
    /// Nesting deeper than [`MAX_DEPTH`].
    #[error("nesting deeper than {MAX_DEPTH} levels")]
    TooDeep,
    /// A number that is not a finite binary64 value.
    #[error("a number is not a finite IEEE-754 binary64 value")]
    NonFinite,
}

/// Why a preparation / genesis pair does not bind a declaration to a covenant id.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum BindingError {
    /// The preparation transaction is not version 1.
    #[error("the preparation transaction is version {0}, KCC-23 requires version 1")]
    PreparationVersion(u16),
    /// The genesis transaction is version 0 (no covenant bindings).
    #[error("the genesis transaction is version {0}, KIP-20 covenants need version 1 or later")]
    GenesisVersion(u16),
    /// The preparation payload is not a declaration.
    #[error("preparation payload: {0}")]
    Payload(#[from] Kcc23Error),
    /// No input of the genesis spends an output of the preparation transaction into a group of the expected covenant id.
    #[error("no genesis input spends an output of preparation {preparation} into the covenant {covenant_id}")]
    NotBound {
        /// The recomputed preparation transaction id.
        preparation: Hash,
        /// The expected covenant id.
        covenant_id: Hash,
    },
}

// ------------------------------------------------------------------------------------------------ codec

/// Strict I-JSON reader: one value at `depth`, refusing duplicate member names, nesting beyond [`MAX_DEPTH`] and non-finite numbers.
struct Strict {
    depth: usize,
}

const DUP: &str = "kcc23:duplicate:";
const DEEP: &str = "kcc23:too-deep";
const NONFINITE: &str = "kcc23:non-finite";

impl<'de> DeserializeSeed<'de> for Strict {
    type Value = Value;
    fn deserialize<D: de::Deserializer<'de>>(self, d: D) -> Result<Value, D::Error> {
        d.deserialize_any(self)
    }
}

impl<'de> Visitor<'de> for Strict {
    type Value = Value;
    fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.write_str("a JSON value")
    }
    fn visit_bool<E>(self, v: bool) -> Result<Value, E> {
        Ok(Value::Bool(v))
    }
    fn visit_i64<E>(self, v: i64) -> Result<Value, E> {
        Ok(Value::Number(v.into()))
    }
    fn visit_u64<E>(self, v: u64) -> Result<Value, E> {
        Ok(Value::Number(v.into()))
    }
    fn visit_f64<E: de::Error>(self, v: f64) -> Result<Value, E> {
        Number::from_f64(v).map(Value::Number).ok_or_else(|| E::custom(NONFINITE))
    }
    fn visit_str<E>(self, v: &str) -> Result<Value, E> {
        Ok(Value::String(v.to_owned()))
    }
    fn visit_string<E>(self, v: String) -> Result<Value, E> {
        Ok(Value::String(v))
    }
    fn visit_unit<E>(self) -> Result<Value, E> {
        Ok(Value::Null)
    }
    fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<Value, A::Error> {
        let depth = self.depth + 1;
        if depth > MAX_DEPTH {
            return Err(de::Error::custom(DEEP));
        }
        let mut out = vec![];
        while let Some(v) = seq.next_element_seed(Strict { depth })? {
            out.push(v);
        }
        Ok(Value::Array(out))
    }
    fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Value, A::Error> {
        let depth = self.depth + 1;
        if depth > MAX_DEPTH {
            return Err(de::Error::custom(DEEP));
        }
        let mut seen = BTreeSet::new();
        let mut out = Map::new();
        while let Some(k) = map.next_key::<String>()? {
            if !seen.insert(k.clone()) {
                return Err(de::Error::custom(format!("{DUP}{k}")));
            }
            let v = map.next_value_seed(Strict { depth })?;
            out.insert(k, v);
        }
        Ok(Value::Object(out))
    }
}

/// Parses `text` as one strict I-JSON object (the rules of KCC-23 section 3.1 after the codec byte).
pub fn parse_object(text: &str) -> Result<Map<String, Value>, Kcc23Error> {
    if text.starts_with('\u{feff}') {
        return Err(Kcc23Error::Bom);
    }
    let mut de = serde_json::Deserializer::from_str(text);
    let v = Strict { depth: 0 }.deserialize(&mut de).map_err(json_error)?;
    de.end().map_err(json_error)?;
    match v {
        Value::Object(m) => Ok(m),
        _ => Err(Kcc23Error::NotObject),
    }
}

fn json_error(e: serde_json::Error) -> Kcc23Error {
    let s = e.to_string();
    if let Some(i) = s.find(DUP) {
        let rest = &s[i + DUP.len()..];
        let key = rest.split(" at line ").next().unwrap_or(rest);
        return Kcc23Error::DuplicateKey(key.to_string());
    }
    if s.contains(DEEP) || s.contains("recursion limit exceeded") {
        return Kcc23Error::TooDeep;
    }
    if s.contains(NONFINITE) {
        return Kcc23Error::NonFinite;
    }
    Kcc23Error::Json(s)
}

/// The KCC-23 payload of `json`: `0x00` followed by its exact UTF-8 bytes (whitespace and member order kept: the commitment covers
/// them). Refused unless `json` is one strict I-JSON object.
pub fn encode(json: &str) -> Result<Vec<u8>, Kcc23Error> {
    parse_object(json)?;
    let mut p = Vec::with_capacity(json.len() + 1);
    p.push(CODEC_JSON);
    p.extend_from_slice(json.as_bytes());
    Ok(p)
}

/// The JSON text of a KCC-23 payload, after checking the codec byte and every rule of section 3.1. No size limit: callers that
/// read untrusted payloads use [`decode_limited`].
pub fn decode(payload: &[u8]) -> Result<&str, Kcc23Error> {
    let (&codec, rest) = payload.split_first().ok_or(Kcc23Error::Empty)?;
    if codec != CODEC_JSON {
        return Err(Kcc23Error::UnknownCodec(codec));
    }
    let text = std::str::from_utf8(rest).map_err(|_| Kcc23Error::Utf8)?;
    parse_object(text)?;
    Ok(text)
}

/// [`decode`] with a size limit (codec byte included), returning the parsed object too.
pub fn decode_limited(payload: &[u8], max: usize) -> Result<(&str, Map<String, Value>), Kcc23Error> {
    if payload.len() > max {
        return Err(Kcc23Error::TooLarge { len: payload.len(), max });
    }
    let text = decode(payload)?;
    Ok((text, parse_object(text)?))
}

/// True when `payload` starts with the JSON codec byte: the cheap pre-filter of an indexer before [`decode_limited`].
pub fn looks_like_declaration(payload: &[u8]) -> bool {
    payload.first() == Some(&CODEC_JSON) && payload.len() >= 3
}

// ------------------------------------------------------------------------------------------------ binding

/// Section 3.3: the declaration of `preparation` when it is bound to `expected` through `genesis`. Returns the JSON text.
pub fn verify_binding<'a>(preparation: &'a Transaction, genesis: &Transaction, expected: Hash) -> Result<&'a str, BindingError> {
    if preparation.version != 1 {
        return Err(BindingError::PreparationVersion(preparation.version));
    }
    if genesis.version < 1 {
        return Err(BindingError::GenesisVersion(genesis.version));
    }
    let text = decode(&preparation.payload)?;
    let preparation_id = tx_id(preparation);
    for (input_index, input) in genesis.inputs.iter().enumerate() {
        let outpoint = input.previous_outpoint;
        if outpoint.transaction_id != preparation_id || preparation.outputs.get(outpoint.index as usize).is_none() {
            continue;
        }
        let Ok(authorizing) = u16::try_from(input_index) else { continue };
        let group: Vec<(u32, _)> = genesis
            .outputs
            .iter()
            .enumerate()
            .filter(|(_, o)| o.covenant.is_some_and(|b| b.authorizing_input == authorizing && b.covenant_id == expected))
            .map(|(i, o)| (i as u32, o))
            .collect();
        if !group.is_empty() && covenant_id(outpoint, group.into_iter()) == expected {
            return Ok(text);
        }
    }
    Err(BindingError::NotBound { preparation: preparation_id, covenant_id: expected })
}

// ------------------------------------------------------------------------------------------------ KOB token convention

/// Characters refused in any displayed text: control, zero-width and bidi controls (the registry's rule).
pub fn has_bad_char(s: &str) -> bool {
    s.chars().any(|c| {
        c.is_control()
            || matches!(c, '\u{200B}'..='\u{200F}' | '\u{202A}'..='\u{202E}' | '\u{2060}'..='\u{2064}' | '\u{2066}'..='\u{2069}' | '\u{FEFF}')
    })
}

fn text_ok(s: &str, max: usize) -> bool {
    !s.trim().is_empty() && s.chars().count() <= max && !has_bad_char(s)
}

/// `https://x.com/<handle>` with a handle of 1..=15 of `A-Z a-z 0-9 _` (the canonical form KOB writes).
pub fn is_x_url(s: &str) -> bool {
    s.strip_prefix("https://x.com/")
        .is_some_and(|h| (1..=15).contains(&h.len()) && h.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_'))
}

/// `https://t.me/<handle>` with a handle of 5..=32 of `A-Z a-z 0-9 _` (the canonical form KOB writes).
pub fn is_telegram_url(s: &str) -> bool {
    s.strip_prefix("https://t.me/")
        .is_some_and(|h| (5..=32).contains(&h.len()) && h.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_'))
}

fn handle_of<'a>(input: &'a str, hosts: &[&str], len: std::ops::RangeInclusive<usize>) -> Option<&'a str> {
    let s = input.trim();
    let h = hosts
        .iter()
        .find_map(|host| {
            s.strip_prefix("https://").and_then(|r| r.strip_prefix("www.").or(Some(r))).and_then(|r| r.strip_prefix(host))
        })
        .map(|r| r.strip_suffix('/').unwrap_or(r))
        .or_else(|| (!s.contains('/')).then(|| s.strip_prefix('@').unwrap_or(s)))?;
    (len.contains(&h.len()) && h.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')).then_some(h)
}

/// The canonical X address of `input`: a handle (`@name`, `name`) or an `https://x.com/name` / `https://twitter.com/name` address
/// becomes `https://x.com/name`; anything else is None.
pub fn x_url(input: &str) -> Option<String> {
    handle_of(input, &["x.com/", "twitter.com/"], 1..=15).map(|h| format!("https://x.com/{h}"))
}

/// The canonical Telegram address of `input`: a handle or an `https://t.me/name` / `https://telegram.me/name` address becomes
/// `https://t.me/name`; anything else is None.
pub fn telegram_url(input: &str) -> Option<String> {
    handle_of(input, &["t.me/", "telegram.me/"], 5..=32).map(|h| format!("https://t.me/{h}"))
}

/// A link a reader takes: `https://` (or, for `logo`, also `ipfs://`), at most [`MAX_TEXT_CHARS`], no space or bad character.
fn link_ok(s: &str, ipfs: bool) -> bool {
    (s.starts_with("https://") || (ipfs && s.starts_with("ipfs://")))
        && s.len() > "https://".len()
        && s.chars().count() <= MAX_TEXT_CHARS
        && !has_bad_char(s)
        && !s.chars().any(char::is_whitespace)
}

/// The token metadata KOB writes into a preparation payload (`docs/spec/token-metadata.md`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TokenMetadata {
    /// Display name, 1..=64 characters.
    pub name: String,
    /// Ticker: 2..=12 of `A-Z 0-9` (KOB's issuance rule).
    pub symbol: String,
    /// Decimal places of the whole token, 0..=18.
    pub decimals: u8,
    /// Free text, at most 512 characters.
    pub description: Option<String>,
    /// `https://` or `ipfs://` address of the logo.
    pub logo: Option<String>,
    /// `https://` address of the website.
    pub website: Option<String>,
    /// `https://x.com/<handle>`.
    pub x: Option<String>,
    /// `https://t.me/<handle>`.
    pub telegram: Option<String>,
}

impl TokenMetadata {
    /// Checks every field as a reader would take it (so what KOB writes reads back whole) and the payload size.
    pub fn validate(&self) -> Result<(), String> {
        if !text_ok(&self.name, MAX_NAME_CHARS) {
            return Err(format!("name must be 1..={MAX_NAME_CHARS} characters without control, zero-width or bidi characters"));
        }
        if !(2..=12).contains(&self.symbol.len()) || !self.symbol.bytes().all(|b| b.is_ascii_uppercase() || b.is_ascii_digit()) {
            return Err("symbol must be 2..=12 uppercase ASCII letters or digits".into());
        }
        if self.decimals > MAX_DECIMALS {
            return Err(format!("decimals must be at most {MAX_DECIMALS}"));
        }
        if let Some(d) = &self.description {
            if !text_ok(d, MAX_TEXT_CHARS) {
                return Err(format!(
                    "description must be 1..={MAX_TEXT_CHARS} characters without control, zero-width or bidi characters"
                ));
            }
        }
        if self.logo.as_deref().is_some_and(|l| !link_ok(l, true)) {
            return Err("logo must be an https:// or ipfs:// address".into());
        }
        if self.website.as_deref().is_some_and(|w| !link_ok(w, false)) {
            return Err("website must be an https:// address".into());
        }
        if self.x.as_deref().is_some_and(|x| !is_x_url(x)) {
            return Err("x must be https://x.com/<handle> (1..=15 of A-Z a-z 0-9 _)".into());
        }
        if self.telegram.as_deref().is_some_and(|t| !is_telegram_url(t)) {
            return Err("telegram must be https://t.me/<handle> (5..=32 of A-Z a-z 0-9 _)".into());
        }
        let len = self.payload_unchecked().len();
        if len > MAX_TOKEN_PAYLOAD {
            return Err(format!(
                "the token metadata takes {len} bytes on chain, at most {MAX_TOKEN_PAYLOAD}: shorten the description or the links"
            ));
        }
        Ok(())
    }

    /// The compact JSON text, members in the fixed order `name, symbol, decimals, description, logo, website, x, telegram` (unset
    /// optional members are left out).
    pub fn json(&self) -> String {
        let s = |v: &str| serde_json::to_string(v).expect("json string");
        let mut out = format!("{{\"name\":{},\"symbol\":{},\"decimals\":{}", s(&self.name), s(&self.symbol), self.decimals);
        for (k, v) in [
            ("description", &self.description),
            ("logo", &self.logo),
            ("website", &self.website),
            ("x", &self.x),
            ("telegram", &self.telegram),
        ] {
            if let Some(v) = v {
                out.push_str(&format!(",\"{k}\":{}", s(v)));
            }
        }
        out.push('}');
        out
    }

    fn payload_unchecked(&self) -> Vec<u8> {
        let json = self.json();
        let mut p = Vec::with_capacity(json.len() + 1);
        p.push(CODEC_JSON);
        p.extend_from_slice(json.as_bytes());
        p
    }

    /// The preparation payload: `0x00` and [`TokenMetadata::json`]. Validates first.
    pub fn payload(&self) -> Result<Vec<u8>, String> {
        self.validate()?;
        encode(&self.json()).map_err(|e| e.to_string())
    }
}

/// What a reader takes from a declaration under KOB's convention: every field independently, dropped (and named in `issues`) when
/// it has the wrong type or is out of range. Unknown fields are ignored.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize)]
pub struct TokenDeclaration {
    /// `name`: 1..=64 characters without bad characters.
    pub name: Option<String>,
    /// `symbol`: 1..=32 characters without bad characters or white space.
    pub symbol: Option<String>,
    /// `decimals`: a JSON integer 0..=18.
    pub decimals: Option<u8>,
    /// `description`: 1..=512 characters without bad characters (line breaks are refused too).
    pub description: Option<String>,
    /// `logo`: an https:// or ipfs:// link.
    pub logo: Option<String>,
    /// `website`: an https:// link.
    pub website: Option<String>,
    /// `x`: `https://x.com/<handle>`.
    pub x: Option<String>,
    /// `telegram`: `https://t.me/<handle>`.
    pub telegram: Option<String>,
    /// The fields present but not taken (`decimals: not an integer from 0 to 18`, ...).
    pub issues: Vec<String>,
}

impl TokenDeclaration {
    /// Reads KOB's token fields from a decoded declaration object.
    pub fn from_object(obj: &Map<String, Value>) -> Self {
        let mut d = TokenDeclaration::default();
        let mut issues = vec![];
        let text = |k: &str, max: usize, ok: &dyn Fn(&str) -> bool, issues: &mut Vec<String>| -> Option<String> {
            match obj.get(k)? {
                Value::String(s) if text_ok(s, max) && ok(s) => Some(s.clone()),
                Value::String(_) => {
                    issues.push(issue(k, "not an acceptable value"));
                    None
                }
                _ => {
                    issues.push(issue(k, "not a string"));
                    None
                }
            }
        };
        d.name = text("name", MAX_NAME_CHARS, &|_| true, &mut issues);
        d.symbol = text("symbol", MAX_SYMBOL_CHARS, &|s| !s.chars().any(char::is_whitespace), &mut issues);
        d.description = text("description", MAX_TEXT_CHARS, &|_| true, &mut issues);
        d.logo = text("logo", MAX_TEXT_CHARS, &|s| link_ok(s, true), &mut issues);
        d.website = text("website", MAX_TEXT_CHARS, &|s| link_ok(s, false), &mut issues);
        d.x = text("x", MAX_TEXT_CHARS, &is_x_url, &mut issues);
        d.telegram = text("telegram", MAX_TEXT_CHARS, &is_telegram_url, &mut issues);
        d.decimals = match obj.get("decimals") {
            None => None,
            Some(Value::Number(n)) => match n.as_u64() {
                Some(v) if v <= u64::from(MAX_DECIMALS) => Some(v as u8),
                _ => {
                    issues.push(issue("decimals", "not an integer from 0 to 18"));
                    None
                }
            },
            Some(_) => {
                issues.push(issue("decimals", "not a number"));
                None
            }
        };
        d.issues = issues;
        d
    }

    /// Decodes a preparation payload (at most [`MAX_READ_PAYLOAD`] bytes) and reads its token fields.
    pub fn from_payload(payload: &[u8]) -> Result<Self, Kcc23Error> {
        let (_, obj) = decode_limited(payload, MAX_READ_PAYLOAD)?;
        Ok(Self::from_object(&obj))
    }
}

fn issue(k: &str, why: &str) -> String {
    format!("{k}: {why}")
}

// ------------------------------------------------------------------------------------------------ launch proof

/// Format tag of a [`LaunchProof`].
pub const PROOF_FORMAT: &str = "kob-kcc23-proof/1";

/// One input of a proof transaction: what the version 1 transaction id commits to (no signature script, no compute budget).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ProofInput {
    /// Spent transaction id.
    #[serde(with = "crate::json::field")]
    pub transaction_id: [u8; 32],
    /// Spent output index.
    pub index: u32,
    /// Input sequence.
    #[serde(with = "crate::json::field")]
    pub sequence: u64,
}

/// The script public key of a proof output.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ProofSpk {
    /// Script version.
    pub version: u16,
    /// Script bytes.
    #[serde(with = "crate::json::field")]
    pub script: Vec<u8>,
}

/// A KIP-20 covenant binding of a proof output.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ProofCovenant {
    /// Index of the authorising input.
    pub authorizing_input: u16,
    /// Covenant id.
    #[serde(with = "crate::json::field")]
    pub covenant_id: [u8; 32],
}

/// One output of a proof transaction.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ProofOutput {
    /// Sompi.
    #[serde(with = "crate::json::field")]
    pub value: u64,
    /// Script public key.
    pub script_public_key: ProofSpk,
    /// Covenant binding, if any.
    #[serde(default)]
    pub covenant: Option<ProofCovenant>,
}

/// The fields of a transaction its version 1 id commits to (`kaspa_consensus_core::hashing::tx::id`): enough to recompute the id
/// and the covenant id of a genesis group, nothing to execute.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ProofTx {
    /// Transaction version.
    pub version: u16,
    /// Inputs.
    pub inputs: Vec<ProofInput>,
    /// Outputs.
    pub outputs: Vec<ProofOutput>,
    /// Lock time.
    #[serde(with = "crate::json::field")]
    pub lock_time: u64,
    /// Subnetwork id (20 bytes).
    #[serde(with = "crate::json::field")]
    pub subnetwork_id: Vec<u8>,
    /// Gas.
    #[serde(with = "crate::json::field")]
    pub gas: u64,
    /// Payload.
    #[serde(with = "crate::json::field")]
    pub payload: Vec<u8>,
}

impl ProofTx {
    /// The id-relevant fields of `tx`.
    pub fn of(tx: &Transaction) -> Self {
        ProofTx {
            version: tx.version,
            inputs: tx
                .inputs
                .iter()
                .map(|i| ProofInput {
                    transaction_id: i.previous_outpoint.transaction_id.as_bytes(),
                    index: i.previous_outpoint.index,
                    sequence: i.sequence,
                })
                .collect(),
            outputs: tx
                .outputs
                .iter()
                .map(|o| ProofOutput {
                    value: o.value,
                    script_public_key: ProofSpk {
                        version: o.script_public_key.version(),
                        script: o.script_public_key.script().to_vec(),
                    },
                    covenant: o
                        .covenant
                        .map(|c| ProofCovenant { authorizing_input: c.authorizing_input, covenant_id: c.covenant_id.as_bytes() }),
                })
                .collect(),
            lock_time: tx.lock_time,
            subnetwork_id: AsRef::<[u8]>::as_ref(&tx.subnetwork_id).to_vec(),
            gas: tx.gas,
            payload: tx.payload.clone(),
        }
    }

    /// A transaction with these fields (empty signature scripts, zero compute budgets and storage mass: none of them enters the id).
    pub fn to_tx(&self) -> Result<Transaction, String> {
        use kaspa_consensus_core::subnets::SubnetworkId;
        use kaspa_consensus_core::tx::{CovenantBinding, ScriptPublicKey, TransactionInput, TransactionOutpoint, TransactionOutput};
        let subnet: [u8; 20] = self
            .subnetwork_id
            .as_slice()
            .try_into()
            .map_err(|_| format!("subnetwork id of {} bytes, 20 expected", self.subnetwork_id.len()))?;
        let inputs = self
            .inputs
            .iter()
            .map(|i| {
                TransactionInput::new_with_compute_budget(
                    TransactionOutpoint::new(Hash::from_bytes(i.transaction_id), i.index),
                    vec![],
                    i.sequence,
                    0,
                )
            })
            .collect();
        let outputs = self
            .outputs
            .iter()
            .map(|o| {
                let mut out = TransactionOutput::new(
                    o.value,
                    ScriptPublicKey::from_vec(o.script_public_key.version, o.script_public_key.script.clone()),
                );
                out.covenant = o.covenant.as_ref().map(|c| CovenantBinding::new(c.authorizing_input, Hash::from_bytes(c.covenant_id)));
                out
            })
            .collect();
        Ok(Transaction::new(
            self.version,
            inputs,
            outputs,
            self.lock_time,
            SubnetworkId::from_bytes(subnet),
            self.gas,
            self.payload.clone(),
        ))
    }
}

/// A KCC-23 launch proof (section 3.3): the preparation and genesis transactions of a covenant id, in the fields their ids commit
/// to. Whoever supplies it (an indexer, a file), [`LaunchProof::verify`] recomputes everything and either returns the declaration
/// bound to `covenant_id` or refuses: a supplier can withhold a declaration, never forge one.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct LaunchProof {
    /// [`PROOF_FORMAT`].
    pub format: String,
    /// The covenant id the declaration is claimed for.
    #[serde(with = "crate::json::field")]
    pub covenant_id: [u8; 32],
    /// The preparation transaction.
    pub preparation: ProofTx,
    /// The genesis transaction.
    pub genesis: ProofTx,
}

/// A declaration that [`LaunchProof::verify`] found bound to its covenant id.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct VerifiedDeclaration {
    /// The covenant id.
    #[serde(with = "crate::json::field")]
    pub covenant_id: [u8; 32],
    /// Recomputed preparation transaction id.
    #[serde(with = "crate::json::field")]
    pub preparation_txid: [u8; 32],
    /// Recomputed genesis transaction id.
    #[serde(with = "crate::json::field")]
    pub genesis_txid: [u8; 32],
    /// The declaration's JSON text, exactly as committed.
    pub json: String,
    /// KOB's token fields read from it.
    pub token: TokenDeclaration,
}

impl LaunchProof {
    /// The proof of `covenant_id` from its preparation and genesis transactions.
    pub fn of(preparation: &Transaction, genesis: &Transaction, covenant_id: Hash) -> Self {
        LaunchProof {
            format: PROOF_FORMAT.into(),
            covenant_id: covenant_id.as_bytes(),
            preparation: ProofTx::of(preparation),
            genesis: ProofTx::of(genesis),
        }
    }

    /// Recomputes both transaction ids and the covenant id ([`verify_binding`]) and reads the declaration (at most
    /// [`MAX_READ_PAYLOAD`] bytes).
    pub fn verify(&self) -> Result<VerifiedDeclaration, String> {
        if self.format != PROOF_FORMAT {
            return Err(format!("unknown proof format {:?}", self.format));
        }
        if self.preparation.payload.len() > MAX_READ_PAYLOAD {
            return Err(Kcc23Error::TooLarge { len: self.preparation.payload.len(), max: MAX_READ_PAYLOAD }.to_string());
        }
        let prep = self.preparation.to_tx()?;
        let genesis = self.genesis.to_tx()?;
        let id = Hash::from_bytes(self.covenant_id);
        let json = verify_binding(&prep, &genesis, id).map_err(|e| e.to_string())?.to_string();
        let token = TokenDeclaration::from_object(&parse_object(&json).map_err(|e| e.to_string())?);
        Ok(VerifiedDeclaration {
            covenant_id: self.covenant_id,
            preparation_txid: tx_id(&prep).as_bytes(),
            genesis_txid: tx_id(&genesis).as_bytes(),
            json,
            token,
        })
    }
}

// ------------------------------------------------------------------------------------------------ tests

#[cfg(test)]
mod tests {
    use super::*;
    use kaspa_consensus_core::subnets::SUBNETWORK_ID_NATIVE;
    use kaspa_consensus_core::tx::{CovenantBinding, ScriptPublicKey, TransactionInput, TransactionOutpoint, TransactionOutput};

    #[test]
    fn declaration_vectors_of_the_spec() {
        // kcc-0023.md section 6 and the reference implementation's own cases
        for p in [b"\0{}".as_slice(), b"\0{\"name\":\"\xc3\xa9\"}".as_slice(), b"\0 { \"b\": 2, \"a\": 1 } ".as_slice()] {
            let json = decode(p).unwrap();
            assert_eq!(encode(json).unwrap(), p);
        }
        assert_eq!(decode(b"{}"), Err(Kcc23Error::UnknownCodec(b'{')));
        assert_eq!(decode(b"\x01{}"), Err(Kcc23Error::UnknownCodec(1)));
        assert_eq!(decode(b"\0[]"), Err(Kcc23Error::NotObject));
        assert_eq!(decode(b"\0{\"x\":\"\xff\"}"), Err(Kcc23Error::Utf8));
        assert!(matches!(decode(b"\0{}{}"), Err(Kcc23Error::Json(_))));
        assert_eq!(decode(b""), Err(Kcc23Error::Empty));
        assert!(encode("[]").is_err() && encode("{}{}").is_err());
    }

    #[test]
    fn i_json_rules() {
        assert_eq!(decode("\0\u{feff}{}".as_bytes()), Err(Kcc23Error::Bom));
        assert_eq!(decode(b"\0{\"a\":1,\"a\":2}"), Err(Kcc23Error::DuplicateKey("a".into())));
        assert_eq!(decode(b"\0{\"o\":{\"k\":1,\"k\":1}}"), Err(Kcc23Error::DuplicateKey("k".into())));
        assert!(decode(b"\0{\"n\":1e400}").is_err(), "out of binary64 range");
        assert!(decode(b"\0{\"s\":\"\\ud800\"}").is_err(), "lone surrogate");
        assert!(decode(b"\0{\"s\":\"\\ud83d\\ude00\"}").is_ok(), "a surrogate pair is fine");
        assert!(decode(b"\0{\"n\":18446744073709551616}").is_ok(), "a large integer converts to a finite binary64");
        // depth: the root object is 1; 64 levels pass, 65 do not
        let nest = |levels: usize| format!("\0{}{}", "{\"a\":".repeat(levels - 1) + "{", "}".repeat(levels));
        assert!(decode(nest(64).as_bytes()).is_ok());
        assert_eq!(decode(nest(65).as_bytes()), Err(Kcc23Error::TooDeep));
        let arrays = format!("\0{{\"a\":{}{}}}", "[".repeat(63), "]".repeat(63));
        assert!(decode(arrays.as_bytes()).is_ok());
        let arrays = format!("\0{{\"a\":{}{}}}", "[".repeat(64), "]".repeat(64));
        assert_eq!(decode(arrays.as_bytes()), Err(Kcc23Error::TooDeep));
        assert_eq!(decode(nest(300).as_bytes()), Err(Kcc23Error::TooDeep));
        assert!(matches!(decode_limited(&[0u8; 20], 10), Err(Kcc23Error::TooLarge { len: 20, max: 10 })));
    }

    fn meta() -> TokenMetadata {
        TokenMetadata { name: "Example Token".into(), symbol: "EXT".into(), decimals: 8, ..Default::default() }
    }

    #[test]
    fn token_metadata_is_compact_ordered_and_reads_back() {
        let m = meta();
        assert_eq!(m.json(), r#"{"name":"Example Token","symbol":"EXT","decimals":8}"#, "the KCC-23 example, byte for byte");
        let full = TokenMetadata {
            description: Some("A \"quoted\" token ✓".into()),
            logo: Some("ipfs://bafy".into()),
            website: Some("https://example.org".into()),
            x: Some("https://x.com/example".into()),
            telegram: Some("https://t.me/example_chat".into()),
            ..m.clone()
        };
        let p = full.payload().unwrap();
        assert_eq!(p[0], 0);
        assert!(std::str::from_utf8(&p[1..])
            .unwrap()
            .starts_with(r#"{"name":"Example Token","symbol":"EXT","decimals":8,"description":"#));
        let d = TokenDeclaration::from_payload(&p).unwrap();
        assert_eq!(
            (d.name.as_deref(), d.symbol.as_deref(), d.decimals, d.description.as_deref()),
            (Some("Example Token"), Some("EXT"), Some(8), Some("A \"quoted\" token ✓"))
        );
        assert_eq!((d.logo, d.website, d.x, d.telegram), (full.logo, full.website, full.x, full.telegram));
        assert!(d.issues.is_empty());
    }

    #[test]
    fn token_metadata_rules() {
        let bad = |f: &dyn Fn(&mut TokenMetadata)| {
            let mut m = meta();
            f(&mut m);
            m.validate().unwrap_err()
        };
        bad(&|m| m.name = "".into());
        bad(&|m| m.name = "a\u{202e}b".into());
        bad(&|m| m.symbol = "ext".into());
        bad(&|m| m.decimals = 19);
        bad(&|m| m.logo = Some("http://x".into()));
        bad(&|m| m.website = Some("ipfs://x".into()));
        bad(&|m| m.x = Some("@name".into()));
        bad(&|m| m.telegram = Some("https://t.me/abc".into()));
        let e = bad(&|m| m.description = Some("é".repeat(500)));
        assert!(e.contains("bytes on chain"), "{e}");
    }

    #[test]
    fn social_handles_become_canonical_addresses() {
        assert_eq!(x_url("@kaspa_x").as_deref(), Some("https://x.com/kaspa_x"));
        assert_eq!(x_url("kaspa").as_deref(), Some("https://x.com/kaspa"));
        assert_eq!(x_url("https://twitter.com/kaspa/").as_deref(), Some("https://x.com/kaspa"));
        assert_eq!(x_url("https://www.x.com/kaspa").as_deref(), Some("https://x.com/kaspa"));
        assert_eq!(x_url("https://x.com/a/b"), None);
        assert_eq!(x_url("http://x.com/kaspa"), None);
        assert_eq!(x_url("@this_handle_is_too_long"), None);
        assert_eq!(telegram_url("@kaspa_chat").as_deref(), Some("https://t.me/kaspa_chat"));
        assert_eq!(telegram_url("https://telegram.me/kaspa_chat").as_deref(), Some("https://t.me/kaspa_chat"));
        assert_eq!(telegram_url("@abc"), None);
        for u in [x_url("@k").unwrap(), telegram_url("kaspa_chat").unwrap()] {
            assert!(is_x_url(&u) || is_telegram_url(&u));
        }
    }

    #[test]
    fn a_reader_takes_each_field_on_its_own() {
        let d = TokenDeclaration::from_payload(
            br#" {"name":"N","symbol":"S","decimals":8.5,"logo":"javascript:alert(1)","x":"https://x.com/ok","extra":[1]}"#.as_slice(),
        )
        .unwrap_err();
        assert_eq!(d, Kcc23Error::UnknownCodec(b' '));
        let p = br#"{"name":"N","symbol":"S Y","decimals":8.5,"logo":"javascript:alert(1)","x":"https://x.com/ok","extra":[1]}"#;
        let d = TokenDeclaration::from_payload(&[&[0u8][..], p].concat()).unwrap();
        assert_eq!(
            (d.name.as_deref(), d.symbol, d.decimals, d.logo, d.x.as_deref()),
            (Some("N"), None, None, None, Some("https://x.com/ok"))
        );
        assert_eq!(d.issues.len(), 3, "{:?}", d.issues);
        for (json, dec) in [
            (r#"{"decimals":0}"#, Some(0)),
            (r#"{"decimals":18}"#, Some(18)),
            (r#"{"decimals":19}"#, None),
            (r#"{"decimals":-1}"#, None),
            (r#"{"decimals":"8"}"#, None),
            (r#"{}"#, None),
        ] {
            let d = TokenDeclaration::from_payload(&encode(json).unwrap()).unwrap();
            assert_eq!(d.decimals, dec, "{json}");
        }
        let d = TokenDeclaration::from_payload(&encode(r#"{"name":"line\nbreak","description":"tab\there"}"#).unwrap()).unwrap();
        assert_eq!((d.name, d.description, d.issues.len()), (None, None, 2));
    }

    fn spk() -> ScriptPublicKey {
        ScriptPublicKey::from_vec(0, vec![0x51])
    }

    /// The binding vector of kcc-0023.md section 6 (the full conformance file runs in kob-tests).
    #[test]
    fn binding_vector_and_mutations() {
        let payload = encode(r#"{"name":"Example Token","symbol":"EXT","decimals":8}"#).unwrap();
        let input = |txid: Hash, i: u32| TransactionInput::new_with_compute_budget(TransactionOutpoint::new(txid, i), vec![], 0, 0);
        let prep = Transaction::new(
            1,
            vec![input(Hash::from_bytes([0x11; 32]), 0)],
            vec![TransactionOutput::new(100_000_000, spk())],
            0,
            SUBNETWORK_ID_NATIVE,
            0,
            payload,
        );
        let prep_id = tx_id(&prep);
        assert_eq!(prep_id.to_string(), "206a0c31dbe45384b86de451a177fd7c8e986d278f2a86724d941937443113ef");
        let mut genesis = Transaction::new(
            1,
            vec![input(prep_id, 0)],
            vec![TransactionOutput::new(90_000_000, spk())],
            0,
            SUBNETWORK_ID_NATIVE,
            0,
            vec![],
        );
        let id = covenant_id(genesis.inputs[0].previous_outpoint, [(0u32, &genesis.outputs[0])].into_iter());
        assert_eq!(id.to_string(), "c86366cec738e9a1578b2d871ee493dcaabfd79156810f13f3b62429859e394e");
        genesis.outputs[0].covenant = Some(CovenantBinding::new(0, id));
        assert_eq!(verify_binding(&prep, &genesis, id).unwrap(), r#"{"name":"Example Token","symbol":"EXT","decimals":8}"#);

        let mut changed = prep.clone();
        let n = changed.payload.len() - 2;
        changed.payload[n] = b'9';
        assert!(matches!(verify_binding(&changed, &genesis, id), Err(BindingError::NotBound { .. })));
        let mut v0 = prep.clone();
        v0.version = 0;
        assert_eq!(verify_binding(&v0, &genesis, id), Err(BindingError::PreparationVersion(0)));
        let mut g = genesis.clone();
        g.inputs[0].previous_outpoint.index = 1;
        assert!(verify_binding(&prep, &g, id).is_err(), "spends an output the preparation does not have");
        let mut g = genesis.clone();
        g.outputs.push(g.outputs[0].clone());
        assert!(verify_binding(&prep, &g, id).is_err(), "the group changed, the id does not match");
        let mut g = genesis.clone();
        g.outputs[0].covenant = Some(CovenantBinding::new(1, id));
        assert!(verify_binding(&prep, &g, id).is_err(), "authorised by another input");
        let mut g = genesis.clone();
        g.version = 0;
        assert_eq!(verify_binding(&prep, &g, id), Err(BindingError::GenesisVersion(0)));
        let mut bad = prep.clone();
        bad.payload = b"\x01{}".to_vec();
        assert_eq!(verify_binding(&bad, &genesis, id), Err(BindingError::Payload(Kcc23Error::UnknownCodec(1))));
        assert!(verify_binding(&prep, &genesis, Hash::from_bytes([1; 32])).is_err(), "another covenant id");

        // the same pair as a launch proof, through its JSON form
        let proof = LaunchProof::of(&prep, &genesis, id);
        let text = serde_json::to_string(&proof).unwrap();
        assert!(text.contains(r#""value":"100000000""#), "64-bit values as strings: {text}");
        let back: LaunchProof = serde_json::from_str(&text).unwrap();
        let v = back.verify().unwrap();
        assert_eq!((v.preparation_txid, v.token.decimals), (prep_id.as_bytes(), Some(8)));
        assert_eq!(v.genesis_txid, tx_id(&genesis).as_bytes());
        let mut forged = back.clone();
        forged.preparation.payload = encode(r#"{"name":"Example Token","symbol":"EXT","decimals":2}"#).unwrap();
        assert!(forged.verify().is_err(), "a supplier cannot change the declaration");
        let mut other = back.clone();
        other.covenant_id = [7; 32];
        assert!(other.verify().is_err(), "nor claim it for another covenant id");
        let mut fmt = back;
        fmt.format = "x".into();
        assert!(fmt.verify().is_err());
        assert!(
            serde_json::from_str::<LaunchProof>(&text.replace("\"gas\"", "\"extra\":1,\"gas\"")).is_err(),
            "unknown fields refused"
        );
    }
}
