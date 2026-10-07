//! `KOB1` transaction payload: versioned TLV (spec: `docs/spec/kob1-payload.md`).
//!
//! ```text
//! payload  = "KOB1" version:u8 record*
//! record   = type:u8 len:u16le value[len]
//! ```
//!
//! Record types `0x00..=0x7f` are critical (a decoder that does not know one must reject the
//! payload); `0x80..=0xff` are optional (skipped). The payload is a hint for indexers and for
//! recovery: nothing in it is trusted. Every order record is re-derived (template hash pinned, state
//! decoded, P2SH recomputed, genesis covenant id recomputed) before it is accepted, see
//! [`recover_orders`]; every in-place amend against the state its input spent, see [`verify_amend`].
//!
//! Version 4 (protocol v3, written whenever a payload carries an `ORDER` or `AMEND` record) is the compact record of
//! version 3 (no template hash: the family and kind name the pinned template; the state span as its fields, integers as
//! LEB128, all-zero 32-byte fields as a bit in a mask; LEB128 indices) over the protocol v3 state layouts (amounts in
//! base units, no lots). A pair order's record (kinds `0x08` `KobPair`, `0x09` `KobCondPair`, `0x0a` `KobIfdPair`, one
//! template each for both families) carries the family of its BASE token A in its family byte; its custody parts are
//! encoded in the family of each custody's own token ([`custody_families`]): the custody of S (`KobPair`, `KobCondPair`;
//! a buy-first `KobIfdPair`'s B escrow, a sell-first one's A custody), then a sell-first `KobIfdPair`'s B PREFUND custody
//! (the record's `prefund` part, present exactly when the state's `custody` is non-zero).
//!
//! Payloads without order records (an x402 commitment, a note) are written as version 2, byte for byte what they were.
//! An `ORDER` record of version 2 and any payload of version 3 describe templates this build does not pin: they do not
//! decode, like a record of any other unknown template.

use kaspa_consensus_core::hashing::covenant_id::covenant_id;
use kaspa_consensus_core::tx::{TransactionId, TransactionOutpoint, TransactionOutput};
use serde::{Deserialize, Serialize};

use crate::artifacts::{pinned_hash, template, token_template_by_hash, Template, TemplateId};
use crate::error::{Error, Result};
use crate::family::Family;
use crate::state::{AnyState, TokenState};
use crate::tx::{spk_from_string, spk_to_string, SigPlan, TxJson};

/// Payload magic.
pub const MAGIC: &[u8; 4] = b"KOB1";
/// Payload format version written for order records (4: the compact placement and in-place amend records of protocol v3,
/// amounts in base units). A payload without order records is written as version 2; version 1 was never deployed, and
/// version 3 (like an `ORDER` record of version 2) named templates this build does not pin.
pub const PAYLOAD_VERSION: u8 = 4;
/// Version 2: written for payloads that carry no `ORDER` / `AMEND` record.
pub const PAYLOAD_VERSION_2: u8 = 2;

/// Record type codes.
pub const REC_ORDER: u8 = 0x01;
/// Retired (reserved, never reused): the v2.4 receipt genesis marker. Protocol v2.6 has no receipts; a
/// payload carrying this critical record type no longer decodes.
pub const REC_RETIRED_RECEIPT_GENESIS: u8 = 0x02;
/// In-place amend (payload versions 3 and 4): the maker's cancel continues the order's covenant id with a new state.
pub const REC_AMEND: u8 = 0x03;
pub const REC_X402: u8 = 0x81;
pub const REC_NOTE: u8 = 0x82;
/// Sweep in place (optional): the maker's cancel continues the order's covenant id with the SAME script and moves only
/// strays. Optional on purpose: an indexer that does not know it treats the continuation as any other unproven
/// continuation of a cancel (state unknown, listed nowhere), never as something else.
pub const REC_SWEEP: u8 = 0x83;

/// Compact ORDER flag: the record carries a day-order deadline.
const FLAG_DEADLINE: u8 = 0x01;
/// Compact ORDER flag: the custody's extension commitment is all zero (not spelled; KCC-20 only).
const FLAG_EXT_ZERO: u8 = 0x02;
/// Compact ORDER flag: the second custody part's (a sell-first `KobIfdPair`'s B prefund) extension commitment is all zero
/// (not spelled; KCC-20 only).
const FLAG_PREFUND_EXT_ZERO: u8 = 0x04;

/// Token family codes (`Family::code`).
pub const FAMILY_KCC20: u8 = 0x01;
pub const FAMILY_KRON46: u8 = 0x02;

/// Legacy x402 text payload prefix (`X402:<hex>`), accepted by [`decode`].
pub const LEGACY_X402_PREFIX: &[u8] = b"X402:";

/// Token custody part of an order record (token-holding kinds only). The amount is not recorded:
/// it is the order's `amountLeft` (exact custody).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Custody {
    /// Output index of the token UTXO owned by the new order (scheme 0x04 / KRON `id_type` 2).
    pub token_output: u16,
    /// KCC-20 extension commitment of the custody output. KRON has none: the KRON record carries no
    /// such field (2-byte custody part) and this must be all zero.
    #[serde(with = "crate::json::field")]
    pub extension_commitment: [u8; 32],
}

/// One payload record.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum Record {
    /// An order created at `output` by this transaction (a pinned template; payload version 4).
    #[serde(rename_all = "camelCase")]
    Order {
        output: u16,
        family: u8,
        template: TemplateId,
        #[serde(with = "crate::json::field")]
        template_hash: [u8; 32],
        #[serde(with = "crate::json::field")]
        state: Vec<u8>,
        custody: Option<Custody>,
        /// The second custody of a sell-first `KobIfdPair` (its B prefund, when it holds one); `None` for every other kind.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        prefund: Option<Custody>,
        /// Day orders: wall-clock deadline (UTC unix seconds) after which conforming matchers stop
        /// filling (`docs/spec/matcher.md` §2.3, §10.10).
        #[serde(with = "crate::json::field", default)]
        deadline: Option<u64>,
    },
    /// An in-place amend (payload version 4): the maker's `cancel` of the order spent at `input` continues
    /// its covenant id at `output` with `state` (the same template; an ask's custody stays where it is). Only
    /// the plain ask and the plain bid of either family ([`amendable`]) and only terms that keep the custody
    /// exact ([`check_amend_terms`]).
    #[serde(rename_all = "camelCase")]
    Amend {
        output: u16,
        input: u16,
        family: u8,
        template: TemplateId,
        #[serde(with = "crate::json::field")]
        state: Vec<u8>,
        #[serde(with = "crate::json::field", default)]
        deadline: Option<u64>,
    },
    /// A maker's sweep in place (optional type 0x83): the maker's `cancel` of the order spent at `input` continues its
    /// covenant id at `output` with the SAME script, so the same state and custody; the transaction only returns strays
    /// (`verify_sweep`).
    #[serde(rename_all = "camelCase")]
    Sweep { output: u16, input: u16 },
    /// x402 payment reference (opaque to KOB indexers).
    #[serde(rename_all = "camelCase")]
    X402 {
        #[serde(with = "crate::json::field")]
        reference: Vec<u8>,
    },
    /// Free-form client tag (UTF-8, at most 64 bytes).
    #[serde(rename_all = "camelCase")]
    Note { text: String },
    /// An optional record of a type this version does not know (kept verbatim).
    #[serde(rename_all = "camelCase")]
    Unknown {
        record_type: u8,
        #[serde(with = "crate::json::field")]
        value: Vec<u8>,
    },
}

impl Record {
    /// Order record (placement record) for a state created at `output`; the family byte is the
    /// state's family (a pair order: the family of its base token A).
    pub fn order(output: u16, state: &AnyState, custody: Option<Custody>, deadline: Option<u64>) -> Record {
        Record::order_with_prefund(output, state, custody, None, deadline)
    }

    /// [`Record::order`] with the second custody part (a sell-first `KobIfdPair`'s B prefund).
    pub fn order_with_prefund(
        output: u16,
        state: &AnyState,
        custody: Option<Custody>,
        prefund: Option<Custody>,
        deadline: Option<u64>,
    ) -> Record {
        let id = state.template_id();
        Record::Order {
            output,
            family: state.family().code(),
            template: id,
            template_hash: template(id).hash,
            state: state.encode(),
            custody,
            prefund,
            deadline,
        }
    }

    /// In-place amend record: `state` continues the order spent at `input` at `output`.
    pub fn amend(output: u16, input: u16, state: &AnyState, deadline: Option<u64>) -> Record {
        let id = state.template_id();
        Record::Amend { output, input, family: state.family().code(), template: id, state: state.encode(), deadline }
    }

