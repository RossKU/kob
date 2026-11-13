// Planner of the plain order kinds (intent-simple.ts).
//
// The kinds are limit (GTC, GTD, day, timed), IOC, FOK, market, streaming, close, TWAP, DCA, Dutch
// decay and rising bid. The pipeline is validate, exact OrderState, funding and tokens,
// kob.build (createOrder), Disclosure.
//
// References are docs/spec/order-types.md, docs/spec/matcher.md sections 2.1 (quote functions),
// 10.2 to 10.5 (market, streaming, FOK and IOC, marketable limit) and 10.10 (day orders), and
// contracts/orders/KobAsk.sil and KobBid.sil. User-input problems are `issues` with stable codes
// (ISSUE_CATALOG in common-issues.ts). Nothing here throws for them.
import type { Disclosure, OrderPlan, PlanEnv, PlanFills, PlanIssue } from '../plan-types';
import type { OrderState } from '../types';
import type {
  Activation, CloseIntent, DcaIntent, DutchIntent, FokIntent, IocIntent, Lifetime, LimitIntent, MarketIntent, SimpleIntent, StreamingIntent, TwapIntent,
} from '../intent-simple';
import { SIMPLE_TYPES } from '../intent-simple';
import type { Duration, Expiry, ExpiryKind } from '../daa';
import {
  IOC_KILL_DAA, IOC_LIFE_DAA, MARKET_ACTIVATION_DAA, MARKET_AUCTION_DAA, SLIPPAGE_BPS, durationToDaa, expiryFor, secondsToNextMidnight, unixToDaaBound,
} from '../daa';
import {
  checkAmount, checkCarrierRatio, checkFok, checkMinFill, checkNotional, checkPrice, checkMarketBand, checkSelfTrade, checkTimes, checkTip, crossingTouch, depthWithin,
  estimateFills, touchPrice,
} from '../guards';
import { bpsFloor, ceilDiv, fitsI64, roundToTick } from '../units';
import {
  DEFAULT_BID_FILLS, buildCreate, carrierOf, defaultMinFillFor, failedPlan, finishPlan, hasError, heldAmount, issue, makeAsk, makeBid, makeDisclosure,
} from './common';

/** A fully resolved order: everything the state needs, in planner terms. */
interface Draft {
  side: 'sell' | 'buy';
  amount: bigint;
  /** the order's minFill: the intent's, else the wallet default (`immediate` orders: 1) */
  minFill: bigint | null;
  /** IOC / FOK / market / streaming / close: the default minimum fill is 1 base unit */
  immediate: boolean;
  price: bigint;
  tip: bigint;
  tif: 0 | 1 | 2;
  activeFrom: bigint;
  /** the activation the user asked for, to report a moment already in the past */
  requestedActiveFrom: bigint | null;
  expiryDaa: bigint;
  interval: bigint;
  maxFill: bigint;
  slope: bigint;
  priceEnd: bigint;
  decayStep: bigint;
  deadline: bigint | null;
  /** bids: delivery carriers budgeted */
  fills: bigint;
  /** the most aggressive price the order can trade at (limit / auction bound / decay end) */
  reach: bigint;
  expiryKind: ExpiryKind;
  expectedPrice: bigint | null;
  worstPrice: bigint | null;
  /** disclosure limit (limit or worst bound) */
  limitPrice: bigint;
  activatesAt: bigint | null;
  notes: string[];
}

const LEAD = MARKET_ACTIVATION_DAA;

// ---- small helpers

/** The activation as a DAA score, or null when the intent has none. */
export function activationDaa(env: PlanEnv, a: Activation | undefined): bigint | null {
  if (!a) return null;
  return 'daa' in a ? a.daa : unixToDaaBound(env.clock, a.unixSeconds, 'start');
}

/** Resolves a lifetime into an expiry. A day order ending within 5 minutes adds an issue. */
export function resolveLifetime(env: PlanEnv, lt: Lifetime | undefined, pre: PlanIssue[]): Expiry {
  const kind = lt?.kind ?? 'gtc';
  if (kind === 'gtd') return expiryFor('gtd', env.clock, { at: (lt as { at: bigint }).at });
  if (kind === 'day') {
    const e = expiryFor('day', env.clock, { kob: env.kob });
    const left = secondsToNextMidnight(env.clock.unixSeconds);
    if (left < 300n) pre.push(issue('DAY_ORDER_ENDS_SOON', { minutes: Number(left / 60n) }));
    return e;
  }
  return expiryFor('gtc', env.clock);
}

