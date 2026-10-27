// The "Private key" wallet: the key is typed into signer.html, framed `sandbox="allow-scripts"` (opaque origin). The app never sees it: it sends
// the unsigned transaction, the frame shows its own decoding (the final confirmation: a title and a table of the order's terms, the fee and the
// token; the app shows no popup of its own for this wallet) and signs only after a click on its own Confirm button.
import type { Frame, Page } from '@playwright/test';
import { test, expect, TEST_KEYS } from '../fixtures';
import { fund, mockClock, openMarket } from '../helpers/env';
import { runFlow } from '../helpers/orders';
import { clickPlace, confirmReceipt, expectStopped, fillFields, pickType, placedBy, readReceipt, readSignerFrame, waitReviewable, waitSubmitted, type ReceiptRead } from '../helpers/ticket';
import { formatKas } from '../../src/kob/units';
/** the header chip shows the address with its network prefix, the head and tail of its body (WalletButton.tsx chipAddress) */
const chipAddress = (a: string): string => {
  const body = a.slice(a.indexOf(':') + 1);
  return `${a.slice(0, a.indexOf(':') + 1)}${body.slice(0, 6)}…${body.slice(-6)}`;
};

test.use({ walletId: null, autoOpen: false });

const SECRET = TEST_KEYS.alice.secretKey;
const signer = (page: Page) => page.frameLocator('[data-testid="signer-frame"]');
const signerFrame = (page: Page): Frame => {
  const f = page.frames().find((x) => x.url().includes('signer.html'));
  if (!f) throw new Error('no signer frame');
  return f;
};

async function connectKey(page: Page, key = SECRET): Promise<void> {
  await page.getByTestId('wallet-connect-local-key').click();
  const overlay = page.getByTestId('signer-overlay');
  await expect(overlay).toBeVisible();
  await expect(overlay).toContainText('A browser extension wallet is recommended.');
  const f = signer(page);
  await expect(f.getByTestId('signer-key')).toHaveAttribute('type', 'password');
  await expect(f.getByTestId('signer-key')).toHaveAttribute('autocomplete', 'off');
  await expect(f.getByTestId('signer-key')).toHaveAttribute('spellcheck', 'false');
  // before anything is pasted: what the key is, what forgets it and which key to use
  await expect(f.getByTestId('signer-guide')).toContainText('64 hexadecimal characters');
  await expect(f.getByTestId('signer-guide')).toContainText('seed phrase');
  await expect(f.getByTestId('signer-guide')).toContainText('Reloading or closing the page forgets it');
  await expect(f.getByTestId('signer-guide')).toContainText('dedicated key with a small balance');
  await f.getByTestId('signer-key').fill(key);
  await f.getByTestId('signer-connect').click();
}

async function sellTicket(page: Page): Promise<void> {
  await pickType(page, 'limit', 'sell');
  await fillFields(page, { amount: '2', price: '0.027' });
  await waitReviewable(page);
}

/** the fee the frame shows is the exact fee of the transaction the node accepted (full precision) */
const feeText = (sompi: string): string => `${formatKas(BigInt(sompi))} KAS`;

/**
 * Waits for the signer frame of the request the app just made (the click on the order button or on a My orders action; the app shows no popup for
 * this wallet), checks its title, summary lines and order tables, approves in the frame (Approve signs), and returns what the frame showed.
 * `lines` are exact summary lines by label, `orderTitles` the titles of the orders it lists.
 */
async function approveInFrame(page: Page, want: { title: string; lines?: Record<string, string>; orderTitles?: string[] }): Promise<ReceiptRead> {
  const f = signer(page);
  await expect(page.getByTestId('signer-overlay')).toBeVisible();
  const receipt = await readReceipt(page);
  expect(receipt.from).toBe('signer');
  expect(receipt.title).toBe(want.title);
  for (const [label, value] of Object.entries(want.lines ?? {})) expect(receipt.lines[label], `summary line ${label}`).toBe(value);
  expect(receipt.orders.map((o) => o.title)).toEqual(want.orderTitles ?? []);
  expect(receipt.fee, 'the frame names the network fee').toMatch(/^[0-9.,]+ KAS$/);
  await expect(page.getByTestId('confirm-order')).toHaveCount(0); // the app shows no popup of its own
  await expect(f.getByTestId('signer-problems')).toHaveCount(0);
  await confirmReceipt(page); // scrolls the frame if its table is taller than it, then Approve
  return receipt;
}