    /// True for the records that need the order payload version (any `ORDER` or `AMEND` record).
    fn is_order_record(&self) -> bool {
        matches!(self, Record::Order { .. } | Record::Amend { .. })
    }
}

/// A decoded payload.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Payload {
    pub version: u8,
    pub records: Vec<Record>,
    /// True if decoded from the legacy `X402:<hex>` text form.
    #[serde(default)]
    pub legacy: bool,
}

fn put_record(out: &mut Vec<u8>, ty: u8, value: &[u8]) -> Result<()> {
    let len = u16::try_from(value.len()).map_err(|_| Error::Payload(format!("record {ty:#04x} longer than 65535 bytes")))?;
    out.push(ty);
    out.extend_from_slice(&len.to_le_bytes());
    out.extend_from_slice(value);
    Ok(())
}

// ---------------------------------------------------------------- compact fields (versions 3 and 4)

/// Appends `x` as unsigned LEB128 (7 bits per byte, low group first, high bit = more follows).
fn put_varint(out: &mut Vec<u8>, mut x: u64) {
    loop {
        let b = (x & 0x7f) as u8;
        x >>= 7;
        if x == 0 {
            out.push(b);
            return;
        }
        out.push(b | 0x80);
    }
}

/// Reads a canonical unsigned LEB128 (the shortest spelling, at most 10 bytes, no bits beyond 64): any other
/// spelling of a value is refused, so every record has exactly one encoding.
fn get_varint(v: &[u8], p: &mut usize, what: &str) -> Result<u64> {
    let bad = || Error::Payload(format!("{what}: malformed LEB128 integer"));
    let mut x = 0u64;
    for k in 0..10 {
        let b = *v.get(*p).ok_or_else(|| Error::Payload(format!("{what}: truncated")))?;
        *p += 1;
        if k == 9 && b > 1 {
            return Err(bad());
        }
        x |= u64::from(b & 0x7f) << (7 * k);
        if b & 0x80 == 0 {
            if b == 0 && k > 0 {
                return Err(bad()); // a trailing zero group: not the shortest spelling
            }
            return Ok(x);
        }
    }
    Err(bad())
}

fn get_u16_varint(v: &[u8], p: &mut usize, what: &str) -> Result<u16> {
    u16::try_from(get_varint(v, p, what)?).map_err(|_| Error::Payload(format!("{what}: out of range")))
}

fn get_u8(v: &[u8], p: &mut usize, what: &str) -> Result<u8> {
    let b = *v.get(*p).ok_or_else(|| Error::Payload(format!("{what}: truncated")))?;
    *p += 1;
    Ok(b)
}

fn get_bytes<'a>(v: &'a [u8], p: &mut usize, n: usize, what: &str) -> Result<&'a [u8]> {
    let s = v.get(*p..*p + n).ok_or_else(|| Error::Payload(format!("{what}: truncated")))?;
    *p += n;
    Ok(s)
}

/// The pushes that make up a pinned template's state span, in order (see [`state_layout_of`]).
pub fn state_layout(t: TemplateId) -> Vec<(Vec<u8>, usize)> {
    state_layout_of(template(t))
}

/// The pushes that make up a template's state span, in order: each field's push header (opcode
/// and, for a push of more than 75 bytes, its length bytes) and data length. Every state field is a fixed-width canonical
/// push (`0x20` + 32 bytes, `0x08` + 8 bytes, a committed exit state behind `OP_PUSHDATA1/2`), so the layout is the
/// template's: it is read from the artifact's compiled instance.
pub fn state_layout_of(tpl: &Template) -> Vec<(Vec<u8>, usize)> {
    let code = &tpl.contract().compiled.bytecode;
    let span = &code[tpl.prefix.len()..tpl.prefix.len() + tpl.state_len];
    let mut out = vec![];
    let mut p = 0;
    while p < span.len() {
        let (h, n) = match span[p] {
            op @ 0x01..=0x4b => (1, op as usize),
            0x4c => (2, span[p + 1] as usize),
            0x4d => (3, u16::from_le_bytes([span[p + 1], span[p + 2]]) as usize),
            op => panic!("{}: state field {} is not a data push ({op:#04x})", tpl.contract_name, out.len()),
        };
        out.push((span[p..p + h].to_vec(), n));
        p += h + n;
    }
    assert_eq!(p, span.len(), "{}: state span is not a sequence of pushes", tpl.contract_name);
    out
}

/// Compact state: `zeroMask:LEB128` then every field in template order: an 8-byte field (an int) as the LEB128 of its
/// eight bytes read as a little-endian u64, a 32-byte field raw unless its bit in `zeroMask` says it is all zero (bit k =
/// the k-th 32-byte field), any other field raw. The push headers are the template's and are not spelled.
fn put_compact_state(out: &mut Vec<u8>, tpl: &Template, span: &[u8]) -> Result<()> {
    let mut fields: Vec<&[u8]> = vec![];
    let mut p = 0;
    for (h, n) in state_layout_of(tpl) {
        if span.get(p..p + h.len()) != Some(h.as_slice()) || span.len() < p + h.len() + n {
            return Err(Error::Payload(format!("{} state is not in the template's layout", tpl.contract_name)));
        }
        fields.push(&span[p + h.len()..p + h.len() + n]);
        p += h.len() + n;
    }
    if p != span.len() {
        return Err(Error::Payload(format!("{} state is not in the template's layout", tpl.contract_name)));
    }
    let mut mask = 0u64;
    for (k, f) in fields.iter().filter(|f| f.len() == 32).enumerate() {
        if f.iter().all(|b| *b == 0) {
            mask |= 1 << k;
        }
    }
    put_varint(out, mask);
    let mut k = 0;
    for f in fields {
        match f.len() {
            8 => put_varint(out, u64::from_le_bytes(f.try_into().expect("8"))),
            32 => {
                if mask & (1 << k) == 0 {
                    out.extend_from_slice(f);
                }
                k += 1;
            }
            _ => out.extend_from_slice(f),
        }
    }
    Ok(())
}

/// Reads a compact state and rebuilds the exact state span (refusing a non-canonical spelling: an all-zero 32-byte
/// field not in the mask, a mask bit beyond the fields).
fn get_compact_state(v: &[u8], p: &mut usize, tpl: &Template) -> Result<Vec<u8>> {
    let layout = state_layout_of(tpl);
    let n32 = layout.iter().filter(|(_, n)| *n == 32).count();
    let mask = get_varint(v, p, "state mask")?;
    if n32 < 64 && mask >> n32 != 0 {
        return Err(Error::Payload(format!("{} state mask names a field the template does not have", tpl.contract_name)));
    }
    let mut span = Vec::with_capacity(tpl.state_len);
    let mut k = 0;
    for (h, n) in layout {
        span.extend_from_slice(&h);
        match n {
            8 => span.extend_from_slice(&get_varint(v, p, "state field")?.to_le_bytes()),
            32 => {
                if mask & (1 << k) != 0 {
                    span.extend_from_slice(&[0; 32]);
                } else {
                    let f = get_bytes(v, p, 32, "state field")?;
                    if f.iter().all(|b| *b == 0) {
                        return Err(Error::Payload("an all-zero 32-byte state field must be spelled by the mask".into()));
                    }
                    span.extend_from_slice(f);
                }
                k += 1;
            }
            _ => span.extend_from_slice(get_bytes(v, p, n, "state field")?),
        }
    }
    Ok(span)
}

/// The checks an encoder shares with the decoder: an order template serving the record's family, a state of the
/// template's length that decodes canonically, of that family (a pair order: the family of its base token A is the
/// record's family), and passes the numeric gate ([`AnyState::check_numbers`]).
fn check_order_state(family: u8, t: TemplateId, state: &[u8]) -> Result<AnyState> {
    if t.kind_code().is_none() {
        return Err(Error::Payload(format!("{} is not an order template", t.name())));
    }
    let fam = Family::from_code(family).ok_or_else(|| Error::Payload(format!("unsupported token family {family:#04x}")))?;
    if !t.serves(fam) {
        return Err(Error::Payload(format!("{}: family byte {family:#04x} does not match the template", t.name())));
    }
    if state.len() != template(t).state_len {
        return Err(Error::Payload(format!("{} state must be {} bytes", t.name(), template(t).state_len)));
    }
    let s = AnyState::decode(t, state)?;
    if s.family() != fam {
        return Err(Error::Payload(format!("{}: the record's family {family:#04x} is not the state's (its base token's)", t.name())));
    }
    s.check_numbers().map_err(Error::Payload)?;
    Ok(s)
}

