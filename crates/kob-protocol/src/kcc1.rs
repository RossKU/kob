//! KCC-1 (Covenant Concepts, Byte Layouts, and ABI): the parts of the base specification KOB implements itself.
//!
//! Pinned to kaspanet/kccs `main` at `411b41bc14b3fda8f3a0548242c555f3597cad1a` (KCC-1 in Last Call since 2026-10-01,
//! compliance rewrite #27). The official vectors are vendored in `crates/kob-tests/vectors/kcc1/` and executed by
//! `crates/kob-tests/tests/kcc1_conformance_tests.rs`.
//!
//! KOB encodes its own programs' arguments and state through `silverscript-abi` (the vectors show it conforms). This
//! module adds what KOB needs beyond that encoder:
//!
//! * the hash notation `Hash(x)` / `Hash(x, key)` (section 3.2.1);
//! * the two push forms `PushMinimal` / `PushExplicit` (section 3.4.2) and the integer payloads (section 3.4.3);
//! * canonical type names, dispatch type names, `FunctionSignature` and dispatch tags recomputed from an ABI
//!   description (sections 3.4.1 and 3.5.1), so every committed artifact's tags can be checked;
//! * argument-type validation and a strict decoder of a complete argument push sequence (sections 3.4.2 to 3.4.6);
//! * a canonical state decoder: decode, then require that re-encoding reproduces the bytes (section 3.7.1).

use std::collections::{BTreeMap, VecDeque};

use kaspa_txscript::script_builder::ScriptBuilder;
use silverscript_abi::{
    decode_runtime_state_script, encode_runtime_state_script, ArtifactValue, FieldArtifact, RuntimeStateArtifact, SilAbiArtifact,
    SilContractArtifact, StructArtifact, TypeArtifact,
};

/// Largest KCC-1 `int`: `2^63 - 1`.
pub const INT_MAX: i64 = i64::MAX;
/// Smallest KCC-1 `int`: `-(2^63 - 1)` (signed magnitude has no `-2^63`).
pub const INT_MIN: i64 = -i64::MAX;

/// Errors of the KCC-1 helpers.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum Kcc1Error {
    #[error("type `{0}` is not a KCC-1 type")]
    NotKcc1Type(String),
    #[error("invalid type identifier `{0}`")]
    BadIdentifier(String),
    #[error("unknown record `{0}`")]
    UnknownRecord(String),
    #[error("cyclic record `{0}`")]
    CyclicRecord(String),
    #[error("{0}")]
    BadArgumentType(String),
    #[error("malformed push sequence: {0}")]
    MalformedPush(String),
    #[error("argument {0}: {1}")]
    BadArgument(usize, String),
    #[error("state: {0}")]
    BadState(String),
    #[error("duplicate dispatch tag {tag} for `{first}` and `{second}`")]
    DuplicateDispatchTag { tag: String, first: String, second: String },
    #[error("keyed hash key is {0} bytes, at most 32")]
    KeyTooLong(usize),
}

type R<T> = std::result::Result<T, Kcc1Error>;

// ------------------------------------------------------------------------------------------- hash (3.2.1)

/// `Hash(x)`: unkeyed BLAKE3-256.
pub fn hash(x: &[u8]) -> [u8; 32] {
    *blake3::hash(x).as_bytes()
}

/// `Key32(key) = key || 00^(32 - len(key))`; the key holds at most 32 bytes.
pub fn key32(key: &[u8]) -> R<[u8; 32]> {
    if key.len() > 32 {
        return Err(Kcc1Error::KeyTooLong(key.len()));
    }
    let mut k = [0u8; 32];
    k[..key.len()].copy_from_slice(key);
    Ok(k)
}

/// `Hash(x, key)`: BLAKE3 keyed mode under `Key32(key)`.
pub fn hash_keyed(x: &[u8], key: &[u8]) -> R<[u8; 32]> {
    Ok(*blake3::keyed_hash(&key32(key)?, x).as_bytes())
}

// ------------------------------------------------------------------------------------------ pushes (3.4.2)

/// `PushMinimal(b)`: `OP_0` for empty, `OP_1..OP_16` / `OP_1NEGATE` for the one-byte numbers, else a length-based push.
pub fn push_minimal(b: &[u8]) -> Vec<u8> {
    ScriptBuilder::new().add_data(b).expect("push within the script size limit").drain()
}

