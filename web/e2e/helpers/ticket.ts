// Drives the order ticket (one button -> the wallet's own approval -> sent, with the progress inline) through its test ids; reads the disclosure and the
// status line into plain objects so specs assert on values, never on layout. Everything waits on UI state (visible / enabled / call log), never on fixed sleeps.
import type { Page } from '@playwright/test';
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

/** Waits for a plan without errors: the disclosure is shown and the order button is enabled. */
export async function waitReviewable(page: Page): Promise<void> {
  await settlePlan(page);
  await expect(page.getByTestId('order-issues').locator('[data-severity="error"]')).toHaveCount(0);
  await expect(page.getByTestId('order-place')).toBeEnabled();
  await expect(page.getByTestId('order-disclosure')).toBeVisible();
}

export interface DisclosureRead {
  /** row id -> "value detail" text (`allInPrice`, `allInTotal`, `expiry`, `stopWorst`, `repeat` ...) */
  rows: Record<string, string>;
  /** carrier kind -> text (`orderCarrier`, `tokenCarrier`, ...) */
  carriers: Record<string, string>;
  /** the planner's note tags (`gtc`, `dayOrder`, `stopTrigger` ...) */
  notes: string[];
  summary: string;
  kasLocked: string;
  tokensEscrowed: string | null;
  fee: string | null;
  text: string;
}

export async function readDisclosure(page: Page): Promise<DisclosureRead> {
  await expect(page.getByTestId('order-disclosure')).toBeVisible();
  return page.getByTestId('order-disclosure').evaluate((root) => {
    const rows: Record<string, string> = {};
    const carriers: Record<string, string> = {};
    const joined = (dd: Element) => [...dd.children].map((c) => (c.textContent ?? '').replace(/\s+/g, ' ').trim()).filter(Boolean).join(' | ') || (dd.textContent ?? '').trim();
    const skip = new Set(['summary', 'carriers', 'notes', 'kasLocked', 'tokensEscrowed', 'fee']);
    for (const el of root.querySelectorAll<HTMLElement>('[data-testid^="disc-"]')) {
      const id = el.dataset.testid!.slice(5);
      const dd = el.querySelector('dd');
      if (id.startsWith('carrier-')) {
        if (dd) carriers[id.slice(8)] = joined(dd);
      } else if (!skip.has(id) && dd) rows[id] = joined(dd);
    }
    const val = (id: string) => root.querySelector(`[data-testid="disc-${id}"] dd .tk-value`)?.textContent?.trim() ?? null;
    return {
      rows,
      carriers,
      notes: [...root.querySelectorAll<HTMLElement>('[data-testid="disc-notes"] li')].map((li) => li.dataset.note!),
      summary: root.querySelector('[data-testid="disc-summary"]')?.textContent?.trim() ?? '',
      kasLocked: val('kasLocked') ?? '',
      tokensEscrowed: val('tokensEscrowed'),
      fee: val('fee'),
      text: (root as HTMLElement).innerText,
    };
  });
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

export type SendPhase = 'checking' | 'signing' | 'sending' | 'submitted' | 'blocked' | 'rejected' | 'failed';
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

/** Waits until the ticket is ready, then clicks the order button ONCE: the click goes straight to the checks, the wallet's approval and the node (no confirmation screen exists). */
export async function clickPlace(page: Page): Promise<void> {
  await waitReviewable(page);
  await page.getByTestId('order-place').click();
}

/**
 * Waits for the status line to stop at `phase` (blocked | rejected | failed): ONE error line with the finding / failure code, nothing submitted,
 * no confirmation screen. Returns what it shows. Pair it with `wallet.calls()` where the wallet must not have been asked at all.
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

/** The new flow for one order: ready ticket -> ONE click -> the wallet's approval -> sent -> confirmed. Returns the accepted transaction; the confirmation screen must never appear. */
export async function placeOrder(page: Page, mock: MockClient): Promise<Placed> {
  await clickPlace(page);
  const txid = await waitSubmitted(page);
  await expect(page.getByTestId('confirm-screen')).toHaveCount(0);
  return placedBy(mock, txid);
}

/** A ladder: one click, one transaction per level (each its own wallet request); waits for "All N levels placed" and returns the accepted transactions in order. */
export async function placeLadder(page: Page, mock: MockClient, levels: number): Promise<Placed[]> {
  const before = (await mock.submitted(false)).length;
  await clickPlace(page);
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
