// The landing-only build (KOB_LANDING_ONLY=1, src/app/landing-only.ts): the production front page of a network that has not launched, served from dist-landing/ by
// playwright.landing.config.ts (no mock stack on purpose: the page must not talk to any indexer or node). Checks: every address shows the landing page, there
// is no way into the trading app (no Connect, no "Open the app", no app links), the testnet link is there, and nothing but the page's own files is requested.
import type { Page, Request } from '@playwright/test';
import { expect, test } from '@playwright/test';
import { LANDING_CSP_META } from '../../csp.mjs';

const TESTNET_APP = 'https://kob-tn10.kaspa.ag/#/market';
const COVENANT = 'a'.repeat(64);

/** Every request the page makes, and the WebSockets it opens. */
function watch(page: Page) {
  const requests: Request[] = [];
  const sockets: string[] = [];
  page.on('request', (r) => requests.push(r));
  page.on('websocket', (w) => sockets.push(w.url()));
  return { requests, sockets };
}

test.beforeEach(async ({ page }) => {
  // an indexer and a node that the page, were it the app, would be configured to use: it must never ask them
  await page.addInitScript(() => {
    (window as unknown as { __KOB_CONFIG__: unknown }).__KOB_CONFIG__ = { network: 'mainnet', indexerUrl: 'http://127.0.0.1:8790', nodeUrl: 'ws://127.0.0.1:8790', features: { test: true } };
    const w = window as unknown as { __cspViolations?: string[] };
    w.__cspViolations = [];
    document.addEventListener('securitypolicyviolation', (e) => w.__cspViolations!.push(`${e.violatedDirective} ${e.blockedURI}`));
  });
  // KOB_WIDE_FONTS=1: every page in a font as wide as the CI runner's (see e2e/fixtures.ts), to check the header on a desktop that has not got that font
  if (process.env.KOB_WIDE_FONTS === '1') {
    await page.addInitScript(() => {
      document.addEventListener('DOMContentLoaded', () => {
        const s = document.createElement('style');
        s.textContent = 'html, body, button, input, select, textarea, a, span, h1, h2, h3, p { font-family: Verdana, "DejaVu Sans", sans-serif !important; }';
        document.head.append(s);
      });
    });
  }
});

const noScroll = (page: Page) => page.evaluate(() => document.documentElement.scrollWidth <= window.innerWidth);

test('#/market shows the landing page, cleans the address, and offers no way into the app', async ({ page }) => {
  await page.goto('/#/market');
  await expect(page.getByTestId('landing')).toBeVisible();
  await expect(page.getByTestId('landing')).toHaveAttribute('data-mode', 'soon');
  await expect(page.getByRole('heading', { level: 1, name: 'Decentralized order book on Kaspa.' })).toBeVisible();
  await expect(page).toHaveURL(/\/$/); // #/market is gone from the address
  // no wallet, no app shell, no market list
  await expect(page.getByRole('button', { name: /connect/i })).toHaveCount(0);
  await expect(page.getByText(/connect (a )?wallet/i)).toHaveCount(0);
  for (const id of ['wallet-connect-list', 'wallet-connect', 'brand', 'nav-market', 'notify-bell', 'token-list-view', 'network-chip', 'network-badge']) await expect(page.getByTestId(id)).toHaveCount(0);
  await expect(page.getByRole('link', { name: /open the app/i })).toHaveCount(0);
  // the way forward is the testnet app
  await expect(page.getByTestId('landing-open-app')).toHaveAttribute('href', TESTNET_APP);
  await expect(page.getByTestId('landing-cta').first()).toHaveAttribute('href', TESTNET_APP);
  await expect(page.getByTestId('landing-open-app')).toHaveText('Try it on testnet');
  await expect(page.getByTestId('landing-soon-chip')).toHaveText('Mainnet — in preparation');
  await expect(page.getByTestId('landing-chip')).toHaveText('Mainnet — in preparation');
  await expect(page.getByTestId('landing-soon-note')).toContainText('Mainnet is being built');
  await expect(page.getByTestId('footer-testnet-app')).toHaveAttribute('href', TESTNET_APP);
  // no link anywhere is an in-app route (a hash route would show this same page, but a link to it would promise an app)
  const hrefs = await page.locator('a[href]').evaluateAll((as) => as.map((a) => a.getAttribute('href') ?? ''));
  const toApp = hrefs.filter((h) => h.includes('#/market'));
  expect(toApp.length, 'the header button, the two calls to action and the footer').toBe(4);
  expect(new Set(toApp)).toEqual(new Set([TESTNET_APP]));
  expect(hrefs.filter((h) => h.startsWith('#/') && h !== '#/')).toEqual([]);
  expect(hrefs.filter((h) => /\/\/[^/]*kob\.kaspa\.ag/.test(h))).toEqual([]);
});

