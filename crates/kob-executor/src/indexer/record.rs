//! Extracted transaction records: the only chain data the indexer keeps.
//!
//! A raw VSPC response is about 15 GB per day on TN10 at `High` verbosity (signature scripts carry
//! whole KCC-20 programs). Nothing of it is stored. For every transaction that touches KOB the
//! extractor keeps a *reduced transaction*: outpoints, output values and covenant bindings, the
//! KOB1 placement payload, and, for every spent KOB order input, the revealed template,
//! its state span, the entry that was called and the small arguments. Signature scripts, token
//! programs and everything unrelated to KOB are dropped. The reduced form is exactly what the
//! processor consumes, so the live path and a rebuild from the permanent record log run the same code.
//!
//! Relevance (what is kept) is decided against the database at extraction time:
//! * the payload is a KOB1 placement record (or claims to be and does not decode: kept as a reject),
//! * an input spends a tracked order or token UTXO,
//! * a token output is owned (scheme `0x04`) by a tracked or freshly created order id: custody
//!   after a fill, a delivery to an if-done exit, or a stray (`docs/spec/matcher.md` §1.2). The
//!   token output states are read from the transfer leader's `next_states` argument and each one
//!   is checked against the output's script public key,
//! * an output of a TRACKED token (allowlisted; every token an order trades when the allowlist is not required) whose
//!   state the transfer leader reveals and the output's script public key proves: a *holding* (`HeldOut`), whoever
//!   owns it; an input that spends a tracked holding is relevant too (it marks the holding spent).

use super::db::DbResult;
use crate::hex::Hash32;
use crate::model;
use crate::rpc::types::{Tx, TxInput};
use crate::script::{p2sh_spk, parse_pushes, parse_spk};
use crate::wire::{Reader, WireError, WireResult, Writer};
use kob_protocol::artifacts::{template, token_template, TemplateId};
use kob_protocol::family::Family;
use kob_protocol::payload::{self, Record};
use kob_protocol::state::{AnyState, Kcc20State, KronState, TokenState};
use rusqlite::{params, Connection, OptionalExtension};
use std::collections::{HashMap, HashSet};

/// Arguments longer than this are elided (template prefix / suffix pushes and signatures).
const MAX_ARG_BYTES: usize = 40;

/// A spent KOB order input: what its signature script revealed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Reveal {
    pub template: TemplateId,
    /// The state span of the spent script (exact; the redeem script hashes to the spent output).
    pub state: Vec<u8>,
    /// Dispatch tag of the entry that was called.
    pub tag: [u8; 4],
    /// The entry arguments in ABI order; long ones (more than 40 bytes) are empty placeholders.
    pub args: Vec<Vec<u8>>,
}

impl Reveal {
    pub fn entry(&self) -> Option<&'static str> {
        model::entry_name(self.template, self.tag)
    }

    /// Argument `i` as a script number.
    pub fn int(&self, i: usize) -> Option<i64> {
        crate::script::script_int(self.args.get(i)?)
    }

    pub fn nb(&self) -> Option<model::Nb> {
        model::nb(self.args.first()?)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecIn {
    pub txid: Hash32,
    pub index: u32,
    /// Covenant id of the spent output.
    pub cov: Option<Hash32>,
    /// DAA score of the block that created the spent output (covenant inputs only).
    pub daa: u64,
    pub reveal: Option<Reveal>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecOut {
    pub value: u64,
    /// Version (2 bytes) + script; kept for covenant-bound outputs only.
    pub spk: Vec<u8>,
    /// `(authorizing input, covenant id)`.
    pub cov: Option<(u32, Hash32)>,
}

/// A token output of a tracked token whose state was proven against its script public key.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HeldOut {
    pub out: u32,
    /// The token program the output's script belongs to.
    pub program: TemplateId,
    pub state: TokenState,
}

/// One-byte record-log code of a token program.
pub fn program_code(t: TemplateId) -> Option<u8> {
    Some(match t {
        TemplateId::Kcc20Ref => 1,
        TemplateId::Kcc20Ref4x5 => 2,
        TemplateId::Kcc20Ref8x8 => 3,
        TemplateId::Kcc20Ref16x16 => 4,
        TemplateId::Kcc20P2 => 5,
        TemplateId::KronToken2433 => 6,
        TemplateId::KronToken2732 => 7,
        TemplateId::Kcc20KaspaCom025 => 8,
        _ => return None,
    })
}

pub fn program_from_code(c: u8) -> Option<TemplateId> {
    Some(match c {
        1 => TemplateId::Kcc20Ref,
        2 => TemplateId::Kcc20Ref4x5,
        3 => TemplateId::Kcc20Ref8x8,
        4 => TemplateId::Kcc20Ref16x16,
        5 => TemplateId::Kcc20P2,
        6 => TemplateId::KronToken2433,
        7 => TemplateId::KronToken2732,
        8 => TemplateId::Kcc20KaspaCom025,
        _ => return None,
    })
}

/// A token output owned by an order covenant id (owner scheme `0x04`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TokOut {
    pub out: u32,
    pub amount: i64,
    pub owner: Hash32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TxRecord {
    pub txid: Hash32,
    /// Position in the chain block's accepted-transaction list.
    pub pos: u32,
    /// The KOB1 payload (placement records), empty otherwise.
    pub payload: Vec<u8>,
    pub inputs: Vec<RecIn>,
    pub outputs: Vec<RecOut>,
    pub tok_outs: Vec<TokOut>,
    /// Proven states of tracked-token outputs (see the module docs). Logs written before this field existed decode
    /// with an empty list: the flag is the top bit of `pos` (a block never holds 2^31 transactions).
    pub holds: Vec<HeldOut>,
}

/// Stand-in stored for a KOB1 payload that did not decode (`KOB1` and a version byte no reader supports): the record keeps the
/// fact of the reject, not the junk. The processor reports it as `payload:undecodable`.
pub const UNDECODABLE_PAYLOAD: &[u8] = b"KOB1\xff";

/// Stand-in stored for a KOB1 payload of RETIRED templates (payload versions 2 and 3: `Record::RetiredOrder` /
/// `RetiredAmend`, which are decoded but never written): this marker, then the kind names of its retired records,
/// comma-separated. Such records never become orders; the processor counts each as a reject (`retired_template:<kind>`). The
/// stand-in never decodes as a payload (version 0xfe).
pub const RETIRED_PAYLOAD: &[u8] = b"KOB1\xfe";

/// The kind names a [`RETIRED_PAYLOAD`] stand-in lists (`None` for any other payload).
pub fn retired_payload_kinds(payload: &[u8]) -> Option<Vec<String>> {
    let rest = payload.strip_prefix(RETIRED_PAYLOAD)?;
    let names = std::str::from_utf8(rest).ok()?;
    Some(names.split(',').filter(|k| !k.is_empty()).map(str::to_string).collect())
}

/// Set in the encoded `pos` when the record has a `holds` section.
const HOLDS_FLAG: u64 = 1 << 31;

fn code_of(t: TemplateId) -> u8 {
    model::wire_code(t)
}

/// A frame's template table (record-log format 2): wire code -> template hash of every template the frame refers to.
pub type TemplateTable = std::collections::BTreeMap<u8, [u8; 32]>;

/// What decoding a frame had to drop because this build does not know the layout it was written with. The frame
/// itself is kept in the log (it is hash-chained and never rewritten); a later build that knows the layout reads it.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct DecodeStats {
    /// Order reveals of a retired template (an older artifact of a kind this build has: `layouts`), dropped.
    pub retired_reveals: u64,
    /// Order reveals of a template this build does not know at all (format 2 only), dropped.
    pub unknown_reveals: u64,
    /// Token holdings of a token program layout this build does not know, dropped.
    pub unknown_holds: u64,
    /// Recovery imports of an order layout this build does not know, dropped.
    pub unknown_imports: u64,
    /// Format-1 frames whose reveals fit more than one layout and were resolved by decoding the whole frame.
    pub ambiguous_frames: u64,
}

