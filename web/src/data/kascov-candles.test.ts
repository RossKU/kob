import { describe, expect, it, vi } from 'vitest';
import { fetchKascovCandles, kascovBucketFor, kascovCandlesUrl, parseKascovCandles } from './kascov-candles';

const ID = 'd8c4356d20712463bbf2da82a5411c3aea18fec024a2b9c014bba1b4524cc8c2';
const candle = (t: unknown, base: unknown, quote: unknown, over: Record<string, unknown> = {}) => ({ t, carried: false, close: { base_amount: base, quote_sompi: quote }, open: { base_amount: 1, quote_sompi: 1 }, trades: 3, ...over });
const doc = (candles: unknown[], over: Record<string, unknown> = {}) => ({ bucket: '1h', bucket_ms: 3_600_000, network: 'mainnet', token_id: ID, candles, ...over });
const want = { network: 'mainnet', covenantId: ID, bucket: '1h' } as const;
const json = (body: unknown, init: ResponseInit = {}) => new Response(typeof body === 'string' ? body : JSON.stringify(body), { status: 200, ...init });

describe('which kascov bucket sits under a KOB interval', () => {
  it('1m and 5m use 15m (kascov 1m is about 10 MB for a month of history), 1H uses 1h, 1D uses 1d', () => {
    expect(kascovBucketFor('1m')).toBe('15m');
    expect(kascovBucketFor('5m')).toBe('15m');
    expect(kascovBucketFor('1h')).toBe('1h');
    expect(kascovBucketFor('1d')).toBe('1d');
  });
  it('the request names the network, the token, the bucket and all phases; a base with a trailing slash or a path works', () => {
    expect(kascovCandlesUrl('mainnet', ID, '15m')).toBe(`https://kascov.io/data/mainnet/token/${ID}/candles?bucket=15m&phase=all`);
    expect(kascovCandlesUrl('testnet-10', ID, '1d', 'https://k.example/data/')).toBe(`https://k.example/data/testnet-10/token/${ID}/candles?bucket=1d&phase=all`);
  });
});

describe('parseKascovCandles', () => {
  it('reads the close as an exact ratio, oldest first, one per time; numeric strings are integers too', () => {
    const got = parseKascovCandles(doc([candle(7_200_000, 19716, 484325962), candle(3_600_000, '5', '9007199254740993'), candle(7_200_000, 1, 2)]), want)!;
    expect(got).toEqual([
      { t: 3_600_000, baseAmount: 5n, quoteSompi: 9007199254740993n },
      { t: 7_200_000, baseAmount: 1n, quoteSompi: 2n },
    ]);
  });

  it('skips a candle that is not usable and keeps the rest', () => {
    const bad = [candle(0, 1, 1), candle(-5, 1, 1), candle(1.5, 1, 1), candle('1', 1, 1), candle(1, 0, 1), candle(1, 1, 0), candle(1, 1.5, 1), candle(1, '1e3', 1), candle(1, {}, 1), { t: 5 }, null, 'x'];
    expect(parseKascovCandles(doc([...bad, candle(3_600_000, 2, 3)]), want)).toEqual([{ t: 3_600_000, baseAmount: 2n, quoteSompi: 3n }]);
    expect(parseKascovCandles(doc(bad), want)).toEqual([]);
  });

  it('is null for another bucket, token or network, or when candles is not a list', () => {
    expect(parseKascovCandles(doc([], { bucket: '15m' }), want)).toBeNull();
    expect(parseKascovCandles(doc([], { token_id: 'a'.repeat(64) }), want)).toBeNull();
    expect(parseKascovCandles(doc([], { network: 'testnet-10' }), want)).toBeNull();
    expect(parseKascovCandles(doc([]), want)).toEqual([]);
    expect(parseKascovCandles({ ...doc([]), candles: { a: 1 } }, want)).toBeNull();
    for (const x of [null, undefined, 'x', [], 5]) expect(parseKascovCandles(x, want)).toBeNull();
  });
});

describe('fetchKascovCandles', () => {
  it('asks once with no credentials or referrer, and answers the candles', async () => {
    const f = vi.fn(async (_u: string, _i: RequestInit) => json(doc([candle(3_600_000, 2, 3)])));
    const r = await fetchKascovCandles('mainnet', ID.toUpperCase(), '1h', { fetch: f as unknown as typeof fetch });
    expect(r).toEqual({ ok: true, candles: [{ t: 3_600_000, baseAmount: 2n, quoteSompi: 3n }] });
    expect(f).toHaveBeenCalledTimes(1);
    const [url, init] = f.mock.calls[0]!;
    expect(url).toBe(`https://kascov.io/data/mainnet/token/${ID}/candles?bucket=1h&phase=all`);
    expect(init.credentials).toBe('omit');
    expect(init.referrerPolicy).toBe('no-referrer');
  });

  it('every failure is a result, never a throw', async () => {
    const ask = (f: () => Promise<Response>, over: Record<string, unknown> = {}) => fetchKascovCandles('mainnet', ID, '1h', { fetch: f as unknown as typeof fetch, ...over });
    expect(await ask(async () => json('nope', { status: 503 }))).toEqual({ ok: false, reason: 'http' });
    expect(await ask(async () => json('{not json'))).toEqual({ ok: false, reason: 'shape' });
    expect(await ask(async () => json(doc([], { bucket: '1d' })))).toEqual({ ok: false, reason: 'shape' });
    expect(await ask(async () => { throw new TypeError('offline'); })).toEqual({ ok: false, reason: 'network' });
    expect(await ask(async () => json(doc([candle(3_600_000, 2, 3)])), { maxBytes: 10 })).toEqual({ ok: false, reason: 'too-large' });
    expect(await fetchKascovCandles('regtest', ID, '1h')).toEqual({ ok: false, reason: 'unsupported' });
    expect(await fetchKascovCandles('mainnet', 'zz', '1h')).toEqual({ ok: false, reason: 'unsupported' });
    expect(await fetchKascovCandles('mainnet', ID, '1h', { base: '' })).toEqual({ ok: false, reason: 'unsupported' });
  });

  it('an aborted signal ends it as a failure; a slow answer times out', async () => {
    const ctl = new AbortController();
    ctl.abort();
    const f = vi.fn(async () => json(doc([])));
    expect(await fetchKascovCandles('mainnet', ID, '1h', { fetch: f as unknown as typeof fetch, signal: ctl.signal })).toEqual({ ok: false, reason: 'network' });
    expect(f).not.toHaveBeenCalled();
    const slow = (_u: string, init: RequestInit) => new Promise<Response>((_res, rej) => init.signal?.addEventListener('abort', () => rej(new DOMException('aborted', 'AbortError'))));
    expect(await fetchKascovCandles('mainnet', ID, '1h', { fetch: slow as unknown as typeof fetch, timeoutMs: 20 })).toEqual({ ok: false, reason: 'timeout' });
  });
});
