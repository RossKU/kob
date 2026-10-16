import { describe, expect, it } from 'vitest';
import type { Clock, OrderPlan } from '../../kob/plan-types';
import type { BuiltTx } from '../../kob/types';
import { MARKET_ACTIVATION_DAA } from '../../kob/daa';
import { planOrder } from '../../kob/plan';
import { TOK, makeEnv } from '../../testing/fixtures';
import { AUCTION_START_MARGIN_DAA, auctionSubmitBy } from '../../wallet/sign';
import { CLOCK_REUSE_MS, REVIEW_CACHE_MS, REVIEW_REUSE_MS, liveClockAt, partCurrent, projectClock, reviewClock, sameTransaction, slippageWithin, withinTerms } from './review-check';

describe('partCurrent (is a part on hand the state at the click?)', () => {
  const click = 100_000;
  const live = { liveSince: 10_000, lastSubmitAt: 0 };
  it('a read started within REVIEW_REUSE_MS of the click is, whatever the feed', () => {
    expect(partCurrent({ at: click - REVIEW_REUSE_MS, busy: false, noticeAt: 0 }, click, { liveSince: null, lastSubmitAt: 0 })).toBe(true);
  });
  it('an older read only while the feed has delivered the notices without a gap since before it, at most REVIEW_CACHE_MS old', () => {
    const old = { at: click - 20_000, busy: false, noticeAt: 0 };
    expect(partCurrent(old, click, live)).toBe(true);
    expect(partCurrent(old, click, { ...live, liveSince: null })).toBe(false); // no feed / socket down
    expect(partCurrent(old, click, { ...live, liveSince: click - 10_000 })).toBe(false); // live only since after the read: a notice may be lost
    expect(partCurrent({ at: click - REVIEW_CACHE_MS - 1, busy: false, noticeAt: 0 }, click, live)).toBe(false);
  });
  it('never when a notice concerning it arrived after its read started, even before the debounced read begins, however recent the read', () => {
    expect(partCurrent({ at: click - 500, busy: false, noticeAt: click - 200 }, click, live)).toBe(false);
    expect(partCurrent({ at: click - 500, busy: false, noticeAt: click - 600 }, click, live)).toBe(true);
  });
  it('never when a newer read is running or queued (a notice came), when there is none, or when this page submitted since', () => {
    expect(partCurrent({ at: click - 1_000, busy: true, noticeAt: 0 }, click, live)).toBe(false);
    expect(partCurrent({ at: null, busy: false, noticeAt: 0 }, click, live)).toBe(false);
    expect(partCurrent({ at: click - 1_000, busy: false, noticeAt: 0 }, click, { ...live, lastSubmitAt: click - 500 })).toBe(false);
  });
});

const clock: Clock = { daa: 1_000_000n, unixSeconds: 1_790_000_000n, rateMilli: 10_000 };
const built = (inputs: string[], fee = 1000n): BuiltTx => ({ tx: { id: 'x', inputs: inputs.map((i) => ({ transactionId: i, index: 0 })), outputs: [], fee } }) as unknown as BuiltTx;

describe('projectClock', () => {
  it('moves both halves of the pair by the whole seconds elapsed at the measured rate; the DAA never ahead', () => {
    expect(projectClock(clock, 2_999)).toEqual({ ...clock, unixSeconds: clock.unixSeconds + 2n, daa: clock.daa + 20n });
    expect(projectClock({ ...clock, rateMilli: 9_870 }, 10_000)).toMatchObject({ unixSeconds: clock.unixSeconds + 10n, daa: clock.daa + 98n });
    expect(projectClock(clock, -5)).toEqual(clock);
  });
});

describe('reviewClock', () => {
  it('takes the node clock when it answers in time', async () => {
    const fresh = { ...clock, daa: clock.daa + 7n };
    expect(await reviewClock(async () => fresh, clock, 0, { waitMs: 50 })).toEqual({ clock: fresh, projected: false });
  });

  it('a slow or failing node: the clock on hand projected to now, never a wait longer than waitMs', async () => {
    const slow = () => new Promise<Clock>((r) => setTimeout(() => r(clock), 1_000));
    const t0 = Date.now();
    const r = await reviewClock(slow, clock, 10_000, { waitMs: 30, now: () => 13_500 });
    expect(Date.now() - t0).toBeLessThan(500);
    expect(r).toEqual({ clock: projectClock(clock, 3_500), projected: true });
    const failing = async (): Promise<Clock> => {
      throw new Error('node down');
    };
    expect((await reviewClock(failing, clock, 10_000, { waitMs: 30, now: () => 11_000 })).projected).toBe(true);
  });
});

