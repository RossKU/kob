#![allow(dead_code)]
//! Shared helpers of the flow tests: a world (real builders and artifacts) plus an indexer harness; `pair`: the pair-order
//! fixtures.

pub mod pair;

use kob_protocol::artifacts::token_template;
use kob_protocol::build::*;
use kob_protocol::family::Family;
use kob_protocol::state::*;
use kob_protocol::tx::*;
use kobd::hex::Hash32;
use kobd::indexer::db::open_memory;
use kobd::testkit::*;

pub fn conn() -> rusqlite::Connection {
    open_memory("testnet-10").unwrap()
}

/// The owner of the token state a token input reveals (its redeem script, the last push of its signature script), any pinned program.
pub fn revealed_owner(sigscript: &[u8]) -> Option<Vec<u8>> {
    let pushes = kobd::script::parse_pushes(sigscript)?;
    let redeem = pushes.last()?;
    let (id, state) = kobd::model::identify_redeem(redeem).or_else(|| kobd::model::identify_kron_token(redeem))?;
    if !id.is_token() {
        return None;
    }
    Some(TokenState::decode_with(token_template(id), state).ok()?.owner().to_vec())
}

/// A chain world and an indexer following its mock node.
pub struct Ctx {
    pub w: World,
    pub hs: Harness,
}

impl Ctx {
    pub fn new() -> Ctx {
        // `KOB_TEST_LEAN=1`: the same tests over a lean store (closed orders pruned as soon as the reorg window allows)
        let hs = if std::env::var_os("KOB_TEST_LEAN").is_some() { Harness::lean(8) } else { Harness::new(conn(), None) };
        Ctx { w: World::new(), hs }
    }

    pub fn with(hs: Harness) -> Ctx {
        Ctx { w: World::new(), hs }
    }

    /// Include the transactions in one block and let the indexer catch up. Every executor's transaction among them is checked against
    /// its builder's own figures ([`Ctx::check_income`]).
    pub async fn push(&self, txs: &[&SignedTx]) -> Hash32 {
        let b = self.w.include(&self.hs.node, txs);
        self.hs.sync().await;
        for t in txs {
            self.check_income(t);
        }
        b
    }

    /// The indexer's `executor_txs` row of `t` (income, network fee), if `t` is an executor's transaction it indexed.
    pub fn executor_row(&self, t: &SignedTx) -> Option<(Option<i64>, Option<i64>)> {
        use rusqlite::OptionalExtension;
        let g = self.hs.ingest.lock().unwrap();
        g.conn()
            .query_row("SELECT income, miner_fee FROM executor_txs WHERE txid = ?1 ORDER BY id DESC LIMIT 1", [&t.tx.id[..]], |r| {
                Ok((r.get(0)?, r.get(1)?))
            })
            .optional()
            .unwrap()
    }

    /// The indexer's figures of an executor's transaction against what the transaction itself says, independently of how the indexer
    /// tells the orders' outputs: the network fee is every input's value (the builder's own UTXO entries) minus every output; the
    /// income is that fee plus what the sender took out minus what it put in, the sender being the script of the first plain input
    /// (its KAS) and its key (the token UTXOs the indexer proved it holds). Returns whether the income was compared: a transaction
    /// with an input or a token output whose owner is not known either way is not (`None` income is not compared either).
    pub fn check_income(&self, t: &SignedTx) -> bool {
        use rusqlite::OptionalExtension;
        let Some((income, fee)) = self.executor_row(t) else { return false };
        let ins: i128 = t.tx.inputs.iter().map(|i| i128::from(i.utxo.amount)).sum();
        let outs: i128 = t.tx.outputs.iter().map(|o| i128::from(o.value)).sum();
        let id = kobd::hex::encode(&t.tx.id);
        assert_eq!(fee, Some((ins - outs) as i64), "network fee of {id}");
        let Some(income) = income else { return false };
        let g = self.hs.ingest.lock().unwrap();
        let conn = g.conn();
        let payer = t.tx.inputs.iter().find(|i| i.utxo.covenant_id.is_none()).map(|i| i.utxo.script_public_key.clone());
        // a P2PK script `<version 2 bytes> 0x20 <key> 0xac`, as hex
        let key: Option<Vec<u8>> =
            payer.as_deref().filter(|s| s.len() == 4 + 2 + 64 + 2).map(|s| kobd::hex::decode(&s[6..70]).unwrap());
        let holder = |txid: &[u8], idx: u32| -> Option<Vec<u8>> {
            conn.query_row(
                "SELECT i.h FROM token_holdings h JOIN ids i ON i.id = h.owner_id WHERE h.txid = ?1 AND h.idx = ?2",
                rusqlite::params![txid, idx],
                |r| r.get(0),
            )
            .optional()
            .unwrap()
        };
        let tracked = |txid: &[u8], idx: u32| -> bool {
            conn.query_row(
                "SELECT (SELECT COUNT(*) FROM order_utxos WHERE txid = ?1 AND idx = ?2) + (SELECT COUNT(*) FROM token_utxos WHERE txid = ?1 AND idx = ?2)",
                rusqlite::params![txid, idx],
                |r| r.get::<_, i64>(0),
            )
            .unwrap()
                > 0
        };
        let (mut sender_in, mut sender_out) = (0i128, 0i128);
        for i in &t.tx.inputs {
            if tracked(&i.transaction_id, i.index) {
                continue;
            }
            let mine = match i.utxo.covenant_id {
                None => payer.as_deref() == Some(i.utxo.script_public_key.as_str()),
                Some(_) => key.is_some() && (holder(&i.transaction_id, i.index) == key || revealed_owner(&i.signature_script) == key),
            };
            if !mine {
                return false;
            }
            sender_in += i128::from(i.utxo.amount);
        }
        for (j, o) in t.tx.outputs.iter().enumerate() {
            let mine = match &o.covenant {
                None => payer.as_deref() == Some(o.script_public_key.as_str()),
                Some(_) => {
                    let h = holder(&t.tx.id, j as u32);
                    if h.is_none() && !tracked(&t.tx.id, j as u32) {
                        // a covenant output that is no order's and no proven holding: whose is not known here
                        return false;
                    }
                    key.is_some() && h == key
                }
            };
            if mine {
                sender_out += i128::from(o.value);
            }
        }
        assert_eq!(i128::from(income), (ins - outs) + sender_out - sender_in, "income of {id}");
        true
    }

