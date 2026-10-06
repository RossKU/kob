import { describe, expect, it } from 'vitest';
import { level, makeEnv } from '../../testing/fixtures';
import { IssueLog } from './cond-common';
import { askWorst, bidWorst, resolveLegs, slipForLimit, stopWorstPrice } from './cond-legs';

const codesOf = (log: IssueLog): string[] => log.issues.map((i) => i.code);

describe('slipForLimit (matcher.md 10.6: largest slipBps not beyond the limit)', () => {
  it('converts a sell limit exactly and is tight', () => {
    // stop 230.00 KAS: floor(stop * bps / 10^4) = 23_000 sompi per bps
    expect(slipForLimit('sell', 230_000_000n, 223_100_000n)).toBe(300n);
    expect(slipForLimit('sell', 230_000_000n, 223_100_001n)).toBe(299n);
    expect(slipForLimit('sell', 230_000_000n, 230_000_000n)).toBe(0n);
    expect(slipForLimit('buy', 230_000_000n, 236_900_000n)).toBe(300n);
    expect(slipForLimit('buy', 230_000_000n, 236_899_999n)).toBe(299n);
    expect(slipForLimit('buy', 230_000_000n, 230_000_000n)).toBe(0n);
  });

  it('caps at 10 000 bps', () => {
    // a sell band reaches zero only at 10 000 bps: the largest slip keeping the worst price above the limit is 9 999
    expect(slipForLimit('sell', 230_000_000n, 1n)).toBe(9_999n);
    expect(slipForLimit('buy', 230_000_000n, 900_000_000n)).toBe(10_000n);
  });

  it('multiplies first like the covenants: low stops keep a band and never pass the limit', () => {
    // stop 5 000 sompi per token: the band is stop*bps/10^4 (5 per 10 bps). Divide-first gave floor(5000/10^4)=0, i.e. no band at all.
    expect(stopWorstPrice('sell', 5_000n, 300n)).toBe(4_850n);
    expect(stopWorstPrice('buy', 5_000n, 300n)).toBe(5_150n);
    expect(stopWorstPrice('sell', 9_999n, 300n)).toBe(9_999n - 299n);
    expect(stopWorstPrice('sell', 1n, 9_999n)).toBe(1n);
    expect(stopWorstPrice('sell', 1n, 10_000n)).toBe(0n);
    expect(stopWorstPrice('buy', 1n, 10_000n)).toBe(2n);
    // a stop-limit at a low price: the limit 4 900 of a 5 000 stop allows 201 bps (worst price 4 900), not the 10 000 the old code returned
    expect(slipForLimit('sell', 5_000n, 4_900n)).toBe(201n);
    expect(slipForLimit('sell', 5_000n, 4_899n)).toBe(203n);
    expect(stopWorstPrice('sell', 5_000n, slipForLimit('sell', 5_000n, 4_899n))).toBeGreaterThanOrEqual(4_899n);
    expect(slipForLimit('buy', 5_000n, 5_100n)).toBe(201n);
    expect(slipForLimit('sell', 9_999n, 1n)).toBe(9_999n);
    expect(slipForLimit('sell', 5_000n, 5_000n)).toBe(0n);
    // dist 0 for a 1-sompi stop
    expect(slipForLimit('sell', 1n, 1n)).toBe(0n);
    expect(slipForLimit('buy', 1n, 1n)).toBe(0n);
    expect(slipForLimit('buy', 1n, 2n)).toBe(10_000n);
  });

  it('exhaustive for low stops: the result is the largest bps whose worst price honours the limit (both sides)', () => {
    for (let stop = 1n; stop <= 40n; stop++) {
      for (let limit = 1n; limit <= 90n; limit++) {
        if (limit <= stop) {
          const s = slipForLimit('sell', stop, limit);
          expect(stopWorstPrice('sell', stop, s)).toBeGreaterThanOrEqual(limit);
          if (s < 10_000n && stop > limit) expect(stopWorstPrice('sell', stop, s + 1n)).toBeLessThan(limit);
        }
        if (limit >= stop) {
          const t = slipForLimit('buy', stop, limit);
          expect(stopWorstPrice('buy', stop, t)).toBeLessThanOrEqual(limit);
          if (t < 10_000n && limit > stop) expect(stopWorstPrice('buy', stop, t + 1n)).toBeGreaterThan(limit);
        }
      }
    }
  });

  it('property: the worst price honours the limit and one more bps would not (both sides, many random pairs)', () => {
    let seed = 12345n;
    const next = (): bigint => {
      seed = (seed * 6364136223846793005n + 1442695040888963407n) % (1n << 63n);
      return seed >> 16n;
    };
    for (let i = 0; i < 400; i++) {
      const stop = 1n + (next() % (i % 2 === 0 ? 30_000n : 5_000_000_000n));
      const dist = next() % (stop / 2n + 1n);
      const sellLimit = stop - dist > 0n ? stop - dist : 1n;
      const s = slipForLimit('sell', stop, sellLimit);
      expect(stopWorstPrice('sell', stop, s)).toBeGreaterThanOrEqual(sellLimit);
      if (s < 10_000n && sellLimit < stop) expect(stopWorstPrice('sell', stop, s + 1n)).toBeLessThan(sellLimit);
      const buyLimit = stop + dist;
      const t = slipForLimit('buy', stop, buyLimit);
      expect(stopWorstPrice('buy', stop, t)).toBeLessThanOrEqual(buyLimit);
      if (t < 10_000n && buyLimit > stop) expect(stopWorstPrice('buy', stop, t + 1n)).toBeGreaterThan(buyLimit);
    }
  });
});