/** Everything of the app page a script there could read: storage, cookies, the URL, the page's own markup and IndexedDB names. */
async function pageReadable(page: Page): Promise<string> {
  return page.evaluate(async () => {
    const dump = (s: Storage) => Array.from({ length: s.length }, (_, i) => `${s.key(i)}=${s.getItem(s.key(i)!)}`).join('\n');
    const dbs = 'databases' in indexedDB ? (await indexedDB.databases()).map((d) => d.name).join(',') : '';
    return [dump(localStorage), dump(sessionStorage), document.cookie, location.href, document.documentElement.outerHTML, dbs].join('\n');
  });
}

test('connect with a private key in the signer frame, place and cancel an order approving in the frame; the key never reaches the page', async ({ appPage: page, mock }) => {
  const consoleText: string[] = [];
  page.on('console', (m) => consoleText.push(m.text()));
  await fund(mock);
  await openMarket(page, mock, { connect: false });
  await connectKey(page);
  await expect(page.getByTestId('wallet-address')).toContainText(chipAddress(TEST_KEYS.alice.testnetAddress));
  // the address is the chip's text; how it is connected is the wallet icon's tooltip, never a label next to the address
  await expect(page.getByTestId('wallet-address')).toHaveText(new RegExp(`^${chipAddress(TEST_KEYS.alice.testnetAddress)}`));
  await expect(page.getByTestId('wallet-connected')).not.toContainText('Private key');
  await expect(page.getByTestId('wallet-kind')).toHaveAttribute('title', 'Connected with: private key (this browser)');
  await expect(page.getByTestId('signer-overlay')).toBeHidden();

  // place: the frame's table says the order (2 tokens at 0.027 KAS each), what it locks and the fee; no popup of the app
  await sellTicket(page);
  await clickPlace(page);
  const shown = await approveInFrame(page, {
    title: 'Place order',
    lines: { 'Locked in orders': '4 KAS' },
    orderTitles: ['Sell limit order'],
  });
  expect(shown.orders[0]!.rows).toMatchObject({ Amount: '2 EXKCC', 'Limit price': '0.027 KAS per EXKCC' });
  expect(shown.orders[0]!.token).toMatch(/^EXKCC [(][0-9a-f]{8}…[0-9a-f]{8}[)]$/);
  const placed = await placedBy(mock, await waitSubmitted(page));
  expect(shown.fee).toBe(feeText(placed.submission.fee));
  expect(placed.views[0].contract).toBe('KobAsk');
  await mock.until(() => mock.ordersOf('alice'), (o) => o.length === 1); // the order is alice's: the frame signed with her key
  const id = placed.views[0].covenant_id as string;

  // cancel: the action asks the wallet at once (the frame is the confirmation)
  await page.getByTestId('nav-orders').click();
  await page.getByTestId(`order-cancel-${id}`).click();
  const cancelShown = await approveInFrame(page, { title: 'Cancel order' });
  const [cancelled] = await runFlow(page, mock, null, { steps: 1 });
  expect(cancelShown.fee).toBe(feeText(cancelled!.submission.fee));
  expect(Object.values(cancelShown.lines), 'the 2 EXKCC come back').toContain('+2');
  expect(cancelShown.labels, 'what the cancel releases from orders').toContain('Released from orders');
  await mock.until(() => mock.order(id), (o) => o.status === 'cancelled');
  const subs = await mock.submitted(false);
  expect(subs).toHaveLength(2);
  expect(subs[1]!.closed).toEqual([{ covenantId: id, entry: 'cancel', status: 'cancelled' }]);

  const readable = await pageReadable(page);
  expect(readable).not.toContain(SECRET);
  expect(readable).not.toContain(SECRET.slice(0, 16));
  expect(consoleText.join('\n')).not.toContain(SECRET.slice(0, 16));
});