/** Slope so that `from` reaches `to` after `durationDaa` at `step` DAA per step, rounded up. */
export function decaySlope(from: bigint, to: bigint, durationDaa: bigint, step: bigint): bigint {
  const diff = from > to ? from - to : to - from;
  if (diff === 0n) return 0n;
  return ceilDiv(diff, ceilDiv(durationDaa, step));
}

/** A duration in DAA, or `dflt` when absent. A duration that is not positive adds an issue. */
export const positiveDuration = (env: PlanEnv, d: Duration | undefined, dflt: bigint, field: string, pre: PlanIssue[]): bigint => {
  const v = d ? durationToDaa(env.clock, d) : dflt;
  if (v <= 0n) pre.push(issue('DURATION_INVALID', undefined, field));
  return v;
};

/** The life of an IOC or FOK in DAA, or the default. A life out of range adds an issue. */
export function lifeDaa(env: PlanEnv, d: Duration | undefined, pre: PlanIssue[]): bigint {
  const v = d ? durationToDaa(env.clock, d) : IOC_LIFE_DAA;
  if (v < 1n || v > IOC_KILL_DAA) pre.push(issue('LIFE_OUT_OF_RANGE', { max: Number(IOC_KILL_DAA) }, 'life'));
  return v;
}

/** Checks a slippage tolerance in basis points. Above 10% it only warns. */
export function checkBps(bps: bigint, field: string, pre: PlanIssue[]): void {
  if (bps < 1n || bps > 9_999n) pre.push(issue('SLIPPAGE_INVALID', undefined, field));
  else if (bps > 1_000n) pre.push(issue('SLIPPAGE_HIGH', { bps }, field));
}

/** Checks the number of fills a bid budgets. */
export function checkFills(fills: bigint, pre: PlanIssue[]): void {
  if (fills < 1n) pre.push(issue('MAX_FILLS_INVALID', undefined, 'maxFills'));
}

/** Worst price of an auction from `touch`: sell minus the bound (up to tick), buy plus it (down). */
export function auctionBound(side: 'sell' | 'buy', touch: bigint, bps: bigint, tick: bigint): bigint {
  const move = bpsFloor(touch, bps);
  if (side === 'sell') {
    const end = roundToTick(touch - move, tick, 'up');
    return end > touch ? touch : end;
  }
  const end = roundToTick(touch + move, tick, 'down');
  return end < touch ? touch : end;
}

// ---- the common tail

/** Guards, state, funding, build and disclosure: the tail every kind ends in. */
function place(env: PlanEnv, d: Draft, pre: PlanIssue[]): OrderPlan {
  const m = env.token;
  const issues = [...pre];
  issues.push(...checkAmount(d.amount, m, 'amount', d.side === 'sell'));
  issues.push(...checkTip(d.side, d.tip, d.reach));
  issues.push(...checkTimes(env.clock, { activeFrom: d.activeFrom, expiryDaa: d.expiryDaa, requestedActiveFrom: d.requestedActiveFrom }));
  if (hasError(issues)) return failedPlan(issues);
  // The wallet's default minimum fill is the amount worth 10 KAS at the limit (a resting order)
  // or 1 base unit (an immediate order). A TWAP or DCA slice is never below it, because minFill
  // must not exceed maxFill or a slice could not fill.
  let minFill = d.minFill ?? (d.immediate ? 1n : defaultMinFillFor(env, d.amount, d.limitPrice));
  if (d.minFill === null && d.maxFill > 0n && minFill > d.maxFill) minFill = d.maxFill;
  issues.push(...checkMinFill(minFill, d.amount));
  const top = d.price > d.priceEnd ? d.price : d.priceEnd;
  issues.push(...checkNotional(d.amount, d.side === 'sell' ? top : top + d.tip, m.scale));
  if (hasError(issues)) return failedPlan(issues);
  if (d.tif === 2) issues.push(...checkFok({ side: d.side, amount: d.amount, limit: d.reach, tip: d.tip, slots: m.slots, book: env.book }));
  issues.push(...checkSelfTrade(env.ownOrders, { side: d.side, price: d.reach, tip: d.tip }));
  if (hasError(issues)) return failedPlan(issues);

  const common = {
    minFill, price: d.price, tip: d.tip, tif: d.tif, activeFrom: d.activeFrom, expiryDaa: d.expiryDaa,
    interval: d.interval, maxFill: d.maxFill, slope: d.slope, priceEnd: d.priceEnd, decayStep: d.decayStep,
  };
  let order: OrderState;
  let value: bigint;
  let tokenAmount = 0n;
  if (d.side === 'sell') {
    order = makeAsk(env, { ...common, amount: d.amount });
    value = carrierOf(env);
    tokenAmount = d.amount;
  } else {
    order = makeBid(env, common);
    // The escrow of `amount` base units in at most `fills` fills, as the covenant consumes it.
    const escrow = env.kob.bidEscrow(order, d.amount, d.fills);
    if (escrow === null || !fitsI64(escrow)) return failedPlan([...issues, issue('AMOUNT_TOO_LARGE', undefined, 'amount')]);
    value = escrow;
  }
  const res = buildCreate(env, { order, value, tokenAmount, deadline: d.deadline });
  let disclosure: Disclosure | null = null;
  if (res.built) {
    disclosure = makeDisclosure(env, res, {
      order, amount: d.amount, limitPrice: d.limitPrice, expectedPrice: d.expectedPrice, worstPrice: d.worstPrice,
      expiryKind: d.expiryKind, activatesAt: d.activatesAt, fills: d.fills, notes: d.notes,
    });
    issues.push(...checkCarrierRatio(disclosure.carriers, disclosure.allInTotal));
  }
  return finishPlan(issues, [order], res, disclosure);
}

