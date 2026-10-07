//! The matcher's view of the indexer's store: [`StoreBook`] implements
//! [`OrderBookView`](crate::matcher::book::OrderBookView) over the SQLite tables, so the matcher and
//! the keepers plan against exactly the orders the API serves (`docs/ops/executor.md`).
//!
//! What is listed (`docs/spec/matcher.md` 1.1, 1.2):
//!
//! * an order the indexer validated (`orders.listed`), whose current UTXO is unspent and whose state
//!   is proven (`order_state.state_known`): the placement record checked, a fresh covenant id, the
//!   continuation derived and verified against the output's script public key;
//! * its custody, when the kind holds tokens: the one live `custody` token UTXO. An order with more
//!   than one custody row is left out (the view fails closed); a missing or wrong-sized custody is
//!   reported as it is and the matcher's `custody_ok` rejects the order;
//! * its strays: live token UTXOs owned by the order id that are not its custody. Never liquidity,
//!   never spent by a matcher or keeper;
//!
//! The book is read in one transaction ([`snapshot`]), so orders, custody and strays are of one
//! cursor. The trait methods read piecewise; the runner uses [`snapshot`] through
//! [`crate::executor::IndexerSource`].

use super::db::DbResult;
use super::reads::tip_state;
use crate::hex::Hash32;
use crate::matcher::book::{ListedOrder, MemoryBook, OrderBookView};
use kob_protocol::family::Family;
use kob_protocol::state::TokenState;
use kob_protocol::tx::{OrderUtxo, TokenUtxo, Utxo};
use rusqlite::Connection;
use std::collections::HashMap;

/// The store as an [`OrderBookView`]. `operator` is the key the matcher signs with (informational: since protocol v2.6
/// nothing in the view depends on it; the v2.4 operator receipts are gone).
pub struct StoreBook<'a> {
    pub conn: &'a Connection,
    pub operator: Option<[u8; 32]>,
}

impl<'a> StoreBook<'a> {
    pub fn new(conn: &'a Connection, operator: Option<[u8; 32]>) -> Self {
        StoreBook { conn, operator }
    }
}

fn logged<T: Default>(what: &str, r: DbResult<T>) -> T {
    r.unwrap_or_else(|e| {
        tracing::warn!(error = %e, "cannot read {what} from the store; offering an empty book");
        T::default()
    })
}

impl OrderBookView for StoreBook<'_> {
    fn daa_score(&self) -> u64 {
        logged("the cursor", cursor_daa(self.conn))
    }

    fn orders(&self) -> Vec<ListedOrder> {
        logged("the orders", listed_orders(self.conn))
    }
}

/// DAA score of the chain block the store is synced to.
pub fn cursor_daa(conn: &Connection) -> DbResult<u64> {
    Ok(super::db::meta_get(conn, "cursor_daa")?.and_then(|d| d.parse().ok()).unwrap_or(0))
}

/// One consistent read of the whole book (a single read transaction).
pub fn snapshot(conn: &Connection, _operator: Option<[u8; 32]>) -> DbResult<MemoryBook> {
    let tx = conn.unchecked_transaction()?;
    let book = MemoryBook {
        daa_score: cursor_daa(&tx)?,
        orders: listed_orders(&tx)?,
        // The operator's P2PK token inventory is not on any order id: the indexer does not track it.
        wallet_tokens: vec![],
    };
    tx.commit()?;
    Ok(book)
}

