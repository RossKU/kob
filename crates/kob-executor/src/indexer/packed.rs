//! Packed storage of the state spans the store keeps once per output.
//!
//! An order covenant's UTXOs (`order_utxos.state`) are the same state span a few bytes apart (a fill changes `amountLeft`; most
//! orders are placed, then cancelled, and their one UTXO carries `orders.genesis_state` byte for byte), and a token holding's
//! state (`token_holdings.state`) is the 112-byte (KCC-20) or 46-byte (KRON) layout of columns the row already has (`amount`,
//! `owner`, `owner_kind`) plus zero fields. Stored whole they were 40 % of the store (about 1.2 GB of 8.5 GB on the TN10 soak
//! after two weeks).
//!
//! A stored value is one of
//!
//! * the state span itself, when its first byte is not `0x00` (every state span starts with a push opcode: `0x20` or `0x08`).
//!   A store written before this encoding holds only these, and a reader needs no migration to read them;
//! * `0x00 0x00` + the span (a span that is empty or starts with `0x00`, which must not read as a tag);
//! * `0x00 0x01` + varint `p` + varint `s` + the middle bytes: the span is the first `p` bytes of the BASE, the middle, and the
//!   last `s` bytes of the base. The base of an order UTXO is the order's `genesis_state`; the base of a holding is the span its
//!   own columns spell with zero in every field they do not hold ([`holding_base`]).
//!
//! The decoded value is exactly the span that was stored: [`pack`] checks that before it returns a delta, and falls back to the
//! span itself. The SQL functions registered on every connection ([`register`]) are how queries read and write the packed form:
//!
//! * `kob_pack(state, base)` / `kob_ustate(stored, base)`: an order UTXO's state (`base` = the order's `genesis_state`);
//! * `kob_hpack(state, family, owner, owner_kind, amount)` / `kob_hstate(stored, family, owner, owner_kind, amount)`: a holding's.
//!
//! All four pass NULL through (an unknown state stays unknown).
//!
//! The wide payloads of the history are stored against frozen dictionaries ([`packed_dicts`](super::packed_dicts)):
//!
//! * `orders.genesis_state` (about 380 bytes, 56 % of an order row): `0x00 0x02` + dictionary id + CRC-32 of the span + the
//!   raw deflate of the span with the dictionary of its contract (a representative state of it) followed by the row's own maker,
//!   token, token program and quote token (the bytes its state repeats): about 60 bytes.
//!   `kob_gpack(span, contract, maker, token, tpl, quote)` / `kob_gstate(stored, maker, token, tpl, quote)`;
//! * `order_events.detail` (JSON text, 110 bytes): the same form with the dictionary of the event kind, kept as the text itself
//!   when that is no longer. `kob_dpack(text, kind)` / `kob_dtext(stored)`.
//!
//! The columns the dictionary is made of never change after the row is written, and the CRC says so if one ever did. A dictionary is
//! named by its id inside the value, so reading needs no other row; the ids are append-only ([`super::packed_dicts`]).

use super::packed_dicts::DICTIONARIES;
use rusqlite::functions::{Context, FunctionFlags};
use rusqlite::types::{Value, ValueRef};
use rusqlite::Connection;
use std::borrow::Cow;
use std::sync::OnceLock;