impl DecodeStats {
    pub fn add(&mut self, o: &DecodeStats) {
        self.retired_reveals += o.retired_reveals;
        self.unknown_reveals += o.unknown_reveals;
        self.unknown_holds += o.unknown_holds;
        self.unknown_imports += o.unknown_imports;
        self.ambiguous_frames += o.ambiguous_frames;
    }

    /// Items dropped (reveals, holdings, imports).
    pub fn dropped(&self) -> u64 {
        self.retired_reveals + self.unknown_reveals + self.unknown_holds + self.unknown_imports
    }
}

enum Fmt<'a> {
    /// Format 1: states at the length the template had when written; candidate layouts chosen by `forced` (the frame
    /// search), every choice point traced.
    V1 { forced: &'a [usize], trace: Vec<(usize, usize)> },
    /// Format 2: length-prefixed states; the frame's tables (`None`: this build's templates).
    V2 { orders: Option<&'a TemplateTable>, programs: Option<&'a TemplateTable> },
}

/// How a record being decoded is laid out (record-log format and the frame's template tables) and what was dropped.
pub struct DecodeCtx<'a> {
    fmt: Fmt<'a>,
    pub stats: DecodeStats,
}

/// What a reveal's code and hash resolve to.
enum OrderLayout {
    Current(TemplateId),
    Retired,
    Unknown,
}

impl<'a> DecodeCtx<'a> {
    /// Format 2 with this build's templates (a record encoded by this build on its own).
    pub fn current() -> DecodeCtx<'static> {
        DecodeCtx { fmt: Fmt::V2 { orders: None, programs: None }, stats: DecodeStats::default() }
    }

    /// Format 2 with a frame's tables.
    pub fn v2(orders: &'a TemplateTable, programs: &'a TemplateTable) -> DecodeCtx<'a> {
        DecodeCtx { fmt: Fmt::V2 { orders: Some(orders), programs: Some(programs) }, stats: DecodeStats::default() }
    }

    /// Format 1, taking the candidate `forced[k]` at the k-th ambiguous reveal (0 past the end).
    pub fn v1(forced: &'a [usize]) -> DecodeCtx<'a> {
        DecodeCtx { fmt: Fmt::V1 { forced, trace: vec![] }, stats: DecodeStats::default() }
    }

    fn is_v1(&self) -> bool {
        matches!(self.fmt, Fmt::V1 { .. })
    }

    /// The choice points of a format-1 decode: `(candidate taken, candidates)`.
    fn trace(&self) -> &[(usize, usize)] {
        match &self.fmt {
            Fmt::V1 { trace, .. } => trace,
            Fmt::V2 { .. } => &[],
        }
    }

    /// Format 2: the layout an order code of the frame refers to.
    fn order_layout(&self, code: u8) -> WireResult<OrderLayout> {
        let Fmt::V2 { orders, .. } = &self.fmt else { unreachable!("format 2 only") };
        let current = model::from_wire_code(code);
        let Some(table) = orders else {
            return Ok(match current {
                Some(id) => OrderLayout::Current(id),
                None if model::is_retired_wire_code(code) => OrderLayout::Retired,
                None => OrderLayout::Unknown,
            });
        };
        let hash = table.get(&code).ok_or(WireError("template code missing from the frame's table"))?;
        Ok(match current {
            Some(id) if &template(id).hash == hash => OrderLayout::Current(id),
            _ if super::layouts::retired_by_hash(hash).is_some() => OrderLayout::Retired,
            _ => OrderLayout::Unknown,
        })
    }

    /// Format 2: the token program a holding's code refers to, when this build has that exact program.
    fn program(&self, code: u8) -> WireResult<Option<TemplateId>> {
        let Fmt::V2 { programs, .. } = &self.fmt else { unreachable!("format 2 only") };
        let current = program_from_code(code);
        let Some(table) = programs else { return Ok(current) };
        let hash = table.get(&code).ok_or(WireError("token program code missing from the frame's table"))?;
        Ok(current.filter(|p| &token_template(*p).hash == hash))
    }

    /// Reads a reveal (after its flag): `Some` when this build can interpret it, `None` when it was dropped.
    fn reveal(&mut self, r: &mut Reader<'_>) -> WireResult<Option<Reveal>> {
        let code = r.u8()?;
        if self.is_v1() {
            return self.reveal_v1(code, r);
        }
        let state = r.bytes()?.to_vec();
        let tag: [u8; 4] = r.raw(4)?.try_into().expect("4");
        let args = read_args(r)?;
        Ok(match self.order_layout(code)? {
            OrderLayout::Current(id) if state.len() == template(id).state_len => Some(Reveal { template: id, state, tag, args }),
            OrderLayout::Current(_) | OrderLayout::Unknown => {
                self.stats.unknown_reveals += 1;
                None
            }
            OrderLayout::Retired => {
                self.stats.retired_reveals += 1;
                None
            }
        })
    }

    /// Format 1: the state length is not recorded. Every format-1 frame was written before protocol v3 (format 2 replaced it
    /// on 2026-10-01), so its reveals are of templates that are retired now (`super::layouts`: most v3 templates have the
    /// state length and the dispatch tags of their v2.6 predecessor, so a format-1 reveal must never be read as one of
    /// today's). Candidates: the retired layouts of the code, one per state length; a candidate fits when the four bytes
    /// after its state are one of its dispatch tags. Several fitting candidates make a choice point (the frame search tries
    /// them in order). The reveal is parsed past and dropped, counted.
    fn reveal_v1(&mut self, code: u8, r: &mut Reader<'_>) -> WireResult<Option<Reveal>> {
        let mut cands: Vec<usize> = vec![];
        for l in super::layouts::retired(code) {
            if !cands.contains(&l.state_len) && r.peek(l.state_len, 4).is_some_and(|t| l.has_tag(t)) {
                cands.push(l.state_len);
            }
        }
        if cands.is_empty() {
            // nothing fits by its tag: the layout the last format-1 writers had, the protocol v2.6 template of the kind (an
            // error surfaces at the frame level); a retired code (the v2.4 receipt): the length all its layouts share
            let fam = if code & 0x80 != 0 { Family::Kron } else { Family::Kcc20 };
            let v26 = model::from_wire_code(code)
                .and_then(|_| kob_protocol::retired::payload_layout(fam, code & 0x7f))
                .map(|r| r.template.state_len);
            let mut lens = super::layouts::retired(code).map(|l| l.state_len);
            match (v26, lens.next()) {
                (Some(len), _) => cands.push(len),
                (None, Some(len)) if lens.all(|l| l == len) => cands.push(len),
                _ => return Err(WireError("unknown template code")),
            }
        }
        let pick = if cands.len() > 1 {
            let Fmt::V1 { forced, trace } = &mut self.fmt else { unreachable!("format 1") };
            let k = forced.get(trace.len()).copied().unwrap_or(0).min(cands.len() - 1);
            trace.push((k, cands.len()));
            cands[k]
        } else {
            cands[0]
        };
        r.raw(pick)?;
        r.raw(4)?;
        read_args(r)?;
        self.stats.retired_reveals += 1;
        Ok(None)
    }

    /// Reads a holding: `None` when its program layout is not this build's (dropped).
    fn hold(&mut self, r: &mut Reader<'_>) -> WireResult<Option<HeldOut>> {
        let out = r.var()? as u32;
        let code = r.u8()?;
        if self.is_v1() {
            let program = program_from_code(code).ok_or(WireError("unknown token program code"))?;
            let tpl = token_template(program);
            let state = TokenState::decode_with(tpl, r.raw(tpl.state_len)?).map_err(|_| WireError("token state"))?;
            return Ok(Some(HeldOut { out, program, state }));
        }
        let raw = r.bytes()?;
        let held = self.program(code)?.and_then(|program| {
            let tpl = token_template(program);
            if raw.len() != tpl.state_len {
                return None;
            }
            TokenState::decode_with(tpl, raw).ok().map(|state| HeldOut { out, program, state })
        });
        if held.is_none() {
            self.stats.unknown_holds += 1;
        }
        Ok(held)
    }

    /// An imported order's template: `None` (counted) when this build cannot interpret its state. A format-1 import (written
    /// before protocol v3) is always of a retired template ([`DecodeCtx::reveal_v1`]).
    pub(crate) fn import_template(&mut self, code: u8, state_len: usize) -> WireResult<Option<TemplateId>> {
        let id = if self.is_v1() {
            None
        } else {
            match self.order_layout(code)? {
                OrderLayout::Current(id) => Some(id),
                _ => None,
            }
        };
        let id = id.filter(|id| template(*id).state_len == state_len);
        if id.is_none() {
            self.stats.unknown_imports += 1;
        }
        Ok(id)
    }
}

