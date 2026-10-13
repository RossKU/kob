// Replace by cancel-replace of the order kinds the in-place / quick amend does not cover: the order is turned back into the ticket intent it
// was planned from, the user edits it in the ticket's own fields (every knob: legs, band, trigger rule, trailing, expiry, activation), the planner
// re-plans it, and the planned order becomes the replacement of ONE atomic cancel + new order transaction (planCancelReplace).
//
// Pure: no network, no DOM. Prices in an intent are the state prices (sompi per whole token of the order's scale), amounts base units.
//   * conditional orders (KobCondAsk / KobCondBid) that are not booked exits: stop-market, stop-limit (a band other than the default 3% is shown as
//     its worst price, the stop-limit's limit, so moving the stop no longer moves the effective limit silently), trailing, take-profit, OCO;
//   * if-done entries (KobIfdBid / KobIfdAsk) that do not repeat: IFD, IFO / bracket, stop entries. Their exits already booked stay as they are (own
//     orders of the position); the replacement commits the edited exit for the amount it still has to fill.
//   * pair orders (no in-place amend in the protocol): resting pair limits (KobPair GTC / GTD / day), pair conditionals (KobCondPair) and pair
//     if-done entries (KobIfdPair) the same way as their KAS twins; prices B base units per whole A, the tip KAS.
//   * scheduled plain orders (KobAsk / KobBid / KobPair): TWAP and DCA (slice, interval, per-slice auction), Dutch / rising (the decay goes on from
//     where it is now: its current price, the same end and pace; a decay that has ended rests at its end, replaced as a limit there) and timed
//     limits (the activation kept, to the next whole minute: never earlier). A buy's amount is what its escrow buys now (kob-wasm bidBuyingPower).
// Not replaced here: repeat entries (their booked exits re-arm THIS entry: a new entry would strand their take-profit until rptUntil, so the position
// cancel is the way), booked exits (bound to their entry), plain KAS limits (amended in place or by the quick amend form), IOC / FOK / market
// auctions (they end within their window).
import { SLIPPAGE_BPS, daaToUnix } from '../../kob/daa';
import type { CondExpiry, ExitSpec, TrailSpec } from '../../kob/intent-cond';
import { baseKind, describeOrder, type OrderDescription, type TriggerTerms } from '../../kob/order-facts';
import type { Intent } from '../../kob/plan';
import type { Clock } from '../../kob/plan-types';
import type { OrderState } from '../../kob/types';
import { ceilDiv, roundToTick, safeRounding } from '../../kob/units';
import type { KobWasm } from '../../kob/wasm';
import { SCHEDULED_TYPES, type OrderTypeKey } from './orders-model';

export interface ReplaceContext {
  /** the row's type key (orders-model orderTypeKey) */
  typeKey: OrderTypeKey;
  /** the row's expiry class: a GTC is replaced as GTC (a fresh 90 days: the replacement renews it), a day order as a day order */
  expiryKind: 'gtc' | 'gtd' | 'day' | 'none';
  /** the current DAA: an activation still in the future is kept */
  nowDaa: bigint | null;
  /** a dated order (KAS or pair): its approximate end (UTC unix seconds), kept as the replacement's date (the ticket form holds dates, not DAA scores) */
  expiryUnix?: bigint | null;
  /** DAA score -> approximate UTC unix seconds (the wallet clock): a dated exit keeps its own date through the form */
  daaToUnix?: (daa: bigint) => bigint | null;
  /** the chain clock: a future activation is carried as a date of the ticket (to the next whole minute), a decay's current point is read at `nowDaa` */
  clock?: Clock | null;
  /** a plain bid (KobBid): the KAS its order UTXO holds now (its escrow), for the amount it buys (kob-wasm bidBuyingPower) */
  value?: bigint | null;
  /** the token's price tick (a decay's current price is rounded onto it, never worse for the maker) */
  tick?: bigint;
}

export type ReplaceResult = { ok: true; intent: Intent } | { ok: false; reason: 'repeat' | 'booked' | 'kind' | 'state' };

