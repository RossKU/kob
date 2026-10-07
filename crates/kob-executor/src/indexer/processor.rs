//! The order-book state machine: turns extracted transaction records into order, UTXO, token and event
//! rows, for every protocol v3 order kind (both token families; quantities in token base units, prices per whole token).
//! A placement under a template this build does not pin never becomes an order (its payload does not decode, or its output
//! is not the P2SH of a pinned template: a reject). Arms, trailing ratchets and triggered fills read
//! their trigger evidence from a plain resting `KobAsk` / `KobBid` filled in the same transaction (a pair conditional: two
//! such fills, one of each token, or a resting `KobPair` of its pair); the event records which (`detail.evidence`).
//!
//! Pair orders (`KobPair`, `KobCondPair`, `KobIfdPair`): base token A in `token_cov_id`, quote token B in `quote_cov_id`, one
//! custody row per custody of the state (each of its own token), strays of either token the order's own. A pair fill's event
//! has no price (founder rule: prices come only from KAS-book fills); its pair fields are in `detail.pair` and its volume is a
//! `pair_fills` row with the counterparty (`route` / `netting` / `inventory`, [`pair_counterparty`]).
//!
//! Rules that keep it safe against hostile chain content:
//! * malformed or lying transactions never abort a batch (a stalled follower would be a DoS); they
//!   are recorded in `rejects` or ignored, and only database errors propagate;
//! * an order exists only if `kob_protocol::payload::recover_orders` accepts its placement record: the
//!   P2SH rebuilt from the pinned template and the state equals the output, the output is a fresh
//!   covenant whose id is the consensus genesis id of a group that is this output alone,
//!   and (ask side) the custody output is exactly
//!   `amountLeft` base units owned by that id. The payload is never trusted;
//! * later spends are classified from the input's own signature script: the revealed redeem script
//!   hashed to the spent output (checked at extraction) and matched a pinned template, otherwise the
//!   spend is recorded as `unknown` and the lineage is closed conservatively;
//! * continuations are derived by splicing the mutable windows (`kob_protocol::state::mutable_windows`)
//!   and checking the P2SH against the output: a state is stored only when it is proven;
//! * everything mutable is stamped with the chain block that produced it (see `schema.sql`), so a
//!   reorg is an exact revert.

use super::db::{DbError, DbResult};
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
    pub orders: BTreeSet<Hash32>,
    pub tokens: BTreeSet<Hash32>,
    pub fills: Vec<FillNotice>,
    pub relevant_txs: u64,
    pub new_orders: u64,
    pub rejects: u64,
}

pub struct Processor {
    pub tokens: Arc<TokenAllowlist>,
    pub rules: ListingRules,
}

/// The chain block being applied.
pub struct BlockCtx<'a> {
    pub header: &'a ChainBlockHeader,
    pub seq: i64,
}

impl<'a> BlockCtx<'a> {
    /// Stamp a row for the block (every applied block gets one; rows older than the reorg window are
    /// pruned by the writer).
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

/// Runs protocol arithmetic that divides or multiplies chain-supplied values; a hostile order can
/// make it overflow or divide by zero, which must never take the indexer down.
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
    /// Pair orders: the quote token B (`token` is the base token A).
    quote: Option<Hash32>,
}

fn load_order(conn: &Connection, cov: &Hash32) -> DbResult<Option<OrderRow>> {
    let r = conn
        .prepare_cached("SELECT contract, side, price, token_cov_id, tif, listed, quote_cov_id FROM orders WHERE covenant_id = ?1")?
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
}

fn load_order_utxo(conn: &Connection, txid: &Hash32, idx: u32) -> DbResult<Option<UtxoRow>> {
    Ok(conn
        .prepare_cached("SELECT covenant_id FROM order_utxos WHERE txid = ?1 AND idx = ?2 AND spent_block IS NULL")?
        .query_row(params![&txid.0[..], idx], |r| Ok(UtxoRow { covenant_id: hash_at(r, 0)? }))
        .optional()?)
}

pub fn order_exists(conn: &Connection, cov: &Hash32) -> DbResult<bool> {
    super::record::order_known(conn, cov)
}

/// Whether the order already has a live custody row (a second one never becomes custody).
fn has_live_custody(conn: &Connection, cov: &Hash32) -> DbResult<bool> {
    Ok(conn
        .prepare_cached("SELECT 1 FROM token_utxos WHERE owner = ?1 AND role = 'custody' AND spent_block IS NULL LIMIT 1")?
        .exists([&cov.0[..]])?)
}

/// Whether the order already has a live custody row of `token` (a pair order holds at most one custody per token).
fn has_live_custody_of(conn: &Connection, cov: &Hash32, token: &Hash32) -> DbResult<bool> {
    Ok(conn
        .prepare_cached(
            "SELECT 1 FROM token_utxos WHERE owner = ?1 AND token_cov_id = ?2 AND role = 'custody' AND spent_block IS NULL LIMIT 1",
        )?
        .exists(params![&cov.0[..], &token.0[..]])?)
}

