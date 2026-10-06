// pollWallet's order half: a live order that fell out of the lists is asked for by id; only a definite "not found" reads as reverted.
import { describe, expect, it } from 'vitest';
import type { Services } from '../../app/services';
import type { OrderView } from '../../data/indexer-types';
import { NotificationStore } from '../../kob/notifications';
import { pollWallet } from './poll';

const ID = 'aa'.repeat(32);
const ID2 = 'bb'.repeat(32);
const TOKEN = 'cc'.repeat(32);
const PK = 'dd'.repeat(32);
const NOW = 1_800_000_000_000;

const view = (o: Partial<OrderView> = {}): OrderView =>
  ({
    covenant_id: ID, contract: 'KobAsk', side: 1, maker: PK, token: TOKEN, price: '2505000', scale: 100000000, initial_amount: '1000000000', status: 'open', filled_amount: '0', amount_left: '1000000000', expired: false, ...o,
  }) as OrderView;

class Mem {
  d = new Map<string, string>();
  getItem(k: string) { return this.d.get(k) ?? null; }
  setItem(k: string, v: string) { this.d.set(k, v); }
  removeItem(k: string) { this.d.delete(k); }
}

function services(state: { active: OrderView[]; recent: OrderView[]; byId: Map<string, OrderView | null | Error> }): Services {
  return {
    indexer: {
      allOrders: async () => state.active,
      orders: async () => ({ items: state.recent }),
      order: async (id: string) => {
        const r = state.byId.get(id);
        if (r instanceof Error) throw r;
        return r ?? null;
      },
      tokenUtxos: async () => [],
    },
    node: { getClock: async () => { throw new Error('no clock'); } },
    utxos: { fundingFor: async () => [] },
  } as unknown as Services;
}

const store = () => new NotificationStore(new Mem(), 'testnet-10', PK);

describe('pollWallet: chain re-organisations', () => {
  it('a fill that disappears is reported once, with the amount and the price', async () => {
    const st = store();
    const s1 = services({ active: [view({ status: 'partial', filled_amount: '300000000', amount_left: '700000000' })], recent: [], byId: new Map() });
    await pollWallet(s1, st, PK, NOW); // baseline
    const s2 = services({ active: [view()], recent: [], byId: new Map() });
    const r = await pollWallet(s2, st, PK, NOW + 15_000);
    expect(r.events.map((e) => e.kind)).toEqual(['reorg']);
    expect(r.events[0]!.params).toMatchObject({ what: 'fill', amount: '300000000', price: '2505000', state: 'open' });
    expect((await pollWallet(s2, st, PK, NOW + 30_000)).events).toEqual([]); // the snapshot moved on: no repeat
  });

  it('an ordinary poll (nothing went backwards) reports no reorg', async () => {
    const st = store();
    await pollWallet(services({ active: [view()], recent: [], byId: new Map() }), st, PK, NOW);
    const r = await pollWallet(services({ active: [view({ status: 'partial', filled_amount: '200000000', amount_left: '800000000' })], recent: [], byId: new Map() }), st, PK, NOW + 15_000);
    expect(r.events.map((e) => e.kind)).toEqual(['partial']);
  });

  it('a live order missing from both lists but still known by id is not a reorg; one the indexer does not know is', async () => {
    const st = store();
    await pollWallet(services({ active: [view(), view({ covenant_id: ID2 })], recent: [], byId: new Map() }), st, PK, NOW);
    const byId = new Map<string, OrderView | null | Error>([[ID, view({ status: 'filled', filled_amount: '1000000000', amount_left: '0' })], [ID2, null]]);
    const r = await pollWallet(services({ active: [], recent: [], byId }), st, PK, NOW + 15_000);
    expect(r.events.map((e) => `${e.kind}:${e.orderId === ID ? 'a' : 'b'}`).sort()).toEqual(['fill:a', 'reorg:b']);
    expect(r.events.find((e) => e.kind === 'reorg')!.params).toMatchObject({ what: 'gone' });
    expect(st.snapshots()!.has(ID2 as never)).toBe(false);
  });

  it('a failed lookup reports nothing and keeps the snapshot for the next poll', async () => {
    const st = store();
    await pollWallet(services({ active: [view()], recent: [], byId: new Map() }), st, PK, NOW);
    const r = await pollWallet(services({ active: [], recent: [], byId: new Map([[ID, new Error('503')]]) }), st, PK, NOW + 15_000);
    expect(r.events).toEqual([]);
    expect(st.snapshots()!.has(ID as never)).toBe(true);
  });
});
