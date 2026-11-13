//! The order-book state machine of the indexer.
//!
//! It turns extracted transaction records into order, UTXO, token and event rows, for every
//! protocol v3 order kind in both token families. Quantities are in token base units, and prices
//! are per whole token.
//!
//! A placement under a template this build does not pin never becomes an order. Either its payload
//! does not decode, or its output is not the P2SH of a pinned template, and the placement is a
//! reject.
//!
//! Arms, trailing ratchets and triggered fills read their trigger evidence from a plain resting
//! `KobAsk` or `KobBid` filled in the same transaction. A pair conditional reads two such fills,
//! one of each token, or a resting `KobPair` of its pair. The event records which one it used in
//! `detail.evidence`.
//!
//! # Pair orders
//!
//! Pair orders are `KobPair`, `KobCondPair` and `KobIfdPair`. Base token A is in `token_cov_id`
//! and quote token B in `quote_cov_id`. There is one custody row per custody of the state, each in
//! its own token, and strays of either token are the order's own.
//!
//! A pair fill's event has no price, because prices come only from KAS-book fills. Its pair fields
//! are in `detail.pair`. Its volume is a `pair_fills` row with the counterparty (`route`,
//! `netting` or `inventory`, see [`pair_counterparty`]).
//!
//! # Hostile chain content
//!
//! These rules keep the processor safe against it:
//! - Malformed or lying transactions never abort a batch, because a stalled follower would be a
//!   DoS. They are recorded in `rejects` or ignored, and only database errors propagate.
//! - An order exists only if `kob_protocol::payload::recover_orders` accepts its placement record.
//!   The P2SH rebuilt from the pinned template and the state must equal the output. The output
//!   must be a fresh covenant whose id is the consensus genesis id of a group that is this output
//!   alone. On the ask side the custody output must be exactly `amountLeft` base units owned by
//!   that id. The payload is never trusted.
//! - Later spends are classified from the signature script of the input. Its revealed redeem
//!   script hashes to the spent output, which is checked at extraction, and it must match a pinned
//!   template. Otherwise the spend is recorded as `unknown` and the lineage is closed
//!   conservatively.
//! - Continuations are derived by splicing the mutable windows
//!   (`kob_protocol::state::mutable_windows`) and checking the P2SH against the output. A state is
//!   stored only when it is proven.
//! - Everything mutable is stamped with the chain block that produced it, see `schema.sql`, so a
//!   reorg is an exact revert.

use super::db::{DbError, DbResult};
use super::ids::{id_of, intern, order_of};
use super::packed::{sql_genesis, sql_order_ids, sql_order_ids_more, sql_ustate};
use super::record::{Extractor, Reveal, TxRecord};
use super::recordlog::LogOp;
use super::status::FillNotice;
use crate::hex::Hash32;
use crate::model::{self, Mutable, Nb};
use crate::rpc::types::{ChainBlockHeader, Tx};
use crate::script::{p2sh_spk, spk_bytes};
use crate::tokens::{ListingInput, ListingRules, TokenAllowlist};
use kob_protocol::artifacts::{template, TemplateId};
use kob_protocol::family::Family;
use kob_protocol::payload::{self, Record};
use kob_protocol::state::{rpt_until, AnyState, Booking, IfdAskState, IfdBidState, IfdPairState, PairEvidence};
use kob_protocol::tx::{CovenantJson, TxInputJson, TxJson, TxOutputJson, UtxoJson};
use rusqlite::{params, Connection, OptionalExtension, Row};
use serde_json::json;
use std::collections::{BTreeSet, HashMap, HashSet};
use std::sync::Arc;

/// What a batch changed, for the change feed and metrics.
#[derive(Debug, Default, Clone)]
pub struct Delta {
    /// Orders the batch changed.
    pub orders: BTreeSet<Hash32>,
    /// Tokens whose books or holdings the batch changed.
    pub tokens: BTreeSet<Hash32>,
    /// The fills of the batch.
    pub fills: Vec<FillNotice>,
    /// Transactions of the batch that touched KOB state.
    pub relevant_txs: u64,
    /// Orders created by the batch.
    pub new_orders: u64,
    /// Transactions recorded as rejects.
    pub rejects: u64,
    /// A placement announced a release newer than this build and the stored sightings changed.
    ///
    /// See `super::release`.
    pub release_noticed: bool,
}

/// The rules and token allowlist that decide what the indexer stores and lists.
pub struct Processor {
    /// The token allowlist.
    pub tokens: Arc<TokenAllowlist>,
    /// The listing rules.
    pub rules: ListingRules,
}

impl Processor {
    /// Whether the operator's own jobs, such as maintenance merges and sales, may touch `token`.
    ///
    /// A token qualifies when it is allowlisted. When the allowlist is not required, as in open
    /// listing, it also qualifies when an order trades it. Holdings are tracked for every token of
    /// a pinned program, so anyone can send the operator a token, but only these tokens are worth
    /// the operator's fees.
    pub fn operator_token(&self, conn: &Connection, token: &Hash32) -> DbResult<bool> {
        Ok(self.tokens.get(token).is_some()
            || ((!self.rules.require_allowlist || self.tokens.is_open()) && super::record::token_known(conn, token)?))
    }
}

/// The chain block being applied.
pub struct BlockCtx<'a> {
    /// The header of the chain block.
    pub header: &'a ChainBlockHeader,
    /// The sequence number of the block's row.
    pub seq: i64,
}

impl<'a> BlockCtx<'a> {
    /// Stamps a row for the block.
    ///
    /// Every applied block gets one. The writer prunes the rows older than the reorg window.
    pub fn new(conn: &Connection, header: &'a ChainBlockHeader) -> DbResult<Self> {
        conn.prepare_cached("INSERT INTO blocks (hash, daa) VALUES (?1, ?2)")?
            .execute(params![&header.hash.0[..], header.daa_score as i64])?;
        Ok(BlockCtx { header, seq: conn.last_insert_rowid() })
    }
}

fn hash_at(row: &Row<'_>, i: usize) -> rusqlite::Result<Hash32> {
    let b: Vec<u8> = row.get(i)?;
    Hash32::from_slice(&b).ok_or(rusqlite::Error::InvalidColumnType(i, "hash".into(), rusqlite::types::Type::Blob))
}

fn opt_hash_at(row: &Row<'_>, i: usize) -> rusqlite::Result<Option<Hash32>> {
    let b: Option<Vec<u8>> = row.get(i)?;
    Ok(b.and_then(|b| Hash32::from_slice(&b)))
}

/// Runs protocol arithmetic on chain-supplied values and returns `None` if it panics.
///
/// A hostile order can make the arithmetic overflow or divide by zero, which must never take the
/// indexer down.
pub fn guarded<T>(f: impl FnOnce() -> T) -> Option<T> {
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(f)).ok()
}

#[derive(Debug, Clone)]
struct OrderRow {
    template: TemplateId,
    side: u8,
    price: Option<i64>,
    token: Option<Hash32>,
    tif: Option<i64>,
    listed: bool,
    /// For pair orders, the quote token B. `token` is the base token A.
    quote: Option<Hash32>,
}

fn load_order(conn: &Connection, cov: &Hash32) -> DbResult<Option<OrderRow>> {
    let r = conn
        .prepare_cached(
            "SELECT o.contract, o.side, o.price, tk.h, o.tif, o.listed, qt.h FROM orders o LEFT JOIN ids tk ON tk.id = o.token_id \
             LEFT JOIN ids qt ON qt.id = o.quote_id WHERE o.covenant_id = ?1",
        )?
        .query_row([&cov.0[..]], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, i64>(1)?,
                r.get::<_, Option<i64>>(2)?,
                opt_hash_at(r, 3)?,
                r.get::<_, Option<i64>>(4)?,
                r.get::<_, i64>(5)?,
                opt_hash_at(r, 6)?,
            ))
        })
        .optional()?;
    Ok(r.and_then(|(name, side, price, token, tif, listed, quote)| {
        Some(OrderRow { template: TemplateId::from_name(&name)?, side: side as u8, price, token, tif, listed: listed != 0, quote })
    }))
}

#[derive(Debug, Clone)]
struct UtxoRow {
    covenant_id: Hash32,
    value: i64,
}

fn load_order_utxo(conn: &Connection, txid: &Hash32, idx: u32) -> DbResult<Option<UtxoRow>> {
    Ok(conn
        .prepare_cached("SELECT o.covenant_id, u.value FROM order_utxos u JOIN orders o ON o.id = u.order_id WHERE u.txid = ?1 AND u.idx = ?2 AND u.spent_block IS NULL")?
        .query_row(params![&txid.0[..], idx], |r| Ok(UtxoRow { covenant_id: hash_at(r, 0)?, value: r.get(1)? }))
        .optional()?)
}

/// Whether the order with covenant id `cov` is stored.
pub fn order_exists(conn: &Connection, cov: &Hash32) -> DbResult<bool> {
    super::record::order_known(conn, cov)
}

/// Whether the order already has a live custody row. A second one never becomes custody.
fn has_live_custody(conn: &Connection, cov: &Hash32) -> DbResult<bool> {
    Ok(conn
        .prepare_cached(concat!(
            "SELECT 1 FROM token_utxos WHERE order_id = ",
            order_of!("?1"),
            " AND role = 'custody' AND spent_block IS NULL LIMIT 1"
        ))?
        .exists([&cov.0[..]])?)
}

/// Whether the order already has a live custody row of `token`.
///
/// A pair order holds at most one custody per token.
fn has_live_custody_of(conn: &Connection, cov: &Hash32, token: &Hash32) -> DbResult<bool> {
    Ok(conn
        .prepare_cached(concat!(
            "SELECT 1 FROM token_utxos WHERE order_id = ",
            order_of!("?1"),
            " AND token_id = ",
            id_of!("?2"),
            " AND role = 'custody' AND spent_block IS NULL LIMIT 1"
        ))?
        .exists(params![&cov.0[..], &token.0[..]])?)
}

/// The extension commitments that a pair order's custody of `token` may carry.
///
/// They are:
/// - The ones its custodies of that token carried so far. A rest keeps the extension commitment
///   of the custody it continues.
/// - The one the state names for new outputs of that token, as `aExt` or `bExt` of `KobIfdPair`
///   for the new custody of a merge.
/// - The one the order row stores, which is the first custody part of the placement record, or
///   the entry token of an exit.
fn pair_custody_exts(conn: &Connection, cov: &Hash32, token: &Hash32, state: &AnyState) -> DbResult<Vec<[u8; 32]>> {
    let mut v = vec![];
    let mut st = conn.prepare_cached(
        concat!(
            "SELECT h.program, kob_hstate(h.state, h.family, w.h, h.owner_kind, h.amount) FROM token_holdings h JOIN ids w ON w.id = h.owner_id \
             WHERE h.owner_id = ",
            id_of!("?1"),
            " AND h.token_id = ",
            id_of!("?2"),
            " AND h.role = 'custody' ORDER BY h.created_block DESC, h.idx DESC LIMIT 4"
        ),
    )?;
    let rows = st.query_map(params![&cov.0[..], &token.0[..]], |r| Ok((r.get::<_, String>(0)?, r.get::<_, Vec<u8>>(1)?)))?;
    for r in rows {
        let (program, bytes) = r?;
        let Some(tpl) = TemplateId::from_name(&program).filter(|t| t.is_token()) else { continue };
        if let Ok(ts) = kob_protocol::state::TokenState::decode_with(kob_protocol::artifacts::token_template(tpl), &bytes) {
            v.push(ts.extension());
        }
    }
    if let AnyState::KobIfdPair(s) = state {
        if token.0 == s.a_cov_id {
            v.push(s.a_ext);
        }
        if token.0 == s.b_cov_id {
            v.push(s.b_ext);
        }
    }
    if let Some(e) = order_ext_commit(conn, cov)? {
        v.push(e);
    }
    Ok(v)
}

/// [`is_exact_custody`] of a pair order.
///
/// The token output must be exactly one of the custodies that the CURRENT state of the order
/// requires, with that token and the exact amount ([`AnyState::custodies`]). It must be plain,
/// covenant-owned by the order id, and of the family of that token. A KCC-20 output must also carry
/// an extension commitment that the order's custody of that token may carry
/// ([`pair_custody_exts`]).
fn is_exact_pair_custody(
    conn: &Connection,
    cov: &Hash32,
    token: &Hash32,
    amount: i64,
    held: Option<&kob_protocol::state::TokenState>,
    state: &AnyState,
) -> DbResult<bool> {
    let Some(tokens) = state.pair_tokens() else { return Ok(false) };
    if crate::sanity::check(state).is_err() {
        return Ok(false);
    }
    if !state.custodies().iter().any(|(t, a)| *t == token.0 && *a == amount && *a > 0) {
        return Ok(false);
    }
    let fam = if tokens.a.cov_id == token.0 { tokens.a.family_of() } else { tokens.b.family_of() };
    let Some(held) = held else { return Ok(false) };
    if !(held.is_covenant_owned() && held.is_plain() && held.owner() == cov.0 && Some(held.family()) == fam) {
        return Ok(false);
    }
    if held.family() == Family::Kcc20 && !pair_custody_exts(conn, cov, token, state)?.contains(&held.extension()) {
        return Ok(false);
    }
    Ok(true)
}

/// Whether a token output owned by order `cov` is exactly the custody the covenant requires.
///
/// The covenant rules apply to the CURRENT state of the order. The custody holds the order's own
/// token, exactly `amountLeft` base units, and is plain: borrowing is disabled and it is
/// covenant-owned by the order id. It carries the extension commitment that the order's custody
/// carries. Orders that hold no tokens never have a custody. They are bids, exits of a sell-first
/// entry and an empty repeating entry.
fn is_exact_custody(
    conn: &Connection,
    cov: &Hash32,
    token: &Hash32,
    amount: i64,
    held: Option<&kob_protocol::state::TokenState>,
) -> DbResult<bool> {
    let row = conn
        .prepare_cached(concat!(
            "SELECT o.contract, o_tk.h, o_ex.h, ",
            sql_ustate!("u", "o"),
            " FROM orders o JOIN order_utxos u ON u.order_id = o.id AND u.spent_block IS NULL",
            sql_order_ids!("o"),
            sql_order_ids_more!("o"),
            " WHERE o.covenant_id = ?1 ORDER BY u.created_block DESC, u.idx DESC LIMIT 1"
        ))?
        .query_row([&cov.0[..]], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, Option<Vec<u8>>>(1)?,
                r.get::<_, Option<Vec<u8>>>(2)?,
                r.get::<_, Option<Vec<u8>>>(3)?,
            ))
        })
        .optional()?;
    let Some((contract, order_token, ext, state)) = row else { return Ok(false) };
    if TemplateId::from_name(&contract).is_some_and(|t| t.is_pair()) {
        let Some(state) = super::reads::tip_state(&contract, state) else { return Ok(false) };
        return is_exact_pair_custody(conn, cov, token, amount, held, &state);
    }
    if order_token.as_deref() != Some(&token.0[..]) {
        return Ok(false);
    }
    let Some(state) = super::reads::tip_state(&contract, state) else { return Ok(false) };
    // A state that fails the numeric gate has no custody amount at all.
    let Some(expect) = crate::sanity::custody_amount(&state).filter(|a| *a > 0) else { return Ok(false) };
    if amount != expect {
        return Ok(false);
    }
    let Some(held) = held else { return Ok(false) };
    if !(held.is_covenant_owned() && held.is_plain() && held.owner() == cov.0 && held.family() == state.family()) {
        return Ok(false);
    }
    // The commitment that the covenant of the order requires of its custody. The state names it,
    // see `AnyState::custody_ext`.
    if state.family() == Family::Kcc20 && state.custody_ext(token.0).is_some_and(|e| e != held.extension()) {
        return Ok(false);
    }
    // The extension commitment the order stores. The one of an exit is its entry's, see step 3 of
    // `apply_tx`.
    if let Some(w) = ext {
        if state.family() == Family::Kcc20 && held.extension()[..] != w[..] {
            return Ok(false);
        }
    }
    Ok(true)
}

/// The extension commitment an order stores, or `None` for an unknown order or none stored.
fn order_ext_commit(conn: &Connection, cov: &Hash32) -> DbResult<Option<[u8; 32]>> {
    let e: Option<Vec<u8>> = conn
        .prepare_cached("SELECT e.h FROM orders o JOIN ids e ON e.id = o.ext_id WHERE o.covenant_id = ?1")?
        .query_row([&cov.0[..]], |r| r.get(0))
        .optional()?;
    Ok(e.and_then(|e| <[u8; 32]>::try_from(e.as_slice()).ok()))
}

/// Everything needed to insert a new order lineage.
pub struct NewOrder {
    /// The state of the order at its genesis output.
    pub state: AnyState,
    /// The covenant id of the order.
    pub covenant_id: Hash32,
    /// Id of the transaction that created the output.
    pub txid: Hash32,
    /// Index of the output in that transaction.
    pub out_index: u32,
    /// KAS value of the output.
    pub out_value: u64,
    /// Script public key of the output.
    pub out_spk: Vec<u8>,
    /// Sequence number of the chain block that holds the output.
    pub block_seq: i64,
    /// DAA score of that chain block.
    pub daa: u64,
    /// Timestamp of that chain block.
    pub ts: u64,
    /// Position of the transaction in the chain block.
    pub tx_pos: i64,
    /// The order this one continues, for an if-done exit.
    pub parent: Option<Hash32>,
    /// For day orders, the deadline in UTC unix seconds from the placement record.
    pub deadline: Option<u64>,
    /// The token custody UTXO of an ask-side order, from the placement record or a verified import.
    ///
    /// For a pair order it is the first custody, see [`AnyState::custodies`].
    pub custody: Option<NewCustody>,
    /// The second custody of a sell-first `KobIfdPair`, its B prefund.
    pub prefund: Option<NewCustody>,
    /// The extension commitment of the custody.
    ///
    /// For ask-side kinds it is the custody part of the placement record, or the entry of an
    /// if-done exit.
    pub ext_commit: Option<[u8; 32]>,
    /// `chain` or `import`.
    pub origin: &'static str,
    /// The listing decision of the parent for an if-done exit. Otherwise the rules are evaluated.
    pub inherit_listed: Option<(bool, Option<String>)>,
}

/// A token custody UTXO of a new order.
pub struct NewCustody {
    /// Id of the transaction that created the output.
    pub txid: Hash32,
    /// Index of the output in that transaction.
    pub index: u32,
    /// KAS value of the output.
    pub value: u64,
    /// Token amount in base units.
    pub amount: i64,
    /// The token of the custody, or `None` for the token of the order.
    ///
    /// A pair order names the token of each custody, see [`custody_part_tokens`].
    pub token: Option<Hash32>,
}

/// The tokens of an order's custody parts in record order, with their program template hashes.
///
/// A KAS kind has its one token. A pair order has each custody of [`AnyState::custodies`]:
/// - The S of a `KobPair` or `KobCondPair`.
/// - The B escrow of a buy-first entry.
/// - The A custody and then the B prefund of a sell-first entry.
pub fn custody_part_tokens(s: &AnyState) -> Vec<(Hash32, Option<[u8; 32]>)> {
    match s.pair_tokens() {
        Some(t) => {
            s.custodies().iter().map(|(c, _)| (Hash32(*c), Some(if *c == t.a.cov_id { t.a.tpl_hash } else { t.b.tpl_hash }))).collect()
        }
        None => vec![(Hash32(s.token_cov_id()), s.token_tpl_hash())],
    }
}