/// `PushExplicit(b)`: `OP_0` for empty, a length-based push for every other payload.
pub fn push_explicit(b: &[u8]) -> Vec<u8> {
    ScriptBuilder::new().add_data_with_push_opcode(b).expect("push within the script size limit").drain()
}

// ---------------------------------------------------------------------------------------- integers (3.4.3)

/// Minimal ScriptNum bytes of an `int` (the standalone-argument payload). `None` outside the KCC-1 range.
pub fn int_argument_payload(v: i64) -> Option<Vec<u8>> {
    if v < INT_MIN {
        return None;
    }
    if v == 0 {
        return Some(vec![]);
    }
    let neg = v < 0;
    let mut m = v.unsigned_abs();
    let mut out = vec![];
    while m > 0 {
        out.push((m & 0xff) as u8);
        m >>= 8;
    }
    if out.last().expect("non-empty") & 0x80 != 0 {
        out.push(if neg { 0x80 } else { 0x00 });
    } else if neg {
        *out.last_mut().expect("non-empty") |= 0x80;
    }
    Some(out)
}

/// Decodes a standalone-argument `int` payload: minimal ScriptNum (no redundant sign byte, no negative zero), at most
/// eight bytes, inside the KCC-1 range.
pub fn int_from_argument_payload(b: &[u8]) -> Option<i64> {
    if b.is_empty() {
        return Some(0);
    }
    if b.len() > 8 {
        return None;
    }
    let last = *b.last().expect("non-empty");
    if last & 0x7f == 0 && (b.len() == 1 || b[b.len() - 2] & 0x80 == 0) {
        return None;
    }
    let mut m: u64 = 0;
    for (i, x) in b.iter().enumerate() {
        let x = if i == b.len() - 1 { x & 0x7f } else { *x };
        m |= (x as u64) << (8 * i);
    }
    let m = i64::try_from(m).ok()?;
    Some(if last & 0x80 != 0 { -m } else { m })
}

/// Eight-byte little-endian signed-magnitude `int` (state and fixed-width payload). `None` outside the KCC-1 range.
pub fn int_state_payload(v: i64) -> Option<[u8; 8]> {
    if v < INT_MIN {
        return None;
    }
    let mut b = v.unsigned_abs().to_le_bytes();
    if v < 0 {
        b[7] |= 0x80;
    }
    Some(b)
}

/// Decodes an eight-byte signed-magnitude `int` payload. Negative zero is not canonical (`None`).
pub fn int_from_state_payload(b: &[u8]) -> Option<i64> {
    let b: [u8; 8] = b.try_into().ok()?;
    let neg = b[7] & 0x80 != 0;
    let mut m = b;
    m[7] &= 0x7f;
    let m = i64::from_le_bytes(m);
    match (neg, m) {
        (true, 0) => None,
        (true, m) => Some(-m),
        (false, m) => Some(m),
    }
}

// ------------------------------------------------------------------------------------ type names (3.4.1)

/// Record definitions by name (ordered fields).
pub type Records = BTreeMap<String, StructArtifact>;

/// The records an entry of `contract` can name: the artifact's structs plus the runtime state as `State` (the
/// silverscript convention for `State[]` parameters).
pub fn contract_records(abi: &SilAbiArtifact, contract: &SilContractArtifact) -> Records {
    let mut r = abi.structs.clone();
    r.entry("State".to_string()).or_insert_with(|| StructArtifact {
        fields: contract.runtime_state.fields.iter().map(|f| FieldArtifact { name: f.name.clone(), ty: f.ty.clone() }).collect(),
    });
    r
}

fn is_identifier(s: &str) -> bool {
    let mut c = s.chars();
    matches!(c.next(), Some(x) if x.is_ascii_alphabetic() || x == '_') && c.all(|x| x.is_ascii_alphanumeric() || x == '_')
}

