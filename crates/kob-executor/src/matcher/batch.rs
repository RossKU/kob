//! The global batch planner (`docs/spec/matcher.md` §3): one transaction over every book of the view.
//!
//! A batch is not per book. Every candidate of every book (every order × leg at `t`, [`super::candidate`]) and both legs
//! of every eligible pair order ([`super::pair`]) take part in one allocation; the transaction it describes may fill
//! several token books at once (X/KAS N:M, Y/KAS N:M, ...) together with the pair orders that net against each other or
//! route through them. Each token keeps its own slot accounting (its program's inputs / outputs, `MAX_TOK_IN` of its
//! family, one extension commitment); the only shared resources are the transaction's size and mass and the one input per
//! covenant id.
//!
//! Per plan, at `t = lockTime`:
//!
//! 1. **Netting** (§3.5) runs first in every allocation: the pair orders selling token X for token Y and those selling Y
//!    for X are netted against each other, best limits first, any number per side, each at its exact covenant amounts
//!    ([`Alloc::net_pairs`]); the surplus the netted orders release beyond what they receive goes to the pair asks buying
//!    that token, or is offered to the plain KAS bids of that token ([`PairRole::Surplus`]), or, under the opt-in
//!    surplus-inventory policy ([`super::planner::InventoryPolicy`]), what the bids do not take goes to the operator's key
//!    and counts at its policy value ([`Alloc::kept`]). What is left of a netted order is routed below. Netting moves no
//!    KAS: the operator earns the orders' KAS tips, what the surplus fetches and the value of what it keeps.
//! 2. **Classes** (§3.1), in this order over all books: class 1 (IOC, FOK, market, streaming, and IOC / FOK pair orders),
//!    class 2 (triggered stops and stop entries), class 3 (every resting crossing). Each active order walks the opposite
//!    side of its book in price → tip → age order and takes every crossing chunk it can; a lower class only uses what the
//!    higher classes left. A routed pair order is an ask in the books of the token it sells and a bid in the books of the
//!    token it buys at its implied quotes ([`super::pair`]), ranked with the direct orders of its class by price → tip → age.
//! 3. **Books share the transaction greedily by profit.** The books are grouped into components (books of one token, and
//!    the two tokens of every pair order); within each class the components are visited in decreasing order of the profit
//!    per byte they make on their own, so when the whole book set does not fit one transaction the most profitable batch is
//!    filled first. The rest is planned into the next transaction of the tick (`super::engine`).
//! 4. **Hard rules** are kept on every allocation step: FOK all-or-none, the minimum fill of both orders of a chunk (below
//!    it only when the chunk ends that order: takes its rest, or a `KobBid`'s last minimum fill of buying power), `maxFill`
//!    (TWAP / DCA), one input per covenant id (merged entries and updated orders included), positional outputs, the token
//!    slots of every program, no token left to the operator (with the inventory policy off the reference planner holds no
//!    inventory: a token's surplus needs a pair ask buying it, whose delivery takes it; with it on, the operator may take
//!    that surplus instead, the ask keeping exactly its floor) and the byte budget (the physical limit,
//!    [`super::planner::PHYSICAL_TX_BYTES`], or the operator's cap). Every routed pair order is bounded beforehand by the
//!    largest fill its route could pay on its own, or left out of routing when none can and it has no netting partner
//!    ([`route_cap`]). A purchase of a pair ask's token buys exactly what its delivery needs, more only when an ask's
//!    minimum fill forces it (the excess rides on the maker's delivery); a pair bid's purchase is exact.
//! 5. **Triggers** (§4, protocol v2.6: a stop reads its trigger evidence from a plain resting fill of the SAME
//!    transaction, the covenants' `touch`). After the class walks, the fills of plain `KobAsk` / `KobBid` orders without
//!    decay whose UTXO (and custody) DAA scores are known are the batch's evidence. Then, in class-2 order (first seen,
//!    quote, tip, age, id):
//!    * **triggered fills**: every unarmed stop leg / stop entry whose evidence the batch holds (same book; a resting ask
//!      filled at or below a sell stop, a resting bid at or above a buy stop, of the stop's scale; `n ≥ minTouch`; exposed at
//!      least `minRestDaa` before the lock time) walks the liquidity the other classes left, at its trigger price
//!      (`stop_at(true, ..)` / `price_at(true, ..)`), and is lowered with `evidence` = the leg index of the first such
//!      fill. A stop is sequenced after the trade that triggers it, as on an exchange: it never takes the liquidity of its
//!      own evidence, so the evidence cannot vanish under it. Unarmed pair stops (`KobCondPair`, `KobIfdPair` stop entries)
//!      read two evidence modes: two KAS-book fills of the batch (a plain resting order of A and one of B, the implied
//!      rate) or a resting `KobPair` of the same pair filled in it; a triggered pair stop is routed;
//!    * **updates** (`BatchUpdate`, [`PlannerConfig::arm`]): every other listed unarmed stop / stop entry the evidence
//!      serves, and every trailing stop whose UTXO is `trailWait` old that an opposite-side evidence ratchets (the most
//!      steps), is armed / ratcheted whenever it fits the byte budget, highest tip first (never a booked exit next to its
//!      own entry: the exit refuses an update in a transaction that spends its entry): arming is the default, and an
//!      update is a leg of the batch like any other (its cost, [`super::planner::update_cost`]: its bytes at the fee
//!      rate, at least the measured `updateFee`, is charged against the batch objective; its `keeperTip`, possibly 0,
//!      is income). Updates are dropped only while the batch would otherwise fall below `min_profit`, the worst tip
//!      minus cost first. A batch whose fills pay no spread is a standalone arming transaction: each update needs
//!      `keeperTip >= cost`.
//!
//!    Everything is recomputed on every allocation, so a fill only ever carries evidence that is in the same allocation.
//! 6. **Quantity repair and route reconciliation**: an order left violating its quantity rule is excluded and the walks
//!    re-run; the two legs of every pair order are reconciled to one fill (the token it sells exact, the token it buys
//!    covering its receipt) by lowering its cap until both legs agree (a fixed point: caps only decrease).
//! 7. **Profit**: `Σ crossing spread + Σ tips + Σ update tips (+ Σ kept inventory at its policy value, less the fee of its
//!    token output) − network fee` over the whole transaction, every KAS amount
//!    the exact rounded covenant value of its leg (`Cand::value`: a bid pays `floor(n·(p + tip)/scale)`, an ask receives
//!    `ceil(n·(p − tip)/scale)`, a pair order releases `floor(n·tip/scale(A))`); a walk never adds a direct chunk whose
//!    rounded amounts lose (a tiny chunk at a thin spread). Fills whose marginal profit is negative (their chunks' spread
//!    and tips less the fee of their bytes and of the counterparties only they use) are tried for dropping, several at
//!    once while that raises the profit, class 1 only when the batch would otherwise lose, never shrinking an IOC's fill;
//!    a pair order is dropped with both its legs, never a leg of it alone. A dropped pair order takes with it the losing
//!    pair orders that only took its place (on the same liquidity); every allocation is settled afresh from what the profit
//!    step dropped, so a pair order the reconciliation left out because a dropped fill held its token competes again. A
//!    drop is accepted only when the whole batch's profit rises, so a thin plain crossing that is the evidence of
//!    profitable triggered fills or updates stays (without it they vanish), and a batch whose crossing alone would not pay
//!    its fee is built when the triggered fills and update tips it carries make it profitable.
//!
//! Complexity: candidates are sorted once per plan; each walk scans its passive list from the first live entry and stops at
//! the first position from which nothing can cross it any more (a suffix bound on the all-in prices), and once the
//! transaction has no room for a new leg only the legs already in it are considered. Netting is a two-pointer walk over
//! each token pair's sorted pair orders. No candidate cap is needed; the wall-clock deadline alone bounds a hostile book.
//! Every choice is a pure function of the view, `t` and the configuration up to the deadline, so two honest matchers with
//! the same view build the same batch.

use std::cell::Cell;
use std::cmp::Ordering;
use std::collections::{BTreeMap, BTreeSet};
use std::time::Instant;

use kob_protocol::family::Family;

use super::book::{BookKey, CovId, ListedOrder, Market};
use super::candidate::*;
use super::family::{token_limits, TokenLimits};
use super::pair::{arm_evidence, trail_evidence, KasTouch, PairEv, PairFillTouch, PairInfo};
use super::planner::{
    crosses, est_fee, update_cost, Fill, InventoryPolicy, Kept, Plan, PlanUpdate, PlannerConfig, UpdateKind, KEPT_OUTPUT_BYTES,
};

/// Everything one batch plan reads.
pub struct BatchInput<'a> {
    pub by_id: &'a BTreeMap<CovId, ListedOrder>,
    pub lock_time: u64,
    pub utc: u64,
    /// Orders never planned (pending, backed off, quarantined, dropped by an earlier attempt).
    pub excluded: &'a BTreeSet<CovId>,
    /// Orders whose UTXO is an unaccepted continuation of an earlier transaction of the tick (§7): only chain-safe fills,
    /// never trigger evidence and never updated.
    pub unaccepted: &'a BTreeSet<CovId>,
    /// Token families this build can lower.
    pub families: &'a BTreeSet<Family>,
    /// Byte budget of the transaction.
    pub max_bytes: u64,
    /// Plan only these books (panic isolation); None: every book.
    pub only: Option<&'a BTreeSet<BookKey>>,
    /// No fill is added after this instant.
    pub deadline: Option<Instant>,
}

fn cmp_frac(a: (i128, i128), b: (i128, i128)) -> Ordering {
    (a.0 * b.1).cmp(&(b.0 * a.1))
}

/// A passive list: candidates of one side in priority order, with the bound that stops a walk early.
struct List {
    /// Side of the candidates in the list.
    side: Side,
    items: Vec<usize>,
    /// Suffix bound of the all-in price per base unit: the lowest ask (the highest bid) from each position on.
    bound: Vec<(i128, i128)>,
    /// Bytes of the smallest leg in the list (room check).
    min_bytes: u64,
    /// The position of each member (the list order of the legs already in the transaction).
    pos: BTreeMap<usize, usize>,
}

impl List {
    fn new(side: Side, items: Vec<usize>, cands: &[Cand]) -> List {
        let mut bound = vec![(0i128, 1i128); items.len()];
        let mut acc: Option<(i128, i128)> = None;
        for k in (0..items.len()).rev() {
            let p = cands[items[k]].per_base();
            acc = Some(match acc {
                None => p,
                Some(q) => match side {
                    Side::Ask if cmp_frac(p, q) == Ordering::Less => p,
                    Side::Bid if cmp_frac(p, q) == Ordering::Greater => p,
                    _ => q,
                },
            });
            bound[k] = acc.expect("set");
        }
        let min_bytes = items.iter().map(|&i| cands[i].bytes).min().unwrap_or(0);
        let pos = items.iter().enumerate().map(|(k, &i)| (i, k)).collect();
        List { side, items, bound, min_bytes, pos }
    }
}

/// Per-token information.
struct TokInfo {
    limits: TokenLimits,
}

/// The pair orders of one token pair that may net: those selling X (for Y) and those selling Y (for X), each in priority
/// order (the lowest ratio of what they receive to what they release first, then class, tip, age, id).
struct NetBook {
    g1: Vec<usize>,
    g2: Vec<usize>,
    /// The markets of X and Y.
    mx: Market,
    my: Market,
    /// The surplus offers of X and Y (candidate indices).
    sur_x: usize,
    sur_y: usize,
}

/// The candidates of one plan, sorted once.
struct Universe {
    cands: Vec<Cand>,
    /// The other leg of a pair order.
    twin: Vec<Option<usize>>,
    /// Token index of each candidate's leg.
    tok: Vec<usize>,
    toks: Vec<TokInfo>,
    /// Component of each candidate and each component's tie-break key (its smallest token id).
    comp: Vec<usize>,
    comp_key: Vec<[u8; 32]>,
    lists: Vec<List>,
    /// The list each candidate walks when it is active.
    walks: Vec<Option<usize>>,
    /// Class 1 and 2 walkers (armed stops), in (class, first seen, priority) order.
    actives: Vec<usize>,
    /// Unarmed stops (class 2, [`Cand::trigger`]): they walk after every other class, and only next to their evidence.
    touched: Vec<usize>,
    /// Unarmed pair stops (indices into `porders`), in class-2 order: routed next to their evidence.
    pair_touched: Vec<usize>,
    /// Orders a batch may arm or ratchet with an update, highest tip first.
    ups: Vec<UpCand>,
    /// Lock time `t`.
    t: i64,
    /// The fee rate updates are costed at.
    fee_rate: u64,
    /// The batch's minimum profit (updates are dropped only to reach it, [`Alloc::plan_updates`]).
    min_profit: i64,
    /// Class 3 walkers per component: (walker, list), books in key order, bids in priority order.
    resting: Vec<Vec<(usize, usize)>>,
    /// Pair orders: (Sell leg, Buy leg).
    porders: Vec<(usize, usize)>,
    /// The pair order of each candidate (its legs).
    porder_of: Vec<Option<usize>>,
    /// Netting groups.
    nets: Vec<NetBook>,
    /// The plain resting KAS bids of every market, best first (candidate indices): what a netting surplus may fetch, each
    /// bid only in an amount its own quantity rules accept.
    surplus_bids: BTreeMap<Market, Vec<usize>>,
    /// The surplus-inventory policy (off: no token is ever kept).
    inv: InventoryPolicy,
    /// The netting group of each pair order and whether it sells the group's first token (X).
    net_of: Vec<Option<(usize, bool)>>,
    /// The route bound of every pair order that may net (`i64::MAX`: none): what it may route on top of its netted part
    /// ([`route_cap`]; set by [`plan_batch`]).
    rcap: Vec<i64>,
    /// Candidates per covenant id (exclusion).
    by_id: BTreeMap<CovId, Vec<usize>>,
    /// Work counters of the plan ([`PlanWork`]).
    allocations: Cell<u64>,
    pair_tries: Cell<u64>,
    net_steps: Cell<u64>,
}

