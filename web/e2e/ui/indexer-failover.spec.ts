// Indexer failover (data/indexer-failover.ts): when the primary indexer is down or not ready, the book, charts and orders are read from a healthy further
// indexer of the host's list ("Using backup indexer <host>"), the page goes back to the primary once it has recovered, and the remaining indexers keep
// cross-checking. The backups here are further mock servers (the same mock as the primary, started in-process on their own ports).
import type { Page, Request } from '@playwright/test';
import { startMockServer } from '../../mock/server.mjs';
import { expect, MockClient, test } from '../fixtures';
import { listExkcc } from '../helpers/env';

test.use({ autoOpen: false });

type Srv = Awaited<ReturnType<typeof startMockServer>>;
const servers: Srv[] = [];
test.afterEach(async () => {
  for (const s of servers.splice(0)) await s.close();
});

/** Another mock indexer (default seed); its book is moved by lifting its best ask level so that it differs from the primary's. */
async function backup(opts: { liftBestAsks?: boolean | number; network?: string } = {}): Promise<{ client: MockClient; host: string; url: string }> {
  const srv = await startMockServer({ port: 0, ...(opts.network ? { network: opts.network } : {}) });
  servers.push(srv);
  const client = new MockClient(srv.url);
  await client.reset();
  if (opts.liftBestAsks) {
    const tok = await client.token();
    const book = await client.book(tok.covenant_id, false, 20);
    // `true`: the best ask level only; a number: that many of the best price levels
    const levels = [...new Set<string>(book.asks.map((a: { price: string }) => a.price))].slice(0, opts.liftBestAsks === true ? 1 : opts.liftBestAsks);
    for (const o of book.asks.filter((a: { price: string }) => levels.includes(a.price))) await client.fill(o.covenant_id, o.amount_left, { taker: 'bob' });
  }
  return { client, host: new URL(srv.url).host, url: srv.url };
}

/** The app is told about further indexers after the default config is injected (their ports are only known once the servers run). */
async function withExtras(page: Page, urls: string[]): Promise<void> {
  await page.addInitScript((u) => {
    (window as any).__KOB_CONFIG__.extraIndexerUrls = u;
  }, urls);
}

