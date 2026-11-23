// Market data over HTTP: venue candles, stablecoin rates and the reference snapshot.

import { PARSERS, parseCoinbaseMid, parseKrakenMid } from './candles.mjs';
import { combine, usdtRate, fairValueKas, oneMinuteJump, pegCheck, stableRate, venueVwap } from './reference.mjs';

const MIN_GAP_MS = 1_100;
// Margin after a minute's end before its candle counts as closed (clock skew).
const CLOSE_MARGIN_MS = 3_000;
const USER_AGENT = 'kob-mm-bot/0.1';

/**
 * The venue endpoints: [url builder for a symbol, quote currency, KAS/USDT and KAS/USDC symbols].
 */
const VENUES = {
  bybit: { url: (s) => `https://api.bybit.com/v5/market/kline?category=spot&symbol=${s}&interval=1&limit=8`, usdt: 'KASUSDT', usdc: 'KASUSDC' },
  kucoin: {
    url: (s, nowMs) => `https://api.kucoin.com/api/v1/market/candles?type=1min&symbol=${s}&startAt=${Math.floor(nowMs / 1000) - 600}&endAt=${Math.floor(nowMs / 1000) + 60}`,
    usdt: 'KAS-USDT', usdc: 'KAS-USDC',
  },
  gate: { url: (s) => `https://api.gateio.ws/api/v4/spot/candlesticks?currency_pair=${s}&interval=1m&limit=8`, usdt: 'KAS_USDT', usdc: null },
  mexc: { url: (s) => `https://api.mexc.com/api/v3/klines?symbol=${s}&interval=1m&limit=8`, usdt: 'KASUSDT', usdc: 'KASUSDC' },
  kraken: { url: (s, nowMs) => `https://api.kraken.com/0/public/OHLC?pair=${s}&interval=1&since=${Math.floor(nowMs / 1000) - 600}`, usd: 'KASUSD' },
};

const lastCall = new Map();
const wait = (ms) => new Promise((r) => setTimeout(r, ms));

/** GET a JSON document with a timeout, spacing calls to one host. */
export async function getJson(url, timeoutMs) {
  const host = new URL(url).host;
  const gap = (lastCall.get(host) ?? 0) + MIN_GAP_MS - Date.now();
  if (gap > 0) await wait(gap);
  lastCall.set(host, Date.now());
  const res = await fetch(url, { headers: { accept: 'application/json', 'user-agent': USER_AGENT }, signal: AbortSignal.timeout(timeoutMs) });
  if (!res.ok) throw new Error(`HTTP ${res.status}`);
  return res.json();
}

/** One venue's candles of one pair: { candles } or { error }. */
async function venueCandles(name, symbol, nowMs, timeoutMs) {
  try {
    return { candles: PARSERS[name](await getJson(VENUES[name].url(symbol, nowMs), timeoutMs)) };
  } catch (e) {
    return { error: e.name === 'TimeoutError' ? 'timeout' : e.message };
  }
}

/** The Kraken mids of several pairs in one call: { [pair]: value } (a missing pair is absent). */
async function krakenMids(pairs, timeoutMs) {
  const out = {};
  if (!pairs.length) return out;
  try {
    const body = await getJson(`https://api.kraken.com/0/public/Ticker?pair=${pairs.join(',')}`, timeoutMs);
    for (const p of pairs) {
      try { out[p] = parseKrakenMid(body, p); } catch { /* absent */ }
    }
  } catch { /* every pair absent */ }
  return out;
}

async function coinbaseMid(product, timeoutMs) {
  try { return parseCoinbaseMid(await getJson(`https://api.exchange.coinbase.com/products/${product}/ticker`, timeoutMs)); } catch { return null; }
}

