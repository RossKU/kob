// Real-wallet automation: Kaspire (HUB21 Chrome MV3 extension, window.kaspire) on Kaspa testnet-10.
//   node test/wallets/kaspire.mjs [--headed] [--resetup] [--no-repeat] [--only T1,T2,T3] [--port 8802]
// Flow: unpack the released extension -> Playwright bundled Chromium (persistent profile) -> import a fresh test
// mnemonic (WALLET_MNEMONIC_KASPIRE in .env, generated on first run) -> switch to TN10 through the wallet UI ->
// open the gate page -> connect (approve popup) -> scripts/setup.mjs for the wallet address -> run T1/T2/T3 through
// window.__gate.runTest while capturing + approving every Kaspire approval window. Results: out/wallet-kaspire.json.
import { createHash } from 'node:crypto';
import { existsSync, readFileSync, mkdirSync, writeFileSync } from 'node:fs';
import { join } from 'node:path';
import { spawnSync } from 'node:child_process';
import kaspa from '../../lib/node-kaspa.mjs';
import { ROOT, OUT, loadEnv, upsertEnv, launchWithExtension, extensionId, startServer, runSetup, saveResults, shot } from './common.mjs';

const argv = process.argv.slice(2);
const flag = (n) => argv.includes('--' + n);
const opt = (n, d) => { const i = argv.indexOf('--' + n); return i >= 0 && argv[i + 1] ? argv[i + 1] : d; };
const PORT = Number(opt('port', 8802));
const SKIP_MAIN = flag('skip-main');
const ONLY = opt('only', 'T1,T2,T3').split(',');
const VAULT_PASSWORD = 'kob-gate-kaspire-throwaway-2026'; // throwaway TN10 test wallet only
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

const ZIP = join(ROOT, 'vendor', 'kaspire', 'kaspire-extension-0.5.1.zip');
const EXT_DIR = join(ROOT, 'vendor', 'kaspire', 'ext');
const SOURCE_URL = 'https://github.com/KaspaHUB21/Kaspire-Kaspa-Wallet/releases/download/v0.11.37/kaspire-extension-0.5.1.zip';
const EXPECT_SHA = '8c44f8f9624e552bf7d75b07d981d4c8c7921e0679ea25b735195d4452cc5527'; // == the release's .sha256 asset

// ------------------------------------------------------------------ extension
async function ensureExtension() {
  if (!existsSync(ZIP)) {
    mkdirSync(join(ROOT, 'vendor', 'kaspire'), { recursive: true });
    const r = await fetch(SOURCE_URL);
    if (!r.ok) throw new Error('download failed ' + r.status);
    writeFileSync(ZIP, Buffer.from(await r.arrayBuffer()));
  }
  const sha = createHash('sha256').update(readFileSync(ZIP)).digest('hex');
  if (sha !== EXPECT_SHA) throw new Error(`extension zip sha256 mismatch: ${sha}`);
  if (!existsSync(join(EXT_DIR, 'manifest.json'))) {
    const { unzipSync } = await import('fflate');
    const files = unzipSync(new Uint8Array(readFileSync(ZIP)));
    for (const [name, data] of Object.entries(files)) {
      if (name.endsWith('/')) continue;
      const f = join(EXT_DIR, name);
      mkdirSync(join(f, '..'), { recursive: true });
      writeFileSync(f, data);
    }
  }
  const version = JSON.parse(readFileSync(join(EXT_DIR, 'manifest.json'), 'utf8')).version;
  return { version, sha256: sha };
}

// deterministic id of the unpacked extension (manifest "key"): first 16 bytes of sha256(DER pubkey), hex mapped to a-p
function extIdFromKey() {
  const key = JSON.parse(readFileSync(join(EXT_DIR, 'manifest.json'), 'utf8')).key;
  const h = createHash('sha256').update(Buffer.from(key, 'base64')).digest('hex').slice(0, 32);
  return [...h].map((c) => String.fromCharCode('a'.charCodeAt(0) + parseInt(c, 16))).join('');
}

