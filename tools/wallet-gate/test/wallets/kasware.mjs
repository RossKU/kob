// REAL-wallet driver: KasWare (Chrome extension, official Chrome Web Store build) vs the KOB wallet gate, TN10.
//   node test/wallets/kasware.mjs [--headless] [--keep-open] [--only T1,T2] [--no-variants] [--repeat N]
// Flow: fetch+unpack the official CRX at the pinned version / sha256 (lib/extension-pins.mjs; cached in vendor/ext-kasware) -> Playwright bundled Chromium + persistent profile
//   -> import WALLET_MNEMONIC_KASWARE (.env, generated if absent, never printed) -> switch KasWare to Testnet 10
//   -> scripts/setup.mjs for the wallet address (only if state file missing / UTXOs used up) -> serve gate page
//   -> connect (approve popup) -> T1/T2/T3 via window.__gate.runTest, capturing + approving each KasWare popup.
// Writes out/wallet-kasware.json and screenshots under out/screens/kasware/. Needs Chromium: npx playwright-core install chromium
import { existsSync, readFileSync, writeFileSync, rmSync } from 'node:fs';
import { join } from 'node:path';
import kaspa from '../../lib/node-kaspa.mjs';
import * as C from '../../lib/contracts.mjs';
import * as T from '../../lib/txbuild.mjs';
import * as F from '../../lib/flows.mjs';
import { ROOT, EXTENSIONS, ensurePinnedCrx, loadEnv, upsertEnv, launchWithExtension, runSetup, runGateTest, saveResults, shot, startServer } from './common.mjs';

const argv = process.argv.slice(2);
const flag = (n) => argv.includes('--' + n);
const optv = (n, d) => { const i = argv.indexOf('--' + n); return i >= 0 && argv[i + 1] && !argv[i + 1].startsWith('--') ? argv[i + 1] : d; };
const HEADLESS = flag('headless');
const ONLY = optv('only', 'T1,T2,T3').split(',');
const REPEAT = Number(optv('repeat', 2)); // passes per test (>=2 checks repeatability)
const VARIANTS = !flag('no-variants');
const NEG_ONLY = flag('negative-only'); // node-only control (no browser): merge a wrong-key control into the existing results file
const PORT = Number(optv('port', 8801));
const PASSWORD = 'Kob-Test-12345'; // throwaway UI password of a throwaway profile (KasWare requires "5 digits")
const EXT_ID = EXTENSIONS.kasware.id;
const CRX_URL = EXTENSIONS.kasware.crxUrl; // pinned version + sha256 in lib/extension-pins.mjs
const EXT_DIR = join(ROOT, 'vendor', 'ext-kasware');
const UNPACKED = join(EXT_DIR, 'unpacked');
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
const collapse = (s) => s.replace(/\s+/g, ' ').trim();
const log = (...a) => console.log(new Date().toISOString().slice(11, 19), ...a);

// ------------------------------------------------------------------ 1. extension
async function ensureExtension() {
  const x = await ensurePinnedCrx('kasware', { unpackedDir: UNPACKED, crxPath: join(EXT_DIR, 'kasware.crx'), log });
  return { name: 'KasWare Wallet', id: EXT_ID, version: x.version, source: 'Chrome Web Store CRX: ' + CRX_URL, redirectedTo: x.redirectedTo, sha256: x.sha256, crxBytes: x.bytes };
}

// ------------------------------------------------------------------ 2. mnemonic + expected address
function walletSecrets() {
  let env = loadEnv();
  if (!env.WALLET_MNEMONIC_KASWARE) { upsertEnv({ WALLET_MNEMONIC_KASWARE: kaspa.Mnemonic.random(12).phrase }); env = loadEnv(); }
  const phrase = env.WALLET_MNEMONIC_KASWARE;
  const xprv = new kaspa.XPrv(new kaspa.Mnemonic(phrase).toSeed());
  // KasWare default keyring path (verified against the address the UI shows): m/44'/111111'/0'/0/0, Schnorr P2PK
  const address = xprv.derivePath("m/44'/111111'/0'/0/0").toXPub().toPublicKey().toAddress('testnet-10').toString();
  return { words: phrase.split(' '), address };
}

// ------------------------------------------------------------------ 3. KasWare UI helpers
const clickText = async (page, text, { exact = true, timeout = 15000 } = {}) => page.getByText(text, { exact }).first().click({ timeout });

