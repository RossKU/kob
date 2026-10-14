// Imports the chart history of older soak runs into one STOPPED executor's database (src/history-carry.ts `importHistory`): the
// KAS-book fills (and pair fills) of each source database, by ticker onto the current covenant ids, each source only before the
// next run started (where runs overlap, the run live at the time wins; the last source ends where this database's own chain
// events start). Old ids are named by ticker from `--old-registry` files and from this database's `carried_history` records.
// Idempotent per label. Then `scripts/chart-history.mjs` must report every chart from its first trade.
//
//   node scripts/import-history.mjs --db run/exec-b/data/index.sqlite3 --source m5=<index.sqlite3> --source m9a=<...> \
//        [--old-registry <tokens.json> ...] [--registry run/registry/tokens.json] [--floor run/state/chart-floor.json] [--dry-run]
//
// The first trade per ticker after the import is recorded in the floor file (the earliest one wins): `chart-history.mjs` reports a
// database whose history no longer reaches it (a replay or a rebuilt database drops the import: run this again). `--record-floor`
// writes it from the database alone (no source, read-only).
//
// Hold the executor first (scripts/hold-child.ps1 -Name exec-b): the script refuses a database whose writer is alive.
import { existsSync, mkdirSync, readFileSync, writeFileSync } from 'node:fs';
import { dirname, join, resolve } from 'node:path';
import { DatabaseSync } from 'node:sqlite';
import { fileURLToPath } from 'node:url';
import { firstFillTs, historyFloors, importHistory, importPairs } from '../src/history-carry.ts';

const argv = process.argv.slice(2);
const all = (k) => argv.flatMap((a, i) => (a === `--${k}` ? [argv[i + 1]] : []));
const one = (k, d) => all(k)[0] ?? d;
const dbPath = resolve(one('db', ''));
if (!existsSync(dbPath)) throw new Error('--db <index.sqlite3> of a stopped executor');
const registry = JSON.parse(readFileSync(resolve(one('registry', fileURLToPath(new URL('../run/registry/tokens.json', import.meta.url)))), 'utf8'));
const recordOnly = argv.includes('--record-floor'); // only (re)write the floor from this database (read-only, the executor may run)
const dry = argv.includes('--dry-run') || recordOnly;
const iso = (t) => (t == null ? '-' : new Date(t).toISOString().slice(0, 16) + 'Z');

// the writer must be down: the owner file names its pid
const owner = join(dirname(dbPath), 'kob-writer.owner');
if (existsSync(owner)) {
  const pid = Number(/pid (\d+)/.exec(readFileSync(owner, 'utf8'))?.[1]);
  let alive = false;
  try { if (pid) { process.kill(pid, 0); alive = true; } } catch { alive = false; }
  if (alive && !dry) throw new Error(`the executor writing ${dbPath} is running (pid ${pid}): hold it first (scripts/hold-child.ps1)`);
}

const dst = new DatabaseSync(dbPath, { readOnly: dry });
// old id -> ticker: the older registries, then the carries this database recorded
const old = new Map();
for (const f of all('old-registry')) for (const t of JSON.parse(readFileSync(resolve(f), 'utf8')).tokens ?? []) old.set(String(t.covenant_id).toLowerCase(), t.ticker);
for (const r of dst.prepare("SELECT k, v FROM meta WHERE k LIKE 'carried_history:%'").all()) old.set(r.k.slice('carried_history:'.length), JSON.parse(r.v).ticker);
// this run's own start: its first event of a chain block (imported rows have block 0)
const ownStart = Number(dst.prepare('SELECT MIN(ts) AS t FROM order_events WHERE block_seq > 0 AND ts > 0').get().t);

const sources = (recordOnly ? [] : all('source')).map((s) => {
  const i = s.indexOf('=');
  if (i < 1) throw new Error(`--source <label>=<index.sqlite3>: ${s}`);
  const db = new DatabaseSync(resolve(s.slice(i + 1)), { readOnly: true });
  return { label: s.slice(0, i), path: s.slice(i + 1), db, start: firstFillTs(db) };
}).filter((s) => s.start != null).sort((a, b) => a.start - b.start);

let failed = false;
for (const [k, s] of sources.entries()) {
  const until = Math.min(sources[k + 1]?.start ?? ownStart, ownStart);
  const pairs = importPairs(s.db, old, registry);
  console.log(`${s.label}: ${s.path}\n  live ${iso(s.start)} .. ${iso(until)}  ${pairs.map((p) => `${p.ticker} ${p.from.slice(0, 8)}->${p.to.slice(0, 8)}`).join(', ') || 'no mapped token'}`);
  if (dry || !pairs.length) continue;
  try {
    const r = importHistory(dst, s.db, pairs, { label: s.label, until });
    if (r.already) console.log('  already imported (meta history_import:' + s.label + ')');
    for (const x of r.results) console.log(`  ${x.ticker.padEnd(5)} fills ${x.fills} (${iso(x.first)} .. ${iso(x.last)})  duplicates ${x.duplicates}  later ${x.later}  pair fills ${x.pairFills}`);
  } catch (e) {
    failed = true;
    console.log(`  FAILED (nothing of this source written): ${e.message}`);
  }
}
for (const s of sources) s.db.close();
if (!dry || recordOnly) {
  const floorPath = resolve(one('floor', fileURLToPath(new URL('../run/state/chart-floor.json', import.meta.url))));
  const floor = existsSync(floorPath) ? JSON.parse(readFileSync(floorPath, 'utf8')) : {};
  for (const f of historyFloors(dst, registry)) {
    if (f.firstTrade != null && !(floor[f.ticker] <= f.firstTrade)) floor[f.ticker] = f.firstTrade;
  }
  mkdirSync(dirname(floorPath), { recursive: true });
  writeFileSync(floorPath, JSON.stringify(floor, null, 1) + '\n');
  console.log(`floor ${floorPath}: ${Object.entries(floor).map(([t, v]) => `${t} ${iso(v)}`).join(', ')}`);
}
dst.close();
process.exit(failed ? 1 : 0);
