//! Test support: an in-memory node that speaks the `ChainSource` contract (including reorgs, pruning
//! errors and IBD), fixtures and a chain world built on the `kob-protocol` builders (real protocol v3
//! artifacts, real KCC-20 programs, real covenant ids, transactions validated by the rusty-kaspa
//! script engine), and a deterministic database snapshot for equality checks (incremental vs rebuild).
//!
//! The fixtures count in base units like the protocol's own (`kob-protocol/tests/common`): the fixture token has
//! [`SCALE`] = 1 000 base units per whole token ([`WHOLE`]), prices and tips are sompi per whole token, amounts are whole
//! tokens times [`WHOLE`], and every order's minimum fill is one whole token.
//!
//! Compiled into the library (it is small and has no extra dependencies) so both unit tests and the
//! integration tests under `tests/` can use it. Not part of the supported API.

use crate::hex::{Hash32, HexBytes};
use crate::rpc::types::*;
use crate::rpc::{ChainSource, RpcError};
use kob_protocol::artifacts::{template, token_template, TemplateId};
use kob_protocol::build::*;
use kob_protocol::defaults::tips;
use kob_protocol::family::Family;
use kob_protocol::payload::Record;
use kob_protocol::state::*;
use kob_protocol::tx::*;
use rusqlite::Connection;
use serde_json::json;
use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};
use std::sync::{Arc, Mutex};

/// Deterministic pseudo-random 32-byte value.
pub fn h(tag: &str, n: u64) -> Hash32 {
    let mut hasher = blake3::Hasher::new();
    hasher.update(tag.as_bytes());
    hasher.update(&n.to_le_bytes());
    Hash32(*hasher.finalize().as_bytes())
}

// ---------------------------------------------------------------------------------------------
// fixtures (the shapes of the protocol's own test fixtures)

pub const KAS: u64 = 100_000_000;
pub const CARRIER: u64 = 10 * KAS;
pub const DC: i64 = 10 * KAS as i64;
pub const EC: i64 = 10 * KAS as i64;
/// Base units per whole fixture token (the fixtures' `scale`).
pub const SCALE: i64 = 1_000;
/// One whole fixture token in base units: the fixtures count amounts in whole tokens (`n * WHOLE`).
pub const WHOLE: i64 = SCALE;
/// Priority tip, sompi per whole token.
pub const TIP: i64 = 100_000;
pub const EXPIRY: i64 = 400_000_000;
pub const NO_EXPIRY: i64 = 499_999_999_999;
pub const TOKEN_COV: [u8; 32] = [0x70; 32];
pub const EXT: [u8; 32] = [0xee; 32];
pub const P250: i64 = 250_000_000;
pub const P260: i64 = 260_000_000;
pub const P245: i64 = 245_000_000;

pub const MAKER_A: u8 = 1;
pub const MAKER_B: u8 = 2;
pub const TAKER: u8 = 4;
pub const MATCHER: u8 = 5;
pub const KEEPER: u8 = 6;
pub const FOUNDER: u8 = 7;

pub fn sk(n: u8) -> [u8; 32] {
    [n; 32]
}

pub fn pk(n: u8) -> [u8; 32] {
    pubkey_of(&sk(n)).unwrap()
}

pub fn keys() -> BTreeMap<[u8; 32], [u8; 32]> {
    (1..=40u8).map(|n| (pk(n), sk(n))).collect()
}

pub fn tok_fields(tpl: TemplateId) -> ([u8; 32], i64, i64) {
    if tpl.is_artifact() {
        let t = template(tpl);
        (t.hash, t.prefix.len() as i64, t.suffix.len() as i64)
    } else {
        let t = token_template(tpl);
        (t.hash, t.prefix.len() as i64, t.suffix.len() as i64)
    }
}

pub fn rtip(tpl: TemplateId) -> i64 {
    tips(tpl).unwrap().refund_tip as i64
}

pub fn ktip(tpl: TemplateId) -> i64 {
    tips(tpl).unwrap().keeper_tip as i64
}

/// The token program the fixtures use.
pub const T3: TemplateId = TemplateId::Kcc20Ref;
/// The KRON token program of the KRON fixtures, and their token covenant id.
pub const K3: TemplateId = TemplateId::KronToken2433;
pub const TOKEN_COV_KRON: [u8; 32] = [0x71; 32];

/// Limit ask of 10 whole tokens at `price` (sompi per whole token), minimum fill one whole token.
pub fn ask(maker: u8, price: i64) -> AskState {
    let (hh, p, s) = tok_fields(T3);
    AskState {
        maker: pk(maker),
        token_cov_id: TOKEN_COV,
        token_tpl_hash: hh,
        tpl_prefix_len: p,
        tpl_suffix_len: s,
        scale: SCALE,
        min_fill: WHOLE,
        price,
        tip: TIP,
        tif: 0,
        active_from: 0,
        expiry_daa: EXPIRY,
        refund_tip: rtip(T3),
        interval: 0,
        max_fill: 0,
        slope: 0,
        price_end: 0,
        decay_step: 1_000,
        amount_left: 10 * WHOLE,
    }
}

/// Limit bid at `price` (sompi per whole token), minimum fill one whole token; its quantity is the escrow it is placed with.
pub fn bid(maker: u8, price: i64) -> BidState {
    let (hh, p, s) = tok_fields(T3);
    BidState {
        maker: pk(maker),
        token_cov_id: TOKEN_COV,
        token_tpl_hash: hh,
        tpl_prefix_len: p,
        tpl_suffix_len: s,
        extension_commitment: EXT,
        scale: SCALE,
        min_fill: WHOLE,
        price,
        tip: TIP,
        tif: 0,
        active_from: 0,
        expiry_daa: EXPIRY,
        refund_tip: rtip(T3),
        reserve: 0,
        delivery_carrier: DC,
        interval: 0,
        max_fill: 0,
        slope: 0,
        price_end: 0,
        decay_step: 1_000,
    }
}

/// OCO sell: take-profit 3.00, stop 2.00 (KAS per whole token) with the default 3% band opening over 300 DAA, 10 whole
/// tokens; triggered by evidence of at least one base unit.
pub fn cond_ask(maker: u8) -> CondAskState {
    let (hh, p, s) = tok_fields(T3);
    CondAskState {
        maker: pk(maker),
        token_cov_id: TOKEN_COV,
        token_tpl_hash: hh,
        tpl_prefix_len: p,
        tpl_suffix_len: s,
        scale: SCALE,
        min_fill: WHOLE,
        tip: TIP,
        active_from: 0,
        expiry_daa: EXPIRY,
        refund_tip: rtip(T3),
        tp_price: 300_000_000,
        stop_price: 200_000_000,
        slip_bps: 300,
        trail_step: 0,
        trail_gap: 0,
        trail_wait: 0,
        min_touch: 1,
        min_rest_daa: 600,
        armed: 0,
        band_daa: 300,
        keeper_tip: ktip(T3),
        amount_left: 10 * WHOLE,
        parent: [0; 32],
        rpt_price: 0,
        rpt_until: 0,
    }
}