async function onboard(ctx, words) {
  let [sw] = ctx.serviceWorkers();
  if (!sw) sw = await ctx.waitForEvent('serviceworker', { timeout: 30000 });
  let page;
  for (let i = 0; i < 40 && !page; i++) { page = ctx.pages().find((p) => p.url().includes('/index.html')); if (!page) await sleep(500); }
  if (!page) throw new Error('KasWare onboarding tab did not open');
  await clickText(page, 'I already have a wallet'); await sleep(800);
  await clickText(page, 'Use Password'); await sleep(800);
  await page.getByPlaceholder('Enter your password').fill(PASSWORD);
  await page.getByPlaceholder('...and repeat it').fill(PASSWORD);
  await clickText(page, 'Continue', { exact: false }); await sleep(1500);
  await clickText(page, 'Import Seed Phrase', { exact: false }); await sleep(1500);
  const inputs = page.locator('input[type=password]');
  if ((await inputs.count()) !== 12) throw new Error('expected 12 seed word inputs, got ' + (await inputs.count()));
  for (let i = 0; i < 12; i++) await inputs.nth(i).fill(words[i]);
  await clickText(page, 'Continue'); await sleep(2500);
  for (let i = 0; i < 25 && !page.url().includes('WalletTabScreen'); i++) { await page.getByText('Continue', { exact: true }).first().click({ timeout: 3000 }).catch(() => {}); await sleep(1500); }
  if (!page.url().includes('WalletTabScreen')) throw new Error('KasWare import did not reach the wallet screen: ' + page.url());
  await sleep(1500);
  return page;
}

async function switchToTn10(page) {
  await clickText(page, 'Settings'); await sleep(1200);
  await clickText(page, 'Mainnet'); await sleep(1200);
  await clickText(page, 'Testnet 10'); await sleep(3000);
  await clickText(page, 'Wallet'); await sleep(1500);
  return collapse(await page.innerText('body'));
}

// ------------------------------------------------------------------ 4. popup capture
const popups = [];
const handled = new WeakSet();
const APPROVE = ['Sign', 'Sign & Submit', 'Approve', 'Confirm', 'Connect', 'Submit', 'Allow', 'Sign Transaction'];

async function findPopup(ctx, timeoutMs) {
  const end = Date.now() + timeoutMs;
  while (Date.now() < end) {
    const p = ctx.pages().find((x) => !x.isClosed() && x.url().includes('notification.html') && !handled.has(x));
    if (p) return p;
    await sleep(250);
  }
  return null;
}

/** Runs `startFn()` (returns a promise), waits for the KasWare approval popup, captures it, clicks approve. */
async function withApproval(ctx, label, startFn, { approve = true, timeoutMs = 60000 } = {}) {
  const resP = startFn();
  let settled = false;
  const guard = resP.then((v) => { settled = true; return v; }, (e) => { settled = true; throw e; });
  guard.catch(() => {});
  const end = Date.now() + timeoutMs;
  let pop = null;
  while (!pop && !settled && Date.now() < end) pop = await findPopup(ctx, 400);
  const info = { label, appeared: !!pop };
  if (pop) {
    handled.add(pop);
    await pop.waitForLoadState('domcontentloaded').catch(() => {});
    let text = '';
    for (let i = 0; i < 40; i++) { await sleep(400); text = await pop.innerText('body').catch(() => ''); if (/Cancel/.test(text) && text.length > 30 && i > 3) break; }
    info.url = pop.url();
    info.text = text;
    info.summary = collapse(text).slice(0, 600);
    info.screenshot = await shot(pop, 'kasware', label.replace(/[^\w-]+/g, '_') + '-popup');
    // extra detail: expand anything scrollable? capture the full scroll height too
    info.scrollHeight = await pop.evaluate(() => document.documentElement.scrollHeight).catch(() => null);
    popups.push(info);
    log(`popup [${label}]: ${info.summary.slice(0, 200)}`);
    if (approve) {
      let clicked = null;
      for (const t of APPROVE) {
        const loc = pop.getByText(t, { exact: true });
        if (await loc.count().catch(() => 0)) { await loc.last().click({ timeout: 5000 }).then(() => { clicked = t; }).catch(() => {}); if (clicked) break; }
      }
      info.clicked = clicked;
      if (!clicked) { info.note = 'no known approve button found in popup'; log('NO APPROVE BUTTON FOUND'); }
      await pop.waitForEvent('close', { timeout: 20000 }).catch(() => {});
    }
  }
  const res = await resP;
  return { res, info };
}


