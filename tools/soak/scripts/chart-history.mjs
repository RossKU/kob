// Read-only: do the charts keep their whole history? For every executor of run/config.json, per token of the registry (KAS
// book) and per pair market with pair fills, the first candle of each interval (1d, 1h, 5m, 1m) from that executor's API must be
// the bucket of the first trade in its database, and no fill may be left under an old id carried to a current one (a replay
// of the record log undoes `carry-history`). Exit 1 on any problem. Run it before and after every redeploy (README "Redeploy").
//
//   node scripts/chart-history.mjs [--config run/config.json] [--registry run/registry/tokens.json] [--floor run/state/chart-floor.json]
//        [--only exec-b] [--json]
//
// The floor file (written by scripts/import-history.mjs) holds the earliest first trade per ticker any import reached: a database
// whose first trade is later lost the imported history (a replay or a rebuilt database: import again).
import { existsSync, readFileSync } from 'node:fs';
import { dirname, resolve } from 'node:path';
import { DatabaseSync } from 'node:sqlite';
import { fileURLToPath } from 'node:url';
import { CHART_INTERVALS, chartProblems, historyFloors, pairMarkets } from '../src/history-carry.ts';

const arg = (k, d) => { const i = process.argv.indexOf(`--${k}`); return i >= 0 ? process.argv[i + 1] : d; };
const configPath = resolve(arg('config', fileURLToPath(new URL('../run/config.json', import.meta.url))));
const config = JSON.parse(readFileSync(configPath, 'utf8'));
const runDir = resolve(dirname(configPath), config.runDir ?? '.'); // as loadConfig resolves it
const registry = JSON.parse(readFileSync(resolve(arg('registry', resolve(runDir, 'registry/tokens.json'))), 'utf8'));
const only = arg('only', null);
const floorPath = resolve(arg('floor', resolve(runDir, 'state/chart-floor.json')));
const floor = existsSync(floorPath) ? JSON.parse(readFileSync(floorPath, 'utf8')) : {};
const LIMIT = 1500; // the API's most candles per request

async function firstCandles(api, path, firstTrade) {
  const out = {};
  for (const [name, ms] of CHART_INTERVALS) {
    if (firstTrade == null) { out[name] = null; continue; }
    const from = Math.floor(firstTrade / ms) * ms;
    const r = await fetch(`${api}${path}?interval=${name}&from=${from}&to=${from + ms * LIMIT}&limit=${LIMIT}`);
    if (!r.ok) throw new Error(`${path} ${name}: HTTP ${r.status}`);
    const j = await r.json();
    out[name] = j.items?.[0]?.t ?? null;
  }
  return out;
}

const report = [];
let problems = 0;
for (const e of config.executors) {
  if (only && e.name !== only) continue;
  const api = `http://${e.api}`;
  const db = new DatabaseSync(resolve(runDir, e.name, 'data/index.sqlite3'), { readOnly: true });
  const floors = historyFloors(db, registry);
  const pairs = pairMarkets(db, registry);
  db.close();
  const r = { executor: e.name, api, markets: [], problems: [] };
  for (const f of floors) {
    if (floor[f.ticker] != null && (f.firstTrade == null || f.firstTrade > floor[f.ticker])) {
      r.problems.push(`${f.ticker}: the database starts at ${f.firstTrade == null ? 'none' : new Date(f.firstTrade).toISOString()}, the recorded floor is ${new Date(floor[f.ticker]).toISOString()} (imported history lost: run import-history again)`);
    }
    if (f.leftover > 0) r.problems.push(`${f.ticker}: ${f.leftover} fills still under an id carried to ${f.id} (a replay undid the carry: run carry-history again)`);
    const first = await firstCandles(api, `/v1/candles/${f.id}`, f.firstTrade);
    r.markets.push({ market: f.ticker, firstTrade: f.firstTrade, first });
    r.problems.push(...chartProblems(f.ticker, f.firstTrade, first));
  }
  const byId = new Map(floors.map((f) => [f.id, f]));
  for (const [a, b] of pairs) {
    const fa = byId.get(a).firstTrade, fb = byId.get(b).firstTrade;
    const firstTrade = fa == null || fb == null ? null : Math.max(fa, fb);
    const name = `${byId.get(a).ticker}/${byId.get(b).ticker}`;
    const first = await firstCandles(api, `/v1/pairs/${a}/${b}/candles`, firstTrade);
    r.markets.push({ market: name, firstTrade, first });
    r.problems.push(...chartProblems(name, firstTrade, first));
  }
  problems += r.problems.length;
  report.push(r);
}

if (process.argv.includes('--json')) console.log(JSON.stringify(report, null, 1));
else {
  const iso = (t) => (t == null ? 'none' : new Date(t).toISOString().slice(0, 16) + 'Z');
  for (const r of report) {
    console.log(`${r.executor} (${r.api})`);
    for (const m of r.markets) console.log(`  ${m.market.padEnd(11)} first trade ${iso(m.firstTrade)}  first candle ${CHART_INTERVALS.map(([n]) => `${n} ${iso(m.first[n])}`).join('  ')}`);
    for (const p of r.problems) console.log(`  PROBLEM ${p}`);
  }
  console.log(problems ? `${problems} problem(s)` : 'every chart starts at its first trade');
}
process.exit(problems ? 1 : 0);
