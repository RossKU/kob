// Shared helpers for the REAL-wallet TN10 runs (web/e2e-real): unpack / cache the released wallet extensions, launch Chromium with an
// unpacked extension in a persistent profile, onboard the wallets through their UIs with a throw-away mnemonic, and find / approve
// their popups. Ported from tools/wallet-gate/test/wallets/{common,kasware,kaspire,kastle}.mjs, which proved every step on TN10.
//
// NOTHING RUNS ON IMPORT: no directories are created, no browser is started, no network is touched. The scenario itself (a run script
// that drives the built app against a TN10 node) is written later on top of these functions.
//
// Test money only: mnemonics are generated into the gitignored `e2e-real/.env` and never printed; profiles live in the gitignored
// `e2e-real/.browser-profiles/`; unpacked extensions in `e2e-real/.ext/` (add `e2e-real/.ext/` to web/.gitignore).
import { createHash } from 'node:crypto';
import { cpSync, existsSync, mkdirSync, readFileSync, rmSync, writeFileSync } from 'node:fs';
import { createRequire } from 'node:module';
import { dirname, join, resolve, sep } from 'node:path';
import { fileURLToPath } from 'node:url';
import { unzipSync } from 'fflate';
import { chromium } from 'playwright-core';

export { chromium };

// ------------------------------------------------------------------------------------------------ paths

export const E2E_REAL_DIR = dirname(fileURLToPath(import.meta.url));
export const WEB_ROOT = resolve(E2E_REAL_DIR, '..');
export const REPO_ROOT = resolve(WEB_ROOT, '..');
/** unpacked extensions used by the runs (copied from the wallet-gate cache or downloaded) */
export const EXT_ROOT = join(E2E_REAL_DIR, '.ext');
export const PROFILE_ROOT = join(E2E_REAL_DIR, '.browser-profiles');
export const OUT_DIR = join(E2E_REAL_DIR, 'out');
export const ENV_PATH = join(E2E_REAL_DIR, '.env');

/** Read-only caches of the wallet-gate kit (extensions already downloaded there); searched in this order. */
export const WALLET_GATE_VENDOR_CANDIDATES = [
  process.env.KOB_WALLET_GATE_VENDOR,
  join(REPO_ROOT, 'tools', 'wallet-gate', 'vendor'),
].filter(Boolean);

export const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
export const collapse = (s) => String(s).replace(/\s+/g, ' ').trim();
export const log = (...a) => console.log(new Date().toISOString().slice(11, 19), ...a);

// ------------------------------------------------------------------------------------------------ extension sources

/**
 * Where each wallet comes from. `vendorDir` is relative to the wallet-gate vendor cache; the download URLs are the ones the gate drivers used
 * (Chrome Web Store update endpoint for CRX builds, the release zip for Kaspire).
 *
 * Every wallet is pinned: `version` is the manifest version the runs use, and a download must have the sha256 below
 * (`crxSha256` for the CRX file, `zipSha256` for the zip). The Chrome Web Store endpoint always serves the newest release,
 * so when a wallet publishes a new version the download is refused until the pin is moved here (new version and sha256,
 * after looking at the release). An unpacked copy (e2e-real/.ext, the wallet-gate cache) is used only when its manifest
 * version equals the pin.
 */
