//! Extracted transaction records: the only chain data the indexer keeps.
//!
//! A raw VSPC response is about 15 GB per day on TN10 at `High` verbosity, because signature
//! scripts carry whole KCC-20 programs (docs/ops/executor.md, "Storage"). Nothing of it is stored.
//! For every transaction that touches KOB the extractor keeps a *reduced transaction*:
//!
//! - outpoints, output values and covenant bindings,
//! - the KOB1 placement payload,
//! - for every spent KOB order input, the revealed template, its state span, the entry that was
//!   called and the small arguments.
//!
//! Signature scripts, token programs and everything unrelated to KOB are dropped. The reduced form
//! is exactly what the processor consumes, so the live path and a rebuild from the permanent record
//! log run the same code.
//!
//! # Relevance
//!
//! What is kept is decided against the database at extraction time. A transaction is kept when:
//!
//! - its payload is a KOB1 placement record, or claims to be one and does not decode (kept as a
//!   reject),
//! - an input spends a tracked order or token UTXO,
//! - a token output is owned (scheme `0x04`) by a tracked or freshly created order id: custody
//!   after a fill, a delivery to an if-done exit, or a stray (`docs/spec/matcher.md` §1.2). The
//!   token output states are read from the transfer leader's `next_states` argument, and each one
//!   is checked against the output's script public key,
//! - an output of ANY token of a pinned token program (listed or not, traded by an order or not) is
//!   a *holding* (`HeldOut`), whoever owns it. The creating input must reveal its state (a
//!   transfer leader's `next_states`, the columns of a KRON token input, or the `recipient_state`
//!   of a public-mint app's `mint` / `create`), and the output's script public key must prove it.
//!   An input that spends a tracked holding is relevant too, because it marks the holding spent.
//!   Outputs of a program this build does not pin, or of a look-alike, prove nothing and are not
//!   kept. Neither are burned outputs: a state at a burn owner that the transaction's kUSDT
//!   redemption payload names (`kob_protocol::kusdt::is_burned`) can never move,
//! - the transaction is a KCC-23 candidate preparation or a verified genesis of one
//!   (`super::kcc23`). Its record carries a declaration section (the transaction's id-relevant
//!   fields), and nothing else unless it is relevant to KOB too.

use super::db::DbResult;
use super::kcc23::DeclSection;
use crate::hex::Hash32;
use crate::model;
use crate::rpc::types::{Tx, TxInput};
use crate::script::{p2sh_spk, parse_pushes, parse_spk};
use crate::wire::{Reader, WireError, WireResult, Writer};
use kob_protocol::artifacts::{template, token_template, TemplateId};
use kob_protocol::payload::{self, Record};
use kob_protocol::state::{AnyState, Kcc20State, KronState, TokenState};
use rusqlite::{params, Connection, OptionalExtension};
use std::collections::{HashMap, HashSet};

/// Arguments longer than this are elided: template prefix and suffix pushes and signatures.
const MAX_ARG_BYTES: usize = 40;

/// A spent KOB order input: what its signature script revealed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Reveal {
    /// Template of the spent order script.
    pub template: TemplateId,
    /// The state span of the spent script (exact; the redeem script hashes to the spent output).
    pub state: Vec<u8>,
    /// Dispatch tag of the entry that was called.
    pub tag: [u8; 4],
    /// The entry arguments in ABI order; long ones (more than 40 bytes) are empty placeholders.
    pub args: Vec<Vec<u8>>,
}

impl Reveal {
    /// Name of the entry that was called, when the tag is known.
    pub fn entry(&self) -> Option<&'static str> {
        model::entry_name(self.template, self.tag)
    }

    /// Argument `i` as a script number.
    pub fn int(&self, i: usize) -> Option<i64> {
        crate::script::script_int(self.args.get(i)?)
    }

    /// The first argument as an `nb` push ([`model::nb`]).
    pub fn nb(&self) -> Option<model::Nb> {
        model::nb(self.args.first()?)
    }
}

/// An input of a record.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecIn {
    /// Transaction id of the spent output.
    pub txid: Hash32,
    /// Index of the spent output.
    pub index: u32,
    /// Covenant id of the spent output.
    pub cov: Option<Hash32>,
    /// DAA score of the block that created the spent output (covenant inputs only).
    pub daa: u64,
    /// The reveal of the input's signature script, when it spends a tracked order.
    pub reveal: Option<Reveal>,
}

/// An output of a record.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecOut {
    /// Value in sompi.
    pub value: u64,
    /// Version (2 bytes) + script; kept for covenant-bound outputs only.
    pub spk: Vec<u8>,
    /// `(authorizing input, covenant id)`.
    pub cov: Option<(u32, Hash32)>,
}

/// A token output of a pinned token program whose state was proven against its script public key.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HeldOut {
    /// Index of the output.
    pub out: u32,
    /// The token program the output's script belongs to.
    pub program: TemplateId,
    /// The proven state.
    pub state: TokenState,
}

/// One-byte record-log code of a token program.
///
/// It is the `record_code` of the program's profile (`kob_protocol::profile`). Codes are
/// persisted and never reused.
pub fn program_code(t: TemplateId) -> Option<u8> {
    t.record_code()
}

/// The token program of a record-log code, when this build has it.
pub fn program_from_code(c: u8) -> Option<TemplateId> {
    TemplateId::from_record_code(c)
}

/// A token output owned by an order covenant id (owner scheme `0x04`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TokOut {
    /// Index of the output.
    pub out: u32,
    /// Token amount in base units.
    pub amount: i64,
    /// Covenant id of the owning order.
    pub owner: Hash32,
}

