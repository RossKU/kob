// The Create page's logo file and the declaration's `image` + `image_hash` (docs/spec/token-metadata.md 2), against the mock stack: the browser hashes
// the picked file (one with code in it refused), the preparation declares `image` then `image_hash`, and the token's icon is shown as checked only while the picture at the
// address still hashes to it (a different picture: the letter and a note; a host that sends no CORS header: the unchecked image, as before).
import { createHash } from 'node:crypto';
import { test, expect } from '../fixtures';
import { OTHER_PNG, PNG } from '../helpers/kascov';

const KAS = 100_000_000n;
const URL_ = 'https://logos.example.org/new-coin.png';
const CORS = { 'access-control-allow-origin': '*' };

test.use({ autoOpen: false });

test('a logo file is hashed in the browser, declared as image + image_hash, and the icon is checked against it', async ({ appPage: page, mock }) => {
  await mock.giveKas('alice', 500n * KAS);
  await page.goto('/#/create');
  await page.getByTestId('wallet-connect-kasware').click();
  await expect(page.getByTestId('wallet-address')).toBeVisible();
  await page.getByTestId('issue-name').fill('Hashed Logo Coin');
  await page.getByTestId('issue-ticker').fill('HLC');
  await page.getByTestId('issue-supply').fill('1000');
  await page.getByTestId('issue-icon').fill(URL_);

  // an SVG with a script is refused with a reason and sets no hash; a PNG fills the hash (64 lowercase hex), shown in the advanced field too
  await page.getByTestId('issue-image-file').setInputFiles({ name: 'logo.svg', mimeType: 'image/svg+xml', buffer: Buffer.from('<svg xmlns="http://www.w3.org/2000/svg"><script>alert(1)</script></svg>') });
  await expect(page.getByTestId('issue-form')).toContainText('That picture carries code');
  await expect(page.getByTestId('issue-image-hash-note')).toHaveCount(0);
  const want = createHash('sha256').update(PNG).digest('hex');
  await page.getByTestId('issue-image-file').setInputFiles({ name: 'logo.png', mimeType: 'image/png', buffer: PNG });
  await expect(page.getByTestId('issue-image-hash-note')).toContainText(want.slice(0, 16));
  await expect(page.getByTestId('issue-form')).not.toContainText('That picture carries code');
  await page.getByTestId('issue-image-hash-advanced').locator('summary').click();
  await expect(page.getByTestId('issue-image-hash')).toHaveValue(want);
  await expect(page.getByTestId('issue-form')).toContainText('cannot be changed later');
  // a hash typed by hand must be 64 hex
  await page.getByTestId('issue-image-hash').fill('abc');
  await expect(page.getByTestId('issue-form')).toContainText('64 hexadecimal characters');
  await expect(page.getByTestId('issue-review')).toBeDisabled();
  await page.getByTestId('issue-image-hash').fill(want.toUpperCase());
  await expect(page.getByTestId('issue-review')).toBeEnabled();

  await page.getByTestId('issue-review').click();
  await expect(page.getByTestId('confirm-declare-declare-imageHash')).toContainText(want);
  await page.getByTestId('confirm-ack').check();
  await page.getByTestId('confirm-sign').click();
  await expect(page.getByTestId('tx-status')).toBeVisible({ timeout: 30_000 });
  await page.getByTestId('confirm-close').click();
  await expect(page.getByTestId('issue-result')).toBeVisible({ timeout: 30_000 });
  const tokenId = (await page.getByTestId('issue-result-tokenId').getAttribute('data-value'))!;

  // the preparation's payload: `image`, then `image_hash` (lowercase), compact, in KOB's order
  const submitted = await mock.submitted();
  const prep = submitted[submitted.length - 2];
  const declared = `{"name":"Hashed Logo Coin","symbol":"HLC","decimals":8,"image":"${URL_}","image_hash":"${want}"}`;
  expect(prep.tx.payload).toBe(`00${Buffer.from(declared, 'utf8').toString('hex')}`);

  // the host allows reading: the same bytes -> a checked picture from a blob: URL
  const serve = (body: Buffer, headers: Record<string, string>) =>
    page.route(URL_, (route) => route.fulfill({ status: 200, contentType: 'image/png', headers, body }));
  await serve(PNG, CORS);
  await page.goto(`/#/market/${tokenId}`);
  await page.reload();
  const icon = page.getByTestId(`token-icon-${tokenId}`).first();
  await expect(icon).toHaveAttribute('data-icon', 'image');
  expect(await icon.locator('img').getAttribute('src')).toMatch(/^blob:/);

  // the host serves another picture: the letter and the note, never the other picture
  await page.unroute(URL_);
  await serve(OTHER_PNG, CORS);
  await page.reload();
  await expect(page.getByTestId(`token-icon-${tokenId}`).first()).toHaveAttribute('data-logo-note', 'mismatch');
  await expect(page.getByTestId(`token-icon-${tokenId}`).first()).toHaveAttribute('data-icon', 'letter');
  await expect(page.getByTestId(`token-icon-${tokenId}`).first().locator('img')).toHaveCount(0);

  // the host sends no CORS header (Playwright adds one to a fulfilled response, so the blocked read is simulated by failing the page's fetch only): nothing can
  // be read, so the unchecked image is shown as before
  await page.unroute(URL_);
  await page.route(URL_, (route) =>
    route.request().resourceType() === 'fetch' ? route.abort('failed') : route.fulfill({ status: 200, contentType: 'image/png', body: PNG }),
  );
  await page.reload();
  await expect(page.getByTestId(`token-icon-${tokenId}`).first()).toHaveAttribute('data-icon', 'creator-image');
});
