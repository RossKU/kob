// Planner of the plain pair kinds (`KobPair`, contracts/v2/KobPair.sil): the same intents as planSimple (intent-simple.ts) on a pair A/B: limit
// GTC / GTD / day / timed activation with the crossing policy, IOC, FOK, market, streaming, close (IOC / FOK auctions from the pair book's touch to
// the slippage bound), TWAP / DCA (interval / maxFill, optional per-slice auction), Dutch / rising.
//
// Units: `amount`, `minFill`, `sliceAmount` base units of A; `price`, `priceEnd`, `displayedPrice` B base units per WHOLE A; `tip` KAS (sompi per
// whole A, prefunded on the order UTXO, never part of a B price). sell -> side 1 ASK (holds exactly the amount of A), buy -> side 2 BID (holds a B
// escrow, kob-wasm `pairBidEscrow`: the whole amount at its highest price rounded down, plus one base unit). The order UTXO prefunds one delivery carrier per budgeted fill and the
// tip of the whole amount (kob-wasm `pairKasValue`). Defaults: resting minFill = the amount of A worth 10 KAS on A's KAS book
// (`defaultMinFillPair`, a quarter of the amount without a KAS reference), immediate orders 1; delivery carriers: resting one per possible fill, at most 64
// (or `maxFills`; fewer carriers than possible fills make what is left after them fill only in full), IOC / FOK / auctions 1, TWAP / DCA one per slice.
import type { PairOrderPlan, PairPlanEnv, PlanIssue } from '../plan-types';
import type {
  CloseIntent, DcaIntent, DutchIntent, FokIntent, IocIntent, LimitIntent, MarketIntent, SimpleIntent, StreamingIntent, TwapIntent,
} from '../intent-simple';
import { SIMPLE_TYPES } from '../intent-simple';
import type { Duration, ExpiryKind } from '../daa';
import { MARKET_ACTIVATION_DAA, MARKET_AUCTION_DAA, SLIPPAGE_BPS, daaToUnix } from '../daa';
import { checkAmount, checkFok, checkMinFill, checkPriceBand, checkSelfTrade, checkTimes, crossingTouch, depthWithin, referencePrice, touchPrice } from '../guards';
import { ceilDiv, maxBig } from '../units';
import { hasError, issue } from './common-issues';
import { activationDaa, auctionBound, checkBps, checkFills, decaySlope, lifeDaa, positiveDuration, resolveLifetime } from './simple';
import {
  checkPairNotional, defaultPairFills, checkPairPrice, checkPairTip, failedPairPlan, heldOf, makePairState, pairCarrierOf, pairKasLines, pairTipsOf, placePair,
  withCustody,
} from './pair-common';
import { pairIssue, type PairIssueCode } from './pair-issues';

/** Fully resolved plain pair order (planner terms: base units of A, B per whole A, KAS tip). */
interface Draft {
  side: 'sell' | 'buy';
  amount: bigint;
  /** the intent's minFill (null: the default) */
  minFill: bigint | null;
  /** IOC / FOK / market / streaming / close: default minimum fill 1, one delivery */
  immediate: boolean;
  price: bigint;
  tip: bigint;
  tif: 0 | 1 | 2;
  activeFrom: bigint;
  requestedActiveFrom: bigint | null;
  expiryDaa: bigint;
  interval: bigint;
  maxFill: bigint;
  slope: bigint;
  priceEnd: bigint;
  decayStep: bigint;
  deadline: bigint | null;
  /** delivery carriers budgeted: a fixed number, or null = resting default one per possible fill (at most MAX_DEFAULT_PAIR_FILLS) unless `fillsRequested` */
  fills: bigint | null;
  fillsRequested: bigint | undefined;
  /** the most aggressive price the order can trade at (limit, auction bound, decay end) */
  reach: bigint;
  expiryKind: ExpiryKind;
  expectedPrice: bigint | null;
  worstPrice: bigint | null;
  limitPrice: bigint;
  activatesAt: bigint | null;
  notes: string[];
  pairNotes: string[];
}