/// `TypeName(T)`. SilverScript's `bytes` kind is KCC-1 `byte[]`; its `temporal` kind has no KCC-1 type name.
pub fn type_name(ty: &TypeArtifact) -> R<String> {
    Ok(match ty {
        TypeArtifact::Int => "int".into(),
        TypeArtifact::Bool => "bool".into(),
        TypeArtifact::Byte => "byte".into(),
        TypeArtifact::Text => "string".into(),
        TypeArtifact::Pubkey => "pubkey".into(),
        TypeArtifact::Sig => "sig".into(),
        TypeArtifact::Datasig => "datasig".into(),
        TypeArtifact::Bytes => "byte[]".into(),
        TypeArtifact::FixedBytes { len } => format!("byte[{len}]"),
        TypeArtifact::FixedArray { item, len } => format!("{}[{len}]", type_name(item)?),
        TypeArtifact::DynamicArray { item } => format!("{}[]", type_name(item)?),
        TypeArtifact::Struct { name } => {
            if !is_identifier(name) {
                return Err(Kcc1Error::BadIdentifier(name.clone()));
            }
            name.clone()
        }
        TypeArtifact::Temporal => return Err(Kcc1Error::NotKcc1Type("temporal".into())),
    })
}

/// Parses a KCC-1 type name (`int`, `byte[32]`, `bool[3]`, `int[]`, `Point[2]`, `byte[]` ...). A name that is not a
/// scalar is a record name.
pub fn parse_type_name(s: &str) -> R<TypeArtifact> {
    let base_end = s.find('[').unwrap_or(s.len());
    let base = &s[..base_end];
    if !is_identifier(base) {
        return Err(Kcc1Error::BadIdentifier(s.to_string()));
    }
    let mut dims: Vec<Option<usize>> = vec![];
    let mut rest = &s[base_end..];
    while !rest.is_empty() {
        let close = rest.find(']').ok_or_else(|| Kcc1Error::BadIdentifier(s.to_string()))?;
        if !rest.starts_with('[') {
            return Err(Kcc1Error::BadIdentifier(s.to_string()));
        }
        let n = &rest[1..close];
        dims.push(if n.is_empty() {
            None
        } else {
            if !(n == "0" || (!n.starts_with('0') && n.bytes().all(|b| b.is_ascii_digit()))) {
                return Err(Kcc1Error::BadIdentifier(s.to_string()));
            }
            Some(n.parse().map_err(|_| Kcc1Error::BadIdentifier(s.to_string()))?)
        });
        rest = &rest[close + 1..];
    }
    let mut ty = match base {
        "int" => TypeArtifact::Int,
        "bool" => TypeArtifact::Bool,
        "byte" => TypeArtifact::Byte,
        "string" => TypeArtifact::Text,
        "pubkey" => TypeArtifact::Pubkey,
        "sig" => TypeArtifact::Sig,
        "datasig" => TypeArtifact::Datasig,
        _ => TypeArtifact::Struct { name: base.to_string() },
    };
    for (i, d) in dims.into_iter().enumerate() {
        ty = match (i, &ty, d) {
            (0, TypeArtifact::Byte, Some(len)) => TypeArtifact::FixedBytes { len },
            (0, TypeArtifact::Byte, None) => TypeArtifact::Bytes,
            (_, _, Some(len)) => TypeArtifact::FixedArray { item: Box::new(ty), len },
            (_, _, None) => TypeArtifact::DynamicArray { item: Box::new(ty) },
        };
    }
    Ok(ty)
}

fn record<'a>(records: &'a Records, name: &str) -> R<&'a StructArtifact> {
    records.get(name).ok_or_else(|| Kcc1Error::UnknownRecord(name.to_string()))
}

// ---------------------------------------------------------------------------------- dispatch tags (3.5.1)

/// `DispatchTypeName(T)`: record names and field names omitted, `{T_1,...,T_n}` for a record.
pub fn dispatch_type_name(ty: &TypeArtifact, records: &Records) -> R<String> {
    fn go(ty: &TypeArtifact, records: &Records, depth: usize) -> R<String> {
        if depth > 64 {
            return Err(Kcc1Error::CyclicRecord(format!("{ty:?}")));
        }
        Ok(match ty {
            TypeArtifact::Struct { name } => {
                let r = record(records, name)?;
                let inner = r.fields.iter().map(|f| go(&f.ty, records, depth + 1)).collect::<R<Vec<_>>>()?;
                format!("{{{}}}", inner.join(","))
            }
            TypeArtifact::FixedArray { item, len } => format!("{}[{len}]", go(item, records, depth + 1)?),
            TypeArtifact::DynamicArray { item } => format!("{}[]", go(item, records, depth + 1)?),
            other => type_name(other)?,
        })
    }
    go(ty, records, 0)
}

