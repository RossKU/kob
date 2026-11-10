import { describe, expect, it } from 'vitest';
import { adaptKasUsd, type KasUsdQuote } from '../data/kas-price';
import { HttpIndexer } from '../data/indexer';
import { CURRENCY_PREF_KEY, readCurrencyPref, resetCurrencyPrefMemory, writeCurrencyPref } from './currency-pref';
import {
  KAS_USD_MAX_AGE_S, ageWords, estimateNote, usdCompact, fallbackNotice, fiatOf, kasPerTokenToUsd, quoteAgeS, rateOf, resolveFiat, sompiToUsd, sourceWords, usdCents, usdText, usdTextFit,
  type PriceFeed,
} from './fiat';

const NOW = 1_800_000_000_000;
const quote = (over: Partial<KasUsdQuote> = {}): KasUsdQuote => ({ usd: 0.0298, at: NOW / 1000 - 60, source: 'coingecko', ...over });
const feed = (q: KasUsdQuote | null, over: Partial<PriceFeed> = {}): PriceFeed => ({ quote: q, seenAtMs: q ? NOW - 60_000 : null, status: q ? 'ok' : 'idle', ...over });
const r = (num: number, den = 1) => ({ num: BigInt(num), den: BigInt(den) });

describe('the answer of /v1/price/kas-usd', () => {
  it('is read only in the documented shape', () => {
    expect(adaptKasUsd({ usd: 0.0298, at: 1_800_000_000, source: 'coingecko' })).toEqual({ usd: 0.0298, at: 1_800_000_000, source: 'coingecko' });
    expect(adaptKasUsd({ usd: 0.03, at: 5, source: 'vwap:bybit,kucoin,gate' })?.source).toBe('vwap:bybit,kucoin,gate');
    for (const bad of [null, [], 'x', {}, { usd: 0, at: 5, source: 'coingecko' }, { usd: -1, at: 5, source: 'coingecko' }, { usd: '0.03', at: 5, source: 'coingecko' },
      { usd: Infinity, at: 5, source: 'coingecko' }, { usd: 1e6, at: 5, source: 'coingecko' }, { usd: 0.03, at: 0, source: 'coingecko' }, { usd: 0.03, at: 1.5, source: 'coingecko' },
      { usd: 0.03, at: '5', source: 'coingecko' }, { usd: 0.03, at: 5 }, { usd: 0.03, at: 5, source: '<b>x</b>' }, { usd: 0.03, at: 5, source: 'vwap:' }]) {
      expect(adaptKasUsd(bad), JSON.stringify(bad)).toBeNull();
    }
  });

  it('HttpIndexer.kasUsd reads the route, resolves null where it is not served and throws where there is no price yet', async () => {
    const urls: string[] = [];
    const make = (status: number, body: unknown) =>
      new HttpIndexer({
        baseUrl: 'https://kob.example/',
        fetch: (async (u: RequestInfo | URL) => {
          urls.push(String(u));
          return new Response(JSON.stringify(body), { status, headers: { 'content-type': 'application/json' } });
        }) as typeof fetch,
      });
    expect(await make(200, { usd: 0.03, at: 1_800_000_000, source: 'coingecko' }).kasUsd()).toEqual({ usd: 0.03, at: 1_800_000_000, source: 'coingecko' });
    expect(urls).toEqual(['https://kob.example/v1/price/kas-usd']);
    expect(await make(404, { error: { code: 'not_found', message: 'no such route' } }).kasUsd()).toBeNull();
    expect(await make(405, {}).kasUsd()).toBeNull();
    expect(await make(200, { nope: 1 }).kasUsd()).toBeNull();
    await expect(make(503, { error: { code: 'price_unavailable', message: 'no KAS price yet' } }).kasUsd()).rejects.toMatchObject({ status: 503 });
  });
});

describe('the display currency choice', () => {
  const store = (initial: Record<string, string> = {}, throws = false) => {
    const m = new Map(Object.entries(initial));
    const deny = (): never => {
      throw new Error('denied');
    };
    return { m, getItem: (k: string) => (throws ? deny() : (m.get(k) ?? null)), setItem: (k: string, v: string) => (throws ? deny() : void m.set(k, v)), removeItem: (k: string) => void m.delete(k) };
  };

  it('is KAS by default and USD only when the store says usd', () => {
    resetCurrencyPrefMemory();
    expect(readCurrencyPref(store())).toBe('kas');
    expect(readCurrencyPref(store({ [CURRENCY_PREF_KEY]: 'usd' }))).toBe('usd');
    for (const junk of ['USD', '1', 'eur', '']) expect(readCurrencyPref(store({ [CURRENCY_PREF_KEY]: junk })), junk).toBe('kas');
    expect(readCurrencyPref(null)).toBe('kas');
    expect(readCurrencyPref(store({}, true))).toBe('kas');
  });

  it('writing USD stores it under kob.currency, writing KAS removes the key, and a store that refuses is reported', () => {
    resetCurrencyPrefMemory();
    const s = store();
    expect(writeCurrencyPref('usd', s)).toBe(true);
    expect(s.m.get('kob.currency')).toBe('usd');
    expect(readCurrencyPref(s)).toBe('usd');
    expect(writeCurrencyPref('kas', s)).toBe(true);
    expect(s.m.has('kob.currency')).toBe(false);
    expect(writeCurrencyPref('usd', store({}, true))).toBe(false);
    expect(writeCurrencyPref('usd', null)).toBe(false);
    resetCurrencyPrefMemory();
  });
});