/// A reduced transaction: what the extractor keeps of a transaction that is relevant to KOB.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TxRecord {
    /// Transaction id.
    pub txid: Hash32,
    /// Position in the chain block's accepted-transaction list.
    pub pos: u32,
    /// The KOB1 payload (placement records), empty otherwise.
    pub payload: Vec<u8>,
    /// The inputs, in transaction order.
    pub inputs: Vec<RecIn>,
    /// The outputs, in transaction order.
    pub outputs: Vec<RecOut>,
    /// Token outputs owned by an order covenant id.
    pub tok_outs: Vec<TokOut>,
    /// Proven states of tracked-token outputs (see the module docs).
    ///
    /// A frame without the section decodes with an empty list. The flag is the top bit of the
    /// encoded `pos`, because a block never holds 2^31 transactions.
    pub holds: Vec<HeldOut>,
    /// The KCC-23 declaration section (`super::kcc23`).
    ///
    /// It is a candidate preparation, or a genesis with the preparations it binds. It is flagged by
    /// bit 30 of the encoded `pos`. A frame holding one is written as record-log format 3
    /// (`recordlog::FORMAT_KCC23`).
    pub kcc23: Option<DeclSection>,
    /// The script public key (version and script) of the output that funded the transaction.
    ///
    /// That output is the one spent by the first input that spends no covenant output, the
    /// submitter's own funds such as an Executor's P2PK funding. It is kept for a transaction that
    /// spends a tracked order and has such an input. It is `None` otherwise, and in a record
    /// written before this section existed, where the submitter is unknown. It is flagged by bit 29
    /// of the encoded `pos`. A frame holding one is written as record-log format 4
    /// (`recordlog::FORMAT_PAYER`).
    pub payer: Option<Vec<u8>>,
    /// The sum of the values of the outputs that the transaction spent.
    ///
    /// The node reports each value (VSPC v2 `utxoEntry.amount`, `High` verbosity). The network fee
    /// is this sum minus the sum of the outputs, which the record keeps in full. It is kept for a
    /// transaction that spends a tracked order when the node reported the value of every input. It
    /// is `None` otherwise, and in a record written before this section existed, where the fee is
    /// unknown. It is flagged by bit 28 of the encoded `pos`. A frame holding one is written as
    /// record-log format 5 (`recordlog::FORMAT_IN_VALUE`).
    pub in_value: Option<u64>,
}

/// Stand-in stored for a KOB1 payload that did not decode.
///
/// It is `KOB1` and a version byte no reader supports. The record keeps the fact of the reject,
/// not the junk. The processor reports it as `payload:undecodable`.
pub const UNDECODABLE_PAYLOAD: &[u8] = b"KOB1\xff";

/// Set in the encoded `pos` when the record has a `holds` section.
const HOLDS_FLAG: u64 = 1 << 31;
/// Set in the encoded `pos` when the record has a KCC-23 declaration section.
const KCC23_FLAG: u64 = 1 << 30;
/// Set in the encoded `pos` when the record has a payer section ([`TxRecord::payer`]).
const PAYER_FLAG: u64 = 1 << 29;
/// Set in the encoded `pos` when the record has an input-value section ([`TxRecord::in_value`]).
const IN_VALUE_FLAG: u64 = 1 << 28;
/// The longest payer script kept, in bytes.
///
/// A standard script public key is at most 37 bytes with its version. A longer one is not kept.
pub const MAX_PAYER_SPK: usize = 64;

fn code_of(t: TemplateId) -> u8 {
    model::wire_code(t)
}

/// A frame's template table (record-log format 2).
///
/// It maps the wire code to the template hash of every template the frame refers to.
pub type TemplateTable = std::collections::BTreeMap<u8, [u8; 32]>;

/// What decoding a frame had to drop, because this build does not know its layout.
///
/// The frame itself is kept in the log, which is hash-chained and never rewritten. A later build
/// that knows the layout reads it.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct DecodeStats {
    /// Order reveals of a template this build does not pin, dropped.
    pub unknown_reveals: u64,
    /// Token holdings of a token program layout this build does not know, dropped.
    pub unknown_holds: u64,
    /// Recovery imports of an order layout this build does not know, dropped.
    pub unknown_imports: u64,
}

impl DecodeStats {
    /// Adds the counts of `o`.
    pub fn add(&mut self, o: &DecodeStats) {
        self.unknown_reveals += o.unknown_reveals;
        self.unknown_holds += o.unknown_holds;
        self.unknown_imports += o.unknown_imports;
    }

    /// Items dropped (reveals, holdings, imports).
    pub fn dropped(&self) -> u64 {
        self.unknown_reveals + self.unknown_holds + self.unknown_imports
    }
}

/// How a record being decoded is laid out (the frame's template tables) and what was dropped.
pub struct DecodeCtx<'a> {
    /// The frame's tables (`None`: this build's templates).
    orders: Option<&'a TemplateTable>,
    programs: Option<&'a TemplateTable>,
    pub stats: DecodeStats,
}