/// Whether a placement became an order.
#[derive(Debug, PartialEq, Eq)]
pub enum CreateOutcome {
    /// The order was created.
    Created,
    /// The placement was refused, with the reason.
    Rejected(String),
}

/// What an entry call did to the order, decoded from the revealed arguments.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Act {
    Fill(i64),
    Merge { k: usize, m: i64 },
    Refund,
    Cancel,
    Update,
    Unknown,
}

fn classify(entry: &str, nb: Option<Nb>) -> Act {
    match entry {
        "cancel" => Act::Cancel,
        "update" => Act::Update,
        "refund" | "close" => Act::Refund,
        "settle" | "fill" => match nb {
            Some(Nb::Zero) => Act::Refund,
            Some(Nb::Fill(n)) => Act::Fill(n),
            Some(Nb::Merge { k, m }) => Act::Merge { k, m },
            None => Act::Unknown,
        },
        _ => Act::Unknown,
    }
}

/// The `upd` flag of a pair conditional's `settle` or a pair entry's `fill`.
///
/// It is argument 11 of `settle` and argument 17 of `fill`.
///
/// 1 is an update, an arm or a trail, with nb = 0. 0 is a fill, a merge or a refund.
fn pair_upd(r: &Reveal) -> bool {
    match r.template {
        TemplateId::KobCondPair => r.int(11) == Some(1),
        TemplateId::KobIfdPair => r.int(17) == Some(1),
        _ => false,
    }
}

struct Spent {
    cov: Hash32,
    order: OrderRow,
    input_idx: usize,
    entry: Option<&'static str>,
    act: Act,
    reveal: Option<Reveal>,
    before: Option<AnyState>,
    utxo_daa: i64,
    /// The KAS of the spent order UTXO.
    value: i64,
    /// Filled by the continuation step.
    after: Option<AnyState>,
    /// A verified in-place amend continued the order (the record's day-order deadline).
    amend: Option<Option<u64>>,
    /// A verified sweep in place continued the order unchanged (SWEEP record).
    sweep: bool,
}

impl Processor {
    /// Live path: extracts the relevant transactions of one chain block and applies them.
    ///
    /// It returns the records to append to the log.
    pub fn extract_and_apply_block(
        &self,
        conn: &Connection,
        header: &ChainBlockHeader,
        txs: &[Tx],
        delta: &mut Delta,
    ) -> DbResult<Vec<TxRecord>> {
        self.extract_and_apply_block_pre(conn, header, txs, None, delta)
    }

    /// [`Processor::extract_and_apply_block`] with the precomputed pure work of the batch.
    ///
    /// See [`super::record::precompute`].
    pub fn extract_and_apply_block_pre(
        &self,
        conn: &Connection,
        header: &ChainBlockHeader,
        txs: &[Tx],
        pre: Option<&super::record::PreBatch>,
        delta: &mut Delta,
    ) -> DbResult<Vec<TxRecord>> {
        let ctx = BlockCtx::new(conn, header)?;
        let ex = Extractor { conn };
        let mut out = Vec::new();
        for (pos, tx) in txs.iter().enumerate() {
            let p = pre.and_then(|m| m.get(&tx.verbose_data.transaction_id));
            if let Some(mut rec) = ex.extract_with(tx, pos as u32, p)? {
                self.apply_tx(conn, &ctx, &rec, p, delta)?;
                // A candidate KCC-23 preparation that no genesis has bound is kept in the database
                // only. The record log is permanent, and any transaction can carry a 16 KiB
                // `0x00{..}` payload. A transaction with nothing of KOB in it is not logged at all.
                // One that is relevant to KOB for another reason, such as a token transfer or a
                // stray send, is logged without the section.
                if rec.kcc23.as_ref().is_some_and(|d| !d.is_logged()) {
                    if rec.is_declaration_only() {
                        continue;
                    }
                    rec.kcc23 = None;
                }
                delta.relevant_txs += 1;
                out.push(rec);
            }
        }
        Ok(out)
    }

    /// Replay path: applies the already extracted records of one logged chain block.
    pub fn apply_records(&self, conn: &Connection, header: &ChainBlockHeader, recs: &[TxRecord], delta: &mut Delta) -> DbResult<()> {
        let ctx = BlockCtx::new(conn, header)?;
        for rec in recs {
            self.apply_tx(conn, &ctx, rec, None, delta)?;
            delta.relevant_txs += 1;
        }
        Ok(())
    }

    fn apply_tx(
        &self,
        conn: &Connection,
        ctx: &BlockCtx<'_>,
        rec: &TxRecord,
        pre: Option<&super::record::PreTx>,
        delta: &mut Delta,
    ) -> DbResult<()> {
        let txid = rec.txid;
        let seq = ctx.seq;
        let daa = ctx.header.daa_score;

        // Trigger evidence the transaction carries: the quotes of the plain resting `KobAsk` and
        // `KobBid` orders filled in it. A trailing `update` ratchets from one of them, see
        // `docs/spec/matcher.md` §4.
        let evidence_prices: Vec<i64> = rec.inputs.iter().filter_map(|i| evidence_touch(i.reveal.as_ref()?).map(|(_, p)| p)).collect();

        // 1. Spends of tracked outputs, and what the KOB inputs held, for the income of an executor
        // in step 8.
        let mut spent: Vec<Spent> = Vec::new();
        let mut kob_in: i128 = 0;
        let mut orphan = false;
        // A covenant input that is neither an order nor the custody or stray of an order. These are
        // the sender's own tokens, see step 8.
        let mut foreign_cov_in = false;
        // What every input held, from the outputs this store knows. It gives the network fee of a
        // record that lacks the input values of the node.
        let mut resolved_in: Option<i128> = Some(0);
        for (i, inp) in rec.inputs.iter().enumerate() {
            let known = match load_order_utxo(conn, &inp.txid, inp.index)? {
                Some(u) => Some(u.value),
                None => super::outputs::input_value(conn, &inp.txid, inp.index, daa)?,
            };
            resolved_in = resolved_in.zip(known).map(|(s, v)| s + i128::from(v));
            if let Some(u) = load_order_utxo(conn, &inp.txid, inp.index)? {
                kob_in += i128::from(u.value);
                match self.spend_order(conn, seq, rec, i, u)? {
                    Some(s) => spent.push(s),
                    None => orphan = true,
                }
            } else {
                // A custody or a stray of an order, and its carrier.
                let carrier: Option<i64> = conn
                    .prepare_cached("SELECT value FROM token_utxos WHERE txid = ?1 AND idx = ?2 AND spent_block IS NULL")?
                    .query_row(params![&inp.txid.0[..], inp.index], |r| r.get(0))
                    .optional()?;
                kob_in += i128::from(carrier.unwrap_or(0));
                foreign_cov_in |= inp.cov.is_some() && carrier.is_none();
                conn.prepare_cached(
                    "UPDATE token_utxos SET spent_block = ?1, spent_txid = ?2 WHERE txid = ?3 AND idx = ?4 AND spent_block IS NULL",
                )?
                .execute(params![seq, &txid.0[..], &inp.txid.0[..], inp.index])?;
                conn.prepare_cached(
                    "UPDATE token_holdings SET spent_block = ?1, spent_txid = ?2 WHERE txid = ?3 AND idx = ?4 AND spent_block IS NULL",
                )?
                .execute(params![seq, &txid.0[..], &inp.txid.0[..], inp.index])?;
            }
        }
        super::outputs::record(conn, rec, seq, daa)?;
        let spent_covs: HashSet<Hash32> = spent.iter().map(|s| s.cov).collect();
        // The in-place amends and sweeps the payload announces, as AMEND and SWEEP records. Each is
        // verified against the continuation it names.
        let amends: Vec<Record> = match payload::decode(&rec.payload) {
            Ok(Some(p)) => p.records.into_iter().filter(|r| matches!(r, Record::Amend { .. } | Record::Sweep { .. })).collect(),
            _ => vec![],
        };

        // 2. Outputs: continuations. A maker's cancel continues the id only as a verified in-place
        // amend or sweep. Any other continuation of a cancel stays unproven: its state is unknown
        // and it is listed nowhere.
        let mut continued: HashSet<Hash32> = HashSet::new();
        for (j, out) in rec.outputs.iter().enumerate() {
            let Some((_, cov)) = out.cov else { continue };
            if let Some(si) = spent.iter().position(|s| s.cov == cov) {
                let after = match spent[si].act {
                    Act::Cancel => match self.amend_of(conn, rec, &spent[si], j, &amends, seq, delta)? {
                        Some((st, deadline)) => {
                            spent[si].amend = Some(deadline);
                            Some(st)
                        }
                        None => match self.sweep_of(conn, rec, &spent[si], j, &amends, seq, delta)? {
                            Some(st) => {
                                spent[si].sweep = true;
                                Some(st)
                            }
                            None => None,
                        },
                    },
                    _ => self.derive_after(&spent[si], out.spk.as_slice(), &evidence_prices, rec),
                };
                let state = after.as_ref().map(|a| a.encode());
                self.insert_utxo(conn, &cov, &txid, j as u32, out.value, &out.spk, state.as_deref(), seq, daa)?;
                spent[si].after = after;
                continued.insert(cov);
            }
        }

        // 3. If-done exits: a fresh covenant output that matches the exit the entry committed to.
        let mut exits: HashMap<Hash32, Hash32> = HashMap::new();
        let mut created: HashSet<Hash32> = HashSet::new();
        for s in spent.iter().filter(|s| matches!(s.act, Act::Fill(_)) && model::exit_template(s.order.template).is_some()) {
            let Some((exit_state, cov, j)) = self.derive_exit(s, rec, &spent_covs, conn)? else {
                self.record_reject(conn, seq, &txid, "ifd_exit:not_derivable", delta)?;
                continue;
            };
            let out = &rec.outputs[j];
            // The token identity of the exit is the one of its entry. A buy-first entry pays the
            // bought tokens to the custody of the exit with its own `extensionCommitment`, which
            // the covenant writes. The `KobCondBid` exit of a sell-first entry carries it in its
            // state. A pair entry's exit holds n of A when buy-first, or n of B when sell-first,
            // written with the `aExt` or `bExt` of the entry.
            //
            // It is stored like the one of a placed order, so every view of the exit has it,
            // including the custody the matcher rebuilds.
            let ext = match s.before.as_ref() {
                Some(AnyState::KobIfdPair(e)) => Some(if e.is_buy_first() { e.a_ext } else { e.b_ext }),
                b => match b.and_then(model::extension_of) {
                    Some(e) => Some(e),
                    None => order_ext_commit(conn, &s.cov)?,
                },
            };
            let outcome = self.create_order(
                conn,
                delta,
                NewOrder {
                    state: exit_state,
                    covenant_id: cov,
                    txid,
                    out_index: j as u32,
                    out_value: out.value,
                    out_spk: out.spk.clone(),
                    block_seq: seq,
                    daa,
                    ts: ctx.header.timestamp,
                    tx_pos: rec.pos as i64,
                    parent: Some(s.cov),
                    deadline: None,
                    custody: None,
                    prefund: None,
                    ext_commit: ext,
                    origin: "chain",
                    inherit_listed: Some((s.order.listed, if s.order.listed { None } else { Some("parent_unlisted".to_string()) })),
                },
            )?;
            match outcome {
                CreateOutcome::Created => {
                    exits.insert(s.cov, cov);
                    created.insert(cov);
                }
                CreateOutcome::Rejected(reason) => self.record_reject(conn, seq, &txid, &format!("ifd_exit:{reason}"), delta)?,
            }
        }

        // 4. placement records
        if !rec.payload.is_empty() {
            self.genesis_orders(conn, ctx, rec, pre, delta, &mut created)?;
        }

        // 5. Token outputs owned by orders: the exact custody of the order, or a stray.
        //
        // Nothing on chain says which output of a transaction is the continuation custody. The ask
        // covenants pin ONE token output (`tokOut`) and only guard token INPUTS, so a taker can add
        // further outputs owned by the order id.
        //
        // An output is therefore the custody only if it is exactly what the covenant rules require
        // of the state of the order after this transaction. That is `amountLeft` base units of the
        // token of the order, plain, with the extension commitment of the order. The first such
        // output is the custody, and every other output with the same owner is a stray.
        //
        // A pair order holds one custody per token, see `AnyState::custodies`. A sell-first
        // `KobIfdPair` holds its A and its B prefund. Any other output of either pair token that it
        // owns is its own stray, not a foreign one.
        //
        // Kinds that hold KAS have no custody, so every token they own is a stray. They are bids,
        // exits of a sell-first entry and an empty repeating entry.
        let mut roles: HashMap<u32, String> = HashMap::new();
        for t in &rec.tok_outs {
            let Some(out) = rec.outputs.get(t.out as usize) else { continue };
            let Some((_, token)) = out.cov else { continue };
            if !order_exists(conn, &t.owner)? {
                continue;
            }
            let mut role = "stray";
            if created.contains(&t.owner) || spent_covs.contains(&t.owner) {
                let held = rec.holds.iter().find(|h| h.out == t.out).map(|h| &h.state);
                if is_exact_custody(conn, &t.owner, &token, t.amount, held)? && !has_live_custody_of(conn, &t.owner, &token)? {
                    role = "custody";
                }
            }
            conn.prepare_cached(concat!(
                "INSERT OR IGNORE INTO token_utxos (txid, idx, token_id, order_id, amount, value, role, created_block, created_daa) VALUES (?1,?2,?3,",
                order_of!("?4"),
                ",?5,?6,?7,?8,?9)"
            ))?
            .execute(params![&txid.0[..], t.out, intern(conn, &token.0)?, &t.owner.0[..], t.amount, out.value as i64, role, seq, daa as i64])?;
            // A placement record may already have stored this very output as the custody. Keep the
            // stored role.
            let stored: String = conn
                .prepare_cached("SELECT role FROM token_utxos WHERE txid = ?1 AND idx = ?2")?
                .query_row(params![&txid.0[..], t.out], |r| r.get(0))?;
            roles.insert(t.out, stored);
            delta.tokens.insert(token);
            delta.orders.insert(t.owner);
        }
        // 5b. Holdings: every proven token output of a pinned program, whoever owns it.
        for h in &rec.holds {
            let Some(out) = rec.outputs.get(h.out as usize) else { continue };
            let Some((_, token)) = out.cov else { continue };
            let role = roles.get(&h.out).map(|s| s.as_str()).unwrap_or("owned");
            self.insert_holding(conn, &txid, h.out, &token, h.program, &h.state, out.value, role, seq, daa)?;
            delta.tokens.insert(token);
        }

        // 6. Events. An in-place amend first takes the new terms of the order.
        for s in &spent {
            if let (Some(deadline), Some(after)) = (s.amend, &s.after) {
                let carrier = rec.outputs.iter().find(|o| o.cov.is_some_and(|(_, c)| c == s.cov)).map(|o| o.value).unwrap_or(0);
                self.apply_amend(conn, ctx, &s.cov, after, deadline, carrier)?;
            }
            let closes = !continued.contains(&s.cov);
            self.record_spend_events(conn, ctx, rec, s, closes, &exits, &spent, delta)?;
        }

        // 8. An executor's transaction: what it left to its sender and what it paid the miners.
        // See `executor_txs` and `super::income`.
        if !spent.is_empty() {
            self.executor_tx(conn, ctx, rec, &spent, kob_in, (orphan, foreign_cov_in), &continued, &created, resolved_in)?;
        }

        // 7. KCC-23: a candidate preparation, or the verified launch proofs of a genesis. See
        // `super::kcc23`.
        if let Some(d) = &rec.kcc23 {
            super::kcc23::apply(conn, seq, daa, &txid, d)?;
        }
        Ok(())
    }

    /// Stores the proven holding row of a custody that the node verified.
    ///
    /// Imports and gap adoptions use it.
    ///
    /// The node verified the custody at the script that
    /// `TokenState::custody(amount, order id, ext)` produces. The order views then carry the token
    /// state of the custody, as for a custody seen on chain. Nothing is stored without the
    /// extension commitment, or for a token program this build does not pin.
    #[allow(clippy::too_many_arguments)]
    fn custody_holding(
        &self,
        conn: &Connection,
        cov: &Hash32,
        st: &AnyState,
        c: &NewCustody,
        ext: Option<[u8; 32]>,
        seq: i64,
        daa: u64,
    ) -> DbResult<()> {
        // The token and program of the custody. A pair order names the token of each custody, and
        // its family is the family of that token.
        let (token, tpl_hash) = match c.token {
            Some(t) => (t, custody_part_tokens(st).into_iter().find(|x| x.0 == t).and_then(|x| x.1)),
            None => (Hash32(st.token_cov_id()), st.token_tpl_hash()),
        };
        let Some(tt) = tpl_hash
            .and_then(|h| kob_protocol::artifacts::token_template_by_hash(&h))
            .filter(|t| st.is_pair() || t.family == st.family())
        else {
            return Ok(());
        };
        let ext = match (tt.family, ext) {
            // A KRON token has no extension commitment.
            (Family::Kron, _) => [0; 32],
            (_, Some(e)) => e,
            _ => return Ok(()),
        };
        let ts = kob_protocol::state::TokenState::custody(tt.family, c.amount, cov.0, ext);
        conn.prepare_cached("DELETE FROM token_holdings WHERE txid = ?1 AND idx = ?2")?.execute(params![&c.txid.0[..], c.index])?;
        self.insert_holding(conn, &c.txid, c.index, &token, tt.id, &ts, c.value, "custody", seq, daa)
    }

    /// Stores a token output whose state was proven against its script public key.
    #[allow(clippy::too_many_arguments)]
    fn insert_holding(
        &self,
        conn: &Connection,
        txid: &Hash32,
        idx: u32,
        token: &Hash32,
        program: TemplateId,
        state: &kob_protocol::state::TokenState,
        value: u64,
        role: &str,
        seq: i64,
        daa: u64,
    ) -> DbResult<()> {
        let kind = match state {
            kob_protocol::state::TokenState::Kcc20(k) => k.owner_scheme,
            kob_protocol::state::TokenState::Kron(k) => k.id_type,
        };
        // The first holding under a program proves it. The orders already listed under another
        // program for the covenant id were placed before the proof existed.
        let token_id = intern(conn, &token.0)?;
        let proven_before = conn
            .prepare_cached("SELECT 1 FROM holding_programs WHERE token_id = ?1 AND program = ?2")?
            .exists(params![token_id, program.name()])?;
        conn.prepare_cached(
            "INSERT OR IGNORE INTO token_holdings (txid, idx, token_id, program, family, owner_id, owner_kind, amount, value, state, role, created_block, created_daa) \
             VALUES (?1,?2,?3,?4,?5,?14,?7,?8,?9,kob_hpack(?10,?5,?6,?7,?8),?11,?12,?13)",
        )?
        .execute(params![
            &txid.0[..],
            idx,
            token_id,
            program.name(),
            state.family().code(),
            &state.owner()[..],
            kind,
            state.amount(),
            value as i64,
            state.encode(),
            role,
            seq,
            daa as i64,
            intern(conn, &state.owner()[..])?
        ])?;
        if !proven_before {
            self.unlist_conflicting(conn, token, seq)?;
            self.relist_proven(conn, token, program, seq)?;
        }
        Ok(())
    }