test('every app route shows the landing page too, also when the hash changes while the page is open', async ({ page }) => {
  for (const hash of ['#/market', `#/market/${COVENANT}`, `#/token/${COVENANT}`, `#/pair/${COVENANT}/${'b'.repeat(64)}`, '#/activity', '#/orders', '#/create', '#/settings', '#/disclaimer', '#/support', '#/usd/kas', '#/nope', '#nope']) {
    await page.goto('about:blank');
    await page.goto(`/${hash}`);
    await expect(page.getByTestId('landing'), hash).toBeVisible();
    await expect(page.getByTestId('landing-h1'), hash).toBeVisible();
    await expect(page.getByRole('button', { name: /connect/i }), hash).toHaveCount(0);
    expect(new URL(page.url()).hash, hash).toBe('');
  }
  await page.goto('/');
  await page.evaluate(() => {
    location.hash = '#/orders';
  });
  await expect.poll(() => new URL(page.url()).hash).toBe('');
  await expect(page.getByTestId('landing')).toBeVisible();
  await expect(page.getByTestId('main')).toHaveAttribute('data-route', '#/');
});

test('nothing but the page itself is requested: no indexer, no node, no config, no wasm, no WebSocket, no service worker', async ({ page, baseURL }) => {
  const { requests, sockets } = watch(page);
  await page.goto('/#/market');
  await expect(page.getByTestId('landing')).toBeVisible();
  // scroll the page through (the reveal motion) and let it settle: nothing may be fetched later either
  for (const y of [600, 1800, 4000, 0]) {
    await page.evaluate((top) => window.scrollTo(0, top), y);
    await page.waitForTimeout(100);
  }
  await page.waitForTimeout(500);
  const origin = new URL(baseURL!).origin;
  const urls = requests.map((r) => new URL(r.url()));
  expect(urls.filter((u) => u.origin !== origin).map((u) => u.href), 'no request leaves the site').toEqual([]);
  const paths = urls.map((u) => u.pathname);
  expect(paths.filter((p) => /\/v1\/|\/node\/|wrpc|config\.json|registry|\.wasm|signer|\.well-known|favicon\.ico/.test(p)), 'no indexer, node, config, registry, wasm or signer').toEqual([]);
  // only the page's own files: the document, one script, one stylesheet, the icons and the manifest
  expect(paths.filter((p) => p !== '/' && !/^\/(assets\/main-[\w-]+\.(js|css)|favicon\.svg|apple-touch-icon\.png|manifest\.webmanifest|icon-\d+\.png)$/.test(p))).toEqual([]);
  expect(paths.filter((p) => p.endsWith('.js'))).toHaveLength(1);
  expect(sockets).toEqual([]);
  expect(await page.evaluate(() => navigator.serviceWorker.getRegistrations().then((r) => r.length))).toBe(0);
  expect(await page.evaluate(() => (window as unknown as { __kob?: unknown }).__kob), 'no test hook of the app').toBeUndefined();
  expect(await page.evaluate(() => (window as unknown as { __cspViolations: string[] }).__cspViolations)).toEqual([]);
});

test('the page is its own policy and shell: connect-src none, no signer page, a manifest that opens the site', async ({ request }) => {
  const index = await (await request.get('/')).text();
  const metas = [...index.matchAll(/<meta http-equiv="Content-Security-Policy" content="([^"]*)"/g)].map((m) => m[1]!.replace(/&#39;|&apos;/g, "'"));
  expect(metas).toEqual([LANDING_CSP_META]);
  expect(metas[0]).toContain("connect-src 'none'");
  expect(metas[0]).toContain("frame-src 'none'");
  expect(metas[0]).toContain("require-trusted-types-for 'script'");
  expect((await request.get('/signer.html')).status()).toBe(404);
  const manifest = await (await request.get('/manifest.webmanifest')).json();
  expect(manifest.start_url).toBe('./');
  expect(JSON.stringify(manifest)).not.toContain('#/market');
  expect(manifest.description).toContain('Mainnet is in preparation');
  expect(index).toContain('Mainnet is in preparation');
  expect(index).not.toMatch(/Buy and sell Kaspa tokens/);
});