impl<'a> DecodeCtx<'a> {
    /// This build's templates (a record encoded by this build on its own).
    pub fn current() -> DecodeCtx<'static> {
        DecodeCtx { orders: None, programs: None, stats: DecodeStats::default() }
    }

    /// A frame's tables.
    pub fn v2(orders: &'a TemplateTable, programs: &'a TemplateTable) -> DecodeCtx<'a> {
        DecodeCtx { orders: Some(orders), programs: Some(programs), stats: DecodeStats::default() }
    }

    /// The pinned template that an order code of the frame refers to.
    ///
    /// It is `None` for a template this build does not pin.
    fn order_layout(&self, code: u8) -> WireResult<Option<TemplateId>> {
        let current = model::from_wire_code(code);
        let Some(table) = self.orders else { return Ok(current) };
        let hash = table.get(&code).ok_or(WireError("template code missing from the frame's table"))?;
        Ok(current.filter(|id| &template(*id).hash == hash))
    }

    /// The token program a holding's code refers to, when this build has that exact program.
    fn program(&self, code: u8) -> WireResult<Option<TemplateId>> {
        let current = program_from_code(code);
        let Some(table) = self.programs else { return Ok(current) };
        let hash = table.get(&code).ok_or(WireError("token program code missing from the frame's table"))?;
        Ok(current.filter(|p| &token_template(*p).hash == hash))
    }

    /// Reads a reveal after its flag.
    ///
    /// It is `Some` when this build can interpret the reveal, and `None` when it was dropped.
    fn reveal(&mut self, r: &mut Reader<'_>) -> WireResult<Option<Reveal>> {
        let code = r.u8()?;
        let state = r.bytes()?.to_vec();
        let tag: [u8; 4] = r.raw(4)?.try_into().expect("4");
        let args = read_args(r)?;
        Ok(match self.order_layout(code)? {
            Some(id) if state.len() == template(id).state_len => Some(Reveal { template: id, state, tag, args }),
            _ => {
                self.stats.unknown_reveals += 1;
                None
            }
        })
    }

    /// Reads a holding: `None` when its program layout is not this build's (dropped).
    fn hold(&mut self, r: &mut Reader<'_>) -> WireResult<Option<HeldOut>> {
        let out = r.var()? as u32;
        let code = r.u8()?;
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

    /// An imported order's template: `None` (counted) when this build cannot interpret its state.
    pub(crate) fn import_template(&mut self, code: u8, state_len: usize) -> WireResult<Option<TemplateId>> {
        let id = self.order_layout(code)?.filter(|id| template(*id).state_len == state_len);
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

impl TxRecord {
    /// Encodes the record in record-log format 2.
    ///
    /// States are length-prefixed, and the frame carries the template tables
    /// ([`TxRecord::templates`]). With a KCC-23 declaration section, the frame is format 3
    /// (`recordlog::FORMAT_KCC23`).
    pub fn encode(&self, w: &mut Writer) {
        w.hash(&self.txid);
        w.var(
            self.pos as u64
                | if self.holds.is_empty() { 0 } else { HOLDS_FLAG }
                | if self.kcc23.is_none() { 0 } else { KCC23_FLAG }
                | if self.payer.is_none() { 0 } else { PAYER_FLAG }
                | if self.in_value.is_none() { 0 } else { IN_VALUE_FLAG },
        );
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
                w.bytes(&r.state);
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
                w.bytes(&h.state.encode());
            }
        }
        if let Some(d) = &self.kcc23 {
            d.encode(w);
        }
        if let Some(p) = &self.payer {
            w.bytes(p);
        }
        if let Some(v) = self.in_value {
            w.var(v);
        }
    }

    /// Adds the templates the record uses to a frame's tables.
    ///
    /// They are the order reveals and the holding programs.
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

    /// Decodes a record with the template tables of `ctx`.
    ///
    /// Items that this build cannot read are dropped and counted in `ctx`.
    pub fn decode_in(r: &mut Reader<'_>, ctx: &mut DecodeCtx<'_>) -> WireResult<TxRecord> {
        let txid = r.hash()?;
        let raw_pos = r.var()?;
        let has_holds = raw_pos & HOLDS_FLAG != 0;
        let has_kcc23 = raw_pos & KCC23_FLAG != 0;
        let has_payer = raw_pos & PAYER_FLAG != 0;
        let has_in_value = raw_pos & IN_VALUE_FLAG != 0;
        let pos = (raw_pos & !(HOLDS_FLAG | KCC23_FLAG | PAYER_FLAG | IN_VALUE_FLAG)) as u32;
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
        let kcc23 = if has_kcc23 { Some(DeclSection::decode(r)?) } else { None };
        let payer = if has_payer {
            let spk = r.bytes()?;
            if spk.len() > MAX_PAYER_SPK {
                return Err(WireError("payer script too long"));
            }
            Some(spk.to_vec())
        } else {
            None
        };
        let in_value = if has_in_value { Some(r.var()?) } else { None };
        Ok(TxRecord { txid, pos, payload, inputs, outputs, tok_outs, holds, kcc23, payer, in_value })
    }

    /// The record of a transaction relevant only for its KCC-23 declaration section.
    ///
    /// It has nothing of KOB: no payload, inputs or outputs, because the section carries the
    /// transaction's fields.
    pub fn declaration_only(txid: Hash32, pos: u32, d: DeclSection) -> TxRecord {
        TxRecord {
            txid,
            pos,
            payload: vec![],
            inputs: vec![],
            outputs: vec![],
            tok_outs: vec![],
            holds: vec![],
            kcc23: Some(d),
            payer: None,
            in_value: None,
        }
    }

    /// The record has nothing of KOB in it: only a declaration section.
    pub fn is_declaration_only(&self) -> bool {
        self.payload.is_empty()
            && self.inputs.is_empty()
            && self.outputs.is_empty()
            && self.tok_outs.is_empty()
            && self.holds.is_empty()
    }

    /// Length of the encoded record in bytes.
    pub fn encoded_len(&self) -> usize {
        let mut w = Writer::new();
        self.encode(&mut w);
        w.buf.len()
    }
}

// ----------------------------------------------------------------------------------------- lookups