/// First byte of a stored value that is not the span itself.
const TAG: u8 = 0x00;
const MODE_SPAN: u8 = 0x00;
const MODE_DELTA: u8 = 0x01;
const MODE_DICT: u8 = 0x02;
/// The largest value a dictionary-packed value may inflate to (a damaged length cannot make a read allocate without bound).
const MAX_INFLATED: usize = 1 << 20;

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum PackError {
    #[error("packed state: {0}")]
    Corrupt(&'static str),
    #[error("packed state: a delta needs its base, and the row has none")]
    NoBase,
}

fn put_var(out: &mut Vec<u8>, mut v: usize) {
    while v >= 0x80 {
        out.push((v as u8 & 0x7f) | 0x80);
        v >>= 7;
    }
    out.push(v as u8);
}

fn get_var(b: &[u8], at: &mut usize) -> Result<usize, PackError> {
    let mut v = 0usize;
    for shift in (0..35).step_by(7) {
        let byte = *b.get(*at).ok_or(PackError::Corrupt("truncated length"))?;
        *at += 1;
        v |= ((byte & 0x7f) as usize) << shift;
        if byte & 0x80 == 0 {
            return Ok(v);
        }
    }
    Err(PackError::Corrupt("length too long"))
}

/// The stored form of `state` given the BASE of its row (`None`: the row has none).
pub fn pack(state: &[u8], base: Option<&[u8]>) -> Vec<u8> {
    let whole = || -> Vec<u8> {
        if state.first().is_none_or(|b| *b == TAG) {
            let mut v = Vec::with_capacity(state.len() + 2);
            v.extend_from_slice(&[TAG, MODE_SPAN]);
            v.extend_from_slice(state);
            v
        } else {
            state.to_vec()
        }
    };
    let Some(base) = base else { return whole() };
    let p = state.iter().zip(base).take_while(|(a, b)| a == b).count();
    let s = state[p..].iter().rev().zip(base[p..].iter().rev()).take_while(|(a, b)| a == b).count();
    let middle = &state[p..state.len() - s];
    let mut delta = Vec::with_capacity(middle.len() + 8);
    delta.extend_from_slice(&[TAG, MODE_DELTA]);
    put_var(&mut delta, p);
    put_var(&mut delta, s);
    delta.extend_from_slice(middle);
    // a delta is kept only when it is smaller than the span and gives the span back
    if delta.len() < state.len() && unpack(&delta, Some(base)).is_ok_and(|d| d == state) {
        delta
    } else {
        whole()
    }
}

/// The state span a stored value holds.
pub fn unpack<'a>(stored: &'a [u8], base: Option<&[u8]>) -> Result<Cow<'a, [u8]>, PackError> {
    match stored.first() {
        None => Ok(Cow::Borrowed(stored)),
        Some(b) if *b != TAG => Ok(Cow::Borrowed(stored)),
        Some(_) => match stored.get(1) {
            Some(&MODE_SPAN) => Ok(Cow::Borrowed(&stored[2..])),
            Some(&MODE_DELTA) => {
                let base = base.ok_or(PackError::NoBase)?;
                let mut at = 2;
                let p = get_var(stored, &mut at)?;
                let s = get_var(stored, &mut at)?;
                if p.checked_add(s).is_none_or(|n| n > base.len()) {
                    return Err(PackError::Corrupt("delta longer than its base"));
                }
                let middle = &stored[at..];
                let mut out = Vec::with_capacity(p + middle.len() + s);
                out.extend_from_slice(&base[..p]);
                out.extend_from_slice(middle);
                out.extend_from_slice(&base[base.len() - s..]);
                Ok(Cow::Owned(out))
            }
            _ => Err(PackError::Corrupt("unknown mode")),
        },
    }
}

/// The span a holding's columns spell: family 1 (KCC-20) `0x08 amount | 0x20 owner | 0x01 owner_scheme | 0x01 borrow_scheme
/// | 0x20 borrow_guard | 0x20 extension_commitment` with the last three zero, family 2 (KRON)
/// `0x20 owner | 0x01 id_type | 0x08 amount | 0x01 is_minter` with `is_minter` zero (`kob_protocol::state`). `None` for any
/// other family, an owner that is not 32 bytes or an amount that has no such encoding. Only a base: a holding whose state differs from it keeps the difference.
pub fn holding_base(family: i64, owner: &[u8], owner_kind: i64, amount: i64) -> Option<Vec<u8>> {
    let kind = u8::try_from(owner_kind).ok()?;
    if owner.len() != 32 {
        return None;
    }
    let mut v = Vec::with_capacity(112);
    match family {
        1 => {
            // a KCC-20 amount is a script number: sign and magnitude
            let mut n = amount.checked_abs()?.to_le_bytes();
            if amount < 0 {
                n[7] |= 0x80;
            }
            v.push(0x08);
            v.extend_from_slice(&n);
            v.push(0x20);
            v.extend_from_slice(owner);
            v.extend_from_slice(&[0x01, kind, 0x01, 0x00, 0x20]);
            v.extend_from_slice(&[0u8; 32]);
            v.push(0x20);
            v.extend_from_slice(&[0u8; 32]);
        }
        2 => {
            v.push(0x20);
            v.extend_from_slice(owner);
            v.extend_from_slice(&[0x01, kind, 0x08]);
            v.extend_from_slice(&amount.to_le_bytes());
            v.extend_from_slice(&[0x01, 0x00]);
        }
        _ => return None,
    }
    Some(v)
}

/// One frozen dictionary ([`super::packed_dicts::DICTIONARIES`]).
pub struct Dict {
    pub id: u8,
    pub kind: &'static str,
    pub name: &'static str,
    pub bytes: Vec<u8>,
}

