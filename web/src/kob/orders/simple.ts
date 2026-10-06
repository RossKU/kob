// Planner of the plain order kinds (intent-simple.ts): limit GTC / GTD / day / timed, IOC, FOK, market, streaming, close, TWAP, DCA,
// Dutch decay / rising bid. Pipeline: validate -> exact OrderState -> funding / tokens -> kob.build (createOrder) -> Disclosure.
//
// Reference: docs/spec/order-types.md, docs/spec/matcher.md sections 2.1 (quote functions), 10.2-10.5 (market, streaming, FOK / IOC,
// marketable limit), 10.10 (day orders), contracts/v2/KobAsk.sil and KobBid.sil. User-input problems are `issues` (stable codes,
// see ISSUE_CATALOG in common-issues.ts); nothing here throws for them.
import type { Disclosure, OrderPlan, PlanEnv, PlanIssue } from '../plan-types';
import type { OrderState } from '../types';
import type {
  Activation, CloseIntent, DcaIntent, DutchIntent, FokIntent, IocIntent, Lifetime, LimitIntent, MarketIntent, SimpleIntent, StreamingIntent, TwapIntent,
} from '../intent-simple';
import { SIMPLE_TYPES } from '../intent-simple';
import type { Duration, Expiry, ExpiryKind } from '../daa';
import {
  IOC_KILL_DAA, IOC_LIFE_DAA, MARKET_ACTIVATION_DAA, MARKET_AUCTION_DAA, SLIPPAGE_BPS, durationToDaa, expiryFor, secondsToNextMidnight, unixToDaa,
} from '../daa';
import {
  checkAmount, checkCarrierRatio, checkFok, checkMinFill, checkNotional, checkPrice, checkPriceBand, checkSelfTrade, checkTimes, checkTip, crossingTouch, depthWithin,
  referencePrice, touchPrice,
} from '../guards';
import { bpsFloor, ceilDiv, fitsI64, roundToTick } from '../units';
import {
  DEFAULT_BID_FILLS, buildCreate, carrierOf, defaultMinFillFor, failedPlan, finishPlan, hasError, heldAmount, issue, makeAsk, makeBid, makeDisclosure,
} from './common';

/** Internal fully-resolved order: everything the state needs, in planner terms (base units, prices per whole token). */
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

// ------------------------------------------------------------------------------------------------ small helpers

export function activationDaa(env: PlanEnv, a: Activation | undefined): bigint | null {
  if (!a) return null;
  return 'daa' in a ? a.daa : unixToDaa(env.clock, a.unixSeconds);
}

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

/** slope so that `from` reaches `to` after `durationDaa` at `step` DAA per step (rounded up: the end is reached no later). */
export function decaySlope(from: bigint, to: bigint, durationDaa: bigint, step: bigint): bigint {
  const diff = from > to ? from - to : to - from;
  if (diff === 0n) return 0n;
  return ceilDiv(diff, ceilDiv(durationDaa, step));
}

export const positiveDuration = (env: PlanEnv, d: Duration | undefined, dflt: bigint, field: string, pre: PlanIssue[]): bigint => {
  const v = d ? durationToDaa(env.clock, d) : dflt;
  if (v <= 0n) pre.push(issue('DURATION_INVALID', undefined, field));
  return v;
};

export function lifeDaa(env: PlanEnv, d: Duration | undefined, pre: PlanIssue[]): bigint {
  const v = d ? durationToDaa(env.clock, d) : IOC_LIFE_DAA;
  if (v < 1n || v > IOC_KILL_DAA) pre.push(issue('LIFE_OUT_OF_RANGE', { max: Number(IOC_KILL_DAA) }, 'life'));
  return v;
}

export function checkBps(bps: bigint, field: string, pre: PlanIssue[]): void {
  if (bps < 1n || bps > 9_999n) pre.push(issue('SLIPPAGE_INVALID', undefined, field));
  else if (bps > 1_000n) pre.push(issue('SLIPPAGE_HIGH', { bps }, field));
}