/// Buy OCO: limit leg 2.00, buy-stop 3.00 (KAS per whole token) with the default 3% band over 300 DAA, 10 whole tokens.
pub fn cond_bid(maker: u8) -> CondBidState {
    let (hh, p, s) = tok_fields(T3);
    CondBidState {
        maker: pk(maker),
        token_cov_id: TOKEN_COV,
        token_tpl_hash: hh,
        tpl_prefix_len: p,
        tpl_suffix_len: s,
        extension_commitment: EXT,
        scale: SCALE,
        min_fill: WHOLE,
        tip: TIP,
        active_from: 0,
        expiry_daa: EXPIRY,
        refund_tip: rtip(T3),
        delivery_carrier: DC,
        tp_price: 200_000_000,
        stop_price: 300_000_000,
        slip_bps: 300,
        trail_step: 0,
        trail_gap: 0,
        trail_wait: 0,
        min_touch: 1,
        min_rest_daa: 600,
        amount_left: 10 * WHOLE,
        armed: 0,
        band_daa: 300,
        keeper_tip: ktip(T3),
        parent: [0; 32],
        rpt_price: 0,
        rpt_pre: 0,
        rpt_until: 0,
    }
}

/// The exit every buy-first fixture commits to: OCO take-profit 3.00 / stop 2.20, GTC.
pub fn ifd_exit(maker: u8) -> CondAskState {
    CondAskState { stop_price: 220_000_000, expiry_daa: NO_EXPIRY, ..cond_ask(maker) }
}

/// Buy-first IFO entry at 2.60 (KAS per whole token) for `n` whole tokens (`n * WHOLE` base units), minimum fill one whole
/// token.
pub fn ifd_bid(maker: u8, n: i64) -> IfdBidState {
    let (hh, p, s) = tok_fields(T3);
    IfdBidState {
        maker: pk(maker),
        token_cov_id: TOKEN_COV,
        token_tpl_hash: hh,
        tpl_prefix_len: p,
        tpl_suffix_len: s,
        extension_commitment: EXT,
        scale: SCALE,
        amount_left: n * WHOLE,
        price: P260,
        tip: TIP,
        active_from: 0,
        expiry_daa: EXPIRY,
        refund_tip: rtip(T3),
        delivery_carrier: DC,
        exit_carrier: EC,
        min_fill: WHOLE,
        entry_stop: 0,
        band_daa: 300,
        min_touch: 1,
        min_rest_daa: 600,
        keeper_tip: ktip(T3),
        armed: 0,
        rpt_amount: 0,
        exit_state: IfdBidState::commit_exit(&ifd_exit(maker)),
    }
}

/// The exit every sell-first fixture commits to: buy-back limit 2.40 / buy-stop 2.80, GTC.
pub fn ifda_exit(maker: u8) -> CondBidState {
    CondBidState { tp_price: 240_000_000, stop_price: 280_000_000, expiry_daa: NO_EXPIRY, ..cond_bid(maker) }
}

/// Sell-first IFO entry at 2.50 (KAS per whole token), 10 whole tokens, prefund 0.5 KAS per whole token, minimum fill one
/// whole token.
pub fn ifd_ask(maker: u8) -> IfdAskState {
    let (hh, p, s) = tok_fields(T3);
    IfdAskState {
        maker: pk(maker),
        token_cov_id: TOKEN_COV,
        token_tpl_hash: hh,
        tpl_prefix_len: p,
        tpl_suffix_len: s,
        scale: SCALE,
        price: P250,
        tip: TIP,
        active_from: 0,
        expiry_daa: EXPIRY,
        refund_tip: rtip(T3),
        prefund: KAS as i64 / 2,
        exit_carrier: EC,
        min_fill: WHOLE,
        entry_stop: 0,
        band_daa: 300,
        min_touch: 1,
        min_rest_daa: 600,
        keeper_tip: ktip(T3),
        armed: 0,
        amount_left: 10 * WHOLE,
        rpt_amount: 0,
        exit_state: IfdAskState::commit_exit(&ifda_exit(maker)),
    }
}

/// A KCC-20 fixture as the same order of the KRON family: the KRON token and program, no extension
/// commitment, the KRON keeper tips, and (if-done entries) the exit re-committed as a KRON exit.
pub fn kron(s: AnyState) -> AnyState {
    let (hh, p, x) = tok_fields(K3);
    let kt = |tip: i64| if tip == ktip(T3) { ktip(K3) } else { tip };
    macro_rules! retoken {
        ($s:expr) => {{
            let mut s = $s;
            s.token_cov_id = TOKEN_COV_KRON;
            s.token_tpl_hash = hh;
            s.tpl_prefix_len = p;
            s.tpl_suffix_len = x;
            // the KCC-20 default tip becomes the KRON default; a deliberate tip (0, huge) is kept
            if s.refund_tip == rtip(T3) {
                s.refund_tip = rtip(K3);
            }
            s
        }};
    }
    match s.into_family(Family::Kcc20) {
        AnyState::KobAsk(a) => AnyState::KobAskKron(retoken!(a)),
        AnyState::KobBid(b) => AnyState::KobBidKron(BidState { extension_commitment: [0; 32], ..retoken!(b) }),
        AnyState::KobCondAsk(c) => AnyState::KobCondAskKron(CondAskState { keeper_tip: kt(c.keeper_tip), ..retoken!(c) }),
        AnyState::KobCondBid(c) => {
            AnyState::KobCondBidKron(CondBidState { extension_commitment: [0; 32], keeper_tip: kt(c.keeper_tip), ..retoken!(c) })
        }
        AnyState::KobIfdBid(i) => {
            let exit = match kron(AnyState::KobCondAsk(i.exit().expect("fixture exit"))) {
                AnyState::KobCondAskKron(e) => e,
                _ => unreachable!(),
            };
            AnyState::KobIfdBidKron(IfdBidState {
                extension_commitment: [0; 32],
                keeper_tip: kt(i.keeper_tip),
                exit_state: IfdBidState::commit_exit(&exit),
                ..retoken!(i)
            })
        }
        AnyState::KobIfdAsk(i) => {
            let exit = match kron(AnyState::KobCondBid(i.exit().expect("fixture exit"))) {
                AnyState::KobCondBidKron(e) => e,
                _ => unreachable!(),
            };
            AnyState::KobIfdAskKron(IfdAskState {
                keeper_tip: kt(i.keeper_tip),
                exit_state: IfdAskState::commit_exit_for(Family::Kron, &exit),
                ..retoken!(i)
            })
        }
        r => r.into_family(Family::Kron),
    }
}

// ---------------------------------------------------------------------------------------------
// wire conversion

fn unhex(s: &str) -> Vec<u8> {
    crate::hex::decode(s).expect("hex")
}

/// A transaction as the node's VSPC v2 reports an accepted one (`High` verbosity).
pub fn wire_tx(t: &TxJson) -> Tx {
    Tx {
        inputs: t
            .inputs
            .iter()
            .map(|i| TxInput {
                previous_outpoint: Outpoint { transaction_id: Hash32(i.transaction_id), index: i.index },
                signature_script: HexBytes(i.signature_script.clone()),
                sequence: Some(i.sequence),
                verbose_data: Some(InputVerbose {
                    utxo_entry: Some(SpentUtxo {
                        amount: i.utxo.amount,
                        script_public_key: HexBytes(unhex(&i.utxo.script_public_key)),
                        block_daa_score: None, // the real node reports null here
                        covenant_id: i.utxo.covenant_id.map(Hash32),
                    }),
                }),
            })
            .collect(),
        outputs: t
            .outputs
            .iter()
            .map(|o| TxOutput {
                value: o.value,
                script_public_key: HexBytes(unhex(&o.script_public_key)),
                covenant: o
                    .covenant
                    .as_ref()
                    .map(|c| CovenantBinding { authorizing_input: c.authorizing_input as u32, covenant_id: Hash32(c.covenant_id) }),
            })
            .collect(),
        payload: HexBytes(t.payload.clone()),
        verbose_data: TxVerbose { transaction_id: Hash32(t.id), block_time: 0 },
        version: Some(t.version),
        lock_time: Some(t.lock_time),
        subnetwork_id: Some(HexBytes(t.subnetwork_id.clone())),
        gas: Some(t.gas),
    }
}