fn dicts() -> &'static [Dict] {
    static D: OnceLock<Vec<Dict>> = OnceLock::new();
    D.get_or_init(|| {
        DICTIONARIES
            .iter()
            .map(|(id, kind, name, hex)| Dict {
                id: *id,
                kind,
                name,
                bytes: crate::hex::decode(hex).expect("a frozen dictionary is hex"),
            })
            .collect()
    })
}

/// The newest dictionary for `name` (a contract for `genesis`, an event kind for `detail`), if there is one.
pub fn dict_for(kind: &str, name: &str) -> Option<&'static Dict> {
    dicts().iter().rev().find(|d| d.kind == kind && d.name == name)
}

fn dict_by_id(id: u8) -> Option<&'static Dict> {
    dicts().iter().find(|d| d.id == id)
}

fn crc32(data: &[u8]) -> u32 {
    let mut c = flate2::Crc::new();
    c.update(data);
    c.sum()
}

fn deflate_with(dict: &[u8], data: &[u8]) -> Option<Vec<u8>> {
    let mut c = flate2::Compress::new(flate2::Compression::best(), false);
    c.set_dictionary(dict).ok()?;
    let mut out = Vec::with_capacity(data.len() / 2 + 64);
    for _ in 0..64 {
        let at = c.total_in() as usize;
        match c.compress_vec(&data[at..], &mut out, flate2::FlushCompress::Finish).ok()? {
            flate2::Status::StreamEnd => return Some(out),
            _ => out.reserve(out.capacity().max(256)),
        }
    }
    None
}

fn inflate_with(dict: &[u8], data: &[u8], len_hint: usize) -> Result<Vec<u8>, PackError> {
    let mut d = flate2::Decompress::new(false);
    d.set_dictionary(dict).map_err(|_| PackError::Corrupt("dictionary"))?;
    let mut out = Vec::with_capacity(len_hint.clamp(64, MAX_INFLATED));
    for _ in 0..64 {
        let at = d.total_in() as usize;
        match d.decompress_vec(&data[at..], &mut out, flate2::FlushDecompress::Finish).map_err(|_| PackError::Corrupt("inflate"))? {
            flate2::Status::StreamEnd => return Ok(out),
            _ if out.len() >= MAX_INFLATED => return Err(PackError::Corrupt("inflated value too long")),
            _ if out.len() == out.capacity() => out.reserve(out.capacity()),
            _ => return Err(PackError::Corrupt("truncated deflate stream")),
        }
    }
    Err(PackError::Corrupt("inflate does not end"))
}

fn dict_bytes(d: &[u8], extras: &[&[u8]]) -> Vec<u8> {
    let mut v = Vec::with_capacity(d.len() + extras.iter().map(|e| e.len()).sum::<usize>());
    v.extend_from_slice(d);
    for e in extras {
        v.extend_from_slice(e);
    }
    v
}

/// `0x00 0x02 id crc32 deflate` of `span` against `dict` followed by `extras`; `None` when it is not shorter than the span (or does not read
/// back as the span).
pub fn zpack_checked(span: &[u8], dict: &Dict, extras: &[&[u8]]) -> Option<Vec<u8>> {
    let z = deflate_with(&dict_bytes(&dict.bytes, extras), span)?;
    let mut stored = Vec::with_capacity(z.len() + 7);
    stored.extend_from_slice(&[TAG, MODE_DICT, dict.id]);
    stored.extend_from_slice(&crc32(span).to_le_bytes());
    stored.extend_from_slice(&z);
    (stored.len() < span.len() && zunpack(&stored, extras).is_ok_and(|d| d == span)).then_some(stored)
}

/// A genesis state in its stored form: dictionary-packed when its contract has a dictionary and that is shorter, else as [`pack`] stores
/// a span without a base.
pub fn zpack(span: &[u8], dict: Option<&Dict>, extras: &[&[u8]]) -> Vec<u8> {
    dict.and_then(|d| zpack_checked(span, d, extras)).unwrap_or_else(|| pack(span, None))
}

