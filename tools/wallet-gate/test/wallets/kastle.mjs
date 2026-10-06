// Real-wallet driver: Kastle (Forbole Chrome extension, window.kastle) on Kaspa testnet-10.
//   node test/wallets/kastle.mjs [--fresh] [--setup] [--headless] [--only T2,T3] [--port 8803]
// What it does (everything real, nothing mocked):
//   1. downloads the latest RELEASED build from the Chrome Web Store (id oambclflhjfppdmkghokjmpppmaebego), unpacks the CRX,
//      records version + sha256 (cache: .browser-profiles/kastle-ext/, gitignored)
//   2. launches Playwright's bundled Chromium (branded Chrome >= 137 ignores --load-extension) with a persistent profile,
//      onboards Kastle through its UI by importing WALLET_MNEMONIC_KASTLE (generated into .env if missing, never printed)
//   3. connects the gate page (http://localhost:<port>/), switches Kastle to testnet-10, runs scripts/setup.mjs for its address
//   4. runs T1 (plain P2PK), T2 and T3 with kastleVariant plain / empty-script / redeem-script; for every wallet popup it
//      records screenshot + innerText + the "raw transaction details" panel, approves it, and records the result
//   5. writes out/wallet-kastle.json (+ screenshots in out/screens/kastle/)
//   --fresh    delete the browser profile first (forces a new UI onboarding; the mnemonic stays the same)
//   --setup    force a new scripts/setup.mjs run even if the setup file still has unspent items
import { mkdirSync, existsSync, readFileSync, writeFileSync, rmSync, statSync } from 'node:fs';
import { createHash } from 'node:crypto';
import { join } from 'node:path';
import { inflateRawSync } from 'node:zlib';
import { chromium, ROOT, OUT, loadEnv, upsertEnv, shot, startServer, launchWithExtension, extensionId, runSetup, runGateTest, saveResults } from './common.mjs';
import kaspa from '../../lib/node-kaspa.mjs';

const args = process.argv.slice(2);
const flag = (n) => args.includes('--' + n);
const opt = (n, d) => { const i = args.indexOf('--' + n); return i >= 0 && args[i + 1] ? args[i + 1] : d; };
const PORT = Number(opt('port', 8803));
const ONLY = opt('only', 'T1,T2,T3').split(',');
const EXT_ID_CWS = 'oambclflhjfppdmkghokjmpppmaebego';
const CWS_URL = `https://clients2.google.com/service/update2/crx?response=redirect&prodversion=140.0.0.0&acceptformat=crx2,crx3&x=id%3D${EXT_ID_CWS}%26uc`;
const EXT_ROOT = join(ROOT, '.browser-profiles', 'kastle-ext');
const EXT_DIR = join(EXT_ROOT, 'ext');
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
const say = (...a) => console.log('[kastle]', ...a);

// ------------------------------------------------------------------ 1. extension
/** Minimal unzip (stored/deflate) for the CRX payload - no dependency. */
function unzipTo(buf, dir) {
  let eocd = buf.length - 22;
  while (eocd >= 0 && buf.readUInt32LE(eocd) !== 0x06054b50) eocd--;
  const n = buf.readUInt16LE(eocd + 10);
  let p = buf.readUInt32LE(eocd + 16);
  for (let i = 0; i < n; i++) {
    const method = buf.readUInt16LE(p + 10), csize = buf.readUInt32LE(p + 20);
    const nlen = buf.readUInt16LE(p + 28), elen = buf.readUInt16LE(p + 30), clen = buf.readUInt16LE(p + 32), lho = buf.readUInt32LE(p + 42);
    const name = buf.toString('utf8', p + 46, p + 46 + nlen);
    p += 46 + nlen + elen + clen;
    if (name.endsWith('/')) continue;
    const ln = buf.readUInt16LE(lho + 26), le = buf.readUInt16LE(lho + 28);
    const data = buf.subarray(lho + 30 + ln + le, lho + 30 + ln + le + csize);
    const out = join(dir, name);
    mkdirSync(join(out, '..'), { recursive: true });
    writeFileSync(out, method === 0 ? data : inflateRawSync(data));
  }
}
async function obtainExtension() {
  const crxPath = join(EXT_ROOT, 'kastle.crx');
  if (!existsSync(crxPath) || !existsSync(join(EXT_DIR, 'manifest.json'))) {
    mkdirSync(EXT_ROOT, { recursive: true });
    say('downloading Kastle CRX from the Chrome Web Store ...');
    const r = await fetch(CWS_URL, { redirect: 'follow' });
    if (!r.ok) throw new Error('CRX download failed: HTTP ' + r.status);
    const crx = Buffer.from(await r.arrayBuffer());
    if (crx.toString('latin1', 0, 4) !== 'Cr24') throw new Error('not a CRX file');
    writeFileSync(crxPath, crx);
    rmSync(EXT_DIR, { recursive: true, force: true });
    unzipTo(crx.subarray(12 + crx.readUInt32LE(8)), EXT_DIR); // CRX3: magic(4) version(4) headerLen(4) header zip
  }
  const crx = readFileSync(crxPath);
  const manifest = JSON.parse(readFileSync(join(EXT_DIR, 'manifest.json'), 'utf8'));
  return { version: manifest.version, sha256: createHash('sha256').update(crx).digest('hex'), bytes: crx.length, source: `Chrome Web Store CRX, extension id ${EXT_ID_CWS} (${CWS_URL}); no GitHub release assets exist for forbole/kastle`, downloadedAt: statSync(crxPath).mtime.toISOString() };
}

