// What a position card shows above its member rows. That is the phase chip, prices along the entry
// to exit path, amounts, repeat state and, loaded on demand from the order events, the realised
// fills. It is pure and built from `groupPositions` (kob/positions.ts) and the row models
// (orders-model.ts). Amounts are token base units. Prices are state prices, in sompi per whole
// token of the orders' `scale` base units.
import type { EventView } from '../../data/indexer-types';
import { daaToUnix } from '../../kob/daa';
import type { Clock } from '../../kob/plan-types';
import { isLiveStatus, type Position } from '../../kob/positions';
import type { Hex } from '../../kob/types';
import type { OrderRowModel } from './orders-model';

export const POSITION_PHASES = ['entry-waiting', 'entry-partial', 'exit-resting', 'repeat-waiting', 'closed', 'cancelled'] as const;
export type PositionPhase = (typeof POSITION_PHASES)[number];

const NOT_EXECUTED = new Set(['cancelled', 'refunded', 'killed']);

/**
 * The state chip of a position, in the user's words:
 * - entry-waiting: the entry rests and nothing has filled.
 * - entry-partial: the entry rests and part of it has filled, so its exits are working.
 * - exit-resting: the entry is done and only exits are left.
 * - repeat-waiting: a repeating entry is sold out and waits for its exits to re-arm it.
 * - closed: everything ended after trading.
 * - cancelled: everything ended and nothing ever traded.
 */
export function positionPhase(p: Position): PositionPhase {
  switch (p.status) {
    case 'open':
      return 'entry-waiting';
    case 'partial':
      return 'entry-partial';
    case 'exits-only':
      return 'exit-resting';
    case 'waiting':
      return 'repeat-waiting';
    case 'closed': {
      const members = p.entry ? [p.entry, ...p.exits] : p.exits;
      const traded = members.some((m) => (amountOf(m.filled_amount) ?? 0n) > 0n || m.status === 'filled');
      return !traded && members.every((m) => NOT_EXECUTED.has(m.status) || m.status === 'closed') ? 'cancelled' : 'closed';
    }
  }
}

export interface ExitLeg {
  id: Hex;
  status: string;
  /** Take-profit (limit) price, in sompi per whole token. */
  takeProfit: bigint | null;
  /** Stop trigger, in sompi per whole token, for OCO exits. */
  stop: bigint | null;
  /** Base units left and at creation. */
  amountLeft: bigint | null;
  amountTotal: bigint | null;
}

export interface PositionSummary {
  phase: PositionPhase;
  token: Hex | null;
  side: 'sell' | 'buy';
  /** The entry's limit in sompi per whole token. Null when unknown or a budget entry. */
  entryPrice: bigint | null;
  entryAllIn: bigint | null;
  /** The entry's trigger for stop entries. */
  entryStop: bigint | null;
  exits: ExitLeg[];
  /** Distinct take-profit prices of the exits that are still live, ascending. */
  exitTakeProfits: bigint[];
  /**
   * The exit the live entry commits, decoded from its `exitState` and booked for what it fills. It
   * is shown while no exit of that leg is live yet, because an entry that is waiting would
   * otherwise read a dash for its take-profit and stop. `amount` is what the entry has still to
   * fill, in base units.
   */
  plannedExit: { takeProfit: bigint | null; stop: bigint | null; amount: bigint | null } | null;
  /** Base units of the entry, for one cycle. */
  amountTotal: bigint | null;
  /** Base units the entry has filled. */
  amountEntered: bigint;
  /** Base units the exits have filled, which completed the round trip. */
  amountExited: bigint;
  /**
   * Open base units, which are the entry remainder plus live exits. Null when the entry is a KAS
   * budget.
   */
  amountOpen: bigint | null;
  /**
   * Repeat: booked exits that sold out, base units the entry can still re-arm, and the latest
   * `rpt_until` DAA of the exits. Null when there is none.
   */
  repeat: { cyclesDone: number; rearmAmount: bigint | null; untilDaa: bigint | null; untilUnix: bigint | null } | null;
}