/** The order types the replace form handles (`orders-model` amendKind 'replace'; the limits only for pair orders). */
export const REPLACEABLE_TYPES: readonly OrderTypeKey[] = ['stop', 'trailing', 'take-profit', 'oco', 'ifd', 'ifo', 'limit', 'limit-gtd', 'limit-day', 'twap', 'dca', 'dutch', 'timed'];

const PLAIN_LIMITS: readonly OrderTypeKey[] = ['limit', 'limit-gtd', 'limit-day'];

/** A dated expiry as the ticket form can show it: its date when the clock gives one (the form drops a bare DAA score), else the DAA score. */
function datedExpiry(daa: bigint, unix: bigint | null | undefined): { kind: 'gtdUnix'; atUnixSeconds: bigint } | { kind: 'gtdDaa'; daa: bigint } {
  return unix !== null && unix !== undefined && unix > 0n ? { kind: 'gtdUnix', atUnixSeconds: unix } : { kind: 'gtdDaa', daa };
}

function expiryOf(ctx: ReplaceContext, expiryDaa: bigint): CondExpiry {
  if (ctx.expiryKind === 'day') return { kind: 'day' };
  if (ctx.expiryKind === 'gtd' && expiryDaa > 0n) return datedExpiry(expiryDaa, ctx.expiryUnix ?? ctx.daaToUnix?.(expiryDaa));
  return { kind: 'gtc' };
}

/** The exit's own lifetime: a dated exit keeps its date, anything else is GTC (an exit has no day order). */
function exitExpiryOf(x: OrderDescription, ctx: ReplaceContext): Exclude<CondExpiry, { kind: 'day' }> {
  // an exit committed with a date below the no-date sentinel and not the GTC bound of its creation: keep it as a date only when the entry is dated
  // too (the exit's GTC bound is counted from its own creation, which the committed state cannot tell apart from a date)
  return ctx.expiryKind === 'gtd' && x.expiryDaa > 0n && x.expiryDaa < 1n << 60n ? datedExpiry(x.expiryDaa, ctx.daaToUnix?.(x.expiryDaa)) : { kind: 'gtc' };
}

/** Stop-leg fields of a trigger (shared by conditional orders and if-done exits). */
function stopFields(t: TriggerTerms, side: 'sell' | 'buy'): { stop: bigint; slip: { slipBps: number } | { limit: bigint }; bandDaa: bigint; keeperTip: bigint; minTouch: bigint; minRestDaa: bigint; trail?: TrailSpec } {
  const stop = t.stopPrice;
  // the default band is a stop-market; any other band is a stop-limit whose limit is the worst price the band allows (multiply first, as the covenant)
  const move = (t.stopPrice * t.slipBps) / 10_000n;
  const worst = side === 'sell' ? t.stopPrice - move : t.stopPrice + move;
  const slip = t.slipBps === SLIPPAGE_BPS || worst <= 0n ? { slipBps: Number(t.slipBps) } : { limit: worst };
  return {
    stop, slip, bandDaa: t.bandDaa, keeperTip: t.keeperTip, minTouch: t.minTouch > 0n ? t.minTouch : 1n, minRestDaa: t.minRestDaa,
    ...(t.trailStep > 0n ? { trail: { step: t.trailStep, gap: t.trailGap, wait: t.trailWait } } : {}),
  };
}

/**
 * The ticket intent of a live order, for the replace form. `state` is the order's CURRENT proven state (an armed stop is replaced unarmed, a trailed
 * stop from where it trails now).
 */