/// The signed quote per base unit of an active walker (asks ascending, bids descending: lower is better on both sides).
fn active_key(c: &Cand) -> (i128, i128) {
    let (q, s) = c.quote_frac();
    match c.side {
        Side::Ask => (q, s),
        Side::Bid => (-q, s),
    }
}

/// Priority of two actives by quote: the quote per base unit (a pair order leg's implied one), best first on each side.
fn active_quote(x: &Cand, y: &Cand) -> Ordering {
    cmp_frac(active_key(x), active_key(y))
}

/// Priority within a book (§3.1): quote per base unit (best first; one book has one scale, so this is the quote), tip
/// (higher first), age (older first), then id and leg. A pair order leg ranks at its implied quote.
fn cmp_in_book(cands: &[Cand], a: usize, b: usize) -> Ordering {
    let (x, y) = (&cands[a], &cands[b]);
    let q = match x.side {
        Side::Ask => cmp_frac(x.quote_frac(), y.quote_frac()),
        Side::Bid => cmp_frac(y.quote_frac(), x.quote_frac()),
    };
    q.then(y.tip.cmp(&x.tip)).then(x.age.cmp(&y.age)).then(x.id.cmp(&y.id)).then(x.leg.cmp(&y.leg)).then(x.role().cmp(&y.role()))
}

/// Priority across the books of a market (a pair order leg walking): all-in per base unit (best first), tip, age, id.
fn cmp_in_market(cands: &[Cand], a: usize, b: usize) -> Ordering {
    let (x, y) = (&cands[a], &cands[b]);
    let p = match x.side {
        Side::Ask => cmp_frac(x.per_base(), y.per_base()),
        Side::Bid => cmp_frac(y.per_base(), x.per_base()),
    };
    p.then(y.tip.cmp(&x.tip)).then(x.age.cmp(&y.age)).then(x.id.cmp(&y.id)).then(x.leg.cmp(&y.leg))
}

/// Netting priority of two pair orders selling the same token for the same other token: the lower ratio of what they
/// receive to what they release (the better offer) first, then class, tip, age, id, leg.
fn cmp_net(a: &PairInfo, ca: &Cand, b: &PairInfo, cb: &Cand) -> Ordering {
    let (sa, ta) = a.whole();
    let (sb, tb) = b.whole();
    // ta / sa vs tb / sb
    (ta as i128 * sb as i128)
        .cmp(&(tb as i128 * sa as i128))
        .then(ca.class.cmp(&cb.class))
        .then(cb.tip.cmp(&ca.tip))
        .then(ca.age.cmp(&cb.age))
        .then(ca.id.cmp(&cb.id))
        .then(ca.leg.cmp(&cb.leg))
}

/// Keeps the `cap` best-priority candidates of every (book, class, side) group (the optional operator knob).
fn cap_candidates(cands: Vec<Cand>, cap: usize) -> Vec<Cand> {
    let mut groups: BTreeMap<(BookKey, Class, Side), Vec<usize>> = BTreeMap::new();
    for (i, c) in cands.iter().enumerate() {
        groups.entry((c.book, c.class, c.side)).or_default().push(i);
    }
    if groups.values().all(|g| g.len() <= cap) {
        return cands;
    }
    let mut keep = vec![false; cands.len()];
    for g in groups.values_mut() {
        g.sort_by(|&a, &b| cmp_in_book(&cands, a, b));
        for &i in g.iter().take(cap) {
            keep[i] = true;
        }
    }
    cands.into_iter().zip(keep).filter_map(|(c, k)| k.then_some(c)).collect()
}

/// A covenant-id-like marker of the surplus offer of a market (never an order id: the marker is not a hash).
fn surplus_marker(m: &Market) -> CovId {
    let mut id = [0xffu8; 32];
    for (k, b) in m.token.iter().enumerate() {
        id[k] ^= b.rotate_left(3) ^ 0x5a;
    }
    id[0] = 0xfe;
    id
}

impl Universe {
    fn new(inp: &BatchInput, cfg: &PlannerConfig) -> Universe {
        let t = inp.lock_time as i64;
        // Every order once, in covenant id order: the plan does not depend on the order of the view.
        let orders: Vec<&ListedOrder> = inp.by_id.values().filter(|o| !inp.excluded.contains(&o.id())).collect();
        let supported = |m: &Market| inp.families.contains(&m.family) && token_limits(m.family, &m.template).is_some();
        let wanted = |b: &BookKey| supported(&b.market()) && inp.only.is_none_or(|s| s.contains(b));
        let cx = CandCtx { t, utc: inp.utc, excluded: inp.excluded, by_id: inp.by_id, unaccepted: inp.unaccepted };
        let direct_orders: Vec<&ListedOrder> = orders.iter().copied().filter(|o| !o.order.state.is_pair()).collect();
        let mut direct: Vec<Cand> = candidates_of(&direct_orders, &cx)
            .into_iter()
            .filter(|c| wanted(&c.book))
            // a chained step: an unaccepted continuation only on a covenant path that ignores its parent's DAA score
            .filter(|c| c.chain_safe || !inp.unaccepted.contains(&c.id))
            .collect();
        if cfg.max_candidates_per_group > 0 {
            direct = cap_candidates(direct, cfg.max_candidates_per_group);
        }
        let pair_orders: Vec<&ListedOrder> =
            orders.iter().copied().filter(|o| o.order.state.is_pair() && !inp.unaccepted.contains(&o.id())).collect();
        let mut surplus_bids: BTreeMap<Market, Vec<usize>> = BTreeMap::new();
        for (i, c) in direct.iter().enumerate() {
            if c.is_plain() && c.trigger.is_none() && c.side == Side::Bid {
                surplus_bids.entry(c.book.market()).or_default().push(i);
            }
        }
        for v in surplus_bids.values_mut() {
            v.sort_by(|&i, &j| {
                let ((pi, di), (pj, dj)) = (direct[i].per_base(), direct[j].per_base());
                (pj * di).cmp(&(pi * dj)).then(direct[i].id.cmp(&direct[j].id))
            });
        }
        let mut cands = direct;
        let mut twin: Vec<Option<usize>> = vec![None; cands.len()];
        let mut porders = vec![];
        if !pair_orders.is_empty() {
            for (s, b) in super::pair::legs(&pair_orders, &cands, &cx) {
                let Some(xs) = &s.pair else { continue };
                let (ma, mb) = (xs.info.a, xs.info.b);
                // both markets planned (panic isolation plans a subset of the books)
                if !supported(&ma) || !supported(&mb) {
                    continue;
                }
                if let Some(only) = inp.only {
                    if !only.iter().any(|k| k.market() == ma) || !only.iter().any(|k| k.market() == mb) {
                        continue;
                    }
                }
                let i = cands.len();
                cands.push(s);
                cands.push(b);
                twin.push(Some(i + 1));
                twin.push(Some(i));
                porders.push((i, i + 1));
            }
        }
        // netting groups: per unordered pair of markets, the pair orders selling each of them for the other
        let mut groups: BTreeMap<(Market, Market), (Vec<usize>, Vec<usize>)> = BTreeMap::new();
        for (p, &(s, _)) in porders.iter().enumerate() {
            let x = &cands[s].pair.as_ref().expect("pair leg").info;
            if x.need.is_some() {
                continue; // an unarmed stop fills only next to its evidence (routed)
            }
            let (sm, tm) = (x.s_market(), x.t_market());
            let key = if sm < tm { (sm, tm) } else { (tm, sm) };
            let e = groups.entry(key).or_default();
            if sm == key.0 {
                e.0.push(p);
            } else {
                e.1.push(p);
            }
        }
        let mut nets = vec![];
        let mut net_of: Vec<Option<(usize, bool)>> = vec![None; porders.len()];
        let mut surplus_of: BTreeMap<Market, usize> = BTreeMap::new();
        for ((mx, my), (mut g1, mut g2)) in groups {
            if g1.is_empty() || g2.is_empty() {
                continue;
            }
            let cmp = |a: &usize, b: &usize| {
                let (ca, cb) = (&cands[porders[*a].0], &cands[porders[*b].0]);
                let (ia, ib) = (&ca.pair.as_ref().expect("leg").info, &cb.pair.as_ref().expect("leg").info);
                cmp_net(ia, ca, ib, cb)
            };
            g1.sort_by(cmp);
            g2.sort_by(cmp);
            let mut sur = |m: Market, cands: &mut Vec<Cand>, twin: &mut Vec<Option<usize>>| -> usize {
                *surplus_of.entry(m).or_insert_with(|| {
                    cands.push(super::pair::surplus_cand(m, surplus_marker(&m)));
                    twin.push(None);
                    cands.len() - 1
                })
            };
            let sur_x = sur(mx, &mut cands, &mut twin);
            let sur_y = sur(my, &mut cands, &mut twin);
            for &p in &g1 {
                net_of[p] = Some((nets.len(), true));
            }
            for &p in &g2 {
                net_of[p] = Some((nets.len(), false));
            }
            nets.push(NetBook { g1, g2, mx, my, sur_x, sur_y });
        }
        let n = cands.len();
        let mut porder_of: Vec<Option<usize>> = vec![None; n];
        for (p, &(s, b)) in porders.iter().enumerate() {
            porder_of[s] = Some(p);
            porder_of[b] = Some(p);
        }
        // tokens
        let mut tok_ix: BTreeMap<[u8; 32], usize> = BTreeMap::new();
        let mut toks: Vec<TokInfo> = vec![];
        let mut tok = vec![0usize; n];
        for (i, c) in cands.iter().enumerate() {
            let k = *tok_ix.entry(c.book.token).or_insert_with(|| {
                let limits = token_limits(c.book.family, &c.book.template).expect("supported program");
                toks.push(TokInfo { limits });
                toks.len() - 1
            });
            tok[i] = k;
        }
        // components: tokens joined by pair orders
        let mut parent: Vec<usize> = (0..toks.len()).collect();
        fn find(p: &mut [usize], x: usize) -> usize {
            let mut r = x;
            while p[r] != r {
                r = p[r];
            }
            let mut y = x;
            while p[y] != r {
                let nx = p[y];
                p[y] = r;
                y = nx;
            }
            r
        }
        for &(s, b) in &porders {
            let (x, y) = (find(&mut parent, tok[s]), find(&mut parent, tok[b]));
            if x != y {
                parent[x.max(y)] = x.min(y);
            }
        }
        let tok_ids: Vec<[u8; 32]> = {
            let mut v = vec![[0u8; 32]; toks.len()];
            for (id, &k) in &tok_ix {
                v[k] = *id;
            }
            v
        };
        let mut comp_of_root: BTreeMap<usize, usize> = BTreeMap::new();
        let mut comp_key: Vec<[u8; 32]> = vec![];
        // tokens in id order: component ids follow the smallest token id of each component
        for &k in tok_ix.values() {
            let r = find(&mut parent, k);
            if let std::collections::btree_map::Entry::Vacant(e) = comp_of_root.entry(r) {
                e.insert(comp_key.len());
                comp_key.push(tok_ids[k]);
            }
        }
        let comp: Vec<usize> = (0..n).map(|i| comp_of_root[&find(&mut parent, tok[i])]).collect();
        // lists: per book (direct orders of the book, the route legs and surplus offers of its market), per market (plain
        // orders)
        let mut book_asks: BTreeMap<BookKey, Vec<usize>> = BTreeMap::new();
        let mut book_bids: BTreeMap<BookKey, Vec<usize>> = BTreeMap::new();
        for (i, c) in cands.iter().enumerate() {
            // an unarmed stop is never passive liquidity: it only walks, after its evidence (§4)
            if c.pair.is_none() && c.trigger.is_none() {
                match c.side {
                    Side::Ask => book_asks.entry(c.book).or_default().push(i),
                    Side::Bid => book_bids.entry(c.book).or_default().push(i),
                }
            }
        }
        let books: BTreeSet<BookKey> = book_asks.keys().chain(book_bids.keys()).copied().collect();
        let listed =
            |c: &Cand| c.pair.as_ref().is_some_and(|x| x.role == PairRole::Surplus || (x.info.route && x.info.need.is_none()));
        for (i, c) in cands.iter().enumerate() {
            if !listed(c) {
                continue;
            }
            let m = c.book.market();
            for k in &books {
                if k.market() == m {
                    match c.side {
                        Side::Ask => book_asks.entry(*k).or_default().push(i),
                        Side::Bid => book_bids.entry(*k).or_default().push(i),
                    }
                }
            }
        }
        let mut lists: Vec<List> = vec![];
        let mut ask_list: BTreeMap<BookKey, usize> = BTreeMap::new();
        let mut bid_list: BTreeMap<BookKey, usize> = BTreeMap::new();
        for k in &books {
            let mut a = book_asks.remove(k).unwrap_or_default();
            a.sort_by(|&x, &y| cmp_in_book(&cands, x, y));
            ask_list.insert(*k, lists.len());
            lists.push(List::new(Side::Ask, a, &cands));
            let mut b = book_bids.remove(k).unwrap_or_default();
            b.sort_by(|&x, &y| cmp_in_book(&cands, x, y));
            bid_list.insert(*k, lists.len());
            lists.push(List::new(Side::Bid, b, &cands));
        }
        let mut mkt_asks: BTreeMap<Market, usize> = BTreeMap::new();
        let mut mkt_bids: BTreeMap<Market, usize> = BTreeMap::new();
        for &(s, _) in &porders {
            let x = &cands[s].pair.as_ref().expect("leg").info;
            if !x.route {
                continue;
            }
            let (ms, mt) = (x.s_market(), x.t_market());
            if let std::collections::btree_map::Entry::Vacant(e) = mkt_bids.entry(ms) {
                let mut v: Vec<usize> =
                    (0..n).filter(|&i| cands[i].side == Side::Bid && cands[i].is_plain() && cands[i].book.market() == ms).collect();
                v.sort_by(|&x, &y| cmp_in_market(&cands, x, y));
                e.insert(lists.len());
                lists.push(List::new(Side::Bid, v, &cands));
            }
            if let std::collections::btree_map::Entry::Vacant(e) = mkt_asks.entry(mt) {
                let mut v: Vec<usize> =
                    (0..n).filter(|&i| cands[i].side == Side::Ask && cands[i].is_plain() && cands[i].book.market() == mt).collect();
                v.sort_by(|&x, &y| cmp_in_market(&cands, x, y));
                e.insert(lists.len());
                lists.push(List::new(Side::Ask, v, &cands));
            }
        }
        let walks: Vec<Option<usize>> = cands
            .iter()
            .map(|c| match (&c.pair, c.side) {
                (None, Side::Ask) => bid_list.get(&c.book).copied(),
                (None, Side::Bid) => ask_list.get(&c.book).copied(),
                (Some(x), _) if x.role == PairRole::Surplus || !x.info.route => None,
                (Some(x), _) if x.role == PairRole::Sell => mkt_bids.get(&x.info.s_market()).copied(),
                (Some(x), _) => mkt_asks.get(&x.info.t_market()).copied(),
            })
            .collect();
        let active_order = |a: &usize, b: &usize| {
            let (x, y) = (&cands[*a], &cands[*b]);
            x.class
                .cmp(&y.class)
                .then(x.seen.cmp(&y.seen))
                .then(active_quote(x, y))
                .then(y.tip.cmp(&x.tip))
                .then(x.age.cmp(&y.age))
                .then(x.id.cmp(&y.id))
                .then(x.leg.cmp(&y.leg))
                .then(x.side.cmp(&y.side))
        };
        let is_pair_need = |c: &Cand| c.pair.as_ref().is_some_and(|x| x.info.need.is_some());
        let mut actives: Vec<usize> = (0..n)
            .filter(|&i| {
                let c = &cands[i];
                c.class != Class::Resting && c.trigger.is_none() && !is_pair_need(c) && c.role() != Some(PairRole::Surplus)
            })
            .collect();
        actives.sort_by(active_order);
        let mut touched: Vec<usize> = (0..n).filter(|&i| cands[i].trigger.is_some()).collect();
        touched.sort_by(active_order);
        let mut pair_touched: Vec<usize> = (0..porders.len()).filter(|&p| is_pair_need(&cands[porders[p].0])).collect();
        pair_touched.sort_by(|a, b| active_order(&porders[*a].0, &porders[*b].0));
        let mut ups = if cfg.arm { updatable(&direct_orders, &cx) } else { vec![] };
        ups.retain(|x| wanted(&x.book));
        if cfg.arm && !pair_orders.is_empty() {
            ups.extend(super::pair::updatable(&pair_orders, &cx).into_iter().filter(|x| {
                let ok = |m: &Market| supported(m) && inp.only.is_none_or(|s| s.iter().any(|k| k.market() == *m));
                inp.by_id.get(&x.id).and_then(super::pair::markets).is_some_and(|(a, b)| ok(&a) && ok(&b))
            }));
        }
        ups.sort_by(|a, b| b.tip.cmp(&a.tip).then(a.id.cmp(&b.id)));
        let mut resting: Vec<Vec<(usize, usize)>> = vec![vec![]; comp_key.len()];
        let mut walking: BTreeSet<usize> = BTreeSet::new();
        for k in &books {
            let bl = &lists[bid_list[k]];
            for &b in &bl.items {
                if cands[b].class != Class::Resting || cands[b].role() == Some(PairRole::Surplus) {
                    continue;
                }
                if let Some(w) = walks[b] {
                    // a direct bid walks its book; a pair order's purchase walks the plain asks of its market (once)
                    if walking.insert(b) {
                        resting[comp[b]].push((b, w));
                    }
                }
            }
        }
        let mut by_id: BTreeMap<CovId, Vec<usize>> = BTreeMap::new();
        for (i, c) in cands.iter().enumerate() {
            by_id.entry(c.id).or_default().push(i);
        }
        Universe {
            cands,
            twin,
            tok,
            toks,
            comp,
            comp_key,
            lists,
            walks,
            actives,
            touched,
            pair_touched,
            ups,
            t,
            fee_rate: cfg.fee_rate,
            min_profit: cfg.min_profit,
            resting,
            porders,
            porder_of,
            nets,
            surplus_bids,
            inv: cfg.inventory.clone(),
            net_of,
            rcap: vec![],
            by_id,
            allocations: Cell::new(0),
            pair_tries: Cell::new(0),
            net_steps: Cell::new(0),
        }
    }

