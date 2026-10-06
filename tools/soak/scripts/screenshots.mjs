// Captures the web UI on live TN10 data (the soak's UI server, scripts/serve-ui.mjs) with Playwright's Chromium from web/node_modules.
// The app has one market page with BASE / QUOTE selectors: a token's KAS book (`#/market/<id>`, TOKEN/KAS by convention, KAS/TUSD for the
// USD reference token TUSD) or a token/token pair (`#/pair/<base>/<quote>`, settled through the two KAS books). The set:
//
//   list                   the market list (`#/market`)
//   market-<T>             every soak token's market page, dark, and the same page flipped (the `market-flip` toggle: KAS/TOKEN), the
//                          first token also light, at 1280 px and on a phone
//   pair-<B>-<Q>           every pair of a soak token with TUSD (direct cross-limit levels and the route through the two KAS books)
//   orders                 "My orders" of a soak trader (t1, read through a test wallet that never signs)
//   confirm                the pre-sign confirmation of a far-from-market limit buy worth about 20 KAS on the second token's market (never signed)
//
// The orders and confirm shots inject the web app's test wallet (web/src/testing/mock-wallets.ts, `features.test` through
// `window.__KOB_CONFIG__`, both only in this local browser) with the trader's key from run/keys.json. The wallet is configured to REFUSE
// every signature request (`approve: false`) and the script never presses Sign, so nothing is broadcast. The key never leaves this process
// and the browser it starts (it is not printed and not written anywhere).
//
// Every shot prints the page's state (chart state / bars / inversion, book rows, pair levels, order rows).
//
//   node scripts/screenshots.mjs [--url http://127.0.0.1:8490] [--out screenshots] [--only <name part>] [--tag public] [--no-wallet]
import { createRequire } from 'node:module';
import { mkdirSync, readFileSync } from 'node:fs';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath, pathToFileURL } from 'node:url';

const HERE = dirname(fileURLToPath(import.meta.url));
const SOAK = resolve(HERE, '..');
const WEB = join(SOAK, '..', '..', 'web');
const require = createRequire(join(WEB, 'package.json'));
const { chromium } = require('playwright');
const argv = process.argv.slice(2);
const opt = (n, d) => (argv.includes(n) ? argv[argv.indexOf(n) + 1] : d);
const base = opt('--url', 'http://127.0.0.1:8490').replace(/\/$/, '');
const out = resolve(opt('--out', join(SOAK, 'screenshots')));
const only = opt('--only', '');
const tag = opt('--tag', '');
const withWallet = !argv.includes('--no-wallet');
const stamp = new Date().toISOString().slice(0, 16).replace(/[-:T]/g, '');
const state = JSON.parse(readFileSync(join(SOAK, 'run', 'state.json'), 'utf8'));
const tokens = [state.token, state.token2, state.token3].filter(Boolean);
const [usd] = tokens;
mkdirSync(out, { recursive: true });

// the test wallet (web/src/testing): Node strips the TypeScript types on import
const walletInit = async (secretKey) => {
  const { mockWalletInitScript, DEFAULT_WALLET_CONFIG, WALLET_DEFAULTS } = await import(pathToFileURL(join(WEB, 'src', 'testing', 'mock-wallets.ts')).href);
  const { pubkeyOf } = await import(pathToFileURL(join(WEB, 'src', 'testing', 'local-signer.ts')).href);
  const { addressOfSpk, p2pkSpk } = await import(pathToFileURL(join(WEB, 'mock', 'address.mjs')).href);
  const pubkey = pubkeyOf(secretKey);
  const spk = p2pkSpk(pubkey);
  const cfg = {
    ...DEFAULT_WALLET_CONFIG,
    ...WALLET_DEFAULTS.kasware,
    wallet: 'kasware',
    secretKey,
    pubkey,
    approve: false,
    rejectMessage: 'screenshot wallet: signing disabled',
    addresses: { 'testnet-10': addressOfSpk('kaspatest', spk), mainnet: addressOfSpk('kaspa', spk) },
  };
  return { script: mockWalletInitScript(cfg), address: cfg.addresses['testnet-10'] };
};

const browser = await chromium.launch();

