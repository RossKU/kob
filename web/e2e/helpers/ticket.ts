// Drives the order ticket (one button -> the final confirmation popup -> the wallet's own approval -> sent, with the progress inline) through its test ids;
// reads the confirmation popup and the status line into plain objects so specs assert on values, never on layout. The private-key signer shows the same
// confirmation in its own frame instead of an app popup. Everything waits on UI state (visible / enabled / call log), never on fixed sleeps.
import type { Locator, Page } from '@playwright/test';
import { expect, type MockClient, type MockWalletHandle, type SubmissionSummary } from '../fixtures';

export type Side = 'buy' | 'sell';
export type FieldValue = string | boolean;
/** Field ids are the intent paths of the form (`price`, `stop`, `exit.takeProfit`, `repeat.count`, `lifetime` ...). */
export type FieldValues = Record<string, FieldValue>;

/** The `data-testid` of a ticket field (form-state.ts `FIELDS`): amount / price / tip have the short order-* ids. */
export const fieldTestId = (id: string): string => (id === 'amount' ? 'order-amount' : id === 'price' ? 'order-price' : id === 'tip' ? 'order-tip' : `field-${id}`);

// ------------------------------------------------------------------------------------------------ ticket

/** Chooses side and order type (the side follows the type: TWAP only sells, DCA only buys, close only sells). */
export async function pickType(page: Page, type: string, side?: Side): Promise<void> {
  await page.getByTestId('order-type').selectOption(type);
  await expect(page.getByTestId('order-ticket')).toHaveAttribute('data-type', type);
  if (side) {
    await page.getByTestId(`order-side-${side}`).click();
    await expect(page.getByTestId('order-ticket')).toHaveAttribute('data-side', side);
  }
}

async function openAdvanced(page: Page): Promise<void> {
  const box = page.getByTestId('order-advanced');
  if ((await box.getAttribute('open')) === null) await box.locator('summary').click();
}

/** Fills the fields in order (later fields may only appear after an earlier switch, e.g. `lifetime` = gtd shows `lifetimeAt`). Advanced fields open the disclosure box. */
export async function fillFields(page: Page, values: FieldValues): Promise<void> {
  for (const [id, value] of Object.entries(values)) {
    const loc = page.getByTestId(fieldTestId(id));
    await loc.waitFor({ state: 'attached' });
    if (!(await loc.isVisible())) await openAdvanced(page);
    const tag = await loc.evaluate((el) => `${el.tagName}:${(el as HTMLInputElement).type ?? ''}`);
    if (tag.startsWith('SELECT')) await loc.selectOption(String(value));
    else if (tag === 'INPUT:checkbox') {
      if (value) await loc.check();
      else await loc.uncheck();
    } else await loc.fill(String(value));
  }
}

/** Waits until the planner settled on the typed form: not pending, and either the order button is enabled or an error issue is shown. */
export async function settlePlan(page: Page): Promise<void> {
  await expect(page.getByTestId('order-ticket')).toHaveAttribute('aria-busy', 'false');
  await expect(page.getByTestId('order-pending')).toHaveCount(0);
}

/** Waits for a plan without errors: the order button is enabled (the order's details are shown only in the final confirmation). */
export async function waitReviewable(page: Page): Promise<void> {
  await settlePlan(page);
  await expect(page.getByTestId('order-issues').locator('[data-severity="error"]')).toHaveCount(0);
  await expect(page.getByTestId('order-place')).toBeEnabled();
}

// ------------------------------------------------------------------------------------------------ the final confirmation

/** One order the transaction creates or amends, as the table shows it (an if-done entry's committed exit nested as `exit`). */
export interface ReceiptOrder {
  /** "Sell limit order", "Buy market order (auction)" ... */
  title: string;
  /** "EXKCC (e5e5e5e5…e5e5e5e5)": the ticker hint and the covenant id the transaction carries */
  token: string;
  /** row label -> value ("Limit price" -> "0.027 KAS per EXKCC") */
  rows: Record<string, string>;
  exit: ReceiptOrder | null;
}

/** The final confirmation: the private-key signer's table (title, the net effect, the fee, every order's terms). */
export interface ReceiptRead {
  /** "Place order", "Cancel order", "Send" ... */
  title: string;
  /** the summary lines before the fee: label -> value ("KAS in your wallet" -> "-4.014866 KAS", "Locked in orders" -> "4 KAS", a token's net ...) */
  lines: Record<string, string>;
  /** the labels of the summary lines in order (the first is the net KAS) */
  labels: string[];
  /** the network fee, full precision ("0.014866 KAS"): what `formatKas(fee)` of the accepted transaction gives */
  fee: string;
  orders: ReceiptOrder[];
  /** the "updated" notice of a re-timed order (the app popup only) */
  updated: boolean;
  /** a problems note is shown */
  problems: boolean;
  /** `app` = the popup of the app (external wallets), `signer` = the private-key signer's frame (the same table, the only prompt of that wallet) */
  from: 'app' | 'signer';
}