const blank = (side: 'sell' | 'buy', amount: bigint, price: bigint, tip: bigint, minFill: bigint | undefined): Draft => ({
  side, amount, minFill: minFill ?? null, immediate: false, price, tip, tif: 0, activeFrom: 0n, requestedActiveFrom: null, expiryDaa: 0n, interval: 0n,
  maxFill: 0n, slope: 0n, priceEnd: 0n, decayStep: 1n, deadline: null, fills: 1n, reach: price, expiryKind: 'gtc', expectedPrice: null, worstPrice: null,
  limitPrice: price, activatesAt: null, notes: [],
});

/**
 * The fills a resting bid budgets delivery carriers for.
 *
 * It is as many as the minimum fill allows (`ceil(amount / minFill)`), at most
 * DEFAULT_BID_FILLS, unless the user asked for a number.
 */
export function restingFills(env: Pick<PlanEnv, 'kob' | 'token'>, amount: bigint, price: bigint, minFill: bigint | undefined, requested: bigint | undefined): bigint {
  if (requested !== undefined) return requested;
  const mf = minFill ?? defaultMinFillFor(env, amount, price);
  const possible = mf > 0n && amount > 0n ? ceilDiv(amount, mf) : 1n;
  return possible < DEFAULT_BID_FILLS ? possible : DEFAULT_BID_FILLS;
}

// ---- limit

function planLimit(env: PlanEnv, i: LimitIntent): OrderPlan {
  const m = env.token;
  const pre: PlanIssue[] = [...checkPrice(m, i.price)];
  const tip = i.tip ?? 0n;
  if (i.maxFills !== undefined) checkFills(i.maxFills, pre);
  if (hasError(pre)) return failedPlan(pre);

  const d = blank(i.side, i.amount, i.price, tip, i.minFill);
  const requested = activationDaa(env, i.activeFrom);
  const future = requested !== null && requested > env.clock.daa;
  d.requestedActiveFrom = requested;
  d.activeFrom = future ? requested : 0n;
  d.activatesAt = future ? requested : null;
  const exp = resolveLifetime(env, i.lifetime, pre);
  d.expiryDaa = exp.expiryDaa;
  d.deadline = exp.deadline;
  d.expiryKind = exp.kind;
  d.notes.push(exp.kind === 'gtc' ? 'gtc' : exp.kind === 'day' ? 'dayOrder' : 'gtd');
  d.fills = i.side === 'buy' ? restingFills(env, i.amount, i.price, i.minFill, i.maxFills) : 1n;
  if (i.side === 'sell' && i.maxFills !== undefined) pre.push(issue('SIDE_INVALID', { side: 'sell' }, 'maxFills'));

  pre.push(...checkMarketBand(env.book, env.lastFillPrice, i.side, i.price));
  const x = future ? { crossing: false, touch: null } : crossingTouch(env.book, i.side, i.price, tip);
  if (x.crossing && x.touch !== null) {
    const policy = i.crossing ?? 'auction';
    const start = i.side === 'sell' ? (x.touch > i.price ? x.touch : i.price) : x.touch < i.price ? x.touch : i.price;
    if (policy === 'reject') {
      pre.push(issue('MARKETABLE_REJECTED', { touch: x.touch }, 'price'));
    } else if (policy === 'auction' && start !== i.price) {
      // An auction from the touch to the limit, resting at the limit afterwards (matcher.md 10.5).
      const dur = MARKET_AUCTION_DAA;
      d.price = start;
      d.priceEnd = i.price;
      d.slope = decaySlope(start, i.price, dur, 1n);
      d.decayStep = 1n;
      d.activeFrom = env.clock.daa + LEAD;
      d.activatesAt = null;
      d.reach = i.price;
      d.limitPrice = i.price;
      d.expectedPrice = x.touch;
      d.worstPrice = i.price;
      d.notes.push('auction', 'marketable');
      pre.push(issue('MARKETABLE_AUCTION', { touch: x.touch }, 'price'));
    } else if (start !== i.price) {
      // A plain crossing limit fills at its own limit. The better price is the matcher's margin.
      pre.push(issue('MARKETABLE_LIMIT_FILLS_AT_LIMIT', { touch: x.touch }, 'price'));
    }
    // A buy that crosses budgets fills against the asks it crosses. A sell has no fills to budget.
    if (policy !== 'reject' && i.side === 'buy') return crossingBuy(env, d, i.amount, i.price, tip, i.minFill, i.maxFills, pre);
  }
  return place(env, d, pre);
}

