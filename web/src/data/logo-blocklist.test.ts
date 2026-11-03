// The token-logo blocklist (data/logo-blocklist.ts): reading the list, failing open, the cache, a viewer's own report.
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { createLogoBlocklist, githubReportUrl, logoBlocklistUrlOf, logoReportsUrlOf, LOGO_BLOCKLIST_KEY, parseBlocklist, reportLogo } from './logo-blocklist';

const A = 'a'.repeat(64);
const B = 'b'.repeat(64);
const URL_ = 'https://idx.example/v1/logo-blocklist';

const mem = (initial: Record<string, string> = {}, throws = false) => {
  const m = new Map(Object.entries(initial));
  const boom = (): never => {
    throw new Error('denied');
  };
  return { m, getItem: (k: string) => (throws ? boom() : (m.get(k) ?? null)), setItem: (k: string, v: string) => (throws ? boom() : void m.set(k, v)), removeItem: (k: string) => void m.delete(k) };
};
const json = (body: unknown, status = 200) => vi.fn(() => Promise.resolve(new Response(typeof body === 'string' ? body : JSON.stringify(body), { status })));
const asFetch = (f: unknown) => f as unknown as typeof fetch;

describe('the list address', () => {
  it('is the configured one, else the indexer\'s /v1/logo-blocklist; none without an http(s) indexer', () => {
    expect(logoBlocklistUrlOf({ logoBlocklistUrl: 'https://l.example/v1/logo-blocklist', indexerUrl: 'https://idx.example' })).toBe('https://l.example/v1/logo-blocklist');
    expect(logoBlocklistUrlOf({ logoBlocklistUrl: '', indexerUrl: 'https://idx.example/' })).toBe('https://idx.example/v1/logo-blocklist');
    expect(logoBlocklistUrlOf({ logoBlocklistUrl: '', indexerUrl: 'http://127.0.0.1:8090' })).toBe('http://127.0.0.1:8090/v1/logo-blocklist');
    expect(logoBlocklistUrlOf({ logoBlocklistUrl: '', indexerUrl: '' })).toBe('');
    expect(logoBlocklistUrlOf({ logoBlocklistUrl: '', indexerUrl: 'ws://x' })).toBe('');
  });
  it('the report address sits beside it; a list at another path has none (the GitHub form remains)', () => {
    expect(logoReportsUrlOf(URL_)).toBe('https://idx.example/v1/logo-reports');
    expect(logoReportsUrlOf('https://l.example/list.json')).toBeNull();
    expect(logoReportsUrlOf('')).toBeNull();
  });
  it('the GitHub form is pre-filled with the covenant id', () => {
    expect(githubReportUrl(A.toUpperCase())).toBe(`https://github.com/RossKU/kob/issues/new?template=logo-report.yml&covenant_id=${A}`);
  });
});

describe('parseBlocklist', () => {
  it('takes {hidden: [64-hex ids]}, lower-cased; anything else is null', () => {
    expect([...parseBlocklist({ hidden: [A, B.toUpperCase()], updated_ms: 5 })!]).toEqual([A, B]);
    expect(parseBlocklist({ hidden: [] })!.size).toBe(0);
    for (const bad of [null, [], 'x', {}, { hidden: 'a' }, { hidden: [A, 'zz'] }, { hidden: [A, 7] }, { hidden: new Array<string>(100_001).fill(A) }]) expect(parseBlocklist(bad), JSON.stringify(bad)?.slice(0, 40)).toBeNull();
  });
});