describe('resolveLegs', () => {
  it('applies the wallet defaults to a stop leg (sell)', () => {
    const env = makeEnv();
    const log = new IssueLog();
    const l = resolveLegs(env, log, 'sell', { stop: 230_000_000n })!;
    expect(log.issues).toEqual([]);
    expect(l.slipBps).toBe(300n);
    expect(l.bandDaa).toBe(300n);
    expect(l.minRestDaa).toBe(50n);
    // without a minimum fill the trigger threshold is kob-wasm defaultMinTouch(1) = 1 base unit
    expect(l.minTouch).toBe(1n);
    expect(l.keeperTip).toBe(BigInt(env.token.keeperTip));
    expect(l.keeperTip).toBe(2_100_000n);
    expect(l.tpPrice).toBe(0n);
    expect(l.stopWorst).toBe(223_100_000n);
    expect(askWorst(l)).toBe(223_100_000n);
    expect(l.keeper).toMatchObject({ expectedUpdates: 1, reserve: 2_100_000n, fundedFrom: 'carrier' });
  });

  it('a take-profit only leg has no keeper, band or slippage', () => {
    const env = makeEnv();
    const log = new IssueLog();
    const l = resolveLegs(env, log, 'sell', { tp: 300_000_000n })!;
    expect(l).toMatchObject({ tpPrice: 300_000_000n, stopPrice: 0n, bandDaa: 0n, keeperTip: 0n, trailStep: 0n });
    expect(l.keeper).toBeNull();
    expect(askWorst(l)).toBe(300_000_000n);
  });

  it('the trigger threshold defaults to the order minimum fill whatever the book (no depth rule); the user sets it per order', () => {
    const book = { asks: [level(250_000_000n, 250n)], bids: [level(245_000_000n, 1_001n)] };
    const env = makeEnv({ book });
    const sell = resolveLegs(env, new IssueLog(), 'sell', { stop: 230_000_000n }, '', 4_348n)!;
    expect(sell.minTouch).toBe(4_348n);
    expect(sell.minTouch).toBe(env.kob.defaultMinTouch(4_348n));
    expect(sell.summary.minTouch).toBe(4_348n);
    const buy = resolveLegs(env, new IssueLog(), 'buy', { stop: 260_000_000n }, '', 1_000n)!;
    expect(buy.minTouch).toBe(1_000n);
    const empty = resolveLegs(makeEnv({ book: { asks: [], bids: [] } }), new IssueLog(), 'sell', { stop: 230_000_000n }, '', 7n)!;
    expect(empty.minTouch).toBe(7n);
    const explicit = resolveLegs(env, new IssueLog(), 'sell', { stop: 230_000_000n, minTouch: 7_000n }, '', 4_348n)!;
    expect(explicit.minTouch).toBe(7_000n);
  });

  it('stop-limit: slipBps from the limit; a limit equal to the stop is a single price (no auction)', () => {
    const env = makeEnv();
    const l = resolveLegs(env, new IssueLog(), 'sell', { stop: 230_000_000n, limit: 225_000_000n })!;
    expect(l.slipBps).toBe(217n); // floor(5_000_000 / 23_000)
    expect(l.stopWorst).toBeGreaterThanOrEqual(225_000_000n);
    expect(l.summary.limit).toBe(225_000_000n);
    const flat = resolveLegs(env, new IssueLog(), 'sell', { stop: 230_000_000n, limit: 230_000_000n })!;
    expect(flat.slipBps).toBe(0n);
    expect(flat.bandDaa).toBe(0n);
  });

  it('buy stop-limit: worst price is the ceiling', () => {
    const env = makeEnv();
    const l = resolveLegs(env, new IssueLog(), 'buy', { stop: 260_000_000n, limit: 268_000_000n })!;
    expect(l.slipBps).toBe(307n); // floor(8_000_000 / 26_000)
    expect(l.stopWorst).toBe(260_000_000n + 26_000n * 307n);
    expect(bidWorst(l)).toBe(l.stopWorst);
  });

  it('bidWorst is the larger of the limit leg and the stop ceiling', () => {
    const env = makeEnv();
    const l = resolveLegs(env, new IssueLog(), 'buy', { tp: 200_000_000n, stop: 300_000_000n })!;
    expect(bidWorst(l)).toBe(309_000_000n);
    const tpOnly = resolveLegs(env, new IssueLog(), 'buy', { tp: 200_000_000n })!;
    expect(bidWorst(tpOnly)).toBe(200_000_000n);
  });

  it('refuses legs in the wrong order, per side', () => {
    const env = makeEnv();
    const sell = new IssueLog();
    expect(resolveLegs(env, sell, 'sell', { tp: 200_000_000n, stop: 230_000_000n })).toBeNull();
    expect(codesOf(sell)).toEqual(['COND_TP_STOP_ORDER']);
    expect(sell.issues[0]!.params).toMatchObject({ direction: 'above' });
    const buy = new IssueLog();
    expect(resolveLegs(env, buy, 'buy', { tp: 300_000_000n, stop: 260_000_000n })).toBeNull();
    expect(codesOf(buy)).toEqual(['COND_TP_STOP_ORDER']);
    expect(buy.issues[0]!.params).toMatchObject({ direction: 'below' });
    // equal prices are not an order either
    const eq = new IssueLog();
    expect(resolveLegs(env, eq, 'sell', { tp: 230_000_000n, stop: 230_000_000n })).toBeNull();
  });

  it('refuses a stop-limit price beyond the stop, and a missing leg', () => {
    const env = makeEnv();
    const a = new IssueLog();
    expect(resolveLegs(env, a, 'sell', { stop: 230_000_000n, limit: 231_000_000n })).toBeNull();
    expect(codesOf(a)).toEqual(['COND_STOP_LIMIT_BEYOND_STOP']);
    const b = new IssueLog();
    expect(resolveLegs(env, b, 'buy', { stop: 260_000_000n, limit: 259_000_000n })).toBeNull();
    expect(codesOf(b)).toEqual(['COND_STOP_LIMIT_BEYOND_STOP']);
    const c = new IssueLog();
    expect(resolveLegs(env, c, 'sell', {})).toBeNull();
    expect(codesOf(c)).toEqual(['COND_LEGS_MISSING']);
  });

  it('refuses off-tick prices with the nearest valid ones, and a non-positive price', () => {
    const env = makeEnv(); // tick 100 sompi per token
    const a = new IssueLog();
    expect(resolveLegs(env, a, 'sell', { stop: 230_000_050n })).toBeNull();
    expect(a.issues[0]).toMatchObject({ code: 'PRICE_NOT_ON_TICK', field: 'stop', params: { below: 230_000_000n, above: 230_000_100n } });
    const b = new IssueLog();
    expect(resolveLegs(env, b, 'sell', { tp: 0n })).toBeNull();
    expect(codesOf(b)).toEqual(['PRICE_NOT_POSITIVE']);
  });

  it('validates slippage, band, trigger and keeper parameters', () => {
    const env = makeEnv();
    for (const slipBps of [-1, 10_001, 12.5]) {
      const log = new IssueLog();
      expect(resolveLegs(env, log, 'sell', { stop: 230_000_000n, slipBps })).toBeNull();
      expect(codesOf(log)).toEqual(['COND_SLIP_INVALID']);
    }
    const cases: [Parameters<typeof resolveLegs>[3], string][] = [
      [{ stop: 230_000_000n, bandDaa: -1n }, 'COND_BAND_INVALID'],
      [{ stop: 230_000_000n, minRestDaa: -1n }, 'COND_MIN_REST_INVALID'],
      [{ stop: 230_000_000n, minTouch: 0n }, 'COND_MIN_TOUCH_INVALID'],
      [{ stop: 230_000_000n, keeperTip: -1n }, 'COND_KEEPER_TIP_INVALID'],
    ];
    for (const [input, code] of cases) {
      const log = new IssueLog();
      expect(resolveLegs(env, log, 'sell', input), code).toBeNull();
      expect(codesOf(log)).toEqual([code]);
    }
    // a band that reaches price zero is refused (only possible for absurd slippage on a tiny stop)
    const log = new IssueLog();
    expect(resolveLegs(env, log, 'sell', { stop: 20_000n, slipBps: 10_000 })).toBeNull();
    expect(codesOf(log)).toEqual(['COND_WORST_PRICE_INVALID']);
  });

  it('trailing: step, gap, wait, expected updates', () => {
    const env = makeEnv();
    const l = resolveLegs(env, new IssueLog(), 'sell', { stop: 230_000_000n, trail: { step: 1_000_000n, gap: 5_000_000n } })!;
    expect(l).toMatchObject({ trailStep: 1_000_000n, trailGap: 5_000_000n, trailWait: 6_000n });
    // 20 updates pre-funded plus the arm that follows a trail
    expect(l.keeper).toMatchObject({ expectedUpdates: 21, reserve: 21n * 2_100_000n });
    expect(l.trail).toMatchObject({ step: 1_000_000n, gap: 5_000_000n, waitDaa: 6_000n, waitSeconds: 600n, maxUpdatesPerDay: 144n, expectedUpdates: 20 });
    const custom = resolveLegs(env, new IssueLog(), 'buy', { stop: 260_000_000n, trail: { step: 200n, gap: 0n, wait: 600n, expectedUpdates: 0 } })!;
    expect(custom.trail).toMatchObject({ waitSeconds: 60n, maxUpdatesPerDay: 1_440n, expectedUpdates: 0 });
    expect(custom.keeper!.expectedUpdates).toBe(1);
  });

  it('trailing: refuses bad steps, gaps, waits, update counts and a trail without a stop', () => {
    const env = makeEnv();
    const bad: [Parameters<typeof resolveLegs>[3], string][] = [
      [{ stop: 230_000_000n, trail: { step: 0n, gap: 0n } }, 'COND_TRAIL_STEP_INVALID'],
      [{ stop: 230_000_000n, trail: { step: 1_000_050n, gap: 0n } }, 'PRICE_NOT_ON_TICK'],
      [{ stop: 230_000_000n, trail: { step: 1_000_000n, gap: -1n } }, 'COND_TRAIL_GAP_INVALID'],
      [{ stop: 230_000_000n, trail: { step: 1_000_000n, gap: 0n, wait: 599n } }, 'COND_TRAIL_WAIT_TOO_SHORT'],
      [{ stop: 230_000_000n, trail: { step: 1_000_000n, gap: 0n, expectedUpdates: 1_001 } }, 'COND_TRAIL_UPDATES_INVALID'],
      [{ stop: 230_000_000n, trail: { step: 1_000_000n, gap: 0n, expectedUpdates: 1.5 } }, 'COND_TRAIL_UPDATES_INVALID'],
      [{ tp: 300_000_000n, trail: { step: 1_000_000n, gap: 0n } }, 'COND_TRAIL_NEEDS_STOP'],
    ];
    for (const [input, code] of bad) {
      const log = new IssueLog();
      expect(resolveLegs(env, log, 'sell', input), code).toBeNull();
      expect(codesOf(log)).toEqual([code]);
    }
  });

  it('prices are the state prices (sompi per whole token): no conversion, any price on the tick', () => {
    const base = makeEnv();
    const env = { ...base, token: { ...base.token, tick: 1n } };
    const l = resolveLegs(env, new IssueLog(), 'sell', { stop: 230_000_001n, tp: 300_000_007n, limit: 225_000_003n })!;
    expect(l.stopPrice).toBe(230_000_001n);
    expect(l.tpPrice).toBe(300_000_007n);
    expect(l.summary).toMatchObject({ stop: 230_000_001n, takeProfit: 300_000_007n, limit: 225_000_003n, stopWorst: l.stopWorst });
    // the band never reaches below the limit
    expect(l.stopWorst >= 225_000_003n).toBe(true);
  });
});