/// The extension commitments a pair order's custody of `token` may carry: the ones its custodies of that token carried so
/// far (a rest keeps the extension commitment of the custody it continues), the one the state names for new outputs of that
/// token (`KobIfdPair`'s `aExt` / `bExt`: a merge's new custody) and the one the order row stores (the placement record's
/// first custody part, an exit's entry token).
fn pair_custody_exts(conn: &Connection, cov: &Hash32, token: &Hash32, state: &AnyState) -> DbResult<Vec<[u8; 32]>> {
    let mut v = vec![];
    let mut st = conn.prepare_cached(
        "SELECT program, state FROM token_holdings WHERE owner = ?1 AND token_cov_id = ?2 AND role = 'custody' \
         ORDER BY created_block DESC, idx DESC LIMIT 4",
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

/// [`is_exact_custody`] of a pair order: the token output is exactly one of the custodies the order's CURRENT state
/// requires ([`AnyState::custodies`]: that token, the exact amount), plain, covenant-owned by the order id, of that token's
/// family and (KCC-20) with an extension commitment the order's custody of that token may carry ([`pair_custody_exts`]).
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

/// Whether a token output owned by order `cov` is exactly the custody the covenant rules require of the order's CURRENT
/// state: the order's own token, exactly `amountLeft` base units, plain (borrowing disabled, covenant-owned
/// by the order id) and carrying the extension commitment the order's custody carries. Orders that hold no tokens (bids,
/// exits of a sell-first entry, an empty repeating entry) never have a custody.
fn is_exact_custody(
    conn: &Connection,
    cov: &Hash32,
    token: &Hash32,
    amount: i64,
    held: Option<&kob_protocol::state::TokenState>,
) -> DbResult<bool> {
    let row = conn
        .prepare_cached(
            "SELECT o.contract, o.token_cov_id, o.ext_commit, u.state FROM orders o \
             JOIN order_utxos u ON u.covenant_id = o.covenant_id AND u.spent_block IS NULL \
             WHERE o.covenant_id = ?1 ORDER BY u.created_block DESC, u.idx DESC LIMIT 1",
        )?
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
    // checked: a state that fails the numeric gate has no custody amount at all
    let Some(expect) = crate::sanity::custody_amount(&state).filter(|a| *a > 0) else { return Ok(false) };
    if amount != expect {
        return Ok(false);
    }
    let Some(held) = held else { return Ok(false) };
    if !(held.is_covenant_owned() && held.is_plain() && held.owner() == cov.0 && held.family() == state.family()) {
        return Ok(false);
    }
    // the commitment the order's covenant requires of its custody (its state names it, `AnyState::custody_ext`)
    if state.family() == Family::Kcc20 && state.custody_ext(token.0).is_some_and(|e| e != held.extension()) {
        return Ok(false);
    }
    // the extension commitment the order stores (an exit's is its entry's, see `apply_tx` step 3)
    if let Some(w) = ext {
        if state.family() == Family::Kcc20 && held.extension()[..] != w[..] {
            return Ok(false);
        }
    }
    Ok(true)
}

/// The extension commitment an order stores (`None`: unknown order, or none stored).
fn order_ext_commit(conn: &Connection, cov: &Hash32) -> DbResult<Option<[u8; 32]>> {
    let e: Option<Vec<u8>> = conn
        .prepare_cached("SELECT ext_commit FROM orders WHERE covenant_id = ?1")?
        .query_row([&cov.0[..]], |r| r.get(0))
        .optional()?
        .flatten();
    Ok(e.and_then(|e| <[u8; 32]>::try_from(e.as_slice()).ok()))
}

/// Everything needed to insert a new order lineage.
pub struct NewOrder {
    pub state: AnyState,
    pub covenant_id: Hash32,
    pub txid: Hash32,
    pub out_index: u32,
    pub out_value: u64,
    pub out_spk: Vec<u8>,
    pub block_seq: i64,
    pub daa: u64,
    pub ts: u64,
    pub tx_pos: i64,
    pub parent: Option<Hash32>,
    /// Day orders: UTC unix seconds from the placement record.
    pub deadline: Option<u64>,
    /// Token custody UTXO of an ask-side order (placement record or verified import; a pair order: its first custody,
    /// [`AnyState::custodies`]).
    pub custody: Option<NewCustody>,
    /// A sell-first `KobIfdPair`'s second custody (its B prefund).
    pub prefund: Option<NewCustody>,
    /// Extension commitment of the custody (ask-side kinds: the placement record's custody part, an if-done exit's entry).
    pub ext_commit: Option<[u8; 32]>,
    /// `chain` or `import`.
    pub origin: &'static str,
    /// Inherit the listing decision of the parent (if-done exits), else evaluate the rules.
    pub inherit_listed: Option<(bool, Option<String>)>,
}

pub struct NewCustody {
    pub txid: Hash32,
    pub index: u32,
    pub value: u64,
    pub amount: i64,
    /// The custody's token (`None`: the order's token; a pair order names each custody's token, [`custody_part_tokens`]).
    pub token: Option<Hash32>,
}

/// The tokens of an order's custody parts in record order with their program template hashes: a KAS kind its one token, a
/// pair order each custody of [`AnyState::custodies`] (S of a `KobPair` / `KobCondPair`; a buy-first entry's B escrow; a
/// sell-first entry's A then its B prefund).
pub fn custody_part_tokens(s: &AnyState) -> Vec<(Hash32, Option<[u8; 32]>)> {
    match s.pair_tokens() {
        Some(t) => {
            s.custodies().iter().map(|(c, _)| (Hash32(*c), Some(if *c == t.a.cov_id { t.a.tpl_hash } else { t.b.tpl_hash }))).collect()
        }
        None => vec![(Hash32(s.token_cov_id()), s.token_tpl_hash())],
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum CreateOutcome {
    Created,
    Rejected(String),
}

/// What an entry call did to the order (decoded from the revealed arguments).
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

/// The `upd` flag of a pair conditional's `settle` (argument 11) or a pair entry's `fill` (argument 17): 1 = an update (arm or
/// trail; nb = 0), 0 = a fill, merge or refund.
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
    /// Filled by the continuation step.
    after: Option<AnyState>,
    /// A verified in-place amend continued the order (the record's day-order deadline).
    amend: Option<Option<u64>>,
    /// A verified sweep in place continued the order unchanged (SWEEP record).
    sweep: bool,
}

impl Processor {
    /// Live path: extract the relevant transactions of one chain block and apply them. Returns the
    /// block's row and the records to append to the log.
    pub fn extract_and_apply_block(
        &self,
        conn: &Connection,
        header: &ChainBlockHeader,
        txs: &[Tx],
        delta: &mut Delta,
    ) -> DbResult<Vec<TxRecord>> {
        self.extract_and_apply_block_pre(conn, header, txs, None, delta)
    }

    /// [`Processor::extract_and_apply_block`] with the batch's precomputed pure work ([`super::record::precompute`]).
    pub fn extract_and_apply_block_pre(
        &self,
        conn: &Connection,
        header: &ChainBlockHeader,
        txs: &[Tx],
        pre: Option<&super::record::PreBatch>,
        delta: &mut Delta,
    ) -> DbResult<Vec<TxRecord>> {
        let ctx = BlockCtx::new(conn, header)?;
        let ex = Extractor { conn, tokens: &self.tokens, track_traded: !self.rules.require_allowlist || self.tokens.is_open() };
        let mut out = Vec::new();
        for (pos, tx) in txs.iter().enumerate() {
            let p = pre.and_then(|m| m.get(&tx.verbose_data.transaction_id));
            if let Some(rec) = ex.extract_with(tx, pos as u32, p)? {
                self.apply_tx(conn, &ctx, &rec, p, delta)?;
                delta.relevant_txs += 1;
                out.push(rec);
            }
        }
        Ok(out)
    }

    /// Replay path: apply the already extracted records of one logged chain block.
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

        // Trigger evidence the transaction carries: the quotes of the plain resting KobAsk / KobBid filled in it (a
        // trailing `update` ratchets from one of them; `docs/spec/matcher.md` §4).
        let evidence_prices: Vec<i64> = rec.inputs.iter().filter_map(|i| evidence_touch(i.reveal.as_ref()?).map(|(_, p)| p)).collect();

        // 1. spends of tracked outputs
        let mut spent: Vec<Spent> = Vec::new();
        for (i, inp) in rec.inputs.iter().enumerate() {
            if let Some(u) = load_order_utxo(conn, &inp.txid, inp.index)? {
                if let Some(s) = self.spend_order(conn, seq, rec, i, u)? {
                    spent.push(s);
                }
            } else {
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
        let spent_covs: HashSet<Hash32> = spent.iter().map(|s| s.cov).collect();
        // In-place amends the payload announces (AMEND records): each is verified against the continuation it names.
        let amends: Vec<Record> = match payload::decode(&rec.payload) {
            Ok(Some(p)) => p.records.into_iter().filter(|r| matches!(r, Record::Amend { .. } | Record::Sweep { .. })).collect(),
            _ => vec![],
        };

        // 2. outputs: continuations (a maker's cancel continues the id only as a verified in-place amend; any other
        //    continuation of a cancel stays unproven: state unknown, listed nowhere)
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

        // 3. if-done exits: a fresh covenant output matching the exit the entry committed to
        let mut exits: HashMap<Hash32, Hash32> = HashMap::new();
        let mut created: HashSet<Hash32> = HashSet::new();
        for s in spent.iter().filter(|s| matches!(s.act, Act::Fill(_)) && model::exit_template(s.order.template).is_some()) {
            let Some((exit_state, cov, j)) = self.derive_exit(s, rec, &spent_covs, conn)? else {
                self.record_reject(conn, seq, &txid, "ifd_exit:not_derivable", delta)?;
                continue;
            };
            let out = &rec.outputs[j];
            // The exit's token identity is its entry's: a buy-first entry pays the bought tokens to the exit's custody with its
            // own `extensionCommitment` (the covenant writes it), a sell-first entry's `KobCondBid` exit carries it in its state.
            // Stored like a placed order's, so every view of the exit (and the matcher's rebuilt custody) has it.
            // A pair entry's exit holds n of A (buy-first) or B (sell-first) written with the entry's `aExt` / `bExt`.
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

        // 5. token outputs owned by orders: the order's own exact custody, or a stray.
        //
        // Nothing on chain says which output of a transaction is the continuation custody: the ask covenants pin ONE token
        // output (`tokOut`) and only guard token INPUTS, so a taker can add further outputs owned by the order id. An output
        // is therefore the custody only if it is exactly what the covenant rules require of the order's state after this
        // transaction (`amountLeft` base units of the order's own token, plain, with the order's extension commitment);
        // the first such output is the custody, every other same-owner output is a stray. A pair order holds one custody per
        // token (`AnyState::custodies`: a sell-first `KobIfdPair` its A and its B prefund); any other output of either pair
        // token owned by it is its own stray (not foreign). Kinds that hold KAS (bids, exits of
        // a sell-first entry, an empty repeating entry) have no custody: every token owned by them is a stray.
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
            conn.prepare_cached(
                "INSERT OR IGNORE INTO token_utxos (txid, idx, token_cov_id, owner, amount, value, role, created_block, created_daa) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9)",
            )?
            .execute(params![&txid.0[..], t.out, &token.0[..], &t.owner.0[..], t.amount, out.value as i64, role, seq, daa as i64])?;
            // a placement record may already have stored this very output as the custody: keep the stored role
            let stored: String = conn
                .prepare_cached("SELECT role FROM token_utxos WHERE txid = ?1 AND idx = ?2")?
                .query_row(params![&txid.0[..], t.out], |r| r.get(0))?;
            roles.insert(t.out, stored);
            delta.tokens.insert(token);
            delta.orders.insert(t.owner);
        }
        // 5b. holdings: every proven output of a tracked token, whoever owns it
        for h in &rec.holds {
            let Some(out) = rec.outputs.get(h.out as usize) else { continue };
            let Some((_, token)) = out.cov else { continue };
            let role = roles.get(&h.out).map(|s| s.as_str()).unwrap_or("owned");
            self.insert_holding(conn, &txid, h.out, &token, h.program, &h.state, out.value, role, seq, daa)?;
            delta.tokens.insert(token);
        }

        // 6. events (an in-place amend first takes the order's new terms)
        for s in &spent {
            if let (Some(deadline), Some(after)) = (s.amend, &s.after) {
                let carrier = rec.outputs.iter().find(|o| o.cov.is_some_and(|(_, c)| c == s.cov)).map(|o| o.value).unwrap_or(0);
                self.apply_amend(conn, ctx, &s.cov, after, deadline, carrier)?;
            }
            let closes = !continued.contains(&s.cov);
            self.record_spend_events(conn, ctx, rec, s, closes, &exits, &spent, delta)?;
        }
        Ok(())
    }

    /// The proven holding row of a custody the node verified at the script `TokenState::custody(amount, order id, ext)` produces
    /// (imports, gap adoptions): the order views then carry the custody's token state, as for a custody seen on chain. Nothing
    /// is stored without the extension commitment or for a token program this build does not pin.
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
        // the custody's token and program (a pair order names each custody's token; its family is that token's)
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
            // a KRON token has no extension commitment
            (Family::Kron, _) => [0; 32],
            (_, Some(e)) => e,
            _ => return Ok(()),
        };
        let ts = kob_protocol::state::TokenState::custody(tt.family, c.amount, cov.0, ext);
        conn.prepare_cached("DELETE FROM token_holdings WHERE txid = ?1 AND idx = ?2")?.execute(params![&c.txid.0[..], c.index])?;
        self.insert_holding(conn, &c.txid, c.index, &token, tt.id, &ts, c.value, "custody", seq, daa)
    }

    /// Store a token output whose state was proven against its script public key.
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
        conn.prepare_cached(
            "INSERT OR IGNORE INTO token_holdings (txid, idx, token_cov_id, program, family, owner, owner_kind, amount, value, state, role, created_block, created_daa) \
             VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13)",
        )?
        .execute(params![
            &txid.0[..],
            idx,
            &token.0[..],
            program.name(),
            state.family().code(),
            &state.owner()[..],
            kind,
            state.amount(),
            value as i64,
            state.encode(),
            role,
            seq,
            daa as i64
        ])?;
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
    // placement records

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
                // the custody parts and their tokens (a pair order: `custodies()` order, each in its own token)
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
                        // the placement record reveals the custody states: prove each against its output before storing it
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

    /// Insert an order lineage. Shared by chain processing, if-done exits and recovery imports.
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
        // an order whose numbers the covenant would refuse or the matcher cannot survive is never listed (exits
        // included: they used to inherit the parent's decision without a look at their own numbers)
        let sane = crate::sanity::check(&n.state);
        let pair = n.state.pair_tokens();
        let quote = pair.map(|t| Hash32(t.b.cov_id));
        let (listed, reason) = match n.inherit_listed {
            _ if sane.is_err() => (false, sane.err()),
            Some((l, r)) => (l, r),
            None => self.listing(&n.state, ext, n.daa, n.out_value),
        };
        conn.prepare_cached(
            "INSERT INTO orders (covenant_id, contract, template_hash, family, side, maker, token_cov_id, token_tpl_hash, ext_commit, scale, min_fill, price, tip, tif, expiry_daa, active_from, in_book, budget_rate, reserve, initial_amount, deadline, genesis_state, genesis_txid, genesis_out, genesis_block, genesis_daa, parent, listed, unlisted_reason, origin, quote_cov_id)
             VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17,?18,?19,?20,?21,?22,?23,?24,?25,?26,?27,?28,?29,?30,?31)",
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
        ])?;
        self.insert_utxo(conn, &n.covenant_id, &n.txid, n.out_index, n.out_value, &n.out_spk, Some(&state_bytes), n.block_seq, n.daa)?;
        for c in n.custody.iter().chain(n.prefund.iter()) {
            let ctok = c.token.unwrap_or(token);
            conn.prepare_cached(
                "INSERT OR IGNORE INTO token_utxos (txid, idx, token_cov_id, owner, amount, value, role, created_block, created_daa) VALUES (?1,?2,?3,?4,?5,?6,'custody',?7,?8)",
            )?
            .execute(params![&c.txid.0[..], c.index, &ctok.0[..], &n.covenant_id.0[..], c.amount, c.value as i64, n.block_seq, n.daa as i64])?;
            // a chain placement proves the custody's state from its transfer leader (step 5b); an import verified the custody
            // script the state below produces, so it is proven the same way
            if n.origin == "import" {
                self.custody_holding(conn, &n.covenant_id, &n.state, c, ext, n.block_seq, n.daa)?;
            }
        }
        conn.prepare_cached(
            "INSERT INTO order_events (covenant_id, block_seq, daa, ts, txid, tx_pos, kind, token_cov_id, side, amount, price, closes, detail) VALUES (?1,?2,?3,?4,?5,?6,'create',?7,?8,?9,?10,0,?11)",
        )?
        .execute(params![
            &n.covenant_id.0[..],
            n.block_seq,
            n.daa as i64,
            n.ts as i64,
            &n.txid.0[..],
            n.tx_pos,
            &token.0[..],
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
        // token registry event: first sighting of this (token, program, extension) identity (a pair order names two tokens:
        // each with the extension commitment its listing input carries)
        let identities: Vec<(Hash32, Option<Hash32>, Option<[u8; 32]>)> = match pair_listing(&n.state, n.daa, n.out_value) {
            Some((ia, ib)) => [ia, ib]
                .into_iter()
                .filter_map(|i| {
                    Some((i.token_cov_id?, i.token_tpl_hash, Some(i.extension_commitment.map(|e| e.0).unwrap_or([0; 32]))))
                })
                .collect(),
            None => vec![(token, tpl_hash, ext)],
        };
        for (tok, tpl, e) in identities {
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
        // a pair order is in the pair book of both its tokens: `book:<B>` subscribers refetch too
        delta.tokens.extend(quote);
        delta.new_orders += 1;
        Ok(CreateOutcome::Created)
    }

    /// The listing rules (`tokens::ListingRules`) on an order's state as of `daa` (its genesis, or an in-place amend's
    /// block) with `carrier` on its output.
    fn listing(&self, state: &AnyState, ext: Option<[u8; 32]>, daa: u64, carrier: u64) -> (bool, Option<String>) {
        // a pair order needs both tokens acceptable: A as the order's token, B as any traded token (reasons `quote_...`);
        // neither is measured in KAS (a pair order has no KAS quote)
        if let Some((ia, ib)) = pair_listing(state, daa, carrier) {
            let r = self
                .rules
                .evaluate(&self.tokens, &ia)
                .and_then(|()| self.rules.evaluate(&self.tokens, &ib).map_err(|r| format!("quote_{r}")));
            return match r {
                Ok(()) => (true, None),
                Err(r) => (false, Some(r)),
            };
        }
        let terms = model::terms_of(state);
        // a conditional order is measured at its take-profit leg, or at its stop when it has none (`tpPrice` 0: a stop-market,
        // stop-limit or trailing stop); measured at the absent take-profit every such stop was worth 0 and never listed
        let listing_price = match state {
            AnyState::KobCondAsk(c) | AnyState::KobCondAskKron(c) => Some(if c.tp_price > 0 { c.tp_price } else { c.stop_price }),
            AnyState::KobCondBid(c) | AnyState::KobCondBidKron(c) => Some(if c.tp_price > 0 { c.tp_price } else { c.stop_price }),
            _ => terms.price,
        };
        let r = self.rules.evaluate(
            &self.tokens,
            &ListingInput {
                token_cov_id: Some(Hash32(state.token_cov_id())),
                token_tpl_hash: state.token_tpl_hash().map(Hash32),
                extension_commitment: ext.map(Hash32),
                family: Some(state.family()),
                scale: Some(terms.scale).filter(|s| *s > 0),
                min_fill: Some(terms.min_fill),
                // a bid has no amount: its quantity (and value) is its escrow, the carrier
                amount: state.amount_left(),
                price: listing_price,
                tip: Some(terms.tip),
                expiry_daa: Some(terms.expiry_daa),
                genesis_daa: daa,
                carrier,
            },
        );
        match r {
            Ok(()) => (true, None),
            Err(r) => (false, Some(r)),
        }
    }

    /// The state an in-place amend (AMEND record) continues a maker-cancelled order with at output `j`, and the record's
    /// deadline, if a record names exactly this continuation and it verifies: [`payload::verify_amend`] against the
    /// revealed previous state (template, the maker's `cancel`, terms that keep the custody exact, the output the P2SH of
    /// the new state and the only output carrying the id), a state the numeric gate accepts, and the order's custody still
    /// live after the transaction (an amend never moves it). A record that fails is a reject and the continuation stays
    /// unproven (state unknown: listed nowhere).
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
        // the spent script is proven (its redeem script hashed to the spent output at extraction): the input is its P2SH
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
        // an ask's custody stays where it is (a bid owns none: its quantity is the continuation's escrow)
        let holds_custody = matches!(a.order, AnyState::KobAsk(_) | AnyState::KobAskKron(_));
        if holds_custody && !has_live_custody(conn, &s.cov)? {
            self.record_reject(conn, seq, &rec.txid, "amend:custody_not_live", delta)?;
            return Ok(None);
        }
        Ok(Some((a.order, a.deadline)))
    }

    /// The state a sweep in place (SWEEP record) continues a maker-cancelled order with at output `j`: the SAME state, if a
    /// record names exactly this continuation and [`payload::verify_sweep`] accepts it against the revealed previous state,
    /// and the order's custody (token-holding kinds with an amount left) is still live after the transaction (a sweep never
    /// moves it). A record that fails is a reject and the continuation stays unproven.
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

    /// Takes an in-place amend's terms into the order row (price, tip, time in force, expiry, activation, deadline) and
    /// re-runs the listing rules on them (as of the amend's block, with the continuation's carrier). The replaced values
    /// are kept in `order_amends`, stamped with the block, so a reorg restores them exactly ([`revert_block`]).
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
            "INSERT INTO order_amends (covenant_id, block_seq, price, tip, tif, expiry_daa, active_from, deadline, listed, unlisted_reason, budget_rate, reserve) \
             SELECT covenant_id, ?2, price, tip, tif, expiry_daa, active_from, deadline, listed, unlisted_reason, budget_rate, reserve FROM orders WHERE covenant_id = ?1",
        )?
        .execute(params![&cov.0[..], ctx.seq])?;
        // a bid's budget rate and reserve follow its new terms (its remaining amount is the buying power of its escrow)
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
            Ok(()) => self.listing(state, order_ext_commit(conn, cov)?, ctx.header.daa_score, carrier),
        };
        conn.prepare_cached(
            "UPDATE orders SET price = ?2, tip = ?3, tif = ?4, expiry_daa = ?5, active_from = ?6, deadline = ?7, listed = ?8, \
             unlisted_reason = ?9 WHERE covenant_id = ?1",
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
            reason
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
            "INSERT OR REPLACE INTO order_utxos (txid, idx, covenant_id, value, spk, state, created_block, created_daa) VALUES (?1,?2,?3,?4,?5,?6,?7,?8)",
        )?
        .execute(params![&txid.0[..], idx, &cov.0[..], value as i64, spk, state, seq, daa as i64])?;
        Ok(())
    }

    // -----------------------------------------------------------------------------------------
    // spends

    fn spend_order(&self, conn: &Connection, seq: i64, rec: &TxRecord, input_idx: usize, u: UtxoRow) -> DbResult<Option<Spent>> {
        let Some(order) = load_order(conn, &u.covenant_id)? else { return Ok(None) };
        let inp = &rec.inputs[input_idx];
        let reveal = inp.reveal.clone().filter(|r| r.template == order.template);
        let entry = reveal.as_ref().and_then(|r| r.entry());
        let act = match (&reveal, entry) {
            (Some(r), Some(e)) => match classify(e, r.nb()) {
                // a pair conditional's / pair stop entry's update is `settle` / `fill` with nb = 0 and upd = 1
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
            after: None,
            amend: None,
            sweep: false,
        }))
    }

    // -----------------------------------------------------------------------------------------
    // derivations (each result is proven against the output's script public key)

    /// The state of the continuation output, if it can be proven: the spent script with its mutable
    /// windows (`amountLeft`, `armed`, `stopPrice`, `rptAmount`) replaced by values the entry call could
    /// have written. Merges, fills, arms and trailing steps all fall in the candidate sets; the
    /// P2SH of the spliced script decides.
    fn derive_after(&self, s: &Spent, out_spk: &[u8], evidence_prices: &[i64], rec: &TxRecord) -> Option<AnyState> {
        let before = s.before.as_ref()?;
        let id = before.template_id();
        let t = template(id);
        let redeem = before.redeem();
        // unchanged script (Bid fills, IOC-less updates that changed nothing)
        if p2sh_spk(&redeem) == out_spk {
            return Some(before.clone());
        }
        let windows = kob_protocol::state::mutable_windows(id);
        if windows.is_empty() {
            return None;
        }
        let cur = model::mutable_of(before);
        let is_update = s.act == Act::Update;
        // checked: the fill / merge amount comes from the chain (a spend the covenant accepted keeps it in range)
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
                // a pair conditional trails by the k its pair evidence justifies (`CondPairState::trail_k`)
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
        // a pair order's exact custody (`custody`): what each entry call can leave in it (the P2SH decides)
        let mut custs: Vec<Option<i64>> = vec![cur.custody];
        if let Some(c) = cur.custody {
            let arg = |i: usize| s.reveal.as_ref().and_then(|r| r.int(i));
            match (before, s.act) {
                (AnyState::KobPair(_) | AnyState::KobCondPair(_), Act::Fill(n)) => {
                    // sOut (argument 4) released; an ask releases n
                    custs.extend([arg(4), Some(n)].into_iter().flatten().map(|d| c.checked_sub(d)));
                    if let AnyState::KobCondPair(x) = before {
                        // a re-arming BID exit keeps custody - proceeds - back while it continues
                        let keep = guarded(|| c.checked_sub(x.rpt_proceeds(n)?)?.checked_sub(x.rpt_back(n)?)).flatten();
                        custs.push(keep);
                    }
                    custs.push(Some(0));
                }
                (AnyState::KobIfdPair(e), Act::Fill(n)) => {
                    // buy-first: the escrow minus the spend (argument amt 6); sell-first: the prefund minus pre(n)
                    custs.extend(arg(6).map(|a| c.checked_sub(a)));
                    custs.push(guarded(|| c.checked_sub(e.pre_of(n)?)).flatten());
                    custs.push(Some(0));
                }
                (AnyState::KobIfdPair(e), Act::Merge { m, .. }) => {
                    // buy-first: the escrow grows by budget(m); sell-first: the prefund by pre(m)
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

    /// The exit an if-done entry fill created: the committed exit with `amountLeft = n` and the
    /// repeat fields the entry wrote (booked while re-arms remain). Returns the exit state, its
    /// covenant id and its output index.
    fn derive_exit(
        &self,
        s: &Spent,
        rec: &TxRecord,
        spent_covs: &HashSet<Hash32>,
        conn: &Connection,
    ) -> DbResult<Option<(AnyState, Hash32, usize)>> {
        let (Some(before), Act::Fill(n), Some(reveal)) = (&s.before, s.act, &s.reveal) else { return Ok(None) };
        // the call's time argument: the last one (KobIfdPair.fill: argument 13, before aOut, bOut, xc and upd)
        let t = match before {
            AnyState::KobIfdPair(_) => reveal.int(13),
            _ => reveal.args.last().and_then(|a| crate::script::script_int(a)),
        }
        .unwrap_or(0);
        let exit_arg = match before {
            AnyState::KobIfdBid(_) | AnyState::KobIfdBidKron(_) => reveal.int(2),
            AnyState::KobIfdAsk(_) | AnyState::KobIfdAskKron(_) => reveal.int(3),
            AnyState::KobIfdPair(_) => reveal.int(5),
            _ => None,
        };
        let entry_cov = s.cov.0;
        let candidates: Vec<AnyState> = match before {
            AnyState::KobIfdBid(b) | AnyState::KobIfdBidKron(b) => {
                exit_candidates_bid(b, n, entry_cov, t, s.utxo_daa, before.family())
            }
            AnyState::KobIfdAsk(a) | AnyState::KobIfdAskKron(a) => {
                exit_candidates_ask(a, n, entry_cov, t, s.utxo_daa, before.family())
            }
            AnyState::KobIfdPair(e) => exit_candidates_pair(e, n, entry_cov, t, s.utxo_daa, reveal.int(6)),
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
    // events

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
        // a pair order is paid in token B or A (its positional output is a token delivery; its KAS is the delivery carrier)
        let payout = if matches!(s.act, Act::Fill(_)) && s.order.side == 1 && !is_pair {
            rec.outputs.get(s.input_idx).map(|o| o.value as i64)
        } else {
            None
        };
        let mut detail = json!({"input": s.input_idx, "entry": s.entry, "verified": s.before.is_some()});
        // which plain resting fill of this transaction armed, ratcheted or triggered the order (a pair order: its pair evidence,
        // two KAS-book fills of A and B or a resting KobPair of the pair)
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
                // the call's time argument: the last one, except for the pair kinds (settle: argument 3; fill: argument 13)
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
                // A pair fill (docs/spec/matcher.md, price rule): no KAS price (the event's `price` is null, it is never a KAS
                // trade, candle or last price), `detail.pair` carries the pair fields and `pair_fills` its volume.
                if let (Some(b), Some(r)) = (pair, s.reveal.as_ref()) {
                    let row = pair_fill_of(rec, s.input_idx, b, r, n, quote);
                    detail["pair"] = row.detail();
                    pair_fill = Some(row);
                }
                // bid side: what the fill drew from the escrow (the continuation keeps the rest) and the delivery output at the
                // input's position (the maker's token output: its KAS is the delivery carrier plus used(n) - allIn(t), the difference
                // between the budget the fill consumed at pMax + tip, rounded up, and what it paid at its quote, rounded down), so a
                // reader can check that a buy never cost more than its all-in limit (escrow_after = 0 when the bid sold out):
                // escrow_before - escrow_after - delivery_value <= floor(n * (q + tip) / scale)
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
                // the event's price: the KAS quote of a KAS kind; null for a pair fill (its quote in B is in `detail.pair`)
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
                // in-place amend: the same order (covenant id, custody) with new terms; `detail.previous` the replaced ones
                if let Some(b) = &s.before {
                    let t = model::terms_of(b);
                    detail["previous"] =
                        json!({"price": t.price, "tip": t.tip, "tif": t.tif, "expiryDaa": t.expiry_daa, "activeFrom": t.active_from});
                }
                let after = s.after.as_ref();
                events.push(("amend", after.and_then(|a| a.amount_left()), after.and_then(|a| model::terms_of(a).price), false));
            }
            Act::Cancel if s.sweep => {
                // sweep in place: the same order at a new output, its strays returned to the maker
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
        for (i, (kind, amount, price, closing)) in events.iter().enumerate() {
            let d = if i == 0 {
                detail.to_string()
            } else {
                json!({"input": s.input_idx, "entry": s.entry, "remainder": true}).to_string()
            };
            conn.prepare_cached(
                "INSERT INTO order_events (covenant_id, block_seq, daa, ts, txid, tx_pos, kind, token_cov_id, side, amount, price, payout, closes, detail) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14)",
            )?
            .execute(params![
                &s.cov.0[..],
                seq,
                daa,
                ts,
                &txid.0[..],
                rec.pos as i64,
                kind,
                s.order.token.map(|t| t.0.to_vec()),
                s.order.side as i64,
                amount,
                price,
                if i == 0 { payout } else { None },
                *closing as i64,
                d
            ])?;
        }
        if let Some(p) = &pair_fill {
            p.insert(conn, seq, daa, ts, &txid, rec.pos as i64, &s.cov, s.order.template)?;
        }
        refresh_order_state(conn, &s.cov)?;
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

    /// Quote bound of a fill at the call's time argument (auctions: the price the covenant bounds it by).
    fn fill_quote(&self, before: &AnyState, s: &Spent, _n: i64) -> Option<i64> {
        let r = s.reveal.as_ref()?;
        let daa = s.utxo_daa;
        // the call's time argument: the last one, except for the pair kinds (KobPair / KobCondPair.settle: argument 3,
        // KobIfdPair.fill: argument 13)
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
            // the pair kinds: the quote in token B per whole A (never a KAS price)
            AnyState::KobPair(p) => p.price_at(t, daa),
            AnyState::KobCondPair(c) => {
                let leg = r.int(6)?;
                c.leg_price(leg, c.armed == 0 && leg == 1, t, daa)
            }
            AnyState::KobIfdPair(e) => e.price_at(e.entry_stop > 0 && e.armed == 0, t, daa),
        })
        .flatten()
    }
}

/// The listing inputs of a pair order's two tokens (`None` for the KAS kinds): A with the order's scale, minimum fill, amount
/// and expiry; B with its own scale. Neither carries a KAS price (a pair order is not measured in KAS). The extension
/// commitment of each KCC-20 token: the state names it (`sExt` of the custody of S, `tExt` of the deliveries of T; an entry
/// `aExt` and `bExt`).
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
            min_fill: base.then_some(terms.min_fill),
            amount: if base { state.amount_left() } else { None },
            price: None,
            tip: None,
            expiry_daa: base.then_some(terms.expiry_daa),
            genesis_daa,
            carrier,
        }
    };
    Some((input(&t.a, ea, true), input(&t.b, eb, false)))
}