export function intentFromOrder(state: OrderState, ctx: ReplaceContext, kob?: KobWasm): ReplaceResult {
  const d = describeOrder(state, kob);
  const base = baseKind(state.kind);
  // a plain bid holds KAS, not an amount: what its escrow buys now at its own terms
  let amount = d.amountLeft;
  if (amount === null && base === 'KobBid' && kob && ctx.value != null && ctx.value > 0n) {
    try {
      amount = kob.bidBuyingPower(state, ctx.value);
    } catch {
      amount = null;
    }
  }
  if (amount === null || amount < 1n) return { ok: false, reason: 'state' };
  const expiry = expiryOf(ctx, d.expiryDaa);
  // a future activation: a date of the ticket when the clock is known (rounded UP to the minute the date field shows: never earlier), else its DAA
  const future = ctx.nowDaa !== null && d.activeFrom > ctx.nowDaa;
  const activeFrom = !future ? {} : ctx.clock ? { activeFrom: { unixSeconds: ceilDiv(daaToUnix(ctx.clock, d.activeFrom), 60n) * 60n } } : { activeFrom: { daa: d.activeFrom } };
  // the order's own minimum fill is kept (at most the amount left)
  const minFill = d.minFill > 0n && d.minFill <= amount ? { minFill: d.minFill } : {};
  const common = { side: d.side, amount, tip: d.tip, expiry, ...minFill, ...activeFrom };

  if (base === 'KobPair' || base === 'KobAsk' || base === 'KobBid') {
    // plain orders: the resting pair limits and the scheduled kinds (a plain KAS limit is amended in place; IOC / FOK / market end within their window)
    const ok = SCHEDULED_TYPES.includes(ctx.typeKey) || (base === 'KobPair' && PLAIN_LIMITS.includes(ctx.typeKey));
    if (!ok) return { ok: false, reason: 'kind' };
    const lifetime = ctx.expiryKind === 'day' ? { kind: 'day' as const } : ctx.expiryKind === 'gtd' && ctx.expiryUnix ? { kind: 'gtd' as const, at: ctx.expiryUnix } : { kind: 'gtc' as const };
    const { expiry: _e, ...rest } = common;
    void _e;
    const plain = { ...rest, lifetime };
    const a = d.auction;
    if (ctx.typeKey === 'twap' || ctx.typeKey === 'dca') {
      if (!a || a.interval <= 0n || a.maxFill <= 0n) return { ok: false, reason: 'state' };
      // the per-slice auction: from the price to its end at `slope` per DAA (step 1)
      const diff = d.price > a.priceEnd ? d.price - a.priceEnd : a.priceEnd - d.price;
      const slice = a.slope > 0n && a.priceEnd > 0n && diff > 0n ? { priceEnd: a.priceEnd, sliceAuction: { daa: ceilDiv(diff, a.slope) } } : {};
      // the minimum fill never above a slice
      const capped = d.minFill > a.maxFill && d.minFill <= amount ? { minFill: a.maxFill } : {};
      return {
        ok: true,
        intent: { type: ctx.typeKey, ...plain, ...capped, sliceAmount: a.maxFill < amount ? a.maxFill : amount, interval: { daa: a.interval }, price: d.price, ...slice } as Intent,
      };
    }
    if (ctx.typeKey === 'dutch') {
      if (!a || a.slope <= 0n || a.priceEnd <= 0n) return { ok: false, reason: 'state' };
      const sell = d.side === 'sell';
      const step = a.decayStep > 0n ? a.decayStep : 1n;
      const ended = (p: bigint): boolean => (sell ? p <= a.priceEnd : p >= a.priceEnd);
      // where the decay is now: the price after the steps taken since its origin (activeFrom), never past its end
      const steps = ctx.nowDaa !== null && ctx.nowDaa > d.activeFrom ? (ctx.nowDaa - d.activeFrom) / step : 0n;
      const moved = steps * a.slope;
      let start = sell ? d.price - moved : d.price + moved;
      if (!ended(start) && ctx.tick !== undefined && ctx.tick > 1n) start = roundToTick(start, ctx.tick, safeRounding(d.side));
      // the decay has ended: the order rests at its end, as a limit does
      if (ended(start)) return { ok: true, intent: { type: 'limit', ...plain, price: a.priceEnd } as Intent };
      // the same pace: the steps left at `slope` per step
      const left = ceilDiv(sell ? start - a.priceEnd : a.priceEnd - start, a.slope);
      return { ok: true, intent: { type: 'dutch', ...plain, price: start, priceEnd: a.priceEnd, duration: { daa: left * step }, stepDaa: step } as Intent };
    }
    // a resting or timed limit
    return { ok: true, intent: { type: 'limit', ...plain, price: d.price } as Intent };
  }

  if (base === 'KobCondAsk' || base === 'KobCondBid' || base === 'KobCondPair') {
    if (d.booked) return { ok: false, reason: 'booked' };
    const t = d.trigger;
    if (!t) return { ok: false, reason: 'state' };
    const tp = d.price > 0n ? d.price : null;
    if (t.stopPrice <= 0n) return tp === null ? { ok: false, reason: 'state' } : { ok: true, intent: { type: 'takeProfit', ...common, price: tp } as Intent };
    const s = stopFields(t, d.side);
    const trigger = { bandDaa: s.bandDaa, keeperTip: s.keeperTip, minTouch: s.minTouch, minRestDaa: s.minRestDaa };
    if (s.trail) {
      // a trailing stop keeps its band as slipBps (the trailing ticket has no stop-limit field)
      const slipBps = Number(t.slipBps);
      return { ok: true, intent: { type: 'trailingStop', ...common, stop: s.stop, slipBps, ...trigger, trail: s.trail, ...(tp !== null ? { takeProfit: tp } : {}) } as Intent };
    }
    if (tp !== null) return { ok: true, intent: { type: 'oco', ...common, takeProfit: tp, stop: s.stop, ...s.slip, ...trigger } as Intent };
    if ('limit' in s.slip) return { ok: true, intent: { type: 'stopLimit', ...common, stop: s.stop, limit: s.slip.limit, ...trigger } as Intent };
    return { ok: true, intent: { type: 'stopMarket', ...common, stop: s.stop, slipBps: s.slip.slipBps, ...trigger } as Intent };
  }

  if (base === 'KobIfdBid' || base === 'KobIfdAsk' || base === 'KobIfdPair') {
    const e = d.entry;
    if (!e) return { ok: false, reason: 'state' };
    if (e.rptAmount > 0n) return { ok: false, reason: 'repeat' };
    const x = e.exit;
    if (!x || !x.trigger) return { ok: false, reason: 'state' };
    const exitSide = x.side;
    const exit: ExitSpec = { tip: x.tip, expiry: exitExpiryOf(x, ctx), ...(x.minFill > 0n ? { minFill: x.minFill } : {}) };
    if (x.price > 0n) exit.takeProfit = x.price;
    if (x.trigger.stopPrice > 0n) {
      const s = stopFields(x.trigger, exitSide);
      exit.stop = s.stop;
      if ('limit' in s.slip) exit.stopLimit = s.slip.limit;
      else exit.slipBps = s.slip.slipBps;
      exit.bandDaa = s.bandDaa;
      exit.keeperTip = s.keeperTip;
      exit.minTouch = s.minTouch;
      exit.minRestDaa = s.minRestDaa;
      if (s.trail) exit.trail = s.trail;
    }
    const both = exit.takeProfit !== undefined && exit.stop !== undefined;
    const entry = {
      price: d.price,
      ...(e.entryStop > 0n ? { stop: e.entryStop, bandDaa: e.bandDaa, keeperTip: e.keeperTip, minTouch: e.minTouch > 0n ? e.minTouch : 1n, minRestDaa: e.minRestDaa } : {}),
    };
    // a sell-first entry's buy-back prefund (KAS entries: sompi per whole token; a pair entry: B base units per whole A)
    const sellFirst = base === 'KobIfdAsk' || (base === 'KobIfdPair' && d.side === 'sell');
    const prefund = sellFirst && e.prefund > 0n ? { prefund: e.prefund } : {};
    return { ok: true, intent: { type: both ? 'ifo' : 'ifd', ...common, entry, exit, ...prefund } as Intent };
  }
  return { ok: false, reason: 'kind' };
}