export function checkFills(fills: bigint, pre: PlanIssue[]): void {
  if (fills < 1n) pre.push(issue('MAX_FILLS_INVALID', undefined, 'maxFills'));
}

/** Worst price of an auction from `touch`: sell = touch minus the bound (rounded up to the tick), buy = plus the bound (rounded down). */
export function auctionBound(side: 'sell' | 'buy', touch: bigint, bps: bigint, tick: bigint): bigint {
  const move = bpsFloor(touch, bps);
  if (side === 'sell') {
    const end = roundToTick(touch - move, tick, 'up');
    return end > touch ? touch : end;
  }
  const end = roundToTick(touch + move, tick, 'down');
  return end < touch ? touch : end;
}

// ------------------------------------------------------------------------------------------------ the common tail

/** Guards, state, funding, build, disclosure. */
function place(env: PlanEnv, d: Draft, pre: PlanIssue[]): OrderPlan {
  const m = env.token;
  const issues = [...pre];
  issues.push(...checkAmount(d.amount, m, 'amount', d.side === 'sell'));
  issues.push(...checkTip(d.side, d.tip, d.reach));
  issues.push(...checkTimes(env.clock, { activeFrom: d.activeFrom, expiryDaa: d.expiryDaa, requestedActiveFrom: d.requestedActiveFrom }));
  if (hasError(issues)) return failedPlan(issues);
  // the wallet's default minimum fill: the amount worth 10 KAS at the limit (a resting order), 1 base unit (an immediate order); a TWAP / DCA
  // slice is never below it (minFill <= maxFill, or a slice could not fill)
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
    // the escrow of `amount` base units in at most `fills` fills, exactly as the covenant consumes it (kob-wasm bidEscrow)
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
    issues.push(...checkCarrierRatio(disclosure.kasLocked, disclosure.allInTotal));
  }
  return finishPlan(issues, [order], res, disclosure);
}

const blank = (side: 'sell' | 'buy', amount: bigint, price: bigint, tip: bigint, minFill: bigint | undefined): Draft => ({
  side, amount, minFill: minFill ?? null, immediate: false, price, tip, tif: 0, activeFrom: 0n, requestedActiveFrom: null, expiryDaa: 0n, interval: 0n,
  maxFill: 0n, slope: 0n, priceEnd: 0n, decayStep: 1n, deadline: null, fills: 1n, reach: price, expiryKind: 'gtc', expectedPrice: null, worstPrice: null,
  limitPrice: price, activatesAt: null, notes: [],
});

/**
 * Resting bids budget a delivery carrier per expected fill: as many fills as the minimum fill allows (`ceil(amount / minFill)`), at most
 * DEFAULT_BID_FILLS, unless the user asked for a number.
 */
export function restingFills(env: Pick<PlanEnv, 'kob' | 'token'>, amount: bigint, price: bigint, minFill: bigint | undefined, requested: bigint | undefined): bigint {
  if (requested !== undefined) return requested;
  const mf = minFill ?? defaultMinFillFor(env, amount, price);
  const possible = mf > 0n && amount > 0n ? ceilDiv(amount, mf) : 1n;
  return possible < DEFAULT_BID_FILLS ? possible : DEFAULT_BID_FILLS;
}

// ------------------------------------------------------------------------------------------------ limit

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

  pre.push(...checkPriceBand(referencePrice(env.book), i.side, i.price));
  const x = future ? { crossing: false, touch: null } : crossingTouch(env.book, i.side, i.price, tip);
  if (x.crossing && x.touch !== null) {
    const policy = i.crossing ?? 'auction';
    const start = i.side === 'sell' ? (x.touch > i.price ? x.touch : i.price) : x.touch < i.price ? x.touch : i.price;
    if (policy === 'reject') {
      pre.push(issue('MARKETABLE_REJECTED', { touch: x.touch }, 'price'));
    } else if (policy === 'auction' && start !== i.price) {
      // auction from the touch to the limit, resting at the limit afterwards (matcher.md 10.5)
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
      // a plain crossing limit fills at its own limit; the better price is the matcher's margin
      pre.push(issue('MARKETABLE_LIMIT_FILLS_AT_LIMIT', { touch: x.touch }, 'price'));
    }
  }
  return place(env, d, pre);
}