/**
 * Plans a buy that crosses the book now (a limit buy at or through the asks, or a rising bid whose
 * end does).
 *
 * What crosses needs about `est.fills` fills (guards.ts `estimateFills`), plus one for a rest that
 * will rest. Without the user's own Fills value the default (`restingFills`) is raised to that. It
 * stays at most MAX_DEFAULT_CROSSING_FILLS and never above the fills the minimum fill allows. So a
 * large crossing buy against many small asks is not left with an unfillable tail. Each extra fill
 * deposits one more delivery carrier, returned unused with a cancel or the refund. The user's value
 * stands, with a warning when it is short.
 */
function crossingBuy(env: PlanEnv, d: Draft, amount: bigint, bound: bigint, tip: bigint, minFill: bigint | undefined, wanted: bigint | undefined, pre: PlanIssue[]): OrderPlan {
  if (wanted === undefined) {
    const est = estimateFills({ side: 'buy', amount, limit: bound, tip, slots: env.token.slots, book: env.book });
    const need = BigInt(est.fills + (est.enough ? 0 : 1));
    const mf = minFill ?? defaultMinFillFor(env, amount, bound);
    const possible = mf > 0n ? ceilDiv(amount, mf) : 1n;
    const raised = need < MAX_DEFAULT_CROSSING_FILLS ? need : MAX_DEFAULT_CROSSING_FILLS;
    const target = raised < possible ? raised : possible;
    if (target > d.fills) d.fills = target;
  }
  const facts = fillsFacts(env, 'buy', amount, bound, tip, d.fills, null, pre, true);
  return withFills(place(env, d, pre), facts);
}

// ---- IOC / FOK

function planImmediate(env: PlanEnv, i: IocIntent | FokIntent): OrderPlan {
  const pre: PlanIssue[] = [...checkPrice(env.token, i.price)];
  const tip = i.tip ?? 0n;
  const life = lifeDaa(env, i.life, pre);
  if (hasError(pre)) return failedPlan(pre);
  const d = blank(i.side, i.amount, i.price, tip, i.minFill);
  d.immediate = true;
  const requested = activationDaa(env, i.activeFrom);
  const future = requested !== null && requested > env.clock.daa;
  d.requestedActiveFrom = requested;
  d.activeFrom = future ? requested : 0n;
  d.activatesAt = future ? requested : null;
  d.tif = i.type === 'fok' ? 2 : 1;
  d.expiryKind = i.type;
  d.expiryDaa = (future ? (requested as bigint) : env.clock.daa) + life;
  d.fills = 1n;
  d.notes.push(i.type === 'fok' ? 'fokAllOrNothing' : 'iocRemainderReturned');
  pre.push(...checkMarketBand(env.book, env.lastFillPrice, i.side, i.price));
  // FOK: the pre-check refuses what one transaction cannot fill (checkFok). An IOC fills what one
  // transaction takes and returns the rest.
  if (i.type === 'fok' || future) return place(env, d, pre);
  const facts = fillsFacts(env, i.side, i.amount, i.price, tip, 1n, 'ioc', pre);
  return withFills(place(env, d, pre), facts);
}

// ---- fills against the visible book

/**
 * Most fills the wallet budgets by default for a buy that crosses the book (a limit buy placed as
 * an auction, a rising bid).
 *
 * It is enough for the asks that cross now, instead of DEFAULT_BID_FILLS, which leaves the tail of
 * a large crossing buy unfillable. Every budgeted fill deposits one delivery carrier in the escrow.
 * A cancel or the refund returns the unused ones, and a used one rides on the delivered token UTXO.
 * The user's own Fills value always wins.
 */
export const MAX_DEFAULT_CROSSING_FILLS = 10n;

