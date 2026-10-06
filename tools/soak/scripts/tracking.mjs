// Tracking error of the soak's USD prices against the real markets the bots follow (Binance USD-M KASUSDT / ETHUSDT / BTCUSDT).
//
//   node scripts/tracking.mjs [--config run/config.json] [--minutes 60 | --from <ISO> [--to <ISO>]] [--indexer http://127.0.0.1:8091] [--json]
//
// Soak side, from the indexer's trades and 1m candles (block time), converted the way the web app does (web/src/ui/market/usd-model.ts):
//   KAS/USD = 1 / (KAS per TUSD)          ETH/USD = KAS per TETH / KAS per TUSD          BTC/USD = KAS per TBTC / KAS per TUSD
// Reference side: every good Binance poll of the bots (run/state/ref-prices.jsonl, price.ts RefRecorder; exchange time, ~3 s apart).
//
// Two views per market:
//   trades   every trade of the market's own book in the window (TUSD trades for KAS/USD, TETH / TBTC trades for ETH / BTC): its price,
//            divided by the newest TUSD trade at or before it (the time-aligned USD rate; skipped when that is older than 10 min),
//            against the newest Binance price at or before (trade time - lag).
//   candles  the app's chart: 1m buckets, each leg's close (carried across buckets without trades), the ratio of the closes, against the
//            last Binance price of the same bucket; buckets where no leg traded are left out.
// Deviation = soak / Binance - 1 in basis points. Lag: the shift (0 to 600 s, step 5 s) of the reference that minimises the median |deviation|
// of the trades view (a positive lag: the soak follows Binance that much later).
import { existsSync, readFileSync } from 'node:fs';
import { dirname, isAbsolute, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

const HERE = dirname(fileURLToPath(import.meta.url));
const argv = process.argv.slice(2);
const opt = (n, d) => (argv.includes(n) ? argv[argv.indexOf(n) + 1] : d);
const configPath = resolve(opt('--config', join(HERE, '..', 'run', 'config.json')));
const cfg = JSON.parse(readFileSync(configPath, 'utf8'));
const RUN = isAbsolute(cfg.runDir) ? cfg.runDir : resolve(dirname(configPath), cfg.runDir);
const API = (opt('--indexer', cfg.indexerUrl) ?? 'http://127.0.0.1:8091').replace(/\/$/, '');
const now = Date.now();
const to = opt('--to') ? Date.parse(opt('--to')) : now;
const from = opt('--from') ? Date.parse(opt('--from')) : to - Number(opt('--minutes', '60')) * 60_000;
if (!Number.isFinite(from) || !Number.isFinite(to) || from >= to) throw new Error('bad window (--minutes N | --from ISO [--to ISO])');
const MAX_RATE_AGE = 10 * 60_000;

const st = JSON.parse(readFileSync(join(RUN, 'state.json'), 'utf8'));
const symOf = (url) => {
  try {
    return new URL(url).searchParams.get('symbol');
  } catch {
    return null;
  }
};
const usd = st.token;
const markets = [
  { name: 'KAS/USD', symbol: symOf(cfg.price.url) ?? 'KASUSDT', base: null },
  ...['token2', 'token3']
    .filter((s) => st[s] && cfg[s])
    .map((s) => ({ name: `${st[s].ticker.replace(/^T/, '')}/USD`, symbol: symOf(cfg[s].priceUrl), base: st[s] })),
];

// ------------------------------------------------------------------------------------------------ reference record

const ref = new Map(); // symbol -> { t: number[], p: number[] }
{
  const file = join(RUN, 'state', 'ref-prices.jsonl');
  if (!existsSync(file)) throw new Error(`no reference record yet (${file}: written by the bots since the TBTC roll-in)`);
  for (const l of readFileSync(file, 'utf8').split('\n')) {
    if (!l) continue;
    try {
      const { t, s, p } = JSON.parse(l);
      if (t < from - 15 * 60_000 || t > to + 60_000) continue;
      let a = ref.get(s);
      if (!a) ref.set(s, (a = { t: [], p: [] }));
      if (a.t.length && t < a.t[a.t.length - 1]) continue;
      a.t.push(t);
      a.p.push(p);
    } catch {
      /* torn line */
    }
  }
}
/** index of the last element <= x, or -1 */
const floorIdx = (arr, x) => {
  let lo = 0;
  let hi = arr.length;
  while (lo < hi) {
    const m = (lo + hi) >> 1;
    if (arr[m] <= x) lo = m + 1;
    else hi = m;
  }
  return lo - 1;
};
const refAt = (symbol, t, maxAge = 30_000) => {
  const a = ref.get(symbol);
  if (!a) return null;
  const i = floorIdx(a.t, t);
  return i >= 0 && t - a.t[i] <= maxAge ? a.p[i] : null;
};

// ------------------------------------------------------------------------------------------------ indexer

async function get(path) {
  const r = await fetch(API + path, { signal: AbortSignal.timeout(20_000) });
  if (!r.ok) throw new Error(`${path}: HTTP ${r.status}`);
  return r.json();
}

/**
 * trades of a token in [from - 10 min, to], ascending: { t, kas } with kas = KAS per WHOLE token. Protocol v3: `price_basis` is the token's
 * standard scale (10^decimals, sompi per whole token) and the response carries `decimals`; the formula below holds for any basis.
 */
async function trades(tok) {
  const out = [];
  let before;
  for (let page = 0; page < 200; page++) {
    const v = await get(`/v1/trades/${tok.covenantId}?limit=500${before ? `&before=${before}` : ''}`);
    const basis = Number(v.price_basis);
    for (const it of v.items) {
      if (it.ts > to) continue;
      out.push({ t: it.ts, kas: (Number(it.price) / basis) * 10 ** (tok.decimals - 8) });
    }
    const oldest = v.items.at(-1);
    if (!v.next_cursor || !oldest || oldest.ts < from - MAX_RATE_AGE) break;
    before = v.next_cursor;
  }
  return out.filter((x) => x.t >= from - MAX_RATE_AGE).sort((a, b) => a.t - b.t);
}

/** 1m candles of a token in the window: Map bucket -> { c (KAS per whole token), traded } (carried across empty buckets) */
async function candles(tok) {
  const v = await get(`/v1/candles/${tok.covenantId}?interval=1m&from=${from - MAX_RATE_AGE}&to=${to}&limit=2000`);
  const basis = Number(v.price_basis);
  const real = new Map(v.items.map((c) => [c.t, (Number(c.c) / basis) * 10 ** (tok.decimals - 8)]));
  const out = new Map();
  let last = null;
  for (let t = Math.floor((from - MAX_RATE_AGE) / 60_000) * 60_000; t < to; t += 60_000) {
    if (real.has(t)) last = real.get(t);
    if (last !== null) out.set(t, { c: last, traded: real.has(t) });
  }
  return out;
}

// ------------------------------------------------------------------------------------------------ statistics

const q = (sorted, p) => (sorted.length ? sorted[Math.min(sorted.length - 1, Math.max(0, Math.ceil(p * sorted.length) - 1))] : null);
function summary(devs) {
  const abs = devs.map(Math.abs).sort((a, b) => a - b);
  const mean = devs.length ? devs.reduce((s, d) => s + d, 0) / devs.length : null;
  return { n: devs.length, medianBps: q(abs, 0.5), p95Bps: q(abs, 0.95), maxBps: abs.at(-1) ?? null, biasBps: mean };
}
const r1 = (x) => (x === null || x === undefined ? null : Math.round(x * 10) / 10);
const fmt = (s) => `n ${String(s.n).padStart(5)}  median ${String(r1(s.medianBps)).padStart(6)}  p95 ${String(r1(s.p95Bps)).padStart(6)}  max ${String(r1(s.maxBps)).padStart(7)}  bias ${String(r1(s.biasBps)).padStart(6)} bps`;

const usdTrades = await trades(usd);
const usdCandles = await candles(usd);
const report = { window: { from: new Date(from).toISOString(), to: new Date(to).toISOString(), minutes: Math.round((to - from) / 60_000) }, method: 'web usd-model (ratio of KAS prices through TUSD), Binance USD-M reference from the bots', markets: [] };

for (const m of markets) {
  if (!ref.get(m.symbol)?.t.length) {
    report.markets.push({ market: m.name, symbol: m.symbol, error: 'no reference samples in the window' });
    continue;
  }
  // trades view: the soak USD price at each trade of the market's own book
  const own = m.base ? await trades(m.base) : usdTrades;
  const usdT = usdTrades.map((x) => x.t);
  const points = [];
  for (const x of own) {
    if (x.t < from) continue;
    let price;
    if (!m.base) price = 1 / x.kas;
    else {
      const i = floorIdx(usdT, x.t);
      if (i < 0 || x.t - usdT[i] > MAX_RATE_AGE) continue;
      price = x.kas / usdTrades[i].kas;
    }
    points.push({ t: x.t, price });
  }
  const devsAt = (lag) => points.flatMap((p) => {
    const r = refAt(m.symbol, p.t - lag);
    return r ? [(p.price / r - 1) * 10_000] : [];
  });
  let best = { lag: 0, s: summary(devsAt(0)) };
  for (let lag = 5_000; lag <= 600_000; lag += 5_000) {
    const s = summary(devsAt(lag));
    if (s.n >= Math.max(5, best.s.n * 0.8) && s.medianBps !== null && (best.s.medianBps === null || s.medianBps < best.s.medianBps)) best = { lag, s };
  }
  // candles view: the app's chart (1m, ratio of the closes)
  const own1m = m.base ? await candles(m.base) : null;
  const cdevs = [];
  for (const [t, u] of usdCandles) {
    if (t < from) continue;
    const b = own1m ? own1m.get(t) : { c: 1, traded: false };
    if (!b || !(u.traded || b.traded)) continue;
    const a = ref.get(m.symbol);
    const i = floorIdx(a.t, t + 59_999);
    if (i < 0 || a.t[i] < t) continue;
    cdevs.push(((b.c / u.c) / a.p[i] - 1) * 10_000);
  }
  report.markets.push({
    market: m.name,
    symbol: m.symbol,
    last: { soak: points.at(-1)?.price ?? null, binance: refAt(m.symbol, to, 120_000) },
    trades: summary(devsAt(0)),
    lagMs: best.lag,
    tradesAtLag: best.s,
    candles1m: summary(cdevs),
  });
}

if (argv.includes('--json')) process.stdout.write(JSON.stringify(report, null, 1) + '\n');
else {
  console.log(`tracking vs Binance  ${report.window.from} .. ${report.window.to} (${report.window.minutes} min)`);
  for (const m of report.markets) {
    if (m.error) {
      console.log(`${m.market.padEnd(8)} ${m.symbol}: ${m.error}`);
      continue;
    }
    console.log(`${m.market.padEnd(8)} ${m.symbol.padEnd(8)} last soak ${m.last.soak?.toPrecision(6)} binance ${m.last.binance?.toPrecision(6)}`);
    console.log(`  trades       ${fmt(m.trades)}`);
    console.log(`  trades lag ${String(m.lagMs / 1000).padStart(3)}s ${fmt(m.tradesAtLag)}`);
    console.log(`  candles 1m   ${fmt(m.candles1m)}`);
  }
}
