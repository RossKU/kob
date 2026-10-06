//! Which chain blocks of a window from a node other than the primary must be taken from the primary before the window is
//! applied (`rpc::multi`, `rpc::verify`).
//!
//! A checked window from another node carries the primary's chain blocks, headers that hash to them, exactly the accepted
//! transactions the primary reports and transactions that hash to their ids. Two parts of a transaction are outside every
//! hash: its signature scripts and the spent outputs the node attaches to its inputs. The indexer reads both for the
//! transactions it tracks (order reveals, covenant ids of spent outputs), so every chain block holding a transaction that
//! can matter to KOB is compared with the primary's copy first. A transaction can matter when, by the parts the hashes
//! cover or by the store (nothing the other node says alone):
//!
//! * its payload is a KOB1 record,
//! * an input spends an output the store tracks (an order, a token output, a holding), or an output of such a transaction
//!   earlier in the same window (chains of KOB transactions inside one window),
//! * an output carries the covenant binding of a token some order trades or the allowlist tracks.
//!
//! Left to the other node: token transfers of a token nobody trades sent to an order id (a "foreign stray" the indexer flags
//! for information only; a lying node could hide one), and the trigger evidence of untracked fills, which the indexer reads
//! only from transactions that matter by the rules above (compared then).

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
            "SELECT token_cov_id FROM orders WHERE token_cov_id IS NOT NULL UNION SELECT quote_cov_id FROM orders WHERE quote_cov_id IS NOT NULL",
        )?;
        for r in st.query_map([], |r| hash_at(r, 0))? {
            tokens.insert(r?);
        }
        Ok(TrustContext { outpoints, tokens, allowlist })
    }

    /// Per chain block of `blocks`: whether it holds a transaction that can matter to KOB (see the module documentation).
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