const midOf = (page: Page) => page.getByTestId('book-mid').getAttribute('data-value');
/** The origins the page reads books from, as they happen. */
function bookReads(page: Page): { origins: () => string[]; clear: () => void } {
  const seen: Request[] = [];
  page.on('request', (r) => /\/v1\/books\//.test(r.url()) && seen.push(r));
  return { origins: () => seen.map((r) => new URL(r.url()).host), clear: () => void seen.splice(0) };
}

test.describe('indexer failover', () => {
  test('the primary becomes not ready: the book comes from the backup (banner), and from the primary again once it recovered', async ({ appPage: page, mock }) => {
    test.setTimeout(150_000);
    const b = await backup({ liftBestAsks: true });
    await withExtras(page, [b.url]);
    await listExkcc(page);
    const reads = bookReads(page);
    const id = (await mock.token()).covenant_id as string;
    await page.goto(`/#/market/${id}`);
    await expect(page.getByTestId('book-asks').locator('.book-row:not(.book-blank)')).toHaveCount(10);
    const primaryMid = await midOf(page);
    const primaryHost = new URL(mock.url).host;
    await expect(page.getByTestId('banner-backup-indexer')).toHaveCount(0);

    // the primary loses its node: not ready. The page fails over by itself
    await mock.setHealth('node_unavailable');
    const banner = page.getByTestId('banner-backup-indexer');
    await expect(banner).toBeVisible({ timeout: 60_000 });
    await expect(banner).toContainText(`Using backup indexer ${b.host}`);
    await expect(banner).not.toContainText('History may be incomplete');
    // the book on screen is the backup's (its best ask level was lifted): same token, other prices
    await expect.poll(() => midOf(page), { timeout: 30_000 }).not.toBe(primaryMid);
    reads.clear();
    await page.getByTestId('book-refresh').click();
    await expect.poll(() => reads.origins().length).toBeGreaterThan(0);
    expect(new Set(reads.origins())).toEqual(new Set([b.host]));
    // the status bar and the live indicator speak for the indexer in use: no "indexer cannot be reached" banner
    await expect(page.getByTestId('banner-indexer')).toHaveCount(0);

    // the primary recovers: back to it, banner gone
    await mock.setHealth('following');
    await expect(banner).toHaveCount(0, { timeout: 90_000 });
    await expect.poll(() => midOf(page), { timeout: 30_000 }).toBe(primaryMid);
    reads.clear();
    await page.getByTestId('book-refresh').click();
    await expect.poll(() => reads.origins().length).toBeGreaterThan(0);
    // the primary is the indexer again (the backup only cross-checks)
    expect(reads.origins()).toContain(primaryHost);
  });

  test('a backup that started late can serve the live book, and says that history may be incomplete', async ({ appPage: page, mock }) => {
    test.setTimeout(90_000);
    const b = await backup();
    await b.client.setHealth('following', 0, { book_complete: false, history_from_daa: 990 });
    await withExtras(page, [b.url]);
    await mock.setHealth('node_unavailable');
    await listExkcc(page);
    await page.goto('/');
    const banner = page.getByTestId('banner-backup-indexer');
    await expect(banner).toBeVisible({ timeout: 60_000 });
    await expect(banner).toContainText(`Using backup indexer ${b.host}`);
    await expect(banner).toContainText('History may be incomplete');
  });

  test('a backup on another network or of another payload version is never used', async ({ appPage: page, mock }) => {
    test.setTimeout(90_000);
    const wrongNet = await backup({ network: 'mainnet' });
    const wrongPayload = await backup();
    await wrongPayload.client.setHealth('following', 0, { payload_version: 99 });
    await withExtras(page, [wrongNet.url, wrongPayload.url]);
    await mock.setHealth('node_unavailable');
    await page.goto('/');
    // the primary's own banner says what is wrong; no backup banner appears
    await expect(page.getByTestId('banner-indexer')).toBeVisible({ timeout: 60_000 });
    await page.waitForTimeout(8_000);
    await expect(page.getByTestId('banner-backup-indexer')).toHaveCount(0);
  });

  test('on a backup the other indexers still cross-check: a disagreeing one is shown', async ({ appPage: page, mock }) => {
    test.setTimeout(120_000);
    const b1 = await backup();
    const b2 = await backup({ liftBestAsks: 8 }); // most of its asks are gone: its best ask is more than the tolerance away
    await withExtras(page, [b1.url, b2.url]);
    await listExkcc(page);
    const id = (await mock.token()).covenant_id as string;
    await mock.setHealth('node_unavailable');
    await page.goto(`/#/market/${id}`);
    await expect(page.getByTestId('banner-backup-indexer')).toContainText(`Using backup indexer ${b1.host}`, { timeout: 60_000 });
    const finding = page.getByTestId('indexer-disagreement');
    await expect(finding).toBeVisible({ timeout: 30_000 });
    await expect(finding).toContainText(b2.host);
    // the primary is not a reference while it is not ready: it is not reported as unreachable either
    await expect(finding).not.toContainText(new URL(mock.url).host);
  });

  test('the banner fits a 375 px phone, a long host included', async ({ appPage: page, mock }) => {
    test.setTimeout(90_000);
    const b = await backup();
    // the backup behind a long host name (the mock itself, reached through this address)
    const longHost = 'a-very-long-backup-indexer-host-name-for-the-phone-layout.testnet.example.invalid';
    await page.route(`http://${longHost}/**`, async (route) => {
      const res = await route.fetch({ url: route.request().url().replace(`http://${longHost}`, b.url) });
      await route.fulfill({ response: res });
    });
    await page.setViewportSize({ width: 375, height: 800 });
    await withExtras(page, [`http://${longHost}`]);
    await b.client.setHealth('following', 0, { book_complete: false, history_from_daa: 990 });
    await mock.setHealth('node_unavailable');
    await page.goto('/');
    const banner = page.getByTestId('banner-backup-indexer');
    await expect(banner).toBeVisible({ timeout: 60_000 });
    await expect(banner).toContainText(`Using backup indexer ${longHost}`);
    await expect(banner).toContainText('History may be incomplete');
    const over = await page.evaluate(() => document.documentElement.scrollWidth - window.innerWidth);
    expect(over).toBeLessThanOrEqual(0);
    const box = await banner.boundingBox();
    expect(box!.x).toBeGreaterThanOrEqual(0);
    expect(box!.x + box!.width).toBeLessThanOrEqual(375);
    await page.screenshot({ path: process.env.KOB_SHOT_DIR ? `${process.env.KOB_SHOT_DIR}/failover-375.png` : undefined });
  });
});