const LEAD = MARKET_ACTIVATION_DAA;

const blank = (side: 'sell' | 'buy', amount: bigint, price: bigint, tip: bigint, minFill: bigint | undefined): Draft => ({
  side, amount, minFill: minFill ?? null, immediate: false, price, tip, tif: 0, activeFrom: 0n, requestedActiveFrom: null, expiryDaa: 0n, interval: 0n,
  maxFill: 0n, slope: 0n, priceEnd: 0n, decayStep: 1n, deadline: null, fills: null, fillsRequested: undefined, reach: price, expiryKind: 'gtc',
  expectedPrice: null, worstPrice: null, limitPrice: price, activatesAt: null, notes: [], pairNotes: [],
});

const BAND_CODES: Record<string, PairIssueCode> = { PRICE_AGGRESSIVE_VS_MARKET: 'PAIR_PRICE_AGGRESSIVE_VS_MARKET', PRICE_FAR_FROM_MARKET: 'PAIR_PRICE_FAR_FROM_MARKET' };

/** The price-band warnings of the shared guard, against the pair book's reference (B per whole A), as pair codes. */
export function pairBand(env: PairPlanEnv, side: 'sell' | 'buy', price: bigint, field = 'price'): PlanIssue[] {
  return checkPriceBand(referencePrice(env.book), side, price, field).map((i) =>
    pairIssue(BAND_CODES[i.code] ?? 'PAIR_PRICE_FAR_FROM_MARKET', { ...(i.params ?? {}), ticker: env.pair.quote.ticker }, i.field),
  );
}

// ------------------------------------------------------------------------------------------------ the common tail