    /// Most orders one proof re-lists, oldest first. The rest stay out of the books until they are
    /// amended.
    const RELIST_MAX: i64 = 2_000;

    /// Relists the orders refused for a program that the chain newly proves for the token.
    ///
    /// These orders pin the newly proven program. They were refused as `token_program_conflict`
    /// while another program was the only one proved. That happens when a token has two programs,
    /// for example a KRON token upgraded from one to the other. They get the listing rules again,
    /// as of their placement. Without this, an honest order of the second program would stay out of
    /// the books for good, even though its own custody is the proof.
    ///
    /// The rows they had are kept in `order_amends`, stamped with the block, so a reorg gives them
    /// back exactly ([`revert_block`]).
    ///
    /// The carrier the rules measure is the KAS of the genesis output, `orders.genesis_value`. The
    /// archive pruning deletes the spent UTXO row, so the value of that row is used only where an
    /// order has no `genesis_value`.
    fn relist_proven(&self, conn: &Connection, token: &Hash32, program: TemplateId, seq: i64) -> DbResult<()> {
        let Some(tpl) = kob_protocol::artifacts::try_token_template(program).map(|t| t.hash.to_vec()) else { return Ok(()) };
        type Row = (Vec<u8>, String, Vec<u8>, i64, i64);
        let rows: Vec<Row> = conn
            .prepare_cached(
                concat!("SELECT o.covenant_id, o.contract, ", sql_genesis!("o"), ", o.genesis_daa, COALESCE(o.genesis_value, u.value) FROM orders o INDEXED BY orders_token_tpl                  LEFT JOIN order_utxos u ON u.txid = o.genesis_txid AND u.idx = o.genesis_out", sql_order_ids!("o"), " WHERE o.token_id = (SELECT id FROM ids WHERE h = ?1) AND o.token_tpl_id = (SELECT id FROM ids WHERE h = ?2) AND o.listed = 0 AND o.unlisted_reason = 'token_program_conflict'                  AND o.quote_id IS NULL AND COALESCE(o.genesis_value, u.value) IS NOT NULL ORDER BY o.genesis_block, o.covenant_id LIMIT ?3"),
            )?
            .query_map(params![&token.0[..], &tpl, Self::RELIST_MAX], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)))?
            .collect::<rusqlite::Result<_>>()?;
        for (cov, contract, span, daa, value) in rows {
            let Some(id) = TemplateId::from_name(&contract) else { continue };
            let Ok(state) = AnyState::decode(id, &span) else { continue };
            let Ok(cov) = Hash32::from_slice(&cov).ok_or(()) else { continue };
            let (listed, reason) = match crate::sanity::check(&state) {
                Err(why) => (false, Some(why)),
                Ok(()) => self.listing(conn, &state, order_ext_commit(conn, &cov)?, daa.max(0) as u64, value.max(0) as u64)?,
            };
            if !listed && reason.as_deref() == Some("token_program_conflict") {
                continue;
            }
            conn.prepare_cached(
                "INSERT INTO order_amends (order_id, block_seq, price, tip, tif, expiry_daa, active_from, deadline, listed, unlisted_reason, budget_rate, reserve, min_fill)                  SELECT id, ?2, price, tip, tif, expiry_daa, active_from, deadline, listed, unlisted_reason, budget_rate, reserve, min_fill FROM orders WHERE covenant_id = ?1",
            )?
            .execute(params![&cov.0[..], seq])?;
            conn.prepare_cached("UPDATE orders SET listed = ?2, unlisted_reason = ?3 WHERE covenant_id = ?1")?.execute(params![
                &cov.0[..],
                listed as i64,
                reason
            ])?;
        }
        Ok(())
    }

    /// Unlists the orders of `token` that pin a program its holdings do not prove.
    ///
    /// These orders were listed while nothing proved a program. A new order is refused as
    /// `token_program_conflict`, see [`Processor::listing`]. The rows they had are kept in
    /// `order_amends`, stamped with the block, so a reorg gives them back exactly
    /// ([`revert_block`]).
    fn unlist_conflicting(&self, conn: &Connection, token: &Hash32, seq: i64) -> DbResult<()> {
        let proven = proven_hashes(conn, Some(*token))?;
        if proven.is_empty() {
            return Ok(());
        }
        // The distinct programs that the listed orders of the token pin, with one index seek each.
        // See `orders_listed_tokens`.
        let mut next = conn.prepare_cached(
            "SELECT MIN(token_tpl_id) FROM orders INDEXED BY orders_listed_tokens              WHERE listed = 1 AND token_id = (SELECT id FROM ids WHERE h = ?1) AND token_tpl_id IS NOT NULL AND token_tpl_id > ?2",
        )?;
        // The programs are rows of `ids` in the order they were first seen. They are handled in the
        // order of their hashes, so the amend rows of one block get their ids in that order.
        let mut tpls: Vec<(Vec<u8>, i64)> = Vec::new();
        let mut after = 0i64;
        while let Some(tpl_id) = next.query_row(params![&token.0[..], after], |r| r.get::<_, Option<i64>>(0))? {
            after = tpl_id;
            tpls.push((super::ids::hash_of(conn, tpl_id)?, tpl_id));
        }
        tpls.sort();
        for (tpl, tpl_id) in tpls {
            if proven.iter().any(|p| p.0[..] == tpl[..]) {
                continue;
            }
            conn.prepare_cached(
                "INSERT INTO order_amends (order_id, block_seq, price, tip, tif, expiry_daa, active_from, deadline, listed, unlisted_reason, budget_rate, reserve, min_fill)                  SELECT id, ?3, price, tip, tif, expiry_daa, active_from, deadline, listed, unlisted_reason, budget_rate, reserve, min_fill                  FROM orders WHERE token_id = (SELECT id FROM ids WHERE h = ?1) AND token_tpl_id = ?2 AND listed = 1",
            )?
            .execute(params![&token.0[..], tpl_id, seq])?;
            conn.prepare_cached(
                "UPDATE orders SET listed = 0, unlisted_reason = 'token_program_conflict'                  WHERE token_id = (SELECT id FROM ids WHERE h = ?1) AND token_tpl_id = ?2 AND listed = 1",
            )?
            .execute(params![&token.0[..], tpl_id])?;
        }
        Ok(())
    }

    fn record_reject(&self, conn: &Connection, seq: i64, txid: &Hash32, reason: &str, delta: &mut Delta) -> DbResult<()> {
        conn.prepare_cached("INSERT INTO rejects (block_seq, txid, reason) VALUES (?1, ?2, ?3)")?.execute(params![
            seq,
            &txid.0[..],
            reason
        ])?;
        delta.rejects += 1;
        Ok(())
    }

    // -----------------------------------------------------------------------------------------
    // Placement records.

    fn genesis_orders(
        &self,
        conn: &Connection,
        ctx: &BlockCtx<'_>,
        rec: &TxRecord,
        pre: Option<&super::record::PreTx>,
        delta: &mut Delta,
        created: &mut HashSet<Hash32>,
    ) -> DbResult<()> {
        let seq = ctx.seq;
        let p = match payload::decode(&rec.payload) {
            Ok(Some(p)) => p,
            Ok(None) => return Ok(()),
            Err(_) if rec.payload == super::record::UNDECODABLE_PAYLOAD => {
                return self.record_reject(conn, seq, &rec.txid, "payload:undecodable", delta);
            }
            Err(e) => return self.record_reject(conn, seq, &rec.txid, &format!("payload:{e}"), delta),
        };
        // The release that the placement announces, when it is newer than this build. It drives the
        // update notice, see `super::release`.
        let newer_release = payload::release_of(&p).filter(|r| r.is_newer_than_build());
        for r in &p.records {
            match r {
                Record::Order { .. } => {}
                _ => continue,
            }
            // Each record is validated on its own (`recover_orders` is all-or-nothing per payload).
            let single = match payload::encode(std::slice::from_ref(r)) {
                Ok(b) => b,
                Err(e) => {
                    self.record_reject(conn, seq, &rec.txid, &format!("payload:{e}"), delta)?;
                    continue;
                }
            };
            let checked = match pre.and_then(|p| p.placements.get(&single)) {
                Some(r) => r.clone(),
                None => payload::recover_orders(&tx_json(rec, single)).map_err(|e| e.to_string()),
            };
            let recovered = match checked {
                Ok(v) => v,
                Err(e) => {
                    self.record_reject(conn, seq, &rec.txid, &format!("placement:{e}"), delta)?;
                    continue;
                }
            };
            for ro in recovered {
                if !model::is_order(ro.order.template_id()) {
                    self.record_reject(conn, seq, &rec.txid, "not_an_order_template", delta)?;
                    continue;
                }
                let Some(out) = rec.outputs.get(ro.output as usize) else { continue };
                let cov = Hash32(ro.covenant_id);
                // The custody parts and their tokens. A pair order has them in the order of
                // `custodies()`, each in its own token.
                let part_tokens = custody_part_tokens(&ro.order);
                let parts: Vec<(&payload::RecoveredCustody, Hash32, Option<[u8; 32]>)> =
                    ro.custody.iter().chain(ro.prefund.iter()).zip(part_tokens.iter()).map(|(c, (t, h))| (c, *t, *h)).collect();
                let new_custody = |c: &payload::RecoveredCustody, t: Hash32| NewCustody {
                    txid: rec.txid,
                    index: c.output,
                    value: c.value,
                    amount: c.state.amount(),
                    token: Some(t),
                };
                let custody = parts.first().map(|(c, t, _)| new_custody(c, *t));
                let prefund = parts.get(1).map(|(c, t, _)| new_custody(c, *t));
                let ext = ro.custody.as_ref().map(|c| c.state.extension());
                let maker = Hash32(ro.order.maker());
                let outcome = self.create_order(
                    conn,
                    delta,
                    NewOrder {
                        state: ro.order,
                        covenant_id: cov,
                        txid: rec.txid,
                        out_index: ro.output,
                        out_value: out.value,
                        out_spk: out.spk.clone(),
                        block_seq: seq,
                        daa: ctx.header.daa_score,
                        ts: ctx.header.timestamp,
                        tx_pos: rec.pos as i64,
                        parent: None,
                        deadline: ro.deadline,
                        custody,
                        prefund,
                        ext_commit: ext,
                        origin: "chain",
                        inherit_listed: None,
                    },
                )?;
                match outcome {
                    CreateOutcome::Created => {
                        created.insert(cov);
                        if let Some(v) = newer_release {
                            delta.release_noticed |= super::release::note(conn, v, maker, rec.txid, ctx.header.daa_score)?;
                        }
                        // The placement record reveals the custody states. Prove each against its
                        // output before storing it.
                        for (c, token, tpl_hash) in &parts {
                            let token = *token;
                            let tpl = tpl_hash.and_then(|h| kob_protocol::artifacts::token_template_by_hash(&h));
                            let out = rec.outputs.get(c.output as usize);
                            if let (Some(tpl), Some(out)) = (tpl, out) {
                                if crate::script::parse_spk(&out.spk).as_ref() == Some(&c.state.spk_with(tpl)) {
                                    self.insert_holding(
                                        conn,
                                        &rec.txid,
                                        c.output,
                                        &token,
                                        tpl.id,
                                        &c.state,
                                        c.value,
                                        "custody",
                                        seq,
                                        ctx.header.daa_score,
                                    )?;
                                }
                            }
                        }
                    }
                    CreateOutcome::Rejected(reason) => self.record_reject(conn, seq, &rec.txid, &reason, delta)?,
                }
            }
        }
        Ok(())
    }

    /// Inserts an order lineage.
    ///
    /// Chain processing, if-done exits and recovery imports share it.
    pub fn create_order(&self, conn: &Connection, delta: &mut Delta, n: NewOrder) -> DbResult<CreateOutcome> {
        let id = n.state.template_id();
        if !model::is_order(id) {
            return Ok(CreateOutcome::Rejected("not_an_order_template".into()));
        }
        if order_exists(conn, &n.covenant_id)? {
            return Ok(CreateOutcome::Rejected("duplicate_covenant".into()));
        }
        let state_bytes = n.state.encode();
        let terms = model::terms_of(&n.state);
        let side = model::side_of(&n.state);
        let in_book = model::in_book(id);
        let token = Hash32(n.state.token_cov_id());
        let tpl_hash = n.state.token_tpl_hash().map(Hash32);
        let ext = model::extension_of(&n.state).or(n.ext_commit);
        let (budget_rate, reserve) = match &n.state {
            AnyState::KobBid(b) | AnyState::KobBidKron(b) => (b.budget_rate(), Some(b.reserve)),
            AnyState::KobIfdBid(b) | AnyState::KobIfdBidKron(b) => (b.budget_rate(), None),
            _ => (None, None),
        };
        let price = if in_book { terms.price } else { None };
        let initial_amount = n.state.amount_left();
        // An order whose numbers the covenant would refuse or the matcher cannot survive is never
        // listed. Exits are included: they take the decision of the parent only if their own
        // numbers pass.
        let sane = crate::sanity::check(&n.state);
        let pair = n.state.pair_tokens();
        let quote = pair.map(|t| Hash32(t.b.cov_id));
        let (listed, reason) = match n.inherit_listed {
            _ if sane.is_err() => (false, sane.err()),
            Some((l, r)) => (l, r),
            None => self.listing(conn, &n.state, ext, n.daa, n.out_value)?,
        };
        // The maker, token, program, extension, template and quote token are rows of `ids`. The few
        // that the orders name come first, because the migration assigns them the first ids. A new
        // one is added here.
        for h in [&template(id).hash[..], &n.state.maker()[..], &token.0[..]] {
            intern(conn, h)?;
        }
        for h in [tpl_hash.map(|t| t.0.to_vec()), ext.map(|e| e.to_vec()), quote.map(|q| q.0.to_vec())].into_iter().flatten() {
            intern(conn, &h)?;
        }
        conn.prepare_cached(
            "INSERT INTO orders (covenant_id, contract, template_id, family, side, maker_id, token_id, token_tpl_id, ext_id, scale, min_fill, price, tip, tif, expiry_daa, active_from, in_book, budget_rate, reserve, initial_amount, deadline, genesis_state, genesis_txid, genesis_out, genesis_block, genesis_daa, parent, listed, unlisted_reason, origin, quote_id, genesis_value)
             VALUES (?1,?2,(SELECT id FROM ids WHERE h = ?3),?4,?5,(SELECT id FROM ids WHERE h = ?6),(SELECT id FROM ids WHERE h = ?7),(SELECT id FROM ids WHERE h = ?8),(SELECT id FROM ids WHERE h = ?9),?10,?11,?12,?13,?14,?15,?16,?17,?18,?19,?20,?21,kob_gpack(?22,?2,?6,?7,?8,?31),?23,?24,?25,?26,?27,?28,?29,?30,(SELECT id FROM ids WHERE h = ?31),?32)",
        )?
        .execute(params![
            &n.covenant_id.0[..],
            id.name(),
            &template(id).hash[..],
            n.state.family().code() as i64,
            side as i64,
            &n.state.maker()[..],
            &token.0[..],
            tpl_hash.map(|t| t.0.to_vec()),
            ext.map(|e| e.to_vec()),
            terms.scale,
            terms.min_fill,
            price,
            terms.tip,
            terms.tif,
            terms.expiry_daa,
            terms.active_from,
            in_book as i64,
            budget_rate,
            reserve,
            initial_amount,
            n.deadline.map(|d| d as i64),
            &state_bytes[..],
            &n.txid.0[..],
            n.out_index,
            n.block_seq,
            n.daa as i64,
            n.parent.map(|p| p.0.to_vec()),
            listed as i64,
            reason,
            n.origin,
            quote.map(|q| q.0.to_vec()),
            n.out_value as i64,
        ])?;
        self.insert_utxo(conn, &n.covenant_id, &n.txid, n.out_index, n.out_value, &n.out_spk, Some(&state_bytes), n.block_seq, n.daa)?;
        for c in n.custody.iter().chain(n.prefund.iter()) {
            let ctok = c.token.unwrap_or(token);
            conn.prepare_cached(concat!(
                "INSERT OR IGNORE INTO token_utxos (txid, idx, token_id, order_id, amount, value, role, created_block, created_daa) VALUES (?1,?2,?3,",
                order_of!("?4"),
                ",?5,?6,'custody',?7,?8)"
            ))?
            .execute(params![&c.txid.0[..], c.index, intern(conn, &ctok.0)?, &n.covenant_id.0[..], c.amount, c.value as i64, n.block_seq, n.daa as i64])?;
            // A chain placement proves the state of the custody from its transfer leader, in step
            // 5b. An import verified the custody script that the state below produces, so it is
            // proven the same way.
            if n.origin == "import" {
                self.custody_holding(conn, &n.covenant_id, &n.state, c, ext, n.block_seq, n.daa)?;
            }
        }
        conn.prepare_cached(concat!(
            "INSERT INTO order_events (order_id, block_seq, daa, ts, txid, tx_pos, kind, token_id, side, amount, price, closes, detail) VALUES (",
            order_of!("?1"),
            ",?2,?3,?4,?5,?6,'create',?7,?8,?9,?10,0,kob_dpack(?11,'create'))"
        ))?
        .execute(params![
            &n.covenant_id.0[..],
            n.block_seq,
            n.daa as i64,
            n.ts as i64,
            &n.txid.0[..],
            n.tx_pos,
            intern(conn, &token.0)?,
            side as i64,
            initial_amount,
            price,
            json!({
                "contract": id.name(),
                "parent": n.parent.map(|p| p.to_hex()),
                "origin": n.origin,
                "deadline": n.deadline,
                "booked": model::parent_of(&n.state).is_some(),
            })
            .to_string()
        ])?;
        // Token registry event: the first sighting of this (token, program, extension) identity.
        for (tok, tpl, e) in placement_identities(&n.state, token, tpl_hash, ext) {
            let seen = conn
                .prepare_cached("SELECT 1 FROM token_events WHERE token_cov_id = ?1 AND tpl_hash IS ?2 AND ext_commit IS ?3")?
                .exists(params![&tok.0[..], tpl.map(|t| t.0.to_vec()), e.map(|e| e.to_vec())])?;
            if !seen {
                conn.prepare_cached(
                    "INSERT INTO token_events (token_cov_id, tpl_hash, ext_commit, kind, txid, block_seq, daa) VALUES (?1,?2,?3,'seen',?4,?5,?6)",
                )?
                .execute(params![&tok.0[..], tpl.map(|t| t.0.to_vec()), e.map(|e| e.to_vec()), &n.txid.0[..], n.block_seq, n.daa as i64])?;
            }
        }
        refresh_order_state(conn, &n.covenant_id)?;
        delta.orders.insert(n.covenant_id);
        delta.tokens.insert(token);
        // A pair order is in the pair book of both its tokens, so `book:<B>` subscribers refetch
        // too.
        delta.tokens.extend(quote);
        delta.new_orders += 1;
        Ok(CreateOutcome::Created)
    }

    /// Applies the listing rules (`tokens::ListingRules`) to the state of an order.
    ///
    /// `daa` is the DAA score of its genesis, or of the block of an in-place amend. `carrier` is
    /// the KAS on its output.
    fn listing(
        &self,
        conn: &Connection,
        state: &AnyState,
        ext: Option<[u8; 32]>,
        daa: u64,
        carrier: u64,
    ) -> DbResult<(bool, Option<String>)> {
        // A pair order needs both tokens acceptable: A as the token of the order, and B as any
        // traded token, with reasons `quote_...`. Neither is measured in KAS, because a pair order
        // has no KAS quote.
        if let Some((mut ia, mut ib)) = pair_listing(state, daa, carrier) {
            ia.proven_tpls = proven_hashes(conn, ia.token_cov_id)?;
            ib.proven_tpls = proven_hashes(conn, ib.token_cov_id)?;
            let r = self
                .rules
                .evaluate(&self.tokens, &ia)
                .and_then(|()| self.rules.evaluate(&self.tokens, &ib).map_err(|r| format!("quote_{r}")));
            return Ok(match r {
                Ok(()) => (true, None),
                Err(r) => (false, Some(r)),
            });
        }
        let terms = model::terms_of(state);
        // A conditional order is measured at its take-profit leg, or at its stop when it has none.
        // It has none when `tpPrice` is 0, as for a stop-market, stop-limit or trailing stop. Such
        // a stop would be worth 0 at the absent take-profit and would never list.
        let listing_price = match state {
            AnyState::KobCondAsk(c) | AnyState::KobCondAskKron(c) => Some(if c.tp_price > 0 { c.tp_price } else { c.stop_price }),
            AnyState::KobCondBid(c) | AnyState::KobCondBidKron(c) => Some(if c.tp_price > 0 { c.tp_price } else { c.stop_price }),
            _ => terms.price,
        };
        // The scale that the decimals of an open-list token fix. It comes from its KCC-23
        // declaration, else from the convention of its program. See `ListingInput`.
        let standard_scale = if self.tokens.get(&Hash32(state.token_cov_id())).is_none() {
            super::reads::open_standard_scale(conn, &Hash32(state.token_cov_id()), state.token_tpl_hash().map(Hash32).as_ref())?
        } else {
            None
        };
        let r = self.rules.evaluate(
            &self.tokens,
            &ListingInput {
                standard_scale,
                token_cov_id: Some(Hash32(state.token_cov_id())),
                token_tpl_hash: state.token_tpl_hash().map(Hash32),
                extension_commitment: ext.map(Hash32),
                family: Some(state.family()),
                scale: Some(terms.scale).filter(|s| *s > 0),
                min_fill: Some(terms.min_fill),
                // A bid has no amount. Its quantity and value are its escrow, the carrier.
                amount: state.amount_left(),
                price: listing_price,
                tip: Some(terms.tip),
                expiry_daa: Some(terms.expiry_daa),
                genesis_daa: daa,
                carrier,
                keeper_tip: listing_keeper_tip(state, carrier),
                proven_tpls: proven_hashes(conn, Some(Hash32(state.token_cov_id())))?,
            },
        );
        Ok(match r {
            Ok(()) => (true, None),
            Err(r) => (false, Some(r)),
        })
    }

    /// The state that an in-place amend continues a maker-cancelled order with at output `j`.
    ///
    /// The result also carries the deadline of the amend.
    ///
    /// An AMEND record must name exactly this continuation, and it must verify:
    /// - [`payload::verify_amend`] accepts it against the revealed previous state. That covers the
    ///   template, the maker's `cancel`, terms that keep the custody exact, the output being the
    ///   P2SH of the new state, and its being the only output that carries the id.
    /// - The numeric gate accepts the state.
    /// - The custody of the order is still live after the transaction. An amend never moves it.
    ///
    /// A record that fails is a reject, and the continuation stays unproven: its state is unknown
    /// and it is listed nowhere.
    #[allow(clippy::too_many_arguments)]
    fn amend_of(
        &self,
        conn: &Connection,
        rec: &TxRecord,
        s: &Spent,
        j: usize,
        amends: &[Record],
        seq: i64,
        delta: &mut Delta,
    ) -> DbResult<Option<(AnyState, Option<u64>)>> {
        let named =
            |r: &&Record| matches!(r, Record::Amend { output, input, .. } if *output as usize == j && *input as usize == s.input_idx);
        let Some(r) = amends.iter().find(named) else { return Ok(None) };
        let Some(before) = &s.before else {
            self.record_reject(conn, seq, &rec.txid, "amend:previous_state_unknown", delta)?;
            return Ok(None);
        };
        // The spent script is proven, because its redeem script hashed to the spent output at
        // extraction. The input is its P2SH.
        let mut tx = tx_json(rec, rec.payload.clone());
        if let Some(i) = tx.inputs.get_mut(s.input_idx) {
            i.utxo.script_public_key = kob_protocol::tx::spk_to_string(&before.spk());
        }
        let a = match payload::verify_amend(&tx, r, before, s.entry.unwrap_or("")) {
            Ok(a) => a,
            Err(e) => {
                self.record_reject(conn, seq, &rec.txid, &format!("amend:{e}"), delta)?;
                return Ok(None);
            }
        };
        if let Err(why) = crate::sanity::check(&a.order) {
            self.record_reject(conn, seq, &rec.txid, &format!("amend:{why}"), delta)?;
            return Ok(None);
        }
        // The custody of an ask stays where it is. A bid owns none: its quantity is the escrow of
        // the continuation.
        let holds_custody = matches!(a.order, AnyState::KobAsk(_) | AnyState::KobAskKron(_));
        if holds_custody && !has_live_custody(conn, &s.cov)? {
            self.record_reject(conn, seq, &rec.txid, "amend:custody_not_live", delta)?;
            return Ok(None);
        }
        Ok(Some((a.order, a.deadline)))
    }

    /// The state that a sweep in place continues a maker-cancelled order with at output `j`.
    ///
    /// It is the SAME state. A SWEEP record must name exactly this continuation, and
    /// [`payload::verify_sweep`] must accept it against the revealed previous state. The custody of
    /// the order, for token-holding kinds with an amount left, must still be live after the
    /// transaction. A sweep never moves it. A record that fails is a reject, and the continuation
    /// stays unproven.
    #[allow(clippy::too_many_arguments)]
    fn sweep_of(
        &self,
        conn: &Connection,
        rec: &TxRecord,
        s: &Spent,
        j: usize,
        records: &[Record],
        seq: i64,
        delta: &mut Delta,
    ) -> DbResult<Option<AnyState>> {
        let named =
            |r: &&Record| matches!(r, Record::Sweep { output, input } if *output as usize == j && *input as usize == s.input_idx);
        let Some(r) = records.iter().find(named) else { return Ok(None) };
        let Some(before) = &s.before else {
            self.record_reject(conn, seq, &rec.txid, "sweep:previous_state_unknown", delta)?;
            return Ok(None);
        };
        let mut tx = tx_json(rec, rec.payload.clone());
        if let Some(i) = tx.inputs.get_mut(s.input_idx) {
            i.utxo.script_public_key = kob_protocol::tx::spk_to_string(&before.spk());
        }
        if let Err(e) = payload::verify_sweep(&tx, r, before, s.entry.unwrap_or("")) {
            self.record_reject(conn, seq, &rec.txid, &format!("sweep:{e}"), delta)?;
            return Ok(None);
        }
        if before.custody_amount().unwrap_or(0) > 0 && !has_live_custody(conn, &s.cov)? {
            self.record_reject(conn, seq, &rec.txid, "sweep:custody_not_live", delta)?;
            return Ok(None);
        }
        Ok(Some(before.clone()))
    }

    /// Takes the terms of an in-place amend into the order row and re-lists it.
    ///
    /// The terms are the price, tip, minimum fill, time in force, expiry, activation and deadline.
    /// The listing rules run as of the block of the amend, with the carrier of the continuation.
    /// The replaced values are kept in `order_amends`, stamped with the block, so a reorg restores
    /// them exactly ([`revert_block`]).
    fn apply_amend(
        &self,
        conn: &Connection,
        ctx: &BlockCtx<'_>,
        cov: &Hash32,
        state: &AnyState,
        deadline: Option<u64>,
        carrier: u64,
    ) -> DbResult<()> {
        conn.prepare_cached(
            "INSERT INTO order_amends (order_id, block_seq, price, tip, tif, expiry_daa, active_from, deadline, listed, unlisted_reason, budget_rate, reserve, min_fill) \
             SELECT id, ?2, price, tip, tif, expiry_daa, active_from, deadline, listed, unlisted_reason, budget_rate, reserve, min_fill FROM orders WHERE covenant_id = ?1",
        )?
        .execute(params![&cov.0[..], ctx.seq])?;
        // The budget rate and reserve of a bid follow its new terms. Its remaining amount is the
        // buying power of its escrow.
        if let AnyState::KobBid(b) | AnyState::KobBidKron(b) = state {
            conn.prepare_cached("UPDATE orders SET budget_rate = ?2, reserve = ?3 WHERE covenant_id = ?1")?.execute(params![
                &cov.0[..],
                b.budget_rate(),
                b.reserve
            ])?;
        }
        let t = model::terms_of(state);
        let (listed, reason) = match crate::sanity::check(state) {
            Err(why) => (false, Some(why)),
            Ok(()) => self.listing(conn, state, order_ext_commit(conn, cov)?, ctx.header.daa_score, carrier)?,
        };
        conn.prepare_cached(
            "UPDATE orders SET price = ?2, tip = ?3, tif = ?4, expiry_daa = ?5, active_from = ?6, deadline = ?7, listed = ?8, \
             unlisted_reason = ?9, min_fill = ?10 WHERE covenant_id = ?1",
        )?
        .execute(params![
            &cov.0[..],
            t.price,
            t.tip,
            t.tif,
            t.expiry_daa,
            t.active_from,
            deadline.map(|d| d as i64),
            listed as i64,
            reason,
            t.min_fill
        ])?;
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    fn insert_utxo(
        &self,
        conn: &Connection,
        cov: &Hash32,
        txid: &Hash32,
        idx: u32,
        value: u64,
        spk: &[u8],
        state: Option<&[u8]>,
        seq: i64,
        daa: u64,
    ) -> DbResult<()> {
        conn.prepare_cached(
            // The state is stored as its difference from the genesis state of the order, see
            // [`super::packed`]. The order row is written before its first UTXO. An output of a
            // covenant with no order row is not stored, because no read could see it.
            //
            // A stored row of the same outpoint is replaced while it is unspent. A spent one stays
            // spent: an output of a block is written again only when its transaction is applied
            // twice (a reorg deeper than the window, or a lying node), and a reset row would bring
            // the spent order back to the book. The recovery operations (block 0) write outputs the
            // node verified as unspent, and replace either.
            concat!(
                "INSERT INTO order_utxos (txid, idx, order_id, value, spk, state, created_block, created_daa) \
                 SELECT ?1, ?2, g.id, ?4, ?5, kob_pack(?6, ",
                sql_genesis!("g"),
                "), ?7, ?8 FROM orders g",
                sql_order_ids!("g"),
                " WHERE g.covenant_id = ?3 \
                 ON CONFLICT (txid, idx) DO UPDATE SET order_id = excluded.order_id, value = excluded.value, spk = excluded.spk, \
                   state = excluded.state, created_block = excluded.created_block, created_daa = excluded.created_daa, spent_block = NULL, \
                   spent_txid = NULL, spent_entry = NULL, spent_amount = NULL \
                 WHERE order_utxos.spent_block IS NULL OR excluded.created_block = 0"
            ),
        )?
        .execute(params![&txid.0[..], idx, &cov.0[..], value as i64, spk, state, seq, daa as i64])?;
        Ok(())
    }

    // -----------------------------------------------------------------------------------------
    // Spends.

    fn spend_order(&self, conn: &Connection, seq: i64, rec: &TxRecord, input_idx: usize, u: UtxoRow) -> DbResult<Option<Spent>> {
        let value = u.value;
        let Some(order) = load_order(conn, &u.covenant_id)? else { return Ok(None) };
        let inp = &rec.inputs[input_idx];
        let reveal = inp.reveal.clone().filter(|r| r.template == order.template);
        let entry = reveal.as_ref().and_then(|r| r.entry());
        let act = match (&reveal, entry) {
            (Some(r), Some(e)) => match classify(e, r.nb()) {
                // The update of a pair conditional or a pair stop entry is `settle` or `fill`, with
                // nb = 0 and upd = 1.
                Act::Refund if pair_upd(r) => Act::Update,
                a => a,
            },
            _ => Act::Unknown,
        };
        let before = reveal.as_ref().and_then(|r| AnyState::decode(r.template, &r.state).ok());
        let filled = if let Act::Fill(n) = act { Some(n) } else { None };
        conn.prepare_cached(
            "UPDATE order_utxos SET spent_block = ?1, spent_txid = ?2, spent_entry = ?3, spent_amount = ?4 WHERE txid = ?5 AND idx = ?6",
        )?
        .execute(params![seq, &rec.txid.0[..], entry, filled, &inp.txid.0[..], inp.index])?;
        Ok(Some(Spent {
            cov: u.covenant_id,
            order,
            input_idx,
            entry,
            act,
            reveal,
            before,
            utxo_daa: inp.daa as i64,
            value,
            after: None,
            amend: None,
            sweep: false,
        }))
    }

    // -----------------------------------------------------------------------------------------
    // Derivations. Each result is proven against the script public key of the output.

    /// The state of the continuation output, if it can be proven.
    ///
    /// It is the spent script with its mutable windows (`amountLeft`, `armed`, `stopPrice`,
    /// `rptAmount`) replaced by values that the entry call could have written. Merges, fills, arms
    /// and trailing steps all fall in the candidate sets. The P2SH of the spliced script decides.
    fn derive_after(&self, s: &Spent, out_spk: &[u8], evidence_prices: &[i64], rec: &TxRecord) -> Option<AnyState> {
        let before = s.before.as_ref()?;
        let id = before.template_id();
        let t = template(id);
        let redeem = before.redeem();
        // Unchanged script, as for bid fills and updates that changed nothing.
        if p2sh_spk(&redeem) == out_spk {
            return Some(before.clone());
        }
        let windows = kob_protocol::state::mutable_windows(id);
        if windows.is_empty() {
            return None;
        }
        let cur = model::mutable_of(before);
        let is_update = s.act == Act::Update;
        // The fill or merge amount comes from the chain. A spend the covenant accepted keeps it in
        // range, and the arithmetic is checked anyway.
        let mut amounts: Vec<Option<i64>> = vec![cur.amount];
        let mut rpt: Vec<Option<i64>> = vec![cur.rpt];
        match s.act {
            Act::Fill(n) => {
                amounts.extend(cur.amount.map(|l| l.checked_sub(n)));
                rpt.extend(cur.rpt.map(|r| r.checked_sub(n)));
            }
            Act::Merge { m, .. } => amounts.extend(cur.amount.map(|l| l.checked_add(m))),
            _ => {}
        }
        let mut armed: Vec<Option<i64>> = vec![cur.armed];
        if cur.armed.is_some() {
            armed.extend([Some(1), Some(s.utxo_daa), Some(0)]);
        }
        let mut stops: Vec<Option<i64>> = vec![cur.stop];
        if let (true, Some(stop)) = (is_update, cur.stop) {
            let (step, ks) = match (before, s.reveal.as_ref()) {
                // A pair conditional trails by the k that its pair evidence justifies, see
                // `CondPairState::trail_k`.
                (AnyState::KobCondPair(c), Some(r)) => {
                    let mut ks: Vec<i64> =
                        pair_evidence_of(rec, r, PAIR_EV_COND).and_then(|(ev, _)| c.trail_k(ev)).into_iter().collect();
                    ks.extend(1..=64);
                    (c.trail_step, ks)
                }
                _ => trail_candidates(before, evidence_prices),
            };
            for k in ks {
                if let Some(d) = k.checked_mul(step) {
                    stops.extend([stop.checked_add(d).map(Some), stop.checked_sub(d).map(Some)].into_iter().flatten());
                }
            }
        }
        // The exact custody of a pair order, `custody`: what each entry call can leave in it. The
        // P2SH decides.
        let mut custs: Vec<Option<i64>> = vec![cur.custody];
        if let Some(c) = cur.custody {
            let arg = |i: usize| s.reveal.as_ref().and_then(|r| r.int(i));
            match (before, s.act) {
                (AnyState::KobPair(_) | AnyState::KobCondPair(_), Act::Fill(n)) => {
                    // `sOut` (argument 4) is released. An ask releases n.
                    custs.extend([arg(4), Some(n)].into_iter().flatten().map(|d| c.checked_sub(d)));
                    if let AnyState::KobCondPair(x) = before {
                        // A re-arming BID exit keeps custody - proceeds - back while it continues.
                        let keep = guarded(|| c.checked_sub(x.rpt_proceeds(n)?)?.checked_sub(x.rpt_back(n)?)).flatten();
                        custs.push(keep);
                    }
                    custs.push(Some(0));
                }
                (AnyState::KobIfdPair(e), Act::Fill(n)) => {
                    // Buy-first leaves the escrow minus the spend (argument `amt`, 6). Sell-first
                    // leaves the prefund minus pre(n).
                    custs.extend(arg(6).map(|a| c.checked_sub(a)));
                    custs.push(guarded(|| c.checked_sub(e.pre_of(n)?)).flatten());
                    custs.push(Some(0));
                }
                (AnyState::KobIfdPair(e), Act::Merge { m, .. }) => {
                    // Buy-first grows the escrow by budget(m). Sell-first grows the prefund by
                    // pre(m).
                    custs.push(guarded(|| c.checked_add(e.merge_budget(m)?)).flatten());
                    custs.push(guarded(|| c.checked_add(e.pre_of(m)?)).flatten());
                }
                _ => {}
            }
        }
        let mut tried: HashSet<Mutable> = HashSet::new();
        for l in &amounts {
            for a in &armed {
                for st in &stops {
                    for r in &rpt {
                        for cu in &custs {
                            let m = Mutable { amount: *l, armed: *a, stop: *st, rpt: *r, custody: *cu };
                            if !tried.insert(m) {
                                continue;
                            }
                            let mut rs = redeem.clone();
                            for (name, a, b) in windows {
                                let v = match *name {
                                    "amountLeft" => m.amount,
                                    "armed" => m.armed,
                                    "stopPrice" => m.stop,
                                    "rptAmount" => m.rpt,
                                    "custody" => m.custody,
                                    _ => None,
                                };
                                if let Some(v) = v {
                                    rs[*a..*b].copy_from_slice(&v.to_le_bytes());
                                }
                            }
                            if p2sh_spk(&rs) == out_spk {
                                let span = &rs[t.prefix.len()..t.prefix.len() + t.state_len];
                                return AnyState::decode(id, span).ok();
                            }
                        }
                    }
                }
            }
        }
        None
    }

    /// The exit that an if-done entry fill created.
    ///
    /// It is the committed exit with `amountLeft = n` and the repeat fields the entry wrote, booked
    /// while re-arms remain. It returns the exit state, its covenant id and its output index.
    fn derive_exit(
        &self,
        s: &Spent,
        rec: &TxRecord,
        spent_covs: &HashSet<Hash32>,
        conn: &Connection,
    ) -> DbResult<Option<(AnyState, Hash32, usize)>> {
        let (Some(before), Act::Fill(n), Some(reveal)) = (&s.before, s.act, &s.reveal) else { return Ok(None) };
        let exit_arg = match before {
            AnyState::KobIfdBid(_) | AnyState::KobIfdBidKron(_) => reveal.int(2),
            AnyState::KobIfdAsk(_) | AnyState::KobIfdAskKron(_) => reveal.int(3),
            AnyState::KobIfdPair(_) => reveal.int(5),
            _ => None,
        };
        let entry_cov = s.cov.0;
        let candidates: Vec<AnyState> = match before {
            AnyState::KobIfdBid(b) | AnyState::KobIfdBidKron(b) => exit_candidates_bid(b, n, entry_cov, s.utxo_daa, before.family()),
            AnyState::KobIfdAsk(a) | AnyState::KobIfdAskKron(a) => exit_candidates_ask(a, n, entry_cov, s.utxo_daa, before.family()),
            AnyState::KobIfdPair(e) => exit_candidates_pair(e, n, entry_cov, s.utxo_daa, reveal.int(6)),
            _ => vec![],
        };
        let mut order: Vec<usize> = exit_arg.and_then(|j| usize::try_from(j).ok()).into_iter().collect();
        order.extend(0..rec.outputs.len());
        for j in order {
            let Some(out) = rec.outputs.get(j) else { continue };
            let Some((_, cov)) = out.cov else { continue };
            if cov == s.cov || spent_covs.contains(&cov) || order_exists(conn, &cov)? {
                continue;
            }
            for c in &candidates {
                if spk_bytes(&c.spk()) == out.spk {
                    return Ok(Some((c.clone(), cov, j)));
                }
            }
        }
        Ok(None)
    }

    // -----------------------------------------------------------------------------------------
    // Events.

    #[allow(clippy::too_many_arguments)]
    fn record_spend_events(
        &self,
        conn: &Connection,
        ctx: &BlockCtx<'_>,
        rec: &TxRecord,
        s: &Spent,
        closes: bool,
        exits: &HashMap<Hash32, Hash32>,
        all: &[Spent],
        delta: &mut Delta,
    ) -> DbResult<()> {
        let txid = rec.txid;
        let seq = ctx.seq;
        let daa = ctx.header.daa_score as i64;
        let ts = ctx.header.timestamp as i64;
        let ioc = matches!(s.order.tif, Some(1) | Some(2));
        let amount_before = s.before.as_ref().and_then(|b| b.amount_left());
        let pair = s.before.as_ref().filter(|b| b.is_pair());
        let is_pair = model::is_pair(s.order.template);
        // A pair order is paid in token B or A. Its positional output is a token delivery, and its
        // KAS is the delivery carrier.
        let payout = if matches!(s.act, Act::Fill(_)) && s.order.side == 1 && !is_pair {
            rec.outputs.get(s.input_idx).map(|o| o.value as i64)
        } else {
            None
        };
        let mut detail = json!({"input": s.input_idx, "entry": s.entry, "verified": s.before.is_some()});
        // Which plain resting fill of this transaction armed, ratcheted or triggered the order. For
        // a pair order this is its pair evidence: two KAS-book fills of A and B, or a resting
        // `KobPair` of the pair.
        if let Some((b, r)) = s.before.as_ref().zip(s.reveal.as_ref()) {
            if b.is_pair() {
                if let Some(v) = pair_evidence_detail(rec, b, r, s.act) {
                    detail["evidence"] = v;
                }
            } else if let Some(k) = evidence_input(b, r, s.act) {
                detail["evidence"] = evidence_detail(rec, k);
            }
        }
        let mut pair_fill: Option<PairFillRow> = None;
        let mut events: Vec<(&str, Option<i64>, Option<i64>, bool)> = Vec::new(); // (kind, amount, price, closes)
        match s.act {
            Act::Fill(n) => {
                let quote = s.before.as_ref().and_then(|b| self.fill_quote(b, s, n));
                // The time argument of the call is the last one. The pair kinds differ: `settle`
                // has it at argument 3 and `fill` at argument 13.
                let t_arg = s.reveal.as_ref().and_then(|r| match s.order.template {
                    TemplateId::KobPair | TemplateId::KobCondPair => r.int(3),
                    TemplateId::KobIfdPair => r.int(13),
                    _ => r.args.last().and_then(|a| crate::script::script_int(a)),
                });
                if let Some(t) = t_arg {
                    detail["t"] = json!(t);
                }
                if let Some(x) = exits.get(&s.cov) {
                    detail["exit"] = json!(x.to_hex());
                }
                // A pair fill has no KAS price, see the price rule in docs/spec/matcher.md. The
                // `price` of the event is null, and it is never a KAS trade, candle or last price.
                // `detail.pair` carries the pair fields and `pair_fills` its volume.
                if let (Some(b), Some(r)) = (pair, s.reveal.as_ref()) {
                    let row = pair_fill_of(rec, s.input_idx, b, r, n, quote);
                    detail["pair"] = row.detail();
                    pair_fill = Some(row);
                }
                // Bid side. Record what the fill drew from the escrow, since the continuation keeps
                // the rest, and the delivery output at the position of the input.
                //
                // The delivery output is the token output of the maker. Its KAS is the delivery
                // carrier plus used(n) - allIn(t). That is the difference between the budget the
                // fill consumed at pMax + tip, rounded up, and what it paid at its quote, rounded
                // down.
                //
                // A reader can then check that a buy never cost more than its all-in limit, with
                // escrow_after = 0 when the bid sold out:
                //   escrow_before - escrow_after - delivery_value - refund_value
                //     <= floor(n * (q + tip) / scale)
                // `refund_value` is the plain refund output of the maker for a terminal fill. It is
                // present from 1 KAS up.
                if s.order.side == 2 && !is_pair {
                    if let Some(o) = rec.outputs.get(s.input_idx) {
                        detail["delivery_value"] = json!(o.value.to_string());
                    }
                    if let Some(input) = rec.inputs.get(s.input_idx) {
                        let before: Option<i64> = conn
                            .prepare_cached("SELECT value FROM order_utxos WHERE txid = ?1 AND idx = ?2")?
                            .query_row(params![&input.txid.0[..], input.index as i64], |r| r.get(0))
                            .optional()?;
                        if let Some(v) = before {
                            detail["escrow_before"] = json!(v.to_string());
                            // A terminal fill of a plain bid, one with no continuation, sends the
                            // KAS beyond the delivery carrier back to the maker as an output of its
                            // own. It does so once that remainder is REFUND_MIN or more. This is
                            // the `refundTo` of the covenant: output M - 1 - i for the bid at input
                            // i. A smaller remainder stays on the delivery.
                            //
                            // The covenant fixes which case applies from the state alone, so the
                            // refund output is identified without reading scripts.
                            let continues = rec.outputs.iter().any(|o| o.cov.is_some_and(|(_, c)| c == s.cov));
                            if let (false, Some(AnyState::KobBid(b) | AnyState::KobBidKron(b)), Some(q)) =
                                (continues, s.before.as_ref(), quote)
                            {
                                let rem = b.spend_at(n, q).and_then(|all_in| v.checked_sub(all_in)?.checked_sub(b.delivery_carrier));
                                let slot = rec.outputs.len().checked_sub(1 + s.input_idx);
                                if let (Some(rem), Some(slot)) = (rem, slot) {
                                    if rem >= kob_protocol::tx::REFUND_MIN as i64 {
                                        if let Some(o) = rec.outputs.get(slot).filter(|o| o.cov.is_none()) {
                                            detail["refund_value"] = json!(o.value.to_string());
                                        }
                                    }
                                }
                            }
                        }
                    }
                    if let Some(next) = rec.outputs.iter().find(|o| o.cov.is_some_and(|(_, c)| c == s.cov)) {
                        detail["escrow_after"] = json!(next.value.to_string());
                    }
                }
                if let Some(parent) = s.before.as_ref().and_then(model::parent_of) {
                    let ph = Hash32(parent);
                    if all.iter().any(|o| o.cov == ph && matches!(o.act, Act::Merge { .. })) {
                        detail["merged_into"] = json!(ph.to_hex());
                    }
                }
                let returned = if closes { amount_before.and_then(|l| l.checked_sub(n)).filter(|r| *r > 0) } else { None };
                // The price of the event is the KAS quote of a KAS kind. It is null for a pair
                // fill, whose quote in B is in `detail.pair`.
                let event_price = if is_pair { None } else { quote.or(s.order.price) };
                events.push(("fill", Some(n), event_price, closes && returned.is_none()));
                if let Some(r) = returned {
                    events.push(("kill", Some(r), None, true));
                }
            }
            Act::Merge { k, m } => {
                if let Some(x) = rec.inputs.get(k).and_then(|i| i.cov) {
                    detail["exit"] = json!(x.to_hex());
                }
                events.push(("rearm", Some(m), None, false));
            }
            Act::Refund => {
                let kind = if ioc { "kill" } else { "refund" };
                events.push((kind, amount_before, None, closes));
            }
            Act::Cancel if s.amend.is_some() => {
                // In-place amend: the same order, with the same covenant id and custody, and new
                // terms. `detail.previous` holds the replaced ones.
                if let Some(b) = &s.before {
                    let t = model::terms_of(b);
                    detail["previous"] =
                        json!({"price": t.price, "tip": t.tip, "tif": t.tif, "expiryDaa": t.expiry_daa, "activeFrom": t.active_from});
                }
                let after = s.after.as_ref();
                events.push(("amend", after.and_then(|a| a.amount_left()), after.and_then(|a| model::terms_of(a).price), false));
            }
            Act::Cancel if s.sweep => {
                // Sweep in place: the same order at a new output, with its strays returned to the
                // maker.
                let after = s.after.as_ref();
                events.push(("sweep", after.and_then(|a| a.amount_left()), None, false));
            }
            Act::Cancel => events.push(("cancel", None, None, closes)),
            Act::Update => {
                let kind = match (&s.before, &s.after) {
                    (Some(b), Some(a)) => {
                        let (mb, ma) = (model::mutable_of(b), model::mutable_of(a));
                        if mb.stop != ma.stop {
                            "trail"
                        } else if mb.armed != ma.armed {
                            "arm"
                        } else {
                            "update"
                        }
                    }
                    _ => "update",
                };
                events.push((kind, None, None, closes));
            }
            Act::Unknown => events.push(("unknown", None, None, closes)),
        }
        // For a fill: its priority tip, and the script that funded the transaction. The tip is the
        // one the terms in force name (an amend of an earlier transaction has already taken its new
        // tip into the row), at most the KAS the fill moved for the order. An unproven state, or one
        // that fails the sanity check, records none. See `activity::fill_tip` and `GET /v1/stats`.
        let (fee, executor) = match events.iter().find(|e| e.0 == "fill") {
            Some(&(_, amount, _, _)) => {
                let terms: Option<(Option<i64>, Option<i64>)> = conn
                    .prepare_cached("SELECT tip, scale FROM orders WHERE covenant_id = ?1")?
                    .query_row([&s.cov.0[..]], |r| Ok((r.get(0)?, r.get(1)?)))
                    .optional()?;
                let sane = s.before.as_ref().is_some_and(|b| crate::sanity::check(b).is_ok());
                let moved = fill_moved(rec, s, payout);
                let fee = terms
                    .filter(|_| sane)
                    .and_then(|(tip, scale)| super::activity::fill_tip(super::activity::fill_fee(amount, tip, scale), moved));
                (fee, rec.payer.as_deref().map(|spk| super::activity::intern_script(conn, spk)).transpose()?)
            }
            None => (None, None),
        };
        for (i, (kind, amount, price, closing)) in events.iter().enumerate() {
            let d = if i == 0 {
                detail.to_string()
            } else {
                json!({"input": s.input_idx, "entry": s.entry, "remainder": true}).to_string()
            };
            conn.prepare_cached(concat!(
                "INSERT INTO order_events (order_id, block_seq, daa, ts, txid, tx_pos, kind, token_id, side, amount, price, payout, closes, detail, fee, executor_id) VALUES (",
                order_of!("?1"),
                ",?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,kob_dpack(?14,?7),?15,?16)"
            ))?
            .execute(params![
                &s.cov.0[..],
                seq,
                daa,
                ts,
                &txid.0[..],
                rec.pos as i64,
                kind,
                s.order.token.map(|t| intern(conn, &t.0)).transpose()?,
                s.order.side as i64,
                amount,
                price,
                if i == 0 { payout } else { None },
                *closing as i64,
                d,
                if *kind == "fill" { fee } else { None },
                if *kind == "fill" { executor } else { None }
            ])?;
            // A fill moves the candle bucket of its token. It is recomputed before the transaction
            // commits, in `candles::refresh`.
            if *kind == "fill" {
                if let Some(t) = s.order.token {
                    super::candles::mark(conn, &t.0, ts)?;
                }
            }
        }
        if let Some(p) = &pair_fill {
            p.insert(conn, seq, daa, ts, &txid, rec.pos as i64, &s.cov, s.order.template)?;
        }
        let added = events.iter().filter(|e| e.0 == "fill").fold(0i64, |a, e| a.saturating_add(e.1.unwrap_or(0).max(0)));
        refresh_order_state_adding(conn, &s.cov, added)?;
        delta.orders.insert(s.cov);
        if let Some(t) = s.order.token {
            delta.tokens.insert(t);
        }
        delta.tokens.extend(s.order.quote);
        if let Act::Fill(l) = s.act {
            delta.fills.push(FillNotice {
                order: s.cov,
                token: s.order.token,
                side: s.order.side,
                price: s.order.price,
                amount: l,
                payout,
                txid,
                block: ctx.header.hash,
                daa: ctx.header.daa_score,
            });
        }
        Ok(())
    }

    /// The quote bound of a fill at the time argument of the call.
    ///
    /// For auctions it is the price that the covenant bounds the fill by.
    fn fill_quote(&self, before: &AnyState, s: &Spent, _n: i64) -> Option<i64> {
        let r = s.reveal.as_ref()?;
        let daa = s.utxo_daa;
        // The time argument of the call is the last one. The pair kinds differ: `settle` of
        // `KobPair` and `KobCondPair` has it at argument 3, and `fill` of `KobIfdPair` at argument
        // 13.
        let t = match before {
            AnyState::KobPair(_) | AnyState::KobCondPair(_) => r.int(3),
            AnyState::KobIfdPair(_) => r.int(13),
            _ => r.args.last().and_then(|a| crate::script::script_int(a)),
        }?;
        guarded(|| match before {
            AnyState::KobAsk(a) | AnyState::KobAskKron(a) => a.price_at(t, daa),
            AnyState::KobBid(b) | AnyState::KobBidKron(b) => b.price_at(t, daa),
            AnyState::KobCondAsk(c) | AnyState::KobCondAskKron(c) => {
                let leg = r.int(3)?;
                c.leg_price(leg, c.armed == 0 && leg == 1, t, daa)
            }
            AnyState::KobCondBid(c) | AnyState::KobCondBidKron(c) => {
                let leg = r.int(2)?;
                c.leg_price(leg, c.armed == 0 && leg == 1, t, daa)
            }
            AnyState::KobIfdBid(b) | AnyState::KobIfdBidKron(b) => b.price_at(b.entry_stop > 0 && b.armed == 0, t, daa),
            AnyState::KobIfdAsk(a) | AnyState::KobIfdAskKron(a) => a.price_at(a.entry_stop > 0 && a.armed == 0, t, daa),
            // The pair kinds: the quote in token B per whole A, never a KAS price.
            AnyState::KobPair(p) => p.price_at(t, daa),
            AnyState::KobCondPair(c) => {
                let leg = r.int(6)?;
                c.leg_price(leg, c.armed == 0 && leg == 1, t, daa)
            }
            AnyState::KobIfdPair(e) => e.price_at(e.entry_stop > 0 && e.armed == 0, t, daa),
        })
        .flatten()
    }

    /// Step 8 of a transaction that spent tracked orders: records the `executor_txs` row.
    ///
    /// A transaction that is not only the makers' own, as cancels, in-place amends and sweeps are,
    /// is an executor's and gets a row with:
    /// - Its income, see [`super::income`]. It is `kob_in`, what the KOB inputs held, minus the
    ///   outputs the orders own or pay.
    /// - Its network fee, the input value of the record minus its outputs.
    /// - Whether it is an own trade, `own_trade`. The sender trades for itself, and its income is
    ///   not counted, see [`super::income`].
    ///
    /// The income and the fee are each `None` when the chain data does not settle them.
    ///
    /// `orphan` means an order UTXO was spent whose order is not stored, so its outputs cannot be
    /// told. `foreign_cov_in` means a covenant input is no order's and no custody or stray of one.
    #[allow(clippy::too_many_arguments)]
    fn executor_tx(
        &self,
        conn: &Connection,
        ctx: &BlockCtx<'_>,
        rec: &TxRecord,
        spent: &[Spent],
        kob_in: i128,
        (orphan, foreign_cov_in): (bool, bool),
        continued: &HashSet<Hash32>,
        created: &HashSet<Hash32>,
        resolved_in: Option<i128>,
    ) -> DbResult<()> {
        use super::income::{self, Leg, Spend};
        if spent.iter().all(|s| s.act == Act::Cancel) {
            return Ok(());
        }
        let spent_covs: HashSet<Hash32> = spent.iter().map(|s| s.cov).collect();
        let n_out = rec.outputs.len();
        let income = (|| {
            if orphan {
                return None;
            }
            // Every output that carries the covenant id of an order, as continuations, exits and
            // placements do, and every token output owned by one.
            let mut kob_out: BTreeSet<usize> = rec
                .outputs
                .iter()
                .enumerate()
                .filter(|(_, o)| o.cov.is_some_and(|(_, c)| spent_covs.contains(&c) || created.contains(&c)))
                .map(|(j, _)| j)
                .collect();
            kob_out.extend(rec.tok_outs.iter().map(|t| t.out as usize));
            // And what the covenant of each spent order pins by position.
            for s in spent {
                let spend = match s.act {
                    Act::Fill(n) => Spend::Fill(n),
                    Act::Merge { .. } => Spend::Merge,
                    Act::Refund => Spend::Refund,
                    Act::Update => Spend::Update,
                    // A maker's cancel next to the legs of an executor routes outputs that no
                    // covenant pins. An unclassified spend does the same.
                    Act::Cancel | Act::Unknown => return None,
                };
                let (state, reveal) = (s.before.as_ref()?, s.reveal.as_ref()?);
                let quote = match s.act {
                    Act::Fill(n) => self.fill_quote(state, s, n),
                    _ => None,
                };
                let leg = Leg {
                    state,
                    spend,
                    input: s.input_idx,
                    reveal,
                    continues: continued.contains(&s.cov),
                    parent_spent: model::parent_of(state).is_some_and(|p| spent_covs.contains(&Hash32(p))),
                    quote,
                    value: s.value,
                };
                kob_out.extend(guarded(|| income::pinned(&leg, n_out)).flatten()?);
            }
            // The sender trades for itself in two cases. Its own tokens go in, as a covenant input
            // that is no order's and no custody or stray of one. Or tokens come out that no order
            // owns and no covenant pins, as the inventory of the sender or the rest of a route.
            let own = foreign_cov_in || rec.outputs.iter().enumerate().any(|(j, o)| o.cov.is_some() && !kob_out.contains(&j));
            Some((income::income(kob_in, &rec.outputs, &kob_out)?, own))
        })();
        let (income, own_trade) = match income {
            Some((v, own)) => (Some(v), own),
            None => (None, false),
        };
        // The input values of the node when the record has them, else what the inputs held
        // according to the outputs this store recorded, see `super::outputs`. The latter is known
        // only when every input resolves.
        let resolved = resolved_in.and_then(|v| u64::try_from(v).ok());
        if let (Some(node), Some(ours)) = (rec.in_value, resolved) {
            if node != ours {
                tracing::warn!(txid = %rec.txid.to_hex(), node, ours, "the inputs' values the node reported differ from the recorded outputs'");
                debug_assert_eq!(node, ours, "input values of {}", rec.txid.to_hex());
            }
        }
        let fee = income::miner_fee(rec.in_value.or(resolved), &rec.outputs);
        conn.prepare_cached(
            "INSERT INTO executor_txs (block_seq, daa, ts, txid, income, miner_fee, own_trade) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        )?
        .execute(params![
            ctx.seq,
            ctx.header.daa_score as i64,
            ctx.header.timestamp as i64,
            &rec.txid.0[..],
            income,
            fee,
            own_trade
        ])?;
        Ok(())
    }
}

/// The token identities that an order placement names, as recorded in `token_events`.
///
/// Each identity is a token covenant id, a program and an extension commitment. A plain order names
/// its token with `ext`. A pair order names both its tokens. Each has the commitment that the state
/// carries for a KCC-20 token, and none (NULL) when the family code of the state is not KCC-20. An
/// unknown commitment is never recorded as the all-zero one, which is a real commitment, the
/// standard fixed-supply class, that the token list reports.
pub fn placement_identities(
    state: &AnyState,
    token: Hash32,
    tpl_hash: Option<Hash32>,
    ext: Option<[u8; 32]>,
) -> Vec<(Hash32, Option<Hash32>, Option<[u8; 32]>)> {
    match pair_listing(state, 0, 0) {
        Some((ia, ib)) => [ia, ib]
            .into_iter()
            .filter_map(|i| Some((i.token_cov_id?, i.token_tpl_hash, i.extension_commitment.map(|e| e.0))))
            .collect(),
        None => vec![(token, tpl_hash, ext)],
    }
}

/// The listing inputs of the two tokens of a pair order, or `None` for the KAS kinds.
///
/// A comes with the scale, minimum fill, amount and expiry of the order, and B with its own scale.
/// Neither carries a KAS price, because a pair order is not measured in KAS. The extension
/// commitment of each KCC-20 token is the one the state names: `sExt` of the custody of S, `tExt`
/// of the deliveries of T, and `aExt` and `bExt` of an entry.
fn pair_listing(state: &AnyState, genesis_daa: u64, carrier: u64) -> Option<(ListingInput, ListingInput)> {
    let t = state.pair_tokens()?;
    let terms = model::terms_of(state);
    let (ea, eb) = match state {
        AnyState::KobPair(p) if p.is_ask() => (Some(p.s_ext), Some(p.t_ext)),
        AnyState::KobPair(p) => (Some(p.t_ext), Some(p.s_ext)),
        AnyState::KobCondPair(p) if p.is_ask() => (Some(p.s_ext), Some(p.t_ext)),
        AnyState::KobCondPair(p) => (Some(p.t_ext), Some(p.s_ext)),
        AnyState::KobIfdPair(p) => (Some(p.a_ext), Some(p.b_ext)),
        _ => return None,
    };
    let input = |k: &kob_protocol::state::PairToken, e: Option<[u8; 32]>, base: bool| {
        let fam = k.family_of();
        ListingInput {
            token_cov_id: Some(Hash32(k.cov_id)),
            token_tpl_hash: Some(Hash32(k.tpl_hash)),
            extension_commitment: if fam == Some(Family::Kcc20) { e.map(Hash32) } else { None },
            family: fam,
            scale: Some(k.scale).filter(|s| *s > 0),
            standard_scale: None,
            min_fill: base.then_some(terms.min_fill),
            amount: if base { state.amount_left() } else { None },
            price: None,
            tip: None,
            expiry_daa: base.then_some(terms.expiry_daa),
            genesis_daa,
            carrier,
            keeper_tip: if base { listing_keeper_tip(state, carrier) } else { None },
            proven_tpls: vec![],
        }
    };
    Some((input(&t.a, ea, true), input(&t.b, eb, false)))
}

/// The programs the chain proves for a token, as hashes ([`super::reads::proven_programs`]).
fn proven_hashes(conn: &Connection, token: Option<Hash32>) -> DbResult<Vec<Hash32>> {
    let Some(t) = token else { return Ok(vec![]) };
    Ok(super::reads::proven_programs(conn, &t.0)?.into_iter().filter_map(|h| <[u8; 32]>::try_from(h).ok().map(Hash32)).collect())
}

/// What a keeper's update may take from the order's output of `carrier` sompi.
///
/// This applies to an order that a keeper can still arm: an unarmed stop or a stop entry, see
/// `matcher::candidate::updatable`. It is `None` for every other order, which is never updated. A
/// buy stop pays its keeper only from what its buy does not need, see [`CondBid::keeper_take`],
/// measured at its current stop. Every other order pays its `keeperTip`.
fn listing_keeper_tip(state: &AnyState, carrier: u64) -> Option<i64> {
    match state {
        AnyState::KobCondAsk(s) | AnyState::KobCondAskKron(s) if s.armed == 0 && s.stop_price > 0 => Some(s.keeper_tip),
        AnyState::KobCondBid(s) | AnyState::KobCondBidKron(s) if s.armed == 0 && s.stop_price > 0 => {
            Some(s.keeper_take(carrier.min(i64::MAX as u64) as i64, s.stop_price).unwrap_or(i64::MAX))
        }
        AnyState::KobIfdBid(s) | AnyState::KobIfdBidKron(s) if s.armed == 0 && s.entry_stop > 0 => Some(s.keeper_tip),
        AnyState::KobIfdAsk(s) | AnyState::KobIfdAskKron(s) if s.armed == 0 && s.entry_stop > 0 => Some(s.keeper_tip),
        AnyState::KobCondPair(s) if s.armed == 0 && s.stop_price > 0 => Some(s.keeper_tip),
        AnyState::KobIfdPair(s) if s.armed == 0 && s.entry_stop > 0 => Some(s.keeper_tip),
        _ => None,
    }
}

/// The step and the candidate multiples of a trailing update.
///
/// The multiples are the ones that the trigger evidence in the transaction justifies, which are
/// the quotes of the plain resting orders it fills. A small range is added as a safety net.
fn trail_candidates(before: &AnyState, evidence_prices: &[i64]) -> (i64, Vec<i64>) {
    let (step, steps): (i64, Box<dyn Fn(i64) -> i64>) = match before {
        AnyState::KobCondAsk(c) | AnyState::KobCondAskKron(c) => {
            let c = c.clone();
            (c.trail_step, Box::new(move |rp| guarded(|| c.trail_steps(rp)).unwrap_or(0)))
        }
        AnyState::KobCondBid(c) | AnyState::KobCondBidKron(c) => {
            let c = c.clone();
            (c.trail_step, Box::new(move |rp| guarded(|| c.trail_steps(rp)).unwrap_or(0)))
        }
        _ => return (0, vec![]),
    };
    if step <= 0 {
        return (0, vec![]);
    }
    let mut ks: Vec<i64> = evidence_prices.iter().map(|p| steps(*p)).filter(|k| *k > 0).collect();
    ks.extend(1..=64);
    (step, ks)
}

fn exit_candidates_bid(b: &IfdBidState, n: i64, entry: [u8; 32], utxo_daa: i64, fam: Family) -> Vec<AnyState> {
    let mut v = vec![];
    if let Some(Ok(x)) = guarded(|| b.exit_for(n, None)) {
        v.push(AnyState::KobCondAsk(x).into_family(fam));
    }
    if b.rpt_amount > n {
        let Some(until) = rpt_until(b.expiry_daa, utxo_daa) else { return v };
        let booking = Booking { parent: entry, until };
        if let Some(Ok(x)) = guarded(|| b.exit_for(n, Some(booking))) {
            v.push(AnyState::KobCondAsk(x).into_family(fam));
        }
    }
    v
}

fn exit_candidates_ask(a: &IfdAskState, n: i64, entry: [u8; 32], utxo_daa: i64, fam: Family) -> Vec<AnyState> {
    let mut v = vec![];
    if let Some(Ok(x)) = guarded(|| a.exit_for(n, None)) {
        v.push(AnyState::KobCondBid(x).into_family(fam));
    }
    if a.rpt_amount > n {
        let Some(until) = rpt_until(a.expiry_daa, utxo_daa) else { return v };
        let booking = Booking { parent: entry, until };
        if let Some(Ok(x)) = guarded(|| a.exit_for(n, Some(booking))) {
            v.push(AnyState::KobCondBid(x).into_family(fam));
        }
    }
    v
}

/// The exits that a `KobIfdPair` fill of n can create, see `IfdPairState::exit_for`.
///
/// The custody of an exit is n of A when buy-first. When sell-first it is the proceeds `amt`
/// (argument 6) plus the prefund of the fill, which is `pre(n)` or the whole prefund left when the
/// entry ends. The exit is booked with `rptUntil = rpt_until(expiryDaa, UTXO DAA)` when the entry
/// repeats and `rptAmount > n`.
fn exit_candidates_pair(e: &IfdPairState, n: i64, entry: [u8; 32], utxo_daa: i64, amt: Option<i64>) -> Vec<AnyState> {
    let custodies: Vec<i64> = if e.is_buy_first() {
        vec![n]
    } else {
        let Some(amt) = amt else { return vec![] };
        [guarded(|| e.pre_of(n)).flatten(), Some(e.custody)].into_iter().flatten().filter_map(|p| amt.checked_add(p)).collect()
    };
    let mut bookings = vec![None];
    if e.rpt_amount > n {
        if let Some(until) = rpt_until(e.expiry_daa, utxo_daa) {
            bookings.push(Some(Booking { parent: entry, until }));
        }
    }
    let mut v = vec![];
    for c in custodies {
        for b in &bookings {
            if let Some(Ok(x)) = guarded(|| e.exit_for(n, c, *b)) {
                v.push(AnyState::KobCondPair(x));
            }
        }
    }
    v
}

/// A `TxJson` view of a record, enough for `recover_orders`.
///
/// It holds the outpoints, the covenant ids and the outputs.
pub fn tx_json(rec: &TxRecord, payload: Vec<u8>) -> TxJson {
    TxJson {
        id: rec.txid.0,
        version: 1,
        inputs: rec
            .inputs
            .iter()
            .map(|i| TxInputJson {
                transaction_id: i.txid.0,
                index: i.index,
                sequence: 0,
                sig_op_count: 0,
                compute_budget: 0,
                signature_script: vec![],
                utxo: UtxoJson {
                    address: None,
                    amount: 0,
                    script_public_key: String::new(),
                    block_daa_score: i.daa,
                    is_coinbase: false,
                    covenant_id: i.cov.map(|c| c.0),
                },
            })
            .collect(),
        outputs: rec
            .outputs
            .iter()
            .map(|o| TxOutputJson {
                value: o.value,
                script_public_key: if o.cov.is_some() { crate::hex::encode(&o.spk) } else { String::new() },
                covenant: o.cov.map(|(a, c)| CovenantJson { authorizing_input: a as u16, covenant_id: c.0 }),
            })
            .collect(),
        subnetwork_id: vec![],
        lock_time: 0,
        gas: 0,
        storage_mass: 0,
        payload,
    }
}

/// The KAS a fill moved for its order: the bound of the tip it records ([`super::activity::fill_tip`]).
///
/// It is the larger of what the order UTXO released, its value less its continuation's, and what
/// the fill paid the maker of a sell order, `payout`, the output at the order's input index. A buy
/// order and a pair order pay the tip out of their own KAS. A sell order's tip is a discount on
/// what it is paid.
fn fill_moved(rec: &TxRecord, s: &Spent, payout: Option<i64>) -> i64 {
    let cont = rec.outputs.iter().find(|o| o.cov.is_some_and(|(_, c)| c == s.cov)).map_or(0, |o| i128::from(o.value));
    let released = (i128::from(s.value) - cont).max(0);
    released.max(i128::from(payout.unwrap_or(0))).min(i64::MAX as i128) as i64
}

/// The `(txid, idx, value, state)` of the current UTXO of an order.
type TipRow = (Vec<u8>, i64, i64, Option<Vec<u8>>);

/// The unspent UTXO of an order covenant (`?1`) that is its tip.
///
/// Its state is decoded against the genesis state of the order, see [`super::packed`].
const TIP_UTXO_SQL: &str = concat!(
    "SELECT txid, idx, value, kob_ustate(state, (SELECT ",
    sql_genesis!("g"),
    " FROM orders g",
    sql_order_ids!("g"),
    " WHERE g.id = order_utxos.order_id)) \
     FROM order_utxos WHERE order_id = ",
    order_of!("?1"),
    " AND spent_block IS NULL ORDER BY created_block DESC, idx DESC LIMIT 1"
);

/// The fill amounts of one order.
///
/// They are summed in Rust, saturating. The base-unit amounts of hostile orders can sum past an
/// `i64`, where the SQLite `SUM` fails, and a failed refresh would stall the follower.
///
/// `+kind` keeps the query on the events of the order itself, through `order_events_cov`. On
/// `kind = 'fill'` alone the planner picks the market index, `order_events_fill_ts`, and reads
/// every fill of every token.
pub(crate) const FILLED_AMOUNT_SQL: &str =
    concat!("SELECT amount FROM order_events WHERE order_id = ", order_of!("?1"), " AND +kind = 'fill'");

/// The filled amount the `order_state` row of an order holds.
const STORED_FILLED_SQL: &str = concat!("SELECT filled_amount FROM order_state WHERE order_id = ", order_of!("?1"));

/// The newest event of an order, one seek of `order_events_cov`.
const LAST_EVENT_SQL: &str =
    concat!("SELECT block_seq, daa FROM order_events WHERE order_id = ", order_of!("?1"), " ORDER BY id DESC LIMIT 1");

/// The newest closing event of an order, one seek of `order_events_closes`.
const CLOSING_EVENT_SQL: &str =
    concat!("SELECT kind, amount FROM order_events WHERE order_id = ", order_of!("?1"), " AND closes = 1 ORDER BY id DESC LIMIT 1");

/// Base units filled from one order: the sum of its fill events, saturating at `i64::MAX`.
fn filled_amount(conn: &Connection, cov: &Hash32) -> DbResult<i64> {
    let mut st = conn.prepare_cached(FILLED_AMOUNT_SQL)?;
    let mut rows = st.query([&cov.0[..]])?;
    let mut sum: i128 = 0;
    while let Some(r) = rows.next()? {
        sum += r.get::<_, Option<i64>>(0)?.unwrap_or(0).max(0) as i128;
    }
    Ok(sum.min(i64::MAX as i128) as i64)
}

/// How a refresh gets the filled amount of the order.
#[derive(Debug, Clone, Copy)]
enum Filled {
    /// The sum of the order's fill events. It costs one read per fill of the order.
    Recount,
    /// The stored amount plus `n` base units: the fill events a spend has just added.
    Add(i64),
}

/// Re-derives the `order_state` row of one order from the base tables.
///
/// It is called after every revert and every gap operation, so it is the single definition of
/// "status". The filled amount is summed from the fill events of the order, once. A spend adds its
/// fill to the stored amount instead, see [`refresh_order_state_adding`].
pub fn refresh_order_state(conn: &Connection, cov: &Hash32) -> DbResult<()> {
    refresh_with(conn, cov, Filled::Recount)
}

/// [`refresh_order_state`] after a spend whose events added `n` filled base units (0 for none).
///
/// The filled amount is a running total: the stored one plus `n`, saturating at `i64::MAX`. That
/// is the saturating sum of the fill events, as a recount gives, because every write of the events
/// refreshes the row in the same transaction: a spend adds, and a revert recounts. The pruning
/// deletes the events of an order only with its `order_state` row. Without a stored row it recounts.
///
/// A fill thus costs the same however many fills the order had. A recount on every fill cost one
/// read per earlier fill, so a maker filling its own big ask one unit at a time made the follower's
/// work quadratic in the fills.
pub fn refresh_order_state_adding(conn: &Connection, cov: &Hash32, n: i64) -> DbResult<()> {
    refresh_with(conn, cov, Filled::Add(n))
}

fn refresh_with(conn: &Connection, cov: &Hash32, how: Filled) -> DbResult<()> {
    let Some(order) = load_order_full(conn, cov)? else {
        conn.execute(concat!("DELETE FROM order_state WHERE order_id = ", order_of!("?1")), [&cov.0[..]])?;
        return Ok(());
    };
    let stored = match how {
        Filled::Recount => None,
        Filled::Add(n) => conn
            .prepare_cached(STORED_FILLED_SQL)?
            .query_row([&cov.0[..]], |r| r.get::<_, i64>(0))
            .optional()?
            .map(|f| f.saturating_add(n.max(0))),
    };
    let filled = match stored {
        Some(f) => f,
        None => filled_amount(conn, cov)?,
    };
    let tip: Option<TipRow> =
        conn.prepare_cached(TIP_UTXO_SQL)?.query_row([&cov.0[..]], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))).optional()?;
    let last: Option<(i64, i64)> =
        conn.prepare_cached(LAST_EVENT_SQL)?.query_row([&cov.0[..]], |r| Ok((r.get(0)?, r.get(1)?))).optional()?;
    // Only a closed order reads its closing event: an open one has none to find.
    let closing: Option<(String, Option<i64>)> = match tip {
        Some(_) => None,
        None => conn.prepare_cached(CLOSING_EVENT_SQL)?.query_row([&cov.0[..]], |r| Ok((r.get(0)?, r.get(1)?))).optional()?,
    };
    let status = match (&tip, closing.as_ref().map(|c| c.0.as_str())) {
        (Some(_), _) if filled > 0 => "partial",
        (Some(_), _) => "open",
        (None, Some("fill")) => "filled",
        (None, Some("cancel")) => "cancelled",
        (None, Some("refund")) => "refunded",
        (None, Some("kill")) => "killed",
        (None, _) => "closed",
    };
    // The remaining amount is exact from the tip state where the kind has `amountLeft`. For a bid
    // it is its buying power.
    let (remaining, exact): (Option<i64>, bool) = match &tip {
        None => (Some(closing.as_ref().filter(|c| c.0 == "kill").and_then(|c| c.1).unwrap_or(0)), true),
        Some((_, _, value, state)) => {
            let known = state.as_ref().and_then(|s| AnyState::decode(order.template, s).ok());
            match (known.as_ref().and_then(|s| s.amount_left()), &known, order.template.base()) {
                (Some(l), _, _) => (Some(l), true),
                // The quantity of a bid is its escrow: the most one fill can buy. That is the
                // largest n whose budget ceil(n * (pMax + tip) / scale) leaves the delivery carrier
                // and the reserve. It is an upper bound, because every further fill moves one more
                // carrier.
                (None, Some(AnyState::KobBid(b) | AnyState::KobBidKron(b)), _) => (Some(b.buying_power(*value)), false),
                // An unproven bid state gets the same bound from the order row. No delivery carrier
                // is known.
                (None, _, TemplateId::KobBid) => match (order.budget_rate, order.reserve, order.scale) {
                    (Some(rate), Some(rs), Some(scale)) if rate > 0 && scale > 0 => {
                        let budget = (*value as i128 - rs as i128).max(0);
                        (Some((budget * scale as i128 / rate as i128).min(i64::MAX as i128) as i64), false)
                    }
                    _ => (None, false),
                },
                _ => (order.initial_amount.map(|i| i.saturating_sub(filled).max(0)), false),
            }
        }
    };
    let (last_block, last_daa) = last.unwrap_or((0, 0));
    conn.prepare_cached(concat!(
        "INSERT OR REPLACE INTO order_state (order_id, status, filled_amount, remaining_amount, amount_exact, cur_txid, cur_idx, cur_value, state_known, last_block, last_daa) VALUES (",
        order_of!("?1"),
        ",?2,?3,?4,?5,?6,?7,?8,?9,?10,?11)"
    ))?
    .execute(params![
        &cov.0[..],
        status,
        filled,
        remaining,
        exact as i64,
        tip.as_ref().map(|t| t.0.clone()),
        tip.as_ref().map(|t| t.1),
        tip.as_ref().map(|t| t.2),
        tip.as_ref().map(|t| t.3.is_some()).unwrap_or(false) as i64,
        last_block,
        last_daa
    ])?;
    // An if-done entry with a stop is a stop order until it is armed. Nothing fills it at its limit
    // before trigger evidence in the same transaction, see `docs/spec/matcher.md` §4. So it is in
    // no KAS book, depth or count, where its limit would read as resting liquidity. The limit of a
    // sell-stop entry sits below the market and would look like a crossed ask. Once armed, it
    // auctions in its book. The flag follows the tip state both ways, reorgs included.
    if matches!(order.template.base(), TemplateId::KobIfdAsk | TemplateId::KobIfdBid) {
        if let Some(known) = tip.as_ref().and_then(|t| t.3.as_ref()).and_then(|s| AnyState::decode(order.template, s).ok()) {
            let unarmed_stop = match &known {
                AnyState::KobIfdAsk(a) | AnyState::KobIfdAskKron(a) => a.entry_stop > 0 && a.armed == 0,
                AnyState::KobIfdBid(b) | AnyState::KobIfdBidKron(b) => b.entry_stop > 0 && b.armed == 0,
                _ => false,
            };
            conn.prepare_cached("UPDATE orders SET in_book = ?1 WHERE covenant_id = ?2 AND in_book != ?1")?
                .execute(params![(!unarmed_stop) as i64, &cov.0[..]])?;
        }
    }
    Ok(())
}