describe('what is shown', () => {
  const base = { nowMs: NOW, testnet: false };

  it('KAS stays KAS, whatever the price says', () => {
    expect(resolveFiat({ ...base, currency: 'kas', feed: feed(quote()) })).toEqual({ kind: 'kas' });
    expect(fiatOf(resolveFiat({ ...base, currency: 'kas', feed: feed(quote()) }))).toBeNull();
  });

  it('USD in force needs a quote that is there and not older than 10 minutes', () => {
    const s = resolveFiat({ ...base, currency: 'usd', feed: feed(quote()) });
    expect(s.kind).toBe('usd');
    if (s.kind === 'usd') {
      expect(s.ageS).toBe(60);
      expect(s.fiat.rate.usd).toBe(0.0298);
      expect(s.fiat.testnet).toBe(false);
    }
    // the limit: 10 minutes is still fresh, a second more is stale
    const at = (ageS: number) => resolveFiat({ ...base, currency: 'usd', feed: feed(quote({ at: NOW / 1000 - ageS }), { seenAtMs: NOW - ageS * 1000 }) });
    expect(KAS_USD_MAX_AGE_S).toBe(600);
    expect(at(600).kind).toBe('usd');
    const stale = at(601);
    expect(stale).toMatchObject({ kind: 'fallback', reason: 'stale' });
    expect(fiatOf(stale)).toBeNull();
  });

  it('a missing price falls back to KAS with its reason: loading, the route is not served, a failed read', () => {
    const f = (status: PriceFeed['status']) => resolveFiat({ ...base, currency: 'usd', feed: feed(null, { status }) });
    expect(f('idle')).toMatchObject({ kind: 'fallback', reason: 'loading' });
    expect(f('loading')).toMatchObject({ kind: 'fallback', reason: 'loading' });
    expect(f('unavailable')).toMatchObject({ kind: 'fallback', reason: 'unavailable' });
    expect(f('error')).toMatchObject({ kind: 'fallback', reason: 'missing' });
    // a quote the rate cannot use is no price
    expect(resolveFiat({ ...base, currency: 'usd', feed: feed(quote({ usd: Number.NaN })) })).toMatchObject({ kind: 'fallback', reason: 'missing' });
  });

  it('a failed read keeps the last quote until it is too old', () => {
    const q = quote({ at: NOW / 1000 - 300 });
    expect(resolveFiat({ ...base, currency: 'usd', feed: feed(q, { seenAtMs: NOW - 300_000, status: 'error' }) }).kind).toBe('usd');
    expect(resolveFiat({ ...base, currency: 'usd', feed: feed(q, { seenAtMs: NOW - 300_000, status: 'error' }), nowMs: NOW + 400_000 }).kind).toBe('fallback');
  });

  it('the age is the larger of the clock\'s and the time this browser has held the quote: a slow clock does not make an old quote young', () => {
    const q = quote({ at: NOW / 1000 + 3600 });
    expect(quoteAgeS(q, null, NOW)).toBe(0);
    expect(quoteAgeS(q, NOW - 20 * 60_000, NOW)).toBe(1200);
    expect(resolveFiat({ ...base, currency: 'usd', feed: feed(q, { seenAtMs: NOW - 20 * 60_000 }) })).toMatchObject({ kind: 'fallback', reason: 'stale' });
    // a fast clock fails safe (KAS), never a wrong number
    expect(quoteAgeS(quote({ at: NOW / 1000 - 5000 }), NOW, NOW)).toBe(5000);
  });

  it('testnet is carried to the views', () => {
    const s = resolveFiat({ ...base, testnet: true, currency: 'usd', feed: feed(quote()) });
    expect(s.kind === 'usd' && s.fiat.testnet).toBe(true);
  });
});

