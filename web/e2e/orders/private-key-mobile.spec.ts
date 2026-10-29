// The private-key signer on Android-like phones. Regression guard for a bug the headless emulator never showed: on a real Android Chrome (fractional
// device pixel ratio) the frame's IntersectionObserver v2 check reported 3 of 5 summary rows as covered, because the boxes of adjacent `.line` rows
// touch and a fractional ratio made them overlap by a device pixel, so Approve never enabled. The signer observes each line's dt / dd (inside the
// row's padding) instead of the row. Two mechanical guards, per profile (several ratios, widths and both color schemes):
//   1. the whole flow works: place + Approve and cancel + Approve, Approve enabled within seconds, the transaction submitted;
//   2. geometry: inside the frame, no two observed parts' boxes touch or overlap (the actual failure mode, independent of the emulator's occlusion).
import { readFileSync } from 'node:fs';
import type { Page } from '@playwright/test';
import { test, expect, TEST_KEYS } from '../fixtures';
import { fund, openMarket, gotoNav, openConnect } from '../helpers/env';
import { runFlow } from '../helpers/orders';
import { clickPlace, fillFields, pickType, placedBy, readReceipt, readSignerFrame, waitReviewable, waitSubmitted } from '../helpers/ticket';

test.use({ walletId: null, autoOpen: false });

const ANDROID_UA = 'Mozilla/5.0 (Linux; Android 14; Pixel 8) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/130.0.0.0 Mobile Safari/537.36';

interface Profile { name: string; width: number; height: number; dpr: number; scheme: 'light' | 'dark' }
const PROFILES: Profile[] = [
  { name: 'pixel-412x915@2.625 light', width: 412, height: 915, dpr: 2.625, scheme: 'light' },
  { name: 'pixel-412x915@2.625 dark', width: 412, height: 915, dpr: 2.625, scheme: 'dark' },
  { name: 'galaxy-393x851@2.75 light', width: 393, height: 851, dpr: 2.75, scheme: 'light' },
  { name: 'galaxy-360x780@3 dark', width: 360, height: 780, dpr: 3, scheme: 'dark' },
  { name: 'phone-384x854@3 light', width: 384, height: 854, dpr: 3, scheme: 'light' },
  { name: 'phone-360x800@3.5 dark', width: 360, height: 800, dpr: 3.5, scheme: 'dark' },
  { name: 'phone-412x892@3.5 light', width: 412, height: 892, dpr: 3.5, scheme: 'light' },
];

const signer = (page: Page) => page.frameLocator('[data-testid="signer-frame"]');

async function connectKey(page: Page): Promise<void> {
  await openConnect(page, 'wallet-connect-local-key');
  await expect(page.getByTestId('signer-overlay')).toBeVisible();
  await signer(page).getByTestId('signer-key').fill(TEST_KEYS.alice.secretKey);
  await signer(page).getByTestId('signer-connect').click();
  await expect(page.getByTestId('wallet-address')).toBeVisible();
}

/** The geometry of the summary: the boxes of every observed part (title, each line's label and value, each order's title and token). */
async function observedPartsGap(page: Page): Promise<{ count: number; rows: number; minGap: number; closest: string; lineParts: number }> {
  return page.frameLocator('[data-testid="signer-frame"]').locator('body').evaluate(() => {
    const root = document.getElementById('signer')!;
    // the same set of parts as signer/main.ts: the title, the label and value of every line, each order's title and token
    const parts = [...root.querySelectorAll('h1, h2, p.token, .line > dt, .line > dd')].filter((e) => {
      const r = e.getBoundingClientRect();
      return r.width > 0 && r.height > 0;
    });
    const boxes = parts.map((e) => ({ e, r: e.getBoundingClientRect() }));
    let minGap = Infinity;
    let closest = '';
    for (let i = 0; i < boxes.length; i++) {
      for (let j = i + 1; j < boxes.length; j++) {
        const a = boxes[i]!.r;
        const b = boxes[j]!.r;
        // the distance between two boxes: the larger of the horizontal and vertical gaps (negative = they overlap on both axes)
        const gap = Math.max(a.left - b.right, b.left - a.right, a.top - b.bottom, b.top - a.bottom);
        if (gap < minGap) {
          minGap = gap;
          closest = `${boxes[i]!.e.tagName}"${(boxes[i]!.e.textContent ?? '').slice(0, 24)}" ${JSON.stringify(a)} / ${boxes[j]!.e.tagName}"${(boxes[j]!.e.textContent ?? '').slice(0, 24)}" ${JSON.stringify(b)}`;
        }
      }
    }
    return { count: parts.length, rows: root.querySelectorAll('.line').length, minGap, closest, lineParts: parts.filter((e) => e.classList.contains('line')).length };
  });
}

