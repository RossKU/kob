//! Decides which chain blocks of a window from a non-primary node are re-read from the primary.
//!
//! See `rpc::multi` and `rpc::verify` for the callers. A checked window from another node carries
//! the primary's chain blocks, headers that hash to them, exactly the accepted transactions the
//! primary reports, and transactions that hash to their ids. Two parts of a transaction are
//! outside every hash: its signature scripts and the spent outputs the node attaches to its
//! inputs. The indexer reads both for the transactions it tracks (order reveals, and covenant ids
//! of spent outputs). So every chain block that holds a transaction that can matter to KOB is
//! compared with the primary's copy first.
//!
//! A transaction can matter when one of these holds. Only the parts the hashes cover and the
//! store decide it, never the other node alone.
//!
//! - Its payload is a KOB1 record.
//! - An input spends an output the store tracks (an order, a token output or a holding), or an
//!   output of such a transaction earlier in the same window.
//! - An output carries the covenant binding of a token that some order trades or the allowlist
//!   tracks.
//!
//! Two things are left to the other node:
//! - Token transfers of a token nobody trades, sent to an order id (a "foreign stray"). The
//!   indexer flags them for information only, and a lying node could hide one.
//! - The trigger evidence of untracked fills. The indexer reads it only from transactions that
//!   matter by the rules above, and compares those then.

use super::db::DbResult;
use crate::hex::Hash32;
use crate::rpc::types::RawChainBlock;
use crate::tokens::TokenAllowlist;
use rusqlite::Connection;
use std::collections::HashSet;
use std::sync::Arc;

/// What the store tracks, read once per window.
pub struct TrustContext {
    outpoints: HashSet<(Hash32, u32)>,
    tokens: HashSet<Hash32>,
    allowlist: Arc<TokenAllowlist>,
}

fn hash_at(r: &rusqlite::Row, i: usize) -> rusqlite::Result<Hash32> {
    let b: Vec<u8> = r.get(i)?;
    Ok(Hash32::from_slice(&b).unwrap_or_default())
}

impl TrustContext {
    /// Reads the unspent tracked outpoints and the traded token ids from the store.
    pub fn load(conn: &Connection, allowlist: Arc<TokenAllowlist>) -> DbResult<TrustContext> {
        let mut outpoints = HashSet::new();
        for table in ["order_utxos", "token_utxos", "token_holdings"] {
            let mut st = conn.prepare_cached(&format!("SELECT txid, idx FROM {table} WHERE spent_block IS NULL"))?;
            let rows = st.query_map([], |r| Ok((hash_at(r, 0)?, r.get::<_, u32>(1)?)))?;
            for r in rows {
                outpoints.insert(r?);
            }
        }
        let mut tokens = HashSet::new();
        let mut st = conn.prepare_cached(
            "SELECT h FROM ids WHERE id IN (SELECT token_id FROM orders WHERE token_id IS NOT NULL UNION SELECT quote_id FROM orders WHERE quote_id IS NOT NULL)",
        )?;
        for r in st.query_map([], |r| hash_at(r, 0))? {
            tokens.insert(r?);
        }
        Ok(TrustContext { outpoints, tokens, allowlist })
    }

    /// Per chain block of `blocks`: whether it holds a transaction that can matter to KOB.
    ///
    /// See the module documentation for what matters.
    pub fn flag(&self, blocks: &[RawChainBlock]) -> Vec<bool> {
        let mut created: HashSet<(Hash32, u32)> = HashSet::new();
        blocks
            .iter()
            .map(|b| {
                let mut any = false;
                for t in &b.accepted_transactions {
                    let matters = t.payload.0.starts_with(kob_protocol::payload::MAGIC)
                        || t.inputs.iter().any(|i| {
                            let op = (i.previous_outpoint.transaction_id, i.previous_outpoint.index);
                            self.outpoints.contains(&op) || created.contains(&op)
                        })
                        || t.outputs
                            .iter()
                            .filter_map(|o| o.covenant)
                            .any(|c| self.tokens.contains(&c.covenant_id) || self.allowlist.get(&c.covenant_id).is_some());
                    if matters {
                        any = true;
                        let id = t.verbose_data.transaction_id;
                        created.extend((0..t.outputs.len() as u32).map(|k| (id, k)));
                    }
                }
                any
            })
            .collect()
    }
}