/**
 * A decimal string of the API, in base units, as a bigint. Zero is allowed. It is null when absent
 * or malformed.
 */
const amountOf = (v: string | number | null | undefined): bigint | null => {
  if (v === null || v === undefined || v === '') return null;
  try {
    const b = BigInt(v);
    return b >= 0n ? b : null;
  } catch {
    return null;
  }
};

const big = (v: string | number | null | undefined): bigint | null => {
  if (v === null || v === undefined || v === '') return null;
  try {
    const b = BigInt(v);
    return b > 0n ? b : null;
  } catch {
    return null;
  }
};

export function summarizePosition(p: Position, rows: ReadonlyMap<Hex, OrderRowModel>, clock: Clock | null = null): PositionSummary {
  const entryRow = p.entry ? rows.get(p.entry.covenant_id) : undefined;
  const exits: ExitLeg[] = p.exits.map((x) => {
    const r = rows.get(x.covenant_id);
    return { id: x.covenant_id, status: x.status, takeProfit: r?.price ?? null, stop: r?.stopPrice ?? null, amountLeft: amountOf(x.amount_left), amountTotal: amountOf(x.initial_amount) };
  });
  const live = exits.filter((e) => isLiveStatus(e.status) && e.takeProfit !== null).map((e) => e.takeProfit as bigint);
  const tps = [...new Set(live)].sort((a, b) => (a < b ? -1 : a > b ? 1 : 0));
  let repeat: PositionSummary['repeat'] = null;
  if (p.cycles) {
    const untils = p.exits.map((x) => big(x.repeat?.rpt_until)).filter((x): x is bigint => x !== null);
    const untilDaa = untils.length ? untils.reduce((a, b) => (b > a ? b : a)) : null;
    repeat = { cyclesDone: p.cycles.done, rearmAmount: p.cycles.rearmAmount, untilDaa, untilUnix: untilDaa !== null && clock ? daaToUnix(clock, untilDaa) : null };
  }
  return {
    phase: positionPhase(p),
    token: entryRow?.token ?? p.entry?.token ?? p.exits[0]?.token ?? null,
    side: p.side,
    entryPrice: entryRow?.price ?? null,
    entryAllIn: entryRow?.allInPrice ?? null,
    entryStop: entryRow?.stopPrice ?? null,
    exits,
    exitTakeProfits: tps,
    plannedExit: entryRow?.live && entryRow.plannedExit ? entryRow.plannedExit : null,
    amountTotal: p.amountTotal,
    amountEntered: p.amountDone,
    amountExited: p.exits.reduce((s, x) => s + (amountOf(x.filled_amount) ?? 0n), 0n),
    amountOpen: p.amountLeft,
    repeat,
  };
}

/**
 * Base units the position still has to unwind once its orders are cancelled (close). That is what
 * its entry filled minus what its exits filled. It is never negative. Buy first: tokens bought and
 * not sold yet. They return to the wallet with the cancel, and the close sells them. Sell first:
 * tokens sold and not bought back yet. The cancel returns the KAS the exits held, and the close
 * buys them back at market.
 */
export function unwindAmount(p: Position): bigint {
  const exited = p.exits.reduce((s, x) => s + (amountOf(x.filled_amount) ?? 0n), 0n);
  return p.amountDone > exited ? p.amountDone - exited : 0n;
}

// Realised fills.

export interface FillSummary {
  /** Number of fill events. */
  fills: number;
  /** Base units filled. */
  amount: bigint;
  /**
   * Amount-weighted average fill price, in sompi per whole token, rounded down. Null when no fill
   * was priced.
   */
  avgPrice: bigint | null;
  /** KAS paid out to the maker by sell-side fills, in sompi. */
  payout: bigint;
}

export interface PositionFills {
  entry: FillSummary;
  exits: FillSummary;
  /**
   * Realised spread per whole token for a completed leg pair. It is the exits' average minus the
   * entry's (buy first) or the reverse (sell first). Null when unknown.
   */
  spreadPerToken: bigint | null;
  /** The orders' scale, in base units per whole token, that the prices are quoted per. */
  scale: bigint;
}

