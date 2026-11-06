//! Hex helpers and serde adapters for hashes and byte strings as the node's JSON wRPC spells them.

use serde::de::{self, Deserializer};
use serde::ser::Serializer;
use serde::{Deserialize, Serialize};
use std::fmt;

/// Error for malformed hex input.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("invalid hex: {0}")]
pub struct HexError(pub String);

pub fn encode(bytes: &[u8]) -> String {
    faster_hex::hex_string(bytes)
}

pub fn decode(s: &str) -> Result<Vec<u8>, HexError> {
    let s = s.strip_prefix("0x").unwrap_or(s);
    if !s.len().is_multiple_of(2) {
        return Err(HexError(format!("odd length {}", s.len())));
    }
    let mut out = vec![0u8; s.len() / 2];
    faster_hex::hex_decode(s.as_bytes(), &mut out).map_err(|e| HexError(e.to_string()))?;
    Ok(out)
}

/// A 32-byte hash (block hash, transaction id, covenant id, template hash).
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Default)]
pub struct Hash32(pub [u8; 32]);

impl Hash32 {
    pub const ZERO: Hash32 = Hash32([0u8; 32]);

    pub fn from_slice(b: &[u8]) -> Option<Hash32> {
        <[u8; 32]>::try_from(b).ok().map(Hash32)
    }

    pub fn parse(s: &str) -> Result<Hash32, HexError> {
        let v = decode(s)?;
        Hash32::from_slice(&v).ok_or_else(|| HexError(format!("expected 32 bytes, got {}", v.len())))
    }

    pub fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }

    pub fn to_hex(&self) -> String {
        encode(&self.0)
    }
}

impl fmt::Debug for Hash32 {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.to_hex())
    }
}

impl fmt::Display for Hash32 {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.to_hex())
    }
}

impl Serialize for Hash32 {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&self.to_hex())
    }
}

impl<'de> Deserialize<'de> for Hash32 {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let s = String::deserialize(d)?;
        Hash32::parse(&s).map_err(de::Error::custom)
    }
}

/// Arbitrary bytes, hex in JSON.
#[derive(Clone, PartialEq, Eq, Default)]
pub struct HexBytes(pub Vec<u8>);

impl fmt::Debug for HexBytes {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", encode(&self.0))
    }
}

impl Serialize for HexBytes {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&encode(&self.0))
    }
}

impl<'de> Deserialize<'de> for HexBytes {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let s = String::deserialize(d)?;
        decode(&s).map(HexBytes).map_err(de::Error::custom)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip() {
        let h = Hash32([7u8; 32]);
        let j = serde_json::to_string(&h).unwrap();
        let back: Hash32 = serde_json::from_str(&j).unwrap();
        assert_eq!(h, back);
        assert!(Hash32::parse("00").is_err());
        assert!(decode("abc").is_err());
        assert_eq!(decode("0aff").unwrap(), vec![0x0a, 0xff]);
    }
}