/** True when the connected wallet is the private-key signer (its frame shows the confirmation; the app shows no popup of its own). */
export async function isPrivateKey(page: Page): Promise<boolean> {
  const chip = page.getByTestId('wallet-connected');
  return (await chip.count()) > 0 && (await chip.getAttribute('data-wallet')) === 'local-key';
}

const signerFrameOf = (page: Page) => page.frameLocator('[data-testid="signer-frame"]');

interface TableIds {
  title: string;
  summary: string;
  fee: string;
  order: string;
  line: string;
  feeLine: string;
  head: string;
  token: string;
  problems: string;
}
const APP_IDS: TableIds = { title: 'confirm-order-title', summary: 'confirm-order-summary', fee: 'confirm-order-fee', order: 'confirm-order-order-', line: '.send-line', feeLine: 'send-fee', head: 'h3', token: 'p.send-token', problems: 'confirm-order-problems' };
const SIGNER_IDS: TableIds = { title: 'signer-title', summary: 'signer-summary', fee: 'signer-fee', order: 'signer-order-', line: '.line', feeLine: 'fee', head: 'h2', token: 'p.token', problems: 'signer-problems' };

/** Reads the table inside the page (the same structure in the popup and in the frame; only the ids and classes differ). */
const readTable = (root: Element, ids: TableIds) => {
  const text = (e: Element | null | undefined) => (e?.textContent ?? '').replace(/\s+/g, ' ').trim();
  const byId = (id: string) => root.querySelector(`[data-testid="${id}"]`);
  const lines: { label: string; value: string }[] = [];
  for (const el of byId(ids.summary)?.querySelectorAll(ids.line) ?? []) if (!el.classList.contains(ids.feeLine)) lines.push({ label: text(el.querySelector('dt')), value: text(el.querySelector('dd')) });
  const order = (el: Element): { title: string; token: string; rows: Record<string, string>; exit: unknown } => {
    const rows: Record<string, string> = {};
    // a start or an expiry is said from the clock ("in 28 s" over its DAA score): read as the DAA score it carries, as the row says it without a clock
    const value = (dd: Element | null) => (dd?.getAttribute('data-daa') ? 'DAA score ' + dd.getAttribute('data-daa') : text(dd));
    for (const l of el.querySelectorAll(':scope > dl > ' + ids.line)) rows[text(l.querySelector('dt'))] = value(l.querySelector('dd'));
    const exit = el.querySelector(':scope > section');
    return { title: text(el.querySelector(':scope > ' + ids.head)), token: text(el.querySelector(':scope > ' + ids.token)), rows, exit: exit ? order(exit) : null };
  };
  const orders = [...root.querySelectorAll(`[data-testid^="${ids.order}"]`)].filter((e) => !(e as HTMLElement).dataset.testid!.endsWith('-exit') && !(e as HTMLElement).dataset.testid!.endsWith('problems')).map(order);
  return { title: text(byId(ids.title)), lines, fee: text(byId(ids.fee)), orders, problems: !!byId(ids.problems) };
};

type Table = ReturnType<typeof readTable>;
function toReceipt(t: Table, from: 'app' | 'signer', updated: boolean): ReceiptRead {
  const conv = (o: any): ReceiptOrder => ({ title: o.title, token: o.token, rows: o.rows, exit: o.exit ? conv(o.exit) : null });
  return {
    title: t.title, lines: Object.fromEntries(t.lines.map((l) => [l.label, l.value])), labels: t.lines.map((l) => l.label), fee: t.fee, orders: t.orders.map(conv),
    updated, problems: t.problems, from,
  };
}

/** Reads the open final confirmation: the popup of the app, or (private-key signer) the signer frame. Waits until it is shown; confirms nothing. */
export async function readReceipt(page: Page): Promise<ReceiptRead> {
  if (await isPrivateKey(page)) {
    const f = signerFrameOf(page);
    await expect(f.getByTestId('signer-title')).toBeVisible({ timeout: 30_000 });
    await expect(f.getByTestId('signer-fee')).toBeVisible();
    return toReceipt(await f.locator('body').evaluate(readTable, SIGNER_IDS), 'signer', false);
  }
  const popup = page.getByTestId('confirm-order');
  await expect(popup).toBeVisible({ timeout: 30_000 });
  await expect(popup.getByTestId('confirm-order-fee')).toBeVisible();
  return toReceipt(await popup.evaluate(readTable, APP_IDS), 'app', (await popup.getByTestId('confirm-order-updated').count()) > 0);
}

