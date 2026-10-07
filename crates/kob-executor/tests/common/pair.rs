//! Pair-order fixtures of the indexer flow tests (`KobPair`, `KobCondPair`, `KobIfdPair`): a pair A/B of the world's two
//! tokens (A of family `fa`, B of family `fb`: `TOKEN_COV` / `T3` for KCC-20, `TOKEN_COV_KRON` / `K3` for KRON), both of
//! scale `SCALE`, prices in B base units per whole A (`RATE` = one whole B per whole A). Orders are created with the real
//! `CreateOrder` builder (custodies of either token drawn from synthetic maker coins) and filled with `build_batch` legs.

use super::*;
use kob_executor::hex::Hash32;
use kob_executor::testkit::*;
use kob_protocol::artifacts::TemplateId;
use kob_protocol::build::*;
use kob_protocol::family::Family;
use kob_protocol::state::*;
use kob_protocol::tx::*;

/// KAS the pair orders put on each token delivery (prefunded on the order UTXO).
pub const PDC: i64 = 2 * KAS as i64;
/// A pair order's value: its own carrier plus four prefunded deliveries.
pub const PV: u64 = CARRIER + 4 * PDC as u64;
/// B base units per whole A: one whole B per whole A.
pub const RATE: i64 = WHOLE;
/// Entry and exit carriers of the pair entries.
pub const PEC: i64 = 6 * KAS as i64;
/// KAS tip of the plain pair orders, sompi per whole A.
pub const PTIP: i64 = 10_000;

/// The two tokens of a pair: A of family `fa`, B of family `fb` (the world's KCC-20 and KRON tokens).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Pair {
    pub fa: Family,
    pub fb: Family,
}

/// A = the KCC-20 token, B = the KRON token.
pub const AB: Pair = Pair { fa: Family::Kcc20, fb: Family::Kron };
/// A = the KRON token, B = the KCC-20 token.
pub const BA: Pair = Pair { fa: Family::Kron, fb: Family::Kcc20 };

/// Program and covenant id of the world's token of a family.
pub fn prog(f: Family) -> (TemplateId, [u8; 32]) {
    if f == Family::Kron {
        (K3, TOKEN_COV_KRON)
    } else {
        (T3, TOKEN_COV)
    }
}

/// The extension commitment of the world's token of a family (KRON: none).
pub fn ext_of(f: Family) -> [u8; 32] {
    if f == Family::Kron {
        [0; 32]
    } else {
        EXT
    }
}

impl Pair {
    pub fn a(&self) -> Hash32 {
        Hash32(prog(self.fa).1)
    }
    pub fn b(&self) -> Hash32 {
        Hash32(prog(self.fb).1)
    }
    /// The family of a token of the pair.
    pub fn fam_of(&self, token: [u8; 32]) -> Family {
        if token == prog(self.fa).1 {
            self.fa
        } else {
            self.fb
        }
    }
    fn tok(&self, a: bool) -> ([u8; 32], [u8; 32], i64, i64, i64) {
        let f = if a { self.fa } else { self.fb };
        let (p, c) = prog(f);
        let (h, pre, suf) = tok_fields(p);
        (c, h, pre, suf, f.code() as i64)
    }
    /// Default refund and keeper tips of a pair order of this pair.
    pub fn tips(&self) -> (i64, i64) {
        let t = kob_protocol::defaults::pair_tips(prog(self.fa).0, prog(self.fb).0);
        (t.as_ref().map(|t| t.refund_tip as i64).unwrap_or(10_000_000), t.as_ref().map(|t| t.keeper_tip as i64).unwrap_or(5_000_000))
    }
}

/// A plain pair order of `n` whole A at `price` B per whole A: an ASK (sells A, custody = the amount) or a BID (holds a B
/// escrow for four fills), KAS tip `PTIP`, minimum fill one whole A.
pub fn pair_order(p: Pair, maker: u8, ask: bool, price: i64, n: i64) -> PairState {
    let (ac, ah, apre, asuf, afam) = p.tok(true);
    let (bc, bh, bpre, bsuf, bfam) = p.tok(false);
    let ((sc, sh, sp, ss, sf), (tc, th, tp, ts, tf, te)) = if ask {
        ((ac, ah, apre, asuf, afam), (bc, bh, bpre, bsuf, bfam, ext_of(p.fb)))
    } else {
        ((bc, bh, bpre, bsuf, bfam), (ac, ah, apre, asuf, afam, ext_of(p.fa)))
    };
    let mut s = PairState {
        maker: pk(maker),
        side: if ask { SIDE_ASK } else { SIDE_BID },
        s_cov_id: sc,
        s_tpl_hash: sh,
        s_pre: sp,
        s_suf: ss,
        s_family: sf,
        s_scale: SCALE,
        t_cov_id: tc,
        t_tpl_hash: th,
        t_pre: tp,
        t_suf: ts,
        t_family: tf,
        t_ext: te,
        t_scale: SCALE,
        min_fill: WHOLE,
        price,
        tip: PTIP,
        tif: TIF_GTC,
        active_from: 0,
        expiry_daa: EXPIRY,
        refund_tip: p.tips().0,
        delivery_carrier: PDC,
        interval: 0,
        max_fill: 0,
        slope: 0,
        price_end: 0,
        decay_step: 0,
        amount_left: n * WHOLE,
        custody: n * WHOLE,
        s_ext: if ask { ext_of(p.fa) } else { ext_of(p.fb) },
    };
    if !ask {
        s.custody = s.bid_escrow(s.amount_left, 4).unwrap();
    }
    s
}

