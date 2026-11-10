// The activity log's reads (ActivityStore) against a fake indexer: the sources per market filter, the horizon that keeps the two sources from
// interleaving with a gap, the reads further back, a gap read after a pause, the pair page's own copy of the pair fills, standalone mode and the owner lookups.
import { describe, expect, it, vi } from 'vitest';
import type { IndexerApi } from '../../data/indexer';
import type { EventView, OrderView, PairFillsView } from '../../data/indexer-types';
import { IndexerCarry } from '../../data/stream-carry';
import { ActivityStore } from './activity-store';
import type { ActivityKind } from './activity-model';

const T = 'aa'.repeat(32);
const U = 'bb'.repeat(32);
const hex = (n: number) => n.toString(16).padStart(64, '0');
const KINDS: ReadonlySet<ActivityKind> = new Set(['placed', 'fill']);

const ev = (id: number, daa: number, token = T): EventView => ({
  id, covenant_id: hex(10_000 + id), block_seq: 0, daa, ts: daa * 100, txid: hex(id), tx_pos: 0, kind: 'fill', token, side: 1, amount: '100', price: '1000', payout: null, closes: false,
  detail: null, confirmations: 5, settled: false,
});
const ord = (id: number, daa: number, o: Partial<OrderView> = {}): OrderView =>
  ({
    covenant_id: hex(20_000 + id), contract: 'KobAsk', side: 1, maker: 'cd'.repeat(32), token: T, scale: 100, price: '1000', initial_amount: '100', amount_left: '100',
    genesis: { txid: hex(50_000 + id), out: 0, block_seq: id, daa, confirmations: 5, settled: false }, ...o,
  }) as OrderView;

/** A fake indexer over fixed lists (newest first), paged like the real one. */
function fakeIndexer(fills: EventView[], orders: OrderView[]) {
  const calls: string[] = [];
  const ix = {
    fills: vi.fn(async (q: { token?: string; limit?: number; before?: string | number }) => {
      calls.push(`fills ${q.token ?? '*'} ${q.before ?? '-'}`);
      const all = fills.filter((e) => (!q.token || e.token === q.token) && (q.before === undefined || e.id < Number(q.before)));
      const items = all.slice(0, q.limit ?? 50);
      return { items, next_cursor: all.length > items.length ? String(items[items.length - 1]!.id) : null };
    }),
    orders: vi.fn(async (q: { token?: string; limit?: number; cursor?: string }) => {
      calls.push(`orders ${q.token ?? '*'} ${q.cursor ?? '-'}`);
      const start = q.cursor ? Number(q.cursor) : 0;
      const all = orders.filter((o) => !q.token || o.token === q.token);
      const items = all.slice(start, start + (q.limit ?? 50));
      return { items, next_cursor: start + items.length < all.length ? String(start + items.length) : null };
    }),
    pairFills: vi.fn(async () => {
      calls.push('pairFills');
      return null;
    }),
    order: vi.fn(async (id: string) => ({ covenant_id: id, maker: 'ef'.repeat(32), scale: 100, genesis: { daa: 1 } }) as unknown as OrderView),
  };
  return { ix: ix as unknown as IndexerApi, calls, raw: ix };
}

const storeOf = (ix: IndexerApi | null, market: ConstructorParameters<typeof ActivityStore>[0] = { kind: 'all' }, extra: Partial<ConstructorParameters<typeof ActivityStore>[1]> = {}) =>
  new ActivityStore(market, { indexer: ix, standalone: null, ...extra }, () => 100n, () => undefined, () => 0);