    /// The node's current DAA score (a fresh lock time for the next transaction).
    pub fn daa(&self) -> u64 {
        self.hs.node.tip_daa()
    }
}

/// Batch with the usual taker defaults.
pub fn batch(w: &World, lock: u64, legs: Vec<Leg>) -> Batch {
    Batch {
        lock_time: lock,
        legs,
        updates: vec![],
        taker_tokens: vec![],
        taker: Some(pk(TAKER)),
        taker_token_carrier: CARRIER,
        keep_surplus: vec![],
        keep_carrier: None,
        receivers: vec![],
        payments: vec![],
        funding: vec![w.coin(TAKER, 1000)],
        change: Some(pk(TAKER)),
        records: vec![],
        fee: FeeOptions::default(),
    }
}

/// Index of the output bound to covenant `cov` (an order's continuation).
pub fn find_cov(t: &SignedTx, cov: &Hash32) -> Option<usize> {
    t.tx.outputs.iter().position(|o| o.covenant.as_ref().is_some_and(|c| c.covenant_id == cov.0))
}

/// The custody output holding `amount` token units for `owner` (KCC-20 scheme 0x04).
pub fn find_custody(t: &SignedTx, owner: &Hash32, amount: i64) -> Option<usize> {
    find_custody_for(Family::Kcc20, t, owner, amount)
}

/// [`find_custody`] in a token family (KRON: `id_type` 2).
pub fn find_custody_for(fam: Family, t: &SignedTx, owner: &Hash32, amount: i64) -> Option<usize> {
    let program = if fam == Family::Kron { K3 } else { T3 };
    let spk = spk_to_string(&TokenState::custody(fam, amount, owner.0, EXT).spk_with(token_template(program)));
    t.tx.outputs.iter().position(|o| o.script_public_key == spk)
}

/// A genesis output of the transaction that is not one of `known` covenant ids (an if-done exit).
pub fn find_fresh(t: &SignedTx, known: &[Hash32]) -> Option<(usize, Hash32)> {
    t.tx.outputs.iter().enumerate().find_map(|(i, o)| {
        let c = o.covenant.as_ref()?;
        (!known.iter().any(|k| k.0 == c.covenant_id) && c.covenant_id != TOKEN_COV && c.covenant_id != TOKEN_COV_KRON)
            .then_some((i, Hash32(c.covenant_id)))
    })
}

pub fn amount_left(s: &AnyState) -> i64 {
    s.amount_left().unwrap()
}

/// The API view of an order at the node's current DAA score and a chosen wall clock.
pub fn view_at(c: &Ctx, cov: &Hash32, now_unix: u64) -> kobd::indexer::reads::OrderView {
    let ctx = kobd::indexer::reads::ReadCtx { node_daa: Some(c.daa()), settle_depth_daa: 100, now_unix: Some(now_unix) };
    let g = c.hs.ingest.lock().unwrap();
    kobd::indexer::reads::order(g.conn(), &ctx, cov).unwrap().expect("order")
}

pub fn view(c: &Ctx, cov: &Hash32) -> kobd::indexer::reads::OrderView {
    view_at(c, cov, 1_790_000_000)
}

// ---------------------------------------------------------------------------------------------
// trigger evidence (a stop arms from a plain resting fill in the same transaction)