/// Encodes records into a `KOB1` payload: version 4 when it carries an `ORDER` or `AMEND` record, otherwise version 2
/// (an x402 commitment or a note is the same bytes in every version, and stays what the x402 profile specifies).
pub fn encode(records: &[Record]) -> Result<Vec<u8>> {
    let mut out = MAGIC.to_vec();
    out.push(if records.iter().any(Record::is_order_record) { PAYLOAD_VERSION } else { PAYLOAD_VERSION_2 });
    for r in records {
        match r {
            Record::Order { output, family, template: t, template_hash, state, custody, prefund, deadline } => {
                // The rules a decoder applies: a malformed placement is refused here, not only
                // by the indexer after it is on chain.
                let s = check_order_state(*family, *t, state)?;
                if *template_hash != pinned_hash(*t) {
                    return Err(Error::Payload(format!("template hash of {} is not the pinned one", t.name())));
                }
                let fams = custody_families(&s);
                let parts: Vec<&Custody> = custody.iter().chain(prefund.iter()).collect();
                if parts.len() != fams.len() || (prefund.is_some() && custody.is_none()) {
                    return Err(Error::Payload(format!(
                        "{}: the record needs exactly {} custody part(s) (the custodies the order holds)",
                        t.name(),
                        fams.len()
                    )));
                }
                let mut flags = 0;
                if deadline.is_some() {
                    flags |= FLAG_DEADLINE;
                }
                for (k, (c, fam)) in parts.iter().zip(&fams).enumerate() {
                    let zero = c.extension_commitment == [0; 32];
                    match (fam, zero) {
                        (Family::Kron, false) => {
                            return Err(Error::Payload("KRON tokens have no extension commitment (must be zero)".into()))
                        }
                        (Family::Kcc20, true) => flags |= if k == 0 { FLAG_EXT_ZERO } else { FLAG_PREFUND_EXT_ZERO },
                        _ => {}
                    }
                }
                let mut v = vec![];
                put_varint(&mut v, u64::from(*output));
                v.push(*family);
                v.push(t.kind_code().expect("kind"));
                v.push(flags);
                put_compact_state(&mut v, template(*t), state)?;
                for (c, fam) in parts.iter().zip(&fams) {
                    put_varint(&mut v, u64::from(c.token_output));
                    if *fam == Family::Kcc20 && c.extension_commitment != [0; 32] {
                        v.extend_from_slice(&c.extension_commitment);
                    }
                }
                if let Some(d) = deadline {
                    put_varint(&mut v, *d);
                }
                put_record(&mut out, REC_ORDER, &v)?;
            }
            Record::Amend { output, input, family, template: t, state, deadline } => {
                check_order_state(*family, *t, state)?;
                if !amendable(*t) {
                    return Err(Error::Payload(format!("{} orders are not amended in place", t.name())));
                }
                let mut v = vec![];
                put_varint(&mut v, u64::from(*output));
                put_varint(&mut v, u64::from(*input));
                v.push(*family);
                v.push(t.kind_code().expect("kind"));
                v.push(if deadline.is_some() { FLAG_DEADLINE } else { 0 });
                put_compact_state(&mut v, template(*t), state)?;
                if let Some(d) = deadline {
                    put_varint(&mut v, *d);
                }
                put_record(&mut out, REC_AMEND, &v)?;
            }
            Record::Sweep { output, input } => {
                let mut v = vec![];
                put_varint(&mut v, u64::from(*output));
                put_varint(&mut v, u64::from(*input));
                put_record(&mut out, REC_SWEEP, &v)?;
            }
            Record::X402 { reference } => {
                if reference.is_empty() || reference.len() > 64 {
                    return Err(Error::Payload("x402 reference must be 1..=64 bytes".into()));
                }
                put_record(&mut out, REC_X402, reference)?
            }
            Record::Note { text } => {
                if text.len() > 64 {
                    return Err(Error::Payload("note longer than 64 bytes".into()));
                }
                put_record(&mut out, REC_NOTE, text.as_bytes())?
            }
            Record::Unknown { record_type, value } => {
                if *record_type < 0x80 {
                    return Err(Error::Payload("unknown records must use an optional type (>= 0x80)".into()));
                }
                put_record(&mut out, *record_type, value)?
            }
        }
    }
    Ok(out)
}

/// True for the KAS kinds that hold tokens in covenant-id custody (and carry a custody part). The pair kinds hold one or
/// two custodies depending on their state: [`custody_families`].
pub fn holds_tokens(t: TemplateId) -> bool {
    matches!(t.base(), TemplateId::KobAsk | TemplateId::KobCondAsk | TemplateId::KobIfdAsk)
}

/// The custody parts a placement record of `s` carries, in order, as the family of each custody's token (the codec of
/// its extension commitment): a KAS kind's one custody of its token; a pair order's [`AnyState::custodies`] (the custody
/// of S, a sell-first entry's A custody and then its B prefund), each in its own token's family.
pub fn custody_families(s: &AnyState) -> Vec<Family> {
    match s.pair_tokens() {
        Some(t) => s
            .custodies()
            .iter()
            .map(|(tok, _)| if *tok == t.a.cov_id { t.a.family_of() } else { t.b.family_of() }.unwrap_or(Family::Kcc20))
            .collect(),
        None if holds_tokens(s.template_id()) => vec![s.family()],
        None => vec![],
    }
}

/// Decodes a payload. Returns `Ok(None)` for payloads that are neither `KOB1` nor legacy x402.
pub fn decode(bytes: &[u8]) -> Result<Option<Payload>> {
    if let Some(hex) = bytes.strip_prefix(LEGACY_X402_PREFIX) {
        let s = std::str::from_utf8(hex).map_err(|_| Error::Payload("legacy x402 payload is not text".into()))?;
        let reference = crate::json::from_hex(s).map_err(Error::Payload)?;
        return Ok(Some(Payload { version: 0, records: vec![Record::X402 { reference }], legacy: true }));
    }
    if !bytes.starts_with(MAGIC) {
        return Ok(None);
    }
    let bad = |m: &str| Err(Error::Payload(m.to_string()));
    if bytes.len() < 5 {
        return bad("truncated header");
    }
    let version = bytes[4];
    if !matches!(version, PAYLOAD_VERSION | PAYLOAD_VERSION_2) {
        return Err(Error::Payload(format!("unsupported KOB1 version {version}")));
    }
    let mut records = vec![];
    let mut p = 5;
    while p < bytes.len() {
        if p + 3 > bytes.len() {
            return bad("truncated record header");
        }
        let ty = bytes[p];
        let len = u16::from_le_bytes([bytes[p + 1], bytes[p + 2]]) as usize;
        p += 3;
        if p + len > bytes.len() {
            return bad("truncated record value");
        }
        let v = &bytes[p..p + len];
        p += len;
        records.push(match (ty, version) {
            (REC_ORDER, PAYLOAD_VERSION) => decode_order_compact(v)?,
            (REC_ORDER, _) => return bad("a version-2 ORDER record names a template this build does not pin"),
            (REC_AMEND, PAYLOAD_VERSION) => decode_amend(v)?,
            (REC_RETIRED_RECEIPT_GENESIS, _) => return bad("record type 0x02 (the v2.4 receipt genesis) is retired"),
            (REC_X402, _) if v.is_empty() || v.len() > 64 => return bad("x402 reference must be 1..=64 bytes"),
            (REC_X402, _) => Record::X402 { reference: v.to_vec() },
            (REC_NOTE, _) if v.len() > 64 => return bad("note longer than 64 bytes"),
            // NOTE is optional: bytes that are not UTF-8 are kept as an unknown optional record instead of
            // failing the whole payload.
            (REC_NOTE, _) => match String::from_utf8(v.to_vec()) {
                Ok(text) => Record::Note { text },
                Err(_) => Record::Unknown { record_type: REC_NOTE, value: v.to_vec() },
            },
            (REC_SWEEP, _) => decode_sweep(v)?,
            (t, _) if t >= 0x80 => Record::Unknown { record_type: t, value: v.to_vec() },
            (t, _) => return Err(Error::Payload(format!("unknown critical record type {t:#04x}"))),
        });
    }
    Ok(Some(Payload { version, records, legacy: false }))
}

