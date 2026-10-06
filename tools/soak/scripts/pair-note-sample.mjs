// Samples the pair page's spread line (data-testid="pair-spread") of the soak's UI every N seconds with Playwright's Chromium from
// web/node_modules, one JSON line per sample: the text and its class (crossed = a negative spread, spread = a
// positive or zero one). Read-only.
//
//   node scripts/pair-note-sample.mjs --out <file.jsonl> [--every 20] [--minutes 50] [--url http://127.0.0.1:8490] [--asset token2|token3]
//   node scripts/pair-note-sample.mjs --summary <file.jsonl>      # crossed share, runs, longest runs
import { createRequire } from 'node:module';
import { appendFileSync, readFileSync } from 'node:fs';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

const argv = process.argv.slice(2);
const opt = (n, d) => (argv.includes(n) ? argv[argv.indexOf(n) + 1] : d);
const klass = (t) => (/Spread -/.test(t) ? 'crossed' : /Spread /.test(t) ? 'spread' : 'other');

if (opt('--summary', null)) {
  const rows = readFileSync(opt('--summary'), 'utf8').split('\n').filter(Boolean).map((l) => JSON.parse(l));
  const n = rows.length;
  const crossed = rows.filter((r) => r.cls === 'crossed').length;
  const runs = [];
  let start = null, last = null;
  for (const r of rows) {
    if (r.cls === 'crossed') { if (start === null) start = r.t; last = r.t; } else if (start !== null) { runs.push((last - start) / 1000 + 20); start = null; }
  }
  if (start !== null) runs.push((last - start) / 1000 + 20);
  runs.sort((a, b) => b - a);
  const cls = {};
  for (const r of rows) cls[r.cls] = (cls[r.cls] ?? 0) + 1;
  const changes = rows.reduce((c, r, i) => c + (i && r.cls !== rows[i - 1].cls ? 1 : 0), 0);
  console.log(`samples ${n} (${new Date(rows[0].t).toISOString()} .. ${new Date(rows[n - 1].t).toISOString()}); classes ${JSON.stringify(cls)}; crossed ${crossed} (${((100 * crossed) / n).toFixed(1)} %); runs ${runs.length}; longest ${runs.slice(0, 5).map((x) => (x / 60).toFixed(1) + ' min').join(', ')}; class changes ${changes}`);
  console.log(`run lengths (s, ~20 s resolution): ${[...runs].reverse().join(' ')}`);
  process.exit(0);
}

const HERE = dirname(fileURLToPath(import.meta.url));
const SOAK = resolve(HERE, '..');
const require = createRequire(join(SOAK, '..', '..', 'web', 'package.json'));
const { chromium } = require('playwright');
const base = opt('--url', 'http://127.0.0.1:8490');
const out = opt('--out');
const every = Number(opt('--every', 20)) * 1000;
const until = Date.now() + Number(opt('--minutes', 50)) * 60_000;
const state = JSON.parse(readFileSync(join(SOAK, 'run', 'state.json'), 'utf8'));
const hash = `#/pair/${state[opt('--asset', 'token2')].covenantId}/${state.token.covenantId}`;
const browser = await chromium.launch();
const ctx = await browser.newContext({ viewport: { width: 1600, height: 1000 } });
const page = await ctx.newPage();
await page.addInitScript(() => { localStorage.setItem('kob.theme', 'dark'); });
await page.goto(`${base}/${hash}`);
while (Date.now() < until) {
  const t0 = Date.now();
  let text = '';
  try {
    await page.reload();
    await page.getByTestId('pair-spread').waitFor({ timeout: 15_000 });
    await page.waitForTimeout(1500);
    text = (await page.getByTestId('pair-spread').innerText()).replace(/\s+/g, ' ').trim();
  } catch (e) { text = `ERR ${e.message.split('\n')[0]}`; }
  appendFileSync(out, JSON.stringify({ t: Date.now(), text, cls: klass(text) }) + '\n');
  await new Promise((r) => setTimeout(r, Math.max(0, every - (Date.now() - t0))));
}
await browser.close();
