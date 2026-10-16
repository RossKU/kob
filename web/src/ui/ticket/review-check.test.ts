import { describe, expect, it } from 'vitest';
import type { Clock, OrderPlan, PlanIssue } from '../../kob/plan-types';
import type { BuiltTx } from '../../kob/types';
import { REVIEW_CACHE_MS, REVIEW_REUSE_MS, partCurrent, projectClock, recheckVerdict, reviewClock, sameTransaction } from './review-check';

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
const plan = (b: BuiltTx | null, issues: PlanIssue[] = []): OrderPlan => ({ ok: true, built: b, issues }) as unknown as OrderPlan;
const issue = (code: string, severity: PlanIssue['severity']): PlanIssue => ({ code, severity, message: code });

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

describe('recheckVerdict', () => {
  it('holds only for the same transaction with no error and no warning the screen did not show', () => {
    const shown = plan(built(['a', 'b']), [issue('W1', 'warning')]);
    expect(recheckVerdict(shown, plan(built(['a', 'b']), [issue('W1', 'warning')]))).toEqual({ ok: true });
    // another coin, another fee: another transaction
    expect(recheckVerdict(shown, plan(built(['a', 'c'])))).toEqual({ ok: false, issues: [] });
    expect(recheckVerdict(shown, plan(built(['a', 'b'], 2000n)))).toEqual({ ok: false, issues: [] });
    // the wallet's own resting order now crosses it (read with the holdings): an error
    const self = issue('SELF_TRADE', 'error');
    expect(recheckVerdict(shown, plan(built(['a', 'b']), [self]))).toEqual({ ok: false, issues: [self] });
    // a warning the screen did not show
    const w2 = issue('W2', 'warning');
    expect(recheckVerdict(shown, plan(built(['a', 'b']), [w2]))).toEqual({ ok: false, issues: [w2] });
    expect(recheckVerdict(shown, plan(null))).toEqual({ ok: false, issues: [] });
  });

  it('sameTransaction compares the unsigned transaction byte for byte', () => {
    expect(sameTransaction(built(['a']), built(['a']))).toBe(true);
    expect(sameTransaction(built(['a']), built(['b']))).toBe(false);
  });
});