test('the frame is an opaque origin: the app cannot read it, cannot sign through it without the click in the frame, and forged messages do nothing', async ({ appPage: page, mock }) => {
  await fund(mock);
  await openMarket(page, mock, { connect: false });
  await connectKey(page);
  await expect(page.getByTestId('wallet-address')).toBeVisible();

  // the frame's side: opaque origin, no storage of the app's origin, no access to the app's document
  const inside = await signerFrame(page).evaluate(() => {
    const r: Record<string, string> = { origin: String(self.origin) };
    try {
      r.storage = String(localStorage.length);
    } catch (e) {
      r.storage = (e as Error).name;
    }
    try {
      r.parentDoc = String(window.parent.document.title);
    } catch (e) {
      r.parentDoc = (e as Error).name;
    }
    return r;
  });
  expect(inside).toEqual({ origin: 'null', storage: 'SecurityError', parentDoc: 'SecurityError' });

  await sellTicket(page);
  await clickPlace(page);
  await expect(signer(page).getByTestId('signer-approve')).toBeVisible();

  // the app's side: a script of the page tries to read the frame, click its button, and fake approvals
  const outside = await page.evaluate(() => {
    const f = document.querySelector<HTMLIFrameElement>('[data-testid="signer-frame"]')!;
    const r: Record<string, string> = { sandbox: f.getAttribute('sandbox') ?? '', contentDocument: String(f.contentDocument) };
    try {
      r.document = String(f.contentWindow!.document.body.innerHTML);
    } catch (e) {
      r.document = (e as Error).name;
    }
    try {
      (f.contentWindow!.document.querySelector('[data-testid="signer-key"]') as HTMLInputElement | null)?.value;
      r.key = 'readable';
    } catch (e) {
      r.key = (e as Error).name;
    }
    try {
      (f.contentWindow!.document.querySelector('[data-testid="signer-approve"]') as HTMLButtonElement | null)?.click();
      r.click = 'clicked';
    } catch (e) {
      r.click = (e as Error).name;
    }
    const P = 'kob-signer/1';
    for (const m of [{ p: P, type: 'approve' }, { p: P, type: 'approve', id: 'x' }, { p: P, type: 'sign' }, 'approve']) f.contentWindow!.postMessage(m, '*');
    // a forged answer posted to the app's own window: its source is not the frame, the app drops it
    window.postMessage({ p: P, type: 'signed', id: 'r1', signatures: [{ inputIndex: 0, signature: 'ab'.repeat(64) + '01' }] }, '*');
    return r;
  });
  expect(outside).toEqual({ sandbox: 'allow-scripts', contentDocument: 'null', document: 'SecurityError', key: 'SecurityError', click: 'SecurityError' });

  // nothing happens without the click in the frame
  await page.waitForTimeout(3000);
  expect(await mock.submitted(false)).toHaveLength(0);
  await expect(page.getByTestId('order-status')).toHaveAttribute('data-phase', 'signing');
  await expect(signer(page).getByTestId('signer-approve')).toBeVisible();

  // the user rejects in the frame: the neutral outcome, nothing sent
  await signer(page).getByTestId('signer-reject').click();
  await expectStopped(page, 'rejected');
  await expect(page.getByTestId('signer-overlay')).toBeHidden();
  expect(await mock.submitted(false)).toHaveLength(0);
});

