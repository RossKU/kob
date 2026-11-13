//! A small binary writer and reader for the record log.
//!
//! The log is the permanent store, so its encoding is compact and hand-rolled rather than a
//! self-describing format. The encoding is:
//!
//! - unsigned integers are LEB128 varints,
//! - signed integers are zigzag varints,
//! - byte strings carry a varint length,
//! - hashes are 32 raw bytes.

use crate::hex::Hash32;

/// A malformed or truncated record. The message names what was wrong.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("malformed record: {0}")]
pub struct WireError(pub &'static str);

/// A result with a [`WireError`].
pub type WireResult<T> = Result<T, WireError>;

/// Appends values to a byte buffer in the record encoding.
#[derive(Default)]
pub struct Writer {
    /// The bytes written so far.
    pub buf: Vec<u8>,
}

impl Writer {
    /// An empty writer.
    pub fn new() -> Self {
        Writer { buf: Vec::with_capacity(256) }
    }

    /// One raw byte.
    pub fn u8(&mut self, v: u8) {
        self.buf.push(v);
    }

    /// An unsigned integer as a LEB128 varint.
    pub fn var(&mut self, mut v: u64) {
        while v >= 0x80 {
            self.buf.push((v as u8 & 0x7f) | 0x80);
            v >>= 7;
        }
        self.buf.push(v as u8);
    }

    /// Signed integer as zigzag varint.
    pub fn svar(&mut self, v: i64) {
        self.var(((v << 1) ^ (v >> 63)) as u64);
    }

    /// The bytes as they are, with no length.
    pub fn raw(&mut self, b: &[u8]) {
        self.buf.extend_from_slice(b);
    }

    /// A byte string: a varint length, then the bytes.
    pub fn bytes(&mut self, b: &[u8]) {
        self.var(b.len() as u64);
        self.buf.extend_from_slice(b);
    }

    /// A hash as 32 raw bytes.
    pub fn hash(&mut self, h: &Hash32) {
        self.buf.extend_from_slice(&h.0);
    }

    /// An optional hash: a tag byte (0 absent, 1 present), then the hash when present.
    pub fn opt_hash(&mut self, h: &Option<Hash32>) {
        match h {
            Some(h) => {
                self.u8(1);
                self.hash(h);
            }
            None => self.u8(0),
        }
    }
}

/// Reads values from a byte slice in the record encoding.
pub struct Reader<'a> {
    b: &'a [u8],
    p: usize,
}

impl<'a> Reader<'a> {
    /// A reader at the start of `b`.
    pub fn new(b: &'a [u8]) -> Self {
        Reader { b, p: 0 }
    }

    /// True when every byte has been read.
    pub fn done(&self) -> bool {
        self.p >= self.b.len()
    }

    /// One raw byte.
    pub fn u8(&mut self) -> WireResult<u8> {
        let v = *self.b.get(self.p).ok_or(WireError("truncated"))?;
        self.p += 1;
        Ok(v)
    }

    /// An unsigned LEB128 varint. A value that does not fit in 64 bits is an error.
    pub fn var(&mut self) -> WireResult<u64> {
        let mut v = 0u64;
        let mut shift = 0u32;
        loop {
            let b = self.u8()?;
            if shift >= 64 || (shift == 63 && b > 1) {
                return Err(WireError("varint overflow"));
            }
            v |= ((b & 0x7f) as u64) << shift;
            if b & 0x80 == 0 {
                return Ok(v);
            }
            shift += 7;
        }
    }

    /// A zigzag varint as a signed integer.
    pub fn svar(&mut self) -> WireResult<i64> {
        let z = self.var()?;
        Ok(((z >> 1) as i64) ^ -((z & 1) as i64))
    }

    /// The next `n` bytes.
    pub fn raw(&mut self, n: usize) -> WireResult<&'a [u8]> {
        let end = self.p.checked_add(n).ok_or(WireError("length overflow"))?;
        let s = self.b.get(self.p..end).ok_or(WireError("truncated"))?;
        self.p = end;
        Ok(s)
    }

    /// The `n` bytes `off` bytes ahead of the position, without consuming anything.
    pub fn peek(&self, off: usize, n: usize) -> Option<&'a [u8]> {
        let start = self.p.checked_add(off)?;
        self.b.get(start..start.checked_add(n)?)
    }

    /// A byte string: a varint length, then the bytes.
    pub fn bytes(&mut self) -> WireResult<&'a [u8]> {
        let n = self.var()? as usize;
        self.raw(n)
    }

    /// A hash of 32 raw bytes.
    pub fn hash(&mut self) -> WireResult<Hash32> {
        Hash32::from_slice(self.raw(32)?).ok_or(WireError("hash"))
    }

    /// An optional hash. Any tag other than 0 or 1 is an error.
    pub fn opt_hash(&mut self) -> WireResult<Option<Hash32>> {
        match self.u8()? {
            0 => Ok(None),
            1 => Ok(Some(self.hash()?)),
            _ => Err(WireError("option tag")),
        }
    }

    /// A count that must be plausible for the remaining input (each item takes at least `min_item` bytes).
    pub fn count(&mut self, min_item: usize) -> WireResult<usize> {
        let n = self.var()? as usize;
        if n.checked_mul(min_item.max(1)).is_none_or(|t| t > self.b.len() - self.p) {
            return Err(WireError("count exceeds input"));
        }
        Ok(n)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn varints_and_strings_roundtrip() {
        let mut w = Writer::new();
        for v in [0u64, 1, 127, 128, 300, u32::MAX as u64, u64::MAX] {
            w.var(v);
        }
        for v in [0i64, -1, 1, i64::MIN, i64::MAX, -12345] {
            w.svar(v);
        }
        w.bytes(b"hello");
        w.hash(&Hash32([9; 32]));
        w.opt_hash(&None);
        let mut r = Reader::new(&w.buf);
        for v in [0u64, 1, 127, 128, 300, u32::MAX as u64, u64::MAX] {
            assert_eq!(r.var().unwrap(), v);
        }
        for v in [0i64, -1, 1, i64::MIN, i64::MAX, -12345] {
            assert_eq!(r.svar().unwrap(), v);
        }
        assert_eq!(r.bytes().unwrap(), b"hello");
        assert_eq!(r.hash().unwrap(), Hash32([9; 32]));
        assert_eq!(r.opt_hash().unwrap(), None);
        assert!(r.done());
        assert!(Reader::new(&[0x80]).var().is_err());
        assert!(Reader::new(&[5, 1]).bytes().is_err());
    }
}
