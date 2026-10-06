import { describe, expect, it } from 'vitest';
import { loadKobNode } from './wasm.node';
import { groupPositions, positionOf } from './positions';
import type { OrderView } from '../data/indexer-types';
import { orderViewOf, placeGolden } from '../testing/chain-fixtures';

const kob = loadKobNode();
const id = (c: string) => c.repeat(32);
const entrySnap = placeGolden(kob, 'create.ifdBid').snapshots[0];
const repeatEntrySnap = placeGolden(kob, 'create.ifdBid.repeat').snapshots[0];
const exitSnap = placeGolden(kob, 'create.condAsk').snapshots[0];
const single = placeGolden(kob, 'create.ask').snapshots[0];

const genesis = (daa: number, seq = daa) => ({ txid: id('01'), out: 0, block_seq: seq, daa, confirmations: 10, settled: true });

function entry(over: Partial<OrderView> = {}): OrderView {
  return orderViewOf(kob, { ...entrySnap, covenantId: id('e1') }, { genesis: genesis(100), initial_amount: '10000', amount_left: '10000', filled_amount: '0', ...over });
}
function repeatEntry(over: Partial<OrderView> = {}): OrderView {
  return orderViewOf(kob, { ...repeatEntrySnap, covenantId: id('e2') }, {
    genesis: genesis(100), initial_amount: '4000', amount_left: '4000', filled_amount: '0', repeat: { role: 'entry', rpt_amount: '8001', rearm_amount: '8000' }, ...over,
  });
}
function exit(n: string, parent: string, over: Partial<OrderView> = {}): OrderView {
  return orderViewOf(kob, { ...exitSnap, covenantId: id(n) }, { parent: id(parent), genesis: genesis(200 + parseInt(n, 16)), initial_amount: '3000', amount_left: '3000', ...over });
}