// ------------------------------------------------------------------ 2. mnemonic
function ensureMnemonic() {
  const env = loadEnv();
  if (env.WALLET_MNEMONIC_KASTLE) return env;
  const m = kaspa.Mnemonic.random(12);
  upsertEnv({ WALLET_MNEMONIC_KASTLE: m.phrase, WALLET_PASSWORD_KASTLE: 'KobGate-Kastle-2026!x' });
  say('generated a new 12-word test mnemonic into .env (not printed)');
  return loadEnv();
}

// ------------------------------------------------------------------ popup helpers
const popupUrl = (p) => { const u = p.url(); return u.startsWith('chrome-extension://') && /requestId=/.test(u); };
async function waitPopup(ctx, known, ms = 30000) {
  const t0 = Date.now();
  while (Date.now() - t0 < ms) {
    const p = ctx.pages().find((x) => !known.has(x) && popupUrl(x));
    if (p) { await p.waitForLoadState('domcontentloaded').catch(() => {}); return p; }
    await sleep(200);
  }
  return null;
}
async function unlockIfNeeded(pop, env) {
  await sleep(700);
  if (/unlock/i.test(pop.url())) {
    say('popup is locked -> unlocking');
    await pop.locator('input').first().fill(env.WALLET_PASSWORD_KASTLE);
    await pop.getByRole('button', { name: 'Unlock' }).click();
    await sleep(1500);
  }
}
const text = async (p) => (await p.innerText('body').catch(() => '')).trim();

/** Onboards Kastle by importing the mnemonic (only if the profile has no wallet yet). */
async function onboardIfNeeded(ctx, extId, env) {
  // opening popup.html on a profile with no wallet makes the background worker open /onboarding tab(s) (a few seconds later)
  const probe = await ctx.newPage();
  await probe.goto(`chrome-extension://${extId}/popup.html`);
  const onb = () => ctx.pages().filter((x) => x.url().includes('#/onboarding'));
  for (let i = 0; i < 60 && !onb().length; i++) await sleep(250);
  const [p, ...extra] = onb();
  if (!p) { say('wallet already onboarded in this profile (popup url ' + probe.url().split('?')[0] + ')'); await probe.close(); return false; }
  for (const x of [probe, ...extra]) await x.close().catch(() => {});
  say('onboarding: importing mnemonic via the UI');
  await p.getByText('Import existing wallet').click();
  await p.locator('input').nth(0).fill(env.WALLET_PASSWORD_KASTLE);
  await p.locator('input').nth(1).fill(env.WALLET_PASSWORD_KASTLE);
  await p.locator('input[type=checkbox]').check();
  await p.getByRole('button', { name: 'Next' }).click();
  await p.getByText('Use a 12- or 24-word recovery phrase.').click();
  const words = env.WALLET_MNEMONIC_KASTLE.split(' ');
  for (let i = 0; i < 12; i++) await p.locator('input[type=password]').nth(i).fill(words[i]);
  await p.getByRole('button', { name: 'Import Wallet' }).click();
  await p.waitForSelector('text=Import Accounts', { timeout: 60000 });
  await p.locator('input[type=checkbox]').first().waitFor({ timeout: 60000 });
  await sleep(1500);
  if (!(await p.locator('input[type=checkbox]').first().isChecked())) await p.locator('input[type=checkbox]').first().check(); // account 0 is normally pre-checked
  await p.getByRole('button', { name: 'Import Wallet' }).click();
  try { await p.waitForSelector('text=successfully imported', { timeout: 60000 }); } catch (e) { await shot(p, 'kastle', 'onboard-fail'); throw new Error('onboarding did not finish; see out/screens/kastle/*onboard-fail.png; page text: ' + (await text(p)).slice(0, 300)); }
  await shot(p, 'kastle', 'onboarded');
  await p.close();
  return true;
}

