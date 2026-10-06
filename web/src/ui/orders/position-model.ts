// What a position card shows above its member rows: the phase chip, prices along the entry -> exit path, amounts, repeat state, and (loaded on
// demand from the order events) the realised fills. Pure: built from `groupPositions` (kob/positions.ts) and the row models (orders-model.ts).
// Amounts are token base units, prices state prices (sompi per whole token of the orders' `scale` base units).
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
 * The state chip of a position, in the user's words.
 *  entry-waiting: the entry rests, nothing filled. entry-partial: the entry rests and part of it has filled (its exits are working).
 *  exit-resting: the entry is done, only exits are left. repeat-waiting: a repeating entry is sold out and waits for its exits to re-arm it.
 *  closed: everything ended after trading. cancelled: everything ended and nothing ever traded.
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
  /** take-profit (limit) price, sompi per whole token */
  takeProfit: bigint | null;
  /** stop trigger, sompi per whole token (OCO exits) */
  stop: bigint | null;
  /** base units left / at creation */
  amountLeft: bigint | null;
  amountTotal: bigint | null;
}

export interface PositionSummary {
  phase: PositionPhase;
  token: Hex | null;
  side: 'sell' | 'buy';
  /** the entry's limit, sompi per whole token (null: unknown or a budget entry) */
  entryPrice: bigint | null;
  entryAllIn: bigint | null;
  /** the entry's trigger for stop entries */
  entryStop: bigint | null;
  exits: ExitLeg[];
  /** distinct take-profit prices of the exits that are still live, ascending */
  exitTakeProfits: bigint[];
  /** base units of the entry (one cycle) */
  amountTotal: bigint | null;
  /** base units the entry has filled */
  amountEntered: bigint;
  /** base units the exits have filled (that completed the round trip) */
  amountExited: bigint;
  /** open base units (entry remainder + live exits); null when the entry is a KAS budget */
  amountOpen: bigint | null;
  /** repeat: booked exits that sold out / base units the entry can still re-arm / the latest `rpt_until` DAA of the exits (null: none) */
  repeat: { cyclesDone: number; rearmAmount: bigint | null; untilDaa: bigint | null; untilUnix: bigint | null } | null;
}

/** A decimal string of the API (base units) as a bigint (0 allowed); null when absent or malformed. */
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
    amountTotal: p.amountTotal,
    amountEntered: p.amountDone,
    amountExited: p.exits.reduce((s, x) => s + (amountOf(x.filled_amount) ?? 0n), 0n),
    amountOpen: p.amountLeft,
    repeat,
  };
}

/**
 * Base units the position still has to unwind once its orders are cancelled (close): what its entry filled minus what its exits filled.
 * Buy first: tokens bought and not sold yet (they return to the wallet with the cancel; the close sells them). Sell first: tokens sold and not
 * bought back yet (the cancel returns the KAS the exits held; the close buys them back at market). Never negative.
 */
export function unwindAmount(p: Position): bigint {
  const exited = p.exits.reduce((s, x) => s + (amountOf(x.filled_amount) ?? 0n), 0n);
  return p.amountDone > exited ? p.amountDone - exited : 0n;
}

// ------------------------------------------------------------------------------------------------ realised fills

export interface FillSummary {
  /** number of fill events */
  fills: number;
  /** base units filled */
  amount: bigint;
  /** amount-weighted average fill price, sompi per whole token (rounded down); null: no priced fill */
  avgPrice: bigint | null;
  /** KAS paid out to the maker by sell-side fills, sompi */
  payout: bigint;
}

export interface PositionFills {
  entry: FillSummary;
  exits: FillSummary;
  /** realised spread per whole token for a completed leg pair: exits' avg minus entry's avg (buy first) or the reverse (sell first); null when unknown */
  spreadPerToken: bigint | null;
  /** the orders' scale (base units per whole token) the prices are quoted per */
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
 * Sums the fill events of the entry and of the exits (each list is one order's `/v1/orders/{id}/events`). Duplicate events (same id) count once.
 * `scale` is the orders' scale (an entry and its exits always share it).
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
  /** base units that completed the round trip: filled by the entry AND by the exits */
  amountClosed: bigint;
  /** realised profit or loss in sompi (the spread per whole token times the amount closed over the scale, rounded toward zero; 0 when nothing closed yet); null when the fills carry no prices */
  pnl: bigint | null;
  /** the spread relative to the entry's average price, percent (2 decimals); null when unknown */
  returnPct: number | null;
}

/** Realised PnL of a position from its loaded fills (before network fees): the spread per whole token times the amount both entered and exited. */
export function realisedPnl(f: PositionFills): RealisedPnl {
  const amountClosed = f.entry.amount < f.exits.amount ? f.entry.amount : f.exits.amount;
  if (amountClosed <= 0n) return { amountClosed: 0n, pnl: 0n, returnPct: null };
  if (f.spreadPerToken === null) return { amountClosed, pnl: null, returnPct: null };
  const base = f.entry.avgPrice;
  const returnPct = base !== null && base > 0n ? Number((f.spreadPerToken * 10_000n) / base) / 100 : null;
  return { amountClosed, pnl: (f.spreadPerToken * amountClosed) / f.scale, returnPct };
}

/** Fill ratio 0..1 of a progress bar; null when the denominator is unknown or not positive (no bar, never NaN). */
export function ratio(done: bigint | null | undefined, total: bigint | null | undefined): number | null {
  if (done === null || done === undefined || total === null || total === undefined || total <= 0n) return null;
  const r = Number((done * 1_000_000n) / total) / 1_000_000;
  return Math.min(1, Math.max(0, r));
}
