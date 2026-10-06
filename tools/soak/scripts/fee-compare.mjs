// Read-only: compares the bots' transaction log (run/state/txs.jsonl) between two windows: fees per hour, transactions per hour, and
// size / fee / payload per action, so a redeploy's cost change can be read off.
//   node scripts/fee-compare.mjs --before <ISO> <ISO> --after <ISO> [<ISO>] [--run run]
import { readFileSync } from 'node:fs';
import { join } from 'node:path';
const a = process.argv.slice(2);
const at = (k) => { const i = a.indexOf(k); return i < 0 ? [] : a.slice(i + 1).filter((x, j, arr) => !x.startsWith('--') && arr.slice(0, j).every((y) => !y.startsWith('--'))); };
const run = at('--run')[0] ?? join(import.meta.dirname, '..', 'run');
const [b0, b1] = at('--before').map(Date.parse);
const [a0, a1 = Date.now()] = at('--after').map(Date.parse);
const L = readFileSync(join(run, 'state', 'txs.jsonl'), 'utf8').trim().split('\n').map((l) => JSON.parse(l));
const kind = (w) => w.replace(/@TETH$/, '').replace(/:[A-Z]+>[A-Z]+$/, '').replace(/^cli_cancel:.*/, 'cancel').replace(/^cancel:.*/, 'cancel');
function win(t0, t1) {
  const xs = L.filter((t) => t.ts >= t0 && t.ts < t1);
  const h = (t1 - t0) / 3.6e6;
  const by = {};
  for (const t of xs) { const k = kind(t.what); const e = (by[k] ??= { n: 0, fee: 0, size: 0, payload: 0, np: 0 }); e.n++; e.fee += Number(t.fee); e.size += t.size; if (t.payload !== undefined) { e.payload += t.payload; e.np++; } }
  return { hours: h, txs: xs.length, feeKas: xs.reduce((s, t) => s + Number(t.fee), 0) / 1e8, bytes: xs.reduce((s, t) => s + t.size, 0), by };
}
const B = win(b0, b1), A = win(a0, a1);
const f = (x, d = 2) => x.toFixed(d);
console.log(`before ${new Date(b0).toISOString()} .. ${new Date(b1).toISOString()} (${f(B.hours)} h): ${B.txs} txs, ${f(B.feeKas, 3)} KAS, ${f(B.feeKas / B.hours, 3)} KAS/h, ${f(B.txs / B.hours, 1)} tx/h, ${f(B.feeKas / B.txs, 5)} KAS/tx, ${Math.round(B.bytes / B.txs)} B/tx`);
console.log(`after  ${new Date(a0).toISOString()} .. ${new Date(a1).toISOString()} (${f(A.hours)} h): ${A.txs} txs, ${f(A.feeKas, 3)} KAS, ${f(A.feeKas / A.hours, 3)} KAS/h, ${f(A.txs / A.hours, 1)} tx/h, ${f(A.feeKas / A.txs, 5)} KAS/tx, ${Math.round(A.bytes / A.txs)} B/tx`);
console.log(`fee per tx ${f((A.feeKas / A.txs / (B.feeKas / B.txs) - 1) * 100, 1)} %, bytes per tx ${f((A.bytes / A.txs / (B.bytes / B.txs) - 1) * 100, 1)} %, fee per hour ${f((A.feeKas / A.hours / (B.feeKas / B.hours) - 1) * 100, 1)} %`);
// the after-window's action mix priced at the before-window's per-action fee: the cost change with the activity held constant
let mixB = 0, mixA = 0;
for (const [k, e] of Object.entries(A.by)) { const b = B.by[k]; if (!b) continue; mixB += e.n * (b.fee / b.n); mixA += e.fee; }
console.log(`same action mix (after's counts at before's per-action fee): ${f((mixA / mixB - 1) * 100, 1)} %`);
console.log('action'.padEnd(18), 'n before/h', 'n after/h', 'B before', 'B after', 'fee before', 'fee after', 'payload after');
const ks = [...new Set([...Object.keys(B.by), ...Object.keys(A.by)])].sort((x, y) => (A.by[y]?.n ?? 0) - (A.by[x]?.n ?? 0));
for (const k of ks) {
  const b = B.by[k], c = A.by[k];
  console.log(k.padEnd(18), String(b ? f(b.n / B.hours, 1) : '-').padStart(10), String(c ? f(c.n / A.hours, 1) : '-').padStart(9), String(b ? Math.round(b.size / b.n) : '-').padStart(8), String(c ? Math.round(c.size / c.n) : '-').padStart(7), String(b ? f(b.fee / b.n / 1e8, 5) : '-').padStart(10), String(c ? f(c.fee / c.n / 1e8, 5) : '-').padStart(9), String(c?.np ? Math.round(c.payload / c.np) : '-').padStart(13));
}
