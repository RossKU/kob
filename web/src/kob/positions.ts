// Positions: an if-done / if-one-cancels entry or a repeat entry grouped with its exits, one logical position for the UI (matcher.md 6, 6.1, 10.8, 10.9).
//
// How the indexer links orders (crates/kob-executor/src/indexer/reads.rs): an exit's `parent` is its entry's covenant id; the detail view of an
// entry lists `children` (exit ids); `repeat` is `{role:'entry', rpt_amount, rearm_amount = rpt_amount - 1}` on a repeating entry and
// `{role:'exit', parent}` on a booked exit. We use all three, so grouping works on list views (no `children`) and detail views alike.
// A pair position (a `KobIfdPair` entry and its `KobCondPair` exits) is one pair: amounts are base units of A, `quote` names B.
import type { OrderView } from '../data/indexer-types';
import { pairOfView } from './pair-view';
import type { Hex } from './types';

export type PositionKind = 'single' | 'ifd' | 'repeat';
export type PositionStatus = 'open' | 'partial' | 'exits-only' | 'waiting' | 'closed';

export interface Position {
  /** entry covenant id (or the order's own id for a standalone order; the parent id for an orphan exit group) */
  id: Hex;
  kind: PositionKind;
  /** null when only exits are known (entry closed or not in the input) */
  entry: OrderView | null;
  /** exits ordered by genesis DAA (oldest first) */
  exits: OrderView[];
  /** exits whose entry is not among the input views */
  orphanExits: boolean;
  side: 'sell' | 'buy';
  /** a pair position: its quote token B (amounts stay base units of A); null for a KAS-quoted position */
  quote: Hex | null;
  /**
   * open: entry live, nothing filled yet. partial: entry live with fills (exits exist). exits-only: entry gone, exits still live.
   * waiting: a repeating entry that is sold out (amount 0) and waits for its exits to re-arm it. closed: every member terminal.
   */
  status: PositionStatus;
  /** the entry's initial amount in base units (single: the order's); null when unknown */
  amountTotal: bigint | null;
  /** base units the entry has filled so far (single: filled_amount) */
  amountDone: bigint;
  /** open base units: the entry's amount left plus the amount left of every live exit; null when the entry's quantity is a KAS budget */
  amountLeft: bigint | null;
  /**
   * Repeat positions only. `done` = booked exits that sold out on their take-profit (completed cycles, one per exit); `rearmAmount` = base units
   * the entry can still re-arm (`repeat.rearm_amount`, i.e. rptAmount - 1); null when the entry is not in the input.
   */
  cycles: { done: number; rearmAmount: bigint | null } | null;
  /** every LIVE (open / partial) order of the position, entry first then exits: exactly what `planCancelAll` cancels in one go */
  cancelIds: Hex[];
}

// `killed`: an IOC / FOK / market order whose unfilled rest was returned (indexer processor.rs `(None, Some("kill")) => "killed"`)
const TERMINAL = new Set(['filled', 'cancelled', 'refunded', 'killed', 'closed']);
const LIVE = new Set(['open', 'partial']);
export const isTerminalStatus = (s: string): boolean => TERMINAL.has(s);
export const isLiveStatus = (s: string): boolean => LIVE.has(s);

const byGenesis = (a: OrderView, b: OrderView): number =>
  a.genesis.daa - b.genesis.daa || a.genesis.block_seq - b.genesis.block_seq || (a.covenant_id < b.covenant_id ? -1 : 1);

/** Contract name without the family suffix: `KobIfdBidKron` -> `KobIfdBid` (the KRON family shares every rule of the KCC-20 kinds). */
export const baseContract = (c: string | null | undefined): string => (c ?? '').replace(/Kron$/, '');

const parentOf = (v: OrderView): Hex | null => v.parent ?? (v.repeat?.role === 'exit' ? (v.repeat.parent ?? null) : null);

/** A decimal string of the API (base units) as a bigint; null when absent or malformed. */
const amountOf = (v: string | null | undefined): bigint | null => (typeof v === 'string' && /^\d+$/.test(v) ? BigInt(v) : null);