/// The operator's own key-owned token UTXOs, live and accepted, with their programs (`token_holdings`: every proven output of
/// a tracked token, whoever owns it), oldest first. Tokens a transaction leaves the operator (a route of an older planner,
/// a taker's own inventory) land here; the maintenance jobs (`crate::maintenance`) merge or sell them.
pub fn operator_tokens(conn: &Connection, operator: &[u8; 32]) -> DbResult<Vec<crate::maintenance::OwnToken>> {
    let mut st = conn.prepare_cached(
        "SELECT txid, idx, token_cov_id, program, value, state, created_daa FROM token_holdings \
         WHERE owner = ?1 AND spent_block IS NULL ORDER BY created_block, txid, idx",
    )?;
    let rows = st.query_map([&operator[..]], |r| {
        Ok((
            r.get::<_, Vec<u8>>(0)?,
            r.get::<_, u32>(1)?,
            r.get::<_, Vec<u8>>(2)?,
            r.get::<_, String>(3)?,
            r.get::<_, i64>(4)?,
            r.get::<_, Vec<u8>>(5)?,
            r.get::<_, i64>(6)?,
        ))
    })?;
    let mut out = vec![];
    for r in rows {
        let (txid, idx, token, program, value, state, daa) = r?;
        let Some(program) = kob_protocol::artifacts::TemplateId::from_name(&program).filter(|p| p.is_token()) else { continue };
        let Ok(state) = TokenState::decode_with(kob_protocol::artifacts::token_template(program), &state) else { continue };
        let (Some(txid), Some(token)) = (h32(txid), h32(token)) else { continue };
        // key-owned only (the operator's P2PK / address-presence tokens), never an order's custody or stray
        if !state.is_user() || state.owner() != *operator {
            continue;
        }
        let utxo = Utxo {
            transaction_id: txid,
            index: idx,
            amount: value.max(0) as u64,
            block_daa_score: daa.max(0) as u64,
            covenant_id: Some(token),
        };
        out.push(crate::maintenance::OwnToken { program, token: TokenUtxo { utxo, state } });
    }
    Ok(out)
}

fn h32(b: Vec<u8>) -> Option<[u8; 32]> {
    <[u8; 32]>::try_from(b.as_slice()).ok()
}

/// A live token UTXO owned by an order id.
struct OwnedToken {
    utxo: Utxo,
    amount: i64,
    custody: bool,
    /// The state the indexer proved for the output (`token_holdings`), when it has one.
    proven: Option<TokenState>,
    /// The program it was proven under.
    program: Option<kob_protocol::artifacts::TemplateId>,
}

fn owned_tokens(conn: &Connection) -> DbResult<HashMap<[u8; 32], Vec<OwnedToken>>> {
    let mut st = conn.prepare_cached(
        "SELECT t.owner, t.txid, t.idx, t.amount, t.value, t.role, t.created_daa, t.token_cov_id, h.program, h.state \
         FROM token_utxos t LEFT JOIN token_holdings h ON h.txid = t.txid AND h.idx = t.idx \
         WHERE t.spent_block IS NULL ORDER BY t.created_block, t.txid, t.idx",
    )?;
    let rows = st.query_map([], |r| {
        Ok((
            (
                r.get::<_, Vec<u8>>(0)?,
                r.get::<_, Vec<u8>>(1)?,
                r.get::<_, u32>(2)?,
                r.get::<_, i64>(3)?,
                r.get::<_, i64>(4)?,
                r.get::<_, String>(5)?,
                r.get::<_, i64>(6)?,
                r.get::<_, Vec<u8>>(7)?,
            ),
            r.get::<_, Option<String>>(8)?,
            r.get::<_, Option<Vec<u8>>>(9)?,
        ))
    })?;
    let mut m: HashMap<[u8; 32], Vec<OwnedToken>> = HashMap::new();
    for r in rows {
        let ((owner, txid, idx, amount, value, role, daa, token), program, state) = r?;
        let program = program.and_then(|p| kob_protocol::artifacts::TemplateId::from_name(&p)).filter(|p| p.is_token());
        let proven =
            program.zip(state).and_then(|(p, s)| TokenState::decode_with(kob_protocol::artifacts::token_template(p), &s).ok());
        let program = program.filter(|_| proven.is_some());
        let (Some(owner), Some(txid), Some(token)) = (h32(owner), h32(txid), h32(token)) else { continue };
        let utxo = Utxo {
            transaction_id: txid,
            index: idx,
            amount: value.max(0) as u64,
            block_daa_score: daa.max(0) as u64,
            covenant_id: Some(token),
        };
        m.entry(owner).or_default().push(OwnedToken { utxo, amount, custody: role == "custody", proven, program });
    }
    Ok(m)
}