/// `FunctionSignature = UTF8("{name}({dispatch type names})")`.
pub fn function_signature(name: &str, params: &[TypeArtifact], records: &Records) -> R<String> {
    let names = params.iter().map(|t| dispatch_type_name(t, records)).collect::<R<Vec<_>>>()?;
    Ok(format!("{name}({})", names.join(",")))
}

/// `dispatch_tag = Hash(FunctionSignature)[0:4]`.
pub fn dispatch_tag(function_signature: &str) -> [u8; 4] {
    hash(function_signature.as_bytes())[..4].try_into().expect("4 bytes")
}

/// One entry's dispatch tag recomputed from its ABI types: `(entry, FunctionSignature, recomputed tag, artifact tag)`.
pub type RecomputedTag = (String, String, [u8; 4], [u8; 4]);

/// Recomputes the dispatch tag of every entry of `contract` from its ABI types.
pub fn contract_dispatch_tags(abi: &SilAbiArtifact, contract: &SilContractArtifact) -> R<Vec<RecomputedTag>> {
    let records = contract_records(abi, contract);
    contract
        .entries
        .iter()
        .map(|(name, e)| {
            let params: Vec<TypeArtifact> = e.params.iter().map(|p| p.ty.clone()).collect();
            let sig = function_signature(name, &params, &records)?;
            let tag = dispatch_tag(&sig);
            Ok((name.clone(), sig, tag, e.dispatch_tag.into_bytes()))
        })
        .collect()
}

/// A program's entrypoints together: every dispatch tag distinct (identical preimages and truncated collisions both
/// rejected). `entries` are `(name, argument types)`.
pub fn check_entrypoints(entries: &[(String, Vec<TypeArtifact>)], records: &Records) -> R<()> {
    let mut seen: BTreeMap<[u8; 4], String> = BTreeMap::new();
    for (name, params) in entries {
        for p in params {
            validate_argument_type(p, records)?;
        }
        let sig = function_signature(name, params, records)?;
        let tag = dispatch_tag(&sig);
        if let Some(first) = seen.insert(tag, sig.clone()) {
            return Err(Kcc1Error::DuplicateDispatchTag { tag: faster_hex::hex_string(&tag), first, second: sig });
        }
    }
    Ok(())
}

// ------------------------------------------------------------------------------- widths and validation

/// Fixed payload width of a type in array / packed context, `None` when variable.
pub fn fixed_width(ty: &TypeArtifact) -> Option<usize> {
    match ty {
        TypeArtifact::Int => Some(8),
        TypeArtifact::Bool | TypeArtifact::Byte => Some(1),
        TypeArtifact::Pubkey => Some(32),
        TypeArtifact::Sig => Some(65),
        TypeArtifact::Datasig => Some(64),
        TypeArtifact::FixedBytes { len } => Some(*len),
        TypeArtifact::FixedArray { item, len } => fixed_width(item)?.checked_mul(*len),
        _ => None,
    }
}

/// One lowered argument leaf: its type and, inside an array of records, the array it belongs to.
#[derive(Clone, Debug)]
struct Leaf {
    ty: TypeArtifact,
    /// `(group id, fixed length)` for a field group of an array of records.
    group: Option<(usize, Option<usize>)>,
}

fn record_leaves(name: &str, records: &Records, depth: usize, out: &mut Vec<TypeArtifact>) -> R<()> {
    if depth > 64 {
        return Err(Kcc1Error::CyclicRecord(name.to_string()));
    }
    for f in &record(records, name)?.fields {
        match &f.ty {
            TypeArtifact::Struct { name } => record_leaves(name, records, depth + 1, out)?,
            TypeArtifact::FixedArray { item, .. } | TypeArtifact::DynamicArray { item } if contains_record(item) => {
                return Err(Kcc1Error::BadArgumentType(format!("array of records nested in the array of records `{name}`")))
            }
            t => out.push(t.clone()),
        }
    }
    Ok(())
}

