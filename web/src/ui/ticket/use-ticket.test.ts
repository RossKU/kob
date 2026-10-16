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


describe('Serialized.since / settledSince (Review)', () => {
  /** a Serialized over manually resolved loads and a manual clock */
  const setup = () => {
    let now = 1_000;
    const loads: ReturnType<typeof deferred<number>>[] = [];
    const s = new Serialized<number>(
      () => {
        const d = deferred<number>();
        loads.push(d);
        return d.promise;
      },
      { now: () => now },
    );
    return { s, loads, advance: (ms: number) => (now += ms), at: () => now };
  };
  const flush = async () => {
    for (let i = 0; i < 4; i++) await Promise.resolve();
  };

  it('a result read at or after t is returned at once, without a new read', async () => {
    const { s, loads, advance, at } = setup();
    const a = s.run();
    loads[0]!.resolve(1);
    await a;
    await flush();
    advance(2_000);
    expect(await s.since(at() - 3_000)).toBe(1);
    expect(loads).toHaveLength(1);
    expect(s.latest()).toMatchObject({ value: 1, at: 1_000, doneAt: 1_000 });
  });

  it('an older result is never returned: a run in flight that started after t is joined, else a new read starts', async () => {
    const { s, loads, advance, at } = setup();
    const a = s.run();
    loads[0]!.resolve(1);
    await a;
    await flush();
    advance(5_000);
    const b = s.since(at() - 3_000);
    expect(loads).toHaveLength(2); // too old: a new read
    advance(1_000);
    const c = s.since(at() - 3_000); // that read started 1 s ago: joined
    expect(loads).toHaveLength(2);
    loads[1]!.resolve(2);
    expect(await b).toBe(2);
    expect(await c).toBe(2);
  });

  it('a run in flight that started before t is not joined: the read after it is waited for', async () => {
    const { s, loads, advance, at } = setup();
    const old = s.run();
    advance(10_000);
    const want = s.since(at() - 3_000);
    loads[0]!.resolve(1);
    expect(await old).toBe(1);
    await flush();
    expect(loads).toHaveLength(2);
    loads[1]!.resolve(2);
    expect(await want).toBe(2);
  });

  it('busy while a read runs or is queued; latest keeps the start of the newest good read', async () => {
    const { s, loads, advance } = setup();
    expect(s.busy()).toBe(false);
    expect(s.latest()).toBeNull();
    const a = s.run();
    void s.run(); // queued behind it
    expect(s.busy()).toBe(true);
    advance(500);
    loads[0]!.resolve(1);
    await a;
    await flush();
    expect(s.busy()).toBe(true); // the queued one runs now
    loads[1]!.resolve(2);
    await flush();
    expect(s.busy()).toBe(false);
    expect(s.latest()).toMatchObject({ value: 2, at: 1_500 });
  });

  it('a failed run keeps the newest good result and rejects its callers', async () => {
    const { s, loads, at } = setup();
    const a = s.run();
    loads[0]!.resolve(1);
    await a;
    await flush();
    const b = s.run();
    loads[1]!.reject(new Error('down'));
    await expect(b).rejects.toThrow('down');
    await flush();
    expect(s.latest()?.value).toBe(1);
    expect(await s.since(at() - 1)).toBe(1);
  });
});