fn read_args(r: &mut Reader<'_>) -> WireResult<Vec<Vec<u8>>> {
    let n = r.count(1)?;
    let mut args = Vec::with_capacity(n);
    for _ in 0..n {
        args.push(r.bytes()?.to_vec());
    }
    Ok(args)
}

/// Most frame decodes a format-1 frame may take to resolve ambiguous reveals.
const MAX_V1_ATTEMPTS: usize = 256;

/// Decodes a whole format-1 body with `f`, backtracking over the choice points of ambiguous reveals (depth first, today's
/// layout first) until one decodes the whole body.
pub fn decode_v1<T>(
    body: &[u8],
    mut f: impl FnMut(&mut Reader<'_>, &mut DecodeCtx<'_>) -> WireResult<T>,
) -> WireResult<(T, DecodeStats)> {
    let mut forced: Vec<usize> = vec![];
    for attempt in 0..MAX_V1_ATTEMPTS {
        let mut ctx = DecodeCtx::v1(&forced);
        let mut r = Reader::new(body);
        let res = f(&mut r, &mut ctx).and_then(|v| if r.done() { Ok(v) } else { Err(WireError("trailing bytes")) });
        match res {
            Ok(v) => {
                let ambiguous = attempt > 0 || !ctx.trace().is_empty();
                let mut stats = ctx.stats;
                stats.ambiguous_frames += ambiguous as u64;
                return Ok((v, stats));
            }
            Err(e) => {
                let mut trace = ctx.trace().to_vec();
                while trace.last().is_some_and(|&(k, n)| k + 1 >= n) {
                    trace.pop();
                }
                let Some(last) = trace.last_mut() else { return Err(e) };
                last.0 += 1;
                forced = trace.iter().map(|d| d.0).collect();
            }
        }
    }
    Err(WireError("too many ambiguous reveals"))
}

impl TxRecord {
    /// Encodes the record in record-log format 2 (length-prefixed states; the frame carries the template tables, see
    /// [`TxRecord::templates`]).
    pub fn encode(&self, w: &mut Writer) {
        self.encode_as(w, 2)
    }

    /// Encodes the record in record-log format 1 (states at their length, unmarked): what builds before 2026-10-01
    /// wrote. Tests and tools reproduce old logs with it; this build never writes it.
    #[doc(hidden)]
    pub fn encode_v1(&self, w: &mut Writer) {
        self.encode_as(w, 1)
    }

    fn encode_as(&self, w: &mut Writer, format: u8) {
        let state = |w: &mut Writer, s: &[u8]| if format == 1 { w.raw(s) } else { w.bytes(s) };
        w.hash(&self.txid);
        w.var(self.pos as u64 | if self.holds.is_empty() { 0 } else { HOLDS_FLAG });
        w.bytes(&self.payload);
        w.var(self.inputs.len() as u64);
        for i in &self.inputs {
            let flags = i.cov.is_some() as u8 | (i.reveal.is_some() as u8) << 1;
            w.u8(flags);
            w.hash(&i.txid);
            w.var(i.index as u64);
            if let Some(c) = &i.cov {
                w.hash(c);
                w.var(i.daa);
            }
            if let Some(r) = &i.reveal {
                w.u8(code_of(r.template));
                state(w, &r.state);
                w.raw(&r.tag);
                w.var(r.args.len() as u64);
                for a in &r.args {
                    w.bytes(a);
                }
            }
        }
        w.var(self.outputs.len() as u64);
        for o in &self.outputs {
            w.u8(o.cov.is_some() as u8);
            w.var(o.value);
            if let Some((auth, cov)) = &o.cov {
                w.bytes(&o.spk);
                w.var(*auth as u64);
                w.hash(cov);
            }
        }
        w.var(self.tok_outs.len() as u64);
        for t in &self.tok_outs {
            w.var(t.out as u64);
            w.svar(t.amount);
            w.hash(&t.owner);
        }
        if !self.holds.is_empty() {
            w.var(self.holds.len() as u64);
            for h in &self.holds {
                w.var(h.out as u64);
                w.u8(program_code(h.program).expect("token program"));
                state(w, &h.state.encode());
            }
        }
    }

