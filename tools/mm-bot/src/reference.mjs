// The KAS/USD reference (per-venue VWAP of closed candles, median, outlier rule) and the fair
// value.

const MINUTE = 60_000;

/** The start of the minute that contains `nowMs`: candles from here on are still in progress. */
export const minuteStart = (nowMs) => Math.floor(nowMs / MINUTE) * MINUTE;

/** The candles of the last `windowMin` closed minutes (the minute in progress is dropped). */
export function closedWindow(candles, nowMs, windowMin) {
  const end = minuteStart(nowMs);
  const start = end - windowMin * MINUTE;
  return candles.filter((c) => c.t >= start && c.t < end);
}

/** The median of a non-empty list of numbers. */
export function median(xs) {
  if (!xs.length) throw new Error('median of nothing');
  const s = [...xs].sort((a, b) => a - b);
  const m = s.length >> 1;
  return s.length % 2 ? s[m] : (s[m - 1] + s[m]) / 2;
}

/** One venue: its USD VWAP over the window and its last two closes, or { fresh: false, reason }. */
export function venueVwap(candles, usdRate, { nowMs, windowMin, maxCandleAgeSec }) {
  if (!(usdRate > 0)) return { fresh: false, reason: 'no USD rate for the quote currency' };
  const rows = closedWindow(candles, nowMs, windowMin).sort((a, b) => a.t - b.t);
  if (!rows.length) return { fresh: false, reason: 'no closed candle in the window' };
  const newest = rows[rows.length - 1].t;
  // A closed candle starting at `newest` ended one minute later. Its age is measured from that end.
  const ageSec = (nowMs - (newest + MINUTE)) / 1000;
  if (ageSec > maxCandleAgeSec) return { fresh: false, reason: `newest closed candle is ${ageSec.toFixed(0)} s old` };
  const base = rows.reduce((a, c) => a + c.base, 0);
  const quote = rows.reduce((a, c) => a + c.quote, 0);
  if (!(base > 0 && quote > 0)) return { fresh: false, reason: 'no volume in the window' };
  const last = rows[rows.length - 1];
  const prev = rows.length > 1 ? rows[rows.length - 2] : null;
  return {
    fresh: true,
    vwap: (quote / base) * usdRate,
    base,
    quote: quote * usdRate,
    newest,
    lastClose: last.close * usdRate,
    prevClose: prev && prev.t === last.t - MINUTE ? prev.close * usdRate : null,
  };
}

/**
 * The median of the fresh venues, with outliers past maxDeviation dropped. It is ok only with
 * minVenues left. With only two left the median is their midpoint, which one bad venue moves, so
 * they must agree within pairAgreement.
 */
export function combine(venues, { minVenues, maxDeviation, pairAgreement = null }) {
  const dropped = [];
  const fresh = [];
  for (const [name, v] of Object.entries(venues)) {
    if (v?.fresh) fresh.push([name, v.vwap]);
    else dropped.push({ name, reason: v?.reason ?? 'no data' });
  }
  if (fresh.length < minVenues) {
    return { ok: false, kasUsd: null, used: fresh.map(([n]) => n), dropped, reason: `${fresh.length} fresh venue(s), need ${minVenues}` };
  }
  const first = median(fresh.map(([, p]) => p));
  const kept = [];
  for (const [name, p] of fresh) {
    const dev = Math.abs(p / first - 1);
    if (dev > maxDeviation) dropped.push({ name, reason: `${(dev * 100).toFixed(2)}% from the median` });
    else kept.push([name, p]);
  }
  if (kept.length < minVenues) {
    return { ok: false, kasUsd: null, used: kept.map(([n]) => n), dropped, reason: `${kept.length} venue(s) within ${(maxDeviation * 100).toFixed(2)}% of the median, need ${minVenues}` };
  }
  if (kept.length === 2 && pairAgreement != null) {
    const [a, b] = kept.map(([, p]) => p).sort((x, y) => x - y);
    if (b / a - 1 > pairAgreement) {
      return { ok: false, kasUsd: null, used: kept.map(([n]) => n), dropped,
        reason: `only 2 venues and they differ by ${((b / a - 1) * 100).toFixed(2)}%, more than ${(pairAgreement * 100).toFixed(3)}%` };
    }
  }
  return { ok: true, kasUsd: median(kept.map(([, p]) => p)), used: kept.map(([n]) => n), dropped };
}

