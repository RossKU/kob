// Drives the order ticket and the pre-sign confirmation screen through their test ids; reads the disclosure and the decoded confirmation into plain
// objects so specs assert on values, never on layout. Everything waits on UI state (visible / enabled / call log), never on fixed sleeps.
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

/** Waits until the planner settled on the typed form: not pending, and either review is enabled or an error issue is shown. */
export async function settlePlan(page: Page): Promise<void> {
  await expect(page.getByTestId('order-ticket')).toHaveAttribute('aria-busy', 'false');
  await expect(page.getByTestId('order-pending')).toHaveCount(0);
}

/** Waits for a plan without errors: the disclosure is shown and review is enabled. */
export async function waitReviewable(page: Page): Promise<void> {
  await settlePlan(page);
  await expect(page.getByTestId('order-issues').locator('[data-severity="error"]')).toHaveCount(0);
  await expect(page.getByTestId('order-review')).toBeEnabled();
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

// ------------------------------------------------------------------------------------------------ confirmation screen

export interface ConfirmCard {
  id: string;
  title: string;
  badge: string;
  rows: Record<string, string>;
  children: ConfirmCard[];
}
export interface ConfirmSection {
  id: string;
  rows: Record<string, string>;
  cards: ConfirmCard[];
}
export interface ConfirmRead {
  heading: string;
  kind: string;
  sections: Record<string, ConfirmSection>;
  /** the orders the transaction creates */
  created: ConfirmCard[];
  /** the orders it closes */
  closed: ConfirmCard[];
  blocking: string[];
  warnings: string[];
  info: string[];
  wallet: string[];
  text: string;
  summaryText: string;
}

/** Reads the decoded confirmation screen (the modal must be open). Row keys are the row ids of confirm-model.ts (`amount`, `minFill`, `price`, `allIn`, `tip`, `expiry`, `locked-total`, `net-kas` ...). */
export async function readConfirm(page: Page): Promise<ConfirmRead> {
  await expect(page.getByTestId('confirm-screen')).toBeVisible();
  return page.getByTestId('confirm-screen').evaluate((root) => {
    const clean = (s: string | null | undefined) => (s ?? '').replace(/\s+/g, ' ').trim();
    const rowsOf = (scope: Element, prefix: string): Record<string, string> => {
      const out: Record<string, string> = {};
      for (const dd of scope.querySelectorAll<HTMLElement>(':scope > dl > .cf-row > dd[data-testid]')) {
        const id = dd.dataset.testid!;
        if (id.startsWith(`${prefix}-`)) out[id.slice(prefix.length + 1)] = [...dd.children].map((c) => clean(c.textContent)).filter(Boolean).join(' | ') || clean(dd.textContent);
      }
      return out;
    };
    const cardOf = (el: HTMLElement, section: string): any => ({
      id: el.dataset.testid!.slice(`confirm-${section}-`.length),
      title: clean(el.querySelector(':scope > .cf-card-head h4')?.textContent),
      badge: clean(el.querySelector(':scope > .cf-card-head .cf-badge')?.textContent),
      rows: rowsOf(el, el.dataset.testid!),
      children: [...el.querySelectorAll<HTMLElement>(':scope > article.cf-card')].map((c) => cardOf(c, section)),
    });
    const sections: Record<string, any> = {};
    for (const s of root.querySelectorAll<HTMLElement>('[data-testid^="confirm-section-"]')) {
      const id = s.dataset.testid!.slice('confirm-section-'.length);
      sections[id] = { id, rows: rowsOf(s, `confirm-${id}`), cards: [...s.querySelectorAll<HTMLElement>(':scope > article.cf-card')].map((c) => cardOf(c, id)) };
    }
    const list = (tid: string) => [...root.querySelectorAll(`[data-testid="${tid}"] li`)].map((li) => clean(li.textContent));
    return {
      heading: clean(root.querySelector('h2')?.textContent),
      kind: (root.querySelector('.cf') as HTMLElement | null)?.dataset.kind ?? '',
      sections,
      created: sections.create?.cards ?? [],
      closed: sections.close?.cards ?? [],
      blocking: list('confirm-blocking-list'),
      warnings: list('confirm-warnings'),
      info: list('confirm-info'),
      wallet: list('confirm-wallet-notice'),
      text: (root as HTMLElement).innerText,
      summaryText: (root.querySelector('[data-testid="confirm-summary"]') as HTMLElement | null)?.innerText ?? '',
    };
  });
}

/** Clicks review and waits for the confirmation screen; asserts that the decoder found nothing blocking. */
export async function openReview(page: Page): Promise<ConfirmRead> {
  await waitReviewable(page);
  await page.getByTestId('order-review').click();
  await expect(page.getByTestId('confirm-screen')).toBeVisible();
  await expect(page.getByTestId('confirm-blocking')).toHaveCount(0);
  await expect(page.getByTestId('confirm-summary')).toBeVisible();
  return readConfirm(page);
}

/** Acknowledges and signs; waits until the mock node accepted the transaction and the app shows it confirmed. Returns the tx id. */
export async function acknowledgeAndSign(page: Page): Promise<string> {
  await expect(page.getByTestId('confirm-sign')).toBeDisabled();
  await page.getByTestId('confirm-ack').check();
  await expect(page.getByTestId('confirm-sign')).toBeEnabled();
  await page.getByTestId('confirm-sign').click();
  await expect(page.getByTestId('tx-id')).toBeVisible({ timeout: 30_000 });
  await expect(page.getByTestId('tx-status')).toHaveAttribute('data-status', 'confirmed', { timeout: 30_000 });
  const txid = await page.getByTestId('tx-id').getAttribute('data-value');
  expect(txid).toMatch(/^[0-9a-f]{64}$/);
  return txid!;
}

export interface Placed {
  txid: string;
  submission: SubmissionSummary;
  /** the indexer views (decoded state included) of the orders the transaction created */
  views: any[];
}

/** After `acknowledgeAndSign`: the mock node's decoded submission of that tx and the indexer views of what it created. */
export async function placedBy(mock: MockClient, txid: string): Promise<Placed> {
  const sub = (await mock.submitted()).find((s) => s.txid === txid);
  expect(sub, `the mock node accepted ${txid}`).toBeTruthy();
  const views = await Promise.all(sub!.created.map((c) => mock.order(c.covenantId)));
  return { txid, submission: sub!, views };
}

/** review -> confirm screen -> sign -> accepted. Closes the confirmation dialog. */
export async function reviewAndSign(page: Page, mock: MockClient): Promise<{ confirm: ConfirmRead; placed: Placed }> {
  const confirm = await openReview(page);
  const txid = await acknowledgeAndSign(page);
  const placed = await placedBy(mock, txid);
  await page.getByTestId('confirm-close').click();
  await expect(page.getByTestId('confirm-screen')).toHaveCount(0);
  return { confirm, placed };
}

// ------------------------------------------------------------------------------------------------ wallet call log

/**
 * The wallet was asked to sign exactly what the app built: ONE request for the accepted transaction, over exactly the inputs the app said it signs
 * (`sign[]`, i.e. every input the wallet key controls: P2PK funds and the maker-signed covenant entries), all of them signed, none left unsigned,
 * and the transaction the wallet saw is the one the node accepted (same outputs and payload; only the signature scripts were added).
 */
export async function expectWalletSigned(wallet: MockWalletHandle, placed: Placed, opts: { requests?: number; inputs?: number[] } = {}): Promise<void> {
  const calls = await wallet.calls();
  expect(calls, 'sign requests').toHaveLength(opts.requests ?? 1);
  const call = calls[calls.length - 1]!;
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
