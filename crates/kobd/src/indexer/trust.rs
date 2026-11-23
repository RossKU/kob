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
//! - Its payload is a KOB1 payload with a placement, amend or sweep record. One that does not
//!   decode, or holds only informational records, is read from the payload alone.
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

/// What [`TrustContext::load`] looks up for a window: every outpoint its transactions spend and
/// every distinct covenant id of their outputs.
///
/// They are taken from the window before the store is read, so the read costs what the window
/// holds, not what the store holds.
#[derive(Debug, Default)]
pub struct WindowKeys {
    outpoints: Vec<(Hash32, u32)>,
    covenants: Vec<Hash32>,
}

impl WindowKeys {
    /// The keys of the chain blocks `blocks`.
    pub fn of(blocks: &[RawChainBlock]) -> WindowKeys {
        let mut outpoints = HashSet::new();
        let mut covenants = HashSet::new();
        for t in blocks.iter().flat_map(|b| &b.accepted_transactions) {
            outpoints.extend(t.inputs.iter().map(|i| (i.previous_outpoint.transaction_id, i.previous_outpoint.index)));
            covenants.extend(t.outputs.iter().filter_map(|o| o.covenant).map(|c| c.covenant_id));
        }
        WindowKeys { outpoints: outpoints.into_iter().collect(), covenants: covenants.into_iter().collect() }
    }
}

/// What the store tracks of one window ([`WindowKeys`]).
pub struct TrustContext {
    outpoints: HashSet<(Hash32, u32)>,
    tokens: HashSet<Hash32>,
    allowlist: Arc<TokenAllowlist>,
}

impl TrustContext {
    /// Reads which outpoints of `keys` are unspent tracked outputs, and which of its covenant ids
    /// are tokens that some order trades.
    ///
    /// Each key is one primary-key or index lookup per table, so a window costs what it holds
    /// whatever the size of the store. (An earlier build read every unspent tracked output and
    /// every order of the store for each window from another node.)
    pub fn load(conn: &Connection, allowlist: Arc<TokenAllowlist>, keys: &WindowKeys) -> DbResult<TrustContext> {
        let mut outpoints = HashSet::new();
        let mut tracked: Vec<_> = [
            "SELECT 1 FROM order_utxos WHERE txid = ?1 AND idx = ?2 AND spent_block IS NULL",
            "SELECT 1 FROM token_utxos WHERE txid = ?1 AND idx = ?2 AND spent_block IS NULL",
            concat!("SELECT 1 FROM token_holdings WHERE tx_id = ", super::ids::tx_of!("?1"), " AND idx = ?2 AND spent_block IS NULL"),
        ]
        .iter()
        .map(|sql| conn.prepare_cached(sql))
        .collect::<rusqlite::Result<_>>()?;
        for (txid, idx) in &keys.outpoints {
            for st in tracked.iter_mut() {
                if st.exists(rusqlite::params![&txid.0[..], idx])? {
                    outpoints.insert((*txid, *idx));
                    break;
                }
            }
        }
        let mut tokens = HashSet::new();
        let mut traded = conn.prepare_cached(
            "SELECT 1 FROM ids i WHERE i.h = ?1 AND (EXISTS (SELECT 1 FROM orders WHERE token_id = i.id) \
             OR EXISTS (SELECT 1 FROM orders WHERE quote_id = i.id))",
        )?;
        for c in &keys.covenants {
            if traded.exists([&c.0[..]])? {
                tokens.insert(*c);
            }
        }
        Ok(TrustContext { outpoints, tokens, allowlist })
    }

    /// Per chain block of `blocks`: whether it holds a transaction that can matter to KOB.
    ///
    /// See the module documentation for what matters.
    pub fn flag(&self, blocks: &[RawChainBlock]) -> Vec<bool> {
        self.matters(blocks).into_iter().map(|t| !t.is_empty()).collect()
    }

    /// Per chain block of `blocks`: the positions of its transactions that can matter to KOB.
    pub fn matters(&self, blocks: &[RawChainBlock]) -> Vec<Vec<usize>> {
        let mut created: HashSet<(Hash32, u32)> = HashSet::new();
        blocks
            .iter()
            .map(|b| {
                let mut any = vec![];
                for (k, t) in b.accepted_transactions.iter().enumerate() {
                    let matters = places(&t.payload.0)
                        || t.inputs.iter().any(|i| {
                            let op = (i.previous_outpoint.transaction_id, i.previous_outpoint.index);
                            self.outpoints.contains(&op) || created.contains(&op)
                        })
                        || t.outputs
                            .iter()
                            .filter_map(|o| o.covenant)
                            .any(|c| self.tokens.contains(&c.covenant_id) || self.allowlist.get(&c.covenant_id).is_some());
                    if matters {
                        any.push(k);
                        let id = t.verbose_data.transaction_id;
                        created.extend((0..t.outputs.len() as u32).map(|k| (id, k)));
                    }
                }
                any
            })
            .collect()
    }
}