function build(id: Hex, entry: OrderView | null, exits: OrderView[], orphan: boolean): Position {
  exits = [...exits].sort(byGenesis);
  const members = entry ? [entry, ...exits] : exits;
  const ref = entry ?? exits[0];
  const repeat = entry?.repeat?.role === 'entry' || exits.some((e) => e.repeat?.role === 'exit');
  const base = baseContract(entry?.contract);
  const ifd = base === 'KobIfdBid' || base === 'KobIfdAsk' || base === 'KobIfdPair';
  const kind: PositionKind = repeat ? 'repeat' : exits.length || entry?.children?.length || orphan || ifd ? 'ifd' : 'single';
  const entryLive = !!entry && isLiveStatus(entry.status);
  const exitAmount = exits.filter((e) => isLiveStatus(e.status)).reduce((s, e) => s + (amountOf(e.amount_left) ?? 0n), 0n);
  const entryLeft = entry ? amountOf(entry.amount_left) : null;
  let status: PositionStatus;
  if (members.every((m) => isTerminalStatus(m.status))) status = 'closed';
  else if (!entryLive) status = 'exits-only';
  else if (entryLeft === 0n && entry!.repeat) status = 'waiting';
  else status = (amountOf(entry!.filled_amount) ?? 0n) > 0n || exits.length ? 'partial' : 'open';
  return {
    id, kind, entry, exits, orphanExits: orphan,
    side: ref.side === 1 ? 'sell' : 'buy',
    quote: members.map((m) => pairOfView(m)?.quote ?? null).find((q) => q !== null) ?? null,
    status,
    amountTotal: entry ? amountOf(entry.initial_amount) : null,
    amountDone: entry ? (amountOf(entry.filled_amount) ?? 0n) : 0n,
    amountLeft: entry && entry.amount_left === null ? null : (entryLeft ?? 0n) + exitAmount,
    cycles: kind === 'repeat' ? { done: exits.filter((e) => e.status === 'filled').length, rearmAmount: amountOf(entry?.repeat?.rearm_amount) } : null,
    cancelIds: members.filter((m) => isLiveStatus(m.status)).map((m) => m.covenant_id),
  };
}

/**
 * Groups views into positions. An entry is any order that is the parent of another in the input (or lists `children`, or is a repeat entry);
 * its exits are the orders naming it as `parent`. Exits whose entry is absent form an orphan group per parent id. Everything else is a
 * `single`. Result: newest genesis first (by the entry, or the newest exit of an orphan group), ties broken by id.
 */
export function groupPositions(views: OrderView[]): Position[] {
  const byId = new Map(views.map((v) => [v.covenant_id, v]));
  const exitsOf = new Map<Hex, OrderView[]>();
  const attached = new Set<Hex>();
  for (const v of views) {
    const p = parentOf(v);
    if (!p || p === v.covenant_id) continue;
    (exitsOf.get(p) ?? exitsOf.set(p, []).get(p)!).push(v);
    attached.add(v.covenant_id);
  }
  // children lists cover exits the input already contains but whose own `parent` is missing (older list views)
  for (const v of views) {
    for (const c of v.children ?? []) {
      const ch = byId.get(c);
      if (ch && !attached.has(c) && c !== v.covenant_id) {
        (exitsOf.get(v.covenant_id) ?? exitsOf.set(v.covenant_id, []).get(v.covenant_id)!).push(ch);
        attached.add(c);
      }
    }
  }
  const out: { p: Position; t: number }[] = [];
  for (const v of views) {
    if (attached.has(v.covenant_id)) continue;
    const exits = exitsOf.get(v.covenant_id) ?? [];
    const p = build(v.covenant_id, v, exits, false);
    out.push({ p, t: v.genesis.daa });
  }
  for (const [parent, exits] of exitsOf) {
    if (byId.has(parent)) continue;
    const p = build(parent, null, exits, true);
    out.push({ p, t: Math.max(...exits.map((e) => e.genesis.daa)) });
  }
  return out.sort((a, b) => b.t - a.t || (a.p.id < b.p.id ? -1 : a.p.id > b.p.id ? 1 : 0)).map((x) => x.p);
}

/** The position that contains `id` (as entry or as one of its exits), or null. */
export function positionOf(views: OrderView[], id: Hex): Position | null {
  return groupPositions(views).find((p) => p.id === id || p.entry?.covenant_id === id || p.exits.some((e) => e.covenant_id === id)) ?? null;
}