fn contains_record(ty: &TypeArtifact) -> bool {
    match ty {
        TypeArtifact::Struct { .. } => true,
        TypeArtifact::FixedArray { item, .. } | TypeArtifact::DynamicArray { item } => contains_record(item),
        _ => false,
    }
}

/// Checks that `ty` is a permitted KCC-1 argument (and state) type: KCC-1 scalars, records whose names resolve, dynamic
/// arrays of positive fixed width, arrays of records whose every lowered leaf has a positive fixed width.
pub fn validate_argument_type(ty: &TypeArtifact, records: &Records) -> R<()> {
    fn go(ty: &TypeArtifact, records: &Records, depth: usize) -> R<()> {
        if depth > 64 {
            return Err(Kcc1Error::CyclicRecord(format!("{ty:?}")));
        }
        type_name(ty)?;
        match ty {
            TypeArtifact::Struct { name } => {
                for f in &record(records, name)?.fields {
                    go(&f.ty, records, depth + 1)?;
                }
                Ok(())
            }
            TypeArtifact::FixedArray { item, .. } | TypeArtifact::DynamicArray { item } => {
                let dynamic = matches!(ty, TypeArtifact::DynamicArray { .. });
                if let TypeArtifact::Struct { name } = item.as_ref() {
                    go(item, records, depth + 1)?;
                    let mut leaves = vec![];
                    record_leaves(name, records, depth + 1, &mut leaves)?;
                    if leaves.is_empty() {
                        return Err(Kcc1Error::BadArgumentType(format!("array of the empty record `{name}`")));
                    }
                    for l in leaves {
                        if fixed_width(&l).is_none_or(|w| w == 0) {
                            return Err(Kcc1Error::BadArgumentType(format!(
                                "array of records `{name}`: lowered leaf `{}` has no positive fixed width",
                                type_name(&l)?
                            )));
                        }
                    }
                    return Ok(());
                }
                if contains_record(item) {
                    return Err(Kcc1Error::BadArgumentType("array of arrays of records".into()));
                }
                match fixed_width(item) {
                    Some(w) if w > 0 || !dynamic => Ok(()),
                    _ => Err(Kcc1Error::BadArgumentType(format!(
                        "`{}`: array elements need a {}fixed payload width",
                        type_name(ty)?,
                        if dynamic { "positive " } else { "" }
                    ))),
                }
            }
            _ => Ok(()),
        }
    }
    go(ty, records, 0)
}

fn lower(ty: &TypeArtifact, records: &Records, groups: &mut usize, out: &mut Vec<Leaf>) -> R<()> {
    match ty {
        TypeArtifact::Struct { name } => {
            for f in &record(records, name)?.fields {
                lower(&f.ty, records, groups, out)?;
            }
        }
        TypeArtifact::FixedArray { item, .. } | TypeArtifact::DynamicArray { item }
            if matches!(item.as_ref(), TypeArtifact::Struct { .. }) =>
        {
            let TypeArtifact::Struct { name } = item.as_ref() else { unreachable!() };
            let len = match ty {
                TypeArtifact::FixedArray { len, .. } => Some(*len),
                _ => None,
            };
            let g = *groups;
            *groups += 1;
            let mut leaves = vec![];
            record_leaves(name, records, 0, &mut leaves)?;
            out.extend(leaves.into_iter().map(|l| Leaf { ty: l, group: Some((g, len)) }));
        }
        t => out.push(Leaf { ty: t.clone(), group: None }),
    }
    Ok(())
}

// ----------------------------------------------------------------------------------- argument decoding

/// One push of a push-only script: its payload and its exact bytes.
struct Push<'a> {
    payload: Vec<u8>,
    raw: &'a [u8],
}

