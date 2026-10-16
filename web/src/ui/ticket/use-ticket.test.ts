import { describe, expect, it } from 'vitest';
import { Serialized } from './use-ticket';

const deferred = <T>() => {
  let resolve!: (v: T) => void;
  let reject!: (e: unknown) => void;
  const promise = new Promise<T>((res, rej) => ((resolve = res), (reject = rej)));
  return { promise, resolve, reject };
};

describe('Serialized', () => {
  it('runs one load at a time and folds every call made during it into ONE follow-up run', async () => {
    const loads: ReturnType<typeof deferred<number>>[] = [];
    const s = new Serialized(() => {
      const d = deferred<number>();
      loads.push(d);
      return d.promise;
    });
    const a = s.run();
    const b = s.run();
    const c = s.run();
    expect(loads).toHaveLength(1);
    expect(b).toBe(c);
    loads[0]!.resolve(1);
    expect(await a).toBe(1);
    await Promise.resolve();
    await Promise.resolve();
    expect(loads).toHaveLength(2); // the follow-up started only after the first ended
    loads[1]!.resolve(2);
    expect(await b).toBe(2);
  });

  it('a feed faster than the load still delivers every run (no starvation)', async () => {
    let n = 0;
    const done: number[] = [];
    const s = new Serialized(async () => {
      const k = ++n;
      await new Promise((r) => setTimeout(r, 5));
      done.push(k);
      return k;
    });
    const calls: Promise<number>[] = [];
    for (let i = 0; i < 20; i++) {
      calls.push(s.run());
      await new Promise((r) => setTimeout(r, 1));
    }
    await Promise.all(calls);
    expect(done.length).toBeGreaterThan(1);
    expect(done.length).toBeLessThan(20); // calls during a run were coalesced
    expect(done).toEqual([...done].sort((x, y) => x - y));
  });

  it('a failed run does not block the next one', async () => {
    let k = 0;
    const s = new Serialized(async () => {
      if (++k === 1) throw new Error('down');
      return k;
    });
    await expect(s.run()).rejects.toThrow('down');
    expect(await s.run()).toBe(2);
  });
});

describe('Serialized.fresh (Review)', () => {
  /** a Serialized over manually resolved loads and a manual clock */
  const setup = (reusable?: (v: number | null) => boolean) => {
    let now = 1_000;
    const loads: ReturnType<typeof deferred<number | null>>[] = [];
    const s = new Serialized<number | null>(
      () => {
        const d = deferred<number | null>();
        loads.push(d);
        return d.promise;
      },
      { now: () => now, ...(reusable ? { reusable } : {}) },
    );
    return { s, loads, advance: (ms: number) => (now += ms) };
  };
  const flush = async () => {
    for (let i = 0; i < 4; i++) await Promise.resolve();
  };

  it('reuses a result whose build started less than maxAge ago, without a new build', async () => {
    const { s, loads, advance } = setup();
    const a = s.run();
    loads[0]!.resolve(1);
    await a;
    await flush();
    advance(2_000);
    expect(await s.fresh(3_000)).toBe(1);
    expect(loads).toHaveLength(1);
  });

  it('an older result is read again; the build in flight is joined (never queued behind)', async () => {
    const { s, loads, advance } = setup();
    const a = s.run();
    loads[0]!.resolve(1);
    await a;
    await flush();
    advance(5_000);
    const b = s.fresh(3_000);
    expect(loads).toHaveLength(2); // too old: a new build
    advance(10_000);
    const c = s.fresh(3_000); // that build is in flight (10 s old): joined, not a third build
    expect(loads).toHaveLength(2);
    loads[1]!.resolve(2);
    expect(await b).toBe(2);
    expect(await c).toBe(2);
  });

  it('after invalidate (a submission spent coins) nothing read before it is reused: the queued follow-up is waited for', async () => {
    const { s, loads, advance } = setup();
    const a = s.run();
    loads[0]!.resolve(1);
    await a;
    await flush();
    const inflight = s.run(); // a build in flight from before the submission
    advance(100);
    s.invalidate();
    const after = s.run(); // the refresh the submission asks for: queued behind the one in flight
    const review = s.fresh(3_000);
    expect(loads).toHaveLength(2);
    loads[1]!.resolve(2);
    expect(await inflight).toBe(2);
    await flush();
    expect(loads).toHaveLength(3); // the follow-up started only after the stale build ended
    loads[2]!.resolve(3);
    expect(await after).toBe(3);
    expect(await review).toBe(3); // Review got the post-submission read, not the stale one
  });

  it('a failed build (null) is never reused', async () => {
    const { s, loads } = setup((v) => v !== null);
    const a = s.run();
    loads[0]!.resolve(null);
    await a;
    await flush();
    const b = s.fresh(3_000);
    expect(loads).toHaveLength(2);
    loads[1]!.resolve(7);
    expect(await b).toBe(7);
  });

  it('the result of a new build becomes the one reused', async () => {
    const { s, loads, advance } = setup();
    const first = s.run();
    loads[0]!.resolve(1);
    await first;
    await flush();
    expect(await s.fresh(3_000)).toBe(1);
    advance(4_000);
    const second = s.fresh(3_000);
    loads[1]!.resolve(2);
    expect(await second).toBe(2);
    await flush();
    expect(await s.fresh(3_000)).toBe(2);
    expect(loads).toHaveLength(2);
  });
});