/**
 * Reads the private-key signer's table the way a person does when it is taller than the frame: by scrolling it with the mouse wheel (a table that
 * fits, as at the default 1280x720 viewport, needs nothing). Every part of the table must have been fully on screen for about 0.4 s (signer/seen.ts)
 * before Approve is enabled. A script's `scrollTo` inside the cross-origin frame is not applied reliably; a real wheel is.
 */
export async function readSignerFrame(page: Page): Promise<void> {
  const body = signerFrameOf(page).locator('body');
  const box = (await page.getByTestId('signer-frame').boundingBox())!;
  await page.mouse.move(box.x + box.width / 2, box.y + box.height / 2);
  const { total, view } = await body.evaluate(() => ({ total: document.documentElement.scrollHeight, view: window.innerHeight }));
  if (total <= view) return; // the whole table fits the frame: nothing to scroll
  // a part counts as read after SEEN_MIN_MS (0.4 s) fully on screen and unmoved, which the observer reports with a delay: each stop lasts longer
  const STOP_MS = 900;
  for (let last = -1; ; ) {
    await page.waitForTimeout(STOP_MS);
    const y = await body.evaluate(() => window.scrollY);
    if (y === last) break;
    last = y;
    await page.mouse.wheel(0, Math.floor(view / 3));
  }
}

/** Confirms the open final confirmation: the popup's Continue in wallet, or Approve in the signer frame (which then signs). */
export async function confirmReceipt(page: Page): Promise<void> {
  if (await isPrivateKey(page)) {
    await readSignerFrame(page);
    await expect(signerFrameOf(page).getByTestId('signer-approve')).toBeEnabled();
    await signerFrameOf(page).getByTestId('signer-approve').click();
    await expect(page.getByTestId('signer-overlay')).toBeHidden();
  } else {
    await page.getByTestId('confirm-order-ok').click();
    await expect(page.getByTestId('confirm-order')).toHaveCount(0);
  }
}

/** Cancels the open final confirmation: the app popup's Cancel, or Cancel in the signer frame. Nothing is sent. */
export async function cancelReceipt(page: Page): Promise<void> {
  if (await isPrivateKey(page)) {
    await signerFrameOf(page).getByTestId('signer-reject').click();
    await expect(page.getByTestId('signer-overlay')).toBeHidden();
  } else {
    await page.getByTestId('confirm-order-cancel').click();
    await expect(page.getByTestId('confirm-order')).toHaveCount(0);
  }
}

/** The app popup, or a status that stopped the click before it, after a click on an order button / a My orders action. */
function popupOrStop(page: Page, testid: string): Locator {
  const stopped = ['blocked', 'rejected', 'failed'].map((p) => `[data-testid="${testid}"][data-phase="${p}"]`).join(', ');
  return page.getByTestId('confirm-order').or(page.locator(stopped));
}

/**
 * The step after the click: an external wallet's final confirmation popup is confirmed (the wallet is asked only after that); a click that stops
 * before it (blocked / failed) shows no popup and returns; the private-key signer shows no app popup (its frame is the confirmation: see `readReceipt`).
 */
export async function confirmAfterClick(page: Page, testid: StatusId = 'order-status'): Promise<void> {
  if (await isPrivateKey(page)) return;
  await popupOrStop(page, testid).first().waitFor({ state: 'visible', timeout: 30_000 });
  if (await page.getByTestId('confirm-order').isVisible()) await confirmReceipt(page);
}

/**
 * Confirms every app popup that shows up (a ladder: one per level; a multi-step My orders flow: one per step; the updated popup of a re-timed
 * order) until `done` is visible. The private-key signer shows none, so there this only waits.
 */
export async function confirmUntil(page: Page, done: Locator, opts: { timeout?: number } = {}): Promise<void> {
  const deadline = Date.now() + (opts.timeout ?? 60_000);
  const popup = page.getByTestId('confirm-order');
  while (!(await done.isVisible())) {
    const left = deadline - Date.now();
    if (left <= 0) throw new Error('confirmUntil: the awaited element never showed up');
    await popup.or(done).first().waitFor({ state: 'visible', timeout: left });
    if (await popup.isVisible()) await confirmReceipt(page);
  }
}

// ------------------------------------------------------------------------------------------------ placing (one button, the wallet's own approval)