    fn role(&self, i: usize) -> Option<PairRole> {
        self.cands[i].role()
    }

    fn info(&self, p: usize) -> &PairInfo {
        &self.cands[self.porders[p].0].pair.as_ref().expect("pair leg").info
    }

    /// The leg of pair order `p` whose quantity is its fill n.
    fn primary(&self, p: usize) -> usize {
        let (s, b) = self.porders[p];
        if self.info(p).primary() == PairRole::Sell {
            s
        } else {
            b
        }
    }

    /// Caps of an allocation: the candidates' own, a pair order's legs at its reconciled fill (the S it releases, the T
    /// it receives), the surplus offers none (each allocation sets them from its netting).
    fn caps(&self, xcap: &[i64]) -> Vec<i64> {
        let mut caps: Vec<i64> = self.cands.iter().map(|c| c.cap).collect();
        for (p, &(s, b)) in self.porders.iter().enumerate() {
            let x = self.info(p);
            let n = xcap[p].min(x.cap);
            caps[s] = x.sell_qty(n).unwrap_or(0);
            caps[b] = x.buy_qty(n).unwrap_or(0);
        }
        for nb in &self.nets {
            caps[nb.sur_x] = 0;
            caps[nb.sur_y] = 0;
        }
        caps
    }
}

/// Token slots of one token in the transaction.
#[derive(Clone, Copy, Default, Debug, PartialEq, Eq)]
struct Tok {
    tin: u32,
    tout: u32,
    sold: i128,
    bought: i128,
    legs: u32,
    ext: [u8; 32],
    /// Pair asks buying this token (their deliveries are minimums: the batch's surplus of the token goes to them).
    absorb: u32,
    /// Ask-side legs (asks, pair sales) that keep a remainder output (a chunk that sells one out frees an output).
    rests: u32,
}

impl Tok {
    fn add(&mut self, d: &Tok) {
        if d.legs > 0 && self.legs == 0 {
            self.ext = d.ext;
        }
        self.tin += d.tin;
        self.tout += d.tout;
        self.sold += d.sold;
        self.bought += d.bought;
        self.legs += d.legs;
        self.absorb += d.absorb;
        self.rests += d.rests;
    }
    fn sub(&mut self, d: &Tok) {
        self.tin -= d.tin;
        self.tout -= d.tout;
        self.sold -= d.sold;
        self.bought -= d.bought;
        self.legs -= d.legs;
        self.absorb -= d.absorb;
        self.rests -= d.rests;
    }
    /// Tokens the batch would leave to the operator: released beyond what the legs take with no pair ask buying them. The
    /// reference planner holds no inventory: such an allocation is never built.
    fn op_out(&self) -> bool {
        self.sold > self.bought && self.absorb == 0
    }
    fn ok(&self, lim: &TokenLimits) -> bool {
        (self.tin as usize) <= lim.max_in && (self.tout as usize) <= lim.max_out && !self.op_out() && self.sold >= self.bought
    }
}

enum Undo {
    Add { i: usize, prev: i64, tok: Box<(usize, Tok)>, first: bool, holders: Vec<(CovId, Option<usize>)>, leg_bytes: u64 },
    Chunk,
    Head(usize, usize),
}

/// A planned update: the index in [`Universe::ups`], the evidence candidates, what it does, the steps and its cost.
#[derive(Clone, Copy, Debug)]
struct Up {
    up: usize,
    ev: usize,
    ev_b: Option<usize>,
    kind: UpdateKind,
    steps: i64,
    cost: i64,
}

/// One allocation: the base units of every candidate and the transaction's running accounting.
struct Alloc<'u> {
    u: &'u Universe,
    /// The caps of this allocation (the surplus offers' set by its netting).
    caps: Vec<i64>,
    live: &'u [bool],
    qty: Vec<i64>,
    holder: BTreeMap<CovId, usize>,
    seq: Vec<usize>,
    toks: Vec<Tok>,
    leg_bytes: u64,
    /// (bid, ask, bid quantity, ask quantity) of every chunk (marginal profit).
    chunks: Vec<(usize, usize, i64, i64)>,
    head: Vec<usize>,
    log: Vec<Undo>,
    /// Triggered fills: the evidence candidate of each (§4).
    ev_of: BTreeMap<usize, usize>,
    /// Triggered pair stops: the evidence of each pair order.
    pev_of: BTreeMap<usize, PairEv>,
    /// Pair orders netted in a group that pays for itself (its tips and surplus value cover its bytes): the profit step
    /// judges them as a group, never one by one (a participant's own marginal misses the surplus it enables).
    netted: BTreeSet<usize>,
    /// Updates (§4) and what they add: bytes and the keeper tips the batch takes.
    ups: Vec<Up>,
    up_bytes: u64,
    tips: i64,
    max_bytes: u64,
    deadline: Option<Instant>,
    steps: u64,
}

impl<'u> Alloc<'u> {
    fn new(u: &'u Universe, caps: &[i64], live: &'u [bool], max_bytes: u64, deadline: Option<Instant>) -> Self {
        Alloc {
            u,
            caps: caps.to_vec(),
            live,
            qty: vec![0; u.cands.len()],
            holder: BTreeMap::new(),
            seq: vec![],
            toks: vec![Tok::default(); u.toks.len()],
            leg_bytes: 0,
            chunks: vec![],
            head: vec![0; u.lists.len()],
            log: vec![],
            ev_of: BTreeMap::new(),
            pev_of: BTreeMap::new(),
            netted: BTreeSet::new(),
            ups: vec![],
            up_bytes: 0,
            tips: 0,
            max_bytes,
            deadline,
            steps: 0,
        }
    }

    fn out_of_time(&mut self) -> bool {
        self.steps += 1;
        self.steps.is_multiple_of(64) && self.deadline.is_some_and(|d| Instant::now() >= d)
    }

    fn bytes(&self) -> u64 {
        BASE_BYTES + self.leg_bytes + self.up_bytes
    }