struct OrderFull {
    template: TemplateId,
    initial_amount: Option<i64>,
    budget_rate: Option<i64>,
    reserve: Option<i64>,
    scale: Option<i64>,
}

fn load_order_full(conn: &Connection, cov: &Hash32) -> DbResult<Option<OrderFull>> {
    let r = conn
        .prepare_cached("SELECT contract, initial_amount, budget_rate, reserve, scale FROM orders WHERE covenant_id = ?1")?
        .query_row([&cov.0[..]], |r| Ok((r.get::<_, String>(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)))
        .optional()?;
    Ok(r.and_then(|(name, initial_amount, budget_rate, reserve, scale)| {
        Some(OrderFull { template: TemplateId::from_name(&name)?, initial_amount, budget_rate, reserve, scale })
    }))
}

/// Undoes everything one chain block did. The block must be the newest stored one.
///
/// It returns whether the block had changed any KOB row. A reorg that reverts only unrelated
/// blocks is not worth a record-log frame.
pub fn revert_block(conn: &Connection, seq: i64, delta: &mut Delta) -> DbResult<bool> {
    let mut changed = 0usize;
    let mut affected: BTreeSet<Hash32> = BTreeSet::new();
    let mut collect = |sql: &str| -> DbResult<()> {
        let mut st = conn.prepare_cached(sql)?;
        let rows = st.query_map([seq], |r| hash_at(r, 0))?;
        for r in rows {
            affected.insert(r?);
        }
        Ok(())
    };
    collect("SELECT DISTINCT o.covenant_id FROM order_events e JOIN orders o ON o.id = e.order_id WHERE e.block_seq = ?1")?;
    collect(
        "SELECT o.covenant_id FROM order_utxos u JOIN orders o ON o.id = u.order_id WHERE u.created_block = ?1 OR u.spent_block = ?1",
    )?;
    collect("SELECT covenant_id FROM orders WHERE genesis_block = ?1")?;
    collect(
        "SELECT o.covenant_id FROM token_utxos t JOIN orders o ON o.id = t.order_id WHERE t.created_block = ?1 OR t.spent_block = ?1",
    )?;
    // A holding that a reverted block created or spent belongs to a token whose holders' view
    // changes. Owners are not always orders.
    let mut holding_tokens: BTreeSet<Hash32> = BTreeSet::new();
    {
        let mut st =
            conn.prepare_cached("SELECT DISTINCT i.h FROM token_holdings t JOIN ids i ON i.id = t.token_id WHERE t.created_block = ?1 OR t.spent_block = ?1")?;
        for r in st.query_map([seq], |r| hash_at(r, 0))? {
            holding_tokens.insert(r?);
        }
    }
    // The tokens whose books change.
    let mut tokens: BTreeSet<Hash32> = BTreeSet::new();
    for cov in &affected {
        if let Some(o) = load_order(conn, cov)? {
            if let Some(t) = o.token {
                tokens.insert(t);
            }
            tokens.extend(o.quote);
        }
    }
    // In-place amends of the block: the orders take back the terms they replaced. One statement
    // handles all of them, because a block can hold hundreds of thousands. They come from the
    // unlisting of the orders of a token when a program is first proved, see
    // `Processor::unlist_conflicting`. A row at a time is far too slow.
    //
    // An order amended twice in the block takes the terms of its OLDEST row, which is what it
    // applied newest first. Its min_fill is the oldest one that was recorded.
    changed += conn.execute(
        "WITH first AS (SELECT MIN(id) AS id FROM order_amends WHERE block_seq = ?1 GROUP BY order_id)          UPDATE orders SET price = a.price, tip = a.tip, tif = a.tif, expiry_daa = a.expiry_daa, active_from = a.active_from,                 deadline = a.deadline, listed = a.listed, unlisted_reason = a.unlisted_reason, budget_rate = a.budget_rate,                 reserve = a.reserve          FROM order_amends a WHERE a.id IN (SELECT id FROM first) AND orders.id = a.order_id",
        [seq],
    )?;
    changed += conn.execute(
        "WITH first AS (SELECT MIN(id) AS id FROM order_amends WHERE block_seq = ?1 AND min_fill IS NOT NULL GROUP BY order_id)          UPDATE orders SET min_fill = a.min_fill FROM order_amends a WHERE a.id IN (SELECT id FROM first) AND orders.id = a.order_id",
        [seq],
    )?;
    changed += conn.execute("DELETE FROM order_amends WHERE block_seq = ?1", [seq])?;
    // The candle buckets of the fills about to go. `candles::refresh` recomputes them before the
    // transaction commits.
    super::candles::mark_block(conn, seq)?;
    changed += conn.execute("DELETE FROM order_events WHERE block_seq = ?1", [seq])?;
    changed += conn.execute("DELETE FROM pair_fills WHERE block_seq = ?1", [seq])?;
    changed += conn.execute("DELETE FROM executor_txs WHERE block_seq = ?1", [seq])?;
    changed += super::outputs::revert(conn, seq)?;
    changed += conn.execute("DELETE FROM order_utxos WHERE created_block = ?1", [seq])?;
    changed += conn.execute(
        "UPDATE order_utxos SET spent_block = NULL, spent_txid = NULL, spent_entry = NULL, spent_amount = NULL WHERE spent_block = ?1",
        [seq],
    )?;
    changed += conn.execute("DELETE FROM token_utxos WHERE created_block = ?1", [seq])?;
    changed += conn.execute("UPDATE token_utxos SET spent_block = NULL, spent_txid = NULL WHERE spent_block = ?1", [seq])?;
    changed += conn.execute("DELETE FROM token_holdings WHERE created_block = ?1", [seq])?;
    changed += conn.execute("UPDATE token_holdings SET spent_block = NULL, spent_txid = NULL WHERE spent_block = ?1", [seq])?;
    changed += conn.execute("DELETE FROM token_events WHERE block_seq = ?1", [seq])?;
    changed += conn.execute("DELETE FROM rejects WHERE block_seq = ?1", [seq])?;
    // The state rows of the orders this block created go with them. A row left would name an order
    // that is gone.
    changed += conn.execute("DELETE FROM order_state WHERE order_id IN (SELECT id FROM orders WHERE genesis_block = ?1)", [seq])?;
    changed += conn.execute("DELETE FROM orders WHERE genesis_block = ?1", [seq])?;
    changed += super::kcc23::revert(conn, seq)?;
    conn.execute("DELETE FROM blocks WHERE seq = ?1", [seq])?;
    for cov in &affected {
        refresh_order_state(conn, cov)?;
        delta.orders.insert(*cov);
    }
    delta.tokens.extend(tokens);
    delta.tokens.extend(holding_tokens);
    Ok(changed > 0)
}

impl From<crate::hex::HexError> for DbError {
    fn from(e: crate::hex::HexError) -> Self {
        DbError::Invalid(e.to_string())
    }
}

/// Outcome counters of operator operations.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct OpReport {
    /// Orders imported, or revived at a node-verified continuation.
    pub imported: u64,
    /// Imports that named an order this store already has.
    pub already_known: u64,
    /// Orders closed by a gap reconciliation.
    pub closed: u64,
    /// Orders that continued under a new script and were adopted.
    pub adopted: u64,
    /// One message per rejected operation.
    pub rejected: Vec<String>,
}