export interface Placed {
  txid: string;
  submission: SubmissionSummary;
  /** the indexer views (decoded state included) of the orders the transaction created */
  views: any[];
}

/** After a send: the mock node's decoded submission of that tx and the indexer views of what it created. */
export async function placedBy(mock: MockClient, txid: string): Promise<Placed> {
  const sub = (await mock.submitted()).find((s) => s.txid === txid);
  expect(sub, `the mock node accepted ${txid}`).toBeTruthy();
  const views = await Promise.all(sub!.created.map((c) => mock.order(c.covenantId)));
  return { txid, submission: sub!, views };
}

export type SendPhase = 'checking' | 'confirming' | 'signing' | 'sending' | 'submitted' | 'blocked' | 'rejected' | 'failed';
export type StatusId = 'order-status' | 'flow-status';

export interface SendRead {
  phase: SendPhase;
  /** blocked / rejected / failed: the finding / failure code of `send-error` and its one sentence */
  code: string | null;
  text: string | null;
  /** submitted: the full txid and the `tx-status` (submitted | confirmed | unconfirmed) */
  txid: string | null;
  status: string | null;
}

/** Reads the inline status line (`order-status` of the ticket, `flow-status` of My orders). */
export async function readSendStatus(page: Page, testid: StatusId = 'order-status'): Promise<SendRead> {
  const root = page.getByTestId(testid);
  await expect(root).toBeVisible();
  const one = async (id: string, attr: string | null) => {
    const loc = root.getByTestId(id);
    if ((await loc.count()) === 0) return null;
    return attr ? await loc.getAttribute(attr) : ((await loc.textContent()) ?? '').trim();
  };
  return {
    phase: (await root.getAttribute('data-phase')) as SendPhase,
    code: await one('send-error', 'data-code'),
    text: await one('send-error', null),
    txid: await one('tx-id', 'data-value'),
    status: await one('tx-status', 'data-status'),
  };
}

/**
 * Waits until the ticket is ready, then clicks the order button ONCE and confirms the final confirmation popup of an external wallet (the wallet is
 * asked only after that). With the private-key signer there is no app popup: its frame is the confirmation (approve there).
 */
export async function clickPlace(page: Page): Promise<void> {
  await clickPlaceOnly(page);
  await confirmAfterClick(page);
}

/** Waits until the ticket is ready and clicks the order button ONCE, leaving the final confirmation open (an external wallet's popup, or the signer frame). */
export async function clickPlaceOnly(page: Page): Promise<void> {
  await waitReviewable(page);
  await page.getByTestId('order-place').click();
}

/** Clicks the order button, reads the final confirmation and cancels it: what the order would do, nothing sent. */
export async function peekReceipt(page: Page): Promise<ReceiptRead> {
  await clickPlaceOnly(page);
  const receipt = await readReceipt(page);
  await cancelReceipt(page);
  return receipt;
}

/** Clicks the order button, reads the final confirmation BEFORE confirming it, confirms it, and returns what it showed. */
export async function clickPlaceAndRead(page: Page): Promise<ReceiptRead> {
  await clickPlaceOnly(page);
  const receipt = await readReceipt(page);
  await confirmReceipt(page);
  return receipt;
}

/**
 * Waits for the status line to stop at `phase` (blocked | rejected | failed): ONE error line with the finding / failure code, nothing submitted,
 * no issuance confirmation screen. Returns what it shows. Pair it with `wallet.calls()` where the wallet must not have been asked at all.
 */
export async function expectStopped(page: Page, phase: 'blocked' | 'rejected' | 'failed', opts: { code?: string | RegExp; testid?: StatusId } = {}): Promise<SendRead> {
  const testid = opts.testid ?? 'order-status';
  await expect(page.getByTestId(testid)).toHaveAttribute('data-phase', phase, { timeout: 30_000 });
  await expect(page.getByTestId('confirm-screen')).toHaveCount(0);
  const read = await readSendStatus(page, testid);
  if (typeof opts.code === 'string') expect(read.code, 'finding / failure code').toBe(opts.code);
  else if (opts.code) expect(read.code, 'finding / failure code').toMatch(opts.code);
  expect(read.txid, 'nothing was submitted').toBeNull();
  return read;
}

/** Waits until the status line shows the submitted transaction confirmed in a block; returns its tx id. */
export async function waitSubmitted(page: Page, testid: StatusId = 'order-status'): Promise<string> {
  const root = page.getByTestId(testid);
  await expect(root.getByTestId('tx-id')).toBeVisible({ timeout: 30_000 });
  await expect(root.getByTestId('tx-status')).toHaveAttribute('data-status', 'confirmed', { timeout: 30_000 });
  const txid = await root.getByTestId('tx-id').getAttribute('data-value');
  expect(txid).toMatch(/^[0-9a-f]{64}$/);
  return txid!;
}