    /// Adds the templates the record refers to (order reveals, holding programs) to a frame's tables.
    pub fn templates(&self, orders: &mut TemplateTable, programs: &mut TemplateTable) {
        for r in self.inputs.iter().filter_map(|i| i.reveal.as_ref()) {
            orders.insert(code_of(r.template), template(r.template).hash);
        }
        for h in &self.holds {
            programs.insert(program_code(h.program).expect("token program"), token_template(h.program).hash);
        }
    }

    /// Decodes a record this build encoded on its own (format 2, this build's templates).
    pub fn decode(r: &mut Reader<'_>) -> WireResult<TxRecord> {
        TxRecord::decode_in(r, &mut DecodeCtx::current())
    }

    /// Decodes a format-1 record (record logs written before 2026-10-01) on its own: the whole input is the record.
    pub fn decode_v1(body: &[u8]) -> WireResult<(TxRecord, DecodeStats)> {
        decode_v1(body, TxRecord::decode_in)
    }

    pub fn decode_in(r: &mut Reader<'_>, ctx: &mut DecodeCtx<'_>) -> WireResult<TxRecord> {
        let txid = r.hash()?;
        let raw_pos = r.var()?;
        let has_holds = raw_pos & HOLDS_FLAG != 0;
        let pos = (raw_pos & !HOLDS_FLAG) as u32;
        let payload = r.bytes()?.to_vec();
        let n_in = r.count(34)?;
        let mut inputs = Vec::with_capacity(n_in);
        for _ in 0..n_in {
            let flags = r.u8()?;
            let ptxid = r.hash()?;
            let index = r.var()? as u32;
            let (cov, daa) = if flags & 1 != 0 { (Some(r.hash()?), r.var()?) } else { (None, 0) };
            let reveal = if flags & 2 != 0 { ctx.reveal(r)? } else { None };
            inputs.push(RecIn { txid: ptxid, index, cov, daa, reveal });
        }
        let n_out = r.count(2)?;
        let mut outputs = Vec::with_capacity(n_out);
        for _ in 0..n_out {
            let flags = r.u8()?;
            let value = r.var()?;
            if flags & 1 != 0 {
                let spk = r.bytes()?.to_vec();
                let auth = r.var()? as u32;
                let cov = r.hash()?;
                outputs.push(RecOut { value, spk, cov: Some((auth, cov)) });
            } else {
                outputs.push(RecOut { value, spk: vec![], cov: None });
            }
        }
        let n_tok = r.count(34)?;
        let mut tok_outs = Vec::with_capacity(n_tok);
        for _ in 0..n_tok {
            tok_outs.push(TokOut { out: r.var()? as u32, amount: r.svar()?, owner: r.hash()? });
        }
        let mut holds = Vec::new();
        if has_holds {
            let n = r.count(3)?;
            for _ in 0..n {
                if let Some(h) = ctx.hold(r)? {
                    holds.push(h);
                }
            }
        }
        Ok(TxRecord { txid, pos, payload, inputs, outputs, tok_outs, holds })
    }

    pub fn encoded_len(&self) -> usize {
        let mut w = Writer::new();
        self.encode(&mut w);
        w.buf.len()
    }
}

// ---------------------------------------------------------------------------------------------
// lookups

/// DAA score of the chain block that created a tracked, unspent order UTXO (what `OpTxInputDaaScore`
/// reads: the accepting block's DAA score). The node does not report it per input (`blockDaaScore` is
/// `null` in VSPC v2 at every verbosity), so the indexer keeps it from the block that created the UTXO.
pub fn order_utxo_daa(conn: &Connection, txid: &Hash32, idx: u32) -> DbResult<Option<i64>> {
    Ok(conn
        .prepare_cached("SELECT created_daa FROM order_utxos WHERE txid = ?1 AND idx = ?2 AND spent_block IS NULL")?
        .query_row(params![&txid.0[..], idx], |r| r.get(0))
        .optional()?)
}

pub fn token_utxo_tracked(conn: &Connection, txid: &Hash32, idx: u32) -> DbResult<bool> {
    Ok(conn
        .prepare_cached("SELECT 1 FROM token_utxos WHERE txid = ?1 AND idx = ?2 AND spent_block IS NULL")?
        .exists(params![&txid.0[..], idx])?)
}

pub fn order_known(conn: &Connection, cov: &Hash32) -> DbResult<bool> {
    Ok(conn.prepare_cached("SELECT 1 FROM orders WHERE covenant_id = ?1")?.exists([&cov.0[..]])?)
}

pub fn holding_tracked(conn: &Connection, txid: &Hash32, idx: u32) -> DbResult<bool> {
    Ok(conn
        .prepare_cached("SELECT 1 FROM token_holdings WHERE txid = ?1 AND idx = ?2 AND spent_block IS NULL")?
        .exists(params![&txid.0[..], idx])?)
}

/// A token some order trades: the token of an order, or token B of a pair order (so strays of B sent to a pair order's id
/// are recorded as strays of that order).
pub fn token_known(conn: &Connection, token: &Hash32) -> DbResult<bool> {
    Ok(conn.prepare_cached("SELECT 1 FROM orders WHERE token_cov_id = ?1 LIMIT 1")?.exists([&token.0[..]])?
        || conn.prepare_cached("SELECT 1 FROM orders WHERE quote_cov_id = ?1 LIMIT 1")?.exists([&token.0[..]])?)
}

// ---------------------------------------------------------------------------------------------
// extraction

fn spent_of(inp: &TxInput) -> Option<&crate::rpc::types::SpentUtxo> {
    inp.verbose_data.as_ref().and_then(|v| v.utxo_entry.as_ref())
}

