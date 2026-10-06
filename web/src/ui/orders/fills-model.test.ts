import { describe, expect, it } from 'vitest';
import type { EventView } from '../../data/indexer-types';
import { fillHistory, fillsCsv, fillsFileName } from './fills-model';

const ID = 'ab'.repeat(32);
const TOKEN = 100_000_000n; // one whole token of 8 decimals (scale 1e8)
// a fill of 2 whole tokens at 2.5 KAS per token
const ev = (id: number, over: Partial<EventView> = {}): EventView => ({
  id, covenant_id: ID, block_seq: id, daa: 1000 + id, ts: 1_790_000_000_000 + id * 1000, txid: id.toString(16).padStart(64, '0'), tx_pos: 0, kind: 'fill',
  token: 'cd'.repeat(32), side: 1, amount: String(2n * TOKEN), price: '250000000', payout: '499000000', closes: false, detail: null, confirmations: 10, settled: true, ...over,
});

describe('fillHistory (R-8)', () => {
  it('per-fill rows oldest first, amount-weighted average price per token, duplicates and non-fills skipped', () => {
    const h = fillHistory([ev(3, { amount: String(TOKEN), price: '260000000', payout: '259000000' }), ev(1), ev(1), ev(2, { kind: 'arm', amount: null }), ev(4, { kind: 'cancel', amount: '0' })], TOKEN);
    expect(h.rows.map((r) => r.txid.slice(-1))).toEqual(['1', '3']);
    expect(h.amount).toBe(3n * TOKEN);
    // (2 x 2.5 + 1 x 2.6) / 3 = 2.5333... KAS per token, rounded down
    expect(h.avgPrice).toBe(253_333_333n);
    expect(h.payout).toBe(758_000_000n);
    expect(h.scale).toBe(TOKEN);
  });

  it('amounts are any base units: a fill of 1 base unit weighs 1 / 1e8 of a whole token', () => {
    const h = fillHistory([ev(1, { amount: '1', price: '100000000' }), ev(2, { amount: String(TOKEN), price: '200000000' })], TOKEN);
    expect(h.amount).toBe(TOKEN + 1n);
    // (1 x 1 + 1e8 x 2) / (1e8 + 1) KAS, rounded down to the sompi
    expect(h.avgPrice).toBe((100_000_000n + TOKEN * 200_000_000n) / (TOKEN + 1n));
  });

  it('no priced fill: no average; an empty history is 0 base units', () => {
    expect(fillHistory([ev(1, { price: null })]).avgPrice).toBeNull();
    expect(fillHistory([]).amount).toBe(0n);
  });
});

describe('fillsCsv', () => {
  it('one line per fill, amounts in token units, exact KAS, UTC times, formula-like cells quoted', () => {
    const csv = fillsCsv([{ info: { ticker: '=EVIL', side: 'sell', type: 'limit', decimals: 8 }, history: fillHistory([ev(1)], TOKEN) }]);
    const [head, line] = csv.trim().split('\r\n');
    expect(head).toBe('time_utc,order_id,token,side,type,amount,price_kas_per_token,value_kas,payout_kas,txid,settled');
    expect(line).toBe(`2026-09-21T14:13:21.000Z,${ID},"=EVIL",sell,limit,2,2.50000000,5.00000000,4.99000000,${'1'.padStart(64, '0')},yes`);
  });

  it('without the decimals: base units and the state price in KAS; the value rounds down', () => {
    const csv = fillsCsv([{ info: { ticker: 'X', side: 'buy', type: 'limit' }, history: fillHistory([ev(1, { amount: '3', price: '50' })], 100n) }]);
    const line = csv.trim().split('\r\n')[1]!;
    // 3 base units at 50 sompi per 100 units = 1.5 sompi -> 1 sompi
    expect(line.split(',').slice(5, 8)).toEqual(['3', '0.00000050', '0.00000001']);
  });

  it('file name', () => {
    expect(fillsFileName('testnet-10', Date.UTC(2026, 9, 2))).toBe('kob-fills-testnet-10-20261002.csv');
  });
});