/// The value a dictionary-packed (or whole) stored value holds. A stored value that is none of the forms a build writes (it starts
/// with the tag byte but names no known mode: foreign data, a test fixture) reads as itself.
pub fn zunpack<'a>(stored: &'a [u8], extras: &[&[u8]]) -> Result<Cow<'a, [u8]>, PackError> {
    match (stored.first(), stored.get(1)) {
        (Some(&TAG), Some(&MODE_SPAN)) => Ok(Cow::Borrowed(&stored[2..])),
        (Some(&TAG), Some(&MODE_DICT)) => {
            let id = *stored.get(2).ok_or(PackError::Corrupt("truncated header"))?;
            let crc: [u8; 4] = stored.get(3..7).and_then(|b| b.try_into().ok()).ok_or(PackError::Corrupt("truncated header"))?;
            let dict = dict_by_id(id).ok_or(PackError::Corrupt("unknown dictionary"))?;
            let out = inflate_with(&dict_bytes(&dict.bytes, extras), &stored[7..], 512)?;
            if crc32(&out) != u32::from_le_bytes(crc) {
                return Err(PackError::Corrupt("checksum (a column the dictionary is made of changed?)"));
            }
            Ok(Cow::Owned(out))
        }
        _ => Ok(Cow::Borrowed(stored)),
    }
}

/// The SQL of a genesis state and of an order UTXO's state, for a query that has the `orders` row as alias `$o` (and the UTXO as `$u`).
macro_rules! sql_genesis {
    ($o:literal) => {
        concat!(
            "kob_gstate(",
            $o,
            ".genesis_state, ",
            $o,
            ".maker, ",
            $o,
            ".token_cov_id, ",
            $o,
            ".token_tpl_hash, ",
            $o,
            ".quote_cov_id)"
        )
    };
}
macro_rules! sql_ustate {
    ($u:literal, $o:literal) => {
        concat!("kob_ustate(", $u, ".state, ", $crate::indexer::packed::sql_genesis!($o), ")")
    };
}
pub(crate) use {sql_genesis, sql_ustate};

/// [`sql_genesis`] for the aliases most queries use, as constants for `format!`.
pub const GENESIS_O: &str = sql_genesis!("o");
pub const GENESIS_G: &str = sql_genesis!("g");

fn text_arg<'a>(ctx: &'a Context<'_>, i: usize) -> rusqlite::Result<Option<&'a str>> {
    match ctx.get_raw(i) {
        ValueRef::Null => Ok(None),
        ValueRef::Text(b) => Ok(std::str::from_utf8(b).ok()),
        other => Err(rusqlite::Error::InvalidFunctionParameterType(i, other.data_type())),
    }
}

fn user_err(e: PackError) -> rusqlite::Error {
    rusqlite::Error::UserFunctionError(Box::new(e))
}

/// A blob (or text) argument; NULL is `None`. Any other type is an error, never a silent NULL.
fn blob_arg<'a>(ctx: &'a Context<'_>, i: usize) -> rusqlite::Result<Option<&'a [u8]>> {
    match ctx.get_raw(i) {
        ValueRef::Null => Ok(None),
        ValueRef::Blob(b) => Ok(Some(b)),
        ValueRef::Text(b) => Ok(Some(b)),
        other => Err(rusqlite::Error::InvalidFunctionParameterType(i, other.data_type())),
    }
}

fn int_arg(ctx: &Context<'_>, i: usize) -> rusqlite::Result<Option<i64>> {
    match ctx.get_raw(i) {
        ValueRef::Null => Ok(None),
        ValueRef::Integer(v) => Ok(Some(v)),
        other => Err(rusqlite::Error::InvalidFunctionParameterType(i, other.data_type())),
    }
}

fn blob_value(v: Vec<u8>) -> Value {
    Value::Blob(v)
}

/// The base of a holding row from the arguments `family, owner, owner_kind, amount` starting at argument `at`.
fn holding_base_args(ctx: &Context<'_>, at: usize) -> rusqlite::Result<Option<Vec<u8>>> {
    let (Some(f), Some(o), Some(k), Some(a)) =
        (int_arg(ctx, at)?, blob_arg(ctx, at + 1)?, int_arg(ctx, at + 2)?, int_arg(ctx, at + 3)?)
    else {
        return Ok(None);
    };
    Ok(holding_base(f, o, k, a))
}