/// What a covenant input's signature script reveals about a KOB order, verified against
/// the spent script public key.
pub fn parse_reveal(sigscript: &[u8], spent_spk: &[u8]) -> Option<Reveal> {
    let pushes = parse_pushes(sigscript)?;
    if pushes.len() < 2 {
        return None;
    }
    let redeem = pushes.last()?;
    let tag: [u8; 4] = pushes[pushes.len() - 2].as_slice().try_into().ok()?;
    // identify first (prefix and suffix compared with the pinned templates): only an order's redeem script is hashed, never
    // the token program a transfer input reveals (up to 25 KB)
    let (id, state) = model::identify_redeem(redeem)?;
    if !model::is_order(id) {
        return None;
    }
    if p2sh_spk(redeem) != spent_spk {
        return None;
    }
    let args = pushes[..pushes.len() - 2].iter().map(|a| if a.len() > MAX_ARG_BYTES { vec![] } else { a.clone() }).collect();
    Some(Reveal { template: id, state: state.to_vec(), tag, args })
}

/// True for the reveal of a plain resting `KobAsk` / `KobBid` (either family) or a resting `KobPair` spent by a fill (`n > 0`):
/// trigger evidence (a `KobPair` is the mode-1 evidence of the pair conditionals of its pair).
fn is_evidence_fill(r: &Reveal) -> bool {
    matches!(
        r.template,
        TemplateId::KobAsk | TemplateId::KobAskKron | TemplateId::KobBid | TemplateId::KobBidKron | TemplateId::KobPair
    ) && matches!(r.nb(), Some(model::Nb::Fill(n)) if n > 0)
}

/// A token transfer's leader as the extractor matches it against outputs: the program and every next state with the P2SH
/// script public key it produces (computed once per state, not once per output and state).
pub type Leader = (TemplateId, Vec<(TokenState, kaspa_consensus_core::tx::ScriptPublicKey)>);

/// [`token_next_states`] with each state's script public key under the program.
pub fn leader_of(sigscript: &[u8]) -> Option<Leader> {
    let (tid, states) = token_next_states(sigscript)?;
    let tpl = token_template(tid);
    Some((
        tid,
        states
            .into_iter()
            .map(|s| {
                let spk = s.spk_with(tpl);
                (s, spk)
            })
            .collect(),
    ))
}

/// The database-free work of one transaction (signature-script parsing, script hashing, placement verification), done
/// before the single-threaded database pass, in parallel over a batch ([`precompute`]). Every entry is a pure function of
/// the transaction, so a cache hit gives exactly what the database pass would have computed itself.
#[derive(Debug, Default)]
pub struct PreTx {
    /// Covenant inputs whose redeem script is an order template: the reveal (`None`: not an order input).
    pub reveals: HashMap<u32, Option<Reveal>>,
    /// Authorising inputs of covenant outputs: their transfer leader, if they are one.
    pub leaders: HashMap<u32, Option<Leader>>,
    /// `payload::recover_orders` of each single ORDER record of the payload, keyed by the single-record payload bytes.
    pub placements: HashMap<Vec<u8>, Result<Vec<payload::RecoveredOrder>, String>>,
}

/// Pure results of a batch, by transaction id.
pub type PreBatch = HashMap<Hash32, PreTx>;

impl PreTx {
    /// The pure work of `tx` (nothing for a transaction without a KOB1 payload or covenant input / output).
    pub fn compute(tx: &Tx) -> Option<PreTx> {
        let has_kob1 = tx.payload.0.starts_with(payload::MAGIC);
        let any_cov_in = tx.inputs.iter().any(|i| spent_of(i).is_some_and(|u| u.covenant_id.is_some()));
        let any_cov_out = tx.outputs.iter().any(|o| o.covenant.is_some());
        if !has_kob1 && !any_cov_in && !any_cov_out {
            return None;
        }
        let mut pre = PreTx::default();
        for (i, inp) in tx.inputs.iter().enumerate() {
            let Some(u) = spent_of(inp).filter(|u| u.covenant_id.is_some()) else { continue };
            pre.reveals.insert(i as u32, parse_reveal(&inp.signature_script.0, &u.script_public_key.0));
        }
        for o in &tx.outputs {
            let Some(c) = &o.covenant else { continue };
            pre.leaders
                .entry(c.authorizing_input)
                .or_insert_with(|| tx.inputs.get(c.authorizing_input as usize).and_then(|i| leader_of(&i.signature_script.0)));
        }
        if has_kob1 {
            if let Ok(Some(p)) = payload::decode(&tx.payload.0) {
                let shape = raw_record(tx);
                for r in p.records.iter().filter(|r| matches!(r, Record::Order { .. })) {
                    let Ok(single) = payload::encode(std::slice::from_ref(r)) else { continue };
                    let res = payload::recover_orders(&super::processor::tx_json(&shape, single.clone())).map_err(|e| e.to_string());
                    pre.placements.insert(single, res);
                }
            }
        }
        Some(pre)
    }
}

/// The reduced shape of a raw transaction as far as `processor::tx_json` reads it for placement verification (outpoints,
/// covenant ids of the spent outputs, output values, scripts of covenant outputs and bindings).
fn raw_record(tx: &Tx) -> TxRecord {
    TxRecord {
        txid: tx.verbose_data.transaction_id,
        pos: 0,
        payload: vec![],
        inputs: tx
            .inputs
            .iter()
            .map(|i| RecIn {
                txid: i.previous_outpoint.transaction_id,
                index: i.previous_outpoint.index,
                cov: spent_of(i).and_then(|u| u.covenant_id),
                daa: 0,
                reveal: None,
            })
            .collect(),
        outputs: tx
            .outputs
            .iter()
            .map(|o| match &o.covenant {
                Some(c) => {
                    RecOut { value: o.value, spk: o.script_public_key.0.clone(), cov: Some((c.authorizing_input, c.covenant_id)) }
                }
                None => RecOut { value: o.value, spk: vec![], cov: None },
            })
            .collect(),
        tok_outs: vec![],
        holds: vec![],
    }
}

/// [`PreTx::compute`] for every transaction of `blocks`, on up to `threads` threads (inline below 64 candidate transactions,
/// where a thread costs more than it saves).
pub fn precompute(blocks: &[&[Tx]], threads: usize) -> PreBatch {
    let txs: Vec<&Tx> = blocks
        .iter()
        .flat_map(|b| b.iter())
        .filter(|t| {
            t.payload.0.starts_with(payload::MAGIC)
                || t.outputs.iter().any(|o| o.covenant.is_some())
                || t.inputs.iter().any(|i| spent_of(i).is_some_and(|u| u.covenant_id.is_some()))
        })
        .collect();
    let threads = threads.max(1);
    if threads == 1 || txs.len() < 64 {
        return txs.iter().filter_map(|t| PreTx::compute(t).map(|p| (t.verbose_data.transaction_id, p))).collect();
    }
    let chunk = txs.len().div_ceil(threads);
    std::thread::scope(|s| {
        let handles: Vec<_> = txs
            .chunks(chunk)
            .map(|c| {
                s.spawn(move || {
                    c.iter().filter_map(|t| PreTx::compute(t).map(|p| (t.verbose_data.transaction_id, p))).collect::<Vec<_>>()
                })
            })
            .collect();
        handles.into_iter().flat_map(|h| h.join().expect("precompute thread panicked")).collect()
    })
}