impl Processor {
    /// Applies one operator operation: a recovery import, a gap close or a gap adoption.
    ///
    /// All of them stamp block 0, which is never reverted.
    pub fn apply_op(&self, conn: &Connection, op: &LogOp, cursor_daa: u64, delta: &mut Delta, rep: &mut OpReport) -> DbResult<()> {
        match op {
            LogOp::Import { template: tid, state, covenant_id, txid, index, value, spk, daa, custody, parent, prefund } => {
                let label = covenant_id.to_hex();
                let Ok(st) = AnyState::decode(*tid, state) else {
                    rep.rejected.push(format!("{label}: state does not decode"));
                    return Ok(());
                };
                if !model::is_order(st.template_id()) {
                    rep.rejected.push(format!("{label}: not an order template"));
                    return Ok(());
                }
                if spk_bytes(&st.spk()) != *spk {
                    rep.rejected.push(format!("{label}: state does not match the script"));
                    return Ok(());
                }
                // The token of each custody. It is the token of the order for a KAS kind, or the
                // token of that part of `AnyState::custodies`.
                let part_tokens = custody_part_tokens(&st);
                let to_new = |c: &super::recordlog::ImportCustody, k: usize| NewCustody {
                    txid: c.txid,
                    index: c.index,
                    value: c.value,
                    amount: c.amount,
                    token: if st.is_pair() { part_tokens.get(k).map(|t| t.0) } else { None },
                };
                let cust = custody.as_ref().map(|c| to_new(c, 0));
                let pre = prefund.as_ref().map(|c| to_new(c, 1));
                let ext = custody.as_ref().map(|c| c.ext.0);
                if order_exists(conn, covenant_id)? {
                    // A known order with no live output was closed by a gap reconciliation. It
                    // continued under a new script that the indexer never saw, for example after a
                    // partial fill. It is revived from the node-verified continuation: the node
                    // holds an output of this covenant id at this script, which only the lineage of
                    // the order itself can produce.
                    match self.revive(conn, delta, covenant_id, &st, *txid, *index, *value, spk, *daa, cust.as_ref(), ext)? {
                        true => rep.imported += 1,
                        false => rep.already_known += 1,
                    }
                    return Ok(());
                }
                // An exit names its entry. Only an entry this indexer knows counts, and otherwise
                // the exit is imported as an order of its own.
                let parent = match parent {
                    Some(p) if order_exists(conn, p)? => Some(*p),
                    _ => None,
                };
                match self.create_order(
                    conn,
                    delta,
                    NewOrder {
                        state: st,
                        covenant_id: *covenant_id,
                        txid: *txid,
                        out_index: *index,
                        out_value: *value,
                        out_spk: spk.clone(),
                        block_seq: 0,
                        daa: *daa,
                        ts: 0,
                        tx_pos: 0,
                        parent,
                        deadline: None,
                        custody: cust,
                        prefund: pre,
                        ext_commit: ext,
                        origin: "import",
                        inherit_listed: None,
                    },
                )? {
                    CreateOutcome::Created => rep.imported += 1,
                    CreateOutcome::Rejected(r) if r == "duplicate_covenant" => rep.already_known += 1,
                    CreateOutcome::Rejected(r) => rep.rejected.push(format!("{label}: {r}")),
                }
            }
            LogOp::CloseGap { txid, index, covenant_id } => {
                let n = conn
                    .prepare_cached("UPDATE order_utxos SET spent_block = 0, spent_entry = 'gap' WHERE txid = ?1 AND idx = ?2 AND spent_block IS NULL")?
                    .execute(params![&txid.0[..], index])?;
                if n > 0 {
                    conn.prepare_cached(concat!(
                        "UPDATE token_utxos SET spent_block = 0 WHERE order_id = ",
                        order_of!("?1"),
                        " AND spent_block IS NULL"
                    ))?
                    .execute([&covenant_id.0[..]])?;
                    conn.prepare_cached(concat!(
                        "UPDATE token_holdings SET spent_block = 0 WHERE owner_id = ",
                        id_of!("?1"),
                        " AND role IN ('custody','stray') AND spent_block IS NULL"
                    ))?
                    .execute([&covenant_id.0[..]])?;
                    self.gap_event(conn, covenant_id, txid, cursor_daa, "unknown", true, json!({"reason": "gap_close"}))?;
                    refresh_order_state(conn, covenant_id)?;
                    delta.orders.insert(*covenant_id);
                    if let Some(t) = load_order(conn, covenant_id)?.and_then(|o| o.token) {
                        delta.tokens.insert(t);
                    }
                    rep.closed += 1;
                }
            }
            LogOp::Adopt { covenant_id, old_txid, old_index, txid, index, value, spk, daa, custody, keep } => {
                let state: Option<Vec<u8>> = conn
                    .prepare_cached(concat!(
                        "SELECT kob_ustate(u.state, (SELECT ",
                        sql_genesis!("g"),
                        " FROM orders g",
                        sql_order_ids!("g"),
                        " WHERE g.id = u.order_id)) \
                             FROM order_utxos u WHERE u.txid = ?1 AND u.idx = ?2 AND u.spent_block IS NULL"
                    ))?
                    .query_row(params![&old_txid.0[..], old_index], |r| r.get(0))
                    .optional()?
                    .flatten();
                let n = conn
                    .prepare_cached("UPDATE order_utxos SET spent_block = 0, spent_entry = 'gap' WHERE txid = ?1 AND idx = ?2 AND spent_block IS NULL")?
                    .execute(params![&old_txid.0[..], old_index])?;
                if n > 0 {
                    // The token UTXOs owned by the order that the node no longer has are closed.
                    // The ones it still holds stay, because the reconciliation checked each at the
                    // script its proven state produces.
                    let kept: HashSet<(Hash32, u32)> = keep.iter().copied().collect();
                    let owned: Vec<(Hash32, u32)> = {
                        let mut st = conn.prepare_cached(concat!(
                            "SELECT txid, idx FROM token_utxos WHERE order_id = ",
                            order_of!("?1"),
                            " AND spent_block IS NULL"
                        ))?;
                        let rows = st.query_map([&covenant_id.0[..]], |r| Ok((hash_at(r, 0)?, r.get::<_, u32>(1)?)))?;
                        rows.collect::<rusqlite::Result<_>>()?
                    };
                    for (t, i) in owned.into_iter().filter(|o| !kept.contains(o)) {
                        conn.prepare_cached(
                            "UPDATE token_utxos SET spent_block = 0 WHERE txid = ?1 AND idx = ?2 AND spent_block IS NULL",
                        )?
                        .execute(params![&t.0[..], i])?;
                        conn.prepare_cached(
                            "UPDATE token_holdings SET spent_block = 0 WHERE txid = ?1 AND idx = ?2 AND role IN ('custody','stray') AND spent_block IS NULL",
                        )?
                        .execute(params![&t.0[..], i])?;
                    }
                    // The DAA of the output itself, which the refund and kill times count from. An
                    // operation without it falls back to the cursor's.
                    let udaa = daa.unwrap_or(cursor_daa);
                    self.insert_utxo(conn, covenant_id, txid, *index, *value, spk, state.as_deref(), 0, udaa)?;
                    let tid = load_order(conn, covenant_id)?.map(|o| o.template);
                    let st = match (tid, state.as_deref()) {
                        (Some(t), Some(s)) => AnyState::decode(t, s).ok(),
                        _ => None,
                    };
                    if let (Some(c), Some(st)) = (custody, st) {
                        // The token of the first custody. For a pair order it is the first of
                        // `AnyState::custodies`, in either token.
                        let token = custody_part_tokens(&st).first().map(|t| t.0).unwrap_or(Hash32(st.token_cov_id()));
                        let live = conn
                            .prepare_cached("SELECT 1 FROM token_utxos WHERE txid = ?1 AND idx = ?2 AND spent_block IS NULL")?
                            .exists(params![&c.txid.0[..], c.index])?;
                        if !live {
                            conn.prepare_cached(concat!(
                                "INSERT INTO token_utxos (txid, idx, token_id, order_id, amount, value, role, created_block, created_daa) VALUES (?1,?2,?3,",
                                order_of!("?4"),
                                ",?5,?6,'custody',0,?7) \
                                 ON CONFLICT(txid, idx) DO UPDATE SET spent_block = NULL, spent_txid = NULL, role = 'custody'"
                            ))?
                            .execute(params![&c.txid.0[..], c.index, intern(conn, &token.0)?, &covenant_id.0[..], c.amount, c.value as i64, udaa as i64])?;
                            let nc = NewCustody { txid: c.txid, index: c.index, value: c.value, amount: c.amount, token: Some(token) };
                            self.custody_holding(conn, covenant_id, &st, &nc, Some(c.ext.0), 0, udaa)?;
                        }
                    }
                    self.gap_event(conn, covenant_id, txid, cursor_daa, "update", false, json!({"reason": "gap_adopt"}))?;
                    refresh_order_state(conn, covenant_id)?;
                    delta.orders.insert(*covenant_id);
                    if let Some(t) = load_order(conn, covenant_id)?.and_then(|o| o.token) {
                        delta.tokens.insert(t);
                    }
                    rep.adopted += 1;
                }
            }
        }
        Ok(())
    }

