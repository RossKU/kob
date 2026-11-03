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

use rusqlite::functions::{Context, FunctionFlags};
use rusqlite::types::{Value, ValueRef};
use rusqlite::Connection;
use std::borrow::Cow;

/// First byte of a stored value that is not the span itself.
const TAG: u8 = 0x00;
const MODE_SPAN: u8 = 0x00;
const MODE_DELTA: u8 = 0x01;

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
