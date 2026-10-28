// A machine-translated page (Chrome's built-in Google Translate, which a browser set to another language can run on its own). The translator replaces
// every text node it translates with `<font><font>translation</font></font>` and keeps the original node detached; Preact keeps writing into the
// original, so a text updated in place would stay on screen as the translation of its first value (TN10 2026-10-17: "Node: checking..." beside a green
// dot for hours, an order button naming an earlier side and type, a status line that never said "accepted"). app/translation-safe.ts re-creates a
// text-only element whose text changes while <html> carries the translator's `translated-ltr`; changing texts sit alone in their element.
//
// The init script below does what the translator does to the app page (not to the signer frame, whose page is rebuilt for every request): it marks
// <html>, replaces each text node with a `[ja]`-prefixed <font><font> as soon as it appears (a MutationObserver: the translator follows the page the
// same way), and records each replaced node's text. A text vnode whose replaced node Preact changed afterwards is a stale text on screen.
import type { Page } from '@playwright/test';
import { expect, test, TEST_KEYS } from '../fixtures';
import { fund, openMarket, gotoNav, clickConnectOption, openStatus } from '../helpers/env';
import { seedOrders } from '../helpers/orders';
import { clickPlace, confirmReceipt, fillFields, pickType, waitReviewable, waitSubmitted } from '../helpers/ticket';

test.use({ walletId: null, autoOpen: false });

const MARK = '[ja]';

/** The translator, in the app page only (runs before the app: the first render is translated as it appears). */
function installTranslator(): void {
  if (window.top !== window) return;
  const replaced = new WeakMap<Text, string>();
  (window as unknown as { __translated: WeakMap<Text, string> }).__translated = replaced;
  const SKIP = 'script, style, textarea, select, option, [translate="no"], .notranslate, font[data-gt]';
  const translate = (t: Text) => {
    const parent = t.parentElement;
    if (!parent || !t.data.trim() || parent.closest(SKIP)) return;
    const outer = document.createElement('font');
    const inner = document.createElement('font');
    outer.setAttribute('data-gt', '');
    outer.style.verticalAlign = inner.style.verticalAlign = 'inherit';
    inner.textContent = `[ja]${t.data}`;
    outer.appendChild(inner);
    replaced.set(t, t.data);
    parent.replaceChild(outer, t);
  };
  const sweep = (root: Node) => {
    if (root.nodeType === Node.TEXT_NODE) return translate(root as Text);
    const w = document.createTreeWalker(root, NodeFilter.SHOW_TEXT);
    const all: Text[] = [];
    while (w.nextNode()) all.push(w.currentNode as Text);
    all.forEach(translate);
  };
  const start = () => {
    document.documentElement.classList.add('translated-ltr');
    document.documentElement.lang = 'ja';
    sweep(document.body);
    new MutationObserver((ms) => {
      for (const m of ms) {
        if (m.type === 'characterData') sweep(m.target);
        else m.addedNodes.forEach(sweep);
      }
    }).observe(document.body, { childList: true, subtree: true, characterData: true });
  };
  if (document.body) start();
  else document.addEventListener('DOMContentLoaded', start);
}

/**
 * Texts Preact changed after the translator replaced their node: what the page shows for them is the translation of an older value. Walks the vdom
 * from the root (Preact 10's built names: `__k` children, `__e` DOM node), returns `<nearest test id> :: <text Preact holds>`.
 */
async function staleTexts(page: Page): Promise<string[]> {
  return page.evaluate(() => {
    type V = { type: unknown; props: any; __k?: (V | null)[] | null; __e?: Node | null };
    const replaced = (window as unknown as { __translated: WeakMap<Node, string> }).__translated;
    const out: string[] = [];
    const walk = (v: V | null | undefined, where: string) => {
      if (!v) return;
      const here = typeof v.type === 'string' && v.props?.['data-testid'] ? String(v.props['data-testid']) : where;
      const dom = v.__e;
      if (v.type === null && dom && replaced.has(dom) && replaced.get(dom) !== (dom as Text).data) out.push(`${here} :: ${(dom as Text).data}`);
      for (const c of v.__k ?? []) walk(c, here);
    };
    walk((document.getElementById('app') as unknown as { __k: V }).__k, '(root)');
    return out;
  });
}

/** What the person sees, with the translator's marks taken out (the emulated translation is the English text behind a mark). */
const seen = async (page: Page, testid: string): Promise<string> => ((await page.getByTestId(testid).textContent()) ?? '').split(MARK).join('');

