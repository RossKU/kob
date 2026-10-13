// The "Private key" wallet: the key is typed into signer.html, framed `sandbox="allow-scripts"` (opaque origin). The app never sees it: it sends
// the unsigned transaction, the frame shows its own decoding and signs only after a click on its own Approve button.
import type { Frame, Page } from '@playwright/test';
import { test, expect, TEST_KEYS } from '../fixtures';
import { fund, openMarket } from '../helpers/env';
import { fillFields, openReview, pickType, placedBy, waitReviewable } from '../helpers/ticket';
/** the header chip shows the address without its prefix, head and tail (WalletButton.tsx chipAddress) */
const chipAddress = (a: string): string => {
  const body = a.slice(a.indexOf(':') + 1);
  return body.length <= 14 ? body : `${body.slice(0, 6)}…${body.slice(-6)}`;
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
  await f.getByTestId('signer-key').fill(key);
  await f.getByTestId('signer-connect').click();
}

async function sellTicket(page: Page): Promise<void> {
  await pickType(page, 'limit', 'sell');
  await fillFields(page, { amount: '2', price: '0.027' });
  await waitReviewable(page);
}

/** Acknowledges the app's confirmation screen, signs, and approves in the frame; waits for the accepted transaction. */
async function signInFrame(page: Page, title: string): Promise<string> {
  await page.getByTestId('confirm-ack').check();
  await page.getByTestId('confirm-sign').click();
  const f = signer(page);
  await expect(page.getByTestId('signer-overlay')).toBeVisible();
  await expect(f.getByTestId('signer-title')).toHaveText(title);
  await expect(f.getByTestId('signer-summary')).toContainText('Network fee');
  await expect(f.getByTestId('signer-approve')).toBeEnabled();
  await f.getByTestId('signer-approve').click();
  await expect(page.getByTestId('signer-overlay')).toBeHidden();
  await expect(page.getByTestId('tx-status')).toHaveAttribute('data-status', 'confirmed', { timeout: 30_000 });
  return (await page.getByTestId('tx-id').getAttribute('data-value'))!;
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
  await expect(page.getByTestId('wallet-connected')).toContainText('Private key');
  await expect(page.getByTestId('signer-overlay')).toBeHidden();

  // place
  await sellTicket(page);
  await openReview(page);
  const txid = await signInFrame(page, 'Place order');
  const placed = await placedBy(mock, txid);
  expect(placed.views[0].contract).toBe('KobAsk');
  await mock.until(() => mock.ordersOf('alice'), (o) => o.length === 1); // the order is alice's: the frame signed with her key
  await page.getByTestId('confirm-close').click();
  const id = placed.views[0].covenant_id as string;

  // cancel
  await page.getByTestId('nav-orders').click();
  await page.getByTestId(`order-cancel-${id}`).click();
  await expect(page.getByTestId('confirm-screen')).toBeVisible();
  await signInFrame(page, 'Cancel order');
  await page.getByTestId('confirm-close').click();
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
  await openReview(page);
  await page.getByTestId('confirm-ack').check();
  await page.getByTestId('confirm-sign').click();
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
  await expect(page.getByTestId('confirm-stages')).toHaveAttribute('data-stage', 'signing');
  await expect(signer(page).getByTestId('signer-approve')).toBeVisible();

  // the user rejects in the frame: the neutral outcome, nothing sent
  await signer(page).getByTestId('signer-reject').click();
  await expect(page.getByTestId('confirm-rejected')).toBeVisible();
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
  await openReview(page);
  await page.getByTestId('confirm-ack').check();
  // before the request appears, a script of the page clips the frame so that only its buttons show (its own summary could sit above them)
  await page.evaluate(() => {
    const box = document.querySelector<HTMLElement>('.signer-box')!;
    const frame = document.querySelector<HTMLIFrameElement>('[data-testid="signer-frame"]')!;
    box.style.height = '70px';
    box.style.overflow = 'hidden';
    frame.style.marginTop = '-450px';
  });
  await page.getByTestId('confirm-sign').click();
  const approve = signer(page).getByTestId('signer-approve');
  await expect(approve).toBeVisible();
  await page.waitForTimeout(3000);
  await expect(approve).toBeDisabled();
  expect(await mock.submitted(false)).toHaveLength(0);
  // the whole frame on screen: the summary is seen, then Approve works
  await page.evaluate(() => {
    document.querySelector<HTMLElement>('.signer-box')!.removeAttribute('style');
    document.querySelector<HTMLIFrameElement>('[data-testid="signer-frame"]')!.removeAttribute('style');
  });
  await expect(approve).toBeEnabled();
  await approve.click();
  await expect(page.getByTestId('tx-status')).toHaveAttribute('data-status', 'confirmed', { timeout: 30_000 });
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
