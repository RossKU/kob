//! Tiny binary writer/reader used by the record log (varints and length-prefixed byte strings).
//!
//! The log is the permanent store, so its encoding is compact and hand-rolled rather than a
//! self-describing format: unsigned integers are LEB128 varints, byte strings carry a varint length,
//! hashes are 32 raw bytes.

use crate::hex::Hash32;

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("malformed record: {0}")]
pub struct WireError(pub &'static str);

pub type WireResult<T> = Result<T, WireError>;

#[derive(Default)]
pub struct Writer {
    pub buf: Vec<u8>,
}

impl Writer {
    pub fn new() -> Self {
        Writer { buf: Vec::with_capacity(256) }
    }

    pub fn u8(&mut self, v: u8) {
        self.buf.push(v);
    }

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

    pub fn raw(&mut self, b: &[u8]) {
        self.buf.extend_from_slice(b);
    }

    pub fn bytes(&mut self, b: &[u8]) {
        self.var(b.len() as u64);
        self.buf.extend_from_slice(b);
    }

    pub fn hash(&mut self, h: &Hash32) {
        self.buf.extend_from_slice(&h.0);
    }

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

pub struct Reader<'a> {
    b: &'a [u8],
    p: usize,
}

impl<'a> Reader<'a> {
    pub fn new(b: &'a [u8]) -> Self {
        Reader { b, p: 0 }
    }

    pub fn done(&self) -> bool {
        self.p >= self.b.len()
    }

    pub fn u8(&mut self) -> WireResult<u8> {
        let v = *self.b.get(self.p).ok_or(WireError("truncated"))?;
        self.p += 1;
        Ok(v)
    }

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

    pub fn svar(&mut self) -> WireResult<i64> {
        let z = self.var()?;
        Ok(((z >> 1) as i64) ^ -((z & 1) as i64))
    }

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

    pub fn bytes(&mut self) -> WireResult<&'a [u8]> {
        let n = self.var()? as usize;
        self.raw(n)
    }

    pub fn hash(&mut self) -> WireResult<Hash32> {
        Hash32::from_slice(self.raw(32)?).ok_or(WireError("hash"))
    }

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