/** The reference snapshot; `memory` keeps the rates and the history between calls. */
export async function readReference(cfg, memory, nowMs = Date.now()) {
  const r = cfg.reference;
  const t = cfg.token;
  memory.history ??= [];

  // The stablecoin rates: USDT/USD for the USDT venues, and the token's own USD rate.
  const tokenPair = t.usdSource.startsWith('kraken:') ? t.usdSource.slice(7) : null;
  const wantCross = r.crossCheck?.enabled && nowMs - (memory.lastCrossCheck ?? 0) >= r.crossCheck.everySec * 1000;
  const pairs = [...new Set([r.usdtUsd.includes('kraken') ? 'USDTUSD' : null, tokenPair, wantCross ? 'USDCUSD' : null].filter(Boolean))];
  // Every USDT/USD source is asked: one reading alone would scale three venues together.
  const [mids, cb] = await Promise.all([krakenMids(pairs, r.timeoutMs), r.usdtUsd.includes('coinbase') ? coinbaseMid('USDT-USD', r.timeoutMs) : null]);
  const usdt = usdtRate({ kraken: mids.USDTUSD, coinbase: cb }, { band: r.usdtBand, agreement: r.usdtAgreement });
  if (usdt.value > 0) memory.usdt = { value: usdt.value, at: nowMs, source: usdt.source };
  else if (usdt.reason !== 'no reading') memory.usdt = null; // refused or in disagreement: no rate until they agree
  if (tokenPair && mids[tokenPair] > 0) memory.tokenUsd = { value: mids[tokenPair], at: nowMs, source: `kraken ${tokenPair}` };
  if (mids.USDCUSD > 0) memory.usdc = { value: mids.USDCUSD, at: nowMs, source: 'kraken USDCUSD' };
  const usdtUsd = stableRate(memory.usdt, nowMs, r.stableMaxAgeSec, null);
  const tokenUsd = t.usdSource === 'fixed' ? { value: t.usdFallback, source: 'fixed' } : stableRate(memory.tokenUsd, nowMs, r.stableMaxAgeSec, t.usdFallback);

  // The venues' candles, in parallel (each venue is its own host).
  const opts = { nowMs: nowMs - CLOSE_MARGIN_MS, windowMin: r.windowMin, maxCandleAgeSec: r.maxCandleAgeSec };
  const view = async (name) => {
    const v = VENUES[name];
    const symbol = v.usd ?? v.usdt;
    const rate = v.usd ? 1 : usdtUsd.value;
    const got = await venueCandles(name, symbol, nowMs, r.timeoutMs);
    return [name, got.error ? { fresh: false, reason: got.error } : venueVwap(got.candles, rate, opts)];
  };
  const venues = Object.fromEntries(await Promise.all(r.venues.map(view)));
  const freshPrimary = Object.values(venues).filter((v) => v.fresh).length;
  if (freshPrimary < r.fallbackBelow) Object.assign(venues, Object.fromEntries(await Promise.all(r.fallbackVenues.filter((n) => !venues[n]).map(view))));

  const c = combine(venues, { minVenues: r.minVenues, maxDeviation: r.maxDeviation, pairAgreement: r.pairAgreement });
  if (!c.ok && usdt.value == null && usdt.reason !== 'no reading') c.reason = `${c.reason}; USDT/USD: ${usdt.reason}`;
  const snap = {
    at: nowMs, ok: c.ok, reason: c.reason ?? null, kasUsd: c.kasUsd, fair: null, tokenUsd, usdtUsd,
    venues: Object.fromEntries(Object.entries(venues).map(([n, v]) => [n, v.fresh ? { fresh: true, vwap: v.vwap, base: v.base } : { fresh: false, reason: v.reason }])),
    used: c.used, dropped: c.dropped, jump: 0, crossCheck: null, peg: pegCheck(tokenUsd, t.pegUsd, t.pegBand),
  };
  if (c.ok && !(tokenUsd.value > 0)) {
    snap.ok = false;
    snap.reason = `no USD rate for the token (${t.usdSource}) and no fallback`;
  }
  if (snap.ok) {
    snap.fair = fairValueKas(tokenUsd.value, c.kasUsd);
    snap.jump = oneMinuteJump(venues, c.used, c.kasUsd, memory.history, nowMs);
    memory.history.push({ at: nowMs, kasUsd: c.kasUsd });
    // Kept past the longest backoff (5 min) plus a cycle, so a failing bot still has a reading to compare.
    memory.history = memory.history.filter((h) => nowMs - h.at <= 900_000);
  } else {
    memory.history = [];
  }

  // The cross-check against the thin KAS/USDC books: logged only, it never sets the price.
  if (wantCross && snap.ok) {
    memory.lastCrossCheck = nowMs;
    const usdc = stableRate(memory.usdc, nowMs, r.stableMaxAgeSec, null).value;
    const rows = await Promise.all(r.crossCheck.venues.filter((n) => VENUES[n]?.usdc).map(async (n) => {
      const got = await venueCandles(n, VENUES[n].usdc, nowMs, r.timeoutMs);
      if (got.error) return { venue: n, error: got.error };
      const v = venueVwap(got.candles, usdc ?? 0, opts);
      return v.fresh ? { venue: n, kasUsd: v.vwap, deviation: v.vwap / c.kasUsd - 1, baseKas: v.base } : { venue: n, error: v.reason };
    }));
    snap.crossCheck = { usdcUsd: usdc, rows };
  }
  return snap;
}