/// A plain resting order of MAKER_B (side [`SIDE_ASK`]: sells 10 whole tokens; [`SIDE_BID`]: buys up to 10 whole tokens)
/// quoting `price` (sompi per whole token),
/// created and indexed. A batch that fills it carries it as trigger evidence.
pub struct Resting {
    pub create: SignedTx,
    pub fam: Family,
    pub side: i64,
    /// The KCC-20-typed state (the kind created is the family's).
    pub state: AnyState,
}

impl Resting {
    /// The order's covenant id.
    pub fn cov(&self, c: &Ctx) -> Hash32 {
        c.w.cov(&self.create, 0)
    }

    /// DAA score of the resting UTXO (its custody was created with it): its exposure starts here.
    pub fn daa(&self, c: &Ctx) -> u64 {
        c.w.utxo(&self.create, 0).block_daa_score
    }

    /// The plain leg that fills `amount` base units of it.
    pub fn leg(&self, c: &Ctx, amount: i64) -> Leg {
        match &self.state {
            AnyState::KobAsk(a) => {
                let custody = c.w.token_at(&self.create, 1, TokenState::custody(self.fam, 10 * WHOLE, self.cov(c).0, EXT));
                Leg::Ask { order: c.w.order(&self.create, 0, a.clone()), custody, amount, t: None }
            }
            AnyState::KobBid(b) => Leg::Bid { order: c.w.order(&self.create, 0, b.clone()), amount, t: None },
            _ => unreachable!("a resting order is plain"),
        }
    }

    /// Appends the fill of `amount` base units of it to a batch with its taker side (an ask is bought with the batch's funding, a bid is
    /// sold the taker's tokens) and returns its leg index: the `evidence` of the stops the batch arms or triggers.
    pub fn add_to(&self, c: &Ctx, b: &mut Batch, amount: i64) -> usize {
        if self.side == SIDE_BID {
            // one taker token input per token: the evidence's amount is added to the taker's existing (synthetic) coin
            let cov = if self.fam == Family::Kron { TOKEN_COV_KRON } else { TOKEN_COV };
            match b.taker_tokens.iter_mut().find(|t| t.utxo.covenant_id == Some(cov)) {
                Some(t) => t.state = t.state.with_amount(t.state.amount() + amount),
                None => b.taker_tokens.push(c.w.token_for(self.fam, TAKER, amount)),
            }
        }
        b.legs.push(self.leg(c, amount));
        b.legs.len() - 1
    }
}

/// A KCC-20 [`Resting`] order.
pub async fn resting(c: &Ctx, side: i64, price: i64) -> Resting {
    resting_for(c, Family::Kcc20, side, price).await
}

/// [`resting`] in a token family.
pub async fn resting_for(c: &Ctx, fam: Family, side: i64, price: i64) -> Resting {
    kas_order_for(c, fam, side, price, (0, 0, 0)).await
}

/// [`resting_for`] with its time in force, TWAP interval and maximum fill `(tif, interval, max_fill)` (an IOC / FOK taker or a
/// TWAP order; `(0, 0, 0)`: a plain resting order).
pub async fn kas_order_for(c: &Ctx, fam: Family, side: i64, price: i64, (tif, interval, max_fill): (i64, i64, i64)) -> Resting {
    // the fixtures as KCC-20-typed states with the family's token; the kind created is the family's
    let fx = |s: AnyState| if fam == Family::Kron { kron(s).into_family(Family::Kcc20) } else { s };
    let (state, value, tokens) = if side == SIDE_ASK {
        (fx(AnyState::KobAsk(AskState { tif, interval, max_fill, ..ask(MAKER_B, price) })), CARRIER, 10 * WHOLE)
    } else {
        let s = fx(AnyState::KobBid(BidState { tif, interval, max_fill, ..bid(MAKER_B, price) }));
        let AnyState::KobBid(b) = &s else { unreachable!() };
        let escrow = b.escrow(10 * WHOLE, 3).unwrap() as u64;
        (s, escrow, 0)
    };
    let create = c.w.create_tx(state.clone().into_family(fam), value, MAKER_B, tokens);
    c.push(&[&create]).await;
    Resting { create, fam, side, state }
}

/// A lock time at which `r` has rested for at least `min_rest` DAA (the fixtures use 600).
pub fn rested(c: &Ctx, r: &Resting, min_rest: i64) -> u64 {
    (r.daa(c) as i64 + min_rest + 10) as u64
}

/// The arm / trail of `order` next to the evidence leg `evidence` of the batch (the matcher, here the taker's change key,
/// takes the order's `keeperTip`).
pub fn update(order: OrderUtxo<AnyState>, evidence: usize) -> BatchUpdate {
    BatchUpdate { order, evidence, evidence_b: None, take: None }
}