function place(env: PairPlanEnv, d: Draft, pre: PlanIssue[]): PairOrderPlan {
  const a = env.token;
  const b = env.pair.quote;
  const issues = [...pre];
  issues.push(...checkAmount(d.amount, a, 'amount', d.side === 'sell'));
  issues.push(...checkPairTip(env, d.amount, d.tip));
  issues.push(...checkTimes(env.clock, { activeFrom: d.activeFrom, expiryDaa: d.expiryDaa, requestedActiveFrom: d.requestedActiveFrom }));
  if (hasError(issues)) return failedPairPlan(issues);
  // the default minimum fill: the amount of A worth 10 KAS on A's KAS book (a resting order), 1 (an immediate one); never above a TWAP slice
  let minFill = d.minFill ?? (d.immediate ? 1n : env.kob.defaultMinFillPair(d.amount, env.pair.kasPerWholeA, a.scale));
  if (d.minFill === null && !d.immediate && env.pair.kasPerWholeA === null) issues.push(pairIssue('PAIR_KAS_REFERENCE_MISSING', { ticker: a.ticker }));
  if (d.minFill === null && d.maxFill > 0n && minFill > d.maxFill) minFill = d.maxFill;
  issues.push(...checkMinFill(minFill, d.amount));
  const top = d.price > d.priceEnd ? d.price : d.priceEnd;
  issues.push(...checkPairNotional(env, d.amount, top));
  if (hasError(issues)) return failedPairPlan(issues);
  if (d.tif === 2) issues.push(...checkFok({ side: d.side, amount: d.amount, limit: d.reach, tip: 0n, slots: a.slots, book: env.book }));
  issues.push(...checkSelfTrade(env.ownOrders, { side: d.side, price: d.reach, tip: 0n }));
  if (hasError(issues)) return failedPairPlan(issues);

  const possible = ceilDiv(d.amount, minFill);
  const fills = d.fills ?? d.fillsRequested ?? defaultPairFills(possible);
  // fewer carriers than possible fills: after `fills - 1` partial fills what is left fills only in full (a resting order)
  if (d.tif === 0 && d.interval === 0n && fills < possible) issues.push(pairIssue('PAIR_FILLS_LIMITED', { fills, partials: fills - 1n }));
  const dc = pairCarrierOf(env);
  const tips = pairTipsOf(env);
  const draft = makePairState(env, {
    side: d.side, amount: d.amount, minFill, price: d.price, tip: d.tip, tif: d.tif, activeFrom: d.activeFrom, expiryDaa: d.expiryDaa,
    refundTip: tips.refundTip, deliveryCarrier: dc, interval: d.interval, maxFill: d.maxFill, slope: d.slope, priceEnd: d.priceEnd, decayStep: d.decayStep,
    custody: d.amount,
  });
  let order = draft;
  if (d.side === 'buy') {
    // the B escrow: the whole amount at the highest price rounded down, plus one base unit (kob-wasm pairBidEscrow: every fill pays its exact
    // floor and floors are subadditive, so no slack per fill)
    const escrow = env.kob.pairBidEscrow(draft, d.amount, d.tif === 0 ? env.kob.pairMaxFills(draft) : 1n);
    if (escrow === null) return failedPairPlan([...issues, pairIssue('PAIR_NOTIONAL_TOO_LARGE', { ticker: b.ticker }, 'price')]);
    order = withCustody(draft, escrow);
  }
  // at least the deliveries the protocol requires a new order to fund itself (kob-wasm pairFundedFills: a resting order a partial fill and the
  // fill of its rest)
  const funded = maxBig(fills, env.kob.pairFundedFills(order));
  const kas = pairKasLines(env, order, funded, dc);
  if (kas === null) return failedPairPlan([...issues, issue('TIP_TOO_LARGE', undefined, 'tip')]);

  // B amounts, every one rounded by kob-wasm in the maker's favour: a sell receives at least ceil(n p / scale(A)), a buy pays floor(n p / scale(A))
  const sell = d.side === 'sell';
  const bOf = (n: bigint, p: bigint | null): bigint | null => (p === null ? null : sell ? env.kob.pairTOutMin(order, n, p) : env.kob.pairSOut(order, n, p));
  const worst = d.worstPrice ?? d.limitPrice;
  const atWorst = bOf(d.amount, worst);
  const n1 = minFill < d.amount ? minFill : d.amount;
  const priceMax = env.kob.pairPriceMax(order);
  return placePair(env, issues, {
    order,
    lines: kas.lines,
    deadline: d.deadline,
    side: d.side,
    amount: d.amount,
    expiry: { kind: d.expiryKind, daa: d.expiryDaa, approxUnixSeconds: daaToUnix(env.clock, d.expiryDaa), deadlineUnixSeconds: d.deadline },
    activatesAt: d.activatesAt,
    limitPrice: d.limitPrice,
    expectedPrice: d.expectedPrice,
    worstPrice: d.worstPrice,
    tip: d.tip,
    minTouch: null,
    keeperTip: 0n,
    allInTotal: bOf(d.amount, d.limitPrice),
    receiveMinB: sell ? atWorst : null,
    payMaxB: sell ? null : atWorst,
    expectedB: bOf(d.amount, d.expectedPrice),
    minFillB: bOf(n1, d.limitPrice),
    tipKasTotal: kas.tipKasTotal,
    deliveries: funded,
    deliveryCarrier: dc,
    exitCarrier: null,
    delivery: sell ? { market: b, amount: env.kob.pairTOutMin(order, n1, priceMax) ?? 0n } : { market: a, amount: n1 },
    exitFacts: null,
    repeat: null,
    notes: d.notes,
    pairNotes: d.pairNotes,
    cond: null,
  });
}

// ------------------------------------------------------------------------------------------------ limit