/// Step and candidate multiples of a trailing update: the multiples the trigger evidence in the transaction (the quotes of
/// the plain resting orders it fills) justifies, plus a small range as a safety net.
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

fn exit_candidates_bid(b: &IfdBidState, n: i64, entry: [u8; 32], t: i64, utxo_daa: i64, fam: Family) -> Vec<AnyState> {
    let mut v = vec![];
    if let Some(Ok(x)) = guarded(|| b.exit_for(n, None)) {
        v.push(AnyState::KobCondAsk(x).into_family(fam));
    }
    if b.rpt_amount > n {
        let Some(until) = rpt_until(b.expiry_daa, t, utxo_daa) else { return v };
        let booking = Booking { parent: entry, until };
        if let Some(Ok(x)) = guarded(|| b.exit_for(n, Some(booking))) {
            v.push(AnyState::KobCondAsk(x).into_family(fam));
        }
    }
    v
}

fn exit_candidates_ask(a: &IfdAskState, n: i64, entry: [u8; 32], t: i64, utxo_daa: i64, fam: Family) -> Vec<AnyState> {
    let mut v = vec![];
    if let Some(Ok(x)) = guarded(|| a.exit_for(n, None)) {
        v.push(AnyState::KobCondBid(x).into_family(fam));
    }
    if a.rpt_amount > n {
        let Some(until) = rpt_until(a.expiry_daa, t, utxo_daa) else { return v };
        let booking = Booking { parent: entry, until };
        if let Some(Ok(x)) = guarded(|| a.exit_for(n, Some(booking))) {
            v.push(AnyState::KobCondBid(x).into_family(fam));
        }
    }
    v
}