fn parse_pushes(script: &[u8]) -> R<Vec<Push<'_>>> {
    let bad = |m: String| Kcc1Error::MalformedPush(m);
    let mut out = vec![];
    let mut i = 0usize;
    while i < script.len() {
        let start = i;
        let op = script[i];
        i += 1;
        let len = match op {
            0x00 => 0,
            0x01..=0x4b => op as usize,
            0x4c..=0x4e => {
                let n = 1usize << (op - 0x4c);
                let b = script.get(i..i + n).ok_or_else(|| bad(format!("truncated length at offset {start}")))?;
                i += n;
                let mut le = [0u8; 4];
                le[..n].copy_from_slice(b);
                u32::from_le_bytes(le) as usize
            }
            0x4f => {
                out.push(Push { payload: vec![0x81], raw: &script[start..i] });
                continue;
            }
            0x51..=0x60 => {
                out.push(Push { payload: vec![op - 0x50], raw: &script[start..i] });
                continue;
            }
            _ => return Err(bad(format!("opcode {op:#04x} at offset {start} is not a push"))),
        };
        let end = i.checked_add(len).filter(|e| *e <= script.len()).ok_or_else(|| bad(format!("truncated push at offset {start}")))?;
        out.push(Push { payload: script[i..end].to_vec(), raw: &script[start..end] });
        i = end;
    }
    Ok(out)
}

/// Decodes one fixed-width element payload (array element, record-array field element, packed value).
pub fn decode_element(ty: &TypeArtifact, b: &[u8]) -> Result<ArtifactValue, String> {
    let need = |n: usize| {
        if b.len() == n {
            Ok(())
        } else {
            Err(format!("{} needs {n} bytes, got {}", type_name(ty).unwrap_or_default(), b.len()))
        }
    };
    Ok(match ty {
        TypeArtifact::Int => {
            ArtifactValue::Int(int_from_state_payload(b).ok_or("int payload is not 8-byte canonical signed magnitude")?)
        }
        TypeArtifact::Bool => {
            need(1)?;
            match b[0] {
                0 => ArtifactValue::Bool(false),
                1 => ArtifactValue::Bool(true),
                x => return Err(format!("bool payload {x:#04x} is not 00 or 01")),
            }
        }
        TypeArtifact::Byte => {
            need(1)?;
            ArtifactValue::Byte(b[0])
        }
        TypeArtifact::Pubkey | TypeArtifact::Sig | TypeArtifact::Datasig | TypeArtifact::FixedBytes { .. } => {
            need(fixed_width(ty).expect("fixed"))?;
            ArtifactValue::Bytes(b.to_vec())
        }
        TypeArtifact::FixedArray { item, len } => {
            let w = fixed_width(item).ok_or("array element has no fixed width")?;
            need(w * len)?;
            ArtifactValue::Array(if w == 0 {
                (0..*len).map(|_| decode_element(item, &[])).collect::<Result<_, _>>()?
            } else {
                b.chunks_exact(w).map(|c| decode_element(item, c)).collect::<Result<_, _>>()?
            })
        }
        other => return Err(format!("`{other:?}` has no fixed-width element encoding")),
    })
}

/// Decodes an array payload of `item` elements (`count` = the declared length of a fixed array).
fn decode_array(item: &TypeArtifact, count: Option<usize>, b: &[u8]) -> Result<Vec<ArtifactValue>, String> {
    let w = fixed_width(item).ok_or("array element has no fixed width")?;
    if w == 0 {
        return match count {
            Some(n) if b.is_empty() => (0..n).map(|_| decode_element(item, &[])).collect(),
            _ => Err("zero-width array elements".into()),
        };
    }
    if !b.len().is_multiple_of(w) {
        return Err(format!("array payload of {} bytes is not a multiple of the element width {w}", b.len()));
    }
    if let Some(n) = count {
        if b.len() / w != n {
            return Err(format!("array holds {} elements, the type declares {n}", b.len() / w));
        }
    }
    b.chunks_exact(w).map(|c| decode_element(item, c)).collect()
}

/// Decodes one standalone (non-grouped) argument leaf.
fn decode_leaf(ty: &TypeArtifact, p: &[u8]) -> Result<ArtifactValue, String> {
    Ok(match ty {
        TypeArtifact::Int => {
            ArtifactValue::Int(int_from_argument_payload(p).ok_or("int is not a minimal ScriptNum in the KCC-1 range")?)
        }
        TypeArtifact::Bool => match p {
            [] => ArtifactValue::Bool(false),
            [1] => ArtifactValue::Bool(true),
            _ => return Err("a standalone bool must be OP_0 or OP_1".into()),
        },
        TypeArtifact::Byte => match p {
            [x] => ArtifactValue::Byte(*x),
            _ => return Err(format!("a byte needs exactly one payload byte, got {}", p.len())),
        },
        TypeArtifact::Text => ArtifactValue::Text(String::from_utf8(p.to_vec()).map_err(|e| format!("string is not UTF-8: {e}"))?),
        TypeArtifact::Bytes => ArtifactValue::Bytes(p.to_vec()),
        TypeArtifact::Pubkey | TypeArtifact::Sig | TypeArtifact::Datasig | TypeArtifact::FixedBytes { .. } => decode_element(ty, p)?,
        TypeArtifact::FixedArray { item, len } => ArtifactValue::Array(decode_array(item, Some(*len), p)?),
        TypeArtifact::DynamicArray { item } => ArtifactValue::Array(decode_array(item, None, p)?),
        other => return Err(format!("`{other:?}` is not a KCC-1 argument leaf")),
    })
}

