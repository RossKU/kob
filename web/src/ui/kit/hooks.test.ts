import { describe, expect, it } from 'vitest';
import { Coalescer, onVisibilityChange, pageVisible, type VisibilitySource } from './hooks';

describe('Coalescer', () => {
  it('starts a run when idle; requests during a run fold into ONE follow-up after it, none when nothing came in', () => {
    const c = new Coalescer();
    expect(c.request()).toBe(true);
    c.started();
    expect(c.request()).toBe(false);
    expect(c.request()).toBe(false);
    expect(c.request()).toBe(false);
    expect(c.settled()).toBe(true); // one follow-up for the three requests
    c.started();
    expect(c.settled()).toBe(false); // nothing came in during the follow-up
    expect(c.request()).toBe(true); // idle again
  });

  it('a run that starts covers the requests made before it (a dependency change restarts the load: no extra run)', () => {
    const c = new Coalescer();
    c.started();
    expect(c.request()).toBe(false);
    c.started(); // a newer run replaced the one in flight (it reads after the request)
    expect(c.settled()).toBe(false);
  });

  it('never aborts: a feed faster than the loader still gets every answer through, one load in flight at a time', () => {
    const c = new Coalescer();
    let inFlight = 0;
    let maxInFlight = 0;
    let runs = 0;
    const start = () => {
      c.started();
      runs += 1;
      inFlight += 1;
      maxInFlight = Math.max(maxInFlight, inFlight);
    };
    if (c.request()) start();
    for (let i = 0; i < 10; i++) {
      // five notices per load
      for (let k = 0; k < 5; k++) if (c.request()) start();
      inFlight -= 1;
      if (c.settled()) start();
    }
    expect(maxInFlight).toBe(1);
    expect(runs).toBe(11); // 1 + one follow-up per load, never 50
  });
});

describe('page visibility', () => {
  const fakeDoc = (state: string) => {
    const handlers = new Set<() => void>();
    const doc = {
      visibilityState: state,
      addEventListener: (_t: 'visibilitychange', cb: () => void) => void handlers.add(cb),
      removeEventListener: (_t: 'visibilitychange', cb: () => void) => void handlers.delete(cb),
    };
    const set = (s: string) => {
      doc.visibilityState = s;
      handlers.forEach((h) => h());
    };
    return { doc: doc as VisibilitySource, set, handlers };
  };

  it('is visible unless the document says hidden (no document: visible)', () => {
    expect(pageVisible(null)).toBe(true);
    expect(pageVisible(fakeDoc('visible').doc)).toBe(true);
    expect(pageVisible(fakeDoc('hidden').doc)).toBe(false);
  });

  it('reports every change and unsubscribes', () => {
    const f = fakeDoc('visible');
    const seen: boolean[] = [];
    const off = onVisibilityChange((v) => seen.push(v), f.doc);
    f.set('hidden');
    f.set('visible');
    off();
    f.set('hidden');
    expect(seen).toEqual([false, true]);
    expect(f.handlers.size).toBe(0);
  });
});