/// A non-KOB transaction (plain P2PK payment, unrelated covenant, ...) for noise in blocks.
pub fn noise_tx(salt: u64, covenant: bool) -> Tx {
    let mut out = TxOutput { value: 1_000, script_public_key: HexBytes(vec![0, 0, 0x20, 2, 0xac]), covenant: None };
    if covenant {
        out.covenant = Some(CovenantBinding { authorizing_input: 0, covenant_id: h("other-covenant", salt) });
    }
    let mut t = Tx {
        inputs: vec![TxInput {
            previous_outpoint: Outpoint { transaction_id: h("noise", salt), index: 0 },
            signature_script: HexBytes(vec![0x41; 65]),
            sequence: Some(0),
            verbose_data: Some(InputVerbose {
                utxo_entry: Some(SpentUtxo {
                    amount: 2_000,
                    script_public_key: HexBytes(vec![0, 0, 0x20, 1, 0xac]),
                    block_daa_score: None,
                    covenant_id: None,
                }),
            }),
        }],
        outputs: vec![out],
        payload: HexBytes(vec![]),
        verbose_data: TxVerbose::default(),
        // a covenant output needs a v1 transaction
        version: Some(covenant as u16),
        lock_time: Some(0),
        subnetwork_id: Some(HexBytes(vec![0; 20])),
        gas: Some(0),
    };
    // its real id (a node other than the primary is checked against it)
    t.verbose_data.transaction_id = crate::rpc::verify::tx_id(&t).expect("every field");
    t
}

// ---------------------------------------------------------------------------------------------
// the chain world: builds, signs, validates and tracks real transactions

/// Builds and signs real v2.6 transactions with the protocol builders and tracks the DAA score of
/// every output, so follow-up requests carry the UTXO daa the chain reports.
pub struct World {
    keys: BTreeMap<[u8; 32], [u8; 32]>,
    counter: std::cell::Cell<u64>,
    daa: std::cell::RefCell<HashMap<[u8; 32], u64>>,
    /// Validate every built transaction with the script engine (default on).
    pub validate: bool,
}

impl Default for World {
    fn default() -> Self {
        World::new()
    }
}

impl World {
    pub fn new() -> World {
        World { keys: keys(), counter: std::cell::Cell::new(0), daa: std::cell::RefCell::new(HashMap::new()), validate: true }
    }

    fn next(&self) -> u64 {
        self.counter.set(self.counter.get() + 1);
        self.counter.get()
    }

    /// A synthetic funding coin of `key`.
    pub fn coin(&self, key: u8, kas: u64) -> KeyUtxo {
        let n = self.next();
        KeyUtxo {
            utxo: Utxo { transaction_id: h("coin", n).0, index: 0, amount: kas * KAS, block_daa_score: 10, covenant_id: None },
            pubkey: pk(key),
        }
    }

    /// A synthetic P2PK-owned KCC-20 token UTXO.
    pub fn token(&self, key: u8, amount: i64) -> TokenUtxo {
        self.token_for(Family::Kcc20, key, amount)
    }

    /// A synthetic key-owned token UTXO of a family (KRON: address presence, `id_type` 3).
    pub fn token_for(&self, fam: Family, key: u8, amount: i64) -> TokenUtxo {
        let n = self.next();
        let cov = if fam == Family::Kron { TOKEN_COV_KRON } else { TOKEN_COV };
        TokenUtxo {
            utxo: Utxo { transaction_id: h("tok", n).0, index: 0, amount: CARRIER, block_daa_score: 10, covenant_id: Some(cov) },
            state: TokenState::user(fam, amount, pk(key), EXT),
        }
    }

    /// Build, sign, (validate) and return a signed transaction.
    pub fn sign(&self, action: &Action) -> SignedTx {
        let built = build(action).unwrap_or_else(|e| panic!("build: {e}"));
        self.finish(&built)
    }

    /// Like [`World::sign`], `None` when the builder refuses the request (random traffic generators).
    pub fn try_sign(&self, action: &Action) -> Option<SignedTx> {
        build(action).ok().map(|b| self.finish(&b))
    }

    pub fn finish(&self, built: &BuiltTx) -> SignedTx {
        let sigs = sign_locally(built, &self.keys).unwrap();
        let signed = finalize(built, &sigs, FinalizeOptions::default()).unwrap_or_else(|e| panic!("finalize: {e}"));
        if self.validate {
            kob_protocol::verify::validate_signed(&signed).unwrap_or_else(|e| panic!("engine rejected the transaction: {e}"));
        }
        signed
    }

    /// Include transactions in a new chain block of `node`; remembers the block's DAA per output.
    pub fn include(&self, node: &MockNode, txs: &[&SignedTx]) -> Hash32 {
        let daa = node.tip_daa() + 1;
        for t in txs {
            self.daa.borrow_mut().insert(t.tx.id, daa);
        }
        node.push_block(txs.iter().map(|t| wire_tx(&t.tx)).collect())
    }

    /// Same, with extra unrelated transactions in the block.
    pub fn include_with(&self, node: &MockNode, txs: &[&SignedTx], extra: Vec<Tx>) -> Hash32 {
        let daa = node.tip_daa() + 1;
        for t in txs {
            self.daa.borrow_mut().insert(t.tx.id, daa);
        }
        let mut all: Vec<Tx> = txs.iter().map(|t| wire_tx(&t.tx)).collect();
        all.extend(extra);
        node.push_block(all)
    }

    /// Remember the DAA of transactions placed in a block by other means (reorg blocks).
    pub fn note_daa(&self, txs: &[&SignedTx], daa: u64) {
        for t in txs {
            self.daa.borrow_mut().insert(t.tx.id, daa);
        }
    }

    /// The output `idx` of a signed transaction as a spendable UTXO.
    pub fn utxo(&self, t: &SignedTx, idx: usize) -> Utxo {
        let o = &t.tx.outputs[idx];
        Utxo {
            transaction_id: t.tx.id,
            index: idx as u32,
            amount: o.value,
            block_daa_score: *self.daa.borrow().get(&t.tx.id).expect("transaction was not included in a block"),
            covenant_id: o.covenant.as_ref().map(|c| c.covenant_id),
        }
    }

    pub fn order<S>(&self, t: &SignedTx, idx: usize, state: S) -> OrderUtxo<S> {
        OrderUtxo { utxo: self.utxo(t, idx), state }
    }

    pub fn token_at(&self, t: &SignedTx, idx: usize, state: impl Into<TokenState>) -> TokenUtxo {
        TokenUtxo { utxo: self.utxo(t, idx), state: state.into() }
    }

    /// The covenant id of output `idx`.
    pub fn cov(&self, t: &SignedTx, idx: usize) -> Hash32 {
        Hash32(t.tx.outputs[idx].covenant.as_ref().expect("covenant output").covenant_id)
    }

    /// A create-order request (the maker's tokens are a fresh synthetic coin).
    pub fn create(&self, order: AnyState, value: u64, maker: u8, token_amount: i64) -> CreateOrder {
        let tokens = if order.holds_tokens() { vec![self.token_for(order.family(), maker, token_amount)] } else { vec![] };
        CreateOrder {
            order,
            value,
            tokens,
            token_carrier: CARRIER,
            funding: vec![self.coin(maker, 1_000)],
            change: None,
            lock_time: 0,
            deadline: None,
            records: vec![],
            fee: FeeOptions::default(),
        }
    }