async function connectKey(page: Page): Promise<void> {
  // the translator's marks make every text wider: a header row that does not fit takes the phone layout, where the key is behind "Connect"
  await clickConnectOption(page, 'wallet-connect-local-key');
  await expect(page.getByTestId('signer-overlay')).toBeVisible();
  const f = page.frameLocator('[data-testid="signer-frame"]');
  await f.getByTestId('signer-key').fill(TEST_KEYS.alice.secretKey);
  await f.getByTestId('signer-connect').click();
  await expect(page.getByTestId('wallet-address')).toBeVisible();
  await expect(page.getByTestId('signer-overlay')).toBeHidden();
}

test('a machine-translated page shows every changed text afresh: status bar, ticket, order button and status line through a private-key order, native and flipped', async ({ appPage: page, mock, pageErrors }) => {
  await page.addInitScript(installTranslator);
  await fund(mock, 'alice');
  await openMarket(page, mock, { connect: false });

  // the page is translated (what the translator replaced is on screen) and the status bar reached the node after that: no "checking..." left
  await expect(page.locator('html')).toHaveClass(/translated-ltr/);
  await openStatus(page);
  await expect(page.getByTestId('status-node')).toHaveAttribute('data-level', 'ok');
  await expect.poll(() => seen(page, 'status-node')).toContain('Node: reachable');
  expect(await page.getByTestId('status-node').locator('font[data-gt]').count(), 'the status text is a translation').toBeGreaterThan(0);
  expect(await seen(page, 'status-node')).not.toContain('checking');

  await connectKey(page);

  // native: the side and type change the order button's label while the page is translated
  await pickType(page, 'limit', 'buy');
  await pickType(page, 'limit', 'sell');
  await fillFields(page, { amount: '1', price: '0.04' });
  await waitReviewable(page);
  await expect.poll(() => seen(page, 'order-place')).toContain('Sell');

  // flipped and back: the ticket is worded in the shown pair each time
  await page.getByTestId('market-flip').click();
  await expect(page.getByTestId('order-ticket')).toHaveAttribute('data-inverted', '1');
  await expect.poll(() => seen(page, 'order-place')).toContain('Buy');
  expect(await staleTexts(page), 'stale texts in the flipped view').toEqual([]);
  await page.getByTestId('market-flip').click();
  await expect(page.getByTestId('order-ticket')).not.toHaveAttribute('data-inverted', '1');
  await fillFields(page, { amount: '1', price: '0.04' });
  await waitReviewable(page);
  await expect.poll(() => seen(page, 'order-place')).toContain('Sell');

  // the order through the final confirmation (the signer frame) to the network's acceptance, which the status line says
  await clickPlace(page);
  await confirmReceipt(page);
  await waitSubmitted(page);
  await expect.poll(() => seen(page, 'order-status')).toContain('Accepted by the network:');
  expect(await seen(page, 'order-status')).not.toContain('Checking');

  // flipped again after the order (balances, book and ticket all changed)
  await page.getByTestId('market-flip').click();
  await expect(page.getByTestId('order-ticket')).toHaveAttribute('data-inverted', '1');
  await gotoNav(page, 'nav-orders');
  await expect(page.getByTestId('orders-view')).toBeVisible();
  await page.goBack();
  await expect(page.getByTestId('order-ticket')).toBeVisible();

  expect(await staleTexts(page), 'texts Preact changed after the translator replaced them').toEqual([]);
  await expect(page.getByTestId('view-crashed')).toHaveCount(0);
  expect(pageErrors.filter((e) => /view crashed|insertBefore|NotFoundError/.test(e))).toEqual([]);
});

test('a machine-translated My orders keeps its tab counts current (TN10 2026-10-17: "History (0)" over five listed orders)', async ({ appPage: page, mock }) => {
  await page.addInitScript(installTranslator);
  await fund(mock, 'alice');
  const ids = await seedOrders(mock, [{ side: 'ask', price: 2_700_000, tokens: 1 }, { side: 'ask', price: 2_710_000, tokens: 1 }]);
  await openMarket(page, mock, { route: '/orders', connect: false });
  await expect(page.locator('html')).toHaveClass(/translated-ltr/);
  await connectKey(page);
  for (const id of ids) await expect(page.getByTestId(`order-row-${id}`)).toHaveAttribute('data-status', 'open');
  await expect.poll(() => seen(page, 'orders-tab-active')).toBe('Active (2)');
  // one fills: the counts change while the page is translated
  await mock.fill(ids[0]!);
  await page.getByTestId('orders-refresh').click();
  await expect.poll(() => seen(page, 'orders-tab-history'), { timeout: 20_000 }).toBe('History (1)');
  await expect.poll(() => seen(page, 'orders-tab-active')).toBe('Active (1)');
  expect(await staleTexts(page), 'texts Preact changed after the translator replaced them').toEqual([]);
});