/// DAA score of the chain block that created a tracked, unspent order UTXO.
///
/// It is what `OpTxInputDaaScore` reads: the accepting block's DAA score. The node does not report
/// it per input (`blockDaaScore` is `null` in VSPC v2 at every verbosity), so the indexer keeps it
/// from the block that created the UTXO.
pub fn order_utxo_daa(conn: &Connection, txid: &Hash32, idx: u32) -> DbResult<Option<i64>> {
    Ok(conn
        .prepare_cached("SELECT created_daa FROM order_utxos WHERE txid = ?1 AND idx = ?2 AND spent_block IS NULL")?
        .query_row(params![&txid.0[..], idx], |r| r.get(0))
        .optional()?)
}

/// Whether `txid:idx` is a tracked, unspent token UTXO of an order.
pub fn token_utxo_tracked(conn: &Connection, txid: &Hash32, idx: u32) -> DbResult<bool> {
    Ok(conn
        .prepare_cached("SELECT 1 FROM token_utxos WHERE txid = ?1 AND idx = ?2 AND spent_block IS NULL")?
        .exists(params![&txid.0[..], idx])?)
}

/// Whether an order with covenant id `cov` is in the database.
pub fn order_known(conn: &Connection, cov: &Hash32) -> DbResult<bool> {
    Ok(conn.prepare_cached("SELECT 1 FROM orders WHERE covenant_id = ?1")?.exists([&cov.0[..]])?)
}

/// Whether `txid:idx` is a tracked, unspent token holding.
pub fn holding_tracked(conn: &Connection, txid: &Hash32, idx: u32) -> DbResult<bool> {
    Ok(conn
        .prepare_cached("SELECT 1 FROM token_holdings WHERE txid = ?1 AND idx = ?2 AND spent_block IS NULL")?
        .exists(params![&txid.0[..], idx])?)
}

/// Whether some order trades the token, as the token of an order or as token B of a pair order.
///
/// So strays of B sent to a pair order's id are recorded as strays of that order.
pub fn token_known(conn: &Connection, token: &Hash32) -> DbResult<bool> {
    Ok(conn
        .prepare_cached("SELECT 1 FROM orders WHERE token_id = (SELECT id FROM ids WHERE h = ?1) LIMIT 1")?
        .exists([&token.0[..]])?
        || conn
            .prepare_cached("SELECT 1 FROM orders WHERE quote_id = (SELECT id FROM ids WHERE h = ?1) LIMIT 1")?
            .exists([&token.0[..]])?)
}

// -------------------------------------------------------------------------------------- extraction

fn spent_of(inp: &TxInput) -> Option<&crate::rpc::types::SpentUtxo> {
    inp.verbose_data.as_ref().and_then(|v| v.utxo_entry.as_ref())
}