// ------------------------------------------------------------------------------------------------ IOC / FOK

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
  pre.push(...checkPriceBand(referencePrice(env.book), i.side, i.price));
  return place(env, d, pre);
}

// ------------------------------------------------------------------------------------------------ market / streaming / close

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
  notes: string[];
}

function planAuction(env: PlanEnv, r: AuctionRequest, pre: PlanIssue[]): OrderPlan {
  checkBps(r.bps, 'slippageBps', pre);
  const auctionDaa = positiveDuration(env, r.auction, MARKET_AUCTION_DAA, 'auction', pre);
  const life = lifeDaa(env, r.life, pre);
  const delay = r.activation ?? MARKET_ACTIVATION_DAA;
  if (delay < 0n) pre.push(issue('DURATION_INVALID', undefined, 'activation'));
  if (r.reference === null) pre.push(issue('NO_LIQUIDITY', { counterparty: r.side === 'sell' ? 'bids' : 'asks' }));
  if (hasError(pre)) return failedPlan(pre);
  const touch = r.reference as bigint;

  const end = auctionBound(r.side, touch, r.bps, env.token.tick);
  const d = blank(r.side, r.amount, touch, r.tip, r.minFill);
  d.immediate = true;
  d.tif = r.allOrNothing ? 2 : 1;
  d.expiryKind = r.allOrNothing ? 'fok' : 'ioc';
  d.activeFrom = env.clock.daa + delay;
  d.expiryDaa = d.activeFrom + life;
  d.slope = decaySlope(touch, end, auctionDaa, 1n);
  d.priceEnd = d.slope > 0n ? end : 0n;
  d.decayStep = 1n;
  d.reach = end;
  d.limitPrice = end;
  d.expectedPrice = touch;
  d.worstPrice = end;
  d.fills = 1n;
  d.notes.push('auction', r.allOrNothing ? 'fokAllOrNothing' : 'iocRemainderReturned', ...r.notes);
  pre.push(issue('MARKET_REFERENCE_SOURCE', { reference: touch, worst: end }));
  if (!r.allOrNothing) {
    const depth = depthWithin(env.book, r.side, end, r.tip);
    if (depth < r.amount) pre.push(issue('MARKET_DEPTH_INSUFFICIENT', { available: depth, amount: r.amount }));
  }
  return place(env, d, pre);
}

function planMarket(env: PlanEnv, i: MarketIntent): OrderPlan {
  const pre: PlanIssue[] = [];
  return planAuction(env, {
    side: i.side, amount: i.amount, tip: i.tip ?? 0n, minFill: i.minFill, reference: touchPrice(env.book, i.side), bps: i.slippageBps ?? SLIPPAGE_BPS,
    auction: i.auction, activation: i.activation, life: i.life, allOrNothing: i.allOrNothing ?? false, notes: ['market'],
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

// ------------------------------------------------------------------------------------------------ TWAP / DCA

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
  // the first slice opens `interval` after placement and every fill restarts the clock
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
  pre.push(...checkPriceBand(referencePrice(env.book), side, i.price));
  return place(env, d, pre);
}

// ------------------------------------------------------------------------------------------------ Dutch / rising

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
  // the decay origin IS activeFrom: it must be a real DAA score (0 would put the price at its end at once)
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
  pre.push(...checkPriceBand(referencePrice(env.book), i.side, i.priceEnd, 'priceEnd'));
  return place(env, d, pre);
}

// ------------------------------------------------------------------------------------------------ entry point

/**
 * Plans any simple order. Never throws for user-input problems (they come back as `issues` with severity 'error' and `ok: false`).
 * On success `built` is an unsigned `createOrder` transaction (kob.build) ready for the wallet, `states` the exact order state and
 * `disclosure` the numbers to show.
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