/// Registers `kob_pack`, `kob_ustate`, `kob_hpack` and `kob_hstate` on a connection. Every connection that reads or writes the
/// store registers them ([`super::db`]); they are deterministic.
pub fn register(conn: &Connection) -> rusqlite::Result<()> {
    let flags = FunctionFlags::SQLITE_UTF8 | FunctionFlags::SQLITE_DETERMINISTIC;
    conn.create_scalar_function("kob_pack", 2, flags, |ctx| {
        let Some(state) = blob_arg(ctx, 0)? else { return Ok(Value::Null) };
        Ok(blob_value(pack(state, blob_arg(ctx, 1)?)))
    })?;
    conn.create_scalar_function("kob_ustate", 2, flags, |ctx| {
        let Some(stored) = blob_arg(ctx, 0)? else { return Ok(Value::Null) };
        Ok(blob_value(unpack(stored, blob_arg(ctx, 1)?).map_err(user_err)?.into_owned()))
    })?;
    conn.create_scalar_function("kob_hpack", 5, flags, |ctx| {
        let Some(state) = blob_arg(ctx, 0)? else { return Ok(Value::Null) };
        let base = holding_base_args(ctx, 1)?;
        Ok(blob_value(pack(state, base.as_deref())))
    })?;
    // `kob_gpack(span, contract, maker, token, tpl, quote)`, `kob_gstate(stored, maker, token, tpl, quote)`
    conn.create_scalar_function("kob_gpack", 6, flags, |ctx| {
        let Some(span) = blob_arg(ctx, 0)? else { return Ok(Value::Null) };
        let dict = text_arg(ctx, 1)?.and_then(|c| dict_for("genesis", c));
        let extras = [
            blob_arg(ctx, 2)?.unwrap_or(&[]),
            blob_arg(ctx, 3)?.unwrap_or(&[]),
            blob_arg(ctx, 4)?.unwrap_or(&[]),
            blob_arg(ctx, 5)?.unwrap_or(&[]),
        ];
        Ok(blob_value(zpack(span, dict, &extras)))
    })?;
    conn.create_scalar_function("kob_gstate", 5, flags, |ctx| {
        let Some(stored) = blob_arg(ctx, 0)? else { return Ok(Value::Null) };
        let extras = [
            blob_arg(ctx, 1)?.unwrap_or(&[]),
            blob_arg(ctx, 2)?.unwrap_or(&[]),
            blob_arg(ctx, 3)?.unwrap_or(&[]),
            blob_arg(ctx, 4)?.unwrap_or(&[]),
        ];
        Ok(blob_value(zunpack(stored, &extras).map_err(user_err)?.into_owned()))
    })?;
    // `kob_dpack(text, kind)`: the text itself unless a dictionary makes it shorter; `kob_dtext(stored)`: the text
    conn.create_scalar_function("kob_dpack", 2, flags, |ctx| {
        let Some(text) = blob_arg(ctx, 0)? else { return Ok(Value::Null) };
        let packed = text_arg(ctx, 1)?.and_then(|k| dict_for("detail", k)).and_then(|d| zpack_checked(text, d, &[]));
        Ok(match packed {
            Some(b) => Value::Blob(b),
            None => Value::Text(String::from_utf8_lossy(text).into_owned()),
        })
    })?;
    conn.create_scalar_function("kob_dtext", 1, flags, |ctx| match ctx.get_raw(0) {
        ValueRef::Null => Ok(Value::Null),
        ValueRef::Text(t) => Ok(Value::Text(String::from_utf8_lossy(t).into_owned())),
        ValueRef::Blob(b) => {
            let span = zunpack(b, &[]).map_err(user_err)?;
            Ok(Value::Text(String::from_utf8(span.into_owned()).map_err(|_| user_err(PackError::Corrupt("detail is not UTF-8")))?))
        }
        other => Err(rusqlite::Error::InvalidFunctionParameterType(0, other.data_type())),
    })?;
    conn.create_scalar_function("kob_hstate", 5, flags, |ctx| {
        let Some(stored) = blob_arg(ctx, 0)? else { return Ok(Value::Null) };
        // a stored span needs no base: skip building one
        if stored.first().is_some_and(|b| *b != TAG) {
            return Ok(blob_value(stored.to_vec()));
        }
        let base = holding_base_args(ctx, 1)?;
        Ok(blob_value(unpack(stored, base.as_deref()).map_err(user_err)?.into_owned()))
    })?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lcg(seed: &mut u64) -> u64 {
        *seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        *seed >> 33
    }

    #[test]
    fn spans_round_trip_with_and_without_a_base() {
        let mut seed = 7;
        for _ in 0..4000 {
            let n = (lcg(&mut seed) % 400) as usize;
            let base: Vec<u8> = (0..n).map(|_| lcg(&mut seed) as u8).collect();
            // a state near the base, a state unrelated to it, and the base itself
            let mut near = base.clone();
            for _ in 0..(lcg(&mut seed) % 4) {
                if !near.is_empty() {
                    let at = lcg(&mut seed) as usize % near.len();
                    near[at] = lcg(&mut seed) as u8;
                }
            }
            if lcg(&mut seed).is_multiple_of(3) {
                near.truncate(near.len().saturating_sub(lcg(&mut seed) as usize % 5));
            }
            let far: Vec<u8> = (0..(lcg(&mut seed) % 300) as usize).map(|_| lcg(&mut seed) as u8).collect();
            for s in [near, far, base.clone(), vec![], vec![0], vec![0, 1, 2]] {
                for b in [Some(&base[..]), None, Some(&[][..])] {
                    let stored = pack(&s, b);
                    assert_eq!(unpack(&stored, b).unwrap().as_ref(), &s[..], "state {s:?} base {b:?}");
                }
            }
        }
    }

    #[test]
    fn an_order_utxo_equal_to_its_genesis_state_is_three_bytes() {
        let base = vec![0x20u8; 341];
        assert_eq!(pack(&base, Some(&base)).len(), 5, "tag, mode and the two lengths");
        let mut fill = base.clone();
        fill[200..208].copy_from_slice(&5u64.to_le_bytes());
        let p = pack(&fill, Some(&base));
        assert!(p.len() <= 14, "{}", p.len());
        assert_eq!(unpack(&p, Some(&base)).unwrap().as_ref(), &fill[..]);
    }

    #[test]
    fn a_stored_span_reads_as_itself_and_a_delta_without_a_base_is_an_error() {
        let s = vec![0x20, 1, 2, 3];
        assert_eq!(unpack(&s, None).unwrap().as_ref(), &s[..]);
        assert_eq!(unpack(&[TAG, MODE_DELTA, 0, 0], None), Err(PackError::NoBase));
        assert!(unpack(&[TAG, 9], None).is_err());
        assert!(unpack(&[TAG], None).is_err());
        assert!(unpack(&[TAG, MODE_DELTA, 200, 1], Some(&[1, 2, 3])).is_err());
    }

    #[test]
    fn the_holding_base_is_the_span_the_protocol_encodes() {
        use kob_protocol::state::{Kcc20State, KronState, StateCodec};
        let owner = [7u8; 32];
        for (kind, amount) in [(0u8, 0i64), (4, 30_000_000_000), (3, i64::MAX), (0, -5)] {
            let k = Kcc20State {
                amount,
                owner,
                owner_scheme: kind,
                borrow_scheme: 0,
                borrow_guard: [0; 32],
                extension_commitment: [0; 32],
            };
            assert_eq!(holding_base(1, &owner, kind as i64, amount).unwrap(), k.encode(), "kcc20 {kind} {amount}");
            let r = KronState { owner, id_type: kind, amount, is_minter: 0 };
            assert_eq!(holding_base(2, &owner, kind as i64, amount).unwrap(), r.encode(), "kron {kind} {amount}");
        }
        assert!(holding_base(3, &owner, 0, 1).is_none());
        assert!(holding_base(1, &owner[..31], 0, 1).is_none());
        assert!(holding_base(1, &owner, 256, 1).is_none());
        // a holding with an extension commitment keeps only the difference
        let ext =
            Kcc20State { amount: 9, owner, owner_scheme: 4, borrow_scheme: 0, borrow_guard: [0; 32], extension_commitment: [9; 32] }
                .encode();
        let packed = pack(&ext, holding_base(1, &owner, 4, 9).as_deref());
        assert!(packed.len() < 45, "{}", packed.len());
        assert_eq!(unpack(&packed, holding_base(1, &owner, 4, 9).as_deref()).unwrap().as_ref(), &ext[..]);
    }

    #[test]
    fn the_sql_functions_pack_and_unpack_and_pass_null_through() {
        let conn = Connection::open_in_memory().unwrap();
        register(&conn).unwrap();
        let base = vec![0x20u8; 50];
        let mut st = base.clone();
        st[10] = 1;
        let one = |sql: &str, a: &[u8], b: &[u8]| -> Vec<u8> { conn.query_row(sql, rusqlite::params![a, b], |r| r.get(0)).unwrap() };
        let packed = one("SELECT kob_pack(?1, ?2)", &st, &base);
        assert!(packed.len() < st.len());
        assert_eq!(one("SELECT kob_ustate(?1, ?2)", &packed, &base), st);
        let none: Option<Vec<u8>> = conn.query_row("SELECT kob_ustate(NULL, ?1)", [&base[..]], |r| r.get(0)).unwrap();
        assert_eq!(none, None);
        let none: Option<Vec<u8>> = conn.query_row("SELECT kob_pack(NULL, NULL)", [], |r| r.get(0)).unwrap();
        assert_eq!(none, None);
        // a delta whose base is NULL is an error, not a wrong answer
        assert!(conn.query_row("SELECT kob_ustate(?1, NULL)", [&packed[..]], |r| r.get::<_, Vec<u8>>(0)).is_err());
        // holdings
        let owner = [3u8; 32];
        let span = holding_base(1, &owner, 0, 42).unwrap();
        let hp: Vec<u8> =
            conn.query_row("SELECT kob_hpack(?1, 1, ?2, 0, 42)", rusqlite::params![&span[..], &owner[..]], |r| r.get(0)).unwrap();
        assert_eq!(hp.len(), 4);
        let back: Vec<u8> =
            conn.query_row("SELECT kob_hstate(?1, 1, ?2, 0, 42)", rusqlite::params![&hp[..], &owner[..]], |r| r.get(0)).unwrap();
        assert_eq!(back, span);
    }
}

