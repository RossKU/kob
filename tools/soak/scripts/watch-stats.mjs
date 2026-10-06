// Read-only watch statistics of a running soak, from the indexer API and the executor logs (no secrets read).
//
//   node scripts/watch-stats.mjs --since 2026-10-06T10:25:55Z [--api http://127.0.0.1:8091] [--logs run/logs] [--json]
//
// * pair fills (KobPair, KobCondPair, KobIfdPair: token A for token B): per pair and contract, how they were filled (`counterparty`: the route
//   through the two KAS books, netting against opposite pair orders, or the matcher's inventory), the volume in base units of A and B, and that
//   every one is volume only (`price` null, `price_source` none: a pair fill never sets a price);
// * pair triggers: the arms and trails of pair stops, per evidence mode (0 two KAS-book fills, 1 a resting pair order);
// * matcher income per transaction: the `final txid=.. kind=.. profit=..` lines of every executor log since `--since`, joined with the
//   transactions that hold a pair fill (single = one pair fill, multi = two or more) and the rest (plain batches).
import fs from 'node:fs';
import path from 'node:path';

const arg = (k, d) => { const i = process.argv.indexOf(`--${k}`); return i >= 0 ? process.argv[i + 1] : d; };
const api = arg('api', 'http://127.0.0.1:8091');
const logs = arg('logs', path.join(path.dirname(new URL(import.meta.url).pathname.replace(/^\/([A-Za-z]:)/, '$1')), '..', 'run', 'logs'));
const sinceIso = arg('since', null);
if (!sinceIso) { console.error('--since <ISO time> required'); process.exit(2); }
const sinceMs = Date.parse(sinceIso);
const asJson = process.argv.includes('--json');

const get = async (p) => { const r = await fetch(api + p); if (!r.ok) throw new Error(`${p}: ${r.status}`); return r.json(); };
const median = (a) => { if (!a.length) return null; const s = [...a].sort((x, y) => x - y); const m = s.length >> 1; return s.length % 2 ? s[m] : (s[m - 1] + s[m]) / 2; };
const mean = (a) => (a.length ? a.reduce((x, y) => x + y, 0) / a.length : null);

// fills since the start (newest first)
const fills = [];
let cursor;
for (;;) {
  const j = await get(`/v1/fills?limit=500${cursor ? `&before=${cursor}` : ''}`);
  let stop = false;
  for (const f of j.items) { if (f.ts < sinceMs) { stop = true; break; } fills.push(f); }
  if (stop || !j.next_cursor || !j.items.length) break;
  cursor = j.next_cursor;
}
const pairFills = fills.filter((f) => f.detail && f.detail.pair);
const priced = pairFills.filter((f) => f.price != null || f.detail.pair.price_source !== 'none');
const byPair = new Map();
for (const f of pairFills) {
  const d = f.detail.pair;
  const k = `${d.base.slice(0, 8)}/${d.quote.slice(0, 8)}`;
  const row = byPair.get(k) ?? { fills: 0, amountA: 0n, amountB: 0n, counterparty: {}, contract: {} };
  row.fills++;
  row.amountA += BigInt(d.amount_a ?? 0);
  row.amountB += BigInt(d.amount_b ?? 0);
  row.counterparty[d.counterparty] = (row.counterparty[d.counterparty] ?? 0) + 1;
  row.contract[f.detail.contract ?? '?'] = (row.contract[f.detail.contract ?? '?'] ?? 0) + 1;
  byPair.set(k, row);
}

// arms / trails of pair stops (their events carry `detail.evidence`)
const triggers = { mode0: 0, mode1: 0, other: 0 };
const stops = new Set(pairFills.map((f) => f.covenant_id));
for (const id of stops) {
  const ev = await get(`/v1/orders/${id}/events?limit=200`);
  for (const e of ev.items ?? []) {
    if ((e.kind !== 'arm' && e.kind !== 'trail') || e.ts < sinceMs) continue;
    const m = e.detail?.evidence?.mode;
    if (m === 0) triggers.mode0++;
    else if (m === 1) triggers.mode1++;
    else triggers.other++;
  }
}

// matcher income from the executor logs
const profits = new Map(); // txid -> { kind, profit, exec }
if (fs.existsSync(logs)) {
  for (const name of fs.readdirSync(logs).filter((n) => /^exec-[ab]\.log$/.test(n))) {
    for (const line of fs.readFileSync(path.join(logs, name), 'utf8').split('\n')) {
      const m = /^(\S+)\s+\S+\s+\S+ final txid=([0-9a-f]{64}) kind=(\w+) profit=(-?\d+)/.exec(line);
      if (!m || Date.parse(m[1]) < sinceMs) continue;
      profits.set(m[2], { kind: m[3], profit: Number(m[4]) / 1e8, exec: name.slice(0, 6) });
    }
  }
}
const pairByTx = new Map();
for (const f of pairFills) pairByTx.set(f.txid, (pairByTx.get(f.txid) ?? 0) + 1);
const single = [], multi = [], plain = [];
for (const [txid, p] of profits) {
  if (p.kind !== 'match') continue;
  const k = pairByTx.get(txid) ?? 0;
  (k === 0 ? plain : k === 1 ? single : multi).push(p.profit);
}
const all = [...single, ...multi];

const fmt = (x, d = 2) => (x == null ? 'n/a' : x.toFixed(d));
const out = {
  since: sinceIso,
  pairFills: pairFills.length,
  pricedPairFills: priced.length,
  pairs: Object.fromEntries([...byPair].map(([k, v]) => [k, { ...v, amountA: v.amountA.toString(), amountB: v.amountB.toString() }])),
  triggers,
  income: {
    pairTxs: all.length, avgKas: mean(all), medianKas: median(all),
    single: { n: single.length, avg: mean(single) }, multi: { n: multi.length, avg: mean(multi) },
    plain: { n: plain.length, avg: mean(plain) },
    kinds: Object.fromEntries([...profits.values()].reduce((m, p) => m.set(p.kind, (m.get(p.kind) ?? 0) + 1), new Map())),
  },
};
if (asJson) { console.log(JSON.stringify(out, null, 1)); process.exit(0); }
console.log(`since ${sinceIso}: ${pairFills.length} pair fills in ${pairByTx.size} txs (${priced.length} with a price: must be 0)`);
for (const [k, v] of byPair) console.log(`  ${k}: ${v.fills} fills, A ${v.amountA} B ${v.amountB} base units; counterparty ${JSON.stringify(v.counterparty)}; contracts ${JSON.stringify(v.contract)}`);
console.log(`pair triggers: evidence mode 0 (two KAS-book fills) ${triggers.mode0}, mode 1 (a resting pair order) ${triggers.mode1}, other ${triggers.other}`);
console.log(`matcher income per pair tx: avg ${fmt(out.income.avgKas)} KAS median ${fmt(out.income.medianKas)} (n ${out.income.pairTxs}); single ${fmt(out.income.single.avg)} (n ${single.length}), multi ${fmt(out.income.multi.avg)} (n ${multi.length}); plain match ${fmt(out.income.plain.avg, 3)} (n ${plain.length}); final kinds ${JSON.stringify(out.income.kinds)}`);