/** Runs `start()` (which kicks off a wallet request without awaiting the result) and drives the popup it opens. */
async function drivePopup(ctx, env, label, screens, start, { approve = true, rawDetails = false } = {}) {
  const known = new Set(ctx.pages());
  start();
  const pop = await waitPopup(ctx, known);
  const info = { label, appeared: !!pop };
  if (!pop) return info;
  await unlockIfNeeded(pop, env);
  await sleep(1800); // let the confirm screen render (it loads an RPC client first)
  info.url = pop.url().replace(/(payload=)[^&#]*/, '$1<omitted>');
  info.route = (pop.url().split('#')[1] || '').split('?')[0];
  info.text = await text(pop);
  info.screenshot = await shot(pop, 'kastle', label);
  screens.push(info.screenshot);
  if (rawDetails) {
    const raw = pop.getByText('Show raw transaction details');
    if (await raw.count()) {
      await raw.click().catch(() => {});
      await sleep(800);
      info.textWithRaw = await text(pop);
      info.screenshotRaw = await shot(pop, 'kastle', label + '-raw');
      screens.push(info.screenshotRaw);
    }
  }
  const btnName = /Switch to|Connect$|^Confirm$|^Sign|Approve/;
  const buttons = await pop.getByRole('button').allInnerTexts().catch(() => []);
  info.buttons = buttons;
  if (approve) {
    const target = buttons.find((b) => btnName.test(b.trim()));
    if (!target) info.approveError = 'no approve button found among: ' + buttons.join(' | ');
    else {
      info.clicked = target.trim();
      const done = pop.waitForEvent('close', { timeout: 30000 }).catch(() => null);
      await pop.getByRole('button', { name: target.trim(), exact: true }).click().catch((e) => { info.approveError = String(e.message).slice(0, 200); });
      await done;
    }
  }
  return info;
}

// ------------------------------------------------------------------ main
const env = ensureMnemonic();
const meta = { wallet: 'Kastle', date: new Date().toISOString(), node: env.NODE_WS };
Object.assign(meta, { extension: await obtainExtension() });
if (flag('fresh')) rmSync(join(ROOT, '.browser-profiles', 'kastle'), { recursive: true, force: true });
mkdirSync(join(OUT, 'screens', 'kastle'), { recursive: true });

const server = await startServer(PORT);
const ctx = await launchWithExtension({ extensionDir: EXT_DIR, profileName: 'kastle', headless: false, extraArgs: flag('headless') ? ['--headless=new'] : [] });
meta.chromium = ctx.browser()?.version();
const screens = [];
const popups = [];
const records = [];
let exitCode = 0;
try {
  const extId = await extensionId(ctx);
  meta.extensionIdUnpacked = extId;
  say('extension', meta.extension.version, 'sha256', meta.extension.sha256.slice(0, 16) + '...', 'chromium', meta.chromium);
  meta.onboardedThisRun = await onboardIfNeeded(ctx, extId, env);
  for (const p of ctx.pages()) if (/onboarding/.test(p.url())) await p.close().catch(() => {});

  const page = await ctx.newPage();
  page.on('pageerror', (e) => say('[pageerror]', e.message, String(e.stack).split(String.fromCharCode(10)).slice(1, 4).join(' <- ')));
  await page.goto(`${server.url}`);
  await page.waitForFunction(() => window.__gateReady === true, null, { timeout: 60000 });
  if (!(await page.evaluate(() => !!window.kastle))) throw new Error('window.kastle not injected (extension not active on localhost?)');

  // ---- connect (permission popup) + switch to testnet-10 (popup) + account
  popups.push(await drivePopup(ctx, env, 'connect', screens, () => page.evaluate(() => { window.__connect = window.kastle.connect().then((r) => { window.__connectRes = 'ok:' + r; }).catch((e) => { window.__connectRes = 'err:' + (e?.message ?? JSON.stringify(e)); }); })));
  await page.waitForFunction(() => window.__connectRes !== undefined, null, { timeout: 30000 });
  meta.connectResult = await page.evaluate(() => window.__connectRes);
  let network = await page.evaluate(() => window.kastle.request('kas:get_network'));
  meta.networkBefore = network;
  if (network !== 'testnet-10') {
    popups.push(await drivePopup(ctx, env, 'switch-network', screens, () => page.evaluate(() => { window.__sw = window.kastle.request('kas:switch_network', 'testnet-10').then((r) => { window.__swRes = 'ok:' + JSON.stringify(r); }).catch((e) => { window.__swRes = 'err:' + (e?.message ?? JSON.stringify(e)); }); })));
    await page.waitForFunction(() => window.__swRes !== undefined, null, { timeout: 30000 });
    meta.switchResult = await page.evaluate(() => window.__swRes);
    network = await page.evaluate(() => window.kastle.request('kas:get_network'));
  }
  meta.networkAfter = network;
  const account = await page.evaluate(() => window.kastle.getAccount());
  meta.account = account;
  meta.publicKeyBytes = account.publicKey.length / 2;
  const xonly = account.publicKey.length === 66 ? account.publicKey.slice(2) : account.publicKey;
  const addrFromKey = new kaspa.XOnlyPublicKey(xonly).toAddress('testnet').toString();
  meta.addressMatchesPublicKey = addrFromKey === account.address;
  meta.publicKeyNote = `Kastle returns a ${meta.publicKeyBytes}-byte publicKey (${account.publicKey.slice(0, 2)} prefix); x-only = last 32 bytes; address from x-only key matches wallet address: ${meta.addressMatchesPublicKey}`;
  say('address', account.address, '| pubkey bytes', meta.publicKeyBytes, '| network', network, '| addr matches key', meta.addressMatchesPublicKey);
  if (network !== 'testnet-10') throw new Error('could not switch Kastle to testnet-10: ' + network);

  // ---- setup for this address
  const setupFile = join(ROOT, 'state', `setup-${xonly}.json`);
  meta.setupRuns = 0;
  const summarize = (s, extra = {}) => ({ tokenCovId: s.tokenCovId, tokens: s.tokens.length, bids: s.bids.length, funds: s.funds.length, ...extra });
  if (flag('setup') || !existsSync(setupFile)) { meta.setupRuns = 1; meta.setup = summarize(runSetup(account.address, 'kastle')); }
  else meta.setup = summarize(JSON.parse(readFileSync(setupFile, 'utf8')), { reused: true });
  await page.reload();
  await page.waitForFunction(() => window.__gateReady === true, null, { timeout: 60000 });
  await page.click('#connect-kastle');
  await page.waitForFunction(() => window.__gate.ctx, null, { timeout: 30000 });
  meta.walletInfo = (await page.locator('#walletinfo').innerText()).trim();
  meta.gateSetupInfo = (await page.locator('#setupinfo').innerText()).trim();
  say(meta.walletInfo);

  // ---- tests
  const plan = [];
  if (ONLY.includes('T1')) plan.push(['T1', 'plain']);
  for (const t of ['T2', 'T3']) if (ONLY.includes(t)) for (const v of ['plain', 'empty-script', 'redeem-script']) plan.push([t, v]);
  for (const [id, variant] of plan) {
    const label = `${id}-${variant}`;
    say('=== run', label);
    const opts = { skip: 0, kastleVariant: variant, popup: 'unknown', displayed: '(see popupTexts in wallet-kastle.json)', note: 'kastle driver run' };
    let popInfo;
    const pending = page.evaluate(([id, opts]) => window.__gate.runTest(id, opts).then(({ unsignedTxJson, ...r }) => r), [id, opts]);
    // drive the popup that runTest triggers (start() is a no-op: the request is already in flight)
    popInfo = await drivePopup(ctx, env, label, screens, () => {}, { rawDetails: true, approve: true });
    popInfo.test = id; popInfo.variant = variant;
    popups.push(popInfo);
    const rec = await pending;
    rec.popupAppeared = popInfo.appeared ? 'yes' : 'no';
    const shown = (popInfo.text || '').replace(/\s*\n\s*/g, ' | ').slice(0, 400);
    rec.popupDisplayed = popInfo.appeared ? shown : '(no popup)';
    rec.popupScreenshot = popInfo.screenshot;
    records.push(rec);
    say(label, '->', rec.result, rec.txid ?? '', rec.error ?? '', '| sigscript==ours:', rec.walletSigscriptEqualsOurs, '| kept:', JSON.stringify(rec.walletKeptFields));
  }
} catch (e) {
  meta.driverError = String(e?.stack ?? e);
  console.error('[kastle] DRIVER ERROR', meta.driverError);
  exitCode = 1;
} finally {
  const summary = records.map((r) => ({ test: r.test, variant: r.options?.kastleVariant, popup: r.popupAppeared, signatureReturned: r.signatureReturned ?? false, sigscriptEqualsOurs: r.walletSigscriptEqualsOurs, kept: r.walletKeptFields, accepted: r.accepted ?? false, txid: r.txid, error: r.error, walletError: r.walletError }));
  const file = saveResults(ONLY.length === 3 ? 'kastle' : 'kastle-only-' + ONLY.join('-'), { meta, summary, records, popupTexts: popups, screenshots: screens });
  say('saved', file);
  console.table(summary.map((s) => ({ ...s, kept: JSON.stringify(s.kept), error: (s.error ?? '').slice(0, 90), walletError: (s.walletError ?? '').slice(0, 60) })));
  await ctx.close().catch(() => {});
  server.stop();
}
process.exit(exitCode);