/// Family and kind bytes of a record: a known family, a kind that is not retired.
fn decode_kind(family: u8, kind: u8) -> Result<TemplateId> {
    let Some(fam) = Family::from_code(family) else {
        return Err(Error::Payload(format!("unsupported token family {family:#04x}")));
    };
    if crate::artifacts::RETIRED_KIND_CODES.contains(&kind) {
        return Err(Error::Payload(format!("order kind {kind:#04x} is retired (the v2.4 trade receipt)")));
    }
    TemplateId::from_kind_code(fam, kind).ok_or_else(|| Error::Payload(format!("unknown order kind {kind:#04x}")))
}

/// Compact ORDER record (version 4): `output:LEB128 family:u8 kind:u8 flags:u8 state [custody] [deadline:LEB128]`.
fn decode_order_compact(v: &[u8]) -> Result<Record> {
    let mut p = 0;
    let output = get_u16_varint(v, &mut p, "order output")?;
    let family = get_u8(v, &mut p, "family")?;
    let t = decode_kind(family, get_u8(v, &mut p, "kind")?)?;
    let flags = get_u8(v, &mut p, "flags")?;
    if flags & !(FLAG_DEADLINE | FLAG_EXT_ZERO | FLAG_PREFUND_EXT_ZERO) != 0 {
        return Err(Error::Payload(format!("order record flags {flags:#04x}: unknown bits")));
    }
    let state = get_compact_state(v, &mut p, template(t))?;
    let fams = custody_families(&check_order_state(family, t, &state)?);
    let mut parts = vec![];
    for (k, cfam) in fams.iter().enumerate() {
        let token_output = get_u16_varint(v, &mut p, "custody output")?;
        let flag = if k == 0 { FLAG_EXT_ZERO } else { FLAG_PREFUND_EXT_ZERO };
        let extension_commitment = match (cfam, flags & flag != 0) {
            (Family::Kcc20, true) => [0; 32],
            (Family::Kcc20, false) => {
                let e: [u8; 32] = get_bytes(v, &mut p, 32, "extension commitment")?.try_into().expect("32");
                if e == [0; 32] {
                    return Err(Error::Payload("an all-zero extension commitment must be spelled by its flag".into()));
                }
                e
            }
            (Family::Kron, false) => [0; 32],
            (Family::Kron, true) => return Err(Error::Payload("KRON tokens have no extension commitment (flag set)".into())),
        };
        parts.push(Custody { token_output, extension_commitment });
    }
    for (k, flag) in [FLAG_EXT_ZERO, FLAG_PREFUND_EXT_ZERO].into_iter().enumerate() {
        if k >= fams.len() && flags & flag != 0 {
            return Err(Error::Payload(format!("{} has no custody part {k} (extension flag set)", t.name())));
        }
    }
    let mut parts = parts.into_iter();
    let custody = parts.next();
    let prefund = parts.next();
    let deadline = if flags & FLAG_DEADLINE != 0 { Some(get_varint(v, &mut p, "deadline")?) } else { None };
    if p != v.len() {
        return Err(Error::Payload(format!("order record has {} trailing bytes", v.len() - p)));
    }
    Ok(Record::Order { output, family, template: t, template_hash: pinned_hash(t), state, custody, prefund, deadline })
}

/// SWEEP record (optional, any version): `output:LEB128 input:LEB128`.
fn decode_sweep(v: &[u8]) -> Result<Record> {
    let mut p = 0;
    let output = get_u16_varint(v, &mut p, "sweep output")?;
    let input = get_u16_varint(v, &mut p, "sweep input")?;
    if p != v.len() {
        return Err(Error::Payload(format!("sweep record has {} trailing bytes", v.len() - p)));
    }
    Ok(Record::Sweep { output, input })
}

/// AMEND record (version 4): `output:LEB128 input:LEB128 family:u8 kind:u8 flags:u8 state [deadline:LEB128]`.
fn decode_amend(v: &[u8]) -> Result<Record> {
    let mut p = 0;
    let output = get_u16_varint(v, &mut p, "amend output")?;
    let input = get_u16_varint(v, &mut p, "amend input")?;
    let family = get_u8(v, &mut p, "family")?;
    let t = decode_kind(family, get_u8(v, &mut p, "kind")?)?;
    if !amendable(t) {
        return Err(Error::Payload(format!("{} orders are not amended in place", t.name())));
    }
    let flags = get_u8(v, &mut p, "flags")?;
    if flags & !FLAG_DEADLINE != 0 {
        return Err(Error::Payload(format!("amend record flags {flags:#04x}: unknown bits")));
    }
    let state = get_compact_state(v, &mut p, template(t))?;
    check_order_state(family, t, &state)?;
    let deadline = if flags & FLAG_DEADLINE != 0 { Some(get_varint(v, &mut p, "deadline")?) } else { None };
    if p != v.len() {
        return Err(Error::Payload(format!("amend record has {} trailing bytes", v.len() - p)));
    }
    Ok(Record::Amend { output, input, family, template: t, state, deadline })
}

/// An order re-derived from a genesis transaction.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RecoveredOrder {
    #[serde(with = "crate::json::field")]
    pub transaction_id: [u8; 32],
    pub output: u32,
    #[serde(with = "crate::json::field")]
    pub value: u64,
    #[serde(with = "crate::json::field")]
    pub covenant_id: [u8; 32],
    pub order: AnyState,
    /// Token custody UTXO (token-holding kinds; a pair order: the custody of S, a sell-first entry's A custody).
    pub custody: Option<RecoveredCustody>,
    /// A sell-first `KobIfdPair`'s B prefund custody UTXO (when it holds one).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prefund: Option<RecoveredCustody>,
    /// Day-order deadline from the placement record (UTC unix seconds).
    #[serde(with = "crate::json::field", default)]
    pub deadline: Option<u64>,
}

/// The custody token UTXO of a recovered order.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RecoveredCustody {
    pub output: u32,
    #[serde(with = "crate::json::field")]
    pub value: u64,
    pub state: TokenState,
}

/// The genesis checks of an order record at output `o` whose script must be `spk`: the output is a genesis whose
/// covenant id is recomputed from the authorising input's outpoint, and the genesis group is that output alone. Returns
/// the covenant id and the output's value.
fn verify_genesis(tx: &TxJson, o: usize, spk: &str) -> Result<([u8; 32], u64)> {
    let bad = |m: &str| Err(Error::Payload(format!("order record for output {o}: {m}")));
    let Some(txo) = tx.outputs.get(o) else { return bad("no such output") };
    if spk != txo.script_public_key {
        return bad("output script is not the P2SH of the recorded state");
    }
    let Some(cov) = &txo.covenant else { return bad("output has no covenant binding") };
    let auth = cov.authorizing_input as usize;
    let Some(inp) = tx.inputs.get(auth) else { return bad("authorising input missing") };
    if inp.utxo.covenant_id == Some(cov.covenant_id) {
        return bad("output continues an existing covenant (not a genesis)");
    }
    let group: Vec<(u32, TransactionOutput)> = tx
        .outputs
        .iter()
        .enumerate()
        .filter(|(_, x)| x.covenant.as_ref() == Some(cov))
        .map(|(k, x)| {
            Ok((
                k as u32,
                TransactionOutput { value: x.value, script_public_key: spk_from_string(&x.script_public_key)?, covenant: None },
            ))
        })
        .collect::<Result<_>>()?;
    let op = TransactionOutpoint { transaction_id: TransactionId::from_bytes(inp.transaction_id), index: inp.index };
    let id = covenant_id(op, group.iter().map(|(k, x)| (*k, x))).as_bytes();
    if id != cov.covenant_id {
        return bad("covenant id is not the genesis id of its group");
    }
    // The genesis group is the order output alone: a sibling bound to the same covenant
    // id would carry it forever and could unlock the order's custody (KCC-20 scheme 0x04 / KRON
    // id_type 2 only check that some input carries the id) outside the order's rules.
    let bound = tx.outputs.iter().filter(|x| x.covenant.as_ref().is_some_and(|c| c.covenant_id == id)).count();
    if group.len() != 1 || bound != 1 {
        return bad("the genesis group has other outputs (an order covenant must be its output alone)");
    }
    Ok((id, txo.value))
}