describe('conversion and text', () => {
  const rate = rateOf(0.0298)!;

  it('the rate is exact on nine decimals and refuses nonsense', () => {
    expect(rate).toEqual({ usd: 0.0298, num: 29_800_000n, den: 1_000_000_000n });
    for (const bad of [0, -1, Number.NaN, Infinity, 1000, 1e-12]) expect(rateOf(bad), String(bad)).toBeNull();
  });

  it('a value in KAS is multiplied by the price, exactly', () => {
    // 35,800 KAS x $0.0298 = $1,066.84
    expect(usdText(sompiToUsd(3_580_000_000_000n, rate))).toBe('$1,066.84');
    // 1 sompi x $0.0298 is far below a cent, and still not shown as zero
    expect(usdText(sompiToUsd(1n, rate))).toBe('$0.000000000298');
    expect(usdCents(sompiToUsd(3_580_000_000_000n, rate))).toBe(106_684n);
    // a token price: 0.5 KAS per token
    expect(usdText(kasPerTokenToUsd(r(1, 2), rate))).toBe('$0.0149');
    expect(usdText(sompiToUsd(0n, rate))).toBe('$0.00');
  });

  it('formats as dollars with grouping, two decimals from a dollar up and significant digits below', () => {
    expect(usdText(r(1_066_77, 100))).toBe('$1,066.77');
    expect(usdText(r(1))).toBe('$1.00');
    expect(usdText(r(5, 10))).toBe('$0.50');
    expect(usdText(r(576, 10_000))).toBe('$0.0576');
    expect(usdText(r(1234, 100_000_000))).toBe('$0.00001234');
    expect(usdText(r(999_999_995, 1_000_000))).toBe('$1,000.00');
    expect(usdText({ num: 1n, den: 10n ** 15n })).toBe('<$0.000000000001');
    expect(usdText({ num: -150n, den: 100n })).toBe('-$1.50');
  });

  it('a table cell goes compact before it would cut a digit', () => {
    expect(usdTextFit(r(1_066_77, 100), 12)).toBe('$1,066.77');
    expect(usdTextFit(r(56_609_953_40, 100), 12)).toBe('$56.61M');
    expect(usdTextFit(r(4_200_000_000), 12)).toBe('$4.20B');
    expect(usdTextFit(r(123_456_789_000_000), 12)).toBe('$123.46T');
    expect(usdTextFit(r(999_999), 12)).toBe('$999,999.00');
  });

  it('a tile or an axis is compact and never rounded up: cents below a hundred thousand dollars, then k, M, B, T', () => {
    expect(usdCompact(r(984_099, 100))).toBe('$9,840.99');
    expect(usdCompact(r(99_999_999, 1000))).toBe('$99,999.99');
    expect(usdCompact(r(100_000))).toBe('$100k');
    expect(usdCompact(r(412_999))).toBe('$412k');
    expect(usdCompact(r(9_819_999))).toBe('$9.81M');
    expect(usdCompact(r(1_999_999_999))).toBe('$1.99B');
    expect(usdCompact(r(3_500_000_000_000))).toBe('$3.50T');
    expect(usdCompact(r(0))).toBe('$0.00');
    expect(usdCompact(r(576, 10_000))).toBe('$0.0576');
    expect(usdCompact({ num: -150n, den: 100n })).toBe('-$1.50');
  });

  it('names where the price came from, and says that USDT is taken as USD for an average of exchanges', () => {
    expect(sourceWords('coingecko')).toEqual({ name: 'CoinGecko', usdt: false });
    expect(sourceWords('vwap:bybit,kucoin,gate,mexc')).toEqual({ name: 'Bybit, KuCoin, Gate, MEXC', usdt: true });
    expect(sourceWords('vwap:zzz')).toEqual({ name: 'zzz', usdt: true });
    expect(ageWords(10)).toBe('under a minute');
    expect(ageWords(185)).toBe('3 min');
    expect(ageWords(3900)).toBe('1 h 5 min');
    const s = resolveFiat({ nowMs: NOW, testnet: false, currency: 'usd', feed: feed(quote({ at: NOW / 1000 - 120 })) });
    const f = fiatOf(s)!;
    expect(estimateNote(f, 120)).toBe('USD at the KAS price from CoinGecko, 2 min ago. An estimate, not a quote.');
    expect(estimateNote({ ...f, quote: { ...f.quote, source: 'vwap:bybit,kucoin' } }, 120)).toBe('USD (USDT) at the KAS price from Bybit, KuCoin, 2 min ago. An estimate, not a quote.');
    expect(estimateNote({ ...f, testnet: true }, 120)).toBe(
      'USD at the KAS price from CoinGecko, 2 min ago. An estimate, not a quote. Testnet KAS has no value: priced as mainnet KAS, for display only.',
    );
  });

  it('every fallback has its one-line notice', () => {
    const n = (reason: 'loading' | 'unavailable' | 'stale' | 'missing', ageS: number | null = null) => fallbackNotice({ kind: 'fallback', reason, ageS });
    expect(n('loading')).toContain('Fetching');
    expect(n('unavailable')).toContain('serves no KAS price');
    expect(n('missing')).toContain('could not be read');
    expect(n('stale', 1200)).toBe('The KAS price is 20 min old, too old to show: showing KAS.');
    for (const reason of ['loading', 'unavailable', 'stale', 'missing'] as const) expect(n(reason, 100)).not.toContain('{');
  });
});