/**
 * The least time a multi-fill market order stays fillable at its slippage bound once its auction
 * has reached it (100 DAA, 10 s).
 *
 * The expiry is `activeFrom + max(life, auction + MARKET_FILLS_TAIL_DAA)`. The tail is long enough
 * for the matchers to chain the further transactions (matcher.md 7) and short enough that a
 * market order does not linger as a resting bid.
 */
export const MARKET_FILLS_TAIL_DAA = 100n;

/** Most fills a market order may take. A buy deposits one delivery carrier per fill. */
export const MAX_MARKET_FILLS = 64n;

/**
 * The fills of an order that takes liquidity, against the visible book up to its `bound`.
 *
 * It also warns when the fills cannot take the amount. One transaction (`oneTx`: a market order
 * of one fill, or an IOC) takes at most the token's slots of counterparties and the rest returns.
 * A resting order with too few budgeted fills keeps a tail no fill can take.
 */
function fillsFacts(env: PlanEnv, side: 'sell' | 'buy', amount: bigint, bound: bigint, tip: bigint, budgeted: bigint, oneTx: 'market' | 'ioc' | null, pre: PlanIssue[], crossing = false): PlanFills {
  const est = estimateFills({ side, amount, limit: bound, tip, slots: env.token.slots, book: env.book });
  const k = budgeted > BigInt(Number.MAX_SAFE_INTEGER) ? Number.MAX_SAFE_INTEGER : Number(budgeted);
  const facts: PlanFills = {
    budgeted, needed: est.fills, perTx: est.perTx, counterparty: side === 'sell' ? 'bids' : 'asks', fillable: est.fillableIn(k), enough: est.enough,
    carrierDeposit: side === 'buy' ? budgeted * carrierOf(env) : 0n, scope: oneTx === null && crossing ? 'crossing' : 'amount',
  };
  if (est.fills > k) {
    const params = { perTx: est.perTx, counterparty: facts.counterparty, needed: est.fills, fills: budgeted, fillable: facts.fillable, amount };
    pre.push(issue(oneTx === 'market' ? 'MARKET_ONE_TX_LIMIT' : oneTx === 'ioc' ? 'IOC_ONE_TX_LIMIT' : 'FILLS_BUDGET_SHORT', params, oneTx === 'ioc' ? 'amount' : 'maxFills'));
  }
  return facts;
}

const withFills = (p: OrderPlan, fills: PlanFills): OrderPlan => ({ ...p, fills });

// ---- market / streaming / close

interface AuctionRequest {
  side: 'sell' | 'buy';
  amount: bigint;
  tip: bigint;
  minFill?: bigint;
  /** the price the auction starts from: the touch (market) or the displayed price (streaming) */
  reference: bigint | null;
  bps: bigint;
  auction?: Duration;
  activation?: bigint;
  life?: Duration;
  allOrNothing: boolean;
  /** market only: the fills (transactions) the order may take, above 1 it rests as an auction */
  maxFills?: bigint;
  /** the ticket has a Fills box for this order (market), so its warning says to raise it */
  fillsBox?: boolean;
  notes: string[];
}

/**
 * A market, streaming or close auction from `reference` to the slippage bound.
 *
 * With one fill (the default) it is an IOC (tif 1), or a FOK (tif 2) with `allOrNothing`. The
 * unfilled rest returns at once. A market order with `maxFills` N above 1 is the same decaying
 * auction as a resting order (tif 0, the Dutch and rising machinery). A partial fill continues the
 * same auction origin, so it trades within the bound across up to N transactions. It expires
 * MARKET_FILLS_TAIL_DAA after the auction reaches its bound at the least. A buy budgets N delivery
 * carriers, capped at `ceil(amount / minFill)`. What is left at the expiry is refunded by anyone
 * or cancelled by the maker.
 *
 * Its minimum fill, unless the user set one, is the smaller of the wallet default and the smallest
 * crossing counterparty of the visible book, so small counterparties never block a fill. A filler
 * can at most make the order spend its N budgeted fills on fills of that minimum. The bound price
 * still holds for every fill (matcher.md 3.3 and 10.2).
 */
