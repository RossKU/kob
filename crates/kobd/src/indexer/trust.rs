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
//! - Its payload is a KOB1 payload with a placement record whose output carries the record's
//!   order script under a covenant binding (`record::places_order`). Any other KOB1 payload is
//!   read from the payload alone. An amend or a sweep acts only next to a tracked order input,
//!   which makes it matter by the next rule.
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

    /// The user lanes of `blocks` the chain proof proves (R9-K): those that hold a transaction that matters to KOB,
    /// which on a user lane is a spend of an output the store tracks. The native lane is always proven, and a user
    /// lane with nothing tracked on it need not be: a lane the window does not show is left to the node anyway, so
    /// proving one whose transactions KOB never reads would add no safety, only the cost of its two lane proofs.
    pub fn user_lanes(&self, blocks: &[RawChainBlock]) -> HashSet<[u8; 20]> {
        let mut out = HashSet::new();
        for (b, ks) in blocks.iter().zip(self.matters(blocks)) {
            for k in ks {
                let t = &b.accepted_transactions[k];
                if super::record::user_lane(t).is_some() {
                    if let Some(s) = t.subnetwork_id.as_ref().and_then(|s| <[u8; 20]>::try_from(s.0.as_slice()).ok()) {
                        out.insert(s);
                    }
                }
            }
        }
        out
    }

    /// Per chain block of `blocks`: the positions of its transactions that can matter to KOB.
    pub fn matters(&self, blocks: &[RawChainBlock]) -> Vec<Vec<usize>> {
        let mut created: HashSet<(Hash32, u32)> = HashSet::new();
        blocks
            .iter()
            .map(|b| {
                let mut any = vec![];
                for (k, t) in b.accepted_transactions.iter().enumerate() {
                    let spends = t.inputs.iter().any(|i| {
                        let op = (i.previous_outpoint.transaction_id, i.previous_outpoint.index);
                        self.outpoints.contains(&op) || created.contains(&op)
                    });
                    // A user-lane transaction only ends what the store tracks: it matters by a spend alone, and
                    // nothing it creates is tracked, so its outputs never make a later transaction matter (R9-K).
                    let other_lane = super::record::user_lane(t).is_some();
                    let matters = if other_lane {
                        spends
                    } else {
                        places(t)
                            || spends
                            || t.outputs
                                .iter()
                                .filter_map(|o| o.covenant)
                                .any(|c| self.tokens.contains(&c.covenant_id) || self.allowlist.get(&c.covenant_id).is_some())
                    };
                    if matters {
                        any.push(k);
                        if !other_lane {
                            let id = t.verbose_data.transaction_id;
                            created.extend((0..t.outputs.len() as u32).map(|k| (id, k)));
                        }
                    }
                }
                any
            })
            .collect()
    }
}

