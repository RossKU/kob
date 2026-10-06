// Read-only: how busy every market page is. Per KAS book (TUSD, TETH, TBTC): fills and trades per minute (`/v1/fills?token=`,
// `/v1/trades/<token>`, what the market page's recent trades show); per pair (each asset token against TUSD): pair fills per minute
// (`/v1/pairs/<A>/<TUSD>/fills`) split by counterparty, and the price_source values seen (protocol v3: always `none`).
//
//   node scripts/activity.mjs [--minutes 10] [--api http://127.0.0.1:8091] [--json]
import { readFileSync } from 'node:fs';

const arg = (k, d) => { const i = process.argv.indexOf(`--${k}`); return i >= 0 ? process.argv[i + 1] : d; };
const api = arg('api', 'http://127.0.0.1:8091');
const minutes = Number(arg('minutes', '10'));
const now = Date.now();
const from = now - minutes * 60_000;
const get = async (p) => { const r = await fetch(api + p); if (!r.ok) throw new Error(`${p}: ${r.status}`); return r.json(); };
const state = JSON.parse(readFileSync(new URL('../run/state.json', import.meta.url), 'utf8'));
const tokens = ['token', 'token2', 'token3'].filter((s) => state[s]).map((s) => ({ ticker: state[s].ticker, id: state[s].covenantId }));
const tusd = tokens[0];

/** pages a cursor endpoint (newest first) until items are older than the window */
async function window(path, cursorKey) {
  const out = [];
  let cursor;
  for (let page = 0; page < 200; page++) {
    const j = await get(`${path}${path.includes('?') ? '&' : '?'}limit=500${cursor ? `&${cursorKey}=${cursor}` : ''}`);
    const items = j.items ?? [];
    let older = false;
    for (const x of items) { if (x.ts < from) { older = true; break; } out.push(x); }
    if (older || !j.next_cursor || !items.length) break;
    cursor = j.next_cursor;
  }
  return out;
}

const res = { minutes, from: new Date(from).toISOString(), to: new Date(now).toISOString(), books: {}, pairs: {} };
for (const t of tokens) {
  const fills = await window(`/v1/fills?token=${t.id}`, 'before');
  let trades = [];
  try { trades = await window(`/v1/trades/${t.id}`, 'before'); } catch { trades = []; }
  res.books[t.ticker] = { fills: fills.length, fillsPerMin: +(fills.length / minutes).toFixed(2), trades: trades.length, tradesPerMin: +(trades.length / minutes).toFixed(2) };
}
for (const t of tokens.slice(1)) {
  const fills = await window(`/v1/pairs/${t.id}/${tusd.id}/fills`, 'before');
  const by = {}, src = {};
  for (const f of fills) { by[f.counterparty] = (by[f.counterparty] ?? 0) + 1; src[f.price_source] = (src[f.price_source] ?? 0) + 1; }
  res.pairs[`${t.ticker}/${tusd.ticker}`] = { fills: fills.length, fillsPerMin: +(fills.length / minutes).toFixed(2), counterparty: by, priceSource: src };
}
if (process.argv.includes('--json')) console.log(JSON.stringify(res));
else {
  console.log(`activity ${res.from} .. ${res.to} (${minutes} min)`);
  for (const [k, v] of Object.entries(res.books)) console.log(`  ${k.padEnd(10)} KAS book  fills ${String(v.fills).padStart(5)} (${v.fillsPerMin}/min)  trades ${String(v.trades).padStart(5)} (${v.tradesPerMin}/min)`);
  for (const [k, v] of Object.entries(res.pairs)) console.log(`  ${k.padEnd(10)} pair      fills ${String(v.fills).padStart(5)} (${v.fillsPerMin}/min)  by ${JSON.stringify(v.counterparty)}  price_source ${JSON.stringify(v.priceSource)}`);
}