describe('the blocklist store', () => {
  beforeEach(() => vi.useFakeTimers());
  afterEach(() => vi.useRealTimers());

  it('hides what the indexer lists, tells the listeners once per change, and keeps the list in storage', async () => {
    const storage = mem();
    const f = json({ hidden: [A], updated_ms: 1 });
    const l = createLogoBlocklist({ fetch: asFetch(f), storage });
    const seen = vi.fn();
    l.subscribe(seen);
    expect(l.isHidden(A)).toBe(false);
    expect(await l.refresh(URL_)).toBe(true);
    expect([l.isHidden(A), l.isHidden(A.toUpperCase()), l.isHidden(B), seen.mock.calls.length]).toEqual([true, true, false, 1]);
    expect(JSON.parse(storage.m.get(LOGO_BLOCKLIST_KEY)!)).toEqual({ hidden: [A] });
    // the same list again: no change, no notification
    await l.refresh(URL_);
    expect(seen).toHaveBeenCalledTimes(1);
    expect(f).toHaveBeenCalledWith(URL_, expect.objectContaining({ credentials: 'omit', referrerPolicy: 'no-referrer' }));
  });

  it('a restored token leaves the list at the next read', async () => {
    const l = createLogoBlocklist({ fetch: asFetch(vi.fn().mockResolvedValueOnce(new Response(JSON.stringify({ hidden: [A, B] }))).mockResolvedValueOnce(new Response(JSON.stringify({ hidden: [B] })))), storage: mem() });
    await l.refresh(URL_);
    expect([l.isHidden(A), l.isHidden(B)]).toEqual([true, true]);
    await l.refresh(URL_);
    expect([l.isHidden(A), l.isHidden(B)]).toEqual([false, true]);
  });

  it('the last list is the first paint of the next visit', () => {
    const l = createLogoBlocklist({ fetch: asFetch(json({ hidden: [] })), storage: mem({ [LOGO_BLOCKLIST_KEY]: JSON.stringify({ hidden: [A] }) }) });
    expect(l.isHidden(A)).toBe(true);
  });

  it('FAILS OPEN: unreachable, an error status, no such route, junk or a wrong shape keep the last known list (empty on a first visit) and never hide everything', async () => {
    for (const f of [
      () => Promise.reject(new Error('offline')),
      () => Promise.resolve(new Response('{}', { status: 500 })),
      () => Promise.resolve(new Response('', { status: 404 })),
      () => Promise.resolve(new Response('<html>', { status: 200 })),
      () => Promise.resolve(new Response(JSON.stringify({ hidden: 'all' }))),
      () => Promise.resolve(new Response(JSON.stringify({ hidden: [A, 'not an id'] }))),
    ]) {
      const first = createLogoBlocklist({ fetch: asFetch(vi.fn(f)), storage: mem() });
      expect(await first.refresh(URL_)).toBe(false);
      expect([first.isHidden(A), first.isHidden(B)]).toEqual([false, false]);
      const kept = createLogoBlocklist({ fetch: asFetch(vi.fn(f)), storage: mem({ [LOGO_BLOCKLIST_KEY]: JSON.stringify({ hidden: [A] }) }) });
      expect(await kept.refresh(URL_)).toBe(false);
      expect([kept.isHidden(A), kept.isHidden(B)]).toEqual([true, false]);
    }
  });

  it('a refusing storage, a corrupt cache and no URL are all harmless', async () => {
    const l = createLogoBlocklist({ fetch: asFetch(json({ hidden: [A] })), storage: mem({}, true) });
    expect(await l.refresh(URL_)).toBe(true);
    expect(l.isHidden(A)).toBe(true);
    expect(createLogoBlocklist({ fetch: asFetch(json({ hidden: [] })), storage: mem({ [LOGO_BLOCKLIST_KEY]: '{not json' }) }).isHidden(A)).toBe(false);
    const f = json({ hidden: [] });
    expect(await createLogoBlocklist({ fetch: asFetch(f), storage: null }).refresh('')).toBe(false);
    expect(f).not.toHaveBeenCalled();
  });

  it('start reads now and every interval, once per URL however many views start it, and stops when the last one does', async () => {
    const f = json({ hidden: [] });
    const l = createLogoBlocklist({ fetch: asFetch(f), storage: mem(), intervalMs: 120_000 });
    const stop1 = l.start(URL_);
    const stop2 = l.start(URL_);
    await vi.advanceTimersByTimeAsync(0);
    expect(f).toHaveBeenCalledTimes(1);
    await vi.advanceTimersByTimeAsync(120_000);
    expect(f).toHaveBeenCalledTimes(2);
    stop1();
    await vi.advanceTimersByTimeAsync(120_000);
    expect(f).toHaveBeenCalledTimes(3);
    stop2();
    await vi.advanceTimersByTimeAsync(600_000);
    expect(f).toHaveBeenCalledTimes(3);
    expect(l.start('')()).toBeUndefined();
  });

  it('a viewer\'s own report hides the logo for them at once', () => {
    const l = createLogoBlocklist({ fetch: asFetch(json({ hidden: [] })), storage: mem() });
    const seen = vi.fn();
    l.subscribe(seen);
    l.hideLocally(A);
    l.hideLocally(A);
    l.hideLocally('nope');
    expect([l.isHidden(A), seen.mock.calls.length]).toEqual([true, 1]);
  });
});

describe('reportLogo', () => {
  const post = (f: ReturnType<typeof json>) => reportLogo('https://idx.example/v1/logo-reports', A.toUpperCase(), 'impersonation', { fetch: asFetch(f) });

  it('posts the covenant id and the reason as JSON, no cookies, no referrer, and reads the result', async () => {
    const f = json({ result: 'hidden', covenant_id: A });
    expect(await post(f)).toEqual({ ok: true, result: 'hidden' });
    const [url, init] = f.mock.calls[0] as unknown as [string, RequestInit];
    expect(url).toBe('https://idx.example/v1/logo-reports');
    expect(init).toMatchObject({ method: 'POST', credentials: 'omit', referrerPolicy: 'no-referrer' });
    expect(JSON.parse(init.body as string)).toEqual({ covenant_id: A, reason: 'impersonation' });
    expect(await post(json({ result: 'recorded' }))).toEqual({ ok: true, result: 'recorded' });
  });

  it('names the failure: no such route, rate limited (with the wait), a refused body, a server error, junk, offline', async () => {
    expect(await post(json('', 404))).toEqual({ ok: false, reason: 'unavailable' });
    expect(await post(json('', 405))).toEqual({ ok: false, reason: 'unavailable' });
    expect(await post(json('', 400))).toEqual({ ok: false, reason: 'invalid' });
    expect(await post(json('', 500))).toEqual({ ok: false, reason: 'network' });
    expect(await post(json({ result: 'maybe' }))).toEqual({ ok: false, reason: 'network' });
    expect(await post(json('not json'))).toEqual({ ok: false, reason: 'network' });
    const limited = vi.fn(() => Promise.resolve(new Response('', { status: 429, headers: { 'retry-after': '90' } })));
    expect(await post(limited as unknown as ReturnType<typeof json>)).toEqual({ ok: false, reason: 'rate-limited', retryAfterSecs: 90 });
    expect(await reportLogo('https://idx.example/v1/logo-reports', A, 'other', { fetch: asFetch(() => Promise.reject(new Error('offline'))) })).toEqual({ ok: false, reason: 'network' });
  });
});