function summarize(events: readonly EventView[]): FillSummary {
  let amount = 0n;
  let fills = 0;
  let value = 0n;
  let priced = 0n;
  let payout = 0n;
  for (const e of events) {
    const n = amountOf(e.amount);
    if (e.kind !== 'fill' || n === null || n <= 0n) continue;
    fills++;
    amount += n;
    const price = big(e.price);
    if (price !== null) {
      value += price * n;
      priced += n;
    }
    payout += big(e.payout) ?? 0n;
  }
  return { fills, amount, avgPrice: priced > 0n ? value / priced : null, payout };
}

/**
 * Sums the fill events of the entry and of the exits. Each list is one order's
 * `/v1/orders/{id}/events`. Duplicate events with the same id count once. `scale` is the orders'
 * scale. An entry and its exits always share it.
 */
export function summarizePositionFills(entryEvents: readonly EventView[], exitEvents: readonly EventView[], side: 'sell' | 'buy', scale: number | bigint = 1): PositionFills {
  const sc = BigInt(scale) > 0n ? BigInt(scale) : 1n;
  const uniq = (l: readonly EventView[]) => [...new Map(l.map((e) => [e.id, e])).values()];
  const entry = summarize(uniq(entryEvents));
  const exits = summarize(uniq(exitEvents));
  let spread: bigint | null = null;
  if (entry.avgPrice !== null && exits.avgPrice !== null) spread = side === 'buy' ? exits.avgPrice - entry.avgPrice : entry.avgPrice - exits.avgPrice;
  return { entry, exits, spreadPerToken: spread, scale: sc };
}

export interface RealisedPnl {
  /** Base units that completed the round trip, filled by the entry and by the exits. */
  amountClosed: bigint;
  /**
   * Realised profit or loss in sompi. It is the spread per whole token times the amount closed over
   * the scale, rounded toward zero. It is 0 when the entry filled and nothing closed yet. It is
   * null when the fills carry no prices, or when the entry has no fills to measure against, as for
   * an exit group whose entry is gone.
   */
  pnl: bigint | null;
  /**
   * The spread relative to the entry's average price, in percent with 2 decimals. Null when
   * unknown.
   */
  returnPct: number | null;
}

/**
 * Realised PnL of a position from its loaded fills. It is the spread per whole token times the
 * amount both entered and exited. It is before priority tips, stop-exit keeper tips and network
 * fees. A fill event carries none of them, and the indexer's events have no tip field, since a tip
 * in force can have been amended. So none is subtracted, and the note says so.
 */
export function realisedPnl(f: PositionFills): RealisedPnl {
  const amountClosed = f.entry.amount < f.exits.amount ? f.entry.amount : f.exits.amount;
  // No entry fill, as for an exit group whose entry is gone or an entry that never filled. There is
  // nothing to measure the exits against, which is not a realised zero.
  if (f.entry.amount <= 0n) return { amountClosed: 0n, pnl: null, returnPct: null };
  if (amountClosed <= 0n) return { amountClosed: 0n, pnl: 0n, returnPct: null };
  if (f.spreadPerToken === null) return { amountClosed, pnl: null, returnPct: null };
  const base = f.entry.avgPrice;
  const returnPct = base !== null && base > 0n ? Number((f.spreadPerToken * 10_000n) / base) / 100 : null;
  return { amountClosed, pnl: (f.spreadPerToken * amountClosed) / f.scale, returnPct };
}

/**
 * Fill ratio 0..1 of a progress bar. It is null when the denominator is unknown or not positive, so
 * there is no bar and never NaN.
 */
export function ratio(done: bigint | null | undefined, total: bigint | null | undefined): number | null {
  if (done === null || done === undefined || total === null || total === undefined || total <= 0n) return null;
  const r = Number((done * 1_000_000n) / total) / 1_000_000;
  return Math.min(1, Math.max(0, r));
}