/// The custody output of an order record: the P2SH of the custody state (`amount` owned by the order's covenant id
/// `id`) under the order's token program `tpl_hash` of family `fam`, bound to the token covenant `token`.
#[allow(clippy::too_many_arguments)]
fn verify_custody(
    tx: &TxJson,
    o: usize,
    c: &Custody,
    fam: Family,
    tpl_hash: &[u8; 32],
    token: [u8; 32],
    amount: i64,
    id: [u8; 32],
) -> Result<RecoveredCustody> {
    let bad = |m: &str| Err(Error::Payload(format!("order record for output {o}: {m}")));
    let tt = token_template_by_hash(tpl_hash).ok_or_else(|| Error::Payload("unknown token program".into()))?;
    if tt.family != fam {
        return bad("the token program is of the other family");
    }
    if amount <= 0 {
        return bad("a new token-holding order must hold tokens");
    }
    let st = TokenState::custody(fam, amount, id, c.extension_commitment);
    let Some(tout) = tx.outputs.get(c.token_output as usize) else { return bad("custody output missing") };
    if spk_to_string(&st.spk_with(tt)) != tout.script_public_key {
        return bad("custody output is not the order's token custody");
    }
    if tout.covenant.as_ref().map(|c| c.covenant_id) != Some(token) {
        return bad("custody output is not bound to the token covenant");
    }
    Ok(RecoveredCustody { output: c.token_output as u32, value: tout.value, state: st })
}

/// The two tokens of a pair order: supported programs of the recorded families, with the lengths the covenant uses, and
/// two different tokens.
fn check_pair_programs(o: usize, t: &crate::state::PairTokens) -> Result<()> {
    let bad = |m: String| Err(Error::Payload(format!("order record for output {o}: {m}")));
    for (what, k) in [("A", &t.a), ("B", &t.b)] {
        let Some(tt) = token_template_by_hash(&k.tpl_hash) else { return bad(format!("unknown token {what} program")) };
        if Some(tt.family) != k.family_of() {
            return bad(format!("token {what} program is not of the recorded family"));
        }
        if (tt.prefix.len() as i64, tt.suffix.len() as i64) != (k.prefix_len, k.suffix_len) {
            return bad(format!("token {what} template lengths do not match its program"));
        }
    }
    if t.a.cov_id == t.b.cov_id {
        return bad("a pair order needs two different tokens".into());
    }
    Ok(())
}

/// Re-derives every order a transaction created from its `KOB1` payload (version 4 records of the pinned templates),
/// trusting nothing: the template must be pinned, the state must decode canonically, the output's script must be
/// the P2SH of `template(state)`, the output must be a genesis whose covenant id is recomputed from
/// the authorising input's outpoint, no other output may be bound to that covenant id (the genesis
/// group is the order output alone), and the custody token output (if any) must be the P2SH of the
/// custody state (exactly `amountLeft`) under the order's token program. Records failing any check are errors.
pub fn recover_orders(tx: &TxJson) -> Result<Vec<RecoveredOrder>> {
    let Some(p) = decode(&tx.payload)? else { return Ok(vec![]) };
    let mut out = vec![];
    for r in &p.records {
        let Record::Order { output, family, template: t, state, custody, prefund, deadline, .. } = r else { continue };
        let o = *output as usize;
        let bad = |m: &str| Err(Error::Payload(format!("order record for output {o}: {m}")));
        let order = check_order_state(*family, *t, state)?;
        let (id, value) = verify_genesis(tx, o, &spk_to_string(&order.spk()))?;
        // the custodies the order holds: (token, its program hash, family, amount)
        let wanted: Vec<([u8; 32], [u8; 32], Family, i64)> = match order.pair_tokens() {
            Some(pt) => {
                check_pair_programs(o, &pt)?;
                order
                    .custodies()
                    .into_iter()
                    .map(|(tok, amount)| {
                        let k = if tok == pt.a.cov_id { pt.a } else { pt.b };
                        (tok, k.tpl_hash, k.family_of().unwrap_or(Family::Kcc20), amount)
                    })
                    .collect()
            }
            None if order.holds_tokens() => vec![(
                order.token_cov_id(),
                order.token_tpl_hash().expect("order"),
                order.family(),
                order.custody_amount().expect("token-holding kind"),
            )],
            None => vec![],
        };
        let parts: Vec<&Custody> = custody.iter().chain(prefund.iter()).collect();
        if parts.len() != wanted.len() {
            return if wanted.is_empty() {
                bad("custody part on a KAS-holding order")
            } else {
                bad("token-holding order without its custody parts")
            };
        }
        let mut got = vec![];
        for (c, (tok, tpl_hash, fam, amount)) in parts.into_iter().zip(wanted) {
            got.push(verify_custody(tx, o, c, fam, &tpl_hash, tok, amount, id)?);
        }
        let mut got = got.into_iter();
        let (custody, prefund) = (got.next(), got.next());
        out.push(RecoveredOrder {
            transaction_id: tx.id,
            output: o as u32,
            value,
            covenant_id: id,
            order,
            custody,
            prefund,
            deadline: *deadline,
        });
    }
    Ok(out)
}

// ---------------------------------------------------------------- in-place amend

/// Kinds a maker may amend in place (AMEND record): the plain ask and the plain bid of either family. Their
/// `cancel` (maker, SIGHASH_ALL) constrains no output, so the maker can continue the order's covenant id. An ask's
/// custody, owned by that id, never moves; a bid owns no tokens (its quantity is the KAS escrow on the order UTXO
/// itself, and a stray owned by its id stays a stray: only a cancel sweeps strays).
pub fn amendable(t: TemplateId) -> bool {
    matches!(t.base(), TemplateId::KobAsk | TemplateId::KobBid)
}

/// The terms an in-place amend keeps.
///
/// A plain ask: the template, the maker, the token (covenant id, program and its template lengths), the `scale` (prices
/// of one lineage stay comparable) and `amountLeft`, so the custody the order already holds stays exactly `amountLeft`
/// (C7). What may change: `price`, `tip`, `minFill`, `tif`, `activeFrom`, `expiryDaa`, `refundTip`, `interval`,
/// `maxFill`, `slope`, `priceEnd`, `decayStep` (a quantity change re-custodies: cancel-replace).
///
/// A plain bid: the template, the maker, the token (covenant id, program, template lengths and the pinned
/// `extensionCommitment` its deliveries carry) and the `scale`. Its quantity is its escrow, so every other term may
/// change (price, tip, minimum fill, time in force, timing, rising-bid terms, `reserve`, `deliveryCarrier`); the builder
/// checks that the continuation still funds one minimum fill at the new terms ([`crate::build::min_order_value`]).
pub fn check_amend_terms(previous: &AnyState, next: &AnyState) -> Result<()> {
    let bad = |m: &str| Err(Error::Payload(format!("amend: {m}")));
    if previous.template_id() != next.template_id() {
        return bad("the amended order must keep its template");
    }
    match (previous, next) {
        (AnyState::KobAsk(a) | AnyState::KobAskKron(a), AnyState::KobAsk(b) | AnyState::KobAskKron(b)) => {
            if a.maker != b.maker {
                return bad("the maker cannot change");
            }
            if (a.token_cov_id, a.token_tpl_hash, a.tpl_prefix_len, a.tpl_suffix_len)
                != (b.token_cov_id, b.token_tpl_hash, b.tpl_prefix_len, b.tpl_suffix_len)
            {
                return bad("the token cannot change");
            }
            if (a.scale, a.amount_left) != (b.scale, b.amount_left) {
                return bad("scale and amountLeft cannot change (the custody stays where it is: change the amount by cancel-replace)");
            }
            if b.amount_left <= 0 {
                return bad("an amended order must hold tokens");
            }
        }
        (AnyState::KobBid(a) | AnyState::KobBidKron(a), AnyState::KobBid(b) | AnyState::KobBidKron(b)) => {
            if a.maker != b.maker {
                return bad("the maker cannot change");
            }
            if (a.token_cov_id, a.token_tpl_hash, a.tpl_prefix_len, a.tpl_suffix_len, a.extension_commitment)
                != (b.token_cov_id, b.token_tpl_hash, b.tpl_prefix_len, b.tpl_suffix_len, b.extension_commitment)
            {
                return bad("the token cannot change");
            }
            if a.scale != b.scale {
                return bad("the scale cannot change (prices of one order stay comparable)");
            }
        }
        _ => return bad("only a plain ask or a plain bid is amended in place"),
    }
    Ok(())
}

/// An order its maker amended in place (AMEND record), re-derived trusting nothing but the previous state, which
/// the caller proves (the indexer from the revealed redeem script it tracks, a wallet from its own signing plan).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AmendedOrder {
    #[serde(with = "crate::json::field")]
    pub transaction_id: [u8; 32],
    /// The continuation output (the amended order).
    pub output: u32,
    /// The order input the maker's `cancel` spent.
    pub input: u32,
    #[serde(with = "crate::json::field")]
    pub value: u64,
    /// The order's covenant id, unchanged.
    #[serde(with = "crate::json::field")]
    pub covenant_id: [u8; 32],
    pub order: AnyState,
    pub previous: AnyState,
    /// Day-order deadline from the record (UTC unix seconds).
    #[serde(with = "crate::json::field", default)]
    pub deadline: Option<u64>,
}