async function open(name, { width = 1600, height = 1000, theme = 'dark', hash, wallet = null }) {
  const ctx = await browser.newContext({ viewport: { width, height }, deviceScaleFactor: width < 600 ? 2 : 1, colorScheme: theme });
  const page = await ctx.newPage();
  await page.addInitScript((t) => localStorage.setItem('kob.theme', t), theme);
  if (wallet) {
    await page.addInitScript(() => {
      window.__KOB_CONFIG__ = { features: { test: true } };
    });
    await page.addInitScript({ content: wallet.script });
  }
  await page.goto(`${base}/${hash}`);
  return { ctx, page };
}

async function readyChart(page, { book = true } = {}) {
  await page.getByTestId('price-chart').waitFor({ timeout: 30_000 }).catch(() => {});
  await page.waitForFunction(() => document.querySelector('[data-testid="price-chart"]')?.getAttribute('data-state') === 'ready', null, { timeout: 30_000 }).catch(() => {});
  if (!book) return;
  // near-zero spreads: the book is replaced by "matching in progress" while a trader's order crosses the touch (until a matcher fills it,
  // a few seconds): wait for both sides to be listed again
  await page
    .waitForFunction(
      () => document.querySelectorAll('[data-testid="book-bids"] [data-testid^="book-"]').length > 0 && document.querySelectorAll('[data-testid="book-asks"] [data-testid^="book-"]').length > 0,
      null,
      { timeout: 60_000 },
    )
    .catch(() => {});
}

async function info(page) {
  return page.evaluate(() => {
    const q = (s) => document.querySelector(s);
    const c = q('[data-testid="price-chart"]');
    return {
      // the market page's BASE / QUOTE selectors (the page title), else its plain pair label
      pair: (() => {
        const sel = [...document.querySelectorAll('select')].filter((s) => /^(base|quote)$/i.test(s.getAttribute('aria-label') ?? '') || /market-(base|quote)/.test(s.dataset.testid ?? ''));
        if (sel.length === 2) return sel.map((s) => s.selectedOptions[0]?.textContent?.split(' ')[0]).join('/');
        return q('[data-testid="market-pair"]')?.textContent?.split('▾')[0] ?? null;
      })(),
      flip: q('[data-testid="market-flip"]')?.getAttribute('aria-pressed') ?? null,
      chart: c ? { state: c.getAttribute('data-state'), bars: c.getAttribute('data-bars'), inverted: c.getAttribute('data-inverted') } : null,
      asks: document.querySelectorAll('[data-testid="book-asks"] [data-testid^="book-"]').length,
      bids: document.querySelectorAll('[data-testid="book-bids"] [data-testid^="book-"]').length,
      pairLevels: document.querySelectorAll('[data-testid="pair-level"]').length,
      listRows: document.querySelectorAll('[data-testid^="token-row-"]').length,
      orderRows: document.querySelectorAll('[data-testid^="order-row-"]').length,
      confirm: q('[data-testid="confirm-screen"]') ? (q('[data-testid="confirm-summary"]')?.textContent ?? '').replace(/\s+/g, ' ').slice(0, 160) : null,
      registryNotice: !!q('[data-testid^="registry-"]') || /custom token registry/i.test(document.body.innerText),
    };
  });
}

async function save(page, name, fullPage = false) {
  await page.waitForTimeout(2500);
  const path = join(out, `${stamp}-${tag ? `${tag}-` : ''}${name}.png`);
  await page.screenshot({ path, fullPage });
  console.log(path, JSON.stringify(await info(page)));
}

const want = (name) => !only || name.includes(only);

async function list() {
  if (!want('list')) return;
  const { ctx, page } = await open('list', { hash: '#/market' });
  await page.getByTestId('token-list').waitFor({ timeout: 30_000 }).catch(() => {});
  await page.locator('[data-testid^="token-row-"]').nth(tokens.length - 1).waitFor({ timeout: 30_000 }).catch(() => {});
  await save(page, 'list-dark-1600');
  await ctx.close();
}

async function market(t, { flipped = false, theme = 'dark', width = 1600, height = 1000, suffix = '' } = {}) {
  const name = `market-${t.ticker.toLowerCase()}${flipped ? '-flipped' : ''}-${theme}-${width < 600 ? 'phone' : width}${suffix}`;
  if (!want(name)) return;
  const { ctx, page } = await open(name, { width, height, theme, hash: `#/market/${t.covenantId}` });
  await readyChart(page);
  if (flipped) {
    await page.getByTestId('market-flip').click();
    await page.waitForFunction(() => document.querySelector('[data-testid="market-flip"]')?.getAttribute('aria-pressed') === 'true', null, { timeout: 10_000 }).catch(() => {});
    await readyChart(page);
  }
  await save(page, name, width < 600);
  await ctx.close();
}