test('reload and disconnect forget the key; a key that does not parse is refused in the frame', async ({ appPage: page, mock }) => {
  await openMarket(page, mock, { connect: false });
  await connectKey(page, 'not-a-key');
  await expect(signer(page).getByTestId('signer-key-error')).toContainText('Not a valid private key');
  await expect(signer(page).getByTestId('signer-key')).toHaveValue('');
  await expect(page.getByTestId('wallet-connected')).toHaveCount(0);
  await page.getByTestId('signer-close').click();
  await expect(page.getByTestId('signer-frame')).toHaveCount(0);

  await connectKey(page);
  await expect(page.getByTestId('wallet-address')).toBeVisible();
  await page.reload();
  await expect(page.getByTestId('wallet-connect-local-key')).toBeVisible();
  await expect(page.getByTestId('wallet-connected')).toHaveCount(0);
  await expect(page.getByTestId('signer-frame')).toHaveCount(0);

  await connectKey(page);
  await expect(page.getByTestId('wallet-address')).toBeVisible();
  await page.getByTestId('wallet-disconnect').click();
  await expect(page.getByTestId('wallet-connected')).toHaveCount(0);
  await expect(page.getByTestId('signer-frame')).toHaveCount(0);
  // connecting again needs the key again: the new frame starts empty
  await page.getByTestId('wallet-connect-local-key').click();
  await expect(signer(page).getByTestId('signer-key')).toHaveValue('');
});

test('a page that clips the frame down to its buttons gets no approval; shown whole, the request is signable', async ({ appPage: page, mock }) => {
  await fund(mock);
  await openMarket(page, mock, { connect: false });
  await connectKey(page);
  await expect(page.getByTestId('wallet-address')).toBeVisible();
  await sellTicket(page);
  // before the request appears, a script of the page clips the frame so that only its buttons show (its own summary could sit above them)
  await page.evaluate(() => {
    const box = document.querySelector<HTMLElement>('.signer-box')!;
    const frame = document.querySelector<HTMLIFrameElement>('[data-testid="signer-frame"]')!;
    box.style.height = '70px';
    box.style.overflow = 'hidden';
    frame.style.marginTop = '-450px';
  });
  await page.getByTestId('order-place').click();
  const approve = signer(page).getByTestId('signer-approve');
  await expect(approve).toBeVisible();
  await page.waitForTimeout(3000);
  await expect(approve).toBeDisabled();
  // why it waits: a sentence a person can act on, the counts behind it small
  const why = signer(page).getByTestId('signer-wait-why');
  await expect(why).toContainText('Approve is locked until you have seen the whole request');
  await expect(why).toContainText('Keep this window fully on screen');
  await expect(signer(page).getByTestId('signer-wait-counts')).toContainText(/^Check: buttons .+, lines seen \d+\/\d+, covered parts/);
  expect(await mock.submitted(false)).toHaveLength(0);
  // the whole frame on screen: the summary is seen, then Approve works
  await page.evaluate(() => {
    document.querySelector<HTMLElement>('.signer-box')!.removeAttribute('style');
    document.querySelector<HTMLIFrameElement>('[data-testid="signer-frame"]')!.removeAttribute('style');
  });
  await readSignerFrame(page);
  await expect(approve).toBeEnabled();
  await approve.click();
  await waitSubmitted(page);
});

test('a browser without IntersectionObserver v2 (no isVisible on the entry) is not offered the private key', async ({ appPage: page, mock }) => {
  await page.addInitScript(() => {
    delete (IntersectionObserverEntry.prototype as { isVisible?: boolean }).isVisible;
  });
  await openMarket(page, mock, { connect: false });
  expect(await page.evaluate(() => 'isVisible' in IntersectionObserverEntry.prototype)).toBe(false);
  const button = page.getByTestId('wallet-connect-local-key');
  await expect(button).toBeVisible();
  await expect(button).toBeDisabled();
  await expect(button).toHaveAttribute('title', 'Not available in this browser. Use a browser extension wallet.');
  // the reason is text on the page, not only a tooltip (there is none on a touch screen)
  const why = page.getByTestId('wallet-key-unsupported');
  await expect(why).toBeVisible();
  await expect(why).toContainText('This browser cannot use the in-page private key signer');
  await expect(why).toContainText('On a desktop, use an extension wallet (KasWare or Kaspire)');
  await expect(page.getByTestId('signer-frame')).toHaveCount(0);
});

