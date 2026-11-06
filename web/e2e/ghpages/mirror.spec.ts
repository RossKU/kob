// The GitHub Pages mirror build (KOB_MIRROR=github-pages, src/app/mirror.ts) served under the sub-path /kob/ with no response headers, as
// https://rossku.github.io/kob/ is: playwright.ghpages.config.ts. Checks: it loads from the sub-path (no request leaves /kob/), the market works
// with the mock stack, the CSP is in a <meta> (the policy of public/_headers minus what a meta cannot carry), a framed page shows only the frame
// notice, and the private-key option is absent everywhere (Connect, Settings, the signer page).
import type { Page } from '@playwright/test';
import { devices } from '@playwright/test';
import { CSP_HEADER, CSP_META, SIGNER_CSP_META } from '../../csp.mjs';
import { expect, test } from '../fixtures';

const BASE = '/kob/';

test.use({ walletId: null, appPath: `${BASE}#/market` });

/** The CSP violations the page reported (an init script listens before any of the page's own scripts run). */
const violations = (page: Page) => page.evaluate(() => (window as unknown as { __cspViolations?: string[] }).__cspViolations ?? []);

test.beforeEach(async ({ page }) => {
  await page.addInitScript(() => {
    const w = window as unknown as { __cspViolations?: string[] };
    w.__cspViolations = [];
    document.addEventListener('securitypolicyviolation', (e) => w.__cspViolations!.push(`${e.violatedDirective} ${e.blockedURI}`));
  });
});

/** Every request to the mirror's host stays under /kob/, and none of them fails (but config.json: the deploy writes it, the mock run injects its config). */
function watchRequests(page: Page, origin: string) {
  const outside: string[] = [];
  const failed: string[] = [];
  page.on('request', (r) => {
    const u = new URL(r.url());
    if (u.origin === origin && !u.pathname.startsWith(BASE)) outside.push(u.pathname);
  });
  page.on('response', (r) => {
    if (new URL(r.url()).origin === origin && r.status() >= 400 && new URL(r.url()).pathname !== `${BASE}config.json`) failed.push(`${r.status()} ${r.url()}`);
  });
  return { outside, failed };
}