/// Verifies one AMEND record of `tx` against `previous`, the state of the order the input spent, and `entry`, the
/// entry that spent it: the record's state decodes canonically under a pinned amendable template, the input was
/// spent by the maker's `cancel`, carries a covenant id and is the P2SH of `previous`, no other input carries that
/// id, the terms keep the custody exact ([`check_amend_terms`]), and the output is the P2SH of the new state,
/// bound to the same covenant id with the order input as its authorising input, and the ONLY output of the
/// transaction bound to that id (as for a genesis, `recover_orders` rule 4: a sibling would carry the id and could
/// unlock the custody outside the order's rules).
pub fn verify_amend(tx: &TxJson, record: &Record, previous: &AnyState, entry: &str) -> Result<AmendedOrder> {
    let Record::Amend { output, input, family, template: t, state, deadline } = record else {
        return Err(Error::Payload("not an amend record".into()));
    };
    let (o, i) = (*output as usize, *input as usize);
    let bad = |m: &str| Err(Error::Payload(format!("amend record for output {o}: {m}")));
    check_order_state(*family, *t, state)?;
    if !amendable(*t) {
        return bad("this kind is not amended in place");
    }
    let order = AnyState::decode(*t, state)?;
    if entry != "cancel" {
        return bad("the order input is not spent by its maker's cancel");
    }
    check_amend_terms(previous, &order)?;
    let Some(inp) = tx.inputs.get(i) else { return bad("no such input") };
    let Some(id) = inp.utxo.covenant_id else { return bad("the input carries no covenant id") };
    if inp.utxo.script_public_key != spk_to_string(&previous.spk()) {
        return bad("the input is not the order the record continues");
    }
    if tx.inputs.iter().filter(|x| x.utxo.covenant_id == Some(id)).count() != 1 {
        return bad("another input carries the order's covenant id");
    }
    let Some(txo) = tx.outputs.get(o) else { return bad("no such output") };
    if spk_to_string(&order.spk()) != txo.script_public_key {
        return bad("output script is not the P2SH of the recorded state");
    }
    if txo.covenant.as_ref().map(|c| (c.authorizing_input, c.covenant_id)) != Some((*input, id)) {
        return bad("the output does not continue the order's covenant id from the order input");
    }
    let bound = tx.outputs.iter().filter(|x| x.covenant.as_ref().is_some_and(|c| c.covenant_id == id)).count();
    if bound != 1 {
        return bad("other outputs carry the order's covenant id (an amended order is one output)");
    }
    Ok(AmendedOrder {
        transaction_id: tx.id,
        output: o as u32,
        input: i as u32,
        value: txo.value,
        covenant_id: id,
        order,
        previous: previous.clone(),
        deadline: *deadline,
    })
}

/// An order its maker swept in place (SWEEP record): the same order at a new output.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SweptOrder {
    #[serde(with = "crate::json::field")]
    pub transaction_id: [u8; 32],
    /// The continuation output (the same order).
    pub output: u32,
    /// The order input the maker's `cancel` spent.
    pub input: u32,
    #[serde(with = "crate::json::field")]
    pub value: u64,
    #[serde(with = "crate::json::field")]
    pub covenant_id: [u8; 32],
}

/// Verifies one SWEEP record of `tx` against `previous`, the state of the order the input spent (the caller proves it), and
/// `entry`, the entry that spent it: the input was spent by the maker's `cancel`, carries a covenant id and is the P2SH of
/// `previous`, no other input carries that id, and the output is the P2SH of the SAME state, bound to the same covenant
/// id with the order input as its authorising input and the only output bound to that id. The order continues unchanged
/// (custody included: a custody spent by the same transaction is the caller's check, as for an amend).
pub fn verify_sweep(tx: &TxJson, record: &Record, previous: &AnyState, entry: &str) -> Result<SweptOrder> {
    let Record::Sweep { output, input } = record else {
        return Err(Error::Payload("not a sweep record".into()));
    };
    let (o, i) = (*output as usize, *input as usize);
    let bad = |m: &str| Err(Error::Payload(format!("sweep record for output {o}: {m}")));
    if entry != "cancel" {
        return bad("the order input is not spent by its maker's cancel");
    }
    let Some(inp) = tx.inputs.get(i) else { return bad("no such input") };
    let Some(id) = inp.utxo.covenant_id else { return bad("the input carries no covenant id") };
    let spk = spk_to_string(&previous.spk());
    if inp.utxo.script_public_key != spk {
        return bad("the input is not the order the record continues");
    }
    if tx.inputs.iter().filter(|x| x.utxo.covenant_id == Some(id)).count() != 1 {
        return bad("another input carries the order's covenant id");
    }
    let Some(txo) = tx.outputs.get(o) else { return bad("no such output") };
    if txo.script_public_key != spk {
        return bad("the output is not the same order (another script)");
    }
    if txo.covenant.as_ref().map(|c| (c.authorizing_input, c.covenant_id)) != Some((*input, id)) {
        return bad("the output does not continue the order's covenant id from the order input");
    }
    let bound = tx.outputs.iter().filter(|x| x.covenant.as_ref().is_some_and(|c| c.covenant_id == id)).count();
    if bound != 1 {
        return bad("other outputs carry the order's covenant id (a swept order is one output)");
    }
    Ok(SweptOrder { transaction_id: tx.id, output: o as u32, input: i as u32, value: txo.value, covenant_id: id })
}

/// Data pushes of a push-only script (direct pushes, OP_PUSHDATA1/2/4, and the small-number opcodes `OP_1NEGATE`,
/// `OP_1` .. `OP_16`, which push no data bytes: they appear as empty items); `None` for any other opcode.
fn script_pushes(script: &[u8]) -> Option<Vec<&[u8]>> {
    let mut out = vec![];
    let mut p = 0usize;
    while p < script.len() {
        let op = script[p];
        p += 1;
        let n = match op {
            0x00 | 0x4f | 0x51..=0x60 => 0,
            0x01..=0x4b => op as usize,
            0x4c => {
                p += 1;
                *script.get(p - 1)? as usize
            }
            0x4d => {
                p += 2;
                u16::from_le_bytes(script.get(p - 2..p)?.try_into().ok()?) as usize
            }
            0x4e => {
                p += 4;
                u32::from_le_bytes(script.get(p - 4..p)?.try_into().ok()?) as usize
            }
            _ => return None,
        };
        out.push(script.get(p..p.checked_add(n)?)?);
        p += n;
    }
    Some(out)
}

/// The previous state and entry of an order input from its signature script (`args… ‖ push(tag) ‖ push(redeem)`):
/// the redeem script must be an instance of `t`, the tag one of its entries.
fn revealed(t: TemplateId, sigscript: &[u8]) -> Result<(AnyState, String)> {
    let bad = |m: &str| Error::Payload(format!("amend: {m}"));
    let pushes = script_pushes(sigscript).ok_or_else(|| bad("the order input's signature script is not push-only"))?;
    let [.., tag, redeem] = pushes.as_slice() else { return Err(bad("the order input is unsigned or reveals no redeem script")) };
    let tpl = template(t);
    let span = tpl.state_of(redeem).ok_or_else(|| bad("the order input does not reveal the record's template"))?;
    let previous = AnyState::decode(t, span)?;
    let entry = tpl
        .contract()
        .entries
        .iter()
        .find(|(_, e)| e.dispatch_tag.as_bytes() == *tag)
        .map(|(k, _)| k.clone())
        .ok_or_else(|| bad("the order input calls no entry of its template"))?;
    Ok((previous, entry))
}

/// Re-derives every in-place amend of a SIGNED transaction from its `KOB1` payload: the previous state and the entry
/// are what each order input's signature script reveals (its redeem script hashes to the spent output: consensus
/// checked it, [`verify_amend`] checks the P2SH again). All-or-nothing per payload, like [`recover_orders`].
pub fn recover_amends(tx: &TxJson) -> Result<Vec<AmendedOrder>> {
    recover_amends_with(tx, |r| {
        let Record::Amend { input, template: t, .. } = r else { unreachable!("amend records only") };
        let inp = tx.inputs.get(*input as usize).ok_or_else(|| Error::Payload("amend: no such input".into()))?;
        revealed(*t, &inp.signature_script)
    })
}

