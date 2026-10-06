// What the orders list shows, in which order: positions (an entry with its exits) as one card, every other order as a row; tabs for active / history.
// Pure: built from the merged entries (orders-model.ts) and the grouped positions (kob/positions.ts).
import type { Position } from '../../kob/positions';
import type { Hex } from '../../kob/types';
import type { OrderEntry, OrderRowModel } from './orders-model';

export type ListItem =
  | { type: 'single'; id: Hex }
  | { type: 'position'; id: Hex; position: Position; /** entry first, then exits; only members the wallet's entries know */ memberIds: Hex[] };

export type ListTab = 'active' | 'history' | 'all';

/**
 * Entries are newest first; the first encountered member of a multi-order position emits the card for the whole position. Single-order positions
 * and orders without a position (records the indexer does not know) are plain rows.
 */
export function buildListItems(entries: readonly OrderEntry[], positions: readonly Position[]): ListItem[] {
  const known = new Set(entries.map((e) => e.id));
  const positionOf = new Map<Hex, Position>();
  for (const p of positions) {
    if (p.kind === 'single') continue;
    for (const id of [p.entry?.covenant_id, ...p.exits.map((x) => x.covenant_id)]) if (id) positionOf.set(id, p);
  }
  const out: ListItem[] = [];
  const emitted = new Set<Hex>();
  for (const e of entries) {
    const p = positionOf.get(e.id);
    if (!p) {
      out.push({ type: 'single', id: e.id });
      continue;
    }
    if (emitted.has(p.id)) continue;
    emitted.add(p.id);
    const ids = [p.entry?.covenant_id, ...p.exits.map((x) => x.covenant_id)].filter((x): x is Hex => !!x && known.has(x));
    out.push({ type: 'position', id: p.id, position: p, memberIds: ids });
  }
  return out;
}

export const itemMemberIds = (item: ListItem): Hex[] => (item.type === 'single' ? [item.id] : item.memberIds);

/** An item is active while any member can still act (live), or when a member's fate is not known (it needs the user's attention). */
export function itemIsActive(item: ListItem, rows: ReadonlyMap<Hex, OrderRowModel>): boolean {
  return itemMemberIds(item).some((id) => {
    const r = rows.get(id);
    return !!r && (r.live || r.status === 'unknown');
  });
}

export function filterItems(items: readonly ListItem[], rows: ReadonlyMap<Hex, OrderRowModel>, tab: ListTab): ListItem[] {
  if (tab === 'all') return [...items];
  return items.filter((i) => itemIsActive(i, rows) === (tab === 'active'));
}

export function tabCounts(items: readonly ListItem[], rows: ReadonlyMap<Hex, OrderRowModel>): { active: number; history: number; all: number } {
  const active = items.filter((i) => itemIsActive(i, rows)).length;
  return { active, history: items.length - active, all: items.length };
}

/** Live, cancellable orders of a token (or of all tokens when `token` is null): what "cancel all" acts on. */
export function cancellableIds(rows: readonly OrderRowModel[], token: Hex | null): Hex[] {
  return rows.filter((r) => r.canCancel && (token === null || r.token === token)).map((r) => r.id);
}