function planLimit(env: PairPlanEnv, i: LimitIntent): PairOrderPlan {
  const pre: PlanIssue[] = [...checkPairPrice(i.price)];
  if (i.maxFills !== undefined) checkFills(i.maxFills, pre);
  if (hasError(pre)) return failedPairPlan(pre);
  const ticker = env.pair.quote.ticker;
  const d = blank(i.side, i.amount, i.price, i.tip ?? 0n, i.minFill);
  const requested = activationDaa(env, i.activeFrom);
  const future = requested !== null && requested > env.clock.daa;
  d.requestedActiveFrom = requested;
  d.activeFrom = future ? requested : 0n;
  d.activatesAt = future ? requested : null;
  const exp = resolveLifetime(env, i.lifetime, pre);
  d.expiryDaa = exp.expiryDaa;
  d.deadline = exp.deadline;
  d.expiryKind = exp.kind;
  d.fillsRequested = i.maxFills;
  d.worstPrice = i.price;
  d.notes.push(exp.kind === 'gtc' ? 'gtc' : exp.kind === 'day' ? 'dayOrder' : 'gtd');
  pre.push(...pairBand(env, i.side, i.price));
  // the crossing policy against the pair book (B prices, no tips: a KAS tip never makes a B price cross)
  const x = future ? { crossing: false, touch: null } : crossingTouch(env.book, i.side, i.price, 0n);
  if (x.crossing && x.touch !== null) {
    const policy = i.crossing ?? 'auction';
    const start = i.side === 'sell' ? (x.touch > i.price ? x.touch : i.price) : x.touch < i.price ? x.touch : i.price;
    if (policy === 'reject') {
      pre.push(pairIssue('PAIR_MARKETABLE_REJECTED', { touch: x.touch, ticker }, 'price'));
    } else if (policy === 'auction' && start !== i.price) {
      // an auction from the touch to the limit (an ASK decays down, a BID rises up), resting at the limit afterwards
      d.price = start;
      d.priceEnd = i.price;
      d.slope = decaySlope(start, i.price, MARKET_AUCTION_DAA, 1n);
      d.decayStep = 1n;
      d.activeFrom = env.clock.daa + LEAD;
      d.activatesAt = null;
      d.reach = i.price;
      d.expectedPrice = x.touch;
      d.notes.push('auction', 'marketable');
      d.pairNotes.push('pairAuction');
      pre.push(pairIssue('PAIR_MARKETABLE_AUCTION', { touch: x.touch, ticker }, 'price'));
    } else if (start !== i.price) {
      pre.push(pairIssue('PAIR_MARKETABLE_LIMIT_FILLS_AT_LIMIT', { touch: x.touch, ticker }, 'price'));
    }
  }
  return place(env, d, pre);
}

// ------------------------------------------------------------------------------------------------ IOC / FOK

function planImmediate(env: PairPlanEnv, i: IocIntent | FokIntent): PairOrderPlan {
  const pre: PlanIssue[] = [...checkPairPrice(i.price)];
  const life = lifeDaa(env, i.life, pre);
  if (hasError(pre)) return failedPairPlan(pre);
  const d = blank(i.side, i.amount, i.price, i.tip ?? 0n, i.minFill);
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
  d.worstPrice = i.price;
  d.notes.push(i.type === 'fok' ? 'fokAllOrNothing' : 'iocRemainderReturned');
  pre.push(...pairBand(env, i.side, i.price));
  return place(env, d, pre);
}

// ------------------------------------------------------------------------------------------------ market / streaming / close

interface AuctionRequest {
  side: 'sell' | 'buy';
  amount: bigint;
  tip: bigint;
  minFill?: bigint;
  reference: bigint | null;
  bps: bigint;
  auction?: Duration;
  activation?: bigint;
  life?: Duration;
  allOrNothing: boolean;
  notes: string[];
}