    /// Sign a plain create of `order` (the maker's tokens are a fresh synthetic coin).
    pub fn create_tx(&self, order: AnyState, value: u64, maker: u8, token_amount: i64) -> SignedTx {
        let req = self.create(order, value, maker, token_amount);
        self.sign(&Action::CreateOrder(req))
    }

    /// A stray: tokens sent to an order's covenant id outside the protocol. Built with the real
    /// `SendTokens` builder and re-targeted at the order id (owner scheme 0x04) before signing.
    pub fn stray(&self, order_cov: Hash32, amount: i64, sender: u8) -> SignedTx {
        self.stray_for(Family::Kcc20, order_cov, amount, sender)
    }

    /// [`World::stray`] for a token family (KRON: the recipient is re-targeted to `id_type` 2).
    pub fn stray_for(&self, fam: Family, order_cov: Hash32, amount: i64, sender: u8) -> SignedTx {
        let tok = self.token_for(fam, sender, amount + 5);
        let coin = self.coin(sender, 100);
        let (cov, program) = if fam == Family::Kron { (TOKEN_COV_KRON, K3) } else { (TOKEN_COV, T3) };
        let req = SendTokens {
            token: TokenRef { covenant_id: cov, program },
            tokens: vec![tok],
            recipients: vec![TokenRecipient { pubkey: pk(30), amount, carrier: CARRIER }],
            token_change: Some(pk(sender)),
            token_change_carrier: CARRIER,
            funding: vec![coin],
            records: vec![],
            change: None,
            fee: FeeOptions::default(),
        };
        let mut built = build(&Action::SendTokens(req)).unwrap();
        let tpl = token_template(program);
        let from = TokenState::user(fam, amount, pk(30), EXT);
        let to = TokenState::custody(fam, amount, order_cov.0, EXT);
        let from_spk = spk_to_string(&from.spk_with(tpl));
        let to_spk = spk_to_string(&to.spk_with(tpl));
        let out = built.tx.outputs.iter_mut().find(|o| o.script_public_key == from_spk).expect("recipient output");
        out.script_public_key = to_spk;
        let mut patched = false;
        for p in built.plans.iter_mut() {
            match (p, &from, &to) {
                (SigPlan::TokenLeader { next_states, .. }, TokenState::Kcc20(f), TokenState::Kcc20(t)) => {
                    for s in next_states.iter_mut().filter(|s| *s == f) {
                        *s = t.clone();
                        patched = true;
                    }
                }
                (SigPlan::KronToken { next_states, .. }, TokenState::Kron(f), TokenState::Kron(t)) => {
                    for s in next_states.iter_mut().filter(|s| *s == f) {
                        *s = t.clone();
                        patched = true;
                    }
                }
                _ => {}
            }
        }
        assert!(patched, "the leader authorises the recipient state");
        self.resign(built, true)
    }

    /// Recompute the sign requests of a built transaction whose outputs or plans were patched, then
    /// sign and finalize it (engine validation optional: a patched genesis is not consensus-valid).
    pub fn resign(&self, mut built: BuiltTx, validate: bool) -> SignedTx {
        let (tx, entries) = built.tx.to_tx().unwrap();
        for r in built.sign.iter_mut() {
            r.sighash = sighash(&tx, &entries, r.input_index);
        }
        built.tx = TxJson::from_tx(&tx, &entries);
        let sigs = sign_locally(&built, &self.keys).unwrap();
        let signed = finalize(&built, &sigs, FinalizeOptions::default()).unwrap_or_else(|e| panic!("finalize: {e}"));
        if validate && self.validate {
            kob_protocol::verify::validate_signed(&signed).unwrap_or_else(|e| panic!("engine rejected the transaction: {e}"));
        }
        signed
    }
}

// ---------------------------------------------------------------------------------------------
// in-memory node

#[derive(Clone)]
pub struct MBlock {
    pub hash: Hash32,
    pub parent: Hash32,
    pub daa: u64,
    pub txs: Vec<Tx>,
    /// A real header: `hash` is its hash (what a node other than the primary is checked against).
    pub header: kaspa_consensus_core::header::Header,
}

/// A real header of a mock chain block: `n` makes it unique.
fn mock_header(parent: Hash32, daa: u64, n: u64) -> kaspa_consensus_core::header::Header {
    use kaspa_consensus_core::header::{CompressedParents, Header};
    use kaspa_consensus_core::Hash;
    let parents = CompressedParents::try_from(vec![vec![Hash::from_bytes(parent.0)]]).expect("one level");
    Header::new_finalized(
        2,
        parents,
        Hash::from_bytes(h("hash-merkle", n).0),
        Hash::from_bytes(h("root", daa).0),
        Hash::from_bytes(h("utxo-commitment", n).0),
        1_790_000_000_000u64 + daa * 100,
        503_498_770,
        n,
        daa,
        kaspa_consensus_core::BlueWorkType::from_u64(daa),
        daa,
        Hash::from_bytes(h("pruning-point", 0).0),
    )
}

impl MBlock {
    fn new(parent: Hash32, daa: u64, txs: Vec<Tx>, n: u64) -> MBlock {
        let header = mock_header(parent, daa, n);
        MBlock { hash: Hash32(header.hash.as_bytes()), parent, daa, txs, header }
    }
}

struct MockState {
    chain: Vec<MBlock>,
    side: HashMap<Hash32, MBlock>,
    forgotten: HashSet<Hash32>,
    retention_floor: usize,
    ibd: bool,
    synced: bool,
    network: String,
    added_cap: usize,
    fail: VecDeque<RpcError>,
    counter: u64,
    utxos: Vec<(String, AddressUtxo)>,
    vspc_calls: usize,
    /// A response carrying more transactions than this times out (a link slower than the chain).
    link_txs: Option<usize>,
    /// `(start hash, minConfirmationCount, timeout scale, answered)` of every VSPC request.
    vspc_log: Vec<(Hash32, Option<u64>, u32, bool)>,
    /// A VSPC answer takes this long to arrive (the round trip and transfer of a remote link, per connection).
    vspc_delay: std::time::Duration,
    /// Plus this long per transaction of the answer (the bandwidth of one connection).
    vspc_delay_per_tx: std::time::Duration,
    /// VSPC requests being answered now, and the most at once.
    vspc_in_flight: usize,
    vspc_max_in_flight: usize,
    /// `getVirtualChainFromBlock` (hashes only) calls.
    hash_calls: usize,
    /// A reorg to apply before the VSPC request of this number: `(call, depth, new blocks)`.
    pending_reorg: Option<(usize, usize, Vec<Vec<Tx>>)>,
    /// Ignore a request's size limit (`VspcRequest::max_bytes`): answer whatever it holds, as a source without the transport
    /// check would. Default: refuse a larger answer with `RpcError::TooLarge`, as the websocket transport does.
    ignore_size_limit: bool,
    /// Answers refused as too large.
    too_large: usize,
    /// `getVirtualChainFromBlock` with transaction ids.
    ids_calls: usize,
}

pub struct MockNode {
    st: Mutex<MockState>,
}

/// DAA score of the anchor block of a default mock node (a realistic scale for the fixtures).
pub const ANCHOR_DAA: u64 = 1_000_000;

impl MockNode {
    pub fn new(network: &str) -> Arc<MockNode> {
        Self::new_at(network, ANCHOR_DAA)
    }