describe('activity store', () => {
  it('the first read takes one page of each source; rows below the horizon of a source with more wait for a read further back', async () => {
    // fills cover DAA 1000..951 in the first page (60 in all); placements 1000..990 step 1 (but only 50 fit); so the horizon is the higher of the two
    const fills = Array.from({ length: 60 }, (_, i) => ev(1000 - i, 1000 - i));
    const orders = Array.from({ length: 60 }, (_, i) => ord(i, 1000 - i * 2));
    const { ix } = fakeIndexer(fills, orders);
    const s = storeOf(ix);
    await s.readNewest({ fills: true, placed: true });
    expect(s.fillsCursor).toBe('951');
    expect(s.ordersCursor).toBe('50');
    // fills reach DAA 951, placements 902: rows older than 951 wait
    expect(s.horizon(KINDS)).toBe(951);
    expect(Math.min(...s.rows(KINDS).map((r) => r.daa))).toBe(951);
    expect(s.hasOlder(KINDS)).toBe(true);
    await s.readOlder(KINDS);
    expect(s.fillsCursor).toBeNull();
    // the fills are complete now; the placements still have more: their horizon applies
    expect(s.horizon(KINDS)).toBe(s.horizon(new Set(['placed'])));
    // placements hidden: only the fills' horizon counts (none now)
    expect(s.horizon(new Set(['fill']))).toBeNull();
  });

  it('a token\'s log reads that token\'s fills and orders; a pair\'s reads the pair fills and the base token\'s orders (pair orders only)', async () => {
    const { ix, calls } = fakeIndexer([ev(2, 2, T), ev(1, 1, U)], [ord(1, 5), ord(2, 6, { pair: { base: T, quote: U, side: 'ask' } as OrderView['pair'] })]);
    const tok = storeOf(ix, { kind: 'token', token: T });
    await tok.readNewest({ fills: true, placed: true });
    expect(calls).toEqual([`fills ${T} -`, `orders ${T} -`]);
    // the pair order of T is not a row of T's KAS book
    expect(tok.rows(KINDS).map((r) => r.market.kind)).toEqual(['token', 'token']);
    calls.length = 0;
    const pair = storeOf(ix, { kind: 'pair', base: U, quote: T });
    await pair.readNewest({ fills: true, placed: true });
    // both orientations: the pair fills of U/T and T/U, and the orders listed under either token
    expect([...calls].sort()).toEqual(['pairFills', 'pairFills', `orders ${T} -`, `orders ${U} -`].sort());
  });

  it('the pair page\'s copy of the pair fills is used, never read again', async () => {
    const { ix, calls } = fakeIndexer([], []);
    const view: PairFillsView = {
      base: T, quote: U, volume_24h: { amount_a: '0', amount_b: '0', fills: 0 }, next_cursor: null,
      items: [{ id: 1, txid: hex(7), ts: 1, daa: 9, order: hex(8), contract: 'KobPair', side: 'ask', amount_a: '5', amount_b: '6', price: null, price_num: null, price_den: null, a_scale: 1, tip_kas: null, counterparty: 'route', price_source: 'none', confirmations: 1, settled: false }],
    };
    const s = storeOf(ix, { kind: 'pair', base: T, quote: U }, { pairFills: { base: T, quote: U, view } });
    await s.readNewest({ fills: true, placed: false });
    expect(calls).toEqual([]);
    expect(s.rows(KINDS).map((r) => r.key)).toEqual([hex(7)]);
  });

  it('a newest page that does not reach what is known reads on (a gap after a pause), a few pages at most', async () => {
    const fills = Array.from({ length: 300 }, (_, i) => ev(300 - i, 300 - i));
    const { ix, raw } = fakeIndexer(fills.slice(200), []);
    const s = storeOf(ix);
    await s.readNewest({ fills: true, placed: false });
    expect(s.fills.size).toBe(50);
    // 100 newer fills arrived meanwhile: two pages close the gap
    (raw.fills as ReturnType<typeof vi.fn>).mockImplementation(async (q: { before?: string }) => {
      const all = fills.filter((e) => q.before === undefined || e.id < Number(q.before));
      const items = all.slice(0, 50);
      return { items, next_cursor: String(items[items.length - 1]!.id) };
    });
    await s.readNewest({ fills: true, placed: false });
    expect(s.fills.size).toBe(200);
  });

  it('standalone mode: what the page saw, nothing older', async () => {
    const seen = { fills: [ev(1, 10)], orders: [ord(1, 11)] };
    const s = storeOf(null, { kind: 'all' }, { standalone: { seenActivity: () => seen } });
    await s.readNewest({ fills: true, placed: true });
    expect(s.rows(KINDS).map((r) => r.kind)).toEqual(['placed', 'fill']);
    expect(s.hasOlder(KINDS)).toBe(false);
  });

  it('owner lookups: each order once, a few at a time; placements already name theirs', async () => {
    const { ix, raw } = fakeIndexer([ev(1, 1), ev(2, 2)], [ord(1, 3)]);
    const s = storeOf(ix);
    await s.readNewest({ fills: true, placed: true });
    const ids = s.rows(KINDS).flatMap((r) => r.orders);
    s.want(ids);
    s.want(ids);
    await vi.waitFor(() => expect(s.facts.size).toBe(3));
    expect(raw.order).toHaveBeenCalledTimes(2);
    expect(s.facts.get(hex(20_001))!.maker).toBe('cd'.repeat(32));
    expect(s.facts.get(hex(10_001))!.maker).toBe('ef'.repeat(32));
  });

  it('the chain DAA from the records (DAA + confirmations) and a time reference from the newest fill', async () => {
    const { ix } = fakeIndexer([ev(1, 100)], []);
    const s = storeOf(ix);
    await s.readNewest({ fills: true, placed: false });
    expect(s.chain).toEqual({ daa: 105, atMs: 0 });
    expect(s.timeRef).toEqual({ daa: 100, ms: 10_000 });
    s.noteChain(200);
    expect(s.chain!.daa).toBe(200);
  });

  it('standalone mode keeps the rows the indexer gave and adds what the node showed; a fill both show is one row', async () => {
    const carry = new IndexerCarry(() => 0);
    const { ix } = fakeIndexer([ev(901, 900), ev(900, 899)], [ord(1, 898)]);
    // the indexer's store of this market parks itself once it has read
    const before = storeOf(ix, { kind: 'all' }, { carry });
    expect(carry.parked('all')).toBeNull();
    await before.readNewest({ fills: true, placed: true });
    expect(carry.parked('all')).toBe(before);
    expect(before.lastReadMs).toBe(0);
    // standalone mode starts: the store of the mode is seeded from it, the node's fills come with their own keys (ids of this page, not the indexer's)
    const seen = { fills: [{ ...ev(901, 900), id: 1 }, { ...ev(1, 905), id: 2, txid: hex(77) }], orders: [ord(2, 906)] };
    const s = storeOf(null, { kind: 'all' }, { carry, standalone: { seenActivity: () => seen } });
    expect(s.keptUntilMs).toBe(0);
    await s.readNewest({ fills: true, placed: true });
    // the indexer's two fills and its placement, the node's new fill and placement; the node's repeat of the first fill (same transaction) is not a second row
    expect(s.rows(KINDS).map((r) => r.kind).sort()).toEqual(['fill', 'fill', 'fill', 'placed', 'placed']);
    expect(s.hasOlder(KINDS)).toBe(false);
    // the indexer's store is untouched
    expect(before.fills.size).toBe(2);
  });

  it('standalone mode without anything the indexer gave starts empty and says nothing was kept', async () => {
    const carry = new IndexerCarry(() => 0);
    const s = storeOf(null, { kind: 'all' }, { carry, standalone: { seenActivity: () => ({ fills: [], orders: [] }) } });
    expect(s.keptUntilMs).toBeNull();
    // a market page that never read does not park an empty store
    const { ix } = fakeIndexer([], []);
    const empty = storeOf(ix, { kind: 'token', token: T }, { carry });
    await empty.readNewest({ fills: true, placed: true });
    expect(carry.parked(T)).toBeNull();
  });
});
