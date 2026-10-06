import { describe, expect, it } from 'vitest';
import { groupPositions } from '../../kob/positions';
import { loadKobNode } from '../../kob/wasm.node';
import { orderViewOf, placeGolden } from '../../testing/chain-fixtures';
import { buildListItems, cancellableIds, filterItems, itemIsActive, tabCounts } from './list-model';
import { buildEntries, describeEntry } from './orders-model';

const kob = loadKobNode();
const ctx = { kob, clock: { daa: 1000n, unixSeconds: 1_790_000_000n, rateMilli: 10_000 } };

const mk = (name: string, over = {}) => orderViewOf(kob, placeGolden(kob, name).snapshots[0], over);

describe('list items', () => {
  // an if-done entry with two exits booked by its fills, a plain order, a finished order
  const entry = mk('create.ifdBid', { covenant_id: 'e1'.repeat(32), genesis: { txid: null, out: null, block_seq: 5, daa: 500, confirmations: 1, settled: true } });
  const exit1 = mk('create.condAsk', { covenant_id: 'e2'.repeat(32), parent: entry.covenant_id, genesis: { txid: null, out: null, block_seq: 6, daa: 600, confirmations: 1, settled: true } });
  const exit2 = mk('create.condAsk', { covenant_id: 'e3'.repeat(32), parent: entry.covenant_id, status: 'filled', genesis: { txid: null, out: null, block_seq: 7, daa: 700, confirmations: 1, settled: true } });
  const plain = mk('create.ask', { covenant_id: 'a1'.repeat(32), genesis: { txid: null, out: null, block_seq: 9, daa: 900, confirmations: 1, settled: true } });
  const done = mk('create.bid', { covenant_id: 'b1'.repeat(32), status: 'cancelled', genesis: { txid: null, out: null, block_seq: 3, daa: 300, confirmations: 1, settled: true } });
  const views = [entry, exit1, exit2, plain, done];
  const entries = buildEntries(views, []);
  const positions = groupPositions(views);
  const items = buildListItems(entries, positions);
  const rows = new Map(entries.map((e) => [e.id, describeEntry(e, ctx)]));

  it('one card per position (entry + exits), newest first, plain orders as rows', () => {
    expect(items.map((i) => (i.type === 'single' ? `s:${i.id.slice(0, 2)}` : `p:${i.id.slice(0, 2)}`))).toEqual(['s:a1', 'p:e1', 's:b1']);
    const pos = items.find((i) => i.type === 'position')!;
    expect(pos.type === 'position' && pos.memberIds).toEqual([entry.covenant_id, exit1.covenant_id, exit2.covenant_id]);
  });

  it('every entry appears exactly once', () => {
    const ids = items.flatMap((i) => (i.type === 'single' ? [i.id] : i.memberIds));
    expect(ids.sort()).toEqual(entries.map((e) => e.id).sort());
  });

  it('a position is active while any member is live; a finished order is history', () => {
    const [plainItem, posItem, doneItem] = items;
    expect(itemIsActive(plainItem, rows)).toBe(true);
    expect(itemIsActive(posItem, rows)).toBe(true);
    expect(itemIsActive(doneItem, rows)).toBe(false);
    expect(tabCounts(items, rows)).toEqual({ active: 2, history: 1, all: 3 });
    expect(filterItems(items, rows, 'history').map((i) => i.id)).toEqual([done.covenant_id]);
    expect(filterItems(items, rows, 'active')).toHaveLength(2);
    expect(filterItems(items, rows, 'all')).toHaveLength(3);
  });

  it('an order of unknown fate stays in the active tab (it needs attention)', () => {
    const v = mk('create.ask', { status: 'unknown' as never });
    const es = buildEntries([v], []);
    const r = new Map([[v.covenant_id, describeEntry(es[0], ctx)]]);
    expect(itemIsActive(buildListItems(es, [])[0], r)).toBe(true);
  });

  it('cancel-all targets only live cancellable orders, optionally of one token', () => {
    const all = [...rows.values()];
    expect(cancellableIds(all, null).sort()).toEqual([entry.covenant_id, exit1.covenant_id, plain.covenant_id].sort());
    const token = rows.get(plain.covenant_id)!.token;
    expect(cancellableIds(all, token).length).toBeGreaterThan(0);
    expect(cancellableIds(all, 'ff'.repeat(32))).toEqual([]);
  });
});
