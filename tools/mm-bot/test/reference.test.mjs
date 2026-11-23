// Tests the candle parsers and the reference price on responses recorded from the venues.
// The fixtures (test/fixtures) were recorded on 2026-11-16 at about 16:56Z.
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { test } from 'node:test';
import { parseBybit, parseCoinbaseMid, parseGate, parseKraken, parseKrakenMid, parseKucoin, parseMexc } from '../src/candles.mjs';
import { closedWindow, combine, fairValueKas, median, oneMinuteJump, stableRate, usdtRate, venueVwap } from '../src/reference.mjs';

const fx = (name) => JSON.parse(readFileSync(new URL(`./fixtures/${name}`, import.meta.url), 'utf8'));
// Bybit's response time. The minute 16:56 (1794848160 s) is still in progress.
const NOW = 1794848208940;
const OPTS = { nowMs: NOW, windowMin: 5, maxCandleAgeSec: 120 };
const close = (a, b, tol = 1e-12) => assert.ok(Math.abs(a - b) <= tol, `${a} != ${b}`);

test('every parser returns oldest-first candles in ms with base and quote volume', () => {
  const sets = {
    bybit: parseBybit(fx('bybit-kasusdt.json')),
    kucoin: parseKucoin(fx('kucoin-kas-usdt.json')),
    gate: parseGate(fx('gate-kas_usdt.json')),
    mexc: parseMexc(fx('mexc-kasusdt.json')),
    kraken: parseKraken(fx('kraken-ohlc-kasusd.json')),
  };
  for (const [name, rows] of Object.entries(sets)) {
    assert.ok(rows.length >= 6, name);
    for (let i = 1; i < rows.length; i++) assert.ok(rows[i].t > rows[i - 1].t, `${name} sorted`);
    for (const r of rows) {
      assert.equal(r.t % 60_000, 0, `${name} minute-aligned ms`);
      assert.ok(r.close > 0.02 && r.close < 0.03, `${name} close ${r.close}`);
      assert.ok(r.base >= 0 && r.quote >= 0);
    }
  }
  // The column orders differ per venue: spot-check one row of each against the raw response.
  const b = sets.bybit.find((r) => r.t === 1794848100000);
  assert.deepEqual(b, { t: 1794848100000, close: 0.02657, base: 76830.4, quote: 2036.9514785 });
  const k = sets.kucoin.find((r) => r.t === 1794848100000);
  assert.deepEqual(k, { t: 1794848100000, close: 0.02652, base: 12767.1188, quote: 337.820710913 });
  const g = sets.gate.find((r) => r.t === 1794848100000);
  assert.deepEqual(g, { t: 1794848100000, close: 0.02654, base: 63670.94, quote: 1687.343256 });
  const m = sets.mexc.find((r) => r.t === 1794848100000);
  assert.deepEqual(m, { t: 1794848100000, close: 0.026602, base: 3610.97, quote: 95.727837 });
  const kr = sets.kraken.find((r) => r.t === 1794848100000);
  close(kr.quote, 37600.40008 * 0.02649, 1e-9); // Kraken gives vwap x volume, not the quote volume
});

test('a parser refuses an error response instead of returning a price', () => {
  assert.throws(() => parseBybit({ retCode: 10001, retMsg: 'params error' }), /params error/);
  assert.throws(() => parseKucoin({ code: '400100', msg: 'bad' }), /bad/);
  assert.throws(() => parseGate({ label: 'INVALID_CURRENCY' }), /INVALID_CURRENCY/);
  assert.throws(() => parseKraken({ error: ['EQuery:Unknown asset pair'] }), /Unknown asset pair/);
  assert.throws(() => parseMexc({ code: -1121, msg: 'Invalid symbol.' }), /Invalid symbol/);
  assert.throws(() => parseBybit({ retCode: 0, result: { list: [['1', '1', '1', '1', 'x', '1', '1']] } }), /bad close/);
});

test('stablecoin mids: Kraken renames USDTUSD to USDTZUSD; Coinbase mid', () => {
  const t = fx('kraken-ticker-stables.json');
  close(parseKrakenMid(t, 'USDTUSD'), (0.999 + 0.99899) / 2);
  close(parseKrakenMid(t, 'USDCUSD'), (0.9998 + 0.9997) / 2);
  assert.throws(() => parseKrakenMid(t, 'DAIUSD'), /no ticker/);
  close(parseCoinbaseMid(fx('coinbase-usdt-usd.json')), (0.999 + 0.99899) / 2);
});