export const EXTENSIONS = {
  kasware: {
    label: 'KasWare',
    vendorDir: 'ext-kasware/unpacked',
    id: 'hklhheigdmpoolooomdihmhlpjjdbklf',
    crxUrl: 'https://clients2.google.com/service/update2/crx?response=redirect&prodversion=153.0.0.0&acceptformat=crx3&x=id%3Dhklhheigdmpoolooomdihmhlpjjdbklf%26uc',
    version: '0.10.0',
    crxSha256: 'c2a9cf257f249653ff856eab02e5ab681a7590d8c58f58feecc4306b5354dcf0',
  },
  kaspire: {
    label: 'Kaspire',
    vendorDir: 'kaspire/ext',
    zipUrl: 'https://github.com/KaspaHUB21/Kaspire-Kaspa-Wallet/releases/download/v0.11.37/kaspire-extension-0.5.1.zip',
    version: '0.5.1',
    zipSha256: '8c44f8f9624e552bf7d75b07d981d4c8c7921e0679ea25b735195d4452cc5527',
  },
  kastle: {
    label: 'Kastle',
    vendorDir: 'kastle/ext',
    id: 'oambclflhjfppdmkghokjmpppmaebego',
    crxUrl: 'https://clients2.google.com/service/update2/crx?response=redirect&prodversion=140.0.0.0&acceptformat=crx2,crx3&x=id%3Doambclflhjfppdmkghokjmpppmaebego%26uc',
    version: '2.61.0',
    crxSha256: '6b1220ceefb73636fc0b47e9a8e7e6c93463067e34457af4f5d473b5fda0ba37',
  },
};

/** The zip payload of a CRX2 / CRX3 file. */
export function crxToZip(buf) {
  const b = Buffer.from(buf);
  if (b.toString('latin1', 0, 4) !== 'Cr24') throw new Error('not a CRX file (missing Cr24 magic)');
  const version = b.readUInt32LE(4);
  if (version === 3) return b.subarray(12 + b.readUInt32LE(8));
  if (version === 2) return b.subarray(16 + b.readUInt32LE(8) + b.readUInt32LE(12));
  throw new Error(`unsupported CRX version ${version}`);
}

/** Unpacks a zip into `dir` (refusing entries that would escape it). Returns the list of written files. */
export function unpackZip(buf, dir) {
  const root = resolve(dir);
  const written = [];
  for (const [name, data] of Object.entries(unzipSync(new Uint8Array(buf)))) {
    if (name.endsWith('/')) continue;
    const file = resolve(root, name);
    if (file !== root && !file.startsWith(root + sep)) throw new Error(`zip entry escapes the target directory: ${name}`);
    mkdirSync(dirname(file), { recursive: true });
    writeFileSync(file, data);
    written.push(name);
  }
  return written;
}

export const unpackCrx = (buf, dir) => unpackZip(crxToZip(buf), dir);

export const extensionVersion = (dir) => JSON.parse(readFileSync(join(dir, 'manifest.json'), 'utf8')).version;
const sha256 = (buf) => createHash('sha256').update(buf).digest('hex');

async function download(url) {
  const res = await fetch(url, { redirect: 'follow' });
  if (!res.ok) throw new Error(`download failed: HTTP ${res.status} ${url}`);
  return Buffer.from(await res.arrayBuffer());
}

/**
 * Makes `web/e2e-real/.ext/<wallet>/` contain the unpacked extension: reuses it, else copies the wallet-gate cache (read-only source),
 * else downloads it (KasWare / Kastle: CRX from the Chrome Web Store endpoint; Kaspire: the release zip). Only the pinned version is
 * used: an unpacked copy with another manifest version is passed over, and a download whose sha256 or manifest version differs from
 * the pin is refused (nothing is unpacked).
 * @returns {Promise<{dir: string, version: string, source: 'cache' | 'vendor' | 'download', sha256?: string}>}
 */