/**
 * USDT/USD from the sources' fresh readings ({ kraken, coinbase }).
 * A reading outside 1 +- band is refused, and two that differ by more than agreement give no rate.
 * A reading that stands alone must sit within agreement of 1, the peg standing in for the second
 * source: one wrong source scales every USDT venue together, and agreement stays below half the
 * spread (config validate), so it cannot move the fair value past the best quote.
 * The result is { value, source } or { value: null, reason }.
 */
export function usdtRate(readings, { band, agreement }) {
  const ok = [];
  const bad = [];
  for (const [source, v] of Object.entries(readings)) {
    if (!(v > 0)) continue;
    if (Math.abs(v - 1) > band) bad.push(`${source} ${v.toFixed(5)} outside 1 +- ${band}`);
    else ok.push([source, v]);
  }
  if (!ok.length) return { value: null, reason: bad.join('; ') || 'no reading' };
  if (ok.length === 1 && Math.abs(ok[0][1] - 1) > agreement) {
    const [[source, v]] = ok;
    return { value: null, reason: `only ${source} ${v.toFixed(5)}, further than ${agreement} from 1 with no second source to agree` };
  }
  if (ok.length > 1) {
    const vs = ok.map(([, v]) => v);
    const d = Math.max(...vs) / Math.min(...vs) - 1;
    if (d > agreement) return { value: null, reason: `${ok.map(([s, v]) => `${s} ${v.toFixed(5)}`).join(' and ')} differ by ${(d * 100).toFixed(2)}%` };
  }
  return { value: ok.reduce((s, [, v]) => s + v, 0) / ok.length, source: ok.map(([s]) => s).join('+') };
}

/** The token's fair value in KAS per whole token: (token USD value) / (KAS USD price). */
export function fairValueKas(tokenUsd, kasUsd) {
  if (!(tokenUsd > 0) || !(kasUsd > 0)) throw new Error('fair value needs two positive prices');
  return tokenUsd / kasUsd;
}

/**
 * The larger of the venues' median last-minute move and the reference's move against about a minute
 * ago.
 */
export function oneMinuteJump(venues, used, kasUsd, history, nowMs) {
  const moves = used.map((n) => venues[n]).filter((v) => v?.fresh && v.prevClose > 0).map((v) => Math.abs(v.lastClose / v.prevClose - 1));
  const candleMove = moves.length ? median(moves) : 0;
  const earlier = [...history].reverse().find((h) => nowMs - h.at >= 55_000 && nowMs - h.at <= 120_000);
  const refMove = earlier ? Math.abs(kasUsd / earlier.kasUsd - 1) : 0;
  return Math.max(candleMove, refMove);
}

/** Whether a live rate is inside the peg band; a fallback or fixed rate always is. */
export function pegCheck(rate, pegUsd, band) {
  if (!(rate?.value > 0) || rate.source === 'fallback' || rate.source === 'fixed') return { ok: true, drift: 0 };
  const drift = rate.value / pegUsd - 1;
  return { ok: Math.abs(drift) <= band, drift };
}

/** The live reading while fresh, else the fallback, else none. */
export function stableRate(reading, nowMs, maxAgeSec, fallback) {
  if (reading && reading.value > 0 && (nowMs - reading.at) / 1000 <= maxAgeSec) return { value: reading.value, source: reading.source };
  return fallback > 0 ? { value: fallback, source: 'fallback' } : { value: null, source: 'none' };
}