    /// The evidence fills of the allocation in `book` (candidate, base units), in allocation order.
    fn evidence_in(&self, book: &BookKey) -> impl Iterator<Item = (usize, i64, TouchSrc)> + '_ {
        let book = *book;
        self.seq.iter().filter_map(move |&e| {
            let c = &self.u.cands[e];
            match c.touch {
                Some(src) if self.qty[e] > 0 && c.book == book => Some((e, self.qty[e], src)),
                _ => None,
            }
        })
    }

    /// The first evidence fill of the allocation that triggers the unarmed stop `a` (§4).
    fn evidence_for(&self, a: usize) -> Option<usize> {
        let c = &self.u.cands[a];
        let need = c.trigger?;
        let t = self.u.t;
        self.evidence_in(&c.book).find(|(_, n, src)| need.served_by(src, *n, t)).map(|(e, _, _)| e)
    }

    /// The fills of the allocation a pair stop may read as evidence: the KAS-book touches (mode 0) and the resting
    /// `KobPair` fills whose legs agree (mode 1), in allocation order.
    fn pair_touches(&self) -> (Vec<KasTouch>, Vec<PairFillTouch>) {
        let u = self.u;
        let mut kas = vec![];
        let mut pairs = vec![];
        for &e in &self.seq {
            let c = &u.cands[e];
            if self.qty[e] <= 0 {
                continue;
            }
            if let Some(src) = c.touch {
                kas.push(KasTouch { ix: e, token: c.book.token, src, n: self.qty[e] });
            }
            if let (Some(x), Some(p)) = (&c.pair, u.porder_of[e]) {
                if x.is_primary() {
                    if let Some(src) = x.info.touch {
                        if self.pair_consistent(p) {
                            pairs.push(PairFillTouch { ix: e, src, n: self.qty[e] });
                        }
                    }
                }
            }
        }
        (kas, pairs)
    }

    /// Triggered fills (§4): every unarmed stop the batch's evidence serves walks what the other classes left.
    fn trigger_walks(&mut self) {
        let u = self.u;
        for &a in &u.touched {
            if !self.live[a] || self.qty[a] > 0 {
                continue;
            }
            if self.deadline.is_some_and(|d| Instant::now() >= d) {
                return;
            }
            let Some(e) = self.evidence_for(a) else { continue };
            if let Some(l) = u.walks[a] {
                self.walk(a, l);
            }
            if self.qty[a] > 0 {
                self.ev_of.insert(a, e);
            }
        }
        // unarmed pair stops: armed in this fill by two KAS-book fills or a resting pair fill of the batch, routed
        for &p in &u.pair_touched {
            let (s, b) = u.porders[p];
            if !self.live[s] || self.qty[s] > 0 || self.qty[b] > 0 {
                continue;
            }
            if self.deadline.is_some_and(|d| Instant::now() >= d) {
                return;
            }
            let x = u.info(p);
            let Some(need) = &x.need else { continue };
            if !x.route {
                continue;
            }
            let (kas, pairs) = self.pair_touches();
            let Some(ev) = arm_evidence(need, &kas, &pairs, u.t) else { continue };
            let (Some(ls), Some(lb)) = (u.walks[s], u.walks[b]) else { continue };
            let mark = self.log.len();
            self.walk(s, ls);
            self.walk(b, lb);
            if self.qty[s] > 0 && self.qty[b] > 0 {
                self.pev_of.insert(p, ev);
            } else {
                self.rollback(mark);
            }
        }
    }

    /// Updates (§4): the listed stops the batch's evidence arms or ratchets without filling them, highest tip first, while
    /// the transaction has room. Arming is the default: an update is part of the batch's objective like any other leg (its
    /// cost is charged, its tip is income), so a zero tip arms when the crossing spread pays for it. Updates are dropped
    /// only when the batch would otherwise fall below `min_profit`: the ones with the worst tip minus cost first (the later
    /// in tip order on a tie), while that raises the profit. A batch whose fills pay no spread is only an arming
    /// transaction of its own: each of its updates must be paid by its tip (`tip >= cost`).
    fn plan_updates(&mut self) {
        let u = self.u;
        let t = u.t;
        let spread_pays = self.margin() > 0;
        let (kas, pairs) = if u.ups.iter().any(|x| x.pair.is_some()) { self.pair_touches() } else { (vec![], vec![]) };
        for (k, x) in u.ups.iter().enumerate() {
            if self.holder.contains_key(&x.id) {
                continue; // filled, merged or held by a fill of this batch: one input per covenant id
            }
            if x.parent.is_some_and(|p| self.holder.contains_key(&p)) {
                continue; // a booked exit is never updated next to its own entry (the exit refuses it, §6.1)
            }
            let cost = update_cost(x, u.fee_rate);
            if (!spread_pays && x.tip < cost) || x.value <= x.tip || self.bytes() + x.bytes > self.max_bytes {
                continue;
            }
            let (ev, ev_b, kind, steps) = if let Some(pu) = &x.pair {
                match arm_evidence(&pu.need, &kas, &pairs, t) {
                    Some((e, eb)) => (e, eb, UpdateKind::Arm, 1),
                    None if pu.trail => match trail_evidence(&pu.need, &kas, &pairs, t) {
                        Some(((e, eb), steps)) => (e, eb, UpdateKind::Trail, steps),
                        None => continue,
                    },
                    None => continue,
                }
            } else {
                let Some(arm_need) = x.arm else { continue };
                let arm = self.evidence_in(&x.book).find(|(_, n, src)| arm_need.served_by(src, *n, t)).map(|(e, _, _)| e);
                match (arm, &x.trail) {
                    (Some(e), _) => (e, None, UpdateKind::Arm, 1),
                    (None, Some(tr)) => {
                        let best = self
                            .evidence_in(&x.book)
                            .filter(|(_, n, src)| {
                                src.side == tr.side()
                                    && src.scale == arm_need.scale
                                    && touch_ok(src, *n, arm_need.min_touch, arm_need.min_rest, t)
                            })
                            .map(|(e, _, src)| (tr.steps(src.price), e))
                            .filter(|(k, _)| *k >= 1)
                            // the most steps; the first such fill on a tie
                            .fold(None::<(i64, usize)>, |b, (k, e)| if b.is_none_or(|(bk, _)| k > bk) { Some((k, e)) } else { b });
                        match best {
                            Some((steps, e)) => (e, None, UpdateKind::Trail, steps),
                            None => continue,
                        }
                    }
                    (None, None) => continue,
                }
            };
            self.ups.push(Up { up: k, ev, ev_b, kind, steps, cost });
            self.up_bytes += x.bytes;
            self.tips = self.tips.saturating_add(x.tip);
        }
        // below `min_profit`: drop the updates that cost more than their tip, worst first
        let mut profit = self.profit(u.fee_rate);
        if profit >= u.min_profit {
            return;
        }
        let mut losers: Vec<(i64, usize)> = self
            .ups
            .iter()
            .enumerate()
            .map(|(pos, x)| (u.ups[x.up].tip.saturating_sub(x.cost), pos))
            .filter(|(net, _)| *net < 0)
            .collect();
        losers.sort_by(|a, b| a.0.cmp(&b.0).then(b.1.cmp(&a.1)));
        let mut dropped = BTreeSet::new();
        for (net, pos) in losers {
            if profit >= u.min_profit {
                break;
            }
            profit = profit.saturating_sub(net);
            dropped.insert(pos);
        }
        if dropped.is_empty() {
            return;
        }
        for (pos, x) in std::mem::take(&mut self.ups).into_iter().enumerate() {
            if dropped.contains(&pos) {
                self.up_bytes -= u.ups[x.up].bytes;
                self.tips = self.tips.saturating_sub(u.ups[x.up].tip);
            } else {
                self.ups.push(x);
            }
        }
    }

    /// What candidate `i` at quantity `n` contributes to its token.
    fn delta(&self, i: usize, n: i64) -> Tok {
        let c = &self.u.cands[i];
        if n <= 0 {
            return Tok::default();
        }
        let mut d = Tok { legs: 1, ext: c.book.extension, ..Tok::default() };
        match (&c.pair, c.side) {
            (Some(x), _) => match x.role {
                PairRole::Sell => {
                    let (tin, tout) = x.info.sell_slots(n);
                    d.tin = tin;
                    d.tout = tout;
                    d.sold = n as i128;
                    d.rests = u32::from(n < c.left);
                }
                PairRole::Buy => {
                    let (tin, tout) = x.info.buy_slots(n);
                    d.tin = tin;
                    d.tout = tout;
                    d.bought = n as i128;
                    d.absorb = u32::from(!x.info.buy_exact());
                }
                // the surplus's tokens are already in the transaction (released by the netted orders' sales)
                PairRole::Surplus => {}
            },
            (None, Side::Ask) => {
                d.tin = 1;
                // custody remainder (or an IOC's tokens back to its maker)
                d.tout = u32::from(n < c.left);
                d.rests = d.tout;
                d.sold = n as i128;
            }
            (None, Side::Bid) => {
                d.tin = u32::from(c.merge_custody);
                d.tout = 1;
                d.bought = n as i128;
            }
        }
        d
    }

    fn usable(&self, i: usize) -> bool {
        let c = &self.u.cands[i];
        let tw = self.u.twin[i];
        let free = |id: &CovId| self.holder.get(id).is_none_or(|h| *h == i || Some(*h) == tw);
        self.live[i]
            && free(&c.id)
            && c.merge.as_ref().is_none_or(free)
            && c.parent.as_ref().is_none_or(free)
            && self.qty[i] < self.caps[i]
    }

    fn dead(&self, i: usize) -> bool {
        !self.live[i] || self.qty[i] >= self.caps[i] || (self.u.cands[i].fok.is_some() && self.qty[i] > 0)
    }

    /// The KRON output amount cap of the candidate's token at a total of `q`: every token output the leg makes (a
    /// delivery, a custody rest) holds at most the program's largest output amount.
    fn amount_ok(&self, i: usize, q: i64) -> bool {
        let c = &self.u.cands[i];
        let Some(cap) = self.u.toks[self.u.tok[i]].limits.max_output_amount else { return true };
        let (cap, q) = (cap as i128, q as i128);
        match (c.role(), c.side) {
            (Some(PairRole::Sell), _) => c.left as i128 - q <= cap,
            (Some(PairRole::Buy), _) => q <= cap,
            (Some(PairRole::Surplus), _) => true,
            (None, Side::Ask) => q <= cap && c.left as i128 - q <= cap,
            (None, Side::Bid) => q <= cap,
        }
    }

    /// Whether adding `adds` (candidate, quantity) of one token keeps the transaction within every rule.
    fn fits(&self, adds: &[(usize, i64)]) -> bool {
        let t = self.u.tok[adds[0].0];
        let mut tok = self.toks[t];
        let mut leg_bytes = self.leg_bytes;
        let mut counted: BTreeSet<CovId> = BTreeSet::new();
        for &(i, q) in adds {
            debug_assert_eq!(self.u.tok[i], t);
            let c = &self.u.cands[i];
            let old = self.qty[i];
            let new = old + q;
            if !self.amount_ok(i, new) {
                return false;
            }
            let (d_old, d_new) = (self.delta(i, old), self.delta(i, new));
            if old == 0 {
                if tok.legs > 0 && c.book.extension != tok.ext {
                    return false;
                }
                // an order's bytes count once (its legs share its input)
                if !self.holder.contains_key(&c.id) && counted.insert(c.id) {
                    leg_bytes += c.bytes;
                }
            }
            tok.sub(&d_old);
            tok.add(&d_new);
        }
        if !tok.ok(&self.u.toks[t].limits) {
            return false;
        }
        BASE_BYTES + leg_bytes + self.up_bytes <= self.max_bytes
    }

    fn add(&mut self, i: usize, q: i64) {
        let c = &self.u.cands[i];
        let t = self.u.tok[i];
        let prev = self.qty[i];
        let first = prev == 0;
        let mut holders = vec![];
        let rec_tok = Box::new((t, self.toks[t]));
        let leg_bytes = self.leg_bytes;
        if first {
            self.seq.push(i);
            if !self.holder.contains_key(&c.id) {
                self.leg_bytes += c.bytes;
            }
            let mut hold = |id: CovId, h: &mut BTreeMap<CovId, usize>| {
                holders.push((id, h.get(&id).copied()));
                h.entry(id).or_insert(i);
            };
            hold(c.id, &mut self.holder);
            // A booked exit holds its entry (merged or not: a stop-loss must not carry it).
            for e in c.merge.iter().chain(c.parent.iter()) {
                holders.push((*e, self.holder.get(e).copied()));
                self.holder.insert(*e, i);
            }
        }
        let (d_old, d_new) = (self.delta(i, prev), self.delta(i, prev + q));
        self.toks[t].sub(&d_old);
        self.toks[t].add(&d_new);
        self.qty[i] = prev + q;
        self.log.push(Undo::Add { i, prev, tok: rec_tok, first, holders, leg_bytes });
    }

    fn rollback(&mut self, mark: usize) {
        while self.log.len() > mark {
            match self.log.pop().expect("log entry") {
                Undo::Add { i, prev, tok, first, holders, leg_bytes } => {
                    self.qty[i] = prev;
                    self.toks[tok.0] = tok.1;
                    if first {
                        self.seq.pop();
                        for (id, h) in holders.into_iter().rev() {
                            match h {
                                Some(h) => self.holder.insert(id, h),
                                None => self.holder.remove(&id),
                            };
                        }
                    }
                    self.leg_bytes = leg_bytes;
                }
                Undo::Chunk => {
                    self.chunks.pop();
                }
                Undo::Head(l, h) => self.head[l] = h,
            }
        }
    }

    /// Σ bid-side spends − Σ ask-side proceeds of the direct legs, each the exact rounded covenant value of the leg's
    /// whole fill (`Cand::value`), plus the KAS tips the pair orders release (`floor(n·tip/scale(A))`; their legs move no
    /// other KAS of their own: a route's KAS is the bids' and the asks').
    fn margin(&self) -> i64 {
        let m: i128 = self.seq.iter().map(|&i| leg_kas(&self.u.cands[i], self.qty[i])).sum();
        m.clamp(i64::MIN as i128, i64::MAX as i128) as i64
    }

    /// The estimated fee: the fills' bytes at the fee rate and each update's cost ([`update_cost`]).
    fn fee(&self, fee_rate: u64) -> i64 {
        let ups: i64 = self.ups.iter().fold(0i64, |a, x| a.saturating_add(x.cost));
        est_fee(self.bytes() - self.up_bytes, fee_rate).saturating_add(ups)
    }

    /// `margin + update tips + kept inventory (its value less its output's fee) − fee`.
    fn profit(&self, fee_rate: u64) -> i64 {
        if self.seq.is_empty() {
            return i64::MIN;
        }
        self.margin().saturating_add(self.tips).saturating_add(self.kept_worth()).saturating_sub(self.fee(fee_rate))
    }

    /// The token surpluses this allocation keeps as the operator's inventory ([`InventoryPolicy`]): every token a pair ask
    /// of the allocation buys (whose delivery would otherwise take the surplus) with a surplus the policy values above the
    /// fee of the operator's token output: (token, base units, value after the haircut).
    fn kept(&self) -> Vec<(usize, Market, i64, i128)> {
        let u = self.u;
        let mut out = vec![];
        if !u.inv.accept_surplus_tokens {
            return out;
        }
        let mut seen: BTreeSet<usize> = BTreeSet::new();
        for &i in &self.seq {
            let Some(x) = &u.cands[i].pair else { continue };
            if x.role != PairRole::Buy || x.info.buy_exact() || self.qty[i] <= 0 || !seen.insert(u.tok[i]) {
                continue;
            }
            let t = &self.toks[u.tok[i]];
            let Ok(q) = i64::try_from(t.sold - t.bought) else { continue };
            let m = x.info.t_market();
            let v = self.keep_value(&m, q, None);
            if v > est_fee(KEPT_OUTPUT_BYTES, u.fee_rate) as i128 {
                out.push((u.tok[i], m, q, v));
            }
        }
        out
    }

    /// Σ over [`Alloc::kept`] of the value less the fee of its token output.
    fn kept_worth(&self) -> i64 {
        let out = est_fee(KEPT_OUTPUT_BYTES, self.u.fee_rate) as i128;
        let w: i128 = self.kept().iter().map(|k| k.3 - out).sum();
        w.clamp(0, i64::MAX as i128) as i64
    }

    /// What `q` base units of market `m`'s token are worth kept as the operator's inventory under the policy, after the
    /// haircut; 0 when the policy does not accept the token or the amount.
    ///
    /// The value is the owner's `refPrice`, or else what the plain resting KAS bids of `m` pay for `q` (best first, each up
    /// to what it has left: its cap less what this allocation fills of it, less `used`, the in-transaction sales a netting
    /// estimate already counts), counting only bids a sale could fill (they accept a fill of their minimum fill or of all
    /// they have left). The best KAS bid alone never
    /// values a surplus: its depth and quantity rules bound what counts. Without `refPrice` the amount must be at least
    /// `minAmount`, by default the smallest such fill of the bids valued (no unsellable dust).
    fn keep_value(&self, m: &Market, q: i64, used: Option<&BTreeMap<usize, i64>>) -> i128 {
        let u = self.u;
        let Some(r) = u.inv.rule(&m.token) else { return 0 };
        if q <= 0 {
            return 0;
        }
        // the operator's token output holds at most the program's largest output amount (KRON)
        let limit = token_limits(m.family, &m.template).and_then(|l| l.max_output_amount);
        if limit.is_some_and(|cap| q as i128 > cap as i128) {
            return 0;
        }
        if let Some(p) = r.ref_price {
            if r.min_amount.is_some_and(|a| q < a) {
                return 0;
            }
            return u.inv.haircut(p.value(q));
        }
        let mut rest = q;
        let mut v: i128 = 0;
        let mut dust = i64::MAX;
        for &i in u.surplus_bids.get(m).map(Vec::as_slice).unwrap_or(&[]) {
            if rest <= 0 {
                break;
            }
            if !self.live[i] {
                continue;
            }
            let c = &u.cands[i];
            let left = self.caps[i] - self.qty[i] - used.and_then(|x| x.get(&i)).copied().unwrap_or(0);
            if left <= 0 {
                continue;
            }
            // a fill a later sale could make: its minimum fill, or all it has
            let probe = c.min_fill.max(1).min(c.cap);
            if !c.quantity_ok(probe) {
                continue;
            }
            let take = rest.min(left);
            let Some(k) = c.value(take) else { continue };
            v += k as i128;
            rest -= take;
            dust = dust.min(probe);
        }
        if v <= 0 || q < r.min_amount.unwrap_or(dust) {
            return 0;
        }
        u.inv.haircut(v)
    }

    /// Whether `a` and `p` may trade with each other at all (sides, books, covenant relations, the route rules: a pair order
    /// leg trades with the plain KAS orders of its token, never with another pair leg, which nets instead).
    fn pair_ok(&self, a: usize, p: usize) -> bool {
        let (ca, cp) = (&self.u.cands[a], &self.u.cands[p]);
        if cp.side == ca.side
            || cp.id == ca.id
            || Some(cp.id) == ca.merge
            || cp.merge == Some(ca.id)
            || cp.parent == Some(ca.id)
            || ca.parent == Some(cp.id)
            || (cp.merge.is_some() && cp.merge == ca.merge)
        {
            return false;
        }
        match (&ca.pair, &cp.pair) {
            (None, None) => ca.book == cp.book,
            (Some(x), None) | (None, Some(x)) => {
                let other = if ca.pair.is_none() { ca } else { cp };
                other.is_plain()
                    && match x.role {
                        PairRole::Sell => x.info.route && other.side == Side::Bid && other.book.market() == x.info.s_market(),
                        PairRole::Buy => x.info.route && other.side == Side::Ask && other.book.market() == x.info.t_market(),
                        PairRole::Surplus => other.side == Side::Bid && other.book.market() == x.info.a,
                    }
            }
            (Some(_), Some(_)) => false,
        }
    }

    /// The chunk `a` and `p` can trade now: (quantity of `a`, quantity of `p`) in base units of their token (the same
    /// number: one token changes hands), or None. The passive's minimum fill holds after the chunk unless the chunk ends
    /// it; the active's is checked when its walk ends (it may still take more).
    fn chunk(&self, a: usize, p: usize) -> Option<(i64, i64)> {
        let (bid, ask) = if self.u.cands[a].side == Side::Bid { (a, p) } else { (p, a) };
        let (cb, cs) = (&self.u.cands[bid], &self.u.cands[ask]);
        if !crosses(cb, cs) {
            return None;
        }
        let rem_a = self.caps[a] - self.qty[a];
        let rem_p = self.caps[p] - self.qty[p];
        if rem_a <= 0 || rem_p <= 0 {
            return None;
        }
        if let Some(x) = cb.pair.as_ref().filter(|x| x.role == PairRole::Buy) {
            // A pair order's purchase: exactly what its receipt still needs, from an ask in any amount of at least the ask's
            // minimum fill (or all it has left); a pair ask's purchase may take more when a minimum fill forces it (the
            // excess rides on its delivery), a bid's never.
            let exact = x.info.buy_exact();
            let rem_ask = self.caps[ask] - self.qty[ask];
            let need = self.caps[bid] - self.qty[bid];
            let mut q = need.min(rem_ask);
            if let Some((_, hi)) = cs.fok {
                // a FOK ask is consumed whole or not at all
                if self.qty[ask] > 0 || hi > rem_ask || (exact && hi > need) {
                    return None;
                }
                q = hi;
            }
            let floor = (cs.min_fill - self.qty[ask]).max(0);
            if q < floor && !cs.ends(self.qty[ask] + q) {
                if exact {
                    return None;
                }
                q = floor.min(rem_ask);
            }
            if q <= 0 {
                return None;
            }
            return Some((q, q));
        }
        let cp = &self.u.cands[p];
        let mut q = rem_a.min(rem_p);
        if let Some((lo, hi)) = cp.fok {
            if self.qty[p] > 0 {
                return None;
            }
            q = hi.min(rem_a);
            if q < lo {
                return None;
            }
        }
        if let Some((_, hi)) = self.u.cands[a].fok {
            q = q.min(hi - self.qty[a]);
        }
        if q <= 0 {
            return None;
        }
        // the passive's minimum fill: a chunk that leaves it below only when the chunk ends it
        if self.qty[p] + q < cp.min_fill && !cp.ends(self.qty[p] + q) {
            return None;
        }
        Some((q, q))
    }

    /// The exact KAS a direct chunk adds to the batch: the rounded value of both legs after the chunk less before (each
    /// leg's whole fill is rounded once, so this is the chunk's true effect on the margin). None with a pair order leg.
    fn chunk_gain(&self, bid: usize, ask: usize, q: i64) -> Option<i128> {
        let (cb, cs) = (&self.u.cands[bid], &self.u.cands[ask]);
        if cb.pair.is_some() || cs.pair.is_some() {
            return None;
        }
        let (b0, s0) = (self.qty[bid], self.qty[ask]);
        let paid = cb.value(b0 + q)? as i128 - cb.value(b0)? as i128;
        let owed = cs.value(s0 + q)? as i128 - cs.value(s0)? as i128;
        Some(paid - owed)
    }

    /// Can the walker `a` (side of `a`) still cross anything from list position `k` on?
    fn can_cross_from(&self, a: usize, list: &List, k: usize) -> bool {
        let pa = self.u.cands[a].per_base();
        let b = list.bound[k];
        match list.side {
            Side::Ask => cmp_frac(pa, b) != Ordering::Less,
            Side::Bid => cmp_frac(b, pa) != Ordering::Less,
        }
    }

    /// A necessary condition for a chunk that adds `asks` new ask-side and `bids` new bid-side legs to the token of `i`, and
    /// `extra` bytes, to fit: a new ask always takes a token input; a new bid takes an output unless the chunk sells out a leg
    /// that keeps a remainder (one at most per chunk). Used to stop scanning once the transaction is full.
    fn room(&self, i: usize, asks: u32, bids: u32, extra: u64) -> bool {
        let t = &self.toks[self.u.tok[i]];
        let lim = &self.u.toks[self.u.tok[i]].limits;
        let close = u32::from(t.rests > 0);
        ((t.tin + asks) as usize) <= lim.max_in
            && ((t.tout + bids).saturating_sub(close) as usize) <= lim.max_out
            && self.bytes() + extra <= self.max_bytes
    }

    /// Match active candidate `a` against list `l` (already in priority order): every crossing chunk it can take.
    fn walk(&mut self, a: usize, l: usize) {
        if !self.usable(a) {
            return;
        }
        let ca_side = self.u.cands[a].side;
        let a_bytes = if self.qty[a] == 0 && !self.holder.contains_key(&self.u.cands[a].id) { self.u.cands[a].bytes } else { 0 };
        let (a_ask, a_bid) =
            if self.qty[a] == 0 { (u32::from(ca_side == Side::Ask), u32::from(ca_side == Side::Bid)) } else { (0, 0) };
        if self.qty[a] == 0 && !self.room(a, a_ask, a_bid, a_bytes) {
            return;
        }
        let u = self.u;
        let list = &u.lists[l];
        let mark = self.log.len();
        // the head of the list: skip what no walk can use any more
        let mut h = self.head[l];
        while h < list.items.len() && self.dead(list.items[h]) {
            h += 1;
        }
        if h != self.head[l] {
            self.log.push(Undo::Head(l, self.head[l]));
            self.head[l] = h;
        }
        let new_passives = self.room(
            a,
            a_ask + u32::from(list.side == Side::Ask),
            a_bid + u32::from(list.side == Side::Bid),
            a_bytes + list.min_bytes,
        );
        if new_passives {
            let mut k = h;
            while k < list.items.len() {
                if self.out_of_time() || !self.usable(a) || !self.can_cross_from(a, list, k) {
                    break;
                }
                let p = list.items[k];
                k += 1;
                self.try_pair(a, p);
            }
        } else {
            // no room for another leg: only the legs already in the transaction, in list order
            let mut ps: Vec<(usize, usize)> =
                self.seq.iter().filter_map(|&p| list.pos.get(&p).map(|&k| (k, p))).filter(|(k, _)| *k >= h).collect();
            ps.sort_unstable();
            for (_, p) in ps {
                if self.out_of_time() || !self.usable(a) {
                    break;
                }
                self.try_pair(a, p);
            }
        }
        // Hard quantity rules of the active order: roll the whole walk back (a pair order's legs are reconciled instead).
        let n = self.qty[a];
        if n > 0 && self.u.cands[a].pair.is_none() && !self.u.cands[a].quantity_ok(n) {
            self.rollback(mark);
        }
    }

    fn try_pair(&mut self, a: usize, p: usize) {
        self.u.pair_tries.set(self.u.pair_tries.get() + 1);
        if p == a || !self.usable(p) || !self.pair_ok(a, p) {
            return;
        }
        let Some((na, np)) = self.chunk(a, p) else { return };
        let (bid, ask, q) = if self.u.cands[a].side == Side::Bid { (a, p, na) } else { (p, a, np) };
        // a direct chunk whose rounded amounts lose (a tiny chunk at a thin spread) is never added
        if self.chunk_gain(bid, ask, q).is_some_and(|g| g < 0) {
            return;
        }
        if !self.fits(&[(a, na), (p, np)]) {
            return;
        }
        self.add(a, na);
        self.add(p, np);
        let (b, s, qb, qs) = if self.u.cands[a].side == Side::Bid { (a, p, na, np) } else { (p, a, np, na) };
        self.chunks.push((b, s, qb, qs));
        self.log.push(Undo::Chunk);
    }

    /// Whether pair order `p`'s legs agree on one fill n (the S it releases exactly, the T it buys covering its receipt,
    /// its quantity rules).
    fn pair_consistent(&self, p: usize) -> bool {
        let u = self.u;
        let (s, b) = u.porders[p];
        let x = u.info(p);
        let (qs, qt) = (self.qty[s], self.qty[b]);
        let n = self.qty[u.primary(p)];
        n > 0
            && x.quantity_ok(n)
            && x.sell_qty(n) == Some(qs)
            && match x.buy_qty(n) {
                Some(need) if x.buy_exact() => need == qt,
                Some(need) => need <= qt,
                None => false,
            }
    }

    /// Netting (§3.5): in every token pair, the pair orders selling X for Y against those selling Y for X, best first, any
    /// number per side; each participant's fill is taken at its covenant amounts, the group's token balances kept exact (a
    /// surplus only where a pair ask buys that token). The netted quantities are added to both legs of every participant;
    /// their surplus is offered to the plain KAS bids of its token.
    fn net_pairs(&mut self) {
        let u = self.u;
        for nb in &u.nets {
            let mut out: BTreeSet<usize> = BTreeSet::new();
            let mut pruned = false;
            for _ in 0..=(nb.g1.len() + nb.g2.len() + 1) {
                if self.deadline.is_some_and(|d| Instant::now() >= d) {
                    return;
                }
                match self.net_group(nb, &out) {
                    NetResult::Empty => break,
                    NetResult::Exclude(p) => {
                        out.insert(p);
                    }
                    NetResult::Done(parts, steps, sx, sy) => {
                        // a netting moves no KAS: the orders' tips and what the surplus may fetch at the best KAS bids pay
                        // its bytes, or the group nets only its self-funding orders, or not at all (a book of untipped
                        // crossing pair orders must not cost every allocation the profit step's drops)
                        if self.net_value(nb, &parts, sx, sy) < 0 {
                            if pruned {
                                break;
                            }
                            pruned = true;
                            for &(p, n, _) in &parts {
                                let x = u.info(p);
                                if (x.tip_kas(n) as i128) < est_fee(u.cands[u.porders[p].0].bytes, u.fee_rate) as i128 {
                                    out.insert(p);
                                }
                            }
                            continue;
                        }
                        if self.inject(nb, &parts, &steps, sx, sy) {
                            self.netted.extend(parts.iter().map(|x| x.0));
                            break;
                        }
                        // the group does not fit the transaction: leave its last participant out
                        match parts.last() {
                            Some(&(p, _, _)) => {
                                out.insert(p);
                            }
                            None => break,
                        }
                    }
                }
            }
        }
    }

    /// What a netting group earns on its own: the orders' KAS tips and its surplus at the best plain KAS bids, less the fee
    /// of the orders' bytes.
    fn net_value(&self, nb: &NetBook, parts: &[(usize, i64, bool)], sx: i64, sy: i64) -> i128 {
        let u = self.u;
        let mut v: i128 = 0;
        let mut bytes: u64 = 0;
        for &(p, n, _) in parts {
            v += u.info(p).tip_kas(n) as i128;
            bytes += u.cands[u.porders[p].0].bytes;
        }
        for (m, q) in [(&nb.mx, sx), (&nb.my, sy)] {
            let mut used: BTreeMap<usize, i64> = BTreeMap::new();
            let (sold, kas) = self.surplus_value(m, q, &mut used);
            v += kas;
            // what the bids cannot take goes to a pair ask's delivery, or (policy) to the operator's inventory
            let keep = self.keep_value(m, q - sold, Some(&used));
            let out = est_fee(KEPT_OUTPUT_BYTES, u.fee_rate) as i128;
            if keep > out {
                v += keep - out;
            }
        }
        v - est_fee(bytes, u.fee_rate) as i128
    }

    /// The KAS `q` base units of a netting surplus of market `m` fetch at the plain KAS bids of `m`, best first, each bid
    /// taking only an amount its own quantity rules accept (at least its minimum fill, or everything it has left), and the
    /// base units sold (`used`: per bid). A surplus below every bid's minimum fill fetches nothing: it goes to the delivery
    /// of a pair ask and pays no fee (or, under the inventory policy, to the operator's inventory, [`Alloc::keep_value`]).
    fn surplus_value(&self, m: &Market, q: i64, used: &mut BTreeMap<usize, i64>) -> (i64, i128) {
        let u = self.u;
        let mut rest = q;
        let mut v: i128 = 0;
        for &i in u.surplus_bids.get(m).map(Vec::as_slice).unwrap_or(&[]) {
            if rest <= 0 {
                break;
            }
            if !self.live[i] {
                continue;
            }
            let c = &u.cands[i];
            let take = rest.min(c.cap);
            if !c.quantity_ok(take) {
                continue;
            }
            if let Some(k) = c.value(take) {
                v += k as i128;
                rest -= take;
                used.insert(i, take);
            }
        }
        (q - rest, v)
    }

    /// The target fill of pair order `p` in this allocation (its reconciled cap).
    fn ncap(&self, p: usize) -> i64 {
        self.caps[self.u.primary(p)].min(self.u.info(p).cap)
    }

    /// One greedy netting pass of a token pair without the participants `out`.
    fn net_group(&self, nb: &NetBook, out: &BTreeSet<usize>) -> NetResult {
        let u = self.u;
        let usable = |p: usize| {
            let (s, b) = u.porders[p];
            !out.contains(&p) && self.live[s] && self.live[b] && self.qty[s] == 0 && self.qty[b] == 0 && self.usable(s)
        };
        let mut n: BTreeMap<usize, i64> = BTreeMap::new();
        let (mut sx, mut bx, mut sy, mut by) = (0i128, 0i128, 0i128, 0i128);
        let (mut abs_x, mut abs_y) = (false, false);
        // ids the participants spend (an order, a merged entry, a booked exit's entry): one input per covenant id
        let mut ids: BTreeSet<CovId> = BTreeSet::new();
        let ids_of = |p: usize| -> Vec<CovId> {
            let c = &u.cands[u.porders[p].0];
            std::iter::once(c.id).chain(c.merge).chain(c.parent).collect()
        };
        let blocked = |p: usize, n: &BTreeMap<usize, i64>, ids: &BTreeSet<CovId>| {
            !usable(p)
                || n.get(&p).copied().unwrap_or(0) >= self.ncap(p)
                || (!n.contains_key(&p) && ids_of(p).iter().any(|i| ids.contains(i)))
        };
        // the token slots bound a group: a seller of X takes an input of X and an output of Y, a seller of Y the other way
        // round, so no more of either side than both tokens' slots hold (the group never grows past one transaction)
        let (gx, gy) = match (nb.g1.first(), nb.g2.first()) {
            (Some(&i), Some(&j)) => {
                let (lx, ly) = (&u.toks[u.tok[u.porders[i].0]].limits, &u.toks[u.tok[u.porders[j].0]].limits);
                (lx.max_in.min(ly.max_out), ly.max_in.min(lx.max_out))
            }
            _ => (0, 0),
        };
        let (mut kx, mut ky) = (0usize, 0usize);
        let (mut ii, mut jj) = (0usize, 0usize);
        let mut order: Vec<usize> = vec![];
        let mut steps: Vec<(usize, usize)> = vec![];
        let mut guard = 0usize;
        loop {
            guard += 1;
            u.net_steps.set(u.net_steps.get() + 1);
            if guard > 64 * (nb.g1.len() + nb.g2.len()) + 64 {
                break;
            }
            while ii < nb.g1.len() && blocked(nb.g1[ii], &n, &ids) {
                ii += 1;
            }
            while jj < nb.g2.len() && blocked(nb.g2[jj], &n, &ids) {
                jj += 1;
            }
            if ii >= nb.g1.len() || jj >= nb.g2.len() {
                break;
            }
            let (i, j) = (nb.g1[ii], nb.g2[jj]);
            if (!n.contains_key(&i) && kx >= gx) || (!n.contains_key(&j) && ky >= gy) {
                break;
            }
            let (xi, xj) = (u.info(i), u.info(j));
            // compatible: what i asks per unit of X it sells is at most what j pays per unit of X it buys
            let ((si, ti), (sj, tj)) = (xi.whole(), xj.whole());
            if (ti as i128) * (tj as i128) > (si as i128) * (sj as i128) {
                break;
            }
            let (ni, nj) = (n.get(&i).copied().unwrap_or(0), n.get(&j).copied().unwrap_or(0));
            let (ci, cj) = (self.ncap(i), self.ncap(j));
            let (Some(sold_i), Some(cap_sold_i), Some(bought_j), Some(cap_bought_j)) =
                (xi.sell_qty(ni), xi.sell_qty(ci), xj.buy_qty(nj), xj.buy_qty(cj))
            else {
                break;
            };
            let mut dx = (cap_sold_i - sold_i).min(cap_bought_j - bought_j);
            let mut accepted = None;
            for _ in 0..48 {
                if dx <= 0 {
                    break;
                }
                let ni2 = xi.n_of_sell(sold_i.saturating_add(dx)).clamp(ni, ci);
                let nj2 = xj.n_of_buy(bought_j.saturating_add(dx)).clamp(nj, cj);
                if ni2 > ni && nj2 > nj {
                    let d = |x: &PairInfo, a: i64, b: i64, f: fn(&PairInfo, i64) -> Option<i64>| -> Option<i128> {
                        Some(f(x, b)? as i128 - f(x, a)? as i128)
                    };
                    let (Some(dsx), Some(dby), Some(dsy), Some(dbx)) = (
                        d(xi, ni, ni2, PairInfo::sell_qty),
                        d(xi, ni, ni2, PairInfo::buy_qty),
                        d(xj, nj, nj2, PairInfo::sell_qty),
                        d(xj, nj, nj2, PairInfo::buy_qty),
                    ) else {
                        break;
                    };
                    let (sx2, bx2, sy2, by2) = (sx + dsx, bx + dbx, sy + dsy, by + dby);
                    let (ax2, ay2) = (abs_x || !xj.buy_exact(), abs_y || !xi.buy_exact());
                    if sx2 >= bx2 && sy2 >= by2 && (sx2 == bx2 || ax2) && (sy2 == by2 || ay2) {
                        accepted = Some((ni2, nj2, sx2, bx2, sy2, by2, ax2, ay2));
                        break;
                    }
                }
                dx /= 2;
            }
            let Some((ni2, nj2, sx2, bx2, sy2, by2, ax2, ay2)) = accepted else { break };
            for (p, k) in [(i, &mut kx), (j, &mut ky)] {
                if !n.contains_key(&p) {
                    order.push(p);
                    ids.extend(ids_of(p));
                    *k += 1;
                }
            }
            if steps.last() != Some(&(i, j)) {
                steps.push((i, j));
            }
            n.insert(i, ni2);
            n.insert(j, nj2);
            (sx, bx, sy, by, abs_x, abs_y) = (sx2, bx2, sy2, by2, ax2, ay2);
        }
        if n.is_empty() {
            return NetResult::Empty;
        }
        // the netted part of every participant keeps its quantity rules on its own (a route may add to it later)
        for &p in &order {
            if !u.info(p).quantity_ok(n[&p]) {
                return NetResult::Exclude(p);
            }
        }
        let parts = order.iter().map(|&p| (p, n[&p], u.net_of[p].is_some_and(|(_, g1)| g1))).collect();
        NetResult::Done(parts, steps, (sx - bx).max(0) as i64, (sy - by).max(0) as i64)
    }

    /// Adds a netting group's quantities to the allocation (both legs of every participant, token by token), with the
    /// surplus offers' caps; false (nothing added) when the group does not fit.
    fn inject(&mut self, nb: &NetBook, parts: &[(usize, i64, bool)], steps: &[(usize, usize)], sx: i64, sy: i64) -> bool {
        let u = self.u;
        let mark = self.log.len();
        // token X: the sales of the g1 participants and the purchases of the g2 ones; token Y the other way round
        let mut xs: Vec<(usize, i64)> = vec![];
        let mut ys: Vec<(usize, i64)> = vec![];
        for &(p, n, g1) in parts {
            let (s, b) = u.porders[p];
            let x = u.info(p);
            let (Some(qs), Some(qt)) = (x.sell_qty(n), x.buy_qty(n)) else { return false };
            if g1 {
                xs.push((s, qs));
                ys.push((b, qt));
            } else {
                ys.push((s, qs));
                xs.push((b, qt));
            }
        }
        for adds in [&xs, &ys] {
            if adds.is_empty() {
                continue;
            }
            if !self.fits(adds) {
                self.rollback(mark);
                return false;
            }
            for &(i, q) in adds.iter() {
                self.add(i, q);
            }
        }
        // the partners of the chunks (for the marginal profit): the pairs the greedy walk matched, in both tokens (linear in
        // the group's size)
        for &(i, j) in steps {
            let ((si, bi), (sj, bj)) = (u.porders[i], u.porders[j]);
            for (b, s) in [(bj, si), (bi, sj)] {
                self.chunks.push((b, s, self.qty[b], self.qty[s]));
                self.log.push(Undo::Chunk);
            }
        }
        self.caps[nb.sur_x] += sx;
        self.caps[nb.sur_y] += sy;
        true
    }
}