/// An OCO pair sell (side ASK): take-profit `RATE + 200`, stop `RATE` with a 3% band over 300 DAA, 10 whole A,
/// minRestDaa 50, minTouch one whole A.
pub fn cond_pair_ask(p: Pair, maker: u8) -> CondPairState {
    let x = pair_order(p, maker, true, RATE, 10);
    CondPairState {
        maker: x.maker,
        side: SIDE_ASK,
        s_cov_id: x.s_cov_id,
        s_tpl_hash: x.s_tpl_hash,
        s_pre: x.s_pre,
        s_suf: x.s_suf,
        s_family: x.s_family,
        s_scale: x.s_scale,
        t_cov_id: x.t_cov_id,
        t_tpl_hash: x.t_tpl_hash,
        t_pre: x.t_pre,
        t_suf: x.t_suf,
        t_family: x.t_family,
        t_ext: x.t_ext,
        t_scale: x.t_scale,
        min_fill: WHOLE,
        tip: 0,
        active_from: 0,
        expiry_daa: EXPIRY,
        refund_tip: p.tips().0,
        delivery_carrier: PDC,
        tp_price: RATE + 200,
        slip_bps: 300,
        trail_step: 0,
        trail_gap: 0,
        trail_wait: 0,
        min_touch: WHOLE,
        min_rest_daa: 50,
        band_daa: 300,
        keeper_tip: p.tips().1,
        stop_price: RATE,
        armed: 0,
        amount_left: 10 * WHOLE,
        custody: 10 * WHOLE,
        parent: [0; 32],
        rpt_price: 0,
        rpt_pre: 0,
        rpt_until: 0,
        s_ext: x.s_ext,
    }
}

/// An OCO pair buy (side BID): limit `RATE - 200`, buy stop `RATE`, 10 whole A, the B escrow of its worst leg.
pub fn cond_pair_bid(p: Pair, maker: u8) -> CondPairState {
    let a = cond_pair_ask(p, maker);
    let mut c = CondPairState {
        side: SIDE_BID,
        s_cov_id: a.t_cov_id,
        s_tpl_hash: a.t_tpl_hash,
        s_pre: a.t_pre,
        s_suf: a.t_suf,
        s_family: a.t_family,
        s_scale: a.t_scale,
        t_cov_id: a.s_cov_id,
        t_tpl_hash: a.s_tpl_hash,
        t_pre: a.s_pre,
        t_suf: a.s_suf,
        t_family: a.s_family,
        t_ext: ext_of(p.fa),
        t_scale: a.s_scale,
        s_ext: ext_of(p.fb),
        tp_price: RATE - 200,
        stop_price: RATE,
        ..a
    };
    c.custody = c.bid_escrow(c.max_fills()).unwrap();
    c
}