    /// Revives a known order without a live output at a node-verified continuation.
    ///
    /// Every output of such an order is closed, for example by `CloseGap`. The output and its
    /// custody become live again, and an `update` event marks the import. It returns `false`, with
    /// nothing changed, when the order still has a live output or the template differs.
    #[allow(clippy::too_many_arguments)]
    fn revive(
        &self,
        conn: &Connection,
        delta: &mut Delta,
        cov: &Hash32,
        st: &AnyState,
        txid: Hash32,
        index: u32,
        value: u64,
        spk: &[u8],
        daa: u64,
        custody: Option<&NewCustody>,
        ext: Option<[u8; 32]>,
    ) -> DbResult<bool> {
        let Some(order) = load_order_full(conn, cov)? else { return Ok(false) };
        if order.template != st.template_id() {
            return Ok(false);
        }
        let live = conn
            .prepare_cached(concat!("SELECT 1 FROM order_utxos WHERE order_id = ", order_of!("?1"), " AND spent_block IS NULL"))?
            .exists([&cov.0[..]])?;
        if live {
            return Ok(false);
        }
        let state = st.encode();
        conn.prepare_cached("DELETE FROM order_utxos WHERE txid = ?1 AND idx = ?2")?.execute(params![&txid.0[..], index])?;
        self.insert_utxo(conn, cov, &txid, index, value, spk, Some(&state), 0, daa)?;
        let token = Hash32(st.token_cov_id());
        if let Some(c) = custody {
            conn.prepare_cached(concat!(
                "INSERT INTO token_utxos (txid, idx, token_id, order_id, amount, value, role, created_block, created_daa) VALUES (?1,?2,?3,",
                order_of!("?4"),
                ",?5,?6,'custody',0,?7) \
                 ON CONFLICT(txid, idx) DO UPDATE SET spent_block = NULL, spent_txid = NULL"
            ))?
            .execute(params![&c.txid.0[..], c.index, intern(conn, &c.token.unwrap_or(token).0)?, &cov.0[..], c.amount, c.value as i64, daa as i64])?;
            let ext = match ext {
                Some(e) => Some(e),
                None => order_ext_commit(conn, cov)?,
            };
            self.custody_holding(conn, cov, st, c, ext, 0, daa)?;
        }
        self.gap_event(conn, cov, &txid, daa, "update", false, json!({"reason": "import_revive"}))?;
        refresh_order_state(conn, cov)?;
        delta.orders.insert(*cov);
        delta.tokens.insert(token);
        Ok(true)
    }