    pub fn new_at(network: &str, anchor_daa: u64) -> Arc<MockNode> {
        let anchor = MBlock::new(Hash32::ZERO, anchor_daa, vec![], 0);
        Arc::new(MockNode {
            st: Mutex::new(MockState {
                chain: vec![anchor],
                side: HashMap::new(),
                forgotten: HashSet::new(),
                retention_floor: 0,
                ibd: false,
                synced: true,
                network: network.to_string(),
                added_cap: 2_480,
                fail: VecDeque::new(),
                counter: 1,
                utxos: vec![],
                vspc_calls: 0,
                link_txs: None,
                vspc_log: vec![],
                vspc_delay: std::time::Duration::ZERO,
                vspc_delay_per_tx: std::time::Duration::ZERO,
                vspc_in_flight: 0,
                vspc_max_in_flight: 0,
                hash_calls: 0,
                pending_reorg: None,
                ignore_size_limit: false,
                too_large: 0,
                ids_calls: 0,
            }),
        })
    }

    fn with<T>(&self, f: impl FnOnce(&mut MockState) -> T) -> T {
        f(&mut self.st.lock().unwrap())
    }

    pub fn anchor(&self) -> Hash32 {
        self.with(|s| s.chain[0].hash)
    }

    pub fn tip(&self) -> Hash32 {
        self.with(|s| s.chain.last().unwrap().hash)
    }

    pub fn tip_daa(&self) -> u64 {
        self.with(|s| s.chain.last().unwrap().daa)
    }

    pub fn chain_hashes(&self) -> Vec<Hash32> {
        self.with(|s| s.chain.iter().map(|b| b.hash).collect())
    }

    pub fn chain(&self) -> Vec<MBlock> {
        self.with(|s| s.chain.clone())
    }

    fn make_block(s: &mut MockState, parent: Hash32, daa: u64, txs: Vec<Tx>) -> MBlock {
        s.counter += 1;
        MBlock::new(parent, daa, txs, s.counter)
    }

    pub fn push_block(&self, txs: Vec<Tx>) -> Hash32 {
        self.with(|s| {
            let last = s.chain.last().unwrap().clone();
            let b = Self::make_block(s, last.hash, last.daa + 1, txs);
            let hash = b.hash;
            s.chain.push(b);
            hash
        })
    }

    /// Push `n` empty chain blocks.
    pub fn push_empty(&self, n: usize) {
        for _ in 0..n {
            self.push_block(vec![]);
        }
    }

    /// Replace the last `depth` chain blocks with new ones (the old ones stay known, off-chain).
    pub fn reorg(&self, depth: usize, new_blocks: Vec<Vec<Tx>>) -> Vec<Hash32> {
        self.with(|s| Self::reorg_in(s, depth, new_blocks))
    }

    /// [`MockNode::reorg`] just before VSPC request number `call` (counted from the node's start, 1-based) is answered:
    /// the requests before it saw the old chain, the ones from it on see the new one.
    pub fn reorg_at_vspc_call(&self, call: usize, depth: usize, new_blocks: Vec<Vec<Tx>>) {
        self.with(|s| s.pending_reorg = Some((call, depth, new_blocks)))
    }

    fn reorg_in(s: &mut MockState, depth: usize, new_blocks: Vec<Vec<Tx>>) -> Vec<Hash32> {
        assert!(depth < s.chain.len());
        for _ in 0..depth {
            let b = s.chain.pop().unwrap();
            s.side.insert(b.hash, b);
        }
        let mut out = vec![];
        for txs in new_blocks {
            let last = s.chain.last().unwrap().clone();
            let b = Self::make_block(s, last.hash, last.daa + 1, txs);
            out.push(b.hash);
            s.chain.push(b);
        }
        out
    }

    pub fn forget(&self, hash: Hash32) {
        self.with(|s| {
            s.forgotten.insert(hash);
        })
    }

    /// Chain blocks with index below `floor` no longer have the retention root on their chain.
    pub fn set_retention_floor(&self, floor: usize) {
        self.with(|s| s.retention_floor = floor)
    }

    pub fn set_ibd(&self, on: bool) {
        self.with(|s| s.ibd = on)
    }

    pub fn set_added_cap(&self, cap: usize) {
        self.with(|s| s.added_cap = cap)
    }

    pub fn inject_error(&self, e: RpcError) {
        self.with(|s| s.fail.push_back(e))
    }

    pub fn set_utxos(&self, u: Vec<(String, AddressUtxo)>) {
        self.with(|s| s.utxos = u)
    }

    pub fn vspc_calls(&self) -> usize {
        self.with(|s| s.vspc_calls)
    }

    /// Answer VSPC requests whatever their size limit (`VspcRequest::max_bytes`), as a source without the transport's check
    /// would; by default a larger answer is refused with `RpcError::TooLarge`, as the websocket transport does.
    pub fn set_ignore_size_limit(&self, ignore: bool) {
        self.with(|s| s.ignore_size_limit = ignore)
    }

    /// VSPC answers refused as larger than the request allowed.
    pub fn too_large(&self) -> usize {
        self.with(|s| s.too_large)
    }

    /// A slow link: a VSPC response with more than `txs` transactions times out (scaled by the request's
    /// timeout scale: a request that may wait twice as long gets twice as much through).
    pub fn set_link_txs(&self, txs: Option<usize>) {
        self.with(|s| s.link_txs = txs)
    }

    /// Every VSPC answer arrives `d` after the request (a remote link: concurrent requests overlap their delays).
    pub fn set_vspc_delay(&self, d: std::time::Duration) {
        self.with(|s| s.vspc_delay = d)
    }

    /// A link of limited bandwidth per connection: an answer arrives `rtt` plus `per_tx` per transaction after the request.
    pub fn set_vspc_link(&self, rtt: std::time::Duration, per_tx: std::time::Duration) {
        self.with(|s| {
            s.vspc_delay = rtt;
            s.vspc_delay_per_tx = per_tx;
        })
    }

    /// The most VSPC requests answered at once so far.
    pub fn vspc_max_in_flight(&self) -> usize {
        self.with(|s| s.vspc_max_in_flight)
    }

    /// `getVirtualChainFromBlock` (hashes only) calls so far.
    pub fn hash_calls(&self) -> usize {
        self.with(|s| s.hash_calls)
    }

    /// The chain from `start` as `getVirtualChainFromBlock` reports it without transaction ids: removed hashes
    /// (tip-first) and up to `added_cap` added hashes, no head stripping.
    fn chain_answer(&self, start: Hash32) -> Result<ChainHashes, RpcError> {
        self.with(|s| {
            s.hash_calls += 1;
            if s.ibd {
                return Err(RpcError::Node("consensus is currently in a transitional ibd state".into()));
            }
            let (removed, anc_idx) = Self::path_from(s, start)?;
            let added = s.chain.iter().skip(anc_idx + 1).take(s.added_cap).map(|b| b.hash).collect();
            Ok(ChainHashes { removed_chain_block_hashes: removed, added_chain_block_hashes: added })
        })
    }

    /// The blocks to remove from `start` (tip-first) and the index of the chain block they lead back to.
    fn path_from(s: &MockState, start: Hash32) -> Result<(Vec<Hash32>, usize), RpcError> {
        let unknown = || RpcError::Node(format!("cannot find header {start}"));
        if s.forgotten.contains(&start) {
            return Err(unknown());
        }
        let no_root = || RpcError::Node("the queried hash does not have retention root on its chain".into());
        let mut removed = vec![];
        let mut cur = start;
        loop {
            if let Some(j) = s.chain.iter().position(|b| b.hash == cur) {
                if j < s.retention_floor {
                    return Err(no_root());
                }
                return Ok((removed, j));
            }
            let b = s.side.get(&cur).ok_or_else(unknown)?;
            removed.push(cur);
            cur = b.parent;
        }
    }

    /// Every VSPC request so far: `(start, minConfirmationCount, timeout scale, answered)`.
    pub fn vspc_log(&self) -> Vec<(Hash32, Option<u64>, u32, bool)> {
        self.with(|s| s.vspc_log.clone())
    }