test.describe('an iPhone (no IntersectionObserver v2, no extension wallets)', () => {
  test.use({ viewport: { width: 390, height: 844 }, userAgent: 'Mozilla/5.0 (iPhone; CPU iPhone OS 17_5 like Mac OS X) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/17.5 Mobile/15E148 Safari/604.1', walletId: null });
  test('says plainly why the private key is off and no longer tells a phone to install an extension in this browser', async ({ appPage: page, mock }) => {
    await page.addInitScript(() => {
      delete (IntersectionObserverEntry.prototype as { isVisible?: boolean }).isVisible;
    });
    await openMarket(page, mock, { connect: false });
    const why = page.getByTestId('wallet-key-unsupported');
    await expect(why).toBeVisible();
    await expect(why).toContainText('iOS browsers and Firefox lack the feature');
    await expect(why).toContainText('Chrome on Android, or open this page on a desktop with an extension wallet (KasWare or Kaspire)');
    await expect(page.getByTestId('wallet-connect-local-key')).toBeDisabled();
    const hint = page.getByTestId('wallet-install-hint');
    await expect(hint).toContainText('Extension wallets do not run in phone browsers');
    await expect(hint).not.toContainText('Install');
    await expect(hint.locator('a')).toHaveCount(0);
    expect(await page.evaluate(() => document.documentElement.scrollWidth <= window.innerWidth)).toBe(true);
  });
});

test('signer.html framed by a same-origin script without the sandbox attribute is still an opaque origin (header policy)', async ({ appPage: page, mock }) => {
  await openMarket(page, mock, { connect: false });
  const r = await page.evaluate(async () => {
    const f = document.createElement('iframe');
    f.src = 'signer.html';
    const loaded = new Promise((res) => f.addEventListener('load', res));
    document.body.append(f);
    await loaded;
    return f.contentDocument === null ? 'opaque' : 'readable';
  });
  expect(r).toBe('opaque');
});

