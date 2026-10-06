//! JSON conventions shared by every request, result and golden vector.
//!
//! * Byte strings (hashes, public keys, scripts, state spans) are lower-case hex strings.
//! * 64-bit integers are emitted as decimal strings so JavaScript never loses precision; on
//!   input both strings and JSON numbers are accepted.
//! * Small integers (`u8`, `u16`, `u32`, indices) are plain JSON numbers.

use serde::de::{self, Deserializer};
use serde::ser::Serializer;
use serde::{Deserialize, Serialize};

/// Lower-case hex encoding.
pub fn to_hex(bytes: &[u8]) -> String {
    let mut out = vec![0u8; bytes.len() * 2];
    faster_hex::hex_encode(bytes, &mut out).expect("hex buffer size");
    String::from_utf8(out).expect("hex is ascii")
}

/// Hex decoding (either case, no `0x` prefix).
pub fn from_hex(s: &str) -> Result<Vec<u8>, String> {
    let s = s.strip_prefix("0x").unwrap_or(s);
    if !s.len().is_multiple_of(2) {
        return Err(format!("odd-length hex string ({} chars)", s.len()));
    }
    let mut out = vec![0u8; s.len() / 2];
    faster_hex::hex_decode(s.as_bytes(), &mut out).map_err(|e| format!("invalid hex: {e}"))?;
    Ok(out)
}

/// Decodes exactly 32 bytes of hex.
pub fn hex32(s: &str) -> Result<[u8; 32], String> {
    let v = from_hex(s)?;
    v.as_slice().try_into().map_err(|_| format!("expected 32 bytes, got {}", v.len()))
}

/// A value with a canonical JSON form (see the module docs).
pub trait JsonField: Sized {
    fn to_json(&self) -> serde_json::Value;
    fn from_json(v: &serde_json::Value) -> Result<Self, String>;
}

impl JsonField for i64 {
    fn to_json(&self) -> serde_json::Value {
        serde_json::Value::String(self.to_string())
    }
    fn from_json(v: &serde_json::Value) -> Result<Self, String> {
        match v {
            serde_json::Value::String(s) => s.parse().map_err(|e| format!("invalid integer {s:?}: {e}")),
            serde_json::Value::Number(n) => n.as_i64().ok_or_else(|| format!("integer out of range: {n}")),
            other => Err(format!("expected an integer, got {other}")),
        }
    }
}

impl JsonField for u64 {
    fn to_json(&self) -> serde_json::Value {
        serde_json::Value::String(self.to_string())
    }
    fn from_json(v: &serde_json::Value) -> Result<Self, String> {
        match v {
            serde_json::Value::String(s) => s.parse().map_err(|e| format!("invalid unsigned integer {s:?}: {e}")),
            serde_json::Value::Number(n) => n.as_u64().ok_or_else(|| format!("unsigned integer out of range: {n}")),
            other => Err(format!("expected an unsigned integer, got {other}")),
        }
    }
}

impl JsonField for u8 {
    fn to_json(&self) -> serde_json::Value {
        serde_json::Value::from(*self)
    }
    fn from_json(v: &serde_json::Value) -> Result<Self, String> {
        let n = match v {
            serde_json::Value::String(s) => s.parse::<u64>().map_err(|e| format!("invalid byte {s:?}: {e}"))?,
            serde_json::Value::Number(n) => n.as_u64().ok_or_else(|| format!("invalid byte {n}"))?,
            other => return Err(format!("expected a byte, got {other}")),
        };
        u8::try_from(n).map_err(|_| format!("byte out of range: {n}"))
    }
}

impl JsonField for [u8; 32] {
    fn to_json(&self) -> serde_json::Value {
        serde_json::Value::String(to_hex(self))
    }
    fn from_json(v: &serde_json::Value) -> Result<Self, String> {
        v.as_str().ok_or_else(|| format!("expected a 32-byte hex string, got {v}")).and_then(hex32)
    }
}

impl JsonField for Vec<u8> {
    fn to_json(&self) -> serde_json::Value {
        serde_json::Value::String(to_hex(self))
    }
    fn from_json(v: &serde_json::Value) -> Result<Self, String> {
        v.as_str().ok_or_else(|| format!("expected a hex string, got {v}")).and_then(from_hex)
    }
}

impl<T: JsonField> JsonField for Option<T> {
    fn to_json(&self) -> serde_json::Value {
        match self {
            Some(v) => v.to_json(),
            None => serde_json::Value::Null,
        }
    }
    fn from_json(v: &serde_json::Value) -> Result<Self, String> {
        if v.is_null() {
            Ok(None)
        } else {
            T::from_json(v).map(Some)
        }
    }
}

/// `#[serde(with = "crate::json::field")]` for any [`JsonField`].
pub mod field {
    use super::*;

    pub fn serialize<S: Serializer, T: JsonField>(v: &T, s: S) -> Result<S::Ok, S::Error> {
        v.to_json().serialize(s)
    }

    pub fn deserialize<'de, D: Deserializer<'de>, T: JsonField>(d: D) -> Result<T, D::Error> {
        let v = serde_json::Value::deserialize(d)?;
        T::from_json(&v).map_err(de::Error::custom)
    }
}

/// `#[serde(with = "crate::json::field_vec")]` for a `Vec` of [`JsonField`] values.
pub mod field_vec {
    use super::*;

    pub fn serialize<S: Serializer, T: JsonField>(v: &[T], s: S) -> Result<S::Ok, S::Error> {
        serde_json::Value::Array(v.iter().map(JsonField::to_json).collect()).serialize(s)
    }

    pub fn deserialize<'de, D: Deserializer<'de>, T: JsonField>(d: D) -> Result<Vec<T>, D::Error> {
        let v = Vec::<serde_json::Value>::deserialize(d)?;
        v.iter().map(|x| T::from_json(x).map_err(de::Error::custom)).collect()
    }
}