function planAuction(env: PlanEnv, r: AuctionRequest, pre: PlanIssue[]): OrderPlan {
  checkBps(r.bps, 'slippageBps', pre);
  const auctionDaa = positiveDuration(env, r.auction, MARKET_AUCTION_DAA, 'auction', pre);
  const life = lifeDaa(env, r.life, pre);
  const delay = r.activation ?? MARKET_ACTIVATION_DAA;
  if (delay < 0n) pre.push(issue('DURATION_INVALID', undefined, 'activation'));
  if (r.maxFills !== undefined) checkFills(r.maxFills, pre);
  if (r.maxFills !== undefined && r.maxFills > MAX_MARKET_FILLS) pre.push(issue('MAX_FILLS_TOO_LARGE', { max: Number(MAX_MARKET_FILLS) }, 'maxFills'));
  if (r.reference === null) pre.push(issue('NO_LIQUIDITY', { counterparty: r.side === 'sell' ? 'bids' : 'asks' }));
  if (hasError(pre)) return failedPlan(pre);
  const touch = r.reference as bigint;
  // FOK stays one transaction, so a fills value does not apply to it.
  const multi = !r.allOrNothing && (r.maxFills ?? 1n) > 1n;

  const end = auctionBound(r.side, touch, r.bps, env.token.tick);
  const d = blank(r.side, r.amount, touch, r.tip, r.minFill);
  d.activeFrom = env.clock.daa + delay;
  d.slope = decaySlope(touch, end, auctionDaa, 1n);
  d.priceEnd = d.slope > 0n ? end : 0n;
  d.decayStep = 1n;
  d.reach = end;
  d.limitPrice = end;
  d.expectedPrice = touch;
  d.worstPrice = end;
  pre.push(issue('MARKET_REFERENCE_SOURCE', { reference: touch, worst: end }));
  if (!r.allOrNothing) {
    const depth = depthWithin(env.book, r.side, end, r.tip);
    if (depth < r.amount) pre.push(issue('MARKET_DEPTH_INSUFFICIENT', { available: depth, amount: r.amount }));
  }
  if (!multi) {
    d.immediate = true;
    d.tif = r.allOrNothing ? 2 : 1;
    d.expiryKind = r.allOrNothing ? 'fok' : 'ioc';
    d.expiryDaa = d.activeFrom + life;
    d.fills = 1n;
    d.notes.push('auction', r.allOrNothing ? 'fokAllOrNothing' : 'iocRemainderReturned', ...r.notes);
    if (r.allOrNothing) return place(env, d, pre);
    // A market order can raise its fills. Streaming and close are one transaction, and their
    // warning says so without a Fills box.
    const facts = fillsFacts(env, r.side, r.amount, end, r.tip, 1n, r.fillsBox ? 'market' : 'ioc', pre);
    return withBuyingPower(env, withFills(place(env, d, pre), facts));
  }
  const est = estimateFills({ side: r.side, amount: r.amount, limit: end, tip: r.tip, slots: env.token.slots, book: env.book });
  const wallet = defaultMinFillFor(env, r.amount, end);
  const minFill = r.minFill ?? (est.smallest !== null && est.smallest < wallet ? (est.smallest > 0n ? est.smallest : 1n) : wallet);
  const possible = ceilDiv(r.amount, minFill > 0n ? minFill : 1n);
  const wanted = r.maxFills as bigint;
  d.minFill = minFill;
  d.tif = 0;
  d.expiryKind = 'gtd';
  d.expiryDaa = d.activeFrom + (life > auctionDaa + MARKET_FILLS_TAIL_DAA ? life : auctionDaa + MARKET_FILLS_TAIL_DAA);
  const allowed = wanted < possible ? wanted : possible;
  d.fills = r.side === 'buy' ? allowed : 1n;
  d.notes.push('auction', 'marketFills', ...r.notes);
  const facts = fillsFacts(env, r.side, r.amount, end, r.tip, allowed, null, pre);
  const clamp = wanted > allowed ? { requested: wanted, minFill } : { minFill };
  return withBuyingPower(env, withFills(place(env, d, pre), { ...facts, ...clamp }));
}

/**
 * Adds a buy's `maxReceive` to its fills: what its escrow lets the fills deliver.
 *
 * It comes from kob-wasm `bidBuyingPower` of the order state and its escrow. A bid's quantity is
 * its KAS escrow, so the carriers of budgeted fills that do not happen buy tokens too. A
 * multi-fill market buy can deliver more than the typed amount, never more KAS than it locks. A
 * sell, or a plan that did not build, is returned as it is.
 */
function withBuyingPower(env: PlanEnv, p: OrderPlan): OrderPlan {
  const order = p.states[0];
  if (!p.fills || !order || !p.request || (order.kind !== 'KobBid' && order.kind !== 'KobBidKron')) return p;
  return { ...p, fills: { ...p.fills, maxReceive: env.kob.bidBuyingPower(order, BigInt(p.request.value)) } };
}