#[cfg(test)]
mod dict_tests {
    use super::*;
    use sha2::{Digest, Sha256};

    #[test]
    fn dictionaries_are_frozen() {
        // an entry is never edited or removed (a stored value names its dictionary by id): a change shows here
        let mut h = Sha256::new();
        for d in dicts() {
            for part in [&[d.id][..], d.kind.as_bytes(), d.name.as_bytes(), &d.bytes] {
                h.update((part.len() as u32).to_le_bytes());
                h.update(part);
            }
        }
        let got: String = h.finalize().iter().map(|b| format!("{b:02x}")).collect();
        assert_eq!(got, super::super::packed_dicts::FROZEN_SHA256);
        // ids are unique, and a contract or kind has one newest dictionary
        let mut ids: Vec<u8> = dicts().iter().map(|d| d.id).collect();
        ids.sort_unstable();
        ids.dedup();
        assert_eq!(ids.len(), dicts().len());
        assert!(
            dict_for("genesis", "KobAsk").is_some()
                && dict_for("detail", "fill").is_some()
                && dict_for("genesis", "NoSuchContract").is_none()
        );
    }

    fn lcg(seed: &mut u64) -> u64 {
        *seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        *seed >> 33
    }

    /// A genesis-state-like span: the dictionary of the contract with a few fields changed and the row's own hashes written into it.
    fn state_like(d: &Dict, extras: &[Vec<u8>], seed: &mut u64) -> Vec<u8> {
        let mut s = d.bytes.clone();
        for e in extras {
            if e.len() == 32 && s.len() > 70 {
                let at = (lcg(seed) as usize) % (s.len() - 33);
                s[at..at + 32].copy_from_slice(e);
            }
        }
        for _ in 0..3 {
            let at = (lcg(seed) as usize) % s.len();
            s[at] = lcg(seed) as u8;
        }
        s
    }