describe('groupPositions', () => {
  it('a plain order is a single position', () => {
    const v = orderViewOf(kob, single, { genesis: genesis(50) });
    const [p] = groupPositions([v]);
    expect(p).toMatchObject({ id: v.covenant_id, kind: 'single', entry: v, exits: [], orphanExits: false, status: 'open', side: 'sell', amountTotal: 10_000n, amountDone: 0n, amountLeft: 10_000n, cycles: null });
    expect(p.cancelIds).toEqual([v.covenant_id]);
  });

  it('an unfilled if-done entry is an ifd position, open', () => {
    const [p] = groupPositions([entry()]);
    expect(p.kind).toBe('ifd');
    expect(p.status).toBe('open');
    expect(p.side).toBe('buy');
  });

  it('entry + 1..3 exits: partial, amounts and cancel ids (entry first, exits by genesis)', () => {
    for (const k of [1, 2, 3]) {
      const names = ['a1', 'a2', 'a3'].slice(0, k);
      // parent links only (list view: no children); exits given newest first to prove the sort
      const exits = names.map((n) => exit(n, 'e1')).reverse();
      const e = entry({ filled_amount: String((3 * k) * 1000), amount_left: String((10 - 3 * k) * 1000), status: 'partial' });
      const [p] = groupPositions([...exits, e]);
      expect(p.id).toBe(id('e1'));
      expect(p.kind).toBe('ifd');
      expect(p.status).toBe('partial');
      expect(p.exits.map((x) => x.covenant_id)).toEqual(names.map(id));
      expect(p.amountTotal).toBe(10_000n);
      expect(p.amountDone).toBe(BigInt(3_000 * k));
      expect(p.amountLeft).toBe(BigInt(10_000 - 3_000 * k + 3_000 * k));
      expect(p.cancelIds).toEqual([id('e1'), ...names.map(id)]);
      expect(p.orphanExits).toBe(false);
    }
  });

  it('children on the entry links exits whose own parent is missing', () => {
    const x = exit('b1', 'e1', { parent: null });
    const e = entry({ children: [id('b1')] });
    const ps = groupPositions([x, e]);
    expect(ps).toHaveLength(1);
    expect(ps[0].exits[0].covenant_id).toBe(id('b1'));
  });

  it('terminal exits are not cancellable and closed exits do not add to the amount', () => {
    const e = entry({ filled_amount: '6000', amount_left: '4000', status: 'partial' });
    const done = exit('c1', 'e1', { status: 'filled', amount_left: '0'});
    const live = exit('c2', 'e1', { status: 'open', amount_left: '3000'});
    const [p] = groupPositions([e, done, live]);
    expect(p.cancelIds).toEqual([id('e1'), id('c2')]);
    expect(p.amountLeft).toBe(4_000n + 3_000n);
  });

  it('repeat entry with booked exits: cycles and waiting status', () => {
    const e = repeatEntry({ amount_left: '0', filled_amount: '4000', status: 'partial' });
    const x1 = exit('d1', 'e2', { repeat: { role: 'exit', parent: id('e2') }, status: 'filled', amount_left: '0'});
    const x2 = exit('d2', 'e2', { repeat: { role: 'exit', parent: id('e2') }, parent: null });
    const [p] = groupPositions([e, x1, x2]);
    expect(p.kind).toBe('repeat');
    expect(p.status).toBe('waiting');
    expect(p.cycles).toEqual({ done: 1, rearmAmount: 8_000n });
    expect(p.cancelIds).toEqual([id('e2'), id('d2')]);
    expect(p.amountTotal).toBe(4_000n);
  });

  it('a repeat entry with an amount and no fills is open, kind repeat', () => {
    const [p] = groupPositions([repeatEntry()]);
    expect(p).toMatchObject({ kind: 'repeat', status: 'open', cycles: { done: 0, rearmAmount: 8_000n } });
  });

  it('an exit whose entry is not in the input is an orphan position (exits-only)', () => {
    const x1 = exit('f1', 'e9');
    const x2 = exit('f2', 'e9');
    const [p] = groupPositions([x1, x2]);
    expect(p).toMatchObject({ id: id('e9'), entry: null, orphanExits: true, status: 'exits-only', amountTotal: null, amountDone: 0n, kind: 'ifd' });
    expect(p.exits).toHaveLength(2);
    expect(p.cancelIds).toEqual([id('f1'), id('f2')]);
  });

  it('entry gone (cancelled) but exits live: exits-only, entry not cancellable', () => {
    const e = entry({ status: 'cancelled', amount_left: '0', filled_amount: '3000'});
    const x = exit('g1', 'e1');
    const [p] = groupPositions([e, x]);
    expect(p.status).toBe('exits-only');
    expect(p.cancelIds).toEqual([id('g1')]);
  });

  it('closed when every member is terminal', () => {
    const e = entry({ status: 'filled', amount_left: '0', filled_amount: '10000'});
    const x = exit('h1', 'e1', { status: 'refunded', amount_left: '0'});
    const [p] = groupPositions([e, x]);
    expect(p.status).toBe('closed');
    expect(p.cancelIds).toEqual([]);
  });

  it('a bid entry has no amount (its quantity is a budget): amountLeft is null', () => {
    const bid = placeGolden(kob, 'create.bid').snapshots[0];
    const v = orderViewOf(kob, bid, { amount_left: null, initial_amount: null });
    const [p] = groupPositions([v]);
    expect(p.amountLeft).toBeNull();
    expect(p.amountTotal).toBeNull();
    expect(p.side).toBe('buy');
  });

  it('is deterministic: newest genesis first, independent of input order', () => {
    const a = orderViewOf(kob, { ...single, covenantId: id('a0') }, { genesis: genesis(10) });
    const b = orderViewOf(kob, { ...single, covenantId: id('b0') }, { genesis: genesis(30) });
    const c = orderViewOf(kob, { ...single, covenantId: id('c0') }, { genesis: genesis(30) });
    const e = entry({ genesis: genesis(20) });
    const x = exit('a4', 'e1');
    const all = [a, b, c, e, x];
    const ref = groupPositions(all).map((p) => p.id);
    expect(ref).toEqual([id('b0'), id('c0'), id('e1'), id('a0')]);
    expect(groupPositions([...all].reverse()).map((p) => p.id)).toEqual(ref);
    expect(groupPositions([x, c, a, e, b]).map((p) => p.id)).toEqual(ref);
  });

  it('positionOf finds a position by entry or exit id', () => {
    const e = entry({ status: 'partial', filled_amount: '3000'});
    const x = exit('k1', 'e1');
    const p = positionOf([e, x], id('k1'));
    expect(p?.id).toBe(id('e1'));
    expect(positionOf([e, x], id('e1'))?.exits).toHaveLength(1);
    expect(positionOf([e, x], id('zz'))).toBeNull();
  });
});