describe('the terms the ticket showed', () => {
  const total = (side: 'buy' | 'sell', allInTotal: bigint | null) => ({ disclosure: { side, allInTotal } }) as unknown as OrderPlan;

  it('a plan at the click keeps them while a buy pays at most and a sell receives at least the total shown, up to a rounding', () => {
    expect(withinTerms(total('buy', 2_000_000_000n), total('buy', 1_950_000_000n))).toBe(true);
    expect(withinTerms(total('buy', 2_000_000_000n), total('buy', 2_000_200_001n))).toBe(true); // 1 bps + 1 sompi
    expect(withinTerms(total('buy', 2_000_000_000n), total('buy', 2_000_200_002n))).toBe(false);
    expect(withinTerms(total('sell', 2_000_000_000n), total('sell', 2_050_000_000n))).toBe(true);
    expect(withinTerms(total('sell', 2_000_000_000n), total('sell', 1_990_000_000n))).toBe(false);
    // nothing shown, or no total to compare: the plan at the click is the order
    expect(withinTerms(null, total('buy', 9n))).toBe(true);
    expect(withinTerms(total('buy', null), total('buy', 9n))).toBe(true);
  });

  it('the slippage that ends an auction at the worst price shown, none when the touch is already beyond it', () => {
    expect(slippageWithin('buy', 250_000_000n, 257_500_000n)).toBe(300n);
    expect(slippageWithin('buy', 252_000_000n, 257_500_000n)).toBe(218n);
    expect(slippageWithin('buy', 257_500_000n, 257_500_000n)).toBeNull();
    expect(slippageWithin('sell', 245_000_000n, 237_650_000n)).toBe(300n);
    expect(slippageWithin('sell', 237_000_000n, 237_650_000n)).toBeNull();
  });
});

describe('sameTransaction', () => {
  it('sameTransaction compares the unsigned transaction byte for byte', () => {
    expect(sameTransaction(built(['a']), built(['a']))).toBe(true);
    expect(sameTransaction(built(['a']), built(['b']))).toBe(false);
  });
});

describe('the clock of a click (an auction starts from it)', () => {
  // TN10 2026-10-17: a market order planned at Review and signed 17 s later started its auction 175 DAA in the past and filled near its worst price
  const market = { type: 'market', side: 'buy', amount: 5n * TOK } as never;
  const startOf = (p: OrderPlan): bigint => BigInt((p.states[0]!.state as { activeFrom: string }).activeFrom);

  it('a stale clock on hand never ends up in activeFrom: the node clock read at the click does, and the send guard follows it', async () => {
    const stale = clock;
    const node: Clock = { ...stale, daa: stale.daa + 175n, unixSeconds: stale.unixSeconds + 17n };
    const read = await reviewClock(async () => node, stale, 0, { waitMs: 100, now: () => 17_500 });
    const plan = planOrder(makeEnv({ clock: read.clock }), market);
    expect(plan.ok).toBe(true);
    expect(startOf(plan)).toBe(node.daa + MARKET_ACTIVATION_DAA);
    expect(auctionSubmitBy(plan.states, read.clock.daa)).toBe(node.daa + MARKET_ACTIVATION_DAA + AUCTION_START_MARGIN_DAA);
    // a node too slow to answer: the clock on hand projected to the click, never the clock on hand itself
    const slow = await reviewClock(() => new Promise<Clock>(() => undefined), stale, 0, { waitMs: 10, now: () => 17_500 });
    expect(startOf(planOrder(makeEnv({ clock: slow.clock }), market))).toBe(stale.daa + 170n + MARKET_ACTIVATION_DAA);
  });

  it('a resting limit order has no auction start to guard', () => {
    const limit = planOrder(makeEnv(), { type: 'limit', side: 'sell', amount: 5n * TOK, price: 900_000_000n } as never);
    expect(limit.ok).toBe(true);
    expect(auctionSubmitBy(limit.states, clock.daa)).toBeNull();
  });
});

describe('the background clock of the ticket', () => {
  it('is projected to the click while it is at most CLOCK_REUSE_MS old, else the click reads the node', () => {
    const live = { clock, at: 10_000 };
    expect(liveClockAt(live, 10_000)).toEqual(clock);
    expect(liveClockAt(live, 12_500)).toEqual(projectClock(clock, 2_500));
    expect(liveClockAt(live, 10_000 + CLOCK_REUSE_MS + 1)).toBeNull();
    expect(liveClockAt(null, 10_000)).toBeNull();
    // a clock "from the future" (the page's clock went back) is not trusted
    expect(liveClockAt(live, 9_000)).toBeNull();
  });
});