/** Reads the frame, checks the geometry, scrolls through the table, and approves: Approve must enable within seconds. */
async function approveAndroid(page: Page, label: string): Promise<void> {
  const receipt = await readReceipt(page);
  expect(receipt.from).toBe('signer');
  const g = await observedPartsGap(page);
  expect(g.lineParts, `${label}: no observed part is a .line row`).toBe(0);
  expect(g.rows, `${label}: the table has summary rows`).toBeGreaterThanOrEqual(3);
  expect(g.count, `${label}: parts observed (a label and a value per row at least)`).toBeGreaterThanOrEqual(g.rows * 2);
  // different parts (other rows, title, order head) are at least 2 CSS px apart: with a fractional device pixel ratio a smaller gap becomes an overlap
  expect(g.minGap, `${label}: observed parts must not touch or overlap; closest ${g.closest}`).toBeGreaterThanOrEqual(2);
  const approve = signer(page).getByTestId('signer-approve');
  await readSignerFrame(page);
  await expect(approve, `${label}: Approve enabled within a few seconds`).toBeEnabled({ timeout: 5_000 });
  await approve.click();
  await expect(page.getByTestId('signer-overlay')).toBeHidden();
}

test('the signer observes lines by their label and value, never by the row (source guard)', () => {
  const src = readFileSync(new URL('../../src/signer/main.ts', import.meta.url), 'utf8');
  const code = src.split('\n').filter((l) => /querySelectorAll/.test(l) && !l.trim().startsWith('//'));
  const withLine = code.filter((l) => l.includes('.line'));
  expect(withLine.length, 'the parts list is built from .line children').toBeGreaterThan(0);
  for (const l of withLine) for (const m of l.matchAll(/\.line\b[^,']*/g)) expect(m[0], `a .line selector must descend to dt / dd: ${l.trim()}`).toMatch(/^\.line\s*>\s*d[td]/);
});

for (const p of PROFILES) {
  test.describe(`Android ${p.name}`, () => {
    test.use({
      viewport: { width: p.width, height: p.height },
      deviceScaleFactor: p.dpr,
      isMobile: true,
      hasTouch: true,
      userAgent: ANDROID_UA,
      colorScheme: p.scheme,
    });

    test('place and cancel with Approve in the signer frame', async ({ appPage: page, mock }) => {
      test.setTimeout(120_000);
      await fund(mock);
      await openMarket(page, mock, { connect: false });
      await connectKey(page);

      await pickType(page, 'limit', 'sell');
      await fillFields(page, { amount: '2', price: '0.027' });
      await waitReviewable(page);
      await clickPlace(page);
      await approveAndroid(page, `${p.name} place`);
      const placed = await placedBy(mock, await waitSubmitted(page));
      await mock.until(() => mock.ordersOf('alice'), (o) => o.length === 1);
      const id = placed.views[0].covenant_id as string;

      await gotoNav(page, 'nav-orders');
      await page.getByTestId(`order-cancel-${id}`).click();
      await approveAndroid(page, `${p.name} cancel`);
      const [cancelled] = await runFlow(page, mock, null, { steps: 1 });
      expect(cancelled).toBeTruthy();
      await mock.until(() => mock.order(id), (o) => o.status === 'cancelled');
      expect(await mock.submitted(false), 'both transactions were submitted').toHaveLength(2);
    });
  });
}