function planMarket(env: PlanEnv, i: MarketIntent): OrderPlan {
  const pre: PlanIssue[] = [];
  return planAuction(env, {
    side: i.side, amount: i.amount, tip: i.tip ?? 0n, minFill: i.minFill, reference: touchPrice(env.book, i.side), bps: i.slippageBps ?? SLIPPAGE_BPS,
    auction: i.auction, activation: i.activation, life: i.life, allOrNothing: i.allOrNothing ?? false, ...(i.maxFills !== undefined ? { maxFills: i.maxFills } : {}), fillsBox: true, notes: ['market'],
  }, pre);
}

function planStreaming(env: PlanEnv, i: StreamingIntent): OrderPlan {
  const pre: PlanIssue[] = [];
  if (i.displayedPrice <= 0n) pre.push(issue('REFERENCE_PRICE_INVALID', undefined, 'displayedPrice'));
  return planAuction(env, {
    side: i.side, amount: i.amount, tip: i.tip ?? 0n, minFill: i.minFill, reference: i.displayedPrice > 0n ? i.displayedPrice : null, bps: i.toleranceBps,
    auction: i.auction, activation: i.activation, life: i.life, allOrNothing: i.allOrNothing ?? false, notes: ['streaming'],
  }, pre);
}

function planClose(env: PlanEnv, i: CloseIntent): OrderPlan {
  const pre: PlanIssue[] = [];
  const held = heldAmount(env);
  const amount = i.amount ?? held;
  if (i.amount === undefined && held <= 0n) return failedPlan([issue('CLOSE_NOTHING_TO_SELL')]);
  return planAuction(env, {
    side: 'sell', amount, tip: i.tip ?? 0n, reference: touchPrice(env.book, 'sell'), bps: i.slippageBps ?? SLIPPAGE_BPS,
    auction: i.auction, activation: i.activation, life: i.life, allOrNothing: i.allOrNothing ?? false, notes: ['market', 'close'],
  }, pre);
}

// ---- TWAP / DCA

function planSchedule(env: PlanEnv, i: TwapIntent | DcaIntent): OrderPlan {
  const side = i.type === 'twap' ? 'sell' : 'buy';
  if (i.side !== undefined && i.side !== side) return failedPlan([issue('SIDE_INVALID', { side: i.side }, 'side')]);
  const m = env.token;
  const pre: PlanIssue[] = [...checkPrice(m, i.price)];
  const tip = i.tip ?? 0n;
  const intervalDaa = positiveDuration(env, i.interval, 0n, 'interval', pre);
  if (i.sliceAmount < 1n) pre.push(issue('SLICE_AMOUNT_INVALID', undefined, 'sliceAmount'));
  if (i.priceEnd !== undefined) {
    pre.push(...checkPrice(m, i.priceEnd, 'priceEnd'));
    if (side === 'sell' ? i.priceEnd >= i.price : i.priceEnd <= i.price) pre.push(issue('PRICE_END_INVALID', { direction: side === 'sell' ? 'below' : 'above' }, 'priceEnd'));
  }
  const sliceAuction = positiveDuration(env, i.sliceAuction, MARKET_AUCTION_DAA, 'sliceAuction', pre);
  if (i.type === 'dca' && i.maxFills !== undefined) checkFills(i.maxFills, pre);
  if (hasError(pre)) return failedPlan(pre);

  const d = blank(side, i.amount, i.price, tip, i.minFill);
  const requested = activationDaa(env, i.activeFrom);
  const future = requested !== null && requested > env.clock.daa;
  d.requestedActiveFrom = requested;
  d.activeFrom = future ? requested : 0n;
  d.activatesAt = future ? requested : null;
  const exp = resolveLifetime(env, i.lifetime, pre);
  d.expiryDaa = exp.expiryDaa;
  d.deadline = exp.deadline;
  d.expiryKind = exp.kind;
  d.interval = intervalDaa;
  d.maxFill = i.sliceAmount > i.amount ? i.amount : i.sliceAmount;
  if (i.sliceAmount >= i.amount) pre.push(issue('TWAP_SINGLE_SLICE'));
  const slices = ceilDiv(i.amount, d.maxFill);
  // The first slice opens `interval` after placement and every fill restarts the clock.
  const span = slices * intervalDaa;
  const start = future ? (requested as bigint) : env.clock.daa;
  if (start + span > d.expiryDaa) pre.push(issue('TWAP_EXCEEDS_LIFE', { slices, intervalDaa }));
  if (i.priceEnd !== undefined) {
    d.priceEnd = i.priceEnd;
    d.slope = decaySlope(i.price, i.priceEnd, sliceAuction, 1n);
    d.reach = i.priceEnd;
    d.worstPrice = i.priceEnd;
    d.expectedPrice = i.price;
    d.limitPrice = i.priceEnd;
    d.notes.push('auction');
  } else {
    d.worstPrice = i.price;
  }
  d.notes.push(i.type, exp.kind === 'day' ? 'dayOrder' : 'gtc');
  if (side === 'buy') {
    const wanted = (i as DcaIntent).maxFills;
    if (wanted !== undefined && wanted < slices) pre.push(issue('MAX_FILLS_INVALID', undefined, 'maxFills'));
    d.fills = wanted !== undefined && wanted >= slices ? wanted : slices;
  }
  pre.push(...checkMarketBand(env.book, env.lastFillPrice, side, i.price));
  return place(env, d, pre);
}