test('the copy never claims mainnet is live: the terminal shows testnet-10, the sentences say "in preparation"', async ({ page }) => {
  await page.goto('/');
  const bots = page.getByTestId('landing-bots');
  await bots.scrollIntoViewIfNeeded();
  const terminal = await bots.locator('pre').innerText();
  expect(terminal).toContain('--network testnet-10');
  expect(terminal).not.toMatch(/--network\s+mainnet/);
  await page.getByTestId('footer').scrollIntoViewIfNeeded();
  await expect(page.getByTestId('landing-disclaimer')).toContainText('Mainnet · in preparation.');
  await expect(page.getByTestId('landing-disclaimer')).toContainText('Nothing here can lock funds yet');
  await expect(page.getByTestId('landing-disclaimer')).not.toContainText('Real funds are at risk');
  await expect(page.getByTestId('landing-audit')).toContainText('Mainnet is being built');
  const text = await page.locator('body').innerText();
  expect(text).not.toMatch(/real funds|is live on mainnet|mainnet is live|open the app|connect wallet|launch app/i);
  // the footer: the testnet app, GitHub, Run an executor; nothing that leads into an app that is not here
  await expect(page.getByTestId('footer')).toHaveAttribute('data-context', 'landing');
  await expect(page.getByTestId('footer').getByRole('link')).toHaveText(['KOB', 'Try it on testnet', 'GitHub', 'Run an executor', 'Security policy']);
  await expect(page.getByTestId('footer-chart-notice')).toHaveCount(0);
  await expect(page.getByTestId('footer-indexer-trust')).toHaveCount(0);
  // the trust model is still all there
  await expect(page.locator('.lp-ledger .lp-lr .lp-k')).toHaveText(['Admin keys', 'Oracle', 'Custody', 'Operator', 'Off switch', 'Closed source', 'Trust us']);
  await expect(page.getByTestId('landing-feedback-security')).toBeVisible();
});

for (const [width, height] of [
  [1440, 900],
  [1024, 800],
  [390, 844],
  [360, 740],
  [320, 640],
] as const) {
  test(`${width} px: the header (brand, chip, Try it on testnet, theme) fits in one row and the page does not scroll sideways`, async ({ page }) => {
    await page.setViewportSize({ width, height });
    await page.goto('/#/market');
    await expect(page.getByTestId('landing-nav')).toBeVisible();
    await expect(page.locator('header')).toHaveCount(1);
    const nav = (await page.getByTestId('landing-nav').boundingBox())!;
    expect(nav.height).toBe(width < 720 ? 52 : 56);
    // the button and the toggle are inside the window, and on one line with the brand
    const btn = (await page.getByTestId('landing-open-app').boundingBox())!;
    const toggle = (await page.getByTestId('theme-toggle').boundingBox())!;
    const brand = (await page.getByTestId('landing-brand').boundingBox())!;
    for (const b of [btn, toggle, brand]) {
      expect(b.x).toBeGreaterThanOrEqual(0);
      expect(b.x + b.width).toBeLessThanOrEqual(width);
      expect(b.y + b.height).toBeLessThanOrEqual(nav.height);
    }
    expect(btn.x, 'the button is after the brand').toBeGreaterThan(brand.x + brand.width);
    expect(toggle.x, 'the toggle is after the button').toBeGreaterThan(btn.x + btn.width);
    await expect(page.getByTestId('landing-open-app')).toHaveText(width < 560 ? 'Testnet app' : 'Try it on testnet');
    const chip = page.getByTestId('landing-soon-chip');
    if (width < 360) await expect(chip).toBeHidden();
    else {
      await expect(chip).toBeVisible();
      await expect(chip).toHaveText(width < 1200 ? 'In preparation' : 'Mainnet — in preparation');
      const c = (await chip.boundingBox())!;
      expect(c.x).toBeGreaterThan(brand.x + brand.width - 1);
      expect(c.x + c.width).toBeLessThanOrEqual(btn.x);
    }
    // the hero says it in any case, beside the kicker
    await expect(page.getByTestId('landing-chip')).toBeVisible();
    expect(await noScroll(page), 'no horizontal scroll at the top').toBe(true);
    await page.getByTestId('footer').scrollIntoViewIfNeeded();
    expect(await noScroll(page), 'no horizontal scroll at the bottom').toBe(true);
  });
}

test('the theme toggle works without the app (one stored preference)', async ({ page }) => {
  await page.addInitScript(() => localStorage.getItem('kob.theme') ?? localStorage.setItem('kob.theme', 'dark'));
  await page.goto('/');
  const html = page.locator('html');
  await expect(html).toHaveAttribute('data-theme', 'dark');
  await page.getByTestId('theme-toggle').click();
  await expect(html).toHaveAttribute('data-theme', 'light');
  await expect(page.getByTestId('landing')).toBeVisible();
});