test('the minute in progress is dropped and the window is exactly the last 5 closed minutes', () => {
  const rows = closedWindow(parseBybit(fx('bybit-kasusdt.json')), NOW, 5);
  assert.deepEqual(rows.map((r) => r.t / 1000), [1794847860, 1794847920, 1794847980, 1794848040, 1794848100]);
  // Kraken was read a minute later and has 16:57 too. At NOW both 16:56 and 16:57 are excluded.
  const kr = closedWindow(parseKraken(fx('kraken-ohlc-kasusd.json')), NOW, 5);
  assert.equal(Math.max(...kr.map((r) => r.t)), 1794848100000);
});

test('venue VWAP = sum(quote) / sum(base) over the window, times the USD rate', () => {
  const v = venueVwap(parseBybit(fx('bybit-kasusdt.json')), 0.9995, OPTS);
  const quote = 1833.5772952 + 719.3491808 + 289.2157086 + 163.9870111 + 2036.9514785;
  const base = 69346.33 + 27215.21 + 10930.2 + 6196.83 + 76830.4;
  assert.equal(v.fresh, true);
  close(v.vwap, (quote / base) * 0.9995);
  close(v.base, base, 1e-6);
  close(v.lastClose, 0.02657 * 0.9995);
  close(v.prevClose, 0.02647 * 0.9995);
  // KuCoin has empty minutes (zero volume) in the window. They add nothing and do not break the
  // VWAP.
  const k = venueVwap(parseKucoin(fx('kucoin-kas-usdt.json')), 1, OPTS);
  close(k.vwap, (337.820710913 + 0 + 0.58664466 + 0 + 31.692) / (12767.1188 + 22.171 + 1200));
});

test('a venue is stale without a recent closed candle, without volume, or without a USD rate', () => {
  const rows = parseBybit(fx('bybit-kasusdt.json'));
  assert.match(venueVwap(rows, 1, { ...OPTS, nowMs: NOW + 4 * 60_000 }).reason, /s old/);
  assert.match(venueVwap(rows.map((r) => ({ ...r, base: 0, quote: 0 })), 1, OPTS).reason, /no volume/);
  assert.match(venueVwap(rows, null, OPTS).reason, /no USD rate/);
  assert.match(venueVwap([], 1, OPTS).reason, /no closed candle/);
});

test('median', () => {
  assert.equal(median([3, 1, 2]), 2);
  assert.equal(median([4, 1, 3, 2]), 2.5);
  assert.throws(() => median([]));
});

test('combine: median of the fresh venues, outliers past 1% dropped, at least minVenues', () => {
  const v = (p) => ({ fresh: true, vwap: p });
  const all = { bybit: v(0.02660), kucoin: v(0.02662), gate: v(0.02664), kraken: v(0.02700), mexc: { fresh: false, reason: 'timeout' } };
  const c = combine(all, { minVenues: 2, maxDeviation: 0.01 });
  assert.equal(c.ok, true);
  assert.deepEqual(c.used, ['bybit', 'kucoin', 'gate']);
  close(c.kasUsd, 0.02662);
  assert.deepEqual(c.dropped.map((d) => d.name).sort(), ['kraken', 'mexc']);
  assert.match(c.dropped.find((d) => d.name === 'kraken').reason, /% from the median/);

  // One fresh venue is not enough.
  const one = combine({ bybit: v(0.0266), kucoin: { fresh: false, reason: 'HTTP 503' } }, { minVenues: 2, maxDeviation: 0.01 });
  assert.equal(one.ok, false);
  assert.match(one.reason, /1 fresh venue/);
  // Two fresh venues far apart: the median sits between them and both are more than 1% off it.
  const apart = combine({ a: v(0.026), b: v(0.028) }, { minVenues: 2, maxDeviation: 0.01 });
  assert.equal(apart.ok, false);
});

test('fair value of a USD-pegged token in KAS', () => {
  close(fairValueKas(0.99975, 0.02662), 0.99975 / 0.02662);
  assert.throws(() => fairValueKas(1, 0));
});