// ---- Dutch / rising

function planDutch(env: PlanEnv, i: DutchIntent): OrderPlan {
  const m = env.token;
  const pre: PlanIssue[] = [...checkPrice(m, i.price), ...checkPrice(m, i.priceEnd, 'priceEnd')];
  const tip = i.tip ?? 0n;
  if (i.side === 'sell' ? i.priceEnd >= i.price : i.priceEnd <= i.price) pre.push(issue('PRICE_END_INVALID', { direction: i.side === 'sell' ? 'below' : 'above' }, 'priceEnd'));
  const duration = positiveDuration(env, i.duration, 0n, 'duration', pre);
  const step = i.stepDaa ?? 1n;
  if (step < 1n) pre.push(issue('DURATION_INVALID', undefined, 'stepDaa'));
  if (i.maxFills !== undefined) checkFills(i.maxFills, pre);
  if (hasError(pre)) return failedPlan(pre);

  const d = blank(i.side, i.amount, i.price, tip, i.minFill);
  const requested = activationDaa(env, i.activeFrom);
  const future = requested !== null && requested > env.clock.daa;
  d.requestedActiveFrom = requested;
  // The decay origin is activeFrom, so it must be a real DAA score (0 would put the price at its
  // end at once).
  d.activeFrom = future ? (requested as bigint) : env.clock.daa + LEAD;
  d.activatesAt = future ? requested : null;
  const exp = resolveLifetime(env, i.lifetime, pre);
  d.expiryDaa = exp.expiryDaa;
  d.deadline = exp.deadline;
  d.expiryKind = exp.kind;
  d.priceEnd = i.priceEnd;
  d.decayStep = step;
  d.slope = decaySlope(i.price, i.priceEnd, duration, step);
  d.reach = i.priceEnd;
  d.limitPrice = i.priceEnd;
  d.worstPrice = i.priceEnd;
  d.expectedPrice = i.price;
  d.fills = i.side === 'buy' ? restingFills(env, i.amount, i.priceEnd, i.minFill, i.maxFills) : 1n;
  d.notes.push('auction', i.side === 'sell' ? 'dutch' : 'rising');
  pre.push(...checkMarketBand(env.book, env.lastFillPrice, i.side, i.priceEnd, 'priceEnd'));
  // A rising bid whose end crosses visible asks is budgeted like a crossing limit buy.
  if (i.side === 'buy' && !future && crossingTouch(env.book, 'buy', i.priceEnd, tip).crossing) return crossingBuy(env, d, i.amount, i.priceEnd, tip, i.minFill, i.maxFills, pre);
  return place(env, d, pre);
}

// ---- entry point

/**
 * Plans any simple order. Never throws for user-input problems.
 *
 * Problems come back as `issues` with severity 'error' and `ok: false`. On success `built` is an
 * unsigned `createOrder` transaction (kob.build) ready for the wallet, `states` holds the exact
 * order state and `disclosure` the numbers to show.
 */
export function planSimple(env: PlanEnv, intent: SimpleIntent): OrderPlan {
  if (!SIMPLE_TYPES.includes(intent.type)) return failedPlan([issue('INTENT_UNKNOWN_TYPE', { type: String((intent as { type?: unknown }).type) })]);
  const amountIssues =
    intent.type === 'close' && intent.amount === undefined ? [] : checkAmount((intent as { amount: bigint }).amount, env.token, 'amount', intent.side !== 'buy');
  if (hasError(amountIssues)) return failedPlan(amountIssues);
  switch (intent.type) {
    case 'limit': return planLimit(env, intent);
    case 'ioc':
    case 'fok': return planImmediate(env, intent);
    case 'market': return planMarket(env, intent);
    case 'streaming': return planStreaming(env, intent);
    case 'close': return planClose(env, intent);
    case 'twap':
    case 'dca': return planSchedule(env, intent);
    case 'dutch': return planDutch(env, intent);
    default:
      return failedPlan([issue('INTENT_UNKNOWN_TYPE', { type: String((intent as { type?: unknown }).type) })]);
  }
}