/// A pair entry of 10 whole A at `RATE`: buy-first (its exit an OCO sell, TP `RATE + 200`, stop `RATE - 100`) or
/// sell-first (prefund 200 B per whole A; its exit an OCO buy-back, limit `RATE - 200`, stop `RATE + 100`). B custody:
/// what the entry needs (`b_custody_needed`).
pub fn ifd_pair(p: Pair, maker: u8, buy: bool) -> IfdPairState {
    let (ac, ah, apre, asuf, afam) = p.tok(true);
    let (bc, bh, bpre, bsuf, bfam) = p.tok(false);
    let exit = if buy {
        CondPairState { stop_price: RATE - 100, expiry_daa: NO_EXPIRY, amount_left: 0, custody: 0, ..cond_pair_ask(p, maker) }
    } else {
        CondPairState { stop_price: RATE + 100, expiry_daa: NO_EXPIRY, amount_left: 0, custody: 0, ..cond_pair_bid(p, maker) }
    };
    let mut s = IfdPairState {
        maker: pk(maker),
        side: if buy { SIDE_BID } else { SIDE_ASK },
        a_cov_id: ac,
        a_tpl_hash: ah,
        a_pre: apre,
        a_suf: asuf,
        a_family: afam,
        a_scale: SCALE,
        a_ext: ext_of(p.fa),
        b_cov_id: bc,
        b_tpl_hash: bh,
        b_pre: bpre,
        b_suf: bsuf,
        b_family: bfam,
        b_scale: SCALE,
        b_ext: ext_of(p.fb),
        price: RATE,
        prefund: if buy { 0 } else { 200 },
        tip: 0,
        active_from: 0,
        expiry_daa: EXPIRY,
        refund_tip: p.tips().0,
        delivery_carrier: PDC,
        exit_carrier: PEC,
        min_fill: WHOLE,
        entry_stop: 0,
        band_daa: 300,
        min_touch: WHOLE,
        min_rest_daa: 50,
        keeper_tip: p.tips().1,
        armed: 0,
        amount_left: 10 * WHOLE,
        custody: 0,
        rpt_amount: 0,
        exit_state: IfdPairState::commit_exit(&exit),
    };
    s.custody = s.b_custody_needed().unwrap();
    s
}

/// The KAS value of an entry UTXO.
pub fn ifd_value(s: &IfdPairState) -> u64 {
    s.kas_value().unwrap() as u64 + CARRIER
}

/// The custodies a new pair order holds, as maker coins: `(family, amount)` per custody of [`AnyState::custodies`].
fn custody_coins(p: Pair, w: &World, s: &AnyState, maker: u8) -> Vec<TokenUtxo> {
    s.custodies().into_iter().map(|(t, a)| w.token_for(p.fam_of(t), maker, a)).collect()
}

/// A created and indexed pair order: its creating transaction and covenant id.
pub struct Placed {
    pub create: SignedTx,
    pub cov: Hash32,
    pub state: AnyState,
}

/// Create a pair order of `maker` (value `value` sompi) and index it.
pub async fn place_pair(c: &Ctx, p: Pair, s: AnyState, value: u64, maker: u8) -> Placed {
    let req = CreateOrder {
        order: s.clone(),
        value,
        tokens: custody_coins(p, &c.w, &s, maker),
        token_carrier: CARRIER,
        funding: vec![c.w.coin(maker, 1_000)],
        change: None,
        lock_time: 0,
        deadline: None,
        records: vec![],
        fee: FeeOptions::default(),
    };
    let create = c.w.sign(&Action::CreateOrder(req));
    c.push(&[&create]).await;
    let cov = c.w.cov(&create, 0);
    Placed { create, cov, state: s }
}

/// The custody UTXO of `token` owned by `owner` in transaction `t` holding exactly `amount` (the world's custody state of
/// that token's family).
pub fn custody_at(c: &Ctx, p: Pair, t: &SignedTx, owner: &Hash32, token: [u8; 32], amount: i64) -> Option<TokenUtxo> {
    let f = p.fam_of(token);
    let st = TokenState::custody(f, amount, owner.0, ext_of(f));
    let spk = spk_to_string(&st.spk_with(kob_protocol::artifacts::token_template(prog(f).0)));
    let i =
        t.tx.outputs.iter().position(|o| o.script_public_key == spk && o.covenant.as_ref().map(|c| c.covenant_id) == Some(token))?;
    Some(c.w.token_at(t, i, st))
}

/// Index of the custody output of `token` owned by `owner` holding `amount` in `t`.
pub fn custody_index(p: Pair, t: &SignedTx, owner: &Hash32, token: [u8; 32], amount: i64) -> Option<usize> {
    let f = p.fam_of(token);
    let st = TokenState::custody(f, amount, owner.0, ext_of(f));
    let spk = spk_to_string(&st.spk_with(kob_protocol::artifacts::token_template(prog(f).0)));
    t.tx.outputs.iter().position(|o| o.script_public_key == spk)
}

/// The `Leg::Pair` of a live pair order (its UTXO at output `idx` of `t`, its custody found in `custody_tx`).
pub fn pair_leg(c: &Ctx, p: Pair, t: &SignedTx, idx: usize, s: &PairState, custody_tx: &SignedTx, amount: i64) -> Leg {
    let cov = c.w.cov(t, idx);
    let custody = custody_at(c, p, custody_tx, &cov, s.s_cov_id, s.custody).expect("the pair order's custody");
    Leg::Pair { order: c.w.order(t, idx, s.clone()), custody, amount, t: None }
}

/// Taker tokens of a family for a batch (the taker's own inventory).
pub fn taker_coin(c: &Ctx, f: Family, amount: i64) -> TokenUtxo {
    c.w.token_for(f, TAKER, amount)
}