export async function ensureExtension(wallet, { download: allowDownload = true, force = false, extRoot = EXT_ROOT, vendors = WALLET_GATE_VENDOR_CANDIDATES } = {}) {
  const spec = EXTENSIONS[wallet];
  if (!spec) throw new Error(`unknown wallet ${wallet}`);
  const pinned = (d) => existsSync(join(d, 'manifest.json')) && extensionVersion(d) === spec.version;
  const dir = join(extRoot, wallet);
  if (!force && pinned(dir)) return { dir, version: spec.version, source: 'cache' };
  for (const vendor of vendors) {
    const src = join(vendor, spec.vendorDir);
    if (pinned(src)) {
      rmSync(dir, { recursive: true, force: true });
      mkdirSync(dirname(dir), { recursive: true });
      cpSync(src, dir, { recursive: true });
      return { dir, version: spec.version, source: 'vendor' };
    }
  }
  if (!allowDownload) throw new Error(`${spec.label} ${spec.version} (the pinned version) not found in ${extRoot} or the wallet-gate cache`);
  const pkg = await download(spec.zipUrl ?? spec.crxUrl);
  const sha = sha256(pkg);
  const want = spec.zipUrl ? spec.zipSha256 : spec.crxSha256;
  if (sha !== want) {
    throw new Error(`${spec.label} download sha256 ${sha} is not the pinned ${want} (${spec.version}): a different release is served; move the pin in EXTENSIONS after review`);
  }
  const files = spec.zipUrl ? unzipSync(new Uint8Array(pkg)) : unzipSync(new Uint8Array(crxToZip(pkg)));
  const version = JSON.parse(Buffer.from(files['manifest.json'] ?? []).toString('utf8').replace(/^﻿/, '') || '{}').version;
  if (version !== spec.version) throw new Error(`${spec.label} download has manifest version ${version}, the pin is ${spec.version}`);
  rmSync(dir, { recursive: true, force: true });
  if (spec.zipUrl) unpackZip(pkg, dir);
  else unpackCrx(pkg, dir);
  return { dir, version, source: 'download', sha256: sha };
}

// ------------------------------------------------------------------------------------------------ .env (secrets, gitignored)