    fn gap_event(
        &self,
        conn: &Connection,
        cov: &Hash32,
        txid: &Hash32,
        daa: u64,
        kind: &str,
        closes: bool,
        detail: serde_json::Value,
    ) -> DbResult<()> {
        let (token, side) = load_order(conn, cov)?.map(|o| (o.token, o.side)).unwrap_or((None, 0));
        conn.prepare_cached(concat!(
            "INSERT INTO order_events (order_id, block_seq, daa, ts, txid, tx_pos, kind, token_id, side, closes, detail) VALUES (",
            order_of!("?1"),
            ", 0, ?2, 0, ?3, 0, ?4, ?5, ?6, ?7, kob_dpack(?8, ?4))"
        ))?
        .execute(params![
            &cov.0[..],
            daa as i64,
            &txid.0[..],
            kind,
            token.map(|t| intern(conn, &t.0)).transpose()?,
            side as i64,
            closes as i64,
            detail.to_string()
        ])?;
        Ok(())
    }
}

// ---------------------------------------------------------------------------------------------
// Trigger evidence.

/// The side and quote of a plain resting `KobAsk` or `KobBid` whose revealed spend is a fill.
///
/// Both families count, and the fill has `n > 0`. That input offers the side and quote as trigger
/// evidence to the conditional orders and stop entries of the same transaction. The side is
/// [`kob_protocol::state::SIDE_ASK`] when a resting ask was filled, and
/// [`kob_protocol::state::SIDE_BID`] for a resting bid. The other rules of the covenants, which
/// are exposure, `minTouch` base units of an order of the same scale, and no decay, are theirs to
/// enforce. The indexer only reads which input the order named.
fn evidence_touch(r: &Reveal) -> Option<(i64, i64)> {
    if !matches!(r.nb(), Some(Nb::Fill(n)) if n > 0) {
        return None;
    }
    match AnyState::decode(r.template, &r.state).ok()? {
        AnyState::KobAsk(s) | AnyState::KobAskKron(s) => Some((kob_protocol::state::SIDE_ASK, s.price)),
        AnyState::KobBid(s) | AnyState::KobBidKron(s) => Some((kob_protocol::state::SIDE_BID, s.price)),
        _ => None,
    }
}