/// The reveal of a covenant input's signature script, verified against the spent script public key.
pub fn parse_reveal(sigscript: &[u8], spent_spk: &[u8]) -> Option<Reveal> {
    let pushes = parse_pushes(sigscript)?;
    if pushes.len() < 2 {
        return None;
    }
    let redeem = pushes.last()?;
    let tag: [u8; 4] = pushes[pushes.len() - 2].as_slice().try_into().ok()?;
    // Identify first (prefix and suffix compared with the pinned templates). Only an order's redeem
    // script is hashed, never the token program that a transfer input reveals (up to 25 KB).
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

/// Whether the reveal is a fill that is trigger evidence.
///
/// That is a plain resting `KobAsk` / `KobBid` (either family), or a resting `KobPair`, spent by a
/// fill (`n > 0`). A `KobPair` is the mode-1 evidence of the pair conditionals of its pair.
fn is_evidence_fill(r: &Reveal) -> bool {
    matches!(
        r.template.canonical(),
        TemplateId::KobAsk | TemplateId::KobAskKron | TemplateId::KobBid | TemplateId::KobBidKron | TemplateId::KobPair
    ) && matches!(r.nb(), Some(model::Nb::Fill(n)) if n > 0)
}

/// A token transfer's leader as the extractor matches it against outputs.
///
/// It is the program and every next state with the P2SH script public key it produces, computed
/// once per state and not once per output and state.
pub type Leader = (TemplateId, Vec<(TokenState, kaspa_consensus_core::tx::ScriptPublicKey)>);

/// The states a covenant input authorises, each with its script public key under the program.
///
/// These are a token transfer's [`token_next_states`], else the holder that a public-mint app's
/// `mint` / `create` call creates ([`kob_protocol::public_mint::created_holder`]).
pub fn leader_of(sigscript: &[u8]) -> Option<Leader> {
    let (tid, states) = token_next_states(sigscript)
        .or_else(|| kob_protocol::public_mint::created_holder(sigscript).map(|(id, s)| (id, vec![TokenState::Kcc20(s)])))?;
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

/// The candidate states a token genesis names in its payload ([`kob_protocol::genesis_states`]).
///
/// There is one [`Leader`] per pinned token program that can read every span as its own state,
/// each with the script public key it produces. Nothing is believed here: the caller keeps a state
/// only where that script is the output's. The result is empty for any other payload.
///
/// It is empty too for a payload that names more states than the transaction has covenant outputs,
/// `cov_outputs`: a genesis names one per token output. Each state is hashed under a token program
/// of up to about 25 KB, and a payload of 255 states would cost some 20 MB of hashing per
/// transaction, for about 29 KB of payload.
pub fn genesis_leaders(payload: &[u8], cov_outputs: usize) -> Vec<Leader> {
    let Some(spans) = kob_protocol::genesis_states::decode(payload) else { return vec![] };
    if spans.len() > cov_outputs {
        return vec![];
    }
    TemplateId::TOKEN_PROGRAMS
        .into_iter()
        .filter_map(|tid| {
            let tpl = token_template(tid);
            let states = spans
                .iter()
                .map(|b| TokenState::decode_with(tpl, b).ok().map(|s| (s.clone(), s.spk_with(tpl))))
                .collect::<Option<Vec<_>>>()?;
            Some((tid, states))
        })
        .collect()
}

/// The database-free work of one transaction, done before the single-threaded database pass.
///
/// It covers signature-script parsing, script hashing and placement verification, and runs in
/// parallel over a batch ([`precompute`]). Every entry is a pure function of the transaction, so a
/// cache hit gives exactly what the database pass would have computed itself.
#[derive(Debug, Default)]
pub struct PreTx {
    /// Reveal of each covenant input; `None` when the redeem script is not an order template.
    pub reveals: HashMap<u32, Option<Reveal>>,
    /// Authorising inputs of covenant outputs: their transfer leader, if they are one.
    pub leaders: HashMap<u32, Option<Leader>>,
    /// The [`genesis_leaders`] of the payload, when an authorising input is no leader.
    pub genesis: Option<Vec<Leader>>,
    /// `payload::recover_orders` of each single ORDER record of the payload, keyed by the
    /// single-record payload bytes.
    pub placements: HashMap<Vec<u8>, Result<Vec<payload::RecoveredOrder>, String>>,
}

/// Pure results of a batch, by transaction id.
pub type PreBatch = HashMap<Hash32, PreTx>;

impl PreTx {
    /// The pure work of `tx`.
    ///
    /// It is `None` for a transaction without a KOB1 payload or covenant input / output.
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
        if pre.leaders.values().any(Option::is_none) {
            pre.genesis = Some(genesis_leaders(&tx.payload.0, cov_outputs(tx)));
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

/// The covenant outputs of a transaction.
fn cov_outputs(tx: &Tx) -> usize {
    tx.outputs.iter().filter(|o| o.covenant.is_some()).count()
}

/// The reduced shape of a raw transaction, as far as `processor::tx_json` reads it.
///
/// `processor::tx_json` reads it for placement verification: outpoints, covenant ids of the spent
/// outputs, output values, and scripts and bindings of covenant outputs.
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
        kcc23: None,
        payer: None,
        in_value: None,
    }
}

/// [`PreTx::compute`] for every transaction of `blocks`, on up to `threads` threads.
///
/// It runs inline below 64 candidate transactions, where a thread costs more than it saves.
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

/// The next states of a KRON token input, from its signature script.
///
/// The script is built by `kob_protocol::script::kron_columns_sigscript`. Its pushes are the
/// columns `owners`, `id_types`, `amounts` and `is_minters` of the whole covenant group, the
/// signature column, the witness column, the program's constant trailing pushes (profile
/// `witness.trailing_pushes`: none for the KRON programs, an entry selector for a multi-entry
/// program) and the redeem script. Every token input of the token carries the same columns.
pub fn kron_next_states(sigscript: &[u8]) -> Option<(TemplateId, Vec<KronState>)> {
    let pushes = parse_pushes(sigscript)?;
    let redeem = pushes.last()?;
    let (id, _) = model::identify_kron_token(redeem)?;
    let trailing = id.witness_trailing();
    if pushes.len() != 7 + trailing.len() || pushes[6..pushes.len() - 1] != trailing[..] {
        return None;
    }
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
/// The `State[]` argument is a KCC-1 record array: one push per field, namely amounts as 8-byte
/// signed magnitude, owners, owner schemes, borrow schemes, borrow guards and extension
/// commitments. Then come the witness, the dispatch tag and the redeem script.
/// [`kob_protocol::kcc20::transfer_next_states`] decodes it strictly.
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

/// The script public key of the output spent by the first input of `tx` that spends no covenant.
///
/// That is the submitter's own funds. It is returned when the node reported it and it is of a
/// standard length ([`MAX_PAYER_SPK`]).
pub fn payer_of(tx: &Tx) -> Option<Vec<u8>> {
    let u = tx.inputs.iter().filter_map(spent_of).find(|u| u.covenant_id.is_none())?;
    let spk = &u.script_public_key.0;
    (!spk.is_empty() && spk.len() <= MAX_PAYER_SPK).then(|| spk.clone())
}

/// The sum of the values of the outputs `tx` spends, when the node reported every one.
///
/// It is `None` otherwise, and the fee is then unknown.
pub fn in_value_of(tx: &Tx) -> Option<u64> {
    tx.inputs.iter().try_fold(0u64, |s, i| s.checked_add(spent_of(i)?.amount?))
}

/// Turns accepted transactions into records, consulting the database for relevance.
pub struct Extractor<'a> {
    /// The database that relevance is decided against.
    pub conn: &'a Connection,
}

impl Extractor<'_> {
    /// Extracts the record of `tx`, which is `Ok(None)` when the transaction is unrelated to KOB.
    pub fn extract(&self, tx: &Tx, pos: u32) -> DbResult<Option<TxRecord>> {
        self.extract_with(tx, pos, None)
    }

    /// [`Extractor::extract`] with the transaction's precomputed pure work ([`PreTx`]).
    ///
    /// Without it, everything is computed here.
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
        // KCC-23: a candidate preparation or a verified genesis, whatever else the transaction is.
        let kcc23 = super::kcc23::extract(self.conn, tx)?;
        let txid = tx.verbose_data.transaction_id;
        if !has_kob1 && !any_cov_in && !any_cov_out {
            return Ok(kcc23.map(|d| TxRecord::declaration_only(txid, pos, d)));
        }

        // Placement records: which orders (and tokens) this transaction announces.
        let mut relevant = false;
        let mut payload_tokens: HashSet<Hash32> = HashSet::new();
        // What of the payload the record keeps. The log is permanent and replayed on every rebuild,
        // so a payload is never stored as sent. Only the order, amend and sweep records survive,
        // and a release record when it is newer than this build. At most one record per output of
        // the transaction is kept, re-encoded canonically. An undecodable payload is replaced by a
        // five-byte stand-in that the processor rejects.
        let mut kept: Vec<Record> = vec![];
        let mut undecodable = false;
        if has_kob1 {
            match payload::decode(&tx.payload.0) {
                Ok(Some(p)) => {
                    // The release a placement announces is kept with it only when it is newer than
                    // this build (the update notice, `super::release`). A build that replays the
                    // log later is this one or newer, for which an older release never matters, so
                    // the log is unchanged whenever there is nothing to tell.
                    let newer_release = payload::release_of(&p).filter(|r| r.is_newer_than_build());
                    for r in p.records {
                        if let Record::Order { template: t, state, .. } = &r {
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
                            // An in-place amend or sweep matters only next to a tracked order
                            // input, which makes the transaction relevant.
                            kept.push(r);
                        }
                    }
                    if let Some(version) = newer_release.filter(|_| kept.iter().any(|r| matches!(r, Record::Order { .. }))) {
                        kept.push(Record::Release { version });
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
        // Untracked covenant inputs: possible trigger evidence, read only if the transaction turns
        // out relevant.
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
                    rec.daa = u64::try_from(daa).unwrap_or(0);
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
        let mut genesis: Option<Vec<Leader>> = None;
        let mut used: HashSet<(u32, usize)> = HashSet::new();
        // The burns a kUSDT redemption payload names. An output proven to hold such a state is
        // burned (its owner is the hash of a script that starts with OP_RETURN), not a holding.
        let burns = kob_protocol::kusdt::redemption_entries(&tx.payload.0).unwrap_or_default();
        for (j, o) in tx.outputs.iter().enumerate() {
            let Some(c) = &o.covenant else { continue };
            let token = c.covenant_id;
            let order_gate = payload_tokens.contains(&token) || token_known(self.conn, &token)?;
            // Every token output is a holding candidate (a wallet's balance of a token no order
            // trades, received from another trading app, a launchpad or a bridge). It may also be
            // owned by an order id, a FOREIGN stray (matcher.md 1.2), which the indexer must flag.
            // The leader is read for every output except one of a token no order trades whose
            // authorising input is a tracked order, because an order's continuation is never a
            // token output. That is one signature-script parse per token transfer.
            if !order_gate && inputs.get(c.authorizing_input as usize).is_some_and(|i: &RecIn| i.reveal.is_some() || i.daa > 0) {
                continue;
            }
            let leader =
                leaders.entry(c.authorizing_input).or_insert_with(|| match pre.and_then(|p| p.leaders.get(&c.authorizing_input)) {
                    Some(l) => l.clone(),
                    None => tx.inputs.get(c.authorizing_input as usize).and_then(|i| leader_of(&i.signature_script.0)),
                });
            // No leader: a token genesis names the states of its outputs in its payload instead
            // (`genesis_states`), as one list of candidate states per pinned token program. Each is
            // believed only if its P2SH is the output's script public key.
            let genesis_cands: &[Leader] = match (leader.is_none(), pre.and_then(|p| p.genesis.as_deref())) {
                (false, _) => &[],
                (true, Some(g)) => g,
                (true, None) => genesis.get_or_insert_with(|| genesis_leaders(&tx.payload.0, cov_outputs(tx))).as_slice(),
            };
            let want = parse_spk(&o.script_public_key.0);
            'found: for (li, (tid, states)) in leader.iter().chain(genesis_cands.iter()).enumerate() {
                for (i, (s, spk)) in states.iter().enumerate() {
                    let k = (li << 16) | i;
                    if used.contains(&(c.authorizing_input, k)) || want.as_ref() != Some(spk) {
                        continue;
                    }
                    // The state is proven: the P2SH it produces is this output's script public key.
                    used.insert((c.authorizing_input, k));
                    if kob_protocol::kusdt::is_burned(&burns, j as u32, s) {
                        break 'found;
                    }
                    let owner = Hash32(s.owner());
                    let order_owned = s.is_covenant_owned()
                        && if order_gate {
                            fresh.contains(&owner) || order_known(self.conn, &owner)?
                        } else {
                            // A token no order trades: a foreign stray of an order the indexer
                            // already knows.
                            order_known(self.conn, &owner)?
                        };
                    if order_owned {
                        tok_outs.push(TokOut { out: j as u32, amount: s.amount(), owner });
                    }
                    // Every proven output is a holding, whoever owns it.
                    holds.push(HeldOut { out: j as u32, program: *tid, state: s.clone() });
                    relevant = true;
                    break 'found;
                }
            }
        }
        if !relevant {
            return Ok(kcc23.map(|d| TxRecord::declaration_only(txid, pos, d)));
        }
        // A fill of a plain KobAsk / KobBid that this indexer does not track (no placement
        // record). The covenants accept it as trigger evidence all the same, because they check
        // its template, not a record. So its quote is kept for the tracked conditional orders of
        // the transaction (a trailing ratchet is derived from it). It does not make the
        // transaction relevant by itself.
        for i in untracked {
            let inp = &tx.inputs[i];
            let spk = spent_of(inp).map(|u| u.script_public_key.0.as_slice()).unwrap_or(&[]);
            inputs[i].reveal = reveal_of(i, inp, spk).filter(is_evidence_fill);
        }
        let payload = if undecodable {
            UNDECODABLE_PAYLOAD.to_vec()
        } else if kept.is_empty() {
            vec![]
        } else {
            payload::encode(&kept).unwrap_or_else(|_| UNDECODABLE_PAYLOAD.to_vec())
        };
        // Who funded the transaction, when it spends a tracked order (a fill, a cancel, a keeper's
        // call): the script of the first input that spends no covenant output (an Executor's
        // funding), as the node reports it. Also what its inputs held, whose difference with its
        // outputs is its network fee (`activity`).
        let spends_order = inputs.iter().any(|i| i.daa > 0 || i.reveal.is_some());
        let payer = if spends_order { payer_of(tx) } else { None };
        let in_value = if spends_order { in_value_of(tx) } else { None };
        Ok(Some(TxRecord { txid, pos, payload, inputs, outputs, tok_outs, holds, kcc23, payer, in_value }))
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
            kcc23: None,
            payer: Some([&[0u8, 0, 0x20][..], &[7u8; 32], &[0xac]].concat()),
            in_value: Some(u64::MAX - 1),
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
        // Every order template has its own wire code. The KRON kinds share the KCC-20 kind codes
        // with the high bit set.
        for id in crate::model::ORDER_KINDS {
            let code = crate::model::wire_code(id);
            assert_eq!(crate::model::from_wire_code(code), Some(id), "{}", id.name());
            assert_eq!(code & 0x80 != 0, id.family() == kob_protocol::family::Family::Kron);
        }
        // A log written before the KRON family existed only has codes below 0x80.
        assert_eq!(crate::model::wire_code(TemplateId::KobAsk), 1);
        // The codes of the retired receipt template decode to no template.
        assert_eq!(crate::model::from_wire_code(7), None);
        assert_eq!(crate::model::from_wire_code(0x87), None);
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
            kcc23: None,
            payer: None,
            in_value: None,
        };
        let mut w = Writer::new();
        rec.encode(&mut w);
        assert_eq!(TxRecord::decode(&mut Reader::new(&w.buf)).unwrap(), rec);
    }

    /// A public-mint app's `mint` reveals the holder it creates.
    ///
    /// The minted output is a holding from its first block, with no transfer needed, for a token no
    /// order trades and no allowlist names. A claimed state that its output does not prove is not.
    #[test]
    fn a_minted_holder_is_a_holding_and_an_unproven_claim_is_not() {
        use crate::rpc::types::*;
        use kob_protocol::public_mint::{mint_sigscript, minter_redeem, MinterState};
        let conn = crate::indexer::db::open_memory("testnet-10").unwrap();
        let ex = Extractor { conn: &conn };
        let m = MinterState { remaining: 1_000, mint_amount: 500, owner: [7; 32], extension_commitment: [0xee; 32] };
        let holder = Kcc20State::p2pk(400, [3; 32], [0xee; 32]);
        let cov = Hash32([0x42; 32]);
        let holder_spk = TokenState::Kcc20(holder.clone()).spk_with(token_template(TemplateId::Kcc20PublicMint));
        let tx = |out_spk: Vec<u8>| Tx {
            inputs: vec![TxInput {
                previous_outpoint: Outpoint { transaction_id: Hash32([1; 32]), index: 0 },
                signature_script: crate::hex::HexBytes(mint_sigscript(&m, &holder).unwrap()),
                sequence: Some(0),
                verbose_data: Some(InputVerbose {
                    utxo_entry: Some(SpentUtxo {
                        amount: Some(5_000),
                        script_public_key: crate::hex::HexBytes(p2sh_spk(&minter_redeem(&m).unwrap())),
                        block_daa_score: None,
                        covenant_id: Some(cov),
                    }),
                }),
            }],
            outputs: vec![TxOutput {
                value: 1_000,
                script_public_key: crate::hex::HexBytes(out_spk),
                covenant: Some(CovenantBinding { authorizing_input: 0, covenant_id: cov }),
            }],
            payload: crate::hex::HexBytes(vec![]),
            verbose_data: TxVerbose { transaction_id: Hash32([2; 32]), block_time: 0 },
            version: Some(1),
            lock_time: Some(0),
            subnetwork_id: None,
            gas: None,
        };
        let rec = ex.extract(&tx(crate::script::spk_bytes(&holder_spk)), 0).unwrap().expect("a minted holding is relevant");
        assert_eq!(
            rec.holds,
            vec![HeldOut { out: 0, program: TemplateId::Kcc20PublicMint, state: TokenState::Kcc20(holder.clone()) }]
        );
        // The same claim next to an output of another amount (or another program) proves nothing:
        // not relevant at all.
        let other =
            TokenState::Kcc20(Kcc20State::p2pk(401, [3; 32], [0xee; 32])).spk_with(token_template(TemplateId::Kcc20PublicMint));
        assert!(ex.extract(&tx(crate::script::spk_bytes(&other)), 0).unwrap().is_none());
        let look_alike = TokenState::Kcc20(holder.clone()).spk_with(token_template(TemplateId::Kcc20Ref));
        assert!(ex.extract(&tx(crate::script::spk_bytes(&look_alike)), 0).unwrap().is_none());
    }

    /// A token genesis names the states of its outputs in its payload (`genesis_states`).
    ///
    /// A state is a holding when the P2SH of its state under a pinned program is the output's
    /// script, whatever the order of the names. Nothing is believed otherwise: another amount, a
    /// payload that is not the exact encoding or no payload leave the transaction unrelated.
    #[test]
    fn a_genesis_names_its_outputs_and_an_unproven_claim_is_not_believed() {
        use crate::rpc::types::*;
        let conn = crate::indexer::db::open_memory("testnet-10").unwrap();
        let ex = Extractor { conn: &conn };
        let cov = Hash32([0x42; 32]);
        let tpl = token_template(TemplateId::Kcc20Ref);
        let (mine, theirs) = (Kcc20State::p2pk(700, [3; 32], [0xee; 32]), Kcc20State::p2pk(300, [4; 32], [0xee; 32]));
        let spk_of = |s: &Kcc20State| crate::script::spk_bytes(&TokenState::Kcc20(s.clone()).spk_with(tpl));
        let tx = |outs: Vec<Vec<u8>>, payload: Vec<u8>| Tx {
            inputs: vec![TxInput {
                previous_outpoint: Outpoint { transaction_id: Hash32([1; 32]), index: 0 },
                // A plain P2PK spend: no leader, no token program in the signature script.
                signature_script: crate::hex::HexBytes(vec![0x41; 65]),
                sequence: Some(0),
                verbose_data: Some(InputVerbose {
                    utxo_entry: Some(SpentUtxo {
                        amount: Some(5_000),
                        script_public_key: crate::hex::HexBytes(vec![0, 0, 0x20, 1, 0xac]),
                        block_daa_score: None,
                        covenant_id: None,
                    }),
                }),
            }],
            outputs: outs
                .into_iter()
                .map(|spk| TxOutput {
                    value: 1_000,
                    script_public_key: crate::hex::HexBytes(spk),
                    covenant: Some(CovenantBinding { authorizing_input: 0, covenant_id: cov }),
                })
                .collect(),
            payload: crate::hex::HexBytes(payload),
            verbose_data: TxVerbose { transaction_id: Hash32([2; 32]), block_time: 0 },
            version: Some(1),
            lock_time: Some(0),
            subnetwork_id: None,
            gas: None,
        };
        let names = |states: &[&Kcc20State]| {
            kob_protocol::genesis_states::encode(&states.iter().map(|s| TokenState::Kcc20((*s).clone()).encode()).collect::<Vec<_>>())
                .unwrap()
        };
        let want = vec![
            HeldOut { out: 0, program: TemplateId::Kcc20Ref, state: TokenState::Kcc20(mine.clone()) },
            HeldOut { out: 1, program: TemplateId::Kcc20Ref, state: TokenState::Kcc20(theirs.clone()) },
        ];
        let outs = vec![spk_of(&mine), spk_of(&theirs)];
        for named in [names(&[&mine, &theirs]), names(&[&theirs, &mine])] {
            let rec = ex.extract(&tx(outs.clone(), named), 0).unwrap().expect("a genesis that names its outputs is relevant");
            assert_eq!(rec.holds, want);
        }
        // A name that is not the output's state proves nothing (the second output stays unnamed).
        let wrong = Kcc20State::p2pk(301, [4; 32], [0xee; 32]);
        let rec = ex.extract(&tx(outs.clone(), names(&[&mine, &wrong])), 0).unwrap().expect("one output is proven");
        assert_eq!(rec.holds, want[..1].to_vec());
        assert!(ex.extract(&tx(vec![outs[1].clone()], names(&[&mine, &wrong])), 0).unwrap().is_none());
        // More names than covenant outputs: refused unhashed, even where one would prove its output.
        assert!(ex.extract(&tx(vec![outs[0].clone()], names(&[&mine, &theirs])), 0).unwrap().is_none());
        assert!(genesis_leaders(&names(&[&mine, &theirs]), 1).is_empty());
        assert!(!genesis_leaders(&names(&[&mine, &theirs]), 2).is_empty());
        // The parallel precompute does the hashing, and the database pass reads it.
        let t = tx(outs.clone(), names(&[&mine, &theirs]));
        let pre = PreTx::compute(&t).expect("covenant outputs");
        assert_eq!(pre.genesis.as_ref().map(Vec::len), Some(genesis_leaders(&t.payload.0, 2).len()));
        assert_eq!(ex.extract_with(&t, 0, Some(&pre)).unwrap().expect("relevant").holds, want);
        let over = tx(vec![outs[0].clone()], names(&[&mine, &theirs]));
        assert_eq!(PreTx::compute(&over).and_then(|p| p.genesis).map(|g| g.len()), Some(0));
        // The same state under another pinned program is another script: it is proven under THAT
        // program only.
        let look_alike =
            crate::script::spk_bytes(&TokenState::Kcc20(mine.clone()).spk_with(token_template(TemplateId::Kcc20PublicMint)));
        let rec =
            ex.extract(&tx(vec![look_alike], names(&[&mine])), 0).unwrap().expect("a public-mint holder is a holding of that program");
        assert_eq!(rec.holds[0].program, TemplateId::Kcc20PublicMint);
        // No payload, a payload that is not the encoding, or one with trailing bytes: unrelated.
        assert!(ex.extract(&tx(outs.clone(), vec![]), 0).unwrap().is_none());
        let mut trailing = names(&[&mine, &theirs]);
        trailing.push(0);
        assert!(ex.extract(&tx(outs.clone(), trailing), 0).unwrap().is_none());
        assert!(ex.extract(&tx(outs, b"KOBG\x02garbage".to_vec()), 0).unwrap().is_none());
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
        // Not a KRON token input: a P2PK signature push.
        assert!(token_next_states(&crate::script::parse_pushes(&[0x01, 0x00]).map(|_| vec![0x01u8, 0x00]).unwrap()).is_none());
    }
}
