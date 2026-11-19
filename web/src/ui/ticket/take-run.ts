// The steps of an order filled from the book directly (OrderTicket `placeTake`, kob/take.ts). The
// ticket gives the driver its planning, its sending, its status line and its stepper.
import type { Intent } from '../../kob/plan';
import type { OrderPlan } from '../../kob/plan-types';
import { MAX_TAKE_STEPS } from '../../kob/take';
import type { StepperItem } from '../kit/stepper-model';

/** Answers of the node or the checks that mean the order a take spends is gone to someone else. */
export const LOST_RACE = new Set(['orphan', 'double-spend', 'inputs-changed', 'inputs-unconfirmed', 'input-unconfirmed', 'input-pending-spent']);

/** A step that could not be planned: its sentence, the raw builder text and the status code. */
export interface TakeStop {
  text: string;
  raw?: string;
  code?: string;
}

/** What a send of one step came to (confirm/SendStatus.tsx `SendOutcome`, the parts used here). */
export type TakeSendOutcome = { status: 'submitted' } | { status: 'stopped'; send: { code?: string } };

export interface TakeRunHooks<J extends { built: unknown; plan?: OrderPlan }> {
  /**
   * Plans one step: `index` is its place among the transactions of the click, `fresh` reads the
   * book again right before it is built. It is held to `shown`, the terms the ticket showed (null:
   * none).
   */
  prepare(intent: Intent, index: number, shown: OrderPlan | null, fresh: boolean): Promise<J | TakeStop>;
  send(job: J): Promise<TakeSendOutcome>;
  /** The status line of a step that stops the click. */
  stop(s: TakeStop): void;
  /** The stepper. */
  show(items: StepperItem[]): void;
  words: {
    /** "Fill" for a single step, else "Fill k+1 of n". */
    step(k: number, n: number): string;
    rest: string;
    retry: string;
    nothingNow: string;
    /** A later step would pass the worst price shown, after `filled` base units. */
    stopTerms(filled: bigint): string;
    /** The orders at the price were gone twice, after `filled` base units. */
    stopGone(filled: bigint): string;
  };
}

/**
 * Runs the takes of a crossing order. Each take is its own transaction, planned at its turn on the
 * book read again right before, signed and sent before the next. Then the rest rests at the limit
 * (GTC, GTD or day) or is not placed (IOC, FOK, market). A take whose order is gone (a lost race)
 * is planned again on a fresh book once. A second loss places the order on the book instead: a
 * limit as its auction, an immediate order as its covenant order while nothing is filled yet.
 * `done` is true when the order is through (all of it, or some taken of an order whose rest is not
 * placed).
 *
 * Every step is held to the terms the ticket showed (`shown`), not only the first: a take at most
 * at the limit shown (a market order's worst price, however its touch moved), and in all no more
 * than the ticket said fills now when the rest is not placed. A step that would pass them stops
 * the run, and the status line says how much was filled.
 */
export async function runTakes<J extends { built: unknown; plan?: OrderPlan }>(intent0: Intent, shown: OrderPlan, h: TakeRunHooks<J>): Promise<{ done: boolean }> {
  const tk0 = shown.take!;
  const restsLimit = tk0.restKind === 'limit';
  const word = h.words.step;
  let items: StepperItem[] = tk0.steps.map((_, k) => ({ label: word(k, tk0.steps.length), state: 'waiting', testid: `order-take-step-${k + 1}` }));
  if (restsLimit && tk0.rest > 0n) items.push({ label: h.words.rest, state: 'waiting', testid: 'order-take-step-rest' });
  const show = () => h.show([...items]);
  const setItem = (k: number, x: Partial<StepperItem>) => {
    while (items.length <= k) items.push({ label: word(items.length, items.length + 1), state: 'waiting', testid: `order-take-step-${items.length + 1}` });
    items[k] = { ...items[k]!, ...x };
    show();
  };
  show();
  const amount = (intent0 as { amount: bigint }).amount;
  // What the run may fill: the whole amount when the rest rests at the limit, else what the ticket
  // showed filling now ("Rest: not placed").
  let left = restsLimit || tk0.total.amount >= amount ? amount : tk0.total.amount;
  let filled = 0n;
  let races = 0;
  let fallback = false;
  let at = 0;
  let txs = 0;
  for (let guard = 0; left > 0n && guard < MAX_TAKE_STEPS + 3; guard++) {
    const now = (fallback
      ? { ...intent0, amount: left, ...(intent0.type === 'limit' ? { crossing: 'auction' } : { take: false }) }
      : { ...intent0, amount: left }) as Intent;
    setItem(at, { state: 'current', busy: true });
    let job = await h.prepare(now, txs, shown, true);
    if ('built' in job && !job.plan?.take && !fallback) {
      // Nothing more to take now. The rest rests as a plain limit, or (IOC, FOK, market) it is
      // not placed.
      if (!restsLimit) {
        items = items.slice(0, at);
        show();
        // Nothing at all was filled (the book moved since the plan). That is said, not a silent
        // end of the click.
        if (txs === 0) h.stop({ text: h.words.nothingNow, code: 'take-empty' });
        break;
      }
      job = await h.prepare({ ...intent0, amount: left, crossing: 'limit' } as Intent, txs + 1, shown, false);
    }
    if (!('built' in job)) {
      setItem(at, { state: 'failed', busy: false });
      // Past the terms after a fill: the fills stand, and the rest is not placed.
      h.stop(txs > 0 && job.code === 'terms' ? { ...job, text: h.words.stopTerms(filled) } : job);
      break;
    }
    const isTake = !!job.plan?.take;
    if (!isTake) setItem(at, { label: h.words.rest, testid: 'order-take-step-rest' });
    setItem(at, { state: 'current', busy: false });
    const out = await h.send(job);
    if (out.status === 'submitted') {
      txs++;
      setItem(at, { state: 'done', busy: false, note: undefined });
      at++;
      if (isTake) {
        const took = job.plan!.take!.amount;
        filled += took;
        left = took < left ? left - took : 0n;
        // An estimate of fewer takes than happen: one more step.
        if (left > 0n && at >= items.length && !restsLimit) items.push({ label: word(at, at + 1), state: 'waiting', testid: `order-take-step-${at + 1}` });
        if (left > 0n && restsLimit && !items.some((x, k) => k >= at)) items.push({ label: h.words.rest, state: 'waiting', testid: 'order-take-step-rest' });
        continue;
      }
      left = 0n;
      break;
    }
    const code = out.send.code ?? '';
    if (isTake && LOST_RACE.has(code) && races < 2) {
      races++;
      // The order was gone. The book is read again and taken again. A second loss places the
      // order on the book.
      if (races > 1) {
        // An order whose rest is not placed and that has filled already stops here: nothing is
        // placed on the book after a take.
        if (!restsLimit && txs > 0) {
          setItem(at, { state: 'failed', busy: false });
          h.stop({ text: h.words.stopGone(filled), code: 'take-gone' });
          break;
        }
        fallback = true;
      }
      setItem(at, { state: 'current', note: h.words.retry });
      continue;
    }
    setItem(at, { state: 'failed', busy: false });
    break;
  }
  // Steps estimated that never came (the book filled it sooner).
  items = items.filter((x, k) => k < at || x.state !== 'waiting');
  show();
  return { done: left === 0n || (!restsLimit && txs > 0) };
}
