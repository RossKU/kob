//! The payload of a token genesis transaction: the states of its token outputs.
//!
//! A token output is a P2SH output, so its state (amount, owner, ...) is not readable from the chain: it is revealed by the
//! transaction that creates it (a transfer's leader lists the `next_states`; [`crate::public_mint`] names the holder it creates).
//! A genesis has no such leader, so until a genesis output was first spent nothing but the issuing wallet knew its state, and a
//! wallet of the same key in another browser, or the CLI, could not find (list, sell) the issued tokens. A KOB genesis therefore
//! carries its states in its payload:
//!
//! ```text
//! "KOBG" | version (1) | count (1..=255) | count x ( length (1..=255) | state span )
//! ```
//!
//! The state spans are the exact bytes a token program commits to (the 112-byte KCC-20 state, the 46-byte KRON state), in the
//! order of the genesis's token outputs. The payload proves nothing by itself: a reader keeps a state only when the P2SH it
//! produces under a pinned token program equals the output's script public key, exactly as it does for a transfer's states, so a
//! wrong or hostile payload can only fail to be believed. The covenant id does not commit to the payload (it commits to the
//! authorising outpoint and the output group), so the payload cannot change the token.

/// The payload's four-byte tag (not the `KOB1` of a placement record: the two never meet in one transaction).
pub const MAGIC: &[u8; 4] = b"KOBG";
/// The payload version.
pub const VERSION: u8 = 1;

/// The payload for the given state spans, in output order; `None` when there are none, more than 255, or one is empty or longer
/// than 255 bytes.
pub fn encode(spans: &[Vec<u8>]) -> Option<Vec<u8>> {
    if spans.is_empty() || spans.len() > 255 || spans.iter().any(|s| s.is_empty() || s.len() > 255) {
        return None;
    }
    let mut v = MAGIC.to_vec();
    v.push(VERSION);
    v.push(spans.len() as u8);
    for s in spans {
        v.push(s.len() as u8);
        v.extend_from_slice(s);
    }
    Some(v)
}

/// Whether `payload` claims to be a genesis-states payload (starts with the tag): a cheap test before [`decode`].
pub fn is_genesis_states(payload: &[u8]) -> bool {
    payload.starts_with(MAGIC)
}

/// The state spans of a genesis-states payload; `None` when it is not one, has another version, or is not exactly the encoding
/// (a trailing byte, a short span, a zero count are all refused).
pub fn decode(payload: &[u8]) -> Option<Vec<Vec<u8>>> {
    let rest = payload.strip_prefix(MAGIC)?;
    let (&version, rest) = rest.split_first()?;
    let (&count, mut rest) = rest.split_first()?;
    if version != VERSION || count == 0 {
        return None;
    }
    let mut out = Vec::with_capacity(count as usize);
    for _ in 0..count {
        let (&len, tail) = rest.split_first()?;
        if len == 0 || tail.len() < len as usize {
            return None;
        }
        let (span, tail) = tail.split_at(len as usize);
        out.push(span.to_vec());
        rest = tail;
    }
    rest.is_empty().then_some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_and_strictness() {
        let spans = vec![vec![1u8; 112], vec![2u8; 46]];
        let p = encode(&spans).unwrap();
        assert_eq!(&p[..6], b"KOBG\x01\x02");
        assert_eq!(p.len(), 6 + 1 + 112 + 1 + 46);
        assert!(is_genesis_states(&p));
        assert_eq!(decode(&p), Some(spans));
        // exact encoding only
        let mut longer = p.clone();
        longer.push(0);
        assert_eq!(decode(&longer), None);
        assert_eq!(decode(&p[..p.len() - 1]), None);
        let mut v2 = p.clone();
        v2[4] = 2;
        assert_eq!(decode(&v2), None);
        let mut none = p.clone();
        none[5] = 0;
        assert_eq!(decode(&none), None);
        assert_eq!(decode(b""), None);
        assert_eq!(decode(b"KOB1\x01\x01\x01\x00"), None);
        // nothing to encode, or a span that does not fit
        assert_eq!(encode(&[]), None);
        assert_eq!(encode(&[vec![]]), None);
        assert_eq!(encode(&[vec![0; 256]]), None);
    }
}