test('a late Approve (9 s in the frame) on a market order: the frame is not shown again, one transaction, and its clock is the one at Approve', async ({ appPage: page, mock }) => {
  test.setTimeout(120_000);
  await fund(mock);
  // counts how often the signer overlay becomes visible (every prompt of the frame shows it): sampled every animation frame
  await page.addInitScript(() => {
    const w = window as unknown as { __overlayShown: number };
    w.__overlayShown = 0;
    let visible = false;
    const sample = () => {
      const el = document.querySelector('[data-testid="signer-overlay"]');
      const now = !!el && el.getBoundingClientRect().height > 0 && getComputedStyle(el).visibility !== 'hidden';
      if (now && !visible) w.__overlayShown += 1;
      visible = now;
      requestAnimationFrame(sample);
    };
    requestAnimationFrame(sample);
  });
  await openMarket(page, mock, { connect: false });
  await connectKey(page);
  await expect(page.getByTestId('wallet-address')).toBeVisible();
  const shownAfterConnect = await page.evaluate(() => (window as unknown as { __overlayShown: number }).__overlayShown);

  // the flipped KAS/EXKCC view: Sell 20 KAS, Market (native: a market buy of EXKCC, an auction)
  await page.getByTestId('market-flip').click();
  await page.getByTestId('order-type').selectOption('market');
  await page.getByTestId('order-side-sell').click();
  await page.getByTestId('order-amount').fill('20');
  await waitReviewable(page);
  await page.getByTestId('order-place').click();
  const f = signer(page);
  await expect(f.getByTestId('signer-title')).toBeVisible({ timeout: 30_000 });
  // the clock reaches the frame before the request: the first view already says every time as "in 20 s | DAA ...", never a bare "DAA score ..."
  const timeCells = await f.getByTestId('signer-order-0').locator('dd[data-daa]').allTextContents();
  expect(timeCells.length).toBeGreaterThan(0);
  for (const text of timeCells) expect(text).toMatch(/^(now|in \d+ \w+)DAA \d+$/);
  const t0 = Date.now();
  const receipt = await readReceipt(page);
  expect(receipt.title).toBe('Place order');
  // the flipped view's own words: a sale of KAS (on chain a market buy of the token)
  expect(receipt.orders.map((o) => o.title)).toEqual(['Sell KAS · market (auction)']);
  expect(receipt.orders[0]!.token).toMatch(/· on chain: Buy market order [(]auction[)]$/);
  // the frame is as tall as its table: no empty space below Reject / Approve, and nothing to scroll
  await expect
    .poll(async () => f.locator('body').evaluate(() => ({ content: Math.ceil(document.getElementById('signer')!.getBoundingClientRect().height), frame: window.innerHeight })), { timeout: 5_000 })
    .toMatchObject({ frame: expect.any(Number) });
  const size = await f.locator('body').evaluate(() => ({ content: Math.ceil(document.getElementById('signer')!.getBoundingClientRect().height), frame: window.innerHeight }));
  test.info().annotations.push({ type: 'signer-table', description: `content ${size.content}px in a frame of ${size.frame}px (viewport ${JSON.stringify(page.viewportSize())})` });
  // as tall as its content, or (a table taller than the viewport leaves: other fonts, a phone) as tall as the viewport leaves, then it scrolls
  const room = await page.getByTestId('signer-frame').evaluate((el) => window.innerHeight - Math.ceil(el.getBoundingClientRect().top) - 14);
  expect(Math.abs(size.frame - Math.min(size.content, room)), 'the frame fits its content, up to the viewport').toBeLessThanOrEqual(2);
  // the table follows the market and the clock while it is open: its start is said from the clock and moves on
  const start = f.getByTestId('signer-order-0').locator('dd[data-daa]').first();
  await expect(start).toContainText(/^(now|in \d+ s)DAA \d+$/, { timeout: 5_000 });
  const daa0 = Number(await start.getAttribute('data-daa'));
  // the user reads for 9 s (the mock chain advances 10 DAA a second): the auction in the table began on the clock of the click
  await readSignerFrame(page);
  await page.waitForTimeout(Math.max(0, 9_000 - (Date.now() - t0)));
  await expect(f.getByTestId('signer-approve')).toBeEnabled();
  expect(Number(await start.getAttribute('data-daa')), 'the start shown moved on with the clock').toBeGreaterThan(daa0);
  // a plan no worse for the user is taken without a note
  await expect(f.getByTestId('signer-updated')).toBeHidden();
  const approvedAt = await mockClock(mock);
  const shownStart = Number(await start.getAttribute('data-daa'));
  await f.getByTestId('signer-approve').click();
  const txid = await waitSubmitted(page);
  const placed = await placedBy(mock, txid);

  // one prompt only: the frame was shown once; the re-timed request after the late Approve was signed without showing it again
  const shown = await page.evaluate(() => (window as unknown as { __overlayShown: number }).__overlayShown);
  expect(shown - shownAfterConnect, 'the frame was shown once for this order').toBe(1);
  await expect(page.getByTestId('signer-overlay')).toBeHidden();
  expect(await mock.submitted(false), 'one transaction').toHaveLength(1);
  // the stale clock of the table did not end up in the order: the auction starts at or after the node clock at Approve
  const state = placed.views[0].state.state;
  expect(placed.views[0].contract).toBe('KobBid');
  expect(Number(state.activeFrom), 'activeFrom >= the DAA at Approve').toBeGreaterThanOrEqual(approvedAt.daa);
  // what was signed is what the table showed at Approve (re-timed after it only if the clock passed its start meanwhile)
  expect(Number(state.activeFrom)).toBeGreaterThanOrEqual(shownStart);
  expect(Number(state.activeFrom) - shownStart).toBeLessThanOrEqual(40);
  expect(Number(state.expiryDaa)).toBeGreaterThan(Number(state.activeFrom));
});