// ------------------------------------------------------------------ negative control (node only, no wallet)
// Proves the TN10 node really executes the covenant scripts: same tx shapes as T2/T3, but signed by a WRONG key and
// assembled exactly like the wallet flow. Expected: the node REJECTS (otherwise "accepted" in T2/T3 would be vacuous).
async function negativeControl(setup) {
  const env = loadEnv();
  const rpc = await T.connectRpc(kaspa, env.NODE_WS);
  const out = [];
  try {
    const art = (n) => JSON.parse(readFileSync(join(ROOT, 'artifacts', n), 'utf8'));
    const ctx = F.deriveContext(kaspa, setup, art('KCC20Ref.json'), art('BidOrder.template.json'));
    const wrongKey = new kaspa.PrivateKey(env.DEV_PRIVATE_KEY); // any key that is not the wallet key
    const live = async (a, txid, i) => (await T.utxosOf(rpc, a)).find((e) => (e.entry ?? e).outpoint.transactionId === txid && (e.entry ?? e).outpoint.index === i);
    const bid = (await Promise.all(setup.bids.map(async (b) => ({ b, u: await live(b.address, b.txid, b.index) })))).find((x) => x.u);
    const tok = (await Promise.all(setup.tokens.map(async (t) => ({ t, u: await live(t.address, t.txid, t.index) })))).find((x) => x.u);
    const jobs = [];
    if (bid) jobs.push(['T2-wrongkey', F.buildT2(kaspa, ctx, bid.b, bid.u)]);
    if (tok) jobs.push(['T3-wrongkey', F.buildT3(kaspa, ctx, tok.u, setup.recipientPubkey || env.DEV_PUBKEY)]);
    for (const [name, t] of jobs) {
      const rec = { name };
      try { const { tx } = F.finalize(t, F.signLocal(kaspa, t, wrongKey)); rec.txid = await T.submit(rpc, tx); rec.rejected = false; }
      catch (e) { rec.rejected = true; rec.error = String(e?.message ?? e).slice(0, 400); }
      out.push(rec);
      log('negative control', name, rec.rejected ? 'REJECTED as expected: ' + rec.error.slice(0, 160) : 'ACCEPTED (unexpected!) ' + rec.txid);
    }
    if (!jobs.length) out.push({ note: 'no unspent T2/T3 UTXO left for the control' });
  } finally { try { await rpc.disconnect(); } catch {} }
  return out;
}