/// Whether a payload is a KOB1 payload with a placement, amend or sweep record.
///
/// Those records are read with the transaction's signature scripts (the leaders of the custody
/// outputs). A KOB1 payload that does not decode, or holds only informational records, is read
/// from the payload alone, which the transaction id covers. So it needs no merged block fetched,
/// and spam with the magic costs the follower nothing more than any other transaction (C-M4).
fn places(payload: &[u8]) -> bool {
    use kob_protocol::payload::{decode, Record, MAGIC};
    payload.starts_with(MAGIC)
        && decode(payload)
            .ok()
            .flatten()
            .is_some_and(|p| p.records.iter().any(|r| matches!(r, Record::Order { .. } | Record::Amend { .. } | Record::Sweep { .. })))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rpc::types::{ChainBlockHeader, CovenantBinding, Outpoint, Tx, TxInput, TxOutput};

    fn input(txid: u8, index: u32) -> TxInput {
        TxInput {
            previous_outpoint: Outpoint { transaction_id: Hash32([txid; 32]), index },
            signature_script: Default::default(),
            sequence: None,
            verbose_data: None,
        }
    }

    fn tx(id: u8, inputs: Vec<TxInput>, covenant: Option<u8>) -> Tx {
        let mut t = Tx { inputs, ..Default::default() };
        t.verbose_data.transaction_id = Hash32([id; 32]);
        t.outputs = vec![TxOutput {
            value: 1,
            script_public_key: Default::default(),
            covenant: covenant.map(|c| CovenantBinding { authorizing_input: 0, covenant_id: Hash32([c; 32]) }),
        }];
        t
    }

    fn block(txs: Vec<Tx>) -> RawChainBlock {
        RawChainBlock { chain_block_header: ChainBlockHeader::default(), accepted_transactions: txs }
    }

    /// The store is read for the keys of the window only, and decides the same as the rules of the
    /// module: a spend of an unspent tracked output, a spend of an output of such a transaction
    /// earlier in the window, and an output of a traded token matter; a spend of a spent output, an
    /// untracked spend and an untraded token do not.
    #[test]
    fn a_window_is_flagged_from_lookups_of_its_own_keys() {
        let conn = crate::indexer::db::open_memory("testnet-10").unwrap();
        conn.execute_batch(
            "INSERT INTO ids (h) VALUES (x'0000000000000000000000000000000000000000000000000000000000000000'), \
             (x'7777777777777777777777777777777777777777777777777777777777777777'); \
             INSERT INTO orders (covenant_id, contract, template_id, family, side, token_id, in_book, genesis_state, genesis_block, genesis_daa, listed) \
             VALUES (x'01', 'KobAsk', 1, 1, 1, 2, 1, x'00', 1, 1, 1); \
             INSERT INTO order_utxos (txid, idx, order_id, value, spk, created_block, created_daa) \
             VALUES (x'1111111111111111111111111111111111111111111111111111111111111111', 0, 1, 1, x'00', 1, 1); \
             INSERT INTO order_utxos (txid, idx, order_id, value, spk, created_block, created_daa, spent_block) \
             VALUES (x'2222222222222222222222222222222222222222222222222222222222222222', 0, 1, 1, x'00', 1, 1, 2);",
        )
        .unwrap();
        let blocks = vec![
            block(vec![tx(0xa1, vec![input(0x11, 0)], None)]),
            block(vec![tx(0xa2, vec![input(0x22, 0)], None), tx(0xa3, vec![input(0x33, 0)], Some(0x88))]),
            block(vec![tx(0xa4, vec![input(0xa1, 0)], None)]),
            block(vec![tx(0xa5, vec![input(0x44, 1)], Some(0x77))]),
        ];
        let keys = WindowKeys::of(&blocks);
        assert_eq!((keys.outpoints.len(), keys.covenants.len()), (5, 2));
        let ctx = TrustContext::load(&conn, Arc::new(TokenAllowlist::from_entries([])), &keys).unwrap();
        assert_eq!(ctx.outpoints, HashSet::from([(Hash32([0x11; 32]), 0)]));
        assert_eq!(ctx.tokens, HashSet::from([Hash32([0x77; 32])]));
        assert_eq!(ctx.flag(&blocks), [true, false, true, true]);
    }

    /// C-M4: the KOB1 magic alone does not make a transaction matter. A payload that does not
    /// decode, or holds only a note, is read from the payload; a sweep (or a placement, an amend)
    /// is read with the signature scripts and matters.
    #[test]
    fn only_a_decoded_placement_amend_or_sweep_payload_matters() {
        use kob_protocol::payload::{encode, Record, MAGIC};
        let conn = crate::indexer::db::open_memory("testnet-10").unwrap();
        let with = |payload: Vec<u8>| {
            let mut t = tx(0xb1, vec![input(0x55, 0)], None);
            t.payload = crate::hex::HexBytes(payload);
            block(vec![t])
        };
        let blocks = vec![
            with([&MAGIC[..], &[0xff; 40]].concat()),
            with(encode(&[Record::Note { text: "hi".into() }]).unwrap()),
            with(encode(&[Record::Sweep { output: 0, input: 0 }]).unwrap()),
        ];
        let ctx = TrustContext::load(&conn, Arc::new(TokenAllowlist::from_entries([])), &WindowKeys::of(&blocks)).unwrap();
        assert_eq!(ctx.flag(&blocks), [false, false, true]);
    }
}