/// Every listed live order with its custody and strays.
pub fn listed_orders(conn: &Connection) -> DbResult<Vec<ListedOrder>> {
    live_orders(conn, true)
}

/// Every live order the indexer proved but does not list (a token outside the allowlist, an exit of an unlisted entry, ...),
/// with its custody and strays: never offered to the matcher; the keepers kill, refund and close them when that pays.
pub fn unlisted_orders(conn: &Connection) -> DbResult<Vec<ListedOrder>> {
    live_orders(conn, false)
}

fn live_orders(conn: &Connection, listed: bool) -> DbResult<Vec<ListedOrder>> {
    let mut tokens = owned_tokens(conn)?;
    let mut st = conn.prepare_cached(
        "SELECT o.covenant_id, o.contract, o.family, o.deadline, o.ext_commit, u.txid, u.idx, u.value, u.state, u.created_daa, \
         o.template_hash \
         FROM orders o \
         JOIN order_state s ON s.covenant_id = o.covenant_id \
         JOIN order_utxos u ON u.txid = s.cur_txid AND u.idx = s.cur_idx \
         WHERE o.listed = ?1 AND s.state_known = 1 AND u.spent_block IS NULL AND u.state IS NOT NULL \
         ORDER BY o.genesis_block, o.covenant_id",
    )?;
    let rows = st.query_map([listed as i64], |r| {
        Ok((
            r.get::<_, Vec<u8>>(0)?,
            r.get::<_, String>(1)?,
            r.get::<_, i64>(2)?,
            r.get::<_, Option<i64>>(3)?,
            r.get::<_, Option<Vec<u8>>>(4)?,
            r.get::<_, Vec<u8>>(5)?,
            r.get::<_, u32>(6)?,
            r.get::<_, i64>(7)?,
            r.get::<_, Option<Vec<u8>>>(8)?,
            r.get::<_, i64>(9)?,
            r.get::<_, Vec<u8>>(10)?,
        ))
    })?;
    let mut out = Vec::new();
    for r in rows {
        let (cov, contract, family, deadline, ext, txid, idx, value, state, daa, tpl_hash) = r?;
        let Some(fam) = u8::try_from(family).ok().and_then(Family::from_code) else { continue };
        let (Some(cov), Some(txid)) = (h32(cov), h32(txid)) else { continue };
        let Some(state) = tip_state(&contract, state) else { continue };
        // An order placed under a template this build does not pin (a database carried over a template change: an older
        // template with today's state layout decodes as today's kind) lives at a script nothing this build plans from today's
        // template (a fill, kill, refund or close) hashes to. It is unknown here, like any other template: never offered to
        // the matcher or the keepers.
        if kob_protocol::artifacts::try_template(state.template_id()).is_none_or(|t| t.hash[..] != tpl_hash[..]) {
            continue;
        }
        // a state the numeric gate refuses is never offered to the matcher (belt and braces: the listing flag already says so)
        if let Err(why) = crate::sanity::check(&state) {
            if !listed {
                continue; // often the very reason it is unlisted
            }
            tracing::warn!(order = %Hash32(cov), %why, "order state fails the numeric gate; not listed");
            continue;
        }
        // a KRON token has no extension commitment: the placement record's is zero and `TokenState` ignores it
        let ext = ext.and_then(h32).unwrap_or([0; 32]);
        let own = tokens.remove(&cov).unwrap_or_default();
        // tokens of the order itself (a pair order: A and B) vs foreign strays (any other token, §1.2)
        let pair = state.pair_tokens();
        let mine = |t: &OwnedToken| {
            let tok = t.utxo.covenant_id;
            tok == Some(state.token_cov_id()) || pair.is_some_and(|p| tok == Some(p.b.cov_id))
        };
        let (own, foreign_tokens): (Vec<_>, Vec<_>) = own.into_iter().partition(|t| mine(t));
        let mut foreign: Vec<kob_protocol::build::ForeignStrays> = vec![];
        for t in foreign_tokens {
            // only a state the indexer proved, under a program this build pins, can be moved
            let (Some(state), Some(program), Some(tok)) = (t.proven, t.program, t.utxo.covenant_id) else { continue };
            let utxo = TokenUtxo { utxo: t.utxo, state };
            match foreign.iter_mut().find(|g| g.token.covenant_id == tok) {
                Some(g) => g.utxos.push(utxo),
                None => foreign.push(kob_protocol::build::ForeignStrays {
                    token: kob_protocol::build::TokenRef { covenant_id: tok, program },
                    utxos: vec![utxo],
                }),
            }
        }
        let (custody, strays): (Vec<_>, Vec<_>) = own.into_iter().partition(|t| t.custody);
        // one custody per token the state holds (a pair order: `AnyState::custodies` order, at most one per token)
        let mut custody_tokens: Vec<Option<[u8; 32]>> = custody.iter().map(|t| t.utxo.covenant_id).collect();
        custody_tokens.sort();
        custody_tokens.dedup();
        if custody.len() > 1 && (pair.is_none() || custody_tokens.len() != custody.len()) {
            if listed {
                tracing::warn!(order = %Hash32(cov), custody = custody.len(), "order has several live custody outputs; not listed");
            }
            continue;
        }
        // Every custody or stray carries the state the indexer proved against the output's script public key when it stored the
        // row (a stray of another token, token B of a pair order, has its own family and extension commitment; a stray of the
        // order's own token need not be plain nor carry the order's commitment). Without a proven state (a token the indexer does
        // not track) it is rebuilt as `custody(family, amount, owner, ext)`, the exact custody's state.
        let as_token = |t: OwnedToken| {
            // a pair order's token B has its own family (its custody's commitment is proven: every custody row has its holding)
            let tfam = match pair {
                Some(p) if t.utxo.covenant_id == Some(p.b.cov_id) => p.b.family_of().unwrap_or(fam),
                Some(p) => p.a.family_of().unwrap_or(fam),
                None => fam,
            };
            let state = t.proven.unwrap_or_else(|| TokenState::custody(tfam, t.amount, cov, ext));
            TokenUtxo { utxo: t.utxo, state }
        };
        // the custodies in `AnyState::custodies` order: `custody` the first, `custody_b` the second (a sell-first pair entry's
        // B prefund)
        let order_of: Vec<[u8; 32]> = state.custodies().iter().map(|c| c.0).collect();
        let mut custody = custody;
        custody.sort_by_key(|t| t.utxo.covenant_id.and_then(|c| order_of.iter().position(|x| *x == c)).unwrap_or(usize::MAX));
        let mut custody = custody.into_iter();
        let (first, second) = (custody.next().map(as_token), custody.next().map(as_token));
        out.push(ListedOrder {
            family: fam,
            order: OrderUtxo {
                utxo: Utxo {
                    transaction_id: txid,
                    index: idx,
                    amount: value.max(0) as u64,
                    block_daa_score: daa.max(0) as u64,
                    covenant_id: Some(cov),
                },
                state,
            },
            custody: first,
            custody_b: second,
            deadline: deadline.and_then(|d| u64::try_from(d).ok()),
            // The chain block that made this UTXO visible: earlier is served first among immediate
            // orders (`docs/spec/matcher.md` 3.1). Deterministic across restarts, unlike a wall clock.
            seen_daa: daa.max(0) as u64,
            foreign,
            strays: strays.into_iter().map(as_token).collect(),
        });
    }
    Ok(out)
}