/// The output states a token input authorises, from its signature script (either family).
pub fn token_next_states(sigscript: &[u8]) -> Option<(TemplateId, Vec<TokenState>)> {
    kcc20_leader_next_states(sigscript)
        .map(|(id, v)| (id, v.into_iter().map(TokenState::Kcc20).collect()))
        .or_else(|| kron_next_states(sigscript).map(|(id, v)| (id, v.into_iter().map(TokenState::Kron).collect())))
}

/// The next states of a KRON token input (`kob_protocol::script::kron_token_sigscript`): the columns
/// `owners`, `id_types`, `amounts`, `is_minters` of the whole covenant group, the signature column, the
/// witness column and the redeem script. Every token input of the token carries the same columns.
pub fn kron_next_states(sigscript: &[u8]) -> Option<(TemplateId, Vec<KronState>)> {
    let pushes = parse_pushes(sigscript)?;
    if pushes.len() != 7 {
        return None;
    }
    let (id, _) = model::identify_kron_token(&pushes[6])?;
    let n = pushes[1].len();
    if pushes[0].len() != 32 * n || pushes[2].len() != 8 * n || pushes[3].len() != n {
        return None;
    }
    (0..n)
        .map(|k| {
            Some(KronState {
                owner: pushes[0][32 * k..32 * k + 32].try_into().ok()?,
                id_type: pushes[1][k],
                amount: i64::from_le_bytes(pushes[2][8 * k..8 * k + 8].try_into().ok()?),
                is_minter: pushes[3][k],
            })
        })
        .collect::<Option<Vec<_>>>()
        .map(|v| (id, v))
}

/// The output states a KCC-20 `transfer` (leader) call authorises, from its signature script.
///
/// The `State[]` argument is a KCC-1 record array (one push per field: amounts as 8-byte signed magnitude,
/// owners, owner schemes, borrow schemes, borrow guards, extension commitments), then the witness, the
/// dispatch tag and the redeem script; [`kob_protocol::kcc20::transfer_next_states`] decodes it strictly.
fn kcc20_leader_next_states(sigscript: &[u8]) -> Option<(TemplateId, Vec<Kcc20State>)> {
    let pushes = parse_pushes(sigscript)?;
    if pushes.len() != 9 {
        return None;
    }
    let redeem = &pushes[8];
    let tag: [u8; 4] = pushes[7].as_slice().try_into().ok()?;
    let (id, _) = model::identify_redeem(redeem)?;
    if !id.is_token() || model::entry_name(id, tag) != Some("transfer") {
        return None;
    }
    let tpl = kob_protocol::artifacts::try_template(id)?;
    Some((id, kob_protocol::kcc20::transfer_next_states(tpl, sigscript)?))
}

/// Turns accepted transactions into records, consulting the database for relevance.
pub struct Extractor<'a> {
    pub conn: &'a Connection,
    /// The token allowlist: its tokens' holdings are tracked.
    pub tokens: &'a crate::tokens::TokenAllowlist,
    /// The allowlist is not required (open listing): every token an order trades is tracked too.
    pub track_traded: bool,
}

impl Extractor<'_> {
    fn tracked(&self, token: &Hash32) -> DbResult<bool> {
        Ok(self.tokens.get(token).is_some() || (self.track_traded && token_known(self.conn, token)?))
    }
}