// ------------------------------------------------------------------ mnemonic
function ensureMnemonic() {
  const env = loadEnv();
  if (env.WALLET_MNEMONIC_KASPIRE) return env.WALLET_MNEMONIC_KASPIRE;
  const phrase = kaspa.Mnemonic.random(12).phrase; // never printed
  upsertEnv({ WALLET_MNEMONIC_KASPIRE: phrase });
  return phrase;
}

// ------------------------------------------------------------------ approval windows
const popups = []; // every approval window captured in this run
const isApproval = (p) => /approval\.html/.test(p.url());

async function capture(p, label) {
  await p.waitForSelector('#approve, #unlock', { timeout: 20000 });
  await p.evaluate(() => document.querySelectorAll('details').forEach((d) => { d.open = true; }));
  await sleep(200);
  const title = await p.evaluate(() => document.querySelector('h1')?.textContent ?? '');
  const text = await p.evaluate(() => document.body.innerText);
  const raw = await p.evaluate(() => document.querySelector('.raw-json pre')?.textContent ?? '');
  const screenshot = await shot(p, 'kaspire', label.replace(/[^a-z0-9-]+/gi, '_'));
  const rec = { label, title, text, rawJsonLength: raw.length, rawJson: raw.length < 60000 ? raw : raw.slice(0, 60000) + '...[truncated]', screenshot, url: p.url() };
  popups.push(rec);
  return rec;
}

/** Runs `work` (a promise) while capturing and approving every Kaspire approval window that appears. */
async function drive(ctx, work, label, { approve = true } = {}) {
  let done = false;
  const settled = work.then((v) => ({ ok: v }), (e) => ({ err: e })).finally(() => { done = true; });
  const seen = new Set();
  const got = [];
  while (!done) {
    const p = ctx.pages().find((x) => isApproval(x) && !seen.has(x));
    if (!p) { await sleep(250); continue; }
    seen.add(p);
    try {
      let n = 0;
      let rec = await capture(p, `${label}-${++n}`);
      if (/unlock/i.test(rec.title) && (await p.$('#unlock'))) {
        await p.fill('#password', VAULT_PASSWORD);
        await p.click('#unlock');
        await sleep(800);
        if (!p.isClosed()) rec = await capture(p, `${label}-${++n}`);
        got.push(rec);
      }
      if (!p.isClosed()) {
        rec = await capture(p, `${label}-${++n}`);
        got.push(rec);
        await p.click(approve ? '#approve' : '#reject');
        await sleep(400);
      }
    } catch (e) {
      got.push({ label, error: String(e.message ?? e) });
    }
  }
  const r = await settled;
  return { result: r, popups: got };
}

// ------------------------------------------------------------------ wallet onboarding via the extension UI
async function onboard(ctx, extId, mnemonic) {
  const w = await ctx.newPage();
  await w.goto(`chrome-extension://${extId}/wallet.html`);
  await w.waitForSelector('#first-seed, #network, #password', { timeout: 30000 });
  if (await w.$('#first-seed')) {
    await w.click('#first-seed');
    await w.waitForSelector('#submit');
    await w.click('button[data-count="12"]');
    await w.waitForSelector('[data-word="11"]');
    const words = mnemonic.trim().split(/\s+/);
    if (words.length !== 12) throw new Error('mnemonic is not 12 words');
    for (let i = 0; i < 12; i++) await w.fill(`[data-word="${i}"]`, words[i]);
    await w.fill('#password', VAULT_PASSWORD);
    await w.fill('#password-confirm', VAULT_PASSWORD);
    await w.click('#submit');
    await w.waitForSelector('#network', { timeout: 60000 });
  } else if (await w.$('#password')) {
    await w.fill('#password', VAULT_PASSWORD);
    await w.click('#unlock, button:has-text("UNLOCK")');
    await w.waitForSelector('#network', { timeout: 30000 });
  }
  // switch to TN10 in the wallet UI (so the provider reports testnet-10 from the start)
  const label = (await w.textContent('#network')) ?? '';
  if (!/TN10/i.test(label)) {
    await w.click('#network');
    await w.click('[data-network="testnet-10"]');
    await w.waitForFunction(() => /TN10/i.test(document.querySelector('#network')?.textContent ?? ''), null, { timeout: 30000 });
  }
  const shotPath = await shot(w, 'kaspire', 'wallet-dashboard-tn10');
  await w.close();
  return shotPath;
}