fn rebuild(ty: &TypeArtifact, records: &Records, leaves: &mut VecDeque<ArtifactValue>) -> R<ArtifactValue> {
    match ty {
        TypeArtifact::Struct { name } => {
            let mut m = BTreeMap::new();
            for f in &record(records, name)?.fields {
                m.insert(f.name.clone(), rebuild(&f.ty, records, leaves)?);
            }
            Ok(ArtifactValue::Object(m))
        }
        TypeArtifact::FixedArray { item, .. } | TypeArtifact::DynamicArray { item }
            if matches!(item.as_ref(), TypeArtifact::Struct { .. }) =>
        {
            let TypeArtifact::Struct { name } = item.as_ref() else { unreachable!() };
            let mut n_leaves = vec![];
            record_leaves(name, records, 0, &mut n_leaves)?;
            let cols: Vec<Vec<ArtifactValue>> = (0..n_leaves.len())
                .map(|_| match leaves.pop_front() {
                    Some(ArtifactValue::Array(v)) => Ok(v),
                    _ => Err(Kcc1Error::BadState("record-array group missing".into())),
                })
                .collect::<R<_>>()?;
            let n = cols.first().map_or(0, Vec::len);
            let mut out = Vec::with_capacity(n);
            for i in 0..n {
                let mut row: VecDeque<ArtifactValue> = cols.iter().map(|c| c[i].clone()).collect();
                out.push(rebuild(item, records, &mut row)?);
            }
            Ok(ArtifactValue::Array(out))
        }
        _ => leaves.pop_front().ok_or_else(|| Kcc1Error::BadState("missing leaf".into())),
    }
}

/// Decodes `script` as the complete canonical push sequence of `types` (no dispatch tag, no redeem script): every
/// type validated, every push the exact `PushMinimal` of its payload, every payload valid for its lowered type, the
/// field groups of each array of records holding the same element count (the declared one for a fixed array), and no
/// push missing or left over.
pub fn decode_arguments(types: &[TypeArtifact], records: &Records, script: &[u8]) -> R<Vec<ArtifactValue>> {
    for t in types {
        validate_argument_type(t, records)?;
    }
    let mut leaves = vec![];
    let mut groups = 0usize;
    let mut leaf_arg = vec![];
    for (i, t) in types.iter().enumerate() {
        let before = leaves.len();
        lower(t, records, &mut groups, &mut leaves)?;
        leaf_arg.extend(std::iter::repeat_n(i, leaves.len() - before));
    }
    let pushes = parse_pushes(script)?;
    if pushes.len() != leaves.len() {
        return Err(Kcc1Error::MalformedPush(format!("{} pushes for {} lowered arguments", pushes.len(), leaves.len())));
    }
    let mut decoded = VecDeque::with_capacity(leaves.len());
    let mut group_count: BTreeMap<usize, usize> = BTreeMap::new();
    for ((leaf, push), arg) in leaves.iter().zip(&pushes).zip(&leaf_arg) {
        if push_minimal(&push.payload) != push.raw {
            return Err(Kcc1Error::BadArgument(*arg, "push is not the PushMinimal form of its payload".into()));
        }
        let v = match leaf.group {
            None => decode_leaf(&leaf.ty, &push.payload),
            Some((g, len)) => decode_array(&leaf.ty, len, &push.payload).and_then(|v| {
                let n = *group_count.entry(g).or_insert(v.len());
                if n == v.len() {
                    Ok(ArtifactValue::Array(v))
                } else {
                    Err(format!("record-array field groups hold {n} and {} elements", v.len()))
                }
            }),
        }
        .map_err(|e| Kcc1Error::BadArgument(*arg, e))?;
        decoded.push_back(v);
    }
    types.iter().map(|t| rebuild(t, records, &mut decoded)).collect()
}