impl Extractor<'_> {
    /// `Ok(None)` when the transaction is unrelated to KOB.
    pub fn extract(&self, tx: &Tx, pos: u32) -> DbResult<Option<TxRecord>> {
        self.extract_with(tx, pos, None)
    }

    /// [`Extractor::extract`] with the transaction's precomputed pure work ([`PreTx`]); without it everything is computed here.
    pub fn extract_with(&self, tx: &Tx, pos: u32, pre: Option<&PreTx>) -> DbResult<Option<TxRecord>> {
        let reveal_of = |i: usize, inp: &TxInput, spk: &[u8]| -> Option<Reveal> {
            match pre.and_then(|p| p.reveals.get(&(i as u32))) {
                Some(r) => r.clone(),
                None => parse_reveal(&inp.signature_script.0, spk),
            }
        };
        let has_kob1 = tx.payload.0.starts_with(payload::MAGIC);
        let any_cov_in = tx.inputs.iter().any(|i| spent_of(i).is_some_and(|u| u.covenant_id.is_some()));
        let any_cov_out = tx.outputs.iter().any(|o| o.covenant.is_some());
        if !has_kob1 && !any_cov_in && !any_cov_out {
            return Ok(None);
        }
        let txid = tx.verbose_data.transaction_id;

        // Placement records: which orders (and tokens) this transaction announces.
        let mut relevant = false;
        let mut payload_tokens: HashSet<Hash32> = HashSet::new();
        // What of the payload the record keeps: the log is permanent and replayed on every rebuild, so a payload is
        // never stored as sent. Only the placement (order) records survive, at most one per output of the transaction and
        // re-encoded canonically; an undecodable payload is replaced by a five-byte stand-in that the processor rejects.
        let mut kept: Vec<Record> = vec![];
        let mut undecodable = false;
        // kind names of the payload's records of retired templates (payload versions 2 and 3): counted, never orders
        let mut retired: Vec<String> = vec![];
        if has_kob1 {
            match payload::decode(&tx.payload.0) {
                Ok(Some(p)) => {
                    for r in p.records {
                        if let Record::RetiredOrder { template, family, .. } | Record::RetiredAmend { template, family, .. } = &r {
                            relevant = true;
                            if retired.len() < tx.outputs.len().max(1) {
                                let fam = Family::from_code(*family).unwrap_or(Family::Kcc20);
                                retired.push(fam.kind_name(template.base().name()));
                            }
                        } else if let Record::Order { template: t, state, .. } = &r {
                            relevant = true;
                            if let Ok(s) = AnyState::decode(*t, state) {
                                payload_tokens.insert(Hash32(s.token_cov_id()));
                                if let Some(p) = s.pair_tokens() {
                                    payload_tokens.insert(Hash32(p.b.cov_id));
                                }
                            }
                            if kept.len() < tx.outputs.len().max(1) {
                                kept.push(r.clone());
                            }
                        } else if matches!(r, Record::Amend { .. } | Record::Sweep { .. }) && kept.len() < tx.outputs.len().max(1) {
                            // an in-place amend or sweep matters only next to a tracked order input (relevant through it)
                            kept.push(r);
                        }
                    }
                }
                Ok(None) => {}
                // A KOB1 payload that does not decode is recorded as a reject.
                Err(_) => {
                    relevant = true;
                    undecodable = true;
                }
            }
        }

        // Inputs.
        let mut inputs = Vec::with_capacity(tx.inputs.len());
        // untracked covenant inputs: possible trigger evidence, read only if the transaction turns out relevant
        let mut untracked = Vec::new();
        for (i, inp) in tx.inputs.iter().enumerate() {
            let prev = &inp.previous_outpoint;
            let spent = spent_of(inp);
            let cov = spent.and_then(|u| u.covenant_id);
            let mut rec = RecIn { txid: prev.transaction_id, index: prev.index, cov, daa: 0, reveal: None };
            if cov.is_some() {
                let spk = spent.map(|u| u.script_public_key.0.as_slice()).unwrap_or(&[]);
                if let Some(daa) = order_utxo_daa(self.conn, &prev.transaction_id, prev.index)? {
                    relevant = true;
                    rec.daa = daa as u64;
                    rec.reveal = reveal_of(i, inp, spk);
                } else if token_utxo_tracked(self.conn, &prev.transaction_id, prev.index)?
                    || holding_tracked(self.conn, &prev.transaction_id, prev.index)?
                {
                    relevant = true;
                } else {
                    untracked.push(i);
                }
            }
            inputs.push(rec);
        }

        // Outputs.
        let mut outputs = Vec::with_capacity(tx.outputs.len());
        for o in &tx.outputs {
            match &o.covenant {
                Some(c) => {
                    outputs.push(RecOut {
                        value: o.value,
                        spk: o.script_public_key.0.clone(),
                        cov: Some((c.authorizing_input, c.covenant_id)),
                    });
                }
                None => outputs.push(RecOut { value: o.value, spk: vec![], cov: None }),
            }
        }

        // Token outputs owned by orders (custody, deliveries to exits, strays).
        let mut tok_outs = Vec::new();
        let mut holds = Vec::new();
        let fresh: HashSet<Hash32> = tx.outputs.iter().filter_map(|o| o.covenant.map(|c| c.covenant_id)).collect();
        let mut leaders: HashMap<u32, Option<Leader>> = HashMap::new();
        let mut used: HashSet<(u32, usize)> = HashSet::new();
        for (j, o) in tx.outputs.iter().enumerate() {
            let Some(c) = &o.covenant else { continue };
            let token = c.covenant_id;
            let order_gate = payload_tokens.contains(&token) || token_known(self.conn, &token)?;
            let tracked = self.tracked(&token)?;
            // Any other token program's output may still be owned by an order id: a FOREIGN stray (matcher.md 1.2), which the
            // indexer must flag. Its leader is read unless the authorising input is a tracked order (an order's continuation is
            // never a token output), so the probe costs one signature-script parse per foreign token transfer.
            let foreign_probe = !order_gate
                && !tracked
                && !inputs.get(c.authorizing_input as usize).is_some_and(|i: &RecIn| i.reveal.is_some() || i.daa > 0);
            if !order_gate && !tracked && !foreign_probe {
                continue;
            }
            let leader =
                leaders.entry(c.authorizing_input).or_insert_with(|| match pre.and_then(|p| p.leaders.get(&c.authorizing_input)) {
                    Some(l) => l.clone(),
                    None => tx.inputs.get(c.authorizing_input as usize).and_then(|i| leader_of(&i.signature_script.0)),
                });
            let Some((tid, states)) = leader else { continue };
            let want = parse_spk(&o.script_public_key.0);
            for (k, (s, spk)) in states.iter().enumerate() {
                if used.contains(&(c.authorizing_input, k)) || want.as_ref() != Some(spk) {
                    continue;
                }
                // the state is proven: the P2SH it produces is this output's script public key
                used.insert((c.authorizing_input, k));
                let owner = Hash32(s.owner());
                let order_owned = s.is_covenant_owned()
                    && if order_gate {
                        fresh.contains(&owner) || order_known(self.conn, &owner)?
                    } else {
                        // a token no order trades (tracked or probed): a foreign stray of an order the indexer already knows
                        order_known(self.conn, &owner)?
                    };
                if order_owned {
                    tok_outs.push(TokOut { out: j as u32, amount: s.amount(), owner });
                    relevant = true;
                }
                if tracked || order_owned {
                    holds.push(HeldOut { out: j as u32, program: *tid, state: s.clone() });
                    relevant = true;
                }
                break;
            }
        }
        if !relevant {
            return Ok(None);
        }
        // A fill of a plain KobAsk / KobBid this indexer does not track (no placement record): the covenants accept it as trigger
        // evidence all the same (they check its template, not a record), so its quote is kept for the tracked conditional orders
        // of the transaction (a trailing ratchet is derived from it). It does not make the transaction relevant by itself.
        for i in untracked {
            let inp = &tx.inputs[i];
            let spk = spent_of(inp).map(|u| u.script_public_key.0.as_slice()).unwrap_or(&[]);
            inputs[i].reveal = reveal_of(i, inp, spk).filter(is_evidence_fill);
        }
        let payload = if undecodable {
            UNDECODABLE_PAYLOAD.to_vec()
        } else if !retired.is_empty() {
            // a payload of retired templates holds nothing else this build applies (its sweeps name orders of those
            // templates, which are not tracked)
            [RETIRED_PAYLOAD, retired.join(",").as_bytes()].concat()
        } else if kept.is_empty() {
            vec![]
        } else {
            payload::encode(&kept).unwrap_or_else(|_| UNDECODABLE_PAYLOAD.to_vec())
        };
        Ok(Some(TxRecord { txid, pos, payload, inputs, outputs, tok_outs, holds }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn record_roundtrip() {
        let id = TemplateId::KobAsk;
        let st = template(id).contract().compiled.bytecode[1..1 + template(id).state_len].to_vec();
        let rec = TxRecord {
            txid: Hash32([1; 32]),
            pos: 300,
            payload: vec![1, 2, 3],
            inputs: vec![
                RecIn { txid: Hash32([2; 32]), index: 1, cov: None, daa: 0, reveal: None },
                RecIn {
                    txid: Hash32([3; 32]),
                    index: 7,
                    cov: Some(Hash32([4; 32])),
                    daa: 123_456,
                    reveal: Some(Reveal { template: id, state: st, tag: [1, 2, 3, 4], args: vec![vec![5; 8], vec![], vec![9]] }),
                },
            ],
            outputs: vec![
                RecOut { value: 10, spk: vec![], cov: None },
                RecOut { value: u64::MAX, spk: vec![0, 0, 0xaa], cov: Some((1, Hash32([5; 32]))) },
            ],
            tok_outs: vec![TokOut { out: 1, amount: -5, owner: Hash32([6; 32]) }],
            holds: vec![
                HeldOut { out: 1, program: TemplateId::Kcc20Ref8x8, state: TokenState::Kcc20(Kcc20State::p2pk(7, [3; 32], [4; 32])) },
                HeldOut { out: 0, program: TemplateId::KronToken2433, state: TokenState::Kron(KronState::addr(9, [5; 32])) },
            ],
        };
        let mut w = Writer::new();
        rec.encode(&mut w);
        let mut r = Reader::new(&w.buf);
        assert_eq!(TxRecord::decode(&mut r).unwrap(), rec);
        assert!(r.done());
        assert!(TxRecord::decode(&mut Reader::new(&w.buf[..w.buf.len() - 3])).is_err());
    }

    #[test]
    fn kron_reveals_survive_the_record_log_and_old_codes_read_unchanged() {
        // every order template has its own wire code; the KRON kinds share the KCC-20 kind codes with the high bit set
        for id in crate::model::ORDER_KINDS {
            let code = crate::model::wire_code(id);
            assert_eq!(crate::model::from_wire_code(code), Some(id), "{}", id.name());
            assert_eq!(code & 0x80 != 0, id.family() == kob_protocol::family::Family::Kron);
        }
        // a log written before the KRON family existed only has codes below 0x80
        assert_eq!(crate::model::wire_code(TemplateId::KobAsk), 1);
        // the retired v2.4 receipt codes decode to no template
        assert_eq!(crate::model::from_wire_code(7), None);
        assert!(crate::model::is_retired_wire_code(7) && crate::model::is_retired_wire_code(0x87));
        assert_eq!(crate::model::wire_code(TemplateId::KobCondBidKron), 0x84);
        let id = TemplateId::KobIfdAskKron;
        let st = template(id).contract().compiled.bytecode[1..1 + template(id).state_len].to_vec();
        let rec = TxRecord {
            txid: Hash32([1; 32]),
            pos: 1,
            payload: vec![],
            inputs: vec![RecIn {
                txid: Hash32([3; 32]),
                index: 0,
                cov: Some(Hash32([4; 32])),
                daa: 9,
                reveal: Some(Reveal { template: id, state: st, tag: [1, 2, 3, 4], args: vec![vec![7; 8]] }),
            }],
            outputs: vec![],
            tok_outs: vec![],
            holds: vec![],
        };
        let mut w = Writer::new();
        rec.encode(&mut w);
        assert_eq!(TxRecord::decode(&mut Reader::new(&w.buf)).unwrap(), rec);
    }

    #[test]
    fn an_old_log_with_a_retired_receipt_reveal_still_reads() {
        // a v2.4 record: one covenant input revealing a KobReceipt (code 7, 120-byte state span), no outputs
        for code in [7u8, 0x87] {
            let mut w = Writer::new();
            w.hash(&Hash32([1; 32]));
            w.var(4);
            w.bytes(&[]);
            w.var(1);
            w.u8(3);
            w.hash(&Hash32([2; 32]));
            w.var(0);
            w.hash(&Hash32([3; 32]));
            w.var(77);
            w.u8(code);
            w.raw(&[0x5a; 120]);
            w.raw(&[1, 2, 3, 4]);
            w.var(1);
            w.bytes(&[9]);
            w.var(0);
            w.var(0);
            let (rec, stats) = TxRecord::decode_v1(&w.buf).unwrap();
            assert_eq!(stats.retired_reveals, 1);
            assert_eq!(rec.inputs.len(), 1);
            assert_eq!(rec.inputs[0].cov, Some(Hash32([3; 32])));
            assert!(rec.inputs[0].reveal.is_none(), "the receipt reveal is dropped");
        }
        // any other unknown code is still an error
        let mut w = Writer::new();
        w.hash(&Hash32([1; 32]));
        w.var(4);
        w.bytes(&[]);
        w.var(1);
        w.u8(3);
        w.hash(&Hash32([2; 32]));
        w.var(0);
        w.hash(&Hash32([3; 32]));
        w.var(77);
        w.u8(0x7e);
        assert!(TxRecord::decode_v1(&w.buf).is_err());
    }

    /// A KOB1 payload of version 3 (the protocol v2.6 lot templates, retired by v3) decodes into retired records: the
    /// transaction is kept with a stand-in that names their kinds (the processor counts each as a reject), never as orders.
    #[test]
    fn a_placement_of_a_retired_template_is_kept_as_a_counted_stand_in() {
        let path = concat!(env!("CARGO_MANIFEST_DIR"), "/../kob-protocol/vectors/payload_v3.json");
        let v: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
        let bytes = crate::hex::decode(v["payloads"][0]["payload"].as_str().unwrap()).unwrap();
        assert!(matches!(payload::decode(&bytes).unwrap().unwrap().records[0], Record::RetiredOrder { .. }));
        let conn = crate::indexer::db::open_memory("testnet-10").unwrap();
        let tokens = crate::tokens::TokenAllowlist::default();
        let ex = Extractor { conn: &conn, tokens: &tokens, track_traded: true };
        let mut tx = crate::testkit::noise_tx(1, false);
        tx.payload = crate::hex::HexBytes(bytes);
        let rec = ex.extract(&tx, 0).unwrap().expect("relevant: the retired placement is counted");
        assert_eq!(retired_payload_kinds(&rec.payload), Some(vec!["KobAsk".to_string()]));
        assert!(payload::decode(&rec.payload).is_err(), "the stand-in never decodes as a payload");
        // it survives the record log
        let mut w = Writer::new();
        rec.encode(&mut w);
        assert_eq!(TxRecord::decode(&mut Reader::new(&w.buf)).unwrap(), rec);
        assert_eq!(retired_payload_kinds(b"KOB1\xff"), None);
    }

    #[test]
    fn kron_token_inputs_authorise_their_next_states() {
        use kob_protocol::script::kron_token_sigscript;
        let next = vec![KronState::custody(5_000, [9; 32]), KronState::addr(1_000, [8; 32])];
        let prog = token_template(TemplateId::KronToken2433);
        let redeem = KronState::addr(6_000, [8; 32]).redeem_with(prog);
        let sig = kron_token_sigscript(&redeem, &next, &[], &[0, 3]);
        let (id, got) = kron_next_states(&sig).expect("a KRON token input");
        assert_eq!((id, got.clone()), (TemplateId::KronToken2433, next.clone()));
        let (_, any) = token_next_states(&sig).unwrap();
        assert_eq!(any, next.into_iter().map(TokenState::Kron).collect::<Vec<_>>());
        // not a KRON token input: a P2PK signature push
        assert!(token_next_states(&crate::script::parse_pushes(&[0x01, 0x00]).map(|_| vec![0x01u8, 0x00]).unwrap()).is_none());
    }
}