/** The flow for one order: ready ticket -> ONE click -> the final confirmation popup -> the wallet's approval -> sent -> confirmed. Returns the accepted transaction; the issuance confirmation screen must never appear. */
export async function placeOrder(page: Page, mock: MockClient): Promise<Placed> {
  await clickPlace(page);
  const txid = await waitSubmitted(page);
  await expect(page.getByTestId('confirm-screen')).toHaveCount(0);
  return placedBy(mock, txid);
}

/** A ladder: one click, one transaction per level (each its own popup and wallet request); waits for "All N levels placed" and returns the accepted transactions in order. */
export async function placeLadder(page: Page, mock: MockClient, levels: number): Promise<Placed[]> {
  const before = (await mock.submitted(false)).length;
  await clickPlaceOnly(page);
  await confirmUntil(page, page.getByTestId('order-result-ladder'));
  await expect(page.getByTestId('order-result-ladder')).toBeVisible({ timeout: 60_000 });
  await expect(page.getByTestId('confirm-screen')).toHaveCount(0);
  const mine = (await mock.submitted()).slice(before);
  expect(mine, 'one transaction per level').toHaveLength(levels);
  return Promise.all(mine.map((s) => placedBy(mock, s.txid)));
}

// ------------------------------------------------------------------------------------------------ wallet call log

/**
 * The wallet was asked to sign exactly what the app built: ONE request for the accepted transaction, over exactly the inputs the app said it signs
 * (`sign[]`, i.e. every input the wallet key controls: P2PK funds and the maker-signed covenant entries), all of them signed, none left unsigned,
 * and the transaction the wallet saw is the one the node accepted (same outputs and payload; only the signature scripts were added).
 */
export async function expectWalletSigned(wallet: MockWalletHandle, placed: Placed, opts: { requests?: number; inputs?: number[]; call?: number } = {}): Promise<void> {
  const calls = await wallet.calls();
  if (opts.requests !== undefined || opts.call === undefined) expect(calls, 'sign requests').toHaveLength(opts.requests ?? 1);
  const call = calls[opts.call ?? calls.length - 1]!; // a flow of several steps: `call` picks the request of that step
  expect(call.status).toBe('signed');
  expect(call.unsignedInputs).toEqual([]);
  expect([...call.signedInputs].sort((a, b) => a - b)).toEqual([...call.inputs].sort((a, b) => a - b));
  if (opts.inputs) expect([...call.inputs].sort((a, b) => a - b)).toEqual(opts.inputs);
  const seen = call.tx;
  const accepted = placed.submission.tx;
  expect(seen.outputs.length).toBe(accepted.outputs.length);
  expect(seen.inputs.length).toBe(accepted.inputs.length);
  expect(seen.payload ?? '').toBe(accepted.payload ?? '');
  seen.outputs.forEach((o: any, i: number) => {
    expect(String(o.value)).toBe(String(accepted.outputs[i].value));
    expect(JSON.stringify(o.scriptPublicKey)).toBe(JSON.stringify(accepted.outputs[i].scriptPublicKey));
  });
}

/**
 * What an accepted transaction commits to for each order it created, without the clock-dependent expiry: the contract, its decoded state, the KAS the
 * order output locks and the token custodies. Two placements of the same economic order (typed in either orientation) must have equal terms.
 */
export function onChainTerms(placed: Placed): { contract: string; state: Record<string, string>; locked: string; custody: string[] }[] {
  return placed.views.map((view, i) => {
    const { expiryDaa: _expiry, ...state } = view.state.state as Record<string, string>;
    return {
      contract: view.contract,
      state,
      locked: String(placed.submission.tx.outputs[placed.submission.created[i]!.output].value),
      custody: placed.submission.token_outputs.filter((o) => o.role === 'custody').map((o) => o.amount),
    };
  });
}

/** `placeOrder` that also returns the final confirmation as it was shown BEFORE it was confirmed (an external wallet's popup, or the signer frame). */
export async function placeOrderAndRead(page: Page, mock: MockClient): Promise<Placed & { receipt: ReceiptRead }> {
  const receipt = await clickPlaceAndRead(page);
  const txid = await waitSubmitted(page);
  await expect(page.getByTestId('confirm-screen')).toHaveCount(0);
  return { ...(await placedBy(mock, txid)), receipt };
}