    #[test]
    fn genesis_states_round_trip_through_their_dictionaries_and_a_changed_column_is_caught() {
        let mut seed = 99;
        for d in dicts().iter().filter(|d| d.kind == "genesis") {
            for _ in 0..40 {
                let extras: Vec<Vec<u8>> = (0..4).map(|_| (0..32).map(|_| lcg(&mut seed) as u8).collect()).collect();
                let refs: Vec<&[u8]> = extras.iter().map(|e| &e[..]).collect();
                let span = state_like(d, &extras, &mut seed);
                let stored = zpack(&span, Some(d), &refs);
                assert_eq!(zunpack(&stored, &refs).unwrap().as_ref(), &span[..], "{}", d.name);
                if span.len() > 150 {
                    assert_eq!(stored[..3], [TAG, MODE_DICT, d.id]);
                    assert!(stored.len() < span.len() / 2, "{}: {} -> {}", d.name, span.len(), stored.len());
                    // the dictionary columns are part of the value: other ones are an error, never another state
                    let mut other = extras.clone();
                    other[0][0] ^= 1;
                    let other_refs: Vec<&[u8]> = other.iter().map(|e| &e[..]).collect();
                    match zunpack(&stored, &other_refs) {
                        Err(_) => {}
                        Ok(v) => assert_eq!(v.as_ref(), &span[..], "{}: another column gave another state", d.name),
                    }
                }
            }
        }
        // no dictionary, or no gain: the span itself (escaped when it starts with the tag byte)
        let plain = vec![0x20u8; 40];
        assert_eq!(zpack(&plain, None, &[]), plain);
        assert_eq!(zpack(&[0u8, 1, 2], None, &[]), [TAG, MODE_SPAN, 0, 1, 2]);
        // a damaged value, an unknown dictionary, and a value no build writes
        let d = dict_for("genesis", "KobAsk").unwrap();
        let stored = zpack(&d.bytes, Some(d), &[]);
        let mut bad = stored.clone();
        let last = bad.len() - 1;
        bad[last] ^= 0xff;
        assert!(zunpack(&bad, &[]).is_err());
        assert!(zunpack(&[TAG, MODE_DICT, 250, 0, 0, 0, 0, 1], &[]).is_err());
        assert!(zunpack(&[TAG, MODE_DICT], &[]).is_err());
        assert_eq!(zunpack(&[TAG, 9, 9], &[]).unwrap().as_ref(), &[TAG, 9, 9][..], "foreign data reads as itself");
        assert_eq!(zunpack(&[TAG], &[]).unwrap().as_ref(), &[TAG][..]);
    }