/// The exits a `KobIfdPair` fill of n can create (`IfdPairState::exit_for`): its custody is n of A (buy-first) or, sell-first,
/// the proceeds `amt` (argument 6) plus the prefund of the fill (`pre(n)`, or the whole prefund left when the entry ends);
/// booked with `rptUntil = rpt_until(expiryDaa, t, UTXO DAA)` when the entry repeats and `rptAmount > n`.
fn exit_candidates_pair(e: &IfdPairState, n: i64, entry: [u8; 32], t: i64, utxo_daa: i64, amt: Option<i64>) -> Vec<AnyState> {
    let custodies: Vec<i64> = if e.is_buy_first() {
        vec![n]
    } else {
        let Some(amt) = amt else { return vec![] };
        [guarded(|| e.pre_of(n)).flatten(), Some(e.custody)].into_iter().flatten().filter_map(|p| amt.checked_add(p)).collect()
    };
    let mut bookings = vec![None];
    if e.rpt_amount > n {
        if let Some(until) = rpt_until(e.expiry_daa, t, utxo_daa) {
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

/// A `TxJson` view of a record, enough for `recover_orders` (outpoints, covenant ids, outputs).
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

/// (txid, idx, value, state) of an order's current UTXO.
type TipRow = (Vec<u8>, i64, i64, Option<Vec<u8>>);

/// The fill amounts of one order (see [`refresh_order_state`] for the `+kind`). Summed in Rust, saturating: base-unit
/// amounts of hostile orders can sum past an `i64`, where SQLite's `SUM` fails (a failed refresh would stall the follower).
pub(crate) const FILLED_AMOUNT_SQL: &str = "SELECT amount FROM order_events WHERE covenant_id = ?1 AND +kind = 'fill'";

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

/// Re-derive one order's `order_state` row from the base tables. Called after every change and
/// after every revert, so it is the single definition of "status".
pub fn refresh_order_state(conn: &Connection, cov: &Hash32) -> DbResult<()> {
    let Some(order) = load_order_full(conn, cov)? else {
        conn.execute("DELETE FROM order_state WHERE covenant_id = ?1", [&cov.0[..]])?;
        return Ok(());
    };
    // `+kind`: through the order's own events (`order_events_cov`). On `kind = 'fill'` alone the planner picks the
    // market index (`order_events_fill_ts`, kind first) and reads every fill of every token, on every fill and placement
    // (refresh cost grew with the chain's fill count: 1.5 ms per KOB transaction after 60 s of a busy chain).
    let filled = filled_amount(conn, cov)?;
    let tip: Option<TipRow> = conn
        .prepare_cached(
            "SELECT txid, idx, value, state FROM order_utxos WHERE covenant_id = ?1 AND spent_block IS NULL ORDER BY created_block DESC, idx DESC LIMIT 1",
        )?
        .query_row([&cov.0[..]], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))
        .optional()?;
    let last: Option<(i64, i64)> = conn
        .prepare_cached("SELECT block_seq, daa FROM order_events WHERE covenant_id = ?1 ORDER BY id DESC LIMIT 1")?
        .query_row([&cov.0[..]], |r| Ok((r.get(0)?, r.get(1)?)))
        .optional()?;
    let closing: Option<(String, Option<i64>)> = conn
        .prepare_cached("SELECT kind, amount FROM order_events WHERE covenant_id = ?1 AND closes = 1 ORDER BY id DESC LIMIT 1")?
        .query_row([&cov.0[..]], |r| Ok((r.get(0)?, r.get(1)?)))
        .optional()?;
    let status = match (&tip, closing.as_ref().map(|c| c.0.as_str())) {
        (Some(_), _) if filled > 0 => "partial",
        (Some(_), _) => "open",
        (None, Some("fill")) => "filled",
        (None, Some("cancel")) => "cancelled",
        (None, Some("refund")) => "refunded",
        (None, Some("kill")) => "killed",
        (None, _) => "closed",
    };
    // remaining amount: exact from the tip state where the kind has `amountLeft`; a bid's is its buying power
    let (remaining, exact): (Option<i64>, bool) = match &tip {
        None => (Some(closing.as_ref().filter(|c| c.0 == "kill").and_then(|c| c.1).unwrap_or(0)), true),
        Some((_, _, value, state)) => {
            let known = state.as_ref().and_then(|s| AnyState::decode(order.template, s).ok());
            match (known.as_ref().and_then(|s| s.amount_left()), &known, order.template.base()) {
                (Some(l), _, _) => (Some(l), true),
                // a bid's quantity is its escrow: the most one fill can buy, the largest n whose budget
                // ceil(n * (pMax + tip) / scale) leaves the delivery carrier and the reserve (an upper bound: every further fill
                // moves one more carrier)
                (None, Some(AnyState::KobBid(b) | AnyState::KobBidKron(b)), _) => (Some(b.buying_power(*value)), false),
                // an unproven bid state: the same bound from the order row (no delivery carrier known)
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
    conn.prepare_cached(
        "INSERT OR REPLACE INTO order_state (covenant_id, status, filled_amount, remaining_amount, amount_exact, cur_txid, cur_idx, cur_value, state_known, last_block, last_daa) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11)",
    )?
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
    // An if-done entry with a stop is a stop order until it is armed: nothing fills it at its limit before trigger evidence
    // in the same transaction (`docs/spec/matcher.md` §4), so it is in no KAS book, depth or count, where its limit would
    // read as resting liquidity (a sell-stop entry's limit sits below the market and looked like a crossed ask). Armed, it
    // auctions in its book. Follows the tip state both ways (reorgs included).
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

/// Undo everything one chain block did. The block must be the newest stored one.
/// Returns whether the block had changed any KOB row (a reorg that reverts only unrelated blocks is not
/// worth a record-log frame).
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
    collect("SELECT DISTINCT covenant_id FROM order_events WHERE block_seq = ?1")?;
    collect("SELECT covenant_id FROM order_utxos WHERE created_block = ?1 OR spent_block = ?1")?;
    collect("SELECT covenant_id FROM orders WHERE genesis_block = ?1")?;
    collect("SELECT owner FROM token_utxos WHERE created_block = ?1 OR spent_block = ?1")?;
    // a holding a reverted block created or spent belongs to a token whose holders' view changes (owners are not always orders)
    let mut holding_tokens: BTreeSet<Hash32> = BTreeSet::new();
    {
        let mut st =
            conn.prepare_cached("SELECT DISTINCT token_cov_id FROM token_holdings WHERE created_block = ?1 OR spent_block = ?1")?;
        for r in st.query_map([seq], |r| hash_at(r, 0))? {
            holding_tokens.insert(r?);
        }
    }
    // tokens whose books change
    let mut tokens: BTreeSet<Hash32> = BTreeSet::new();
    for cov in &affected {
        if let Some(o) = load_order(conn, cov)? {
            if let Some(t) = o.token {
                tokens.insert(t);
            }
            tokens.extend(o.quote);
        }
    }
    // in-place amends of the block: the orders take back the terms they replaced (newest first)
    {
        let mut st = conn.prepare_cached(
            "SELECT id, covenant_id, price, tip, tif, expiry_daa, active_from, deadline, listed, unlisted_reason, budget_rate, reserve \
             FROM order_amends WHERE block_seq = ?1 ORDER BY id DESC",
        )?;
        type AmendRow = (
            i64,
            Vec<u8>,
            Option<i64>,
            Option<i64>,
            Option<i64>,
            Option<i64>,
            Option<i64>,
            Option<i64>,
            i64,
            Option<String>,
            Option<i64>,
            Option<i64>,
        );
        let rows: Vec<AmendRow> = st
            .query_map([seq], |r| {
                Ok((
                    r.get(0)?,
                    r.get(1)?,
                    r.get(2)?,
                    r.get(3)?,
                    r.get(4)?,
                    r.get(5)?,
                    r.get(6)?,
                    r.get(7)?,
                    r.get(8)?,
                    r.get(9)?,
                    r.get(10)?,
                    r.get(11)?,
                ))
            })?
            .collect::<rusqlite::Result<_>>()?;
        for (_, cov, price, tip, tif, expiry, active, deadline, listed, reason, budget_rate, reserve) in rows {
            changed += conn.execute(
                "UPDATE orders SET price = ?2, tip = ?3, tif = ?4, expiry_daa = ?5, active_from = ?6, deadline = ?7, listed = ?8, \
                 unlisted_reason = ?9, budget_rate = ?10, reserve = ?11 WHERE covenant_id = ?1",
                params![cov, price, tip, tif, expiry, active, deadline, listed, reason, budget_rate, reserve],
            )?;
        }
        changed += conn.execute("DELETE FROM order_amends WHERE block_seq = ?1", [seq])?;
    }
    changed += conn.execute("DELETE FROM order_events WHERE block_seq = ?1", [seq])?;
    changed += conn.execute("DELETE FROM pair_fills WHERE block_seq = ?1", [seq])?;
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
    changed += conn.execute("DELETE FROM orders WHERE genesis_block = ?1", [seq])?;
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
    pub imported: u64,
    pub already_known: u64,
    pub closed: u64,
    pub adopted: u64,
    pub rejected: Vec<String>,
}

impl Processor {
    /// Apply one operator operation (recovery import, gap close, gap adoption). All of them stamp
    /// block 0, which is never reverted.
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
                // each custody's token: the order's own (a KAS kind), or the token of that part of `AnyState::custodies`
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
                    // A known order with no live output (closed by a gap reconciliation: it continued under a new script the
                    // indexer never saw, e.g. a partial fill) is revived from the node-verified continuation: the node holds an
                    // output of this covenant id at this script, which only the order's own lineage can produce.
                    match self.revive(conn, delta, covenant_id, &st, *txid, *index, *value, spk, *daa, cust.as_ref(), ext)? {
                        true => rep.imported += 1,
                        false => rep.already_known += 1,
                    }
                    return Ok(());
                }
                // an exit names its entry: only an entry this indexer knows (else the exit is imported as an order of its own)
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
                    conn.prepare_cached("UPDATE token_utxos SET spent_block = 0 WHERE owner = ?1 AND spent_block IS NULL")?
                        .execute([&covenant_id.0[..]])?;
                    conn.prepare_cached("UPDATE token_holdings SET spent_block = 0 WHERE owner = ?1 AND role IN ('custody','stray') AND spent_block IS NULL")?
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
                    .prepare_cached("SELECT state FROM order_utxos WHERE txid = ?1 AND idx = ?2 AND spent_block IS NULL")?
                    .query_row(params![&old_txid.0[..], old_index], |r| r.get(0))
                    .optional()?
                    .flatten();
                let n = conn
                    .prepare_cached("UPDATE order_utxos SET spent_block = 0, spent_entry = 'gap' WHERE txid = ?1 AND idx = ?2 AND spent_block IS NULL")?
                    .execute(params![&old_txid.0[..], old_index])?;
                if n > 0 {
                    // the token UTXOs owned by the order that the node no longer has are closed; the ones it still holds stay
                    // (the reconciliation checked each at the script its proven state produces)
                    let kept: HashSet<(Hash32, u32)> = keep.iter().copied().collect();
                    let owned: Vec<(Hash32, u32)> = {
                        let mut st =
                            conn.prepare_cached("SELECT txid, idx FROM token_utxos WHERE owner = ?1 AND spent_block IS NULL")?;
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
                    // the output's own DAA (the refund and kill times count from it); operations written before 2026-10-02 have
                    // only the cursor's
                    let udaa = daa.unwrap_or(cursor_daa);
                    self.insert_utxo(conn, covenant_id, txid, *index, *value, spk, state.as_deref(), 0, udaa)?;
                    let tid = load_order(conn, covenant_id)?.map(|o| o.template);
                    let st = match (tid, state.as_deref()) {
                        (Some(t), Some(s)) => AnyState::decode(t, s).ok(),
                        _ => None,
                    };
                    if let (Some(c), Some(st)) = (custody, st) {
                        // the first custody's token (a pair order: the first of `AnyState::custodies`, either token)
                        let token = custody_part_tokens(&st).first().map(|t| t.0).unwrap_or(Hash32(st.token_cov_id()));
                        let live = conn
                            .prepare_cached("SELECT 1 FROM token_utxos WHERE txid = ?1 AND idx = ?2 AND spent_block IS NULL")?
                            .exists(params![&c.txid.0[..], c.index])?;
                        if !live {
                            conn.prepare_cached(
                                "INSERT INTO token_utxos (txid, idx, token_cov_id, owner, amount, value, role, created_block, created_daa) VALUES (?1,?2,?3,?4,?5,?6,'custody',0,?7) \
                                 ON CONFLICT(txid, idx) DO UPDATE SET spent_block = NULL, spent_txid = NULL, role = 'custody'",
                            )?
                            .execute(params![&c.txid.0[..], c.index, &token.0[..], &covenant_id.0[..], c.amount, c.value as i64, udaa as i64])?;
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

    /// Revives a known order without a live output (every output closed, e.g. by `CloseGap`) at a node-verified continuation
    /// of the same template: the output and its custody become live again, an `update` event marks the import. `false` (and
    /// nothing changed) when the order still has a live output or the template differs.
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
        let live =
            conn.prepare_cached("SELECT 1 FROM order_utxos WHERE covenant_id = ?1 AND spent_block IS NULL")?.exists([&cov.0[..]])?;
        if live {
            return Ok(false);
        }
        let state = st.encode();
        conn.prepare_cached("DELETE FROM order_utxos WHERE txid = ?1 AND idx = ?2")?.execute(params![&txid.0[..], index])?;
        self.insert_utxo(conn, cov, &txid, index, value, spk, Some(&state), 0, daa)?;
        let token = Hash32(st.token_cov_id());
        if let Some(c) = custody {
            conn.prepare_cached(
                "INSERT INTO token_utxos (txid, idx, token_cov_id, owner, amount, value, role, created_block, created_daa) VALUES (?1,?2,?3,?4,?5,?6,'custody',0,?7) \
                 ON CONFLICT(txid, idx) DO UPDATE SET spent_block = NULL, spent_txid = NULL",
            )?
            .execute(params![&c.txid.0[..], c.index, &c.token.unwrap_or(token).0[..], &cov.0[..], c.amount, c.value as i64, daa as i64])?;
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
        conn.prepare_cached(
            "INSERT INTO order_events (covenant_id, block_seq, daa, ts, txid, tx_pos, kind, token_cov_id, side, closes, detail) VALUES (?1, 0, ?2, 0, ?3, 0, ?4, ?5, ?6, ?7, ?8)",
        )?
        .execute(params![&cov.0[..], daa as i64, &txid.0[..], kind, token.map(|t| t.0.to_vec()), side as i64, closes as i64, detail.to_string()])?;
        Ok(())
    }
}

// ---------------------------------------------------------------------------------------------
// trigger evidence (protocol v2.6 and v3)

/// The side and quote of a plain resting `KobAsk` / `KobBid` (either family) whose revealed spend is a fill (`n > 0`): what
/// that input offers as trigger evidence to the conditional orders and stop entries of the same transaction (side
/// [`kob_protocol::state::SIDE_ASK`]: a resting ask was filled, [`kob_protocol::state::SIDE_BID`]: a resting bid). The
/// covenants' other rules (exposure, `minTouch` base units of an order of the same scale, no decay) are theirs to enforce;
/// the indexer only reads which input the order named.
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

/// The input index of the trigger evidence an order's spend names: the `ev` argument of an `update` (arm or trailing
/// ratchet), or of a fill that triggers an unarmed stop leg (`KobCondAsk.settle(n, tk, _, 1, ev, tk, t)`,
/// `KobCondBid.settle(n, tok, 1, ev, t)`) or an unarmed stop entry (`KobIfdBid.fill(.., ev, t)`,
/// `KobIfdAsk.settle(.., ev, tk, t)`). `None` for every other spend (an armed stop, a take-profit leg, a limit entry).
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

/// `detail.evidence` of an event: the evidence input, its order's covenant id and (when its spend is revealed) its side and
/// quote.
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
// pair orders: trigger evidence, fill rows, counterparty (founder price rule)

/// Argument indices `(evA, evB, evMode)` of a pair conditional's `settle` (fill and update alike).
const PAIR_EV_COND: (usize, usize, usize) = (7, 8, 10);
/// Argument indices `(evA, evB, evMode)` of a pair entry's `fill` (fill and update alike).
const PAIR_EV_IFD: (usize, usize, usize) = (9, 10, 12);

/// The pair trigger evidence a pair spend names and its input indices: mode 0, a plain resting KAS-book order of A (`evA`)
/// and one of B (`evB`) filled in the transaction (the quotes `a` and `b`); mode 1, a resting `KobPair` of the pair (`evA`)
/// filled in it (its `price`). The covenant checks the rest (tokens, sides, scales, exposure, minTouch); the indexer reads
/// what the spend named. `None` when the named inputs reveal no such fill.
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

/// `detail.evidence` of a pair order's spend that used trigger evidence (an update, a fill of an unarmed stop leg, a fill of
/// an unarmed stop entry): `{"mode": 0 | 1, "inputs": [..], "orders": [..], "a" and "b" (mode 0: the two KAS quotes) or
/// "price" (mode 1: the resting pair order's quote in B per whole A)}`.
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

/// The counterparty of a pair fill (founder price rule): `route` when the transaction also fills KAS-book orders of A or B
/// (any KAS-quoted kind: those fills record their own KAS trades), else `netting` when an opposite pair order of the same pair
/// (A, B) fills in it, else `inventory` (the filler's own tokens).
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

/// One pair fill (`pair_fills` row and `detail.pair` of its event).
#[derive(Debug, Clone)]
struct PairFillRow {
    a: [u8; 32],
    b: [u8; 32],
    a_scale: i64,
    side: u8,
    amount_a: i64,
    /// B the maker received (ASK) or paid (BID): the call's `tOut` / `sOut` (`KobPair`, `KobCondPair`) or `amt` (`KobIfdPair`).
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

    /// The per-order lookups of the write path go through the order's own rows: a plan that starts from a column every
    /// order shares (`kind`, `spent_block`) reads the whole table on every fill and placement.
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
        for sql in [
            "SELECT txid, idx, value, state FROM order_utxos WHERE covenant_id = ?1 AND spent_block IS NULL ORDER BY created_block DESC, idx DESC LIMIT 1",
            "SELECT block_seq, daa FROM order_events WHERE covenant_id = ?1 ORDER BY id DESC LIMIT 1",
            "SELECT kind, amount FROM order_events WHERE covenant_id = ?1 AND closes = 1 ORDER BY id DESC LIMIT 1",
        ] {
            let p = plan(sql);
            assert!(p.contains("_cov (covenant_id=?)"), "{sql}: {p}");
        }
    }
}
