import { describe, expect, it } from 'vitest';
import type { StatsView } from '../../data/indexer-types';
import { cmpRational, ratToNumber } from '../../kob/pair';
import type { Candle } from './market-model';
import { kasPerToken, ratioCandles, ratioSeries, resolveHome, usdDp, usdStats, usdText, usdTokenOf, type TokenLeg } from './usd-model';

const MIN = 60_000;
const c = (t: number, o: bigint, h: bigint, l: bigint, cl: bigint, volume = 1n, quote?: bigint): Candle => ({
  t, o, h, l, c: cl, volume, quote: quote ?? cl * volume, trades: 1, filled: false,
});
// TUSD: 8 decimals, price basis 1e8 (one whole TUSD); a price of 25 KAS per TUSD = 2.5e9 sompi per basis -> 0.04 USD per KAS
const TUSD = (candles: Candle[]): TokenLeg => ({ candles, decimals: 8, basis: 100_000_000n });
// TBTC: 8 decimals, price basis 200 base units (0.000002 TBTC); 4 KAS per basis = 4e8 sompi -> 2,000,000 KAS per TBTC
const TBTC = (candles: Candle[]): TokenLeg => ({ candles, decimals: 8, basis: 200n });

describe('usd-model', () => {
  it('KAS per whole token from a candle price', () => {
    expect(ratToNumber(kasPerToken(2_500_000_000n, 8, 100_000_000n))).toBe(25);
    expect(ratToNumber(kasPerToken(400_000_000n, 8, 200n))).toBe(2_000_000);
  });

  it('a token in USD divides the two legs bucket by bucket, carrying a leg without trades', () => {
    const q = TUSD([c(0, 2_500_000_000n, 2_500_000_000n, 2_500_000_000n, 2_500_000_000n), c(2 * MIN, 2_000_000_000n, 2_000_000_000n, 2_000_000_000n, 2_000_000_000n)]);
    const b = TBTC([c(0, 400_000_000n, 400_000_000n, 400_000_000n, 400_000_000n, 200n), c(MIN, 440_000_000n, 440_000_000n, 440_000_000n, 440_000_000n, 200n)]);
    const rs = ratioCandles(b, q, { intervalMs: MIN, until: 2 * MIN });
    expect(rs.map((r) => r.t)).toEqual([0, MIN, 2 * MIN]);
    // 2,000,000 KAS per TBTC / 25 KAS per TUSD = 80,000 USD
    expect(ratToNumber(rs[0]!.c)).toBe(80_000);
    // minute 1: TBTC 2,200,000 KAS, TUSD carried at 25 -> 88,000 (time-aligned: the carried close of the same bucket)
    expect(ratToNumber(rs[1]!.c)).toBe(88_000);
    expect(rs[1]!.filled).toBe(false);
    // minute 2: TBTC carried at 2,200,000, TUSD 20 -> 110,000
    expect(ratToNumber(rs[2]!.c)).toBe(110_000);
    for (const r of rs) {
      expect(cmpRational(r.h, r.o)).toBeGreaterThanOrEqual(0);
      expect(cmpRational(r.h, r.c)).toBeGreaterThanOrEqual(0);
      expect(cmpRational(r.l, r.o)).toBeLessThanOrEqual(0);
      expect(cmpRational(r.l, r.c)).toBeLessThanOrEqual(0);
    }
  });

  it('leaves out buckets before a leg has a price and marks buckets where neither leg traded', () => {
    const q = TUSD([c(0, 2_500_000_000n, 2_500_000_000n, 2_500_000_000n, 2_500_000_000n)]);
    const b = TBTC([c(MIN, 400_000_000n, 400_000_000n, 400_000_000n, 400_000_000n, 200n)]);
    const rs = ratioCandles(b, q, { intervalMs: MIN, until: 3 * MIN });
    expect(rs.map((r) => [r.t, r.filled])).toEqual([[MIN, false], [2 * MIN, true], [3 * MIN, true]]);
    const s = ratioSeries(rs);
    expect(s.bars[0]).toMatchObject({ time: 60, close: 80_000, filled: false });
    expect(s.volumes[0]!.value).toBe(0.000002);
  });

  it('24 h stats in USD from both legs', () => {
    const st = (last: string, open: string, basis = '100000000'): StatsView => ({ token: 'x', price_basis: basis, ts: 0, last, open_24h: open } as unknown as StatsView);
    const btc = usdStats({ stats: st('440000000', '400000000', '200'), decimals: 8 }, { stats: st('2500000000', '2500000000'), decimals: 8 });
    expect(ratToNumber(btc.last!)).toBe(88_000);
    expect(btc.changeBps).toBe(1000);
    expect(usdStats({ stats: null, decimals: 8 }, { stats: st('1', '1'), decimals: 8 }).last).toBeNull();
  });

  it('formats USD prices with about 5 significant digits', () => {
    const btc = { num: 8_457_470n, den: 100n };
    expect(usdText(btc, usdDp(btc))).toBe('84,574.70');
    const kas = { num: 4242n, den: 100_000n };
    expect(usdText(kas, usdDp(kas))).toBe('0.042420');
  });

  it('the USD quote token and the landing screen', () => {
    const A = 'aa'.repeat(32);
    const B = 'bb'.repeat(32);
    const tokens = [
      { covenantId: A, tradable: true, status: 'listed' },
      { covenantId: B, tradable: true, status: 'listed' },
    ];
    expect(usdTokenOf({ [B]: 'USD' }, tokens)?.covenantId).toBe(B);
    expect(usdTokenOf({}, tokens)).toBeNull();
    // the landing is the USD token's own market page (KAS/<ticker> by default), a full market
    expect(resolveHome('auto', B, tokens)).toEqual({ name: 'token', covenantId: B });
    expect(resolveHome('auto', null, tokens)).toEqual({ name: 'token', covenantId: A });
    expect(resolveHome('auto', null, [])).toEqual({ name: 'market' });
    expect(resolveHome('list', B, tokens)).toEqual({ name: 'market' });
    // `usd:<id>` = that token against the USD token: the tradable pair page
    expect(resolveHome(`usd:${A}`, B, tokens)).toEqual({ name: 'pair', base: A, quote: B });
    expect(resolveHome(`usd:${B}`, B, tokens)).toEqual({ name: 'token', covenantId: B });
    expect(resolveHome('kas-usd', B, tokens)).toEqual({ name: 'token', covenantId: B });
    expect(resolveHome(`market:${B}`, null, tokens)).toEqual({ name: 'token', covenantId: B });
    expect(resolveHome('kas-usd', null, tokens)).toEqual({ name: 'token', covenantId: A });
  });
});