async function pair(b, q, { theme = 'dark', width = 1600, height = 1000 } = {}) {
  const name = `pair-${b.ticker.toLowerCase()}-${q.ticker.toLowerCase()}-${theme}-${width < 600 ? 'phone' : width}`;
  if (!want(name)) return;
  const { ctx, page } = await open(name, { width, height, theme, hash: `#/pair/${b.covenantId}/${q.covenantId}` });
  await page.getByTestId('pair-page').waitFor({ timeout: 30_000 }).catch(() => {});
  await page.getByTestId('pair-level').first().waitFor({ timeout: 30_000 }).catch(() => {});
  await readyChart(page, { book: false });
  await save(page, name, width < 600);
  await ctx.close();
}

async function walletShots() {
  if (!withWallet || !(want('orders') || want('confirm'))) return;
  const keys = JSON.parse(readFileSync(join(SOAK, 'run', 'keys.json'), 'utf8'));
  const wallet = await walletInit(keys.t1.secretKey);
  if (want('orders')) {
    const { ctx, page } = await open('orders', { hash: '#/orders', wallet });
    await page.getByTestId('wallet-connect-kasware').click({ timeout: 30_000 }).catch(() => {});
    await page.getByTestId('wallet-address').waitFor({ timeout: 30_000 }).catch(() => {});
    await page.getByTestId('orders-list').waitFor({ timeout: 30_000 }).catch(() => {});
    await page.locator('[data-testid^="order-row-"]').first().waitFor({ timeout: 30_000 }).catch(() => {});
    await save(page, 'orders-dark-1600');
    await ctx.close();
  }
  if (want('confirm')) {
    const t = tokens[1] ?? tokens[0];
    const { ctx, page } = await open('confirm', { height: 1300, hash: `#/market/${t.covenantId}`, wallet });
    await page.getByTestId('wallet-connect-kasware').click({ timeout: 30_000 }).catch(() => {});
    await page.getByTestId('wallet-address').waitFor({ timeout: 30_000 }).catch(() => {});
    await readyChart(page);
    await page.getByTestId('order-type').selectOption('limit');
    await page.getByTestId('order-side-buy').click();
    // about 20 KAS worth, 10 % under the best bid (TOKEN/KAS, the convention): rests far from the market, never crosses the trader's own asks
    // the displayed best bid (KAS per whole token)
    const bidText = await page.locator('[data-testid="book-bids"] [data-testid^="book-"]').first().innerText();
    const price = Number(bidText.trim().split(/\s+/)[0].replace(/[,~]/g, '')) * 0.9;
    // the amount in whole tokens (any amount of base units; the soak tokens have 8 decimals)
    await page.getByTestId('order-amount').fill((20 / price).toFixed(8));
    await page.getByTestId('order-price').fill(price.toFixed(2));
    await page.waitForFunction(() => {
      const tk = document.querySelector('[data-testid="order-ticket"]');
      const btn = document.querySelector('[data-testid="order-review"]');
      return tk?.getAttribute('aria-busy') === 'false' && btn && !btn.disabled;
    }, null, { timeout: 60_000 }).catch(() => {});
    await page.getByTestId('order-review').click({ timeout: 10_000 }).catch(() => {});
    await page.getByTestId('confirm-screen').waitFor({ timeout: 60_000 }).catch(() => {});
    // the confirmation opens over the page: show it from its first line
    await page.getByTestId('confirm-screen').evaluate((el) => el.scrollIntoView({ block: 'start' })).catch(() => {});
    await save(page, 'confirm-dark-1600');
    await ctx.close();
  }
}

await list();
for (const [i, t] of tokens.entries()) {
  await market(t);
  await market(t, { flipped: true });
  if (i === 0) {
    await market(t, { theme: 'light' });
    await market(t, { width: 1280, height: 860 });
    await market(t, { width: 390, height: 844 });
  }
}
for (const b of tokens.slice(1)) await pair(b, usd);
if (tokens[1]) {
  await pair(tokens[1], usd, { theme: 'light' });
  await pair(tokens[1], usd, { width: 390, height: 844 });
}
await walletShots();
await browser.close();
