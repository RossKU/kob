// A curve sell order in the orders list: one row, type "curve", its price the current level's, its
// facts for the sub-line and the details. An unbound curve is listed under the token the indexer
// names. What the rest pays if it sells.
import { describe, expect, it } from 'vitest';
import { loadKobNode } from '../../kob/wasm.node';
import { orderViewOf, placeGolden } from '../../testing/chain-fixtures';
import { buildEntries, describeEntry } from './orders-model';
import { restProceeds } from './CurveDetails';
import type { CurveFacts } from '../../kob/order-facts';
import type { OrderState } from '../../kob/types';

const kob = loadKobNode();

describe('a curve row', () => {
  const snap = placeGolden(kob, 'create.curve').snapshots[0]!;
  const st = snap.order.state as Extract<OrderState, { kind: 'KobCurveAsk' }>;
  const ctx = { kob, clock: null };

  it('is one row of type curve, priced at its current level, with no expiry', () => {
    const view = orderViewOf(kob, snap, { expiry_daa: null, price: st.state.table[0]! });
    const [e] = buildEntries([view], []);
    const row = describeEntry(e!, ctx);
    expect(row.typeKey).toBe('curve');
    expect(row.side).toBe('sell');
    expect(row.price).toBe(BigInt(st.state.table[0]!));
    expect(row.curve).toMatchObject({ level: 0, levels: st.state.table.length, unbound: false });
    expect(row.expiry.kind).toBe('none');
    expect(row.canCancel).toBe(true);
    expect(row.canAmend).toBe(true);
  });

  it('moves on with its state and lists an unbound curve under the indexer\'s token', () => {
    const size = BigInt(st.state.levelSize);
    const left = BigInt(st.state.total) - 3n * size - 1n;
    const moved = { ...st, state: { ...st.state, amountLeft: left.toString(), tokenCovId: '0'.repeat(64) } };
    const view = orderViewOf(kob, snap, { state: moved, token: '77'.repeat(32), expiry_daa: null });
    const row = describeEntry(buildEntries([view], [])[0]!, ctx);
    expect(row.curve).toMatchObject({ level: 3, unbound: true });
    expect(row.price).toBe(BigInt(st.state.table[3]!));
    expect(row.token).toBe('77'.repeat(32));
  });
});

describe('what the rest pays', () => {
  const c: CurveFacts = { level: 1, levels: 4, levelSize: 30n, total: 100n, table: [10n, 20n, 30n, 40n], priceNow: 20n, unbound: false };
  it('sums the open part of every level, rounded up per level', () => {
    // 45 left: 5 of level 1, 30 of level 2, 10 of level 3 (scale 10)
    expect(restProceeds(c, 45n, 10n)).toBe(10n + 90n + 40n);
    expect(restProceeds(c, 100n, 10n)).toBe(30n + 60n + 90n + 40n);
    expect(restProceeds(c, 1n, 10n)).toBe(4n);
  });
});

describe('the details graph', () => {
  it('lifts the steps onto a floor under the lowest price so a gentle rise shows; a flat or steep table stays', async () => {
    const { graphSteps } = await import('./CurveDetails');
    const st = (prices: bigint[]) => prices.map((price, level) => ({ level, from: level / prices.length, to: (level + 1) / prices.length, price, sold: false }));
    expect(graphSteps(st([100n, 104n])).map((s) => s.price)).toEqual([1n, 5n]);
    expect(graphSteps(st([50n, 50n])).map((s) => s.price)).toEqual([50n, 50n]);
    expect(graphSteps(st([10n, 100n])).map((s) => s.price)).toEqual([10n, 100n]);
    expect(graphSteps([])).toEqual([]);
  });
});