    /// A chain block as the node reports it: at `Full` with the whole header and every transaction field; at `High` without
    /// the UTXO commitment and the pruning point, and without the transactions' version and lock time (as the real node).
    fn block_json(b: &MBlock, full: bool) -> serde_json::Value {
        let hd = &b.header;
        let txs: Vec<serde_json::Value> = b
            .txs
            .iter()
            .map(|t| {
                let mut v = serde_json::to_value(t).unwrap();
                if !full {
                    v["version"] = serde_json::Value::Null;
                    v["lockTime"] = serde_json::Value::Null;
                }
                v
            })
            .collect();
        json!({
            "chainBlockHeader": {
                "hash": b.hash, "daaScore": hd.daa_score, "blueScore": hd.blue_score, "timestamp": hd.timestamp,
                "bits": hd.bits, "nonce": hd.nonce, "version": hd.version, "parentsByLevel": hd.parents_by_level,
                "hashMerkleRoot": hd.hash_merkle_root, "acceptedIdMerkleRoot": hd.accepted_id_merkle_root, "blueWork": hd.blue_work,
                "pruningPoint": if full { json!(hd.pruning_point) } else { json!(null) },
                "utxoCommitment": if full { json!(hd.utxo_commitment) } else { json!(null) },
            },
            "acceptedTransactions": txs
        })
    }

    fn vspc_raw(&self, start: Hash32, min_conf: Option<u64>, timeout_scale: u32, full: bool) -> Result<RawVspcResponse, RpcError> {
        let r = self.vspc_answer(start, min_conf, timeout_scale, full);
        self.with(|s| s.vspc_log.push((start, min_conf, timeout_scale, r.is_ok())));
        r
    }

    /// The chain blocks a request from `start` answers: the removed ones (tip-first) and the added ones, capped and stripped
    /// at the head for `minConfirmationCount`.
    fn answer_blocks(s: &MockState, start: Hash32, min_conf: Option<u64>) -> Result<(Vec<Hash32>, Vec<MBlock>), RpcError> {
        let (removed, anc_idx) = Self::path_from(s, start)?;
        let mut added: Vec<MBlock> = s.chain.iter().skip(anc_idx + 1).take(s.added_cap).cloned().collect();
        // the node's head stripping (blue score = DAA score here): keep a block while sink - blue > minConf
        if let Some(mc) = min_conf.filter(|m| *m > 0) {
            let sink_blue = s.chain.last().unwrap().daa;
            while added.last().is_some_and(|b| sink_blue.saturating_sub(b.daa) <= mc) {
                added.pop();
            }
        }
        Ok((removed, added))
    }

    /// `getVirtualChainFromBlock` with accepted transaction ids.
    fn ids_answer(&self, start: Hash32, min_conf: Option<u64>) -> Result<ChainIds, RpcError> {
        self.with(|s| {
            s.ids_calls += 1;
            if s.ibd {
                return Err(RpcError::Node("consensus is currently in a transitional ibd state".into()));
            }
            let (removed, added) = Self::answer_blocks(s, start, min_conf)?;
            Ok(ChainIds {
                removed_chain_block_hashes: removed,
                added_chain_block_hashes: added.iter().map(|b| b.hash).collect(),
                accepted_transaction_ids: added
                    .iter()
                    .map(|b| AcceptedIds {
                        accepting_block_hash: b.hash,
                        accepted_transaction_ids: b.txs.iter().map(|t| t.verbose_data.transaction_id).collect(),
                    })
                    .collect(),
            })
        })
    }

    /// `getVirtualChainFromBlock` with transaction ids so far.
    pub fn ids_calls(&self) -> usize {
        self.with(|s| s.ids_calls)
    }

    fn vspc_answer(&self, start: Hash32, min_conf: Option<u64>, timeout_scale: u32, full: bool) -> Result<RawVspcResponse, RpcError> {
        self.with(|s| {
            s.vspc_calls += 1;
            if s.pending_reorg.as_ref().is_some_and(|p| p.0 <= s.vspc_calls) {
                let (_, depth, blocks) = s.pending_reorg.take().expect("checked");
                Self::reorg_in(s, depth, blocks);
            }
            if s.ibd {
                return Err(RpcError::Node("consensus is currently in a transitional ibd state".into()));
            }
            if let Some(e) = s.fail.pop_front() {
                return Err(e);
            }
            let (removed, added) = Self::answer_blocks(s, start, min_conf)?;
            if let Some(limit) = s.link_txs {
                let txs: usize = added.iter().map(|b| b.txs.len()).sum();
                if txs > limit.saturating_mul(timeout_scale.max(1) as usize) {
                    return Err(RpcError::Timeout(std::time::Duration::from_secs(180 * timeout_scale.max(1) as u64)));
                }
            }
            let v = json!({
                "removedChainBlockHashes": removed,
                "addedChainBlockHashes": added.iter().map(|b| b.hash).collect::<Vec<_>>(),
                "chainBlockAcceptedTransactions": added.iter().map(|b| Self::block_json(b, full)).collect::<Vec<_>>()
            });
            let text = v.to_string();
            let mut r: RawVspcResponse = serde_json::from_str(&text).map_err(|e| RpcError::Decode(e.to_string()))?;
            r.wire_bytes = text.len();
            Ok(r)
        })
    }
}

impl ChainSource for MockNode {
    async fn vspc_v2(&self, req: VspcRequest) -> Result<RawVspcResponse, RpcError> {
        let full = match req.data_verbosity_level {
            Verbosity::High => false,
            // a window from a node other than the primary (`rpc::multi`)
            Verbosity::Full => true,
            v => panic!("the indexer must request High (or Full from another node), not {v:?}"),
        };
        let delay = self.with(|s| {
            s.vspc_in_flight += 1;
            s.vspc_max_in_flight = s.vspc_max_in_flight.max(s.vspc_in_flight);
            (s.vspc_delay, s.vspc_delay_per_tx)
        });
        // counted out on drop too: the follower aborts prefetched requests it no longer needs
        struct InFlight<'a>(&'a MockNode);
        impl Drop for InFlight<'_> {
            fn drop(&mut self) {
                self.0.with(|s| s.vspc_in_flight -= 1);
            }
        }
        let _guard = InFlight(self);
        let r = self.vspc_raw(req.start_hash, req.min_confirmation_count, req.timeout_scale, full);
        let txs = r
            .as_ref()
            .map(|r| r.chain_block_accepted_transactions.iter().map(|b| b.accepted_transactions.len()).sum::<usize>())
            .unwrap_or(0);
        let delay = delay.0 + delay.1 * txs as u32;
        if !delay.is_zero() {
            tokio::time::sleep(delay).await;
        }
        // the transport's size limit: a larger answer is refused as its frame header arrives
        match (r, req.max_bytes) {
            (Ok(r), Some(max)) if r.wire_bytes > max && !self.with(|s| s.ignore_size_limit) => {
                self.with(|s| s.too_large += 1);
                Err(RpcError::TooLarge { size: r.wire_bytes, limit: max })
            }
            (r, _) => r,
        }
    }

    async fn chain_hashes(&self, start: Hash32) -> Result<ChainHashes, RpcError> {
        self.chain_answer(start)
    }

    async fn chain_with_ids(&self, start: Hash32, min_confirmations: Option<u64>) -> Result<ChainIds, RpcError> {
        self.ids_answer(start, min_confirmations)
    }

    async fn sink_blue_score(&self) -> Result<u64, RpcError> {
        self.with(|s| Ok(s.chain.last().unwrap().daa))
    }

    async fn block_blue_score(&self, hash: Hash32) -> Result<Option<u64>, RpcError> {
        self.with(|s| {
            if s.forgotten.contains(&hash) {
                return Ok(None);
            }
            Ok(s.chain.iter().find(|b| b.hash == hash).or_else(|| s.side.get(&hash)).map(|b| b.daa))
        })
    }

    async fn dag_info(&self) -> Result<DagInfo, RpcError> {
        self.with(|s| {
            Ok(DagInfo {
                network: s.network.clone(),
                sink: s.chain.last().unwrap().hash,
                pruning_point_hash: s.chain[s.retention_floor.min(s.chain.len() - 1)].hash,
                virtual_daa_score: s.chain.last().unwrap().daa,
            })
        })
    }

    async fn server_info(&self) -> Result<ServerInfo, RpcError> {
        self.with(|s| {
            Ok(ServerInfo {
                server_version: "2.1.0".into(),
                network_id: s.network.clone(),
                is_synced: s.synced,
                has_utxo_index: true,
                virtual_daa_score: s.chain.last().unwrap().daa,
            })
        })
    }

    async fn utxos_by_addresses(&self, addresses: &[String]) -> Result<Vec<AddressUtxo>, RpcError> {
        self.with(|s| Ok(s.utxos.iter().filter(|(a, _)| addresses.contains(a)).map(|(_, u)| u.clone()).collect()))
    }

    async fn block_exists(&self, hash: Hash32) -> Result<bool, RpcError> {
        self.with(|s| Ok(!s.forgotten.contains(&hash) && (s.chain.iter().any(|b| b.hash == hash) || s.side.contains_key(&hash))))
    }
}

