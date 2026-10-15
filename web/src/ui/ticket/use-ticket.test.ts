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