    #[test]
    fn event_details_round_trip_and_stay_text_when_a_dictionary_does_not_help() {
        let conn = Connection::open_in_memory().unwrap();
        register(&conn).unwrap();
        let pack =
            |text: &str, kind: &str| -> Value { conn.query_row("SELECT kob_dpack(?1, ?2)", [text, kind], |r| r.get(0)).unwrap() };
        let unpack = |v: Value| -> String { conn.query_row("SELECT kob_dtext(?1)", [v], |r| r.get(0)).unwrap() };
        for (kind, text) in [
            ("create", r#"{"booked":false,"contract":"KobBid","deadline":null,"origin":"chain","parent":null}"#),
            (
                "fill",
                r#"{"delivery_value":"200000001","entry":"fill","escrow_after":"43409762848","escrow_before":"54023297450","input":3,"t":0,"verified":true}"#,
            ),
            ("cancel", r#"{"entry":"cancel","input":0,"verified":true}"#),
            (
                "amend",
                r#"{"entry":"cancel","input":0,"previous":{"activeFrom":0,"expiryDaa":679400964,"price":2791119968,"tif":0,"tip":0},"verified":true}"#,
            ),
        ] {
            let stored = pack(text, kind);
            assert!(matches!(stored, Value::Blob(ref b) if b.len() < text.len() / 2), "{kind}: {stored:?}");
            assert_eq!(unpack(stored), text);
        }
        // a kind without a dictionary, a text a dictionary does not shorten, NULL, and a text a build before this wrote
        assert_eq!(pack(r#"{"a":1}"#, "unknown"), Value::Text(r#"{"a":1}"#.into()));
        assert_eq!(pack(r#"{"a":1}"#, "cancel"), Value::Text(r#"{"a":1}"#.into()));
        assert_eq!(unpack(Value::Text("legacy".into())), "legacy");
        let none: Option<String> = conn.query_row("SELECT kob_dtext(NULL)", [], |r| r.get(0)).unwrap();
        assert_eq!(none, None);
    }

    #[test]
    fn the_genesis_functions_pack_and_unpack_a_state_with_its_columns() {
        let conn = Connection::open_in_memory().unwrap();
        register(&conn).unwrap();
        let d = dict_for("genesis", "KobBid").unwrap();
        let (maker, token, tpl) = ([1u8; 32], [2u8; 32], [3u8; 32]);
        let mut span = d.bytes.clone();
        span[40..72].copy_from_slice(&maker);
        span[100] ^= 7;
        let stored: Vec<u8> = conn
            .query_row(
                "SELECT kob_gpack(?1, 'KobBid', ?2, ?3, ?4, NULL)",
                rusqlite::params![&span[..], &maker[..], &token[..], &tpl[..]],
                |r| r.get(0),
            )
            .unwrap();
        assert!(stored.len() < span.len() / 2, "{} -> {}", span.len(), stored.len());
        let back: Vec<u8> = conn
            .query_row(
                "SELECT kob_gstate(?1, ?2, ?3, ?4, NULL)",
                rusqlite::params![&stored[..], &maker[..], &token[..], &tpl[..]],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(back, span);
        // a contract without a dictionary stores the span as it is
        let whole: Vec<u8> =
            conn.query_row("SELECT kob_gpack(?1, 'NoSuch', NULL, NULL, NULL, NULL)", [&span[..]], |r| r.get(0)).unwrap();
        assert_eq!(whole, span);
        let none: Option<Vec<u8>> = conn.query_row("SELECT kob_gstate(NULL, NULL, NULL, NULL, NULL)", [], |r| r.get(0)).unwrap();
        assert_eq!(none, None);
    }
}