// ------------------------------------------------------------------ main
const meta = { wallet: 'kaspire', date: new Date().toISOString(), source: SOURCE_URL, releaseTag: 'v0.11.37' };
const records = [];
const notes = [];
const extra = {};
let ctx, server;
const prevFile = join(OUT, 'wallet-kaspire.json');
const prev = SKIP_MAIN && existsSync(prevFile) ? JSON.parse(readFileSync(prevFile, 'utf8')) : null;
try {
  Object.assign(meta, await ensureExtension().then((x) => ({ extensionVersion: x.version, sha256: x.sha256 })));
  const mnemonic = ensureMnemonic();
  ctx = await launchWithExtension({ extensionDir: EXT_DIR, profileName: 'kaspire', headless: false, extraArgs: flag('headed') ? [] : ['--headless=new'] });
  let extId;
  try { extId = await extensionId(ctx, 60000); } catch { extId = extIdFromKey(); console.log('service worker not seen; using key-derived extension id', extId); }
  meta.extensionId = extId;
  meta.chromium = ctx.browser()?.version?.() ?? 'unknown';
  meta.headless = !flag('headed') ? 'new' : 'no';
  console.log('extension', meta.extensionVersion, extId, 'chromium', meta.chromium);
  meta.dashboardScreenshot = await onboard(ctx, extId, mnemonic);

  server = await startServer(PORT);
  const page = await ctx.newPage();
  page.on('console', (m) => { if (m.type() === 'error') console.log('[page error]', m.text().slice(0, 300)); });
  page.on('pageerror', (e) => console.log('[pageerror]', e.message));
  await page.goto(server.url);
  await page.waitForFunction(() => window.__gateReady === true, null, { timeout: 90000 });
  await page.waitForSelector('#connect-kaspire:not([disabled])', { timeout: 20000 });
  meta.userAgent = await page.evaluate(() => navigator.userAgent);
  meta.providerVersion = await page.evaluate(() => String(window.kaspire?.version ?? '?'));

  // ---- connect (approve popup)
  let d = await drive(ctx, page.evaluate(() => window.__gate.connectWallet('kaspire').then((w) => ({ address: w.address, pubkey: w.pubkey, network: w.network, version: w.version })).catch((e) => ({ error: e?.message ?? String(e) }))), 'connect');
  const conn = d.result.ok;
  meta.connect = { ...conn, popups: d.popups.map((p) => ({ label: p.label, title: p.title, text: p.text, screenshot: p.screenshot })) };
  console.log('connect:', JSON.stringify(conn));
  if (!conn || conn.error || conn.network !== 'testnet-10') throw new Error('connect failed or wrong network: ' + JSON.stringify(conn ?? d.result.err?.message));
  meta.walletAddress = conn.address;
  meta.walletPubkey = conn.pubkey;
  await shot(page, 'kaspire', 'gate-connected');

  // ---- setup for this wallet key
  const setupFile = join(ROOT, 'state', `setup-${conn.pubkey}.json`);
  const liveCounts = async () => page.evaluate(async () => {
    const G = window.__gate; const T = await import('/lib/txbuild.mjs');
    const out = { funds: 0, bids: 0, tokens: 0 };
    if (!G.setup) return out;
    const has = async (address, it) => (await T.utxosOf(G.rpc, address)).some((e) => (e.entry ?? e).outpoint.transactionId === it.txid && (e.entry ?? e).outpoint.index === it.index);
    for (const it of G.setup.funds) if (await has(G.setup.walletAddress, it)) out.funds++;
    for (const it of G.setup.bids) if (await has(it.address, it)) out.bids++;
    for (const it of G.setup.tokens) if (await has(it.address, it)) out.tokens++;
    return out;
  });
  let needSetup = flag('resetup') || !existsSync(setupFile);
  if (!needSetup) {
    await page.evaluate((pk) => window.__gate.loadSetup(pk), conn.pubkey);
    const lc = await liveCounts();
    console.log('live setup UTXOs:', JSON.stringify(lc));
    if (!SKIP_MAIN && (lc.funds < 2 || lc.bids < 3 || lc.tokens < 3)) needSetup = true;
  }
  if (needSetup) {
    console.log('running setup for', conn.address);
    runSetup(conn.address, 'kaspire');
    meta.setupRuns = (meta.setupRuns ?? 0) + 1;
  }
  await page.evaluate((pk) => window.__gate.loadSetup(pk), conn.pubkey);
  const setupOk = await page.evaluate(() => !!window.__gate.ctx && window.__gate.ctx.checks);
  console.log('setup loaded, address checks:', JSON.stringify(setupOk));
  if (!setupOk) throw new Error('setup not loaded in page');

  // ---- run tests
  const runOne = async (id, opts, tag) => {
    const label = `${id}-${tag}`;
    console.log(`\n=== ${label} ${JSON.stringify(opts)}`);
    const o = { ...opts, popup: 'pending', displayed: 'see popupTexts', note: tag };
    const d = await drive(ctx, page.evaluate(([id, o]) => window.__gate.runTest(id, o), [id, o]), label);
    let rec;
    if (d.result.err) rec = { test: id, result: 'FAIL', error: 'driver: ' + String(d.result.err.message ?? d.result.err) };
    else { const { unsignedTxJson, ...slim } = d.result.ok; rec = slim; }
    rec.variant = tag;
    rec.popupAppeared = d.popups.some((p) => p.text) ? 'yes' : 'no';
    rec.popupCount = d.popups.length;
    rec.popupTexts = d.popups.filter((p) => p.text).map((p) => ({ label: p.label, title: p.title, text: p.text, rawJson: p.rawJson, screenshot: p.screenshot }));
    rec.popupDisplayed = d.popups.filter((p) => p.text).map((p) => p.text).join('\n----\n');
    records.push(rec);
    console.log(rec.result, id, tag, rec.txid ?? '', rec.error ?? '', '| popup:', rec.popupAppeared, '| sigscript==ours:', rec.walletSigscriptEqualsOurs, '| kept:', JSON.stringify(rec.walletKeptFields));
    return rec;
  };

  if (!SKIP_MAIN) {
    const passing = {};
    if (ONLY.includes('T1')) { const r = await runOne('T1', {}, 'plain'); if (r.result === 'PASS') passing.T1 = {}; }
    for (const id of ['T2', 'T3'].filter((x) => ONLY.includes(x))) {
      const variants = [
        ['ordered-args', { kaspireMode: 'ordered-args' }],
        ['ordered-args+walletTx', { kaspireMode: 'ordered-args', useWalletTx: true }],
        ['wrap-signature', { kaspireMode: 'wrap-signature' }],
        ['none', { kaspireMode: 'none' }],
      ];
      for (const [tag, o] of variants) {
        const r = await runOne(id, o, tag);
        if (r.result === 'PASS') { passing[id] = { opts: o, tag }; break; }
        // wallet rejected before signing the ordered-args request: continue to the next variant; otherwise still try the rest
      }
    }
    // ---- repeat every passing test once more
    if (!flag('no-repeat')) {
      for (const [id, p] of Object.entries(passing)) await runOne(id, p.opts ?? {}, (p.tag ?? 'plain') + '-repeat');
    }
  }

  // ---- extras (informational): variants that do not consume UTXOs + wallet-tx broadcast + request/response diff
  if (!flag('no-extras')) {
    // 1) what does Kaspire change between the request tx and the returned tx? (no broadcast)
    const pd = await drive(ctx, page.evaluate(async () => {
      const G = window.__gate; const F = await import('/lib/flows.mjs'); const T = await import('/lib/txbuild.mjs');
      const diff = (x, y, path = '') => {
        if (JSON.stringify(x) === JSON.stringify(y)) return [];
        if (x && y && typeof x === 'object' && typeof y === 'object') {
          const keys = [...new Set([...Object.keys(x), ...Object.keys(y)])];
          return keys.flatMap((k) => diff(x[k], y[k], path + '/' + k));
        }
        return [{ path, request: x === undefined ? '<absent>' : String(x).slice(0, 120), response: y === undefined ? '<absent>' : String(y).slice(0, 120) }];
      };
      let entry;
      for (const it of G.setup.funds) {
        entry = (await T.utxosOf(G.rpc, G.setup.walletAddress)).find((e) => (e.entry ?? e).outpoint.transactionId === it.txid && (e.entry ?? e).outpoint.index === it.index);
        if (entry) break;
      }
      if (!entry) return { error: 'no live T1 UTXO for the diff probe' };
      const test = F.buildT1(G.k, G.ctx, entry);
      const signed = await G.adapter.signInput0(test, {});
      const req = JSON.parse(test.txJson());
      const res = JSON.parse(signed.signedTxJson);
      return { diff: diff(req, res), responseKeys: Object.keys(res), responseWrapperKeys: Object.keys(signed.response ?? {}) };
    }).catch((e) => ({ error: String(e?.message ?? e) })), 'probe-diff');
    extra.requestVsResponseDiff = pd.result.ok ?? { error: String(pd.result.err) };
    console.log('request vs response diff:', JSON.stringify(extra.requestVsResponseDiff).slice(0, 800));

    // 2) wallet-mode variants, signing only (no broadcast, no UTXO consumed)
    for (const id of ['T2', 'T3'].filter((x) => ONLY.includes(x))) {
      for (const [tag, o] of [['wrap-signature', { kaspireMode: 'wrap-signature' }], ['none', { kaspireMode: 'none' }]]) {
        await runOne(id, { ...o, broadcast: false }, tag + '-signonly');
      }
    }
    // 3) broadcast the WALLET-RETURNED tx (with our sigscript swapped in; sigscripts are equal anyway)
    for (const id of ['T2', 'T3'].filter((x) => ONLY.includes(x))) await runOne(id, { kaspireMode: 'ordered-args', useWalletTx: true }, 'ordered-args+walletTx');
  }
} catch (e) {
  notes.push('driver error: ' + (e?.stack ?? e));
  console.error('DRIVER ERROR', e?.stack ?? e);
} finally {
  const file = saveResults('kaspire', SKIP_MAIN && prev ? { ...prev, extra: { ...(prev.extra ?? {}), ...extra }, records: [...(prev.records ?? []), ...records], allPopups: [...(prev.allPopups ?? []), ...popups], notes: [...(prev.notes ?? []), ...notes] } : { meta, records, extra, connectPopups: meta.connect?.popups ?? [], allPopups: popups, notes });
  console.log('\nsaved', file);
  console.log('summary:', records.map((r) => `${r.test}[${r.variant}]=${r.result}${r.walletSigscriptEqualsOurs === true ? '(sig==ours)' : ''}`).join('  '));
  try { await ctx?.close(); } catch {}
  server?.stop();
}
process.exit(records.length && records.some((r) => r.result === 'PASS') ? 0 : 1);