/// [`recover_amends`] for a built, unsigned transaction: the previous state and the entry are the signing plan's
/// (`BuiltTx::plans`), which the builder derived from the order the wallet is spending.
pub fn recover_amends_planned(tx: &TxJson, plans: &[SigPlan]) -> Result<Vec<AmendedOrder>> {
    recover_amends_with(tx, |r| {
        let Record::Amend { input, .. } = r else { unreachable!("amend records only") };
        match plans.get(*input as usize) {
            Some(SigPlan::Entry { template: t, state, entry, .. }) => Ok((AnyState::decode(*t, state)?, entry.clone())),
            _ => Err(Error::Payload("amend: the input's signing plan is not an order entry".into())),
        }
    })
}

fn recover_amends_with(tx: &TxJson, previous: impl Fn(&Record) -> Result<(AnyState, String)>) -> Result<Vec<AmendedOrder>> {
    let Some(p) = decode(&tx.payload)? else { return Ok(vec![]) };
    let mut out = vec![];
    for r in p.records.iter().filter(|r| matches!(r, Record::Amend { .. })) {
        let (prev, entry) = previous(r)?;
        out.push(verify_amend(tx, r, &prev, &entry)?);
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn compiled(t: TemplateId) -> AnyState {
        let tpl = template(t);
        AnyState::decode(t, &tpl.contract().compiled.bytecode[tpl.prefix.len()..tpl.prefix.len() + tpl.state_len]).unwrap()
    }

    #[test]
    fn roundtrip_and_rules() {
        let st = compiled(TemplateId::KobBid);
        let recs = vec![
            Record::order(0, &st, None, Some(1_790_000_000)),
            Record::X402 { reference: vec![7; 32] },
            Record::Note { text: "kob-web/1".into() },
            Record::Unknown { record_type: 0x90, value: vec![1, 2, 3] },
        ];
        let bytes = encode(&recs).unwrap();
        assert_eq!(&bytes[..5], b"KOB1\x04");
        // Trailing bytes after a record are rejected.
        let mut odd = encode(&[Record::order(0, &st, None, None)]).unwrap();
        odd.extend_from_slice(&[0; 3]);
        odd[6] += 3;
        assert!(decode(&odd).is_err());
        // Version 1 (the pre-v2.4 layout) and unknown versions are not accepted.
        for v in [1u8, 5, 0xff] {
            let mut x = bytes.clone();
            x[4] = v;
            assert!(decode(&x).is_err(), "{v}");
        }
        let p = decode(&bytes).unwrap().unwrap();
        assert_eq!(p.records, recs);
        // Unknown critical type rejected.
        let mut bad = bytes.clone();
        bad.extend_from_slice(&[0x33, 0, 0]);
        assert!(decode(&bad).is_err());
        // Truncation rejected.
        assert!(decode(&bytes[..bytes.len() - 1]).is_err());
        // X402 references are 1..=64 bytes and NOTE at most 64 bytes, on both sides.
        let rec = |ty: u8, v: &[u8]| {
            let mut b = MAGIC.to_vec();
            b.push(PAYLOAD_VERSION);
            b.push(ty);
            b.extend_from_slice(&(v.len() as u16).to_le_bytes());
            b.extend_from_slice(v);
            b
        };
        assert!(decode(&rec(REC_X402, &[])).is_err());
        assert!(decode(&rec(REC_X402, &[1; 65])).is_err());
        assert_eq!(decode(&rec(REC_X402, &[1; 64])).unwrap().unwrap().records, vec![Record::X402 { reference: vec![1; 64] }]);
        assert!(encode(&[Record::X402 { reference: vec![] }]).is_err());
        assert!(encode(&[Record::X402 { reference: vec![1; 65] }]).is_err());
        assert!(decode(&rec(REC_NOTE, &[b'a'; 65])).is_err());
        // A NOTE that is not UTF-8 does not fail the payload: it is kept (and re-encoded) as an unknown optional record.
        let mut with_order = encode(&[Record::order(0, &st, None, None)]).unwrap();
        with_order.extend_from_slice(&rec(REC_NOTE, &[0xff, 0xfe])[5..]);
        let p = decode(&with_order).unwrap().unwrap();
        assert!(matches!(p.records[0], Record::Order { .. }));
        assert_eq!(p.records[1], Record::Unknown { record_type: REC_NOTE, value: vec![0xff, 0xfe] });
        assert_eq!(encode(&p.records).unwrap(), with_order);
        // The encoder refuses what a decoder refuses: another template hash, another state length, a
        // state that does not decode canonically.
        let Record::Order { output, family, template: t, template_hash, state, custody, deadline, .. } =
            Record::order(0, &st, None, None)
        else {
            panic!()
        };
        let order = |h: [u8; 32], s: Vec<u8>| Record::Order {
            output,
            family,
            template: t,
            template_hash: h,
            state: s,
            custody: custody.clone(),
            prefund: None,
            deadline,
        };
        assert!(encode(&[order(template_hash, state.clone())]).is_ok());
        let mut h = template_hash;
        h[0] ^= 1;
        assert!(encode(&[order(h, state.clone())]).is_err());
        assert!(encode(&[order(template_hash, state[..state.len() - 1].to_vec())]).is_err());
        let mut bad_state = state.clone();
        bad_state[0] = 0x4c; // not the canonical 32-byte push that opens the state
        assert!(encode(&[order(template_hash, bad_state)]).is_err());
        // Legacy x402.
        let p = decode(b"X402:0a0b").unwrap().unwrap();
        assert!(p.legacy);
        assert_eq!(p.records, vec![Record::X402 { reference: vec![10, 11] }]);
        assert!(decode(b"other").unwrap().is_none());
        // A payload without order records stays version 2 (the x402 profile's commitment payload, byte for byte).
        let x = encode(&[Record::X402 { reference: vec![7; 32] }, Record::Note { text: "n".into() }]).unwrap();
        assert_eq!(&x[..5], b"KOB1\x02");
        assert_eq!(decode(&x).unwrap().unwrap().version, 2);
    }

    #[test]
    fn varints_are_canonical() {
        for x in [0u64, 1, 127, 128, 300, 16_383, 16_384, u32::MAX as u64, u64::MAX] {
            let mut v = vec![];
            put_varint(&mut v, x);
            let mut p = 0;
            assert_eq!(get_varint(&v, &mut p, "x").unwrap(), x);
            assert_eq!(p, v.len());
        }
        let bad: [&[u8]; 5] = [
            &[0x80, 0x00],                                                       // trailing zero group
            &[0xff, 0x00],                                                       // trailing zero group
            &[0x80],                                                             // truncated
            &[0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0x02],       // beyond 64 bits
            &[0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0x81, 0x00], // eleven bytes
        ];
        for b in bad {
            assert!(get_varint(b, &mut 0, "x").is_err(), "{b:02x?}");
        }
    }

    /// The compact state of every pinned order template round-trips losslessly and is shorter than the span.
    #[test]
    fn every_template_state_is_a_push_layout_and_compacts_losslessly() {
        for tpl in TemplateId::ALL.into_iter().filter(|t| t.kind_code().is_some()).map(template) {
            let span = tpl.contract().compiled.bytecode[tpl.prefix.len()..tpl.prefix.len() + tpl.state_len].to_vec();
            assert_eq!(state_layout_of(tpl).iter().map(|(h, n)| h.len() + n).sum::<usize>(), tpl.state_len);
            let mut v = vec![];
            put_compact_state(&mut v, tpl, &span).unwrap();
            let mut p = 0;
            assert_eq!(get_compact_state(&v, &mut p, tpl).unwrap(), span, "{}", tpl.contract_name);
            assert_eq!(p, v.len());
            assert!(v.len() < span.len(), "{}: {} -> {}", tpl.contract_name, span.len(), v.len());
        }
    }

    #[test]
    fn compact_order_record_rules() {
        let st = compiled(TemplateId::KobAsk);
        let custody = Some(Custody { token_output: 1, extension_commitment: [0xee; 32] });
        let rec = Record::order(0, &st, custody.clone(), Some(1_790_726_400));
        let bytes = encode(std::slice::from_ref(&rec)).unwrap();
        assert_eq!(decode(&bytes).unwrap().unwrap().records, vec![rec.clone()]);
        // The record carries no template hash and no push opcodes: well under the version-2 layout (2 + 1 + 1 + 32 + 2 +
        // 243 + 34 + 8 bytes).
        assert!(bytes.len() - 8 < 323 - 8 - 32 - 19, "{}", bytes.len());
        // A zero extension commitment is a flag; spelled out it is refused (one encoding per record).
        let zero = Record::order(0, &st, Some(Custody { token_output: 1, extension_commitment: [0; 32] }), None);
        let zb = encode(std::slice::from_ref(&zero)).unwrap();
        assert_eq!(decode(&zb).unwrap().unwrap().records, vec![zero]);
        let flags_at = 5 + 3 + 1 + 1 + 1; // magic+version, type+len, output, family, kind
        assert_eq!(zb[flags_at], FLAG_EXT_ZERO);
        let mut spelled = zb.clone();
        spelled[flags_at] = 0;
        spelled.extend_from_slice(&[0; 32]);
        let len = u16::from_le_bytes([spelled[6], spelled[7]]) + 32;
        spelled[6..8].copy_from_slice(&len.to_le_bytes());
        assert!(decode(&spelled).is_err());
        // Unknown flag bits, trailing bytes and a truncated record are refused.
        let mut f = bytes.clone();
        f[flags_at] |= 0x80;
        assert!(decode(&f).is_err());
        let mut t = bytes.clone();
        t.push(0);
        let len = u16::from_le_bytes([t[6], t[7]]) + 1;
        t[6..8].copy_from_slice(&len.to_le_bytes());
        assert!(decode(&t).is_err());
        assert!(decode(&bytes[..bytes.len() - 1]).is_err());
        // An AMEND record of version 2 is an unknown critical record.
        let am = encode(&[Record::amend(0, 0, &st, None)]).unwrap();
        assert_eq!(decode(&am).unwrap().unwrap().records, vec![Record::amend(0, 0, &st, None)]);
        let mut v2 = am.clone();
        v2[4] = PAYLOAD_VERSION_2;
        assert!(decode(&v2).is_err());
        // Only the plain ask and the plain bid are amended in place.
        let bid = compiled(TemplateId::KobBid);
        let bm = encode(&[Record::amend(0, 0, &bid, None)]).unwrap();
        assert_eq!(decode(&bm).unwrap().unwrap().records, vec![Record::amend(0, 0, &bid, None)]);
        let mut forged = am.clone();
        forged[11] = 0x03; // magic+version, type+len, output, input, family: the kind byte (KobCondAsk) is refused
        assert!(decode(&forged).unwrap_err().to_string().contains("not amended in place"));
    }

    /// A pair order's record names the family of its BASE token A (kind 0x08 / 0x09 / 0x0a in both families); each custody
    /// part is in the family of its own token (a KRON custody spells no extension commitment), and a sell-first KobIfdPair
    /// holding a prefund carries a second custody part.
    #[test]
    fn pair_records_name_the_base_family_and_their_custodies() {
        use crate::state::{IfdPairState, PairState};
        let AnyState::KobPair(x) = compiled(TemplateId::KobPair) else { panic!() };
        let custody =
            |fam: Family| Some(Custody { token_output: 1, extension_commitment: if fam == Family::Kron { [0; 32] } else { [5; 32] } });
        for (side, s_code, t_code) in [(1, 1, 1), (1, 2, 1), (2, 1, 2), (2, 2, 1)] {
            let s = AnyState::KobPair(PairState {
                side,
                s_family: s_code,
                t_family: t_code,
                t_ext: if t_code == 2 { [0; 32] } else { x.t_ext },
                ..x.clone()
            });
            let a_fam = Family::from_code(if side == 1 { s_code } else { t_code } as u8).unwrap();
            let s_fam = Family::from_code(s_code as u8).unwrap();
            assert_eq!(custody_families(&s), vec![s_fam]);
            let rec = Record::order(0, &s, custody(s_fam), None);
            let Record::Order { family, .. } = &rec else { panic!() };
            assert_eq!(*family, a_fam.code());
            let b = encode(std::slice::from_ref(&rec)).unwrap();
            assert_eq!(decode(&b).unwrap().unwrap().records, vec![rec.clone()]);
            // the other family byte is refused, by the encoder and by the decoder
            let Record::Order { output, template, template_hash, state, custody, deadline, .. } = rec else { panic!() };
            let other = if a_fam == Family::Kcc20 { Family::Kron } else { Family::Kcc20 };
            let forged = Record::Order {
                output,
                family: other.code(),
                template,
                template_hash,
                state: state.clone(),
                custody: custody.clone(),
                prefund: None,
                deadline,
            };
            assert!(encode(&[forged]).is_err());
            let mut bb = b.clone();
            bb[9] = other.code(); // magic+version, type+len, output: the family byte
            assert!(decode(&bb).is_err());
            // a missing or an extra custody part is refused
            let none = Record::Order {
                output,
                family: a_fam.code(),
                template,
                template_hash,
                state: state.clone(),
                custody: None,
                prefund: None,
                deadline,
            };
            assert!(encode(&[none]).is_err());
            let two = Record::Order {
                output,
                family: a_fam.code(),
                template,
                template_hash,
                state,
                custody: custody.clone(),
                prefund: custody,
                deadline,
            };
            assert!(encode(&[two]).is_err());
        }
        // a sell-first entry: its A custody, then its B prefund (when it holds one)
        let AnyState::KobIfdPair(e) = compiled(TemplateId::KobIfdPair) else { panic!() };
        let ask = AnyState::KobIfdPair(IfdPairState { side: 1, custody: 0, ..e.clone() });
        assert_eq!(custody_families(&ask).len(), 1);
        let with = AnyState::KobIfdPair(IfdPairState { side: 1, custody: 7, ..e.clone() });
        assert_eq!(custody_families(&with).len(), 2);
        let rec = Record::order_with_prefund(
            0,
            &with,
            custody(Family::Kcc20),
            Some(Custody { token_output: 2, extension_commitment: [0; 32] }),
            None,
        );
        let b = encode(std::slice::from_ref(&rec)).unwrap();
        assert_eq!(decode(&b).unwrap().unwrap().records, vec![rec]);
        assert!(encode(&[Record::order(0, &with, custody(Family::Kcc20), None)]).is_err());
        // a buy-first entry holds its B escrow only
        let bid = AnyState::KobIfdPair(IfdPairState { side: 2, custody: 7, ..e });
        assert_eq!(custody_families(&bid).len(), 1);
    }

    /// The numeric gate applies to every placement record: a scale that is not a power of ten, or an amount whose full
    /// fill at the order's price is worth 2^62 or more, is refused.
    #[test]
    fn records_pass_the_numeric_gate() {
        let AnyState::KobAsk(a) = compiled(TemplateId::KobAsk) else { panic!() };
        let ok = AnyState::KobAsk(crate::state::AskState { scale: 100, amount_left: 1_000, price: 5, ..a.clone() });
        let c = Some(Custody { token_output: 1, extension_commitment: [5; 32] });
        assert!(encode(&[Record::order(0, &ok, c.clone(), None)]).is_ok());
        let bad_scale = AnyState::KobAsk(crate::state::AskState { scale: 300, ..a.clone() });
        assert!(encode(&[Record::order(0, &bad_scale, c.clone(), None)]).is_err());
        let huge = AnyState::KobAsk(crate::state::AskState { scale: 1, amount_left: 1 << 40, price: 1 << 22, ..a });
        assert!(encode(&[Record::order(0, &huge, c, None)]).is_err());
    }

    /// An `ORDER` record of version 2 and every payload of version 3 name templates this build does not pin: they do not
    /// decode. A version-2 payload without order records (an x402 commitment, a note) still does, byte for byte.
    #[test]
    fn order_records_of_older_payload_versions_do_not_decode() {
        let st = compiled(TemplateId::KobAsk);
        let custody = Some(Custody { token_output: 1, extension_commitment: [0xee; 32] });
        let bytes = encode(&[Record::order(0, &st, custody, None)]).unwrap();
        let mut v3 = bytes.clone();
        v3[4] = 3;
        assert!(decode(&v3).unwrap_err().to_string().contains("unsupported KOB1 version 3"));
        let mut v2 = bytes.clone();
        v2[4] = PAYLOAD_VERSION_2;
        assert!(decode(&v2).unwrap_err().to_string().contains("does not pin"));
        let x402 = [Record::X402 { reference: vec![7; 32] }, Record::Note { text: "kob".into() }];
        let b = encode(&x402).unwrap();
        assert_eq!(b[4], PAYLOAD_VERSION_2);
        assert_eq!(decode(&b).unwrap().unwrap().records, x402);
    }
}
