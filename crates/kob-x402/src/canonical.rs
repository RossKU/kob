//! Canonical JSON and SHA-256 hashing of the binding (`kaspa-exact-v2.md`, "Canonical request
//! authorization"): object keys sorted in ascending UTF-16 code-unit order, arrays in order, compact
//! encoding, integers only (no floats), no whitespace.

use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::error::{Diag, Result, X402Error};

/// Deepest nesting the canonicalizer accepts (resource bound).
pub const MAX_DEPTH: usize = 32;

/// Canonical JSON text of a value.
pub fn canonical_json(v: &Value) -> Result<String> {
    let mut out = String::new();
    write(v, &mut out, 0)?;
    Ok(out)
}

fn write(v: &Value, out: &mut String, depth: usize) -> Result<()> {
    if depth > MAX_DEPTH {
        return Err(X402Error::payload(Diag::InvalidKaspaX402Payload, "JSON nesting exceeds the canonicalization bound"));
    }
    match v {
        Value::Null => out.push_str("null"),
        Value::Bool(b) => out.push_str(if *b { "true" } else { "false" }),
        Value::Number(n) => {
            if n.is_f64() {
                return Err(X402Error::payload(
                    Diag::InvalidKaspaX402Payload,
                    "floating point numbers are outside the canonical JSON profile",
                ));
            }
            out.push_str(&n.to_string());
        }
        Value::String(s) => out.push_str(&serde_json::to_string(s).expect("string")),
        Value::Array(a) => {
            out.push('[');
            for (i, x) in a.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                write(x, out, depth + 1)?;
            }
            out.push(']');
        }
        Value::Object(m) => {
            let mut keys: Vec<&String> = m.keys().collect();
            keys.sort_by(|a, b| a.encode_utf16().cmp(b.encode_utf16()));
            out.push('{');
            for (i, k) in keys.into_iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                out.push_str(&serde_json::to_string(k).expect("string"));
                out.push(':');
                write(&m[k], out, depth + 1)?;
            }
            out.push('}');
        }
    }
    Ok(())
}

/// SHA-256 of raw bytes.
pub fn sha256(bytes: &[u8]) -> [u8; 32] {
    Sha256::digest(bytes).into()
}

/// SHA-256 of the canonical JSON of a value.
pub fn canonical_hash(v: &Value) -> Result<[u8; 32]> {
    Ok(sha256(canonical_json(v)?.as_bytes()))
}

/// `paymentRequirementsHash`: SHA-256 of the canonical JSON of the complete selected requirements.
pub fn requirements_hash(req: &crate::wire::PaymentRequirements) -> Result<[u8; 32]> {
    let v = serde_json::to_value(req).map_err(|e| X402Error::payload(Diag::InvalidKaspaX402Payload, e.to_string()))?;
    canonical_hash(&v)
}

/// Default normalized HTTP request fingerprint of the reference SDK:
/// `sha256(canonical({method, url, body|null, paymentRequirementsHash}))`. A resource server may use
/// any 32-byte fingerprint; the facilitator only requires it to be independently computed.
pub fn http_request_hash(method: &str, url: &str, body: Option<&Value>, requirements_hash_hex: &str) -> Result<[u8; 32]> {
    let mut m = serde_json::Map::new();
    m.insert("method".into(), Value::String(method.to_string()));
    m.insert("url".into(), Value::String(url.to_string()));
    m.insert("body".into(), body.cloned().unwrap_or(Value::Null));
    m.insert("paymentRequirementsHash".into(), Value::String(requirements_hash_hex.to_string()));
    canonical_hash(&Value::Object(m))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn sorted_compact_integers_only() {
        let v = json!({"b": [1, {"z": null, "a": true}], "a": "x\"y", "é": 1});
        assert_eq!(canonical_json(&v).unwrap(), r#"{"a":"x\"y","b":[1,{"a":true,"z":null}],"é":1}"#);
        assert!(canonical_json(&json!({"f": 1.5})).is_err());
    }

    #[test]
    fn utf16_order_differs_from_utf8_order() {
        // U+FF5E (BMP, UTF-16 0xFF5E) sorts after U+1F600 (surrogates 0xD83D..) in UTF-16 but before it in UTF-8.
        let v = json!({"\u{ff5e}": 1, "\u{1f600}": 2});
        assert_eq!(canonical_json(&v).unwrap(), "{\"\u{1f600}\":2,\"\u{ff5e}\":1}");
    }
}