test('one-minute jump: the median candle move and the reference against a minute ago', () => {
  const venues = { a: { fresh: true, lastClose: 1.02, prevClose: 1.0 }, b: { fresh: true, lastClose: 1.0, prevClose: 1.0 }, c: { fresh: true, lastClose: 1.03, prevClose: 1.0 } };
  close(oneMinuteJump(venues, ['a', 'b', 'c'], 1, [], 0), 0.02);
  const history = [{ at: 0, kasUsd: 1.0 }, { at: 30_000, kasUsd: 1.01 }];
  close(oneMinuteJump({}, [], 1.05, history, 60_000), 0.05);
  // While cycles back off, the newest reading at least a minute old counts whatever its age (T-5):
  // a 5 % drift over 200 s is still seen.
  close(oneMinuteJump({}, [], 1.05, [{ at: 0, kasUsd: 1 }], 200_000), 0.05);
  // A reading under a minute old is not one to compare with.
  assert.equal(oneMinuteJump({}, [], 1.05, [{ at: 0, kasUsd: 1 }], 50_000), 0);
});

test('stable rate: live while fresh, then the fallback, else none', () => {
  assert.deepEqual(stableRate({ value: 0.9998, at: 0, source: 'kraken' }, 100_000, 300, 1), { value: 0.9998, source: 'kraken' });
  assert.deepEqual(stableRate({ value: 0.9998, at: 0, source: 'kraken' }, 400_000, 300, 1), { value: 1, source: 'fallback' });
  assert.deepEqual(stableRate(null, 0, 300, null), { value: null, source: 'none' });
});

test('two venues must agree within pairAgreement: one venue 1.9 % off idles the bot (sec-ops MM-1)', () => {
  const R = { minVenues: 2, maxDeviation: 0.01, pairAgreement: 0.00125 };
  const v = (vwap) => ({ fresh: true, vwap });
  const off = combine({ bybit: { fresh: false }, gate: { fresh: false }, kucoin: v(0.025), mexc: v(0.025 * 1.019) }, R);
  assert.equal(off.ok, false);
  assert.match(off.reason, /only 2 venues and they differ by 1\.90%/);
  assert.equal(combine({ kucoin: v(0.025), mexc: v(0.025 * 1.001) }, R).ok, true, 'two that agree');
  // Three venues: one bad venue cannot move the median past the others.
  const three = combine({ a: v(0.025), b: v(0.025), c: v(0.025 * 1.009) }, R);
  assert.equal(three.ok, true);
  close(three.kasUsd, 0.025);
});

test('USDT/USD: both sources asked; a reading far from 1 or two that disagree give no rate (sec-ops MM-2)', () => {
  const o = { band: 0.01, agreement: 0.002 };
  assert.deepEqual(usdtRate({ kraken: 1.015, coinbase: 1.0 }, o), { value: 1.0, source: 'coinbase' }, 'kraken 1.5 % off is refused');
  assert.equal(usdtRate({ kraken: 1.004, coinbase: 0.999 }, o).value, null, 'two that differ by 0.5 %');
  assert.match(usdtRate({ kraken: 1.004, coinbase: 0.999 }, o).reason, /differ by 0\.50%/);
  close(usdtRate({ kraken: 1.0005, coinbase: 0.9995 }, o).value, 1.0);
  assert.equal(usdtRate({ kraken: 1.015, coinbase: null }, o).value, null, 'one reading alone, out of the band');
  assert.equal(usdtRate({ kraken: 0.999, coinbase: null }, o).value, 0.999);
  assert.equal(usdtRate({}, o).reason, 'no reading');
});

test('USDT/USD: a lone source is held to the agreement bound, below half the spread (E-L5)', () => {
  const o = { band: 0.01, agreement: 0.002 };
  // Inside the 1 % band but 0.7 % off: alone it would move every USDT venue past a 0.5 % best quote.
  assert.equal(usdtRate({ kraken: 0.993, coinbase: null }, o).value, null);
  assert.match(usdtRate({ kraken: null, coinbase: 1.007 }, o).reason, /only coinbase 1.00700, further than 0.002 from 1/);
  assert.equal(usdtRate({ kraken: 1.015, coinbase: 0.993 }, o).value, null, 'the other refused: the one left stands alone');
  close(usdtRate({ kraken: 0.993, coinbase: 0.9935 }, o).value, 0.99325); // two agreeing sources may carry a real move
  assert.equal(usdtRate({ kraken: 1.0015, coinbase: null }, o).value, 1.0015);
});