/// One netting pass's outcome.
enum NetResult {
    /// Nothing nets.
    Empty,
    /// Leave this participant out and net again (its netted part breaks its quantity rules).
    Exclude(usize),
    /// The participants (pair order, n, sells X) in netting order, the pairs the walk matched (X seller, Y seller), and the
    /// surplus of X and of Y.
    Done(Vec<(usize, i64, bool)>, Vec<(usize, usize)>, i64, i64),
}

/// The KAS a leg filling `n` base units moves for the operator: a direct bid's exact payment (`+`), a direct ask's exact
/// proceeds (`−`), a pair order's released tip on its primary leg (`+`; its other leg, and a surplus offer, move no KAS of
/// their own). A value the covenant cannot compute is never built: it counts as a loss of everything.
fn leg_kas(c: &Cand, n: i64) -> i128 {
    match (&c.pair, c.side) {
        (Some(x), _) if x.is_primary() => x.info.tip_kas(n) as i128,
        (Some(_), _) => 0,
        (None, Side::Bid) => c.value(n).map(|v| v as i128).unwrap_or(i64::MIN as i128),
        (None, Side::Ask) => -(c.value(n).map(|v| v as i128).unwrap_or(i64::MAX as i128)),
    }
}

/// One pass of the class walks with the given liveness and caps.
fn allocate<'u>(
    u: &'u Universe,
    caps: &[i64],
    live: &'u [bool],
    order: &[usize],
    max_bytes: u64,
    deadline: Option<Instant>,
) -> Alloc<'u> {
    u.allocations.set(u.allocations.get() + 1);
    let mut al = Alloc::new(u, caps, live, max_bytes, deadline);
    let mut rank = vec![0usize; u.comp_key.len()];
    for (r, &c) in order.iter().enumerate() {
        rank[c] = r;
    }
    // §3.5: netting first; what a netted order may route on top is bounded by its route's own profit
    al.net_pairs();
    for (p, &r) in u.rcap.iter().enumerate() {
        if r == i64::MAX {
            continue;
        }
        let (s, b) = u.porders[p];
        let x = u.info(p);
        let lim = al.qty[u.primary(p)].saturating_add(r).min(x.cap);
        if let (Some(qs), Some(qt)) = (x.sell_qty(lim), x.buy_qty(lim)) {
            al.caps[s] = al.caps[s].min(qs.max(al.qty[s]));
            al.caps[b] = al.caps[b].min(qt.max(al.qty[b]));
        }
    }
    let mut actives = u.actives.clone();
    // classes first, then the component order (the most profitable books take the transaction first)
    actives.sort_by_key(|&a| (u.cands[a].class, rank[u.comp[a]]));
    for a in actives {
        if !live[a] {
            continue;
        }
        if deadline.is_some_and(|d| Instant::now() >= d) {
            return al;
        }
        if let Some(l) = u.walks[a] {
            al.walk(a, l);
        }
    }
    for &c in order {
        for &(b, l) in &u.resting[c] {
            if !live[b] {
                continue;
            }
            if al.out_of_time() {
                return al;
            }
            al.walk(b, l);
        }
    }
    // §4: the stops the batch's plain fills trigger, then the updates they pay for
    al.trigger_walks();
    al.plan_updates();
    al
}