/** An IOC (or FOK) auction from `reference` (the pair book's touch, or the displayed price) to the slippage bound: ASK decaying, BID rising. */
function planAuction(env: PairPlanEnv, r: AuctionRequest, pre: PlanIssue[]): PairOrderPlan {
  checkBps(r.bps, 'slippageBps', pre);
  const auctionDaa = positiveDuration(env, r.auction, MARKET_AUCTION_DAA, 'auction', pre);
  const life = lifeDaa(env, r.life, pre);
  const delay = r.activation ?? MARKET_ACTIVATION_DAA;
  if (delay < 0n) pre.push(issue('DURATION_INVALID', undefined, 'activation'));
  if (r.reference === null) pre.push(issue('NO_LIQUIDITY', { counterparty: r.side === 'sell' ? 'bids' : 'asks' }));
  if (hasError(pre)) return failedPairPlan(pre);
  const touch = r.reference as bigint;
  // no tick on a B price: the bound is the touch moved by floor(touch x bps / 10^4)
  const end = auctionBound(r.side, touch, r.bps, 1n);
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
  d.pairNotes.push('pairAuction');
  pre.push(pairIssue('PAIR_MARKET_REFERENCE_SOURCE', { reference: touch, worst: end, ticker: env.pair.quote.ticker }));
  if (!r.allOrNothing) {
    const depth = depthWithin(env.book, r.side, end, 0n);
    if (depth < r.amount) pre.push(issue('MARKET_DEPTH_INSUFFICIENT', { available: depth, amount: r.amount }));
  }
  return place(env, d, pre);
}

function planMarket(env: PairPlanEnv, i: MarketIntent): PairOrderPlan {
  return planAuction(env, {
    side: i.side, amount: i.amount, tip: i.tip ?? 0n, minFill: i.minFill, reference: touchPrice(env.book, i.side), bps: i.slippageBps ?? SLIPPAGE_BPS,
    auction: i.auction, activation: i.activation, life: i.life, allOrNothing: i.allOrNothing ?? false, notes: ['market'],
  }, []);
}

function planStreaming(env: PairPlanEnv, i: StreamingIntent): PairOrderPlan {
  const pre: PlanIssue[] = [];
  if (i.displayedPrice <= 0n) pre.push(issue('REFERENCE_PRICE_INVALID', undefined, 'displayedPrice'));
  return planAuction(env, {
    side: i.side, amount: i.amount, tip: i.tip ?? 0n, minFill: i.minFill, reference: i.displayedPrice > 0n ? i.displayedPrice : null, bps: i.toleranceBps,
    auction: i.auction, activation: i.activation, life: i.life, allOrNothing: i.allOrNothing ?? false, notes: ['streaming'],
  }, pre);
}

function planClose(env: PairPlanEnv, i: CloseIntent): PairOrderPlan {
  const held = heldOf(env.token, env.maker, env.tokenUtxos);
  if (i.amount === undefined && held <= 0n) return failedPairPlan([issue('CLOSE_NOTHING_TO_SELL')]);
  return planAuction(env, {
    side: 'sell', amount: i.amount ?? held, tip: i.tip ?? 0n, reference: touchPrice(env.book, 'sell'), bps: i.slippageBps ?? SLIPPAGE_BPS,
    auction: i.auction, activation: i.activation, life: i.life, allOrNothing: i.allOrNothing ?? false, notes: ['market', 'close'],
  }, []);
}

// ------------------------------------------------------------------------------------------------ TWAP / DCA

