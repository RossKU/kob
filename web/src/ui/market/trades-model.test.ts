import { describe, expect, it } from 'vitest';
import type { EventView } from '../../data/indexer-types';
import { tradeRows, vwap } from './trades-model';

const TOKEN = 'e5'.repeat(32);
const ev = (o: Partial<EventView>): EventView => ({
  id: 1, covenant_id: 'aa'.repeat(32), block_seq: 1, daa: 100, ts: 1_700_000_000_000, txid: 'bb'.repeat(32), tx_pos: 0, kind: 'fill', token: TOKEN,
  side: 1, amount: '200', price: '25000', payout: null, closes: false, detail: null, confirmations: 5, settled: true, ...o,
});

describe('tradeRows', () => {
  it('maps the filled order side to the taker side, keeps the price per whole token and values amount x price / scale', () => {
    // scale 100: 200 base units = 2 whole tokens at 25_000 sompi each
    const rows = tradeRows([ev({ id: 1, side: 1 }), ev({ id: 2, side: 2 })], 100n);
    const buy = rows.find((r) => r.id === 1)!;
    const sell = rows.find((r) => r.id === 2)!;
    expect(buy.takerSide).toBe('buy'); // an ask was filled: the taker bought
    expect(sell.takerSide).toBe('sell');
    expect(buy.price).toBe(25_000n);
    expect(buy.amount).toBe(200n);
    expect(buy.value).toBe(50_000n);
    // the value rounds down: 3 base units at 25_000 per 100 units = 750 sompi; 1 unit at 99 per 100 = 0
    expect(tradeRows([ev({ amount: '3' })], 100n)[0]!.value).toBe(750n);
    expect(tradeRows([ev({ amount: '1', price: '99' })], 100n)[0]!.value).toBe(0n);
  });

  it('keeps fills only, newest first, and tolerates missing data', () => {
    const rows = tradeRows(
      [ev({ id: 1, daa: 10 }), ev({ id: 2, daa: 30, kind: 'cancel' }), ev({ id: 3, daa: 20, price: null, ts: 0 }), ev({ id: 4, daa: 20, amount: null })],
      1n,
    );
    expect(rows.map((r) => r.id)).toEqual([4, 3, 1]); // same DAA: higher id first; the cancel is gone
    const r3 = rows.find((r) => r.id === 3)!;
    expect(r3.price).toBeNull();
    expect(r3.value).toBeNull();
    expect(r3.timeMs).toBeNull();
    expect(rows.find((r) => r.id === 4)!.value).toBeNull();
  });

  it('limits the list', () => {
    const many = Array.from({ length: 50 }, (_, i) => ev({ id: i + 1, daa: i }));
    expect(tradeRows(many, 1n, 10)).toHaveLength(10);
  });
});

describe('vwap', () => {
  it('weights by amount and skips incomplete rows', () => {
    const rows = tradeRows([ev({ id: 1, price: '100', amount: '1' }), ev({ id: 2, price: '400', amount: '3' }), ev({ id: 3, price: null })], 1n);
    expect(vwap(rows)).toBe(325n); // (100 + 1200) / 4
    expect(vwap([])).toBeNull();
  });
});