/// The allocation state an attempt carries: liveness and the pair orders' reconciled caps.
#[derive(Clone)]
struct State {
    live: Vec<bool>,
    xcap: Vec<i64>,
}

impl State {
    fn exclude(&mut self, u: &Universe, id: &CovId) {
        for &i in u.by_id.get(id).into_iter().flatten() {
            self.live[i] = false;
        }
    }
}

/// Owned result of a settled allocation.
struct Settled {
    st: State,
    caps: Vec<i64>,
}

/// Runs the class walks until the quantity rules hold and every pair order's legs agree (caps only decrease).
fn settle(u: &Universe, mut st: State, max_bytes: u64, order: &[usize], deadline: Option<Instant>) -> Settled {
    const ROUNDS: usize = 64;
    for round in 0..=ROUNDS {
        let caps = u.caps(&st.xcap);
        let al = allocate(u, &caps, &st.live, order, max_bytes, deadline);
        let bad: Vec<CovId> = al
            .seq
            .iter()
            .filter(|&&i| u.cands[i].pair.is_none() && !u.cands[i].quantity_ok(al.qty[i]))
            .map(|&i| u.cands[i].id)
            .collect();
        let mut changed = !bad.is_empty();
        let qty = al.qty.clone();
        drop(al);
        for id in bad {
            st.exclude(u, &id);
        }
        if !changed {
            for (p, &(s, b)) in u.porders.iter().enumerate() {
                if !st.live[s] {
                    continue;
                }
                let x = u.info(p);
                let (qs, qt) = (qty[s], qty[b]);
                if qs == 0 && qt == 0 {
                    continue;
                }
                let target = st.xcap[p].min(x.cap);
                let n = target.min(x.n_of_sell(qs)).min(x.n_of_buy(qt));
                let agree = n == target
                    && x.quantity_ok(n)
                    && x.sell_qty(n) == Some(qs)
                    && match x.buy_qty(n) {
                        Some(need) if x.buy_exact() => need == qt,
                        Some(need) => need <= qt,
                        None => false,
                    };
                if agree {
                    continue;
                }
                // the largest fill both legs reached (the S sold exactly, the receipt covered), within the order's own
                // quantity rules (FOK, its minimum fill unless it takes everything left); caps only decrease
                let new = if x.quantity_ok(n) && n < target && round < ROUNDS { n } else { 0 };
                if new == 0 {
                    let id = u.cands[s].id;
                    st.exclude(u, &id);
                } else {
                    st.xcap[p] = new;
                }
                changed = true;
            }
        }
        if !changed || deadline.is_some_and(|d| Instant::now() >= d) {
            return Settled { caps, st };
        }
    }
    let caps = u.caps(&st.xcap);
    Settled { caps, st }
}