export function parseEnv(text) {
  const out = {};
  for (const line of String(text).split(/\r?\n/)) {
    if (line.trim().startsWith('#')) continue;
    const m = /^\s*([A-Za-z0-9_]+)\s*=\s*(.*?)\s*$/.exec(line);
    if (m) out[m[1]] = m[2].replace(/^["']|["']$/g, '');
  }
  return out;
}

export function loadEnv(path = ENV_PATH) {
  return existsSync(path) ? parseEnv(readFileSync(path, 'utf8')) : {};
}

export function upsertEnv(pairs, path = ENV_PATH) {
  let text = existsSync(path) ? readFileSync(path, 'utf8') : '';
  for (const [k, v] of Object.entries(pairs)) {
    const re = new RegExp(`^\\s*${k}\\s*=.*$`, 'm');
    text = re.test(text) ? text.replace(re, `${k}=${v}`) : text + (text && !text.endsWith('\n') ? '\n' : '') + `${k}=${v}\n`;
  }
  mkdirSync(dirname(path), { recursive: true });
  writeFileSync(path, text);
}

// ------------------------------------------------------------------------------------------------ throw-away keys

let sdkCache = null;
/** The official kaspa SDK for node (vendored in web/vendor/kaspa-node). */
export const kaspaNode = () => (sdkCache ??= createRequire(import.meta.url)('../vendor/kaspa-node/kaspa.js'));

/** A fresh 12-word mnemonic for a throw-away test wallet. Never print it. */
export const randomMnemonic = () => kaspaNode().Mnemonic.random(12).phrase;

/** The mnemonic stored under `key` in `.env`, generated (and stored) on first use. */
export function ensureMnemonic(key, path = ENV_PATH) {
  const env = loadEnv(path);
  if (env[key]) return env[key];
  const phrase = randomMnemonic();
  upsertEnv({ [key]: phrase }, path);
  return phrase;
}

/** KasWare's default keyring path (m/44'/111111'/0'/0/0, Schnorr P2PK): the address it shows after importing `phrase`. */
export function kaswareAddress(phrase, network = 'testnet-10') {
  const k = kaspaNode();
  const xprv = new k.XPrv(new k.Mnemonic(phrase).toSeed());
  return xprv.derivePath("m/44'/111111'/0'/0/0").toXPub().toPublicKey().toAddress(network).toString();
}

// ------------------------------------------------------------------------------------------------ browser

/**
 * Launches Chromium with an unpacked extension in a persistent profile. Extensions need a headed or new-headless browser; branded
 * Chrome >= 137 ignores --load-extension, so this uses Playwright's Chromium (set KOB_CHROME_PATH for a specific chrome.exe).
 */
export async function launchWalletBrowser({ extensionDir, profileName, headless = false, extraArgs = [], viewport = { width: 1280, height: 900 }, fresh = false }) {
  const userDataDir = join(PROFILE_ROOT, profileName);
  if (fresh) rmSync(userDataDir, { recursive: true, force: true });
  mkdirSync(userDataDir, { recursive: true });
  return chromium.launchPersistentContext(userDataDir, {
    headless: false, // real headless has no extension support: `--headless=new` below is the extension-capable mode
    executablePath: process.env.KOB_CHROME_PATH || undefined,
    args: [`--disable-extensions-except=${extensionDir}`, `--load-extension=${extensionDir}`, ...(headless ? ['--headless=new'] : []), ...extraArgs],
    viewport,
  });
}

/** Extension id of an unpacked MV3 extension via its service worker URL. */
export async function extensionId(ctx, timeoutMs = 30_000) {
  let [sw] = ctx.serviceWorkers();
  if (!sw) sw = await ctx.waitForEvent('serviceworker', { timeout: timeoutMs });
  return new URL(sw.url()).host;
}

export async function shot(page, dir, name) {
  const folder = join(OUT_DIR, 'screens', dir);
  mkdirSync(folder, { recursive: true });
  const file = join(folder, `${Date.now()}-${name}.png`);
  await page.screenshot({ path: file, fullPage: true }).catch(() => {});
  return file;
}

export function saveResults(name, obj) {
  mkdirSync(OUT_DIR, { recursive: true });
  const file = join(OUT_DIR, `wallet-${name}.json`);
  writeFileSync(file, JSON.stringify(obj, (_, v) => (typeof v === 'bigint' ? v.toString() : v), 2));
  return file;
}

// ------------------------------------------------------------------------------------------------ popups

/** How each wallet's approval window is recognised by URL. */
export const POPUP_MATCHERS = {
  kasware: (url) => url.includes('notification.html'),
  kaspire: (url) => /approval\.html/.test(url),
  kastle: (url) => url.startsWith('chrome-extension://') && /requestId=/.test(url),
};

export const isWalletPopup = (wallet, page) => !page.isClosed() && POPUP_MATCHERS[wallet](page.url());

/** Waits for a not-yet-handled approval window of `wallet`; null after `timeoutMs`. */
export async function findPopup(ctx, wallet, { handled = new WeakSet(), timeoutMs = 30_000, pollMs = 250 } = {}) {
  const end = Date.now() + timeoutMs;
  while (Date.now() < end) {
    const p = ctx.pages().find((x) => isWalletPopup(wallet, x) && !handled.has(x));
    if (p) return p;
    await sleep(pollMs);
  }
  return null;
}

/**
 * Watches for approval windows of `wallet` and calls `handler(page, {wallet, index})` for each new one (approve, reject, inspect ...).
 * Returns a controller: `stop()` ends the watch and resolves to `[{index, url, result?, error?}]`.
 *
 *   const watch = approvePopups(ctx, kaswareApprover(), { wallet: 'kasware' });
 *   await page.click('#confirm-sign');            // the app asks the wallet to sign
 *   const seen = await watch.stop();              // popups that appeared and how they were handled
 */
export function approvePopups(ctx, handler, { wallet, pollMs = 250 } = {}) {
  if (!POPUP_MATCHERS[wallet]) throw new Error(`approvePopups: unknown wallet ${wallet}`);
  const handled = new WeakSet();
  const seen = [];
  let running = true;
  const loop = (async () => {
    while (running) {
      const p = ctx.pages().find((x) => isWalletPopup(wallet, x) && !handled.has(x));
      if (!p) {
        await sleep(pollMs);
        continue;
      }
      handled.add(p);
      const entry = { index: seen.length, url: p.url().replace(/(payload=)[^&#]*/, '$1<omitted>') };
      seen.push(entry);
      try {
        entry.result = await handler(p, { wallet, index: entry.index });
      } catch (e) {
        entry.error = String(e?.message ?? e);
      }
    }
  })();
  return {
    seen,
    async stop() {
      running = false;
      await loop;
      return seen;
    },
  };
}

/** Runs `start()` (returns a promise) while approving that wallet's popups; resolves with `{result, popups}`. */
export async function withPopupApproval(ctx, wallet, start, handler, opts = {}) {
  const watch = approvePopups(ctx, handler, { wallet, ...opts });
  try {
    return { result: await start(), popups: watch.seen };
  } finally {
    await watch.stop();
  }
}

const KASWARE_APPROVE = ['Sign', 'Sign & Submit', 'Approve', 'Confirm', 'Connect', 'Submit', 'Allow', 'Sign Transaction'];

/** KasWare: reads the popup text, clicks the first known approve label (or the reject label when `approve` is false). */
export const kaswareApprover =
  ({ approve = true, screenshotDir = 'kasware' } = {}) =>
  async (pop) => {
    await pop.waitForLoadState('domcontentloaded').catch(() => {});
    let text = '';
    for (let i = 0; i < 40; i++) {
      await sleep(400);
      text = await pop.innerText('body').catch(() => '');
      if (/Cancel/.test(text) && text.length > 30 && i > 3) break;
    }
    const info = { text, summary: collapse(text).slice(0, 600), screenshot: await shot(pop, screenshotDir, 'popup'), clicked: null };
    const labels = approve ? KASWARE_APPROVE : ['Cancel', 'Reject'];
    for (const t of labels) {
      const loc = pop.getByText(t, { exact: true });
      if ((await loc.count().catch(() => 0)) > 0) {
        await loc.last().click({ timeout: 5000 }).then(() => (info.clicked = t)).catch(() => {});
        if (info.clicked) break;
      }
    }
    if (!info.clicked) info.note = 'no known button found in the popup';
    await pop.waitForEvent('close', { timeout: 20_000 }).catch(() => {});
    return info;
  };

/** Kaspire: `#approve` / `#reject`; unlocks with the vault password when the window asks for it. */
export const kaspireApprover =
  ({ approve = true, password, screenshotDir = 'kaspire' } = {}) =>
  async (pop) => {
    const grab = async (label) => {
      await pop.waitForSelector('#approve, #unlock', { timeout: 20_000 });
      await pop.evaluate(() => document.querySelectorAll('details').forEach((d) => (d.open = true)));
      return {
        title: await pop.evaluate(() => document.querySelector('h1')?.textContent ?? ''),
        text: await pop.evaluate(() => document.body.innerText),
        rawJson: await pop.evaluate(() => document.querySelector('.raw-json pre')?.textContent ?? ''),
        screenshot: await shot(pop, screenshotDir, label),
      };
    };
    let info = await grab('popup');
    if (/unlock/i.test(info.title) && (await pop.$('#unlock'))) {
      if (!password) throw new Error('kaspireApprover: the popup is locked and no password was given');
      await pop.fill('#password', password);
      await pop.click('#unlock');
      await sleep(800);
      if (!pop.isClosed()) info = await grab('popup-unlocked');
    }
    if (!pop.isClosed()) {
      await pop.click(approve ? '#approve' : '#reject');
      await sleep(400);
    }
    return info;
  };

/** Kastle: unlocks when needed, waits for the confirm screen, clicks the approve button. */
export const kastleApprover =
  ({ approve = true, password, screenshotDir = 'kastle', rawDetails = true } = {}) =>
  async (pop) => {
    await pop.waitForLoadState('domcontentloaded').catch(() => {});
    await sleep(700);
    if (/unlock/i.test(pop.url())) {
      if (!password) throw new Error('kastleApprover: the popup is locked and no password was given');
      await pop.locator('input').first().fill(password);
      await pop.getByRole('button', { name: 'Unlock' }).click();
      await sleep(1500);
    }
    await sleep(1800); // the confirm screen loads an RPC client first
    const info = { url: pop.url().replace(/(payload=)[^&#]*/, '$1<omitted>'), text: (await pop.innerText('body').catch(() => '')).trim(), screenshot: await shot(pop, screenshotDir, 'popup') };
    if (rawDetails) {
      const raw = pop.getByText('Show raw transaction details');
      if (await raw.count()) {
        await raw.click().catch(() => {});
        await sleep(800);
        info.textWithRaw = (await pop.innerText('body').catch(() => '')).trim();
      }
    }
    const buttons = await pop.getByRole('button').allInnerTexts().catch(() => []);
    info.buttons = buttons;
    const want = approve ? /Switch to|Connect$|^Confirm$|^Sign|Approve/ : /^Reject|^Cancel|^Deny/;
    const target = buttons.find((b) => want.test(b.trim()));
    if (!target) info.note = `no matching button among: ${buttons.join(' | ')}`;
    else {
      info.clicked = target.trim();
      const closed = pop.waitForEvent('close', { timeout: 30_000 }).catch(() => null);
      await pop.getByRole('button', { name: target.trim(), exact: true }).click().catch((e) => (info.note = String(e.message).slice(0, 200)));
      await closed;
    }
    return info;
  };

// ------------------------------------------------------------------------------------------------ onboarding (throw-away wallets)

const clickText = (page, text, { exact = true, timeout = 15_000 } = {}) => page.getByText(text, { exact }).first().click({ timeout });

/** KasWare: imports `words` (array of 12) with a throw-away password through the onboarding tab; resolves with the wallet page. */
export async function onboardKasware(ctx, words, { password = 'Kob-Test-12345' } = {}) {
  if (!Array.isArray(words) || words.length !== 12) throw new Error('onboardKasware: words must be an array of 12 words');
  let [sw] = ctx.serviceWorkers();
  if (!sw) sw = await ctx.waitForEvent('serviceworker', { timeout: 30_000 });
  let page;
  for (let i = 0; i < 40 && !page; i++) {
    page = ctx.pages().find((p) => p.url().includes('/index.html'));
    if (!page) await sleep(500);
  }
  if (!page) throw new Error('KasWare onboarding tab did not open');
  await clickText(page, 'I already have a wallet');
  await sleep(800);
  await clickText(page, 'Use Password');
  await sleep(800);
  await page.getByPlaceholder('Enter your password').fill(password);
  await page.getByPlaceholder('...and repeat it').fill(password);
  await clickText(page, 'Continue', { exact: false });
  await sleep(1500);
  await clickText(page, 'Import Seed Phrase', { exact: false });
  await sleep(1500);
  const inputs = page.locator('input[type=password]');
  if ((await inputs.count()) !== 12) throw new Error(`expected 12 seed word inputs, got ${await inputs.count()}`);
  for (let i = 0; i < 12; i++) await inputs.nth(i).fill(words[i]);
  await clickText(page, 'Continue');
  await sleep(2500);
  for (let i = 0; i < 25 && !page.url().includes('WalletTabScreen'); i++) {
    await page.getByText('Continue', { exact: true }).first().click({ timeout: 3000 }).catch(() => {});
    await sleep(1500);
  }
  if (!page.url().includes('WalletTabScreen')) throw new Error(`KasWare import did not reach the wallet screen: ${page.url()}`);
  await sleep(1500);
  return page;
}

/** KasWare: Settings -> network -> Testnet 10. Resolves with the wallet page text (contains the kaspatest: address when it worked). */
export async function switchKaswareToTn10(page) {
  await clickText(page, 'Settings');
  await sleep(1200);
  await clickText(page, 'Mainnet');
  await sleep(1200);
  await clickText(page, 'Testnet 10');
  await sleep(3000);
  await clickText(page, 'Wallet');
  await sleep(1500);
  return collapse(await page.innerText('body'));
}

/** Kaspire: imports the mnemonic on wallet.html and selects testnet-10. Resolves with a screenshot path. */
export async function onboardKaspire(ctx, extId, mnemonic, { password }) {
  if (!password) throw new Error('onboardKaspire: a vault password is required');
  const words = mnemonic.trim().split(/\s+/);
  if (words.length !== 12) throw new Error('onboardKaspire: the mnemonic is not 12 words');
  const w = await ctx.newPage();
  await w.goto(`chrome-extension://${extId}/wallet.html`);
  await w.waitForSelector('#first-seed, #network, #password', { timeout: 30_000 });
  if (await w.$('#first-seed')) {
    await w.click('#first-seed');
    await w.waitForSelector('#submit');
    await w.click('button[data-count="12"]');
    await w.waitForSelector('[data-word="11"]');
    for (let i = 0; i < 12; i++) await w.fill(`[data-word="${i}"]`, words[i]);
    await w.fill('#password', password);
    await w.fill('#password-confirm', password);
    await w.click('#submit');
    await w.waitForSelector('#network', { timeout: 60_000 });
  } else if (await w.$('#password')) {
    await w.fill('#password', password);
    await w.click('#unlock, button:has-text("UNLOCK")');
    await w.waitForSelector('#network', { timeout: 30_000 });
  }
  if (!/TN10/i.test((await w.textContent('#network')) ?? '')) {
    await w.click('#network');
    await w.click('[data-network="testnet-10"]');
    await w.waitForFunction(() => /TN10/i.test(document.querySelector('#network')?.textContent ?? ''), null, { timeout: 30_000 });
  }
  const file = await shot(w, 'kaspire', 'wallet-dashboard-tn10');
  await w.close();
  return file;
}

/** Kastle: imports the mnemonic through the onboarding tab when the profile has no wallet yet. Resolves true when it onboarded. */
export async function onboardKastle(ctx, extId, { mnemonic, password }) {
  if (!password) throw new Error('onboardKastle: a wallet password is required');
  const words = mnemonic.trim().split(/\s+/);
  if (words.length !== 12) throw new Error('onboardKastle: the mnemonic is not 12 words');
  // opening popup.html on a profile without a wallet makes the background worker open an /onboarding tab a few seconds later
  const probe = await ctx.newPage();
  await probe.goto(`chrome-extension://${extId}/popup.html`);
  const onboarding = () => ctx.pages().filter((x) => x.url().includes('#/onboarding'));
  for (let i = 0; i < 60 && !onboarding().length; i++) await sleep(250);
  const [p, ...extra] = onboarding();
  if (!p) {
    await probe.close();
    return false;
  }
  for (const x of [probe, ...extra]) await x.close().catch(() => {});
  await p.getByText('Import existing wallet').click();
  await p.locator('input').nth(0).fill(password);
  await p.locator('input').nth(1).fill(password);
  await p.locator('input[type=checkbox]').check();
  await p.getByRole('button', { name: 'Next' }).click();
  await p.getByText('Use a 12- or 24-word recovery phrase.').click();
  for (let i = 0; i < 12; i++) await p.locator('input[type=password]').nth(i).fill(words[i]);
  await p.getByRole('button', { name: 'Import Wallet' }).click();
  await p.waitForSelector('text=Import Accounts', { timeout: 60_000 });
  await p.locator('input[type=checkbox]').first().waitFor({ timeout: 60_000 });
  await sleep(1500);
  if (!(await p.locator('input[type=checkbox]').first().isChecked())) await p.locator('input[type=checkbox]').first().check();
  await p.getByRole('button', { name: 'Import Wallet' }).click();
  try {
    await p.waitForSelector('text=successfully imported', { timeout: 60_000 });
  } catch {
    await shot(p, 'kastle', 'onboard-fail');
    throw new Error('Kastle onboarding did not finish; see out/screens/kastle/');
  }
  await p.close();
  return true;
}