/// The input index of the trigger evidence that the spend of an order names.
///
/// It is the `ev` argument of an `update`, which is an arm or a trailing ratchet. It is also the
/// `ev` argument of a fill that triggers an unarmed stop leg or an unarmed stop entry:
/// - `KobCondAsk.settle(n, tk, _, 1, ev, tk, t)`
/// - `KobCondBid.settle(n, tok, 1, ev, t)`
/// - `KobIfdBid.fill(.., ev, t)`
/// - `KobIfdAsk.settle(.., ev, tk, t)`
///
/// It is `None` for every other spend: an armed stop, a take-profit leg or a limit entry.
fn evidence_input(before: &AnyState, r: &Reveal, act: Act) -> Option<usize> {
    let ev = match act {
        Act::Update => r.int(0),
        Act::Fill(_) => match before {
            AnyState::KobCondAsk(c) | AnyState::KobCondAskKron(c) if c.armed == 0 && r.int(3) == Some(1) => r.int(4),
            AnyState::KobCondBid(c) | AnyState::KobCondBidKron(c) if c.armed == 0 && r.int(2) == Some(1) => r.int(3),
            AnyState::KobIfdBid(b) | AnyState::KobIfdBidKron(b) if b.entry_stop > 0 && b.armed == 0 => r.int(5),
            AnyState::KobIfdAsk(a) | AnyState::KobIfdAskKron(a) if a.entry_stop > 0 && a.armed == 0 => r.int(6),
            _ => None,
        },
        _ => None,
    }?;
    usize::try_from(ev).ok()
}

/// The `detail.evidence` of an event.
///
/// It holds the evidence input, the covenant id of its order and, when its spend is revealed, its
/// side and quote.
fn evidence_detail(rec: &TxRecord, k: usize) -> serde_json::Value {
    let Some(inp) = rec.inputs.get(k) else { return json!({ "input": k }) };
    let mut v = json!({ "input": k, "order": inp.cov.map(|c| c.to_hex()) });
    if let Some((side, price)) = inp.reveal.as_ref().and_then(evidence_touch) {
        v["side"] = json!(side);
        v["price"] = json!(price.to_string());
    }
    v
}

// ---------------------------------------------------------------------------------------------
// Pair orders: trigger evidence, fill rows and counterparty. Pair fills have no KAS price.

/// Argument indices `(evA, evB, evMode)` of a pair conditional's `settle` (fill and update alike).
const PAIR_EV_COND: (usize, usize, usize) = (7, 8, 10);
/// Argument indices `(evA, evB, evMode)` of a pair entry's `fill` (fill and update alike).
const PAIR_EV_IFD: (usize, usize, usize) = (9, 10, 12);

/// The pair trigger evidence that a pair spend names, and its input indices.
///
/// There are two modes:
/// - Mode 0: a plain resting KAS-book order of A (`evA`) and one of B (`evB`) filled in the
///   transaction, with the quotes `a` and `b`.
/// - Mode 1: a resting `KobPair` of the pair (`evA`) filled in the transaction, with its `price`.
///
/// The covenant checks the rest: tokens, sides, scales, exposure and minTouch. The indexer reads
/// what the spend named. It returns `None` when the named inputs reveal no such fill.
fn pair_evidence_of(rec: &TxRecord, r: &Reveal, (ia, ib, im): (usize, usize, usize)) -> Option<(PairEvidence, Vec<usize>)> {
    let ka = usize::try_from(r.int(ia)?).ok()?;
    let ra = rec.inputs.get(ka)?.reveal.as_ref()?;
    match r.int(im)? {
        0 => {
            let kb = usize::try_from(r.int(ib)?).ok()?;
            let rb = rec.inputs.get(kb)?.reveal.as_ref()?;
            let (_, a) = evidence_touch(ra)?;
            let (_, b) = evidence_touch(rb)?;
            Some((PairEvidence::KasBooks { a, b }, vec![ka, kb]))
        }
        1 => {
            if !matches!(ra.nb(), Some(Nb::Fill(n)) if n > 0) {
                return None;
            }
            match AnyState::decode(ra.template, &ra.state).ok()? {
                AnyState::KobPair(p) => Some((PairEvidence::Pair { price: p.price }, vec![ka])),
                _ => None,
            }
        }
        _ => None,
    }
}

/// The `detail.evidence` of a pair order's spend that used trigger evidence.
///
/// That is an update, a fill of an unarmed stop leg or a fill of an unarmed stop entry. The value
/// is `{"mode": 0 | 1, "inputs": [..], "orders": [..], ...}`. Mode 0 adds `"a"` and `"b"`, the two
/// KAS quotes. Mode 1 adds `"price"`, the quote in B per whole A of the resting pair order.
fn pair_evidence_detail(rec: &TxRecord, before: &AnyState, r: &Reveal, act: Act) -> Option<serde_json::Value> {
    let idx = match (before, act) {
        (AnyState::KobCondPair(_), Act::Update) => PAIR_EV_COND,
        (AnyState::KobCondPair(c), Act::Fill(_)) if c.armed == 0 && r.int(6) == Some(1) => PAIR_EV_COND,
        (AnyState::KobIfdPair(_), Act::Update) => PAIR_EV_IFD,
        (AnyState::KobIfdPair(e), Act::Fill(_)) if e.entry_stop > 0 && e.armed == 0 => PAIR_EV_IFD,
        _ => return None,
    };
    let Some((ev, inputs)) = pair_evidence_of(rec, r, idx) else {
        return Some(json!({ "mode": r.int(idx.2), "input": r.int(idx.0), "input_b": r.int(idx.1) }));
    };
    let orders: Vec<Option<String>> = inputs.iter().map(|k| rec.inputs.get(*k).and_then(|i| i.cov).map(|c| c.to_hex())).collect();
    let mut v = json!({ "mode": ev.mode(), "inputs": inputs, "orders": orders });
    match ev {
        PairEvidence::KasBooks { a, b } => {
            v["a"] = json!(a.to_string());
            v["b"] = json!(b.to_string());
        }
        PairEvidence::Pair { price } => v["price"] = json!(price.to_string()),
    }
    Some(v)
}

/// The counterparty of a pair fill: `route`, `netting` or `inventory`.
///
/// It is `route` when the transaction also fills KAS-book orders of A or B. Any KAS-quoted kind
/// counts, and those fills record their own KAS trades. Otherwise it is `netting` when an opposite
/// pair order of the same pair (A, B) fills in the transaction. Otherwise it is `inventory`, the
/// own tokens of the filler.
pub fn pair_counterparty(rec: &TxRecord, self_input: usize, a: [u8; 32], b: [u8; 32], side: u8) -> &'static str {
    let mut netting = false;
    for (i, inp) in rec.inputs.iter().enumerate() {
        if i == self_input {
            continue;
        }
        let Some(r) = inp.reveal.as_ref() else { continue };
        if !matches!(r.nb(), Some(Nb::Fill(n)) if n > 0) {
            continue;
        }
        let Ok(st) = AnyState::decode(r.template, &r.state) else { continue };
        match st.pair_tokens() {
            None => {
                let t = st.token_cov_id();
                if t == a || t == b {
                    return "route";
                }
            }
            Some(p) => {
                if p.a.cov_id == a && p.b.cov_id == b && model::side_of(&st) != side {
                    netting = true;
                }
            }
        }
    }
    if netting {
        "netting"
    } else {
        "inventory"
    }
}

/// One pair fill, as the `pair_fills` row and the `detail.pair` of its event.
#[derive(Debug, Clone)]
struct PairFillRow {
    a: [u8; 32],
    b: [u8; 32],
    a_scale: i64,
    side: u8,
    amount_a: i64,
    /// B the maker received (ASK) or paid (BID).
    ///
    /// It is the `tOut` or `sOut` of the call for `KobPair` and `KobCondPair`, or `amt` for
    /// `KobIfdPair`.
    amount_b: Option<i64>,
    /// The order's quote at the fill, B base units per whole A.
    price: Option<i64>,
    tip_kas: Option<i64>,
    counterparty: &'static str,
}

impl PairFillRow {
    fn detail(&self) -> serde_json::Value {
        let (num, den) = self
            .price
            .and_then(|p| super::reads::reduce(p as i128, self.a_scale as i128))
            .map(|(n, d)| (Some(n.to_string()), Some(d.to_string())))
            .unwrap_or((None, None));
        json!({
            "side": if self.side == 1 { "ask" } else { "bid" },
            "base": crate::hex::encode(&self.a),
            "quote": crate::hex::encode(&self.b),
            "a_scale": self.a_scale,
            "amount_a": self.amount_a.to_string(),
            "amount_b": self.amount_b.map(|v| v.to_string()),
            "price": self.price.map(|v| v.to_string()),
            "price_num": num,
            "price_den": den,
            "tip_kas": self.tip_kas.map(|v| v.to_string()),
            "counterparty": self.counterparty,
            "price_source": "none",
        })
    }

    #[allow(clippy::too_many_arguments)]
    fn insert(
        &self,
        conn: &Connection,
        seq: i64,
        daa: i64,
        ts: i64,
        txid: &Hash32,
        tx_pos: i64,
        cov: &Hash32,
        template: TemplateId,
    ) -> DbResult<()> {
        conn.prepare_cached(
            "INSERT INTO pair_fills (block_seq, daa, ts, txid, tx_pos, covenant_id, contract, base_cov_id, quote_cov_id, side, \
             amount_a, amount_b, price, a_scale, tip_kas, counterparty, price_source) \
             VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,'none')",
        )?
        .execute(params![
            seq,
            daa,
            ts,
            &txid.0[..],
            tx_pos,
            &cov.0[..],
            template.name(),
            &self.a[..],
            &self.b[..],
            self.side as i64,
            self.amount_a,
            self.amount_b,
            self.price,
            self.a_scale,
            self.tip_kas,
            self.counterparty
        ])?;
        Ok(())
    }
}

/// The pair fields of a fill of n base units of A by the pair order `before` at input `input`.
fn pair_fill_of(rec: &TxRecord, input: usize, before: &AnyState, r: &Reveal, n: i64, quote: Option<i64>) -> PairFillRow {
    let t = before.pair_tokens().expect("a pair order");
    let side = model::side_of(before);
    let (amount_b, tip_kas) = match before {
        AnyState::KobPair(p) => (if p.is_ask() { r.int(5) } else { r.int(4) }, guarded(|| p.tip_kas(n)).flatten()),
        AnyState::KobCondPair(c) => (if c.is_ask() { r.int(5) } else { r.int(4) }, guarded(|| c.tip_kas(n)).flatten()),
        AnyState::KobIfdPair(e) => (r.int(6), guarded(|| e.tip_kas(n)).flatten()),
        _ => (None, None),
    };
    PairFillRow {
        a: t.a.cov_id,
        b: t.b.cov_id,
        a_scale: t.a.scale,
        side,
        amount_a: n,
        amount_b,
        price: quote,
        tip_kas,
        counterparty: pair_counterparty(rec, input, t.a.cov_id, t.b.cov_id, side),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The per-order lookups of the write path go through the rows of the order itself.
    ///
    /// A plan that starts from a column that every order shares, such as `kind` or `spent_block`,
    /// reads the whole table on every fill and placement.
    #[test]
    fn per_order_queries_use_the_covenant_indexes() {
        let conn = crate::indexer::db::open_memory("testnet-10").unwrap();
        let plan = |sql: &str| -> String {
            let mut st = conn.prepare(&format!("EXPLAIN QUERY PLAN {sql}")).unwrap();
            let rows: Vec<String> = st.query_map([vec![0u8; 32]], |r| r.get::<_, String>(3)).unwrap().map(|r| r.unwrap()).collect();
            rows.join("; ")
        };
        let p = plan(FILLED_AMOUNT_SQL);
        assert!(p.contains("order_events_cov"), "{p}");
        let p = plan(LAST_EVENT_SQL);
        assert!(p.contains("order_events_cov (order_id=?"), "{p}");
        // The reads of every refresh are one seek each, whatever the order's history: the live UTXO
        // through the partial index of the unspent ones, and the closing event through the one of
        // the closing events. On `order_utxos_cov` and `order_events_cov` they read every UTXO and
        // every event the order ever had.
        let p = plan(TIP_UTXO_SQL);
        assert!(p.contains("order_utxos_live (order_id=?") && !p.contains("TEMP B-TREE"), "{p}");
        let p = plan(CLOSING_EVENT_SQL);
        assert!(p.contains("order_events_closes (order_id=?"), "{p}");
        let p = plan(STORED_FILLED_SQL);
        assert!(p.contains("order_state USING INTEGER PRIMARY KEY"), "{p}");
    }

    /// An order UTXO written again, as by its transaction applied twice (a reorg deeper than the
    /// window, or a lying node), stays spent. `INSERT OR REPLACE` reset it to unspent and brought
    /// the spent order back to the book. An unspent row is still replaced, and so is a spent one by
    /// a recovery operation, whose output the node verified as unspent.
    #[test]
    fn an_order_utxo_written_again_stays_spent() {
        let conn = crate::indexer::db::open_memory("testnet-10").unwrap();
        conn.execute_batch("INSERT INTO ids (id, h) VALUES (1, zeroblob(32)), (2, randomblob(32));").unwrap();
        let cov = Hash32([7; 32]);
        conn.execute(
            "INSERT INTO orders (id, covenant_id, contract, template_id, family, side, token_id, in_book, genesis_state, genesis_block, \
             genesis_daa, listed) VALUES (1, ?1, 'KobAsk', 1, 1, 1, 2, 1, zeroblob(8), 1, 1, 1)",
            [&cov.0[..]],
        )
        .unwrap();
        let p = crate::testkit::processor();
        let row = |txid: &Hash32| -> (Option<i64>, i64, i64) {
            conn.query_row(
                "SELECT spent_block, value, created_block FROM order_utxos WHERE txid = ?1 AND idx = 0",
                [&txid.0[..]],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .unwrap()
        };
        let (spent, live) = (Hash32([1; 32]), Hash32([2; 32]));
        for t in [&spent, &live] {
            p.insert_utxo(&conn, &cov, t, 0, 1_000, &[0, 0], None, 5, 50).unwrap();
        }
        conn.execute("UPDATE order_utxos SET spent_block = 6, spent_txid = x'09' WHERE txid = ?1", [&spent.0[..]]).unwrap();
        // The same outputs, applied again in a later block.
        for t in [&spent, &live] {
            p.insert_utxo(&conn, &cov, t, 0, 2_000, &[0, 0], None, 9, 90).unwrap();
        }
        assert_eq!(row(&spent), (Some(6), 1_000, 5), "a spent row stays spent");
        assert_eq!(row(&live), (None, 2_000, 9), "an unspent row is replaced");
        // A recovery operation (block 0) writes an output the node verified as unspent.
        p.insert_utxo(&conn, &cov, &spent, 0, 3_000, &[0, 0], None, 0, 95).unwrap();
        assert_eq!(row(&spent), (None, 3_000, 0));
    }
}