function planSchedule(env: PairPlanEnv, i: TwapIntent | DcaIntent): PairOrderPlan {
  const side = i.type === 'twap' ? 'sell' : 'buy';
  if (i.side !== undefined && i.side !== side) return failedPairPlan([issue('SIDE_INVALID', { side: i.side }, 'side')]);
  const pre: PlanIssue[] = [...checkPairPrice(i.price)];
  const intervalDaa = positiveDuration(env, i.interval, 0n, 'interval', pre);
  if (i.sliceAmount < 1n) pre.push(issue('SLICE_AMOUNT_INVALID', undefined, 'sliceAmount'));
  if (i.priceEnd !== undefined) {
    pre.push(...checkPairPrice(i.priceEnd, 'priceEnd'));
    if (side === 'sell' ? i.priceEnd >= i.price : i.priceEnd <= i.price) pre.push(issue('PRICE_END_INVALID', { direction: side === 'sell' ? 'below' : 'above' }, 'priceEnd'));
  }
  const sliceAuction = positiveDuration(env, i.sliceAuction, MARKET_AUCTION_DAA, 'sliceAuction', pre);
  if (i.type === 'dca' && i.maxFills !== undefined) checkFills(i.maxFills, pre);
  if (hasError(pre)) return failedPairPlan(pre);

  const d = blank(side, i.amount, i.price, i.tip ?? 0n, i.minFill);
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
  const start = future ? (requested as bigint) : env.clock.daa;
  if (start + slices * intervalDaa > d.expiryDaa) pre.push(issue('TWAP_EXCEEDS_LIFE', { slices, intervalDaa }));
  if (i.priceEnd !== undefined) {
    d.priceEnd = i.priceEnd;
    d.slope = decaySlope(i.price, i.priceEnd, sliceAuction, 1n);
    d.reach = i.priceEnd;
    d.worstPrice = i.priceEnd;
    d.expectedPrice = i.price;
    d.limitPrice = i.priceEnd;
    d.notes.push('auction');
    d.pairNotes.push('pairAuction');
  } else {
    d.worstPrice = i.price;
  }
  d.notes.push(i.type, exp.kind === 'day' ? 'dayOrder' : 'gtc');
  // every slice is a fill with its own delivery: one carrier per slice (a DCA may budget more)
  const wanted = i.type === 'dca' ? i.maxFills : undefined;
  if (wanted !== undefined && wanted < slices) pre.push(issue('MAX_FILLS_INVALID', undefined, 'maxFills'));
  d.fills = wanted !== undefined && wanted >= slices ? wanted : slices;
  pre.push(...pairBand(env, side, i.price));
  return place(env, d, pre);
}

// ------------------------------------------------------------------------------------------------ Dutch / rising

function planDutch(env: PairPlanEnv, i: DutchIntent): PairOrderPlan {
  const pre: PlanIssue[] = [...checkPairPrice(i.price), ...checkPairPrice(i.priceEnd, 'priceEnd')];
  if (i.side === 'sell' ? i.priceEnd >= i.price : i.priceEnd <= i.price) pre.push(issue('PRICE_END_INVALID', { direction: i.side === 'sell' ? 'below' : 'above' }, 'priceEnd'));
  const duration = positiveDuration(env, i.duration, 0n, 'duration', pre);
  const step = i.stepDaa ?? 1n;
  if (step < 1n) pre.push(issue('DURATION_INVALID', undefined, 'stepDaa'));
  if (i.maxFills !== undefined) checkFills(i.maxFills, pre);
  if (hasError(pre)) return failedPairPlan(pre);

  const d = blank(i.side, i.amount, i.price, i.tip ?? 0n, i.minFill);
  const requested = activationDaa(env, i.activeFrom);
  const future = requested !== null && requested > env.clock.daa;
  d.requestedActiveFrom = requested;
  // the decay origin IS activeFrom: a real DAA score (0 would put the price at its end at once)
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
  d.fillsRequested = i.maxFills;
  d.notes.push('auction', i.side === 'sell' ? 'dutch' : 'rising');
  d.pairNotes.push('pairAuction');
  pre.push(...pairBand(env, i.side, i.priceEnd, 'priceEnd'));
  return place(env, d, pre);
}

// ------------------------------------------------------------------------------------------------ entry point

/** Plans any plain intent on a pair (KobPair). Never throws for user-input problems. */
export function planPairSimple(env: PairPlanEnv, intent: SimpleIntent): PairOrderPlan {
  if (!SIMPLE_TYPES.includes(intent.type)) return failedPairPlan([issue('INTENT_UNKNOWN_TYPE', { type: String((intent as { type?: unknown }).type) })]);
  if (intent.type !== 'close' || intent.amount !== undefined) {
    const amt = checkAmount((intent as { amount: bigint }).amount, env.token, 'amount', intent.side !== 'buy');
    if (hasError(amt)) return failedPairPlan(amt);
  }
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
      return failedPairPlan([issue('INTENT_UNKNOWN_TYPE', { type: String((intent as { type?: unknown }).type) })]);
  }
}