/// A removable unit of an allocation: one direct leg, a pair order with both its legs, or a surplus offer.
fn units_of(u: &Universe, al: &Alloc) -> Vec<Vec<usize>> {
    let mut out = vec![];
    let mut done = BTreeSet::new();
    for &i in &al.seq {
        if done.contains(&i) {
            continue;
        }
        match u.twin[i] {
            Some(t) => {
                done.insert(t);
                out.push(vec![i.min(t), i.max(t)]);
            }
            None => out.push(vec![i]),
        }
        done.insert(i);
    }
    out
}

/// The units of an allocation with the pair orders netted together merged into one (a netting group stands or falls as a
/// group: one participant's own marginal misses the surplus and the partners it enables).
fn netting_units(u: &Universe, al: &Alloc, units: Vec<Vec<usize>>) -> Vec<Vec<usize>> {
    let (mut group, mut out): (Vec<usize>, Vec<Vec<usize>>) = (vec![], vec![]);
    for un in units {
        if un.iter().any(|&i| u.porder_of[i].is_some_and(|p| al.netted.contains(&p))) {
            group.extend(un);
        } else {
            out.push(un);
        }
    }
    if !group.is_empty() {
        group.sort_unstable();
        out.push(group);
    }
    out
}

/// Marginal profit of removing a unit: the margin of its chunks (and a pair order's tip) less the fee of its bytes and of
/// the counterparties only it trades with. Units of direct legs that also trade with a pair order leg are left to the pair
/// order's own unit (None).
fn marginal(u: &Universe, al: &Alloc, unit: &[usize], fee_rate: u64) -> Option<i64> {
    let is_pair = unit.iter().any(|&i| u.cands[i].pair.is_some());
    let mut margin: i128 = unit.iter().map(|&i| if u.cands[i].pair.is_some() { leg_kas(&u.cands[i], al.qty[i]) } else { 0 }).sum();
    // the netting group carries the inventory its surplus leaves the operator (the policy's value less its output's fee)
    if unit.iter().any(|&i| u.porder_of[i].is_some_and(|p| al.netted.contains(&p))) {
        margin += al.kept_worth() as i128;
    }
    let mut partners: BTreeMap<usize, bool> = BTreeMap::new();
    for &(b, s, qb, qs) in &al.chunks {
        let in_b = unit.contains(&b);
        let in_s = unit.contains(&s);
        if !in_b && !in_s {
            continue;
        }
        if !is_pair && (u.cands[b].pair.is_some() || u.cands[s].pair.is_some()) {
            return None;
        }
        let cb = &u.cands[b];
        let cs = &u.cands[s];
        // the chunk at the legs' exact rounded rates (a chunk of a pair order leg: the direct side only)
        let mb = if cb.pair.is_none() { cb.value(qb).map(|v| v as i128).unwrap_or(i64::MIN as i128) } else { 0 };
        let ms = if cs.pair.is_none() { cs.value(qs).map(|v| v as i128).unwrap_or(i64::MAX as i128) } else { 0 };
        margin += mb - ms;
        let other = if in_b { s } else { b };
        partners.entry(other).or_insert(true);
    }
    // a partner that also trades with someone else stays
    for &(b, s, _, _) in &al.chunks {
        for (x, y) in [(b, s), (s, b)] {
            if let Some(only) = partners.get_mut(&x) {
                if !unit.contains(&y) {
                    *only = false;
                }
            }
        }
    }
    let mut ids: BTreeSet<CovId> = BTreeSet::new();
    let mut bytes: u64 = 0;
    for &i in unit.iter().chain(partners.iter().filter(|(_, &only)| only).map(|(p, _)| p)) {
        if ids.insert(u.cands[i].id) {
            bytes += u.cands[i].bytes;
        }
    }
    Some((margin - est_fee(bytes, fee_rate) as i128).clamp(i64::MIN as i128, i64::MAX as i128) as i64)
}

/// Most breakpoints [`route_cap`] evaluates for one pair order; beyond it the order keeps its whole size.
const MAX_STANDALONE_STEPS: usize = 4_096;

/// One side of a pair order's route as cumulative base units and exact KAS over the orders of a list (best first).
struct Ladder<'a> {
    items: Vec<&'a Cand>,
    /// (base units, KAS) taken from the orders before index k, each order whole.
    cum: Vec<(i128, i128)>,
}

impl<'a> Ladder<'a> {
    fn new(u: &'a Universe, items: &[usize]) -> Option<Ladder<'a>> {
        let mut l = Ladder { items: vec![], cum: vec![(0, 0)] };
        for &i in items {
            let c = &u.cands[i];
            let (units, kas) = l.cum[l.cum.len() - 1];
            let v = c.value(c.cap)? as i128;
            l.cum.push((units.checked_add(c.cap as i128)?, kas.checked_add(v)?));
            l.items.push(c);
        }
        Some(l)
    }
    fn total(&self) -> i128 {
        self.cum[self.cum.len() - 1].0
    }
    /// The KAS of `want` base units along the ladder: the whole orders before the one `want` ends in, and that one's part at
    /// its exact rounded value (with `forced`: at least its minimum fill, or all it has: the minimum fill an ask imposes on
    /// a pair ask's purchase). None when the ladder does not hold `want`.
    fn kas(&self, want: i128, forced: bool) -> Option<i128> {
        if want <= 0 {
            return Some(0);
        }
        let k = self.cum.partition_point(|&(units, _)| units < want);
        if k >= self.cum.len() {
            return None;
        }
        let (u0, v0) = self.cum[k - 1];
        let c = self.items[k - 1];
        let mut take = i64::try_from(want - u0).ok()?;
        if forced && take < c.min_fill && !c.ends(take) {
            take = c.min_fill.min(c.cap);
        }
        Some(v0 + c.value(take)? as i128)
    }
}

/// Whether pair order `p` faces a compatible opposite pair order it could net with.
fn may_net(u: &Universe, p: usize) -> bool {
    let Some((k, in_g1)) = u.net_of[p] else { return false };
    let nb = &u.nets[k];
    // the opposite side is sorted best first: its first order is the most compatible
    let Some(&q) = (if in_g1 { nb.g2.first() } else { nb.g1.first() }) else { return false };
    let ((si, ti), (sj, tj)) = (u.info(p).whole(), u.info(q).whole());
    (ti as i128) * (tj as i128) <= (si as i128) * (sj as i128)
}

/// The largest fill of pair order `p` whose route could pay on its own, or None when no fill can (`docs/spec/matcher.md`
/// §3.5).
///
/// The deeper a route reaches the worse its prices (bids of the token it sells best first, asks of the token it buys best
/// first), an ask's minimum fill may force a pair ask's purchase beyond its delivery, and the implied quotes that rank the
/// legs are only the prices at the top of both books: without a bound such an order takes liquidity, token slots and the
/// other orders' tokens and sinks the batch it is in. The bound: the S sold along the plain bids of S (best first, each at
/// its exact rounded value; no slot cap), the T bought along the plain asks of T (best first, an ask's part at least its
/// minimum fill for a pair ask), plus the KAS tip, less the fee of the order's own bytes, against `min_profit`. It is
/// evaluated at every breakpoint of the two ladders and refined within the highest segment that pays. A FOK order is only
/// tried at its whole size; an auction whose price still moves only at the largest fill the books reach. Pure; numbers too
/// large to judge, or more than [`MAX_STANDALONE_STEPS`] breakpoints, keep the whole size. An order with a netting partner
/// is bounded only in what it routes on top of its netted part ([`Universe::rcap`]); an unarmed pair stop (routed only next
/// to its evidence) keeps its whole size.
fn route_cap(u: &Universe, p: usize, cfg: &PlannerConfig) -> Option<i64> {
    let (s, b) = u.porders[p];
    let x = u.info(p);
    let c = &u.cands[s];
    let whole = Some(x.cap);
    if !x.route {
        return None;
    }
    let (Some(lb), Some(la)) = (u.walks[s], u.walks[b]) else { return None };
    let (Some(bids), Some(asks)) = (Ladder::new(u, &u.lists[lb].items), Ladder::new(u, &u.lists[la].items)) else {
        return whole;
    };
    let clamp = |v: i128| v.clamp(0, i64::MAX as i128) as i64;
    // the largest fill the books reach: the S the bids take, whose receipt the asks hold
    let n_cap = (x.cap as i128).min(x.n_of_sell(clamp(bids.total())) as i128).min(x.n_of_buy(clamp(asks.total())) as i128);
    if n_cap <= 0 {
        return None;
    }
    let fee = est_fee(BASE_BYTES + c.bytes, cfg.fee_rate) as i128;
    let forced = !x.buy_exact();
    // the route's margin at a fill of n (None: too large to judge or not held by the books)
    let margin = |n: i128| -> Option<i128> {
        let n64 = i64::try_from(n).ok()?;
        let sold = x.sell_qty(n64)? as i128;
        let need = x.buy_qty(n64)? as i128;
        Some(bids.kas(sold, false)? - asks.kas(need, forced)? + x.tip_kas(n64) as i128 - fee)
    };
    let pays = |n: i128| margin(n).map(|m| m >= cfg.min_profit as i128);
    if x.fok {
        // only the whole size: it pays, or the order is left out
        return match pays(x.cap as i128) {
            Some(true) if x.cap as i128 <= n_cap => whole,
            Some(_) => None,
            None => whole,
        };
    }
    if x.relaxing {
        // judged at the largest reach only (its price moves until the route pays it)
        return match pays(n_cap) {
            Some(false) => None,
            _ => Some(n_cap as i64),
        };
    }
    // breakpoints: where a bid of S ends, where the receipt of a fill ends an ask of T, and the reach
    let mut pts: Vec<i128> = bids.cum.iter().map(|&(units, _)| x.n_of_sell(clamp(units)) as i128).collect();
    for &(units, _) in &asks.cum {
        pts.push(x.n_of_buy(clamp(units)) as i128);
    }
    pts.push(n_cap);
    pts.retain(|&n| n >= 1 && n <= n_cap);
    pts.sort_unstable();
    pts.dedup();
    if pts.len() > MAX_STANDALONE_STEPS {
        return whole;
    }
    // the highest breakpoint that pays, and the next one above it (the margin is close to linear between them)
    let mut best: Option<i128> = None;
    let mut above: Option<i128> = None;
    for k in (0..pts.len()).rev() {
        match pays(pts[k]) {
            None => return whole,
            Some(true) => {
                best = Some(pts[k]);
                above = pts.get(k + 1).copied();
                break;
            }
            Some(false) => {}
        }
    }
    let mut lo = best?;
    // refine within the segment above it: the largest fill that still pays (a bisection; the segment is near-linear)
    if let Some(mut hi) = above {
        while hi - lo > 1 {
            let mid = lo + (hi - lo) / 2;
            match pays(mid) {
                Some(true) => lo = mid,
                Some(false) => hi = mid,
                None => return whole,
            }
        }
    }
    // the order's own quantity rule: a fill below its minimum fill only when it takes everything left
    let n = lo as i64;
    if !x.quantity_ok(n) {
        return if x.quantity_ok(x.min_fill) && x.min_fill as i128 <= n_cap && pays(x.min_fill as i128) == Some(true) {
            Some(x.min_fill)
        } else {
            None
        };
    }
    Some(n)
}

/// Orders an allocation must not keep: a quantity rule broken, a pair order whose two legs disagree (the S released
/// exactly at its reconciled cap, its receipt covered), or tokens left to the operator. Empty for every settled
/// allocation; the deadline can cut a settlement short.
fn offenders(u: &Universe, al: &Alloc) -> Vec<CovId> {
    let mut v = vec![];
    for &i in &al.seq {
        if u.cands[i].pair.is_none() && !u.cands[i].quantity_ok(al.qty[i]) {
            v.push(u.cands[i].id);
        }
    }
    for (p, &(s, b)) in u.porders.iter().enumerate() {
        let (qs, qt) = (al.qty[s], al.qty[b]);
        if qs == 0 && qt == 0 {
            continue;
        }
        let n = al.qty[u.primary(p)];
        if !al.pair_consistent(p) || n != al.caps[u.primary(p)].min(u.info(p).cap) {
            v.push(u.cands[s].id);
        }
    }
    // no token left to the operator, no token delivered that was not released; a KRON token's surplus fits the delivery of
    // every pair ask that may take it (the builder keeps the excess of a KRON delivery above the program's output limit with
    // the taker: the operator)
    for (k, t) in al.toks.iter().enumerate() {
        let surplus = t.sold - t.bought;
        let kron_over = |i: usize| {
            u.toks[k].limits.max_output_amount.is_some_and(|cap| {
                surplus > 0
                    && u.cands[i].pair.as_ref().is_some_and(|x| x.role == PairRole::Buy && !x.info.buy_exact())
                    && al.qty[i] as i128 + surplus > cap as i128
            })
        };
        let bad = t.op_out() || surplus < 0;
        for &i in &al.seq {
            if u.tok[i] == k && u.cands[i].pair.is_some() && u.role(i) != Some(PairRole::Surplus) && (bad || kron_over(i)) {
                v.push(u.cands[i].id);
            }
        }
    }
    v
}

/// IOC fills of an allocation (never shrunk to fatten a batch, §3.2): a pair order's by its fill n.
fn ioc_fills(u: &Universe, al: &Alloc) -> BTreeMap<CovId, i64> {
    al.seq
        .iter()
        .filter(|&&i| u.cands[i].ioc && u.cands[i].pair.as_ref().is_none_or(|x| x.is_primary()))
        .map(|&i| (u.cands[i].id, al.qty[i]))
        .collect()
}

/// The component order: decreasing profit per byte of each component on its own (the allocation with no byte budget),
/// then the component key.
fn density_order(u: &Universe, al: &Alloc, fee_rate: u64) -> Vec<usize> {
    let nc = u.comp_key.len();
    let mut margin = vec![0i128; nc];
    let mut bytes = vec![0u64; nc];
    let mut used = vec![false; nc];
    for &i in &al.seq {
        let c = &u.cands[i];
        let k = u.comp[i];
        used[k] = true;
        bytes[k] += c.bytes;
        margin[k] += leg_kas(c, al.qty[i]);
    }
    let score = |k: usize| -> i128 {
        if !used[k] || bytes[k] == 0 {
            return i128::MIN;
        }
        (margin[k] - est_fee(bytes[k], fee_rate) as i128).saturating_mul(1_000_000) / bytes[k] as i128
    };
    let mut order: Vec<usize> = (0..nc).collect();
    order.sort_by(|&a, &b| score(b).cmp(&score(a)).then(u.comp_key[a].cmp(&u.comp_key[b])));
    order
}

/// What planning cost, in units that do not depend on the machine: a regression that makes the planner do far more work shows
/// in these counts on every machine, where a wall-clock bound only shows it on a fast one (`tests/tick_cost_bound.rs`).
/// Deterministic for a given view and configuration as long as no deadline cut a plan short (`cut_short` 0).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PlanWork {
    /// Plans run.
    pub plans: u64,
    /// Allocation passes: one run of the class walks (netting, routes, direct walks, triggers) over the view. Settling,
    /// ordering the components and the profit step's drops each cost one or more; a plan's work is this count times the
    /// cost of one pass.
    pub allocations: u64,
    /// Candidate pairs the walks tried (the inner step of a pass).
    pub pair_tries: u64,
    /// Steps of the netting walks (one opposite pair order tried against a netting group).
    pub net_steps: u64,
    /// Plans whose deadline (the book budget, or the tick budget) had passed when they returned: the planner ran out of
    /// time and handed over what it had.
    pub cut_short: u64,
}