// ---------------------------------------------------------------------------------------------
// snapshot

fn blob_hex(b: Option<Vec<u8>>) -> String {
    b.map(|b| crate::hex::encode(&b)).unwrap_or_else(|| "-".into())
}

/// A deterministic dump of every derived table with block sequence numbers mapped to block hashes,
/// so two databases that reached the same state through different histories compare equal. Requires
/// the reorg window to cover the chain under test (no pruned block rows).
pub fn snapshot(conn: &Connection) -> String {
    let mut out = String::new();
    let seq_to_hash: HashMap<i64, String> = {
        let mut st = conn.prepare("SELECT seq, hash FROM blocks").unwrap();
        st.query_map([], |r| Ok((r.get::<_, i64>(0)?, crate::hex::encode(&r.get::<_, Vec<u8>>(1)?))))
            .unwrap()
            .map(|r| r.unwrap())
            .collect()
    };
    let bh = |seq: Option<i64>| -> String {
        match seq {
            None => "-".into(),
            Some(0) => "import".into(),
            Some(s) => seq_to_hash.get(&s).cloned().unwrap_or_else(|| format!("MISSING-BLOCK-{s}")),
        }
    };
    let dump = |sql: &str, cols: usize, out: &mut String, name: &str, block_cols: &[usize]| {
        let mut st = conn.prepare(sql).unwrap();
        let rows = st
            .query_map([], |r| {
                let mut parts = Vec::with_capacity(cols);
                for i in 0..cols {
                    let v: rusqlite::types::Value = r.get(i)?;
                    let s = match v {
                        rusqlite::types::Value::Null => "-".to_string(),
                        rusqlite::types::Value::Integer(n) if block_cols.contains(&i) => bh(Some(n)),
                        rusqlite::types::Value::Integer(n) => n.to_string(),
                        rusqlite::types::Value::Real(f) => f.to_string(),
                        rusqlite::types::Value::Text(t) => t,
                        rusqlite::types::Value::Blob(b) => blob_hex(Some(b)),
                    };
                    parts.push(s);
                }
                Ok(parts.join(" "))
            })
            .unwrap();
        for r in rows {
            out.push_str(&format!("{name} {}\n", r.unwrap()));
        }
    };
    dump(
        "SELECT covenant_id, contract, template_hash, side, maker, token_cov_id, token_tpl_hash, ext_commit, scale, min_fill, price, tip, tif, expiry_daa, active_from, in_book, budget_rate, reserve, initial_amount, deadline, genesis_state, genesis_txid, genesis_out, genesis_block, genesis_daa, parent, listed, unlisted_reason, origin, quote_cov_id FROM orders ORDER BY covenant_id",
        30,
        &mut out,
        "order",
        &[23],
    );
    dump(
        "SELECT txid, idx, covenant_id, value, spk, state, created_block, created_daa, spent_block, spent_txid, spent_entry, spent_amount FROM order_utxos ORDER BY txid, idx",
        12,
        &mut out,
        "utxo",
        &[6, 8],
    );
    dump(
        "SELECT covenant_id, block_seq, daa, ts, txid, tx_pos, kind, token_cov_id, side, amount, price, payout, closes, detail FROM order_events ORDER BY id",
        14,
        &mut out,
        "event",
        &[1],
    );
    dump(
        "SELECT covenant_id, status, filled_amount, remaining_amount, amount_exact, cur_txid, cur_idx, cur_value, state_known, last_block, last_daa FROM order_state ORDER BY covenant_id",
        11,
        &mut out,
        "state",
        &[9],
    );
    dump(
        "SELECT txid, idx, token_cov_id, owner, amount, value, role, created_block, created_daa, spent_block, spent_txid FROM token_utxos ORDER BY txid, idx",
        11,
        &mut out,
        "token_utxo",
        &[7, 9],
    );
    dump(
        "SELECT txid, idx, token_cov_id, program, family, owner, owner_kind, amount, value, state, role, created_block, created_daa, spent_block, spent_txid FROM token_holdings ORDER BY txid, idx",
        15,
        &mut out,
        "holding",
        &[11, 13],
    );
    dump(
        "SELECT token_cov_id, tpl_hash, ext_commit, kind, txid, block_seq, daa FROM token_events ORDER BY id",
        7,
        &mut out,
        "token_event",
        &[5],
    );
    dump("SELECT block_seq, txid, reason FROM rejects ORDER BY id", 3, &mut out, "reject", &[0]);
    dump(
        "SELECT covenant_id, block_seq, price, tip, tif, expiry_daa, active_from, deadline, listed, unlisted_reason FROM order_amends ORDER BY id",
        10,
        &mut out,
        "amend",
        &[1],
    );
    out
}

// ---------------------------------------------------------------------------------------------
// harness

use crate::config::StartMode;
use crate::indexer::follower::{Follower, FollowerConfig, StepOutcome};
use crate::indexer::ingest::{Ingest, IngestConfig};
use crate::indexer::processor::Processor;
use crate::indexer::recordlog::RecordLog;
use crate::indexer::status::{HealthState, IndexEvent};
use crate::tokens::{ListingRules, TokenAllowlist, TokenEntry};
use tokio::sync::broadcast;

/// Allowlist of the fixtures' token.
pub fn allowlist() -> TokenAllowlist {
    TokenAllowlist::from_entries([
        TokenEntry {
            ticker: "TST".into(),
            covenant_id: Hash32(TOKEN_COV),
            template_hash: None,
            extension_commitment: None,
            decimals: None,
            family: None,
            enabled: true,
            official: false,
            template_id: None,
            powers: vec![],
        },
        TokenEntry {
            ticker: "KTST".into(),
            covenant_id: Hash32(TOKEN_COV_KRON),
            template_hash: None,
            extension_commitment: None,
            decimals: None,
            family: None,
            enabled: true,
            official: false,
            template_id: None,
            powers: vec![],
        },
    ])
}