test('the build ships its CSP as a <meta>: the policy of public/_headers without frame-ancestors (index.html and signer.html)', async ({ request }) => {
  const index = await (await request.get(`${BASE}`)).text();
  const metas = [...index.matchAll(/<meta http-equiv="Content-Security-Policy" content="([^"]*)"/g)].map((m) => m[1]!.replace(/&#39;|&apos;/g, "'"));
  expect(metas).toEqual([CSP_META]);
  expect(`${metas[0]}; frame-ancestors 'none'`).toBe(CSP_HEADER);
  expect(metas[0]).toContain("require-trusted-types-for 'script'; trusted-types default");
  const signer = await (await request.get(`${BASE}signer.html`)).text();
  const signerMetas = [...signer.matchAll(/<meta http-equiv="Content-Security-Policy" content="([^"]*)"/g)].map((m) => m[1]!.replace(/&#39;|&apos;/g, "'"));
  expect(signerMetas).toEqual([SIGNER_CSP_META]);
  // relative asset and icon URLs (a project site lives under a sub-path), and no inline script
  expect(index).not.toMatch(/(?:src|href)="\/(?!\/)/);
  expect(/<script\b(?![^>]*\bsrc=)[^>]*>\s*\S/i.test(index)).toBe(false);
  const manifest = await request.get(`${BASE}manifest.webmanifest`);
  expect(manifest.status()).toBe(200);
  expect((await manifest.json()).start_url).toBe('./#/market');
});

test('the market loads under /kob/ with the mock stack, every request stays under /kob/, no CSP violation', async ({ page, appPage, mock, pageErrors, baseURL }) => {
  const watched = watchRequests(page, new URL(baseURL!).origin);
  await appPage.reload();
  const tok = await mock.token();
  const row = appPage.getByTestId(`token-row-${tok.covenant_id}`);
  await expect(row).toBeVisible();
  await expect(row.getByTestId(`token-link-${tok.covenant_id}`)).toHaveText('EXKCC');
  // the token page with its book
  await row.getByTestId(`token-link-${tok.covenant_id}`).click();
  await expect(appPage).toHaveURL(new RegExp(`/kob/#/market/${tok.covenant_id}$`));
  await expect(appPage.getByTestId('book-asks')).toBeVisible();
  await expect(appPage.getByTestId('book-bids')).toBeVisible();
  expect(watched.outside).toEqual([]);
  expect(watched.failed).toEqual([]);
  expect(await violations(appPage)).toEqual([]);
  expect(pageErrors.filter((e) => /Content Security Policy|Trusted Type|Refused to/i.test(e))).toEqual([]);
  await expect(appPage.getByTestId('frame-guard')).toHaveCount(0);
});

test('no private-key option: Connect without an extension says why in one line, Settings has the line instead of the button', async ({ appPage }) => {
  const connect = appPage.getByTestId('wallet-connect-local-key');
  await expect(connect).toBeVisible();
  await expect(connect).toHaveAttribute('data-state', 'unavailable');
  await connect.click();
  await expect(appPage.getByTestId('wallet-key-mirror')).toHaveText(/Private-key signing is off on this mirror/);
  await expect(appPage.getByTestId('signer-overlay')).toHaveCount(0);
  await expect(appPage.locator('iframe')).toHaveCount(0);
  await appPage.keyboard.press('Escape');
  await appPage.goto(`${BASE}#/settings`);
  await expect(appPage.getByTestId('settings-private-key-mirror')).toHaveText(/Private-key signing is off on this mirror/);
  await expect(appPage.getByTestId('settings-private-key')).toHaveCount(0);
  await appPage.goto(`${BASE}#/create`);
  await expect(appPage.getByTestId('issue-connect-mirror')).toBeVisible();
  await expect(appPage.getByTestId('issue-connect-none')).toHaveCount(0);
  await expect(appPage.locator('iframe')).toHaveCount(0);
});

test('the signer page never offers key entry: on its own and framed it shows only the reason and a link to the app', async ({ page }) => {
  await page.goto(`${BASE}signer.html`);
  await expect(page.getByTestId('frame-guard')).toContainText('Private key is not available on this mirror');
  await expect(page.getByTestId('frame-guard-link')).toHaveAttribute('href', /\/kob\/$/);
  await expect(page.locator('input, textarea, button')).toHaveCount(0);
  expect(await violations(page)).toEqual([]);
});

test('framed by another page, the app and the signer show only the frame notice and a link out', async ({ page, appPage, mock, baseURL }) => {
  const appUrl = `${baseURL}${BASE}#/market`;
  // the framing page: another origin (the mock server's), without the app's CSP
  await page.goto(`${mock.url}/v1/health`);
  await page.setContent(`<iframe id="app" src="${appUrl}" width="900" height="600"></iframe><iframe id="signer" src="${baseURL}${BASE}signer.html" width="500" height="300"></iframe>`);
  const app = appPage.frameLocator('#app');
  await expect(app.getByTestId('frame-guard')).toContainText('KOB cannot be used inside a frame');
  await expect(app.getByTestId('frame-guard-link')).toHaveAttribute('href', appUrl);
  await expect(app.getByTestId('frame-guard-link')).toHaveAttribute('target', '_blank');
  await expect(app.locator('input, textarea, button, select')).toHaveCount(0);
  await expect(app.locator('#app > *')).toHaveCount(1);
  const signer = appPage.frameLocator('#signer');
  await expect(signer.getByTestId('frame-guard')).toContainText('Private key is not available on this mirror');
  await expect(signer.locator('input, textarea, button')).toHaveCount(0);
});

test.describe('phone (Pixel 7)', () => {
  const { defaultBrowserType: _ignored, ...pixel7 } = devices['Pixel 7'];
  test.use(pixel7);

  test('the market loads and Connect says why there is no private key', async ({ appPage, mock }) => {
    const tok = await mock.token();
    await expect(appPage.getByTestId(`token-row-${tok.covenant_id}`)).toBeVisible();
    await appPage.getByTestId('wallet-connect').click();
    await expect(appPage.getByTestId('wallet-key-mirror')).toBeVisible();
    await expect(appPage.locator('iframe')).toHaveCount(0);
    expect(await violations(appPage)).toEqual([]);
  });
});