// ------------------------------------------------------------------------------------ P2SH envelope (3.6)

/// A spending input's signature script split by the KCC-1 layout `PushArguments(arguments) || OP_DATA_4 dispatch_tag ||
/// PushMinimal(R)`: push-only, the dispatch tag pushed with `OP_DATA_4`, the redeem script the final push in its
/// `PushMinimal` form. Returns `(argument pushes, dispatch tag, R)`.
pub fn split_invocation(signature_script: &[u8]) -> R<(&[u8], [u8; 4], Vec<u8>)> {
    let pushes = parse_pushes(signature_script)?;
    let n = pushes.len();
    if n < 2 {
        return Err(Kcc1Error::MalformedPush("an invocation needs a dispatch tag and a redeem script".into()));
    }
    let (tag, redeem) = (&pushes[n - 2], &pushes[n - 1]);
    if tag.raw.len() != 5 || tag.raw[0] != 0x04 {
        return Err(Kcc1Error::MalformedPush("the dispatch tag must be pushed as OP_DATA_4 and four bytes".into()));
    }
    if push_minimal(&redeem.payload) != redeem.raw {
        return Err(Kcc1Error::MalformedPush("the redeem script push is not PushMinimal(R)".into()));
    }
    let args_len = signature_script.len() - tag.raw.len() - redeem.raw.len();
    Ok((&signature_script[..args_len], tag.payload[..].try_into().expect("4 bytes"), redeem.payload.clone()))
}

// --------------------------------------------------------------------------------------- state (3.7.1)

/// Decodes a complete encoded state against a runtime-state layout and requires the canonical encoding: every lowered
/// field one push, the consumed bytes exactly `PushExplicit(StatePayload(T, v))` (re-encoding reproduces them), no
/// field missing, no trailing bytes.
pub fn decode_state(abi: &SilAbiArtifact, layout: &RuntimeStateArtifact, encoded: &[u8]) -> R<BTreeMap<String, ArtifactValue>> {
    let records = abi.structs.clone();
    for f in &layout.fields {
        validate_argument_type(&f.ty, &records).map_err(|e| Kcc1Error::BadState(format!("field `{}`: {e}", f.name)))?;
    }
    let values = decode_runtime_state_script(abi, layout, encoded).map_err(|e| Kcc1Error::BadState(e.to_string()))?;
    let again = encode_runtime_state_script(abi, layout, &values).map_err(|e| Kcc1Error::BadState(e.to_string()))?;
    if again != encoded {
        return Err(Kcc1Error::BadState("non-canonical encoding (re-encoding the decoded values differs)".into()));
    }
    Ok(values)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ints_round_trip_at_the_edges() {
        for v in [0, 1, -1, 16, 127, -127, 128, -128, 255, 256, -32768, INT_MAX, INT_MIN] {
            let a = int_argument_payload(v).unwrap();
            assert_eq!(int_from_argument_payload(&a), Some(v), "{v}");
            let s = int_state_payload(v).unwrap();
            assert_eq!(int_from_state_payload(&s), Some(v), "{v}");
        }
        assert_eq!(int_argument_payload(i64::MIN), None);
        assert_eq!(int_state_payload(i64::MIN), None);
        assert_eq!(int_from_argument_payload(&[0x80]), None, "negative zero");
        assert_eq!(int_from_argument_payload(&[0x01, 0x00]), None, "redundant sign byte");
        assert_eq!(int_from_state_payload(&[0, 0, 0, 0, 0, 0, 0, 0x80]), None, "negative zero");
    }

    #[test]
    fn type_names_parse_back() {
        for s in ["int", "byte[32]", "bool[3]", "int[]", "byte[]", "Point[2]", "byte[0][]", "byte[4][2]"] {
            assert_eq!(type_name(&parse_type_name(s).unwrap()).unwrap(), s);
        }
        assert!(parse_type_name("byte[01]").is_err());
        assert!(parse_type_name("9x").is_err());
        assert!(type_name(&TypeArtifact::Temporal).is_err());
    }
}