pub fn processor() -> Processor {
    Processor {
        tokens: Arc::new(allowlist()),
        // relaxed rules: the fixtures use round numbers and long expiries
        rules: ListingRules { min_order_value_sompi: 1, max_expiry_span_daa: 1 << 40, ..ListingRules::default() },
    }
}

/// A window that covers any test chain (no block row is pruned).
pub fn wide_window() -> IngestConfig {
    IngestConfig { reorg_window_daa: u64::MAX / 4, checkpoint_daa: u64::MAX / 4, ..IngestConfig::default() }
}

pub struct Harness {
    pub node: Arc<MockNode>,
    pub ingest: Arc<Mutex<Ingest>>,
    pub follower: Follower<MockNode>,
    pub health: Arc<HealthState>,
    pub events: broadcast::Sender<Arc<IndexEvent>>,
}

impl Harness {
    pub fn new(conn: Connection, log: Option<RecordLog>) -> Harness {
        Self::with_node(MockNode::new("testnet-10"), conn, log, wide_window())
    }

    pub fn with_node(node: Arc<MockNode>, conn: Connection, log: Option<RecordLog>, cfg: IngestConfig) -> Harness {
        Self::with_processor(node, conn, log, cfg, processor())
    }

    /// [`Harness::with_node`] with a custom processor (another allowlist or listing rules).
    pub fn with_processor(
        node: Arc<MockNode>,
        conn: Connection,
        log: Option<RecordLog>,
        cfg: IngestConfig,
        proc: Processor,
    ) -> Harness {
        Self::with_follower(node, conn, log, cfg, proc, |_| {})
    }

    /// [`Harness::with_processor`] with the follower's configuration adjusted (parallel fetch, windows).
    pub fn with_follower(
        node: Arc<MockNode>,
        conn: Connection,
        log: Option<RecordLog>,
        cfg: IngestConfig,
        proc: Processor,
        adjust: impl FnOnce(&mut FollowerConfig),
    ) -> Harness {
        let ingest = Arc::new(Mutex::new(Ingest::new(conn, proc, log).with_config(cfg)));
        let health = Arc::new(HealthState::new("testnet-10"));
        let (events, _) = broadcast::channel(64);
        let fcfg = FollowerConfig {
            network: "testnet-10".into(),
            start: StartMode::Hash(node.anchor()),
            poll_interval: std::time::Duration::from_millis(1),
            backoff: std::time::Duration::from_millis(1),
            max_backoff: std::time::Duration::from_millis(5),
            min_confirmations: None,
            max_walkback_blocks: 1_000,
            status_refresh: std::time::Duration::from_millis(0),
            caught_up_daa: 100,
            batch_initial_blue: 600,
            batch_target: std::time::Duration::from_secs(30),
            fetch_parallel: 1,
            prefetch_max_bytes: 256 << 20,
            prefetch_min_lag_blue: 1_200,
            prefetch_initial_blocks: 64,
        };
        let mut fcfg = fcfg;
        adjust(&mut fcfg);
        let follower = Follower::new(node.clone(), ingest.clone(), fcfg, health.clone(), events.clone());
        Harness { node, ingest, follower, health, events }
    }

    /// Step until the follower reports idle (or stops making progress); returns every outcome.
    pub async fn sync(&self) -> Vec<StepOutcome> {
        let mut out = vec![];
        for _ in 0..500 {
            let o = self.follower.step().await;
            let stop = matches!(o, StepOutcome::Idle | StepOutcome::Gap(_) | StepOutcome::Halted(_) | StepOutcome::Retry(_));
            out.push(o);
            if stop {
                return out;
            }
        }
        panic!("follower did not settle: {out:?}");
    }

    /// [`Harness::sync`] through transient failures (timeouts of a slow link): steps until the follower is idle at
    /// the node's sink; panics on a gap or after `max_steps`.
    pub async fn sync_through_retries(&self, max_steps: usize) -> Vec<StepOutcome> {
        let mut out = vec![];
        for _ in 0..max_steps {
            let o = self.follower.step().await;
            let done = matches!(o, StepOutcome::Idle);
            assert!(!matches!(o, StepOutcome::Gap(_)), "gap: {o:?}");
            out.push(o);
            if done {
                return out;
            }
        }
        panic!("follower did not reach the sink in {max_steps} steps: {:?}", &out[out.len().saturating_sub(20)..]);
    }

    pub fn snapshot(&self) -> String {
        snapshot(self.ingest.lock().unwrap().conn())
    }

    pub fn query<T: rusqlite::types::FromSql>(&self, sql: &str, p: impl rusqlite::Params) -> T {
        self.ingest.lock().unwrap().conn().query_row(sql, p, |r| r.get(0)).unwrap()
    }

    pub fn status(&self, cov: &Hash32) -> String {
        self.query("SELECT status FROM order_state WHERE covenant_id = ?1", [&cov.0[..]])
    }

    /// The proven state of an order's current UTXO.
    pub fn tip_state(&self, cov: &Hash32) -> Option<AnyState> {
        let g = self.ingest.lock().unwrap();
        let (contract, state): (String, Option<Vec<u8>>) = g
            .conn()
            .query_row(
                "SELECT o.contract, u.state FROM orders o JOIN order_state s ON s.covenant_id = o.covenant_id \
                 LEFT JOIN order_utxos u ON u.txid = s.cur_txid AND u.idx = s.cur_idx WHERE o.covenant_id = ?1",
                [&cov.0[..]],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .ok()?;
        crate::indexer::reads::tip_state(&contract, state)
    }

    /// Rows of `order_events.kind` of an order, oldest first.
    pub fn event_kinds(&self, cov: &Hash32) -> Vec<String> {
        let g = self.ingest.lock().unwrap();
        let mut st = g.conn().prepare("SELECT kind FROM order_events WHERE covenant_id = ?1 ORDER BY id").unwrap();
        st.query_map([&cov.0[..]], |r| r.get::<_, String>(0)).unwrap().map(|r| r.unwrap()).collect()
    }

    /// The `detail` JSON of an order's latest event of `kind` (e.g. `detail["evidence"]` of an arm).
    pub fn event_detail(&self, cov: &Hash32, kind: &str) -> serde_json::Value {
        let d: String = self.query(
            "SELECT detail FROM order_events WHERE covenant_id = ?1 AND kind = ?2 ORDER BY id DESC LIMIT 1",
            rusqlite::params![&cov.0[..], kind],
        );
        serde_json::from_str(&d).unwrap()
    }
}

/// Snapshot of a database that applied `chain` (blocks after the anchor) in one go, from scratch.
pub fn fresh_snapshot(chain: &[MBlock]) -> String {
    let conn = crate::indexer::db::open_memory("testnet-10").unwrap();
    let mut ing = Ingest::new(conn, processor(), None).with_config(wide_window());
    let anchor = chain[0].hash;
    ing.init_cursor(&crate::indexer::ingest::Cursor { hash: anchor, daa: chain[0].daa }).unwrap();
    let added = chain
        .iter()
        .skip(1)
        .map(|b| AddedBlock {
            header: ChainBlockHeader {
                hash: b.hash,
                daa_score: b.daa,
                blue_score: b.daa,
                timestamp: 1_790_000_000_000u64 + b.daa * 100,
                ..ChainBlockHeader::default()
            },
            txs: b.txs.clone(),
        })
        .collect();
    ing.apply_batch(anchor, &VspcBatch { removed: vec![], added, wire_bytes: 0 }).unwrap();
    snapshot(ing.conn())
}

/// Convenience for tests: a record with a client note (a payload the indexer must ignore).
pub fn note_record() -> Record {
    Record::Note { text: "kob-test/1".into() }
}
