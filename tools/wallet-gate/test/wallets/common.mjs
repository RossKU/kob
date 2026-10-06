// Shared helpers for real-wallet automation drivers (test/wallets/<wallet>.mjs).
// A driver: launches Playwright Chromium with the wallet extension loaded in a persistent profile, imports a test
// mnemonic (from .env, never printed), switches the wallet to testnet-10, opens the gate page, runs setup for the
// wallet address, then calls window.__gate.runTest('T1'|'T2'|'T3', opts) while approving the wallet popups.
import { spawn, spawnSync } from 'node:child_process';
import { mkdirSync, writeFileSync, existsSync, readFileSync } from 'node:fs';
import { join } from 'node:path';
import { chromium } from 'playwright-core';
import { ROOT } from '../../lib/node-kaspa.mjs';
import { loadEnv, upsertEnv } from '../../lib/env.mjs';

export { chromium, ROOT, loadEnv, upsertEnv };
export const OUT = join(ROOT, 'out');
export const shot = async (page, dir, name) => {
  mkdirSync(join(OUT, 'screens', dir), { recursive: true });
  const f = join(OUT, 'screens', dir, `${Date.now()}-${name}.png`);
  await page.screenshot({ path: f, fullPage: true }).catch(() => {});
  return f;
};

/** Starts scripts/serve.mjs on `port`; returns { url, stop }. */
export async function startServer(port) {
  const p = spawn(process.execPath, [join(ROOT, 'scripts', 'serve.mjs'), String(port)], { stdio: 'inherit' });
  await new Promise((r) => setTimeout(r, 1200));
  return { url: `http://localhost:${port}/`, stop: () => p.kill() };
}

/**
 * Launches Chromium with an unpacked extension in a persistent profile (extensions need a headed or new-headless browser).
 * NOTE: branded Chrome >= 137 ignores --load-extension; use Playwright's bundled Chromium (npx playwright-core install chromium).
 */
export async function launchWithExtension({ extensionDir, profileName, headless = false, extraArgs = [] }) {
  const userDataDir = join(ROOT, '.browser-profiles', profileName);
  mkdirSync(userDataDir, { recursive: true });
  const ctx = await chromium.launchPersistentContext(userDataDir, {
    headless,
    args: [`--disable-extensions-except=${extensionDir}`, `--load-extension=${extensionDir}`, ...(headless ? ['--headless=new'] : []), ...extraArgs],
    viewport: { width: 1280, height: 900 },
  });
  return ctx;
}

/** Finds the extension id of an unpacked MV3 extension via its service worker URL. */
export async function extensionId(ctx, timeoutMs = 20000) {
  let [sw] = ctx.serviceWorkers();
  if (!sw) sw = await ctx.waitForEvent('serviceworker', { timeout: timeoutMs });
  return new URL(sw.url()).host;
}

/** Runs scripts/setup.mjs for a wallet address (spends dev funds; serialized by a lock). Returns the setup JSON. */
export function runSetup(address, label) {
  const r = spawnSync(process.execPath, [join(ROOT, 'scripts', 'setup.mjs'), '--wallet-address', address, '--label', label], { encoding: 'utf8', cwd: ROOT });
  process.stdout.write(r.stdout ?? '');
  if (r.status !== 0) throw new Error('setup failed: ' + (r.stderr || r.stdout));
  const pub = /setup-([0-9a-f]{64})\.json/.exec(r.stdout)[1];
  return JSON.parse(readFileSync(join(ROOT, 'state', `setup-${pub}.json`), 'utf8'));
}

/** Calls the page's runTest and returns the record (strips the bulky unsigned tx). */
export async function runGateTest(page, id, opts = {}) {
  const rec = await page.evaluate(([id, opts]) => window.__gate.runTest(id, opts), [id, opts]);
  const { unsignedTxJson, ...slim } = rec;
  return slim;
}

export function saveResults(name, obj) {
  mkdirSync(OUT, { recursive: true });
  const f = join(OUT, `wallet-${name}.json`);
  writeFileSync(f, JSON.stringify(obj, null, 2));
  return f;
}