/// Whether `t` holds a KOB1 placement record that one of its outputs carries
/// ([`super::record::places_order`]).
///
/// A placement is read with the transaction's signature scripts (the leaders of the custody
/// outputs). Any other KOB1 payload is read from the payload alone, which the transaction id
/// covers, so it needs no merged block fetched: spam with the magic, or with a record that decodes
/// (a 12-byte sweep, an order record no output carries), costs the follower nothing more than any
/// other transaction (C-M4, R2C-M2). An amend or a sweep is read only next to a tracked order
/// input, which matters by its outpoint.
fn places(t: &crate::rpc::types::Tx) -> bool {
    use kob_protocol::payload::{decode, MAGIC};
    let payload = &t.payload.0;
    // a coinbase is never read (R9C1-M1); a KOB1 record is read on the native lane only (R9-K)
    !super::record::is_coinbase(t)
        && super::record::user_lane(t).is_none()
        && payload.starts_with(MAGIC)
        && decode(payload).ok().flatten().is_some_and(|p| p.records.iter().any(|r| super::record::places_order(t, r)))
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

    /// A transaction on user lane `n` (KIP-21: 4 namespace bytes, then 16 zero bytes).
    fn on_lane(mut t: Tx, n: u32) -> Tx {
        let mut s = [0u8; 20];
        s[0] = 2;
        s[1..4].copy_from_slice(&n.to_be_bytes()[1..]);
        t.subnetwork_id = Some(crate::hex::HexBytes(s.to_vec()));
        t
    }

    /// A store tracking the order outputs `0x10 + i : 0` for `i < n` (unspent).
    fn tracking(n: u8) -> rusqlite::Connection {
        let conn = crate::indexer::db::open_memory("testnet-10").unwrap();
        conn.execute_batch(
            "INSERT INTO ids (h) VALUES (x'0000000000000000000000000000000000000000000000000000000000000000'); \
             INSERT INTO orders (covenant_id, contract, template_id, family, side, token_id, in_book, genesis_state, genesis_block, genesis_daa, listed) \
             VALUES (x'01', 'KobAsk', 1, 1, 1, 1, 1, x'00', 1, 1, 1);",
        )
        .unwrap();
        for i in 0..n {
            conn.execute(
                "INSERT INTO order_utxos (txid, idx, order_id, value, spk, created_block, created_daa) VALUES (?1, 0, 1, 1, x'00', 1, 1)",
                [vec![0x10 + i; 32]],
            )
            .unwrap();
        }
        conn
    }

    fn lanes_of(conn: &rusqlite::Connection, blocks: &[RawChainBlock]) -> HashSet<[u8; 20]> {
        let ctx = TrustContext::load(conn, Arc::new(TokenAllowlist::from_entries([])), &WindowKeys::of(blocks)).unwrap();
        ctx.user_lanes(blocks)
    }

    /// R9-K: a flood of user-lane transactions that spend nothing the store tracks proves no user lane, and neither
    /// a KOB1 placement nor a token output makes a user-lane transaction matter: KOB state is created on the native
    /// lane only. The lane proofs of R9C1-H1 are thus no longer the attacker's to cause by junk.
    #[test]
    fn a_junk_user_lane_flood_proves_no_lane() {
        let conn = tracking(1);
        let txs: Vec<Tx> = (0..4_100u32).map(|n| on_lane(tx((n % 200) as u8 + 0x30, vec![input(0x99, n)], Some(0x77)), n)).collect();
        let blocks = vec![block(txs)];
        assert!(lanes_of(&conn, &blocks).is_empty());
        let ctx = TrustContext::load(&conn, Arc::new(TokenAllowlist::from_entries([])), &WindowKeys::of(&blocks)).unwrap();
        assert_eq!(ctx.flag(&blocks), [false], "nothing to fetch a merged block for either");
    }

    /// R9-K: a chain of user-lane transactions, each on a lane of its own and each spending the output of the one
    /// before, the first spending a tracked output, proves the first lane alone: what a user-lane transaction creates
    /// is never tracked, so the second link spends nothing tracked. Each further link would otherwise have cost two lane
    /// proofs for a fee.
    #[test]
    fn a_chained_user_lane_flood_proves_only_its_first_link() {
        let conn = tracking(1);
        let mut txs = vec![on_lane(tx(0xc0, vec![input(0x10, 0)], Some(0x01)), 1)];
        for k in 1..1_000u32 {
            let prev = 0xc0u8.wrapping_add((k - 1) as u8);
            let mut t = on_lane(tx(0xc0u8.wrapping_add(k as u8), vec![input(prev, 0)], Some(0x01)), 1 + k);
            // distinct ids beyond 256 links
            t.verbose_data.transaction_id.0[1] = (k >> 8) as u8;
            t.inputs[0].previous_outpoint.transaction_id.0[1] = ((k - 1) >> 8) as u8;
            txs.push(t);
        }
        let blocks = vec![block(txs)];
        let lanes = lanes_of(&conn, &blocks);
        assert_eq!(lanes.len(), 1, "{lanes:?}");
        let mut first = [0u8; 20];
        first[..4].copy_from_slice(&[2, 0, 0, 1]);
        assert!(lanes.contains(&first));
    }

    /// R9-K, the expensive variant: each link spends a tracked output of its own, so each lane is proven, two lane
    /// proofs a link. The work is then bounded by the tracked outputs the attacker made on the native lane beforehand
    /// (each a covenant output costing `4·C/v` grams of storage mass, `docs/ops/api-bounds.md`), and each link ends one
    /// of them: n tracked outputs make at most n proven lanes, once.
    #[test]
    fn user_lanes_that_each_end_a_tracked_output_are_each_proven_once() {
        let conn = tracking(50);
        let txs: Vec<Tx> = (0..50u8).map(|i| on_lane(tx(0x60 + i, vec![input(0x10 + i, 0)], None), 100 + i as u32)).collect();
        let blocks = vec![block(txs)];
        assert_eq!(lanes_of(&conn, &blocks).len(), 50);
        // the same tracked outputs again, spent on fresh lanes in a later window: nothing left to end, nothing proven
        conn.execute("UPDATE order_utxos SET spent_block = 2", []).unwrap();
        let again: Vec<Tx> = (0..50u8).map(|i| on_lane(tx(0xb0 + i, vec![input(0x10 + i, 0)], None), 200 + i as u32)).collect();
        assert!(lanes_of(&conn, &[block(again)]).is_empty());
    }

    /// C-M4, R2C-M2: neither the KOB1 magic nor a record that decodes makes a transaction matter.
    /// A payload that does not decode, a note, a sweep (no tracked input), an order record on a
    /// transaction whose output does not carry its script, or carries it without a covenant
    /// binding, are read from the payload alone; a placement its output carries matters.
    #[test]
    fn only_a_placement_its_output_carries_matters() {
        use kob_protocol::payload::{encode, Record, MAGIC};
        use kob_protocol::state::AnyState;
        let conn = crate::indexer::db::open_memory("testnet-10").unwrap();
        let with = |payload: Vec<u8>| {
            let mut t = tx(0xb1, vec![input(0x55, 0)], None);
            t.payload = crate::hex::HexBytes(payload);
            block(vec![t])
        };
        let b = crate::testkit::bid(1, 250_000_000);
        let state = AnyState::KobBid(b.clone());
        let id = state.template_id();
        let order = Record::Order {
            output: 0,
            family: state.family().code(),
            template: id,
            template_hash: kob_protocol::artifacts::template(id).hash,
            state: state.encode(),
            custody: None,
            prefund: None,
            deadline: None,
        };
        let carried = |cov: Option<u8>, s: &AnyState| {
            let spk = s.spk();
            let mut t = tx(0xb2, vec![input(0x56, 0)], cov);
            t.outputs[0].script_public_key = crate::hex::HexBytes([&spk.version().to_be_bytes()[..], spk.script()].concat());
            t.payload = crate::hex::HexBytes(encode(std::slice::from_ref(&order)).unwrap());
            block(vec![t])
        };
        let other = AnyState::KobBid(kob_protocol::state::BidState { price: b.price + 1, ..b });
        let blocks = vec![
            with([&MAGIC[..], &[0xff; 40]].concat()),
            with(encode(&[Record::Note { text: "hi".into() }]).unwrap()),
            with(encode(&[Record::Sweep { output: 0, input: 0 }]).unwrap()),
            with(encode(std::slice::from_ref(&order)).unwrap()),
            carried(None, &state),
            carried(Some(0x99), &other),
            carried(Some(0x99), &state),
        ];
        let ctx = TrustContext::load(&conn, Arc::new(TokenAllowlist::from_entries([])), &WindowKeys::of(&blocks)).unwrap();
        assert_eq!(ctx.flag(&blocks), [false, false, false, false, false, false, true]);
    }
}