impl PlanWork {
    pub fn add(&mut self, o: PlanWork) {
        self.plans += o.plans;
        self.allocations += o.allocations;
        self.pair_tries += o.pair_tries;
        self.net_steps += o.net_steps;
        self.cut_short += o.cut_short;
    }
}

/// Plans the most profitable transaction over every book of the view, or None when nothing profitable crosses.
pub fn plan_batch(inp: &BatchInput, cfg: &PlannerConfig) -> Option<Plan> {
    plan_batch_counted(inp, cfg).0
}

/// [`plan_batch`] and what it cost ([`PlanWork`]).
pub fn plan_batch_counted(inp: &BatchInput, cfg: &PlannerConfig) -> (Option<Plan>, PlanWork) {
    let mut u = Universe::new(inp, cfg);
    let plan = plan_universe(&mut u, inp, cfg);
    let work = PlanWork {
        plans: 1,
        allocations: u.allocations.get(),
        pair_tries: u.pair_tries.get(),
        net_steps: u.net_steps.get(),
        cut_short: u64::from(inp.deadline.is_some_and(|d| Instant::now() >= d)),
    };
    (plan, work)
}

fn plan_universe(u: &mut Universe, inp: &BatchInput, cfg: &PlannerConfig) -> Option<Plan> {
    if u.cands.is_empty() {
        return None;
    }
    let n = u.cands.len();
    let mut st0 = State { live: vec![true; n], xcap: (0..u.porders.len()).map(|p| u.info(p).cap).collect() };
    // 0. every routed pair order at most the largest fill its route could pay on its own; out when no fill can and it has
    //    no netting partner; an order that may net routes at most that bound on top of its netted part
    let mut rcap = vec![i64::MAX; u.porders.len()];
    for (p, r) in rcap.iter_mut().enumerate() {
        if u.info(p).need.is_some() {
            continue;
        }
        if may_net(u, p) {
            *r = route_cap(u, p, cfg).unwrap_or(0);
            continue;
        }
        match route_cap(u, p, cfg) {
            Some(c) => st0.xcap[p] = st0.xcap[p].min(c),
            None => {
                let id = u.cands[u.porders[p].0].id;
                st0.exclude(u, &id);
            }
        }
    }
    u.rcap = rcap;
    let u = &*u;
    let natural: Vec<usize> = (0..u.comp_key.len()).collect();
    // 1. every component on its own (no byte budget): the profit per byte that orders the components (at most half of the
    //    time left)
    let order = if u.comp_key.len() > 1 {
        let half = inp.deadline.map(|d| {
            let now = Instant::now();
            now + d.saturating_duration_since(now) / 2
        });
        let s0 = settle(u, st0.clone(), u64::MAX, &natural, half);
        let al0 = allocate(u, &s0.caps, &s0.st.live, &natural, u64::MAX, half);
        density_order(u, &al0, cfg.fee_rate)
    } else {
        natural
    };
    // 2. the transaction. `dropped` is what the profit step decided (the standalone bounds and the units it dropped); every
    //    allocation is settled from it afresh, so a pair order the reconciliation left out only because another leg held its
    //    token (a fill the profit step then drops) competes again in the next settlement.
    let mut dropped = st0.clone();
    let mut cur = settle(u, st0, inp.max_bytes, &order, inp.deadline);
    fn run_on<'u>(u: &'u Universe, s: &'u Settled, order: &[usize], max_bytes: u64, deadline: Option<Instant>) -> Alloc<'u> {
        allocate(u, &s.caps, &s.st.live, order, max_bytes, deadline)
    }
    let (mb, dl) = (inp.max_bytes, inp.deadline);
    // 3. drop fills with negative marginal profit; class 1 is protected while the batch is profitable
    let out_of_time = || inp.deadline.is_some_and(|d| Instant::now() >= d);
    for protect in [true, false] {
        let mut kept: BTreeSet<Vec<usize>> = BTreeSet::new();
        loop {
            if out_of_time() {
                break;
            }
            let al = run_on(u, &cur, &order, mb, dl);
            let base = al.profit(cfg.fee_rate);
            if protect && base < cfg.min_profit {
                break;
            }
            let base_ioc = ioc_fills(u, &al);
            let mut neg: Vec<(i64, Vec<usize>)> = netting_units(u, &al, units_of(u, &al))
                .into_iter()
                .filter(|un| !(protect && u.cands[un[0]].class == Class::Immediate))
                .filter(|un| !kept.contains(un))
                .filter_map(|un| marginal(u, &al, &un, cfg.fee_rate).filter(|m| *m < 0).map(|m| (m, un)))
                .collect();
            drop(al);
            if neg.is_empty() {
                break;
            }
            neg.sort_by(|a, b| a.0.cmp(&b.0).then(u.cands[a.1[0]].id.cmp(&u.cands[b.1[0]].id)));
            let accept = |s: &Settled| -> bool {
                let a2 = run_on(u, s, &order, mb, dl);
                a2.profit(cfg.fee_rate) > base
                    && (!protect || {
                        let i2 = ioc_fills(u, &a2);
                        base_ioc.iter().all(|(k, v)| i2.get(k).copied().unwrap_or(0) >= *v)
                    })
            };
            let without = |sets: &[&Vec<usize>]| -> State {
                let mut st = dropped.clone();
                for un in sets {
                    let ids: BTreeSet<CovId> = un.iter().map(|&i| u.cands[i].id).collect();
                    for id in ids {
                        st.exclude(u, &id);
                    }
                }
                st
            };
            // all of them at once, else one at a time (most negative first)
            if neg.len() > 1 {
                let all: Vec<&Vec<usize>> = neg.iter().map(|(_, un)| un).collect();
                let st = without(&all);
                let s_all = settle(u, st.clone(), inp.max_bytes, &order, inp.deadline);
                if accept(&s_all) {
                    (dropped, cur) = (st, s_all);
                    continue;
                }
            }
            let mut progressed = false;
            for (_, un) in &neg {
                if out_of_time() {
                    break;
                }
                // A losing pair order may leave its liquidity to another pair order of the same tokens that loses on it as
                // well (the next in price order, worse prices than its own): drop those substitutes with it, all of them.
                // No cap: every pass excludes at least one more pair order, so the passes are bounded by their number (and
                // the deadline). A cap of 8 gave up a paying route behind ten losing cross limits on one bid (TN10 soak
                // 2026-10-02).
                let mut st = without(&[un]);
                let mut s1 = settle(u, st.clone(), inp.max_bytes, &order, inp.deadline);
                while !accept(&s1) && un.len() == 2 && !out_of_time() {
                    let a1 = run_on(u, &s1, &order, mb, dl);
                    let subs: Vec<CovId> = units_of(u, &a1)
                        .into_iter()
                        .filter(|v| v.len() == 2 && !(protect && u.cands[v[0]].class == Class::Immediate))
                        .filter(|v| marginal(u, &a1, v, cfg.fee_rate).is_some_and(|m| m < 0))
                        .map(|v| u.cands[v[0]].id)
                        .collect();
                    drop(a1);
                    if subs.is_empty() {
                        break;
                    }
                    for id in subs {
                        st.exclude(u, &id);
                    }
                    s1 = settle(u, st.clone(), inp.max_bytes, &order, inp.deadline);
                }
                if accept(&s1) {
                    (dropped, cur) = (st, s1);
                    progressed = true;
                    break;
                }
                kept.insert(un.clone());
            }
            if !progressed {
                break;
            }
        }
        if run_on(u, &cur, &order, mb, dl).profit(cfg.fee_rate) >= cfg.min_profit {
            break;
        }
    }
    // 4. a settlement the deadline cut short may leave a quantity rule or a route unreconciled: drop those orders
    for attempt in 0.. {
        let al = run_on(u, &cur, &order, mb, dl);
        let off = offenders(u, &al);
        if off.is_empty() {
            break;
        }
        drop(al);
        if attempt == 8 {
            return None;
        }
        let mut st = cur.st.clone();
        for id in off {
            st.exclude(u, &id);
        }
        cur = settle(u, st, inp.max_bytes, &order, inp.deadline);
    }
    let al = run_on(u, &cur, &order, mb, dl);
    if al.seq.is_empty() || al.profit(cfg.fee_rate) < cfg.min_profit || !offenders(u, &al).is_empty() {
        return None;
    }
    let plan = to_plan(u, &al, inp.lock_time, cfg);
    if plan.fills.is_empty() {
        return None;
    }
    Some(plan)
}

fn to_plan(u: &Universe, al: &Alloc, lock: u64, cfg: &PlannerConfig) -> Plan {
    // pair order fills lead the transaction (one per order: its primary leg carries the fill n), then the rest in
    // allocation order; surplus offers are no legs (their tokens are the netted orders')
    let mut order: Vec<usize> = vec![];
    let mut seen_p: BTreeSet<usize> = BTreeSet::new();
    for &i in &al.seq {
        if let Some(p) = u.porder_of[i] {
            if seen_p.insert(p) && al.qty[u.primary(p)] > 0 {
                order.push(u.primary(p));
            }
        }
    }
    order.extend(al.seq.iter().copied().filter(|&i| u.cands[i].pair.is_none()));
    // a fill's index in the plan is its leg index in the lowered batch (the evidence references); a pair order's legs both
    // map to its fill
    let mut at: BTreeMap<usize, usize> = order.iter().enumerate().map(|(k, &i)| (i, k)).collect();
    for (k, &i) in order.iter().enumerate() {
        if let Some(t) = u.twin[i] {
            at.insert(t, k);
        }
    }
    let fills: Vec<Fill> = order
        .iter()
        .map(|&i| {
            let (evidence, evidence_b) = match u.porder_of[i].and_then(|p| al.pev_of.get(&p)) {
                Some(&(e, eb)) => (at.get(&e).copied(), eb.and_then(|x| at.get(&x).copied())),
                None => (al.ev_of.get(&i).and_then(|e| at.get(e).copied()), None),
            };
            Fill { cand: u.cands[i].clone(), amount: al.qty[i], evidence, evidence_b }
        })
        .collect();
    let updates: Vec<PlanUpdate> = al
        .ups
        .iter()
        .map(|x| {
            let c = &u.ups[x.up];
            PlanUpdate {
                id: c.id,
                book: c.book,
                outpoint: c.outpoint,
                kind: x.kind,
                evidence: at[&x.ev],
                evidence_b: x.ev_b.map(|e| at[&e]),
                take: c.tip,
                cost: x.cost,
                steps: x.steps,
            }
        })
        .collect();
    // the inventory kept: one operator token output each (its bytes and fee on top of the legs')
    let kept: Vec<Kept> = al
        .kept()
        .into_iter()
        .map(|(_, m, amount, v)| Kept { token: m.token, amount, value: v.clamp(0, i64::MAX as i128) as i64 })
        .collect();
    let n_kept = kept.len() as u64;
    let bytes = al.bytes() + n_kept * KEPT_OUTPUT_BYTES;
    Plan {
        book: fills.first().map(|f| f.cand.book).unwrap_or(u.cands[al.seq[0]].book),
        lock_time: lock,
        fills,
        updates,
        margin: al.margin(),
        tips: al.tips,
        est_bytes: bytes,
        est_fee: al.fee(cfg.fee_rate).saturating_add(est_fee(KEPT_OUTPUT_BYTES, cfg.fee_rate).saturating_mul(n_kept as i64)),
        kept,
    }
}

#[cfg(test)]
mod tok_tests {
    use super::*;

    #[test]
    fn a_token_never_leaves_to_the_operator() {
        let lim = TokenLimits { max_in: 8, max_out: 8, max_output_amount: None };
        let base = Tok { tin: 2, tout: 2, sold: 10, bought: 10, legs: 2, ..Tok::default() };
        assert!(base.ok(&lim));
        // the legs take less than the batch releases and no pair ask buys the token: the rest would be inventory
        assert!(!Tok { bought: 9, ..base }.ok(&lim));
        // a pair ask buying the token takes the surplus on its delivery
        assert!(Tok { bought: 9, absorb: 1, ..base }.ok(&lim));
        // never more delivered than released
        assert!(!Tok { bought: 11, absorb: 1, ..base }.ok(&lim));
    }
}