// ------------------------------------------------------------------ main
const meta = { date: new Date().toISOString(), args: argv, headless: HEADLESS, node: process.version, platform: process.platform };
const records = [];
if (NEG_ONLY) {
  const { address } = walletSecrets();
  const setup = JSON.parse(readFileSync(join(ROOT, 'state', `setup-${C.pubkeyOfAddress(kaspa, address)}.json`), 'utf8'));
  const f = join(ROOT, 'out', 'wallet-kasware.json');
  const cur = JSON.parse(readFileSync(f, 'utf8'));
  cur.negativeControl = await negativeControl(setup);
  writeFileSync(f, JSON.stringify(cur, null, 2));
  process.exit(0);
}
let ctx, server;
let setupRuns = 0;
try {
  meta.extension = await ensureExtension();
  log('KasWare', meta.extension.version, 'sha256', meta.extension.sha256);
  const { words, address } = walletSecrets();
  meta.expectedAddress = address;
  const profile = 'kasware';
  rmSync(join(ROOT, '.browser-profiles', profile), { recursive: true, force: true });
  ctx = await launchWithExtension({ extensionDir: UNPACKED, profileName: profile, headless: HEADLESS });
  meta.chromium = ctx.browser()?.version();
  log('chromium', meta.chromium);

  const ext = await onboard(ctx, words);
  meta.walletTabAfterTn10 = (await switchToTn10(ext)).slice(0, 400);
  meta.onboardScreenshot = await shot(ext, 'kasware', 'wallet-after-tn10');
  log('KasWare on TN10:', meta.walletTabAfterTn10.slice(0, 160));
  if (!/kaspatest:/.test(meta.walletTabAfterTn10)) throw new Error('KasWare did not switch to a testnet address');

  // setup for the derived address (idempotent; reuse the state file when it exists)
  const setupFile = (pk) => join(ROOT, 'state', `setup-${pk}.json`);
  let setupState = null;
  try {
    const pk = (await import('../../lib/contracts.mjs')).pubkeyOfAddress(kaspa, address);
    meta.walletPubkey = pk;
    if (existsSync(setupFile(pk))) { setupState = JSON.parse(readFileSync(setupFile(pk), 'utf8')); log('reusing setup file for', address); }
  } catch (e) { throw new Error('cannot derive pubkey of ' + address + ': ' + e.message); }
  const doSetup = () => { setupRuns++; log('running setup for', address, `(run #${setupRuns})`); setupState = runSetup(address, 'kasware'); };
  if (!setupState) doSetup();

  server = await startServer(PORT);
  const gate = await ctx.newPage();
  gate.on('console', (m) => { if (m.type() === 'error') log('[gate console error]', m.text().slice(0, 300)); });
  gate.on('pageerror', (e) => log('[gate pageerror]', e.message));
  await gate.goto(server.url);
  await gate.waitForFunction(() => window.__gateReady === true, null, { timeout: 90000 });
  await sleep(2000);
  await gate.waitForSelector('#connect-kasware:not([disabled])', { timeout: 20000 });

  // ---- connect
  const conn = await withApproval(ctx, 'connect', () => gate.evaluate(() => window.__gate.connectWallet('kasware').then((w) => ({ address: w.address, pubkey: w.pubkey, network: w.network, version: w.version }))));
  meta.connect = { ...conn.res, popup: conn.info };
  log('connected:', JSON.stringify(conn.res));
  if (conn.res.address !== address) log('WARNING: wallet address differs from derived address', conn.res.address, address);
  const ready = await gate.evaluate(() => !!window.__gate.ctx);
  if (!ready) throw new Error('gate page has no setup ctx after connect');
  meta.gateSetupInfo = await gate.locator('#setupinfo').innerText();
  meta.gateWalletInfo = await gate.locator('#walletinfo').innerText();

  // ---- tests
  async function attempt(id, opts, label) {
    for (let retry = 0; retry < 2; retry++) {
      log(`--- ${label}: ${id} ${JSON.stringify(opts)}`);
      const popupsBefore = popups.length;
      const { res: rec0, info } = await withApproval(ctx, label, () => runGateTest(gate, id, { ...opts, popup: 'pending', displayed: 'pending' }));
      let rec = rec0;
      if (/no unspent .* UTXO left/.test(rec.error ?? '') && setupRuns < 4) { // out of pre-made UTXOs: rerun setup and reload it in the page
        doSetup();
        await gate.evaluate(() => window.__gate.loadSetup(window.__gate.wallet.pubkey));
        continue;
      }
      // the popup is only known after the run starts, so patch the record (page copy + returned copy)
      const popupAppeared = info.appeared ? 'yes' : 'no';
      const displayed = info.appeared ? info.summary : '(no popup appeared)';
      rec = { ...rec, label, popupAppeared, popupDisplayed: displayed, popupClicked: info.clicked ?? null, popupScreenshot: info.screenshot ?? null, options: { ...rec.options, popup: popupAppeared, displayed } };
      await gate.evaluate(([l, pa, pd]) => { const r = window.__gate.records.at(-1); if (r) { r.label = l; r.popupAppeared = pa; r.popupDisplayed = pd; r.options.popup = pa; r.options.displayed = pd; localStorage.setItem('kob-wallet-gate-records', JSON.stringify(window.__gate.records)); } }, [label, popupAppeared, displayed]).catch(() => {});
      rec.screenshot = await shot(gate, 'kasware', `${label}-gate`);
      records.push(rec);
      log(`${label}: ${rec.result} popup=${popupAppeared} sig=${rec.signatureReturned ?? false} sighash=${rec.sighashByte ?? '-'} kept=${JSON.stringify(rec.walletKeptFields ?? null)} sigscript==ours:${rec.walletSigscriptEqualsOurs} tx=${rec.txid ?? '-'} accepted=${rec.accepted ?? false}${rec.error ? ' ERROR: ' + rec.error : ''}`);
      return rec;
    }
    throw new Error('setup exhausted for ' + id);
  }

  for (const id of ONLY) {
    let pass = 0;
    for (let n = 1; n <= REPEAT; n++) {
      const rec = await attempt(id, {}, `${id}-run${n}`);
      if (rec.result === 'PASS') pass++;
      else if (n === 1) break; // default failed: go to variants, repeating a failure adds nothing
    }
    if (pass === 0 && VARIANTS) {
      const variants = [
        [{ useWalletTx: true }, 'useWalletTx'],
        [{ sighashType: 0 }, 'sighash0'],
        [{ sighashType: 129 }, 'sighash129-ALL_ACP'],
      ];
      for (const [o, name] of variants) await attempt(id, o, `${id}-variant-${name}`);
    }
  }
} catch (e) {
  meta.fatal = e?.stack ?? String(e);
  log('FATAL', meta.fatal);
} finally {
  meta.setupRuns = setupRuns;
  const summary = records.map((r) => ({ label: r.label, test: r.test, result: r.result, popup: r.popupAppeared, signatureReturned: !!r.signatureReturned, accepted: !!r.accepted, txid: r.txid ?? null, error: r.error ?? null }));
  let negative = null;
  try { const pk = meta.walletPubkey; if (pk && !meta.fatal) negative = await negativeControl(JSON.parse(readFileSync(join(ROOT, 'state', `setup-${pk}.json`), 'utf8'))); } catch (e) { negative = [{ error: String(e?.message ?? e) }]; }
  const file = saveResults('kasware', { meta, summary, negativeControl: negative, records, popups, screenshots: [...popups.map((p) => p.screenshot), ...records.map((r) => r.screenshot), meta.onboardScreenshot].filter(Boolean) });
  log('saved', file);
  if (!flag('keep-open')) { try { await ctx?.close(); } catch {} }
  server?.stop();
  process.exit(records.length && records.every((r) => r.result === 'PASS') ? 0 : 1);
}
