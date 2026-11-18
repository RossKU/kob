// Venue kline parsers to { t (ms), base, quote, close }, oldest first. They keep the minute in
// progress and throw on an unknown response.

const num = (v, what) => {
  const n = typeof v === 'number' ? v : Number.parseFloat(v);
  if (!Number.isFinite(n) || n < 0) throw new Error(`bad ${what}: ${JSON.stringify(v)}`);
  return n;
};

const sortByTime = (rows) => rows.sort((a, b) => a.t - b.t);

/**
 * Bybit v5 `/v5/market/kline`: `[startMs, open, high, low, close, baseVolume, turnover]`, newest
 * first.
 */
export function parseBybit(body) {
  if (body?.retCode !== 0 || !Array.isArray(body?.result?.list)) throw new Error(`bybit: ${body?.retMsg ?? 'unexpected response'}`);
  return sortByTime(body.result.list.map((r) => ({ t: num(r[0], 'time'), close: num(r[4], 'close'), base: num(r[5], 'volume'), quote: num(r[6], 'turnover') })));
}

/**
 * KuCoin `/api/v1/market/candles`: `[startSec, open, close, high, low, baseVolume, turnover]`,
 * newest first.
 */
export function parseKucoin(body) {
  if (body?.code !== '200000' || !Array.isArray(body?.data)) throw new Error(`kucoin: ${body?.msg ?? 'unexpected response'}`);
  return sortByTime(body.data.map((r) => ({ t: num(r[0], 'time') * 1000, close: num(r[2], 'close'), base: num(r[5], 'volume'), quote: num(r[6], 'turnover') })));
}

/**
 * Gate v4 `/spot/candlesticks`: `[startSec, quoteVolume, close, high, low, open, baseVolume,
 * closed]`, oldest first.
 */
export function parseGate(body) {
  if (!Array.isArray(body)) throw new Error(`gate: ${body?.message ?? body?.label ?? 'unexpected response'}`);
  return sortByTime(body.map((r) => ({ t: num(r[0], 'time') * 1000, close: num(r[2], 'close'), base: num(r[6], 'volume'), quote: num(r[1], 'quote volume') })));
}

/**
 * MEXC v3 `/api/v3/klines`: `[startMs, open, high, low, close, baseVolume, closeMs, quoteVolume]`,
 * oldest first.
 */
export function parseMexc(body) {
  if (!Array.isArray(body)) throw new Error(`mexc: ${body?.msg ?? 'unexpected response'}`);
  return sortByTime(body.map((r) => ({ t: num(r[0], 'time'), close: num(r[4], 'close'), base: num(r[5], 'volume'), quote: num(r[7], 'quote volume') })));
}

/**
 * Kraken `/0/public/OHLC`: `[startSec, o, h, l, c, vwap, volume, count]`, oldest first. quote =
 * vwap x volume.
 */
export function parseKraken(body) {
  if (Array.isArray(body?.error) && body.error.length) throw new Error(`kraken: ${body.error.join(', ')}`);
  const key = Object.keys(body?.result ?? {}).find((k) => k !== 'last');
  const rows = key ? body.result[key] : null;
  if (!Array.isArray(rows)) throw new Error('kraken: unexpected response');
  return sortByTime(rows.map((r) => {
    const base = num(r[6], 'volume');
    return { t: num(r[0], 'time') * 1000, close: num(r[4], 'close'), base, quote: base * num(r[5], 'vwap') };
  }));
}

/**
 * Kraken `/0/public/Ticker` mid of one pair (Kraken may rename it: USDTUSD comes back as USDTZUSD).
 */
export function parseKrakenMid(body, pair) {
  if (Array.isArray(body?.error) && body.error.length) throw new Error(`kraken: ${body.error.join(', ')}`);
  const res = body?.result ?? {};
  const want = pair.toUpperCase();
  const key = Object.keys(res).find((k) => k === want || k.replace(/Z(?=USD$)/, '') === want || k.replace(/^X/, '').replace(/Z(?=USD$)/, '') === want);
  if (!key) throw new Error(`kraken: no ticker ${pair}`);
  const ask = num(res[key]?.a?.[0], 'ask');
  const bid = num(res[key]?.b?.[0], 'bid');
  if (!(ask > 0 && bid > 0 && ask >= bid)) throw new Error(`kraken: bad book for ${pair}`);
  return (ask + bid) / 2;
}

/** Coinbase Exchange `/products/<id>/ticker`: the mid of `bid` and `ask`. */
export function parseCoinbaseMid(body) {
  const ask = num(body?.ask, 'ask');
  const bid = num(body?.bid, 'bid');
  if (!(ask > 0 && bid > 0 && ask >= bid)) throw new Error('coinbase: bad book');
  return (ask + bid) / 2;
}

export const PARSERS = { bybit: parseBybit, kucoin: parseKucoin, gate: parseGate, mexc: parseMexc, kraken: parseKraken };
