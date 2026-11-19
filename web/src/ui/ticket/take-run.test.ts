// The steps of an order filled from the book directly (take-run.ts), with the real planner and
// real resting orders: every step stays within the worst price and the total the ticket showed.
import { describe, expect, it } from 'vitest';
import type { Intent } from '../../kob/plan';
import { planOrder } from '../../kob/plan';
import { errors, type BookOrderUtxo, type BookView, type OrderPlan, type PlanEnv } from '../../kob/plan-types';
import { takesPerTx } from '../../kob/take';
import { CLOCK, KAS, OTHER_PK, TOK, keyUtxo, kob, makeEnv, market3x3, tokenUtxo } from '../../testing/fixtures';
import { snapshotOfRecovered } from '../../testing/chain-fixtures';
import { keepTermsPlan } from './review-check';
import { runTakes, type TakeSendOutcome, type TakeStop } from './take-run';

const K = kob();
const M = market3x3();

type Resting = { row: { covenantId: string; price: bigint; amount: bigint; tip: bigint }; utxo: BookOrderUtxo };

/** A resting sell of OTHER_PK, placed for real and read back as the node would confirm it. */
function ask(price: bigint, amount: bigint): Resting {
  const env: PlanEnv = {
    ...makeEnv({ market: M, book: { asks: [], bids: [] } }), maker: OTHER_PK,
    funding: [keyUtxo(10_000n * KAS, OTHER_PK)], tokenUtxos: [tokenUtxo(M, 1_000n * TOK, 10n * KAS, OTHER_PK)],
  };
  const p = planOrder(env, { type: 'limit', side: 'sell', amount, price, minFill: TOK });
  expect(errors(p)).toEqual([]);
  const snap = snapshotOfRecovered(K.recoverOrders(p.built!.tx)[0]!, CLOCK.daa - 1000n);
  return { row: { covenantId: snap.covenantId, price, amount, tip: 0n }, utxo: { covenantId: snap.covenantId, order: snap.order, custody: snap.custody } };
}

/** The taker's env with `asks` as the node-confirmed book, best first. */
function bookEnv(asks: Resting[]): PlanEnv {
  const book: BookView = { asks: asks.map((r) => ({ price: r.row.price, amount: r.row.amount, orders: 1 })), bids: [], askOrders: asks.map((r) => r.row), bidOrders: [] };
  return { ...makeEnv({ book, funding: [5_000n * KAS] }), bookOrders: new Map(asks.map((r) => [r.row.covenantId.toLowerCase(), r.utxo])) };
}

type Job = { built: unknown; plan?: OrderPlan };

/**
 * Runs the takes of `intent` with the ticket's rules: each step planned on `envAt(index)` (the
 * book read again right before it) and held to the terms it is given (keepTermsPlan). `answers`
 * are the node's answers to the sends in turn (default: submitted).
 */
async function run(intent: Intent, shown: OrderPlan, envAt: (index: number) => PlanEnv, answers: TakeSendOutcome[] = []) {
  const sent: OrderPlan[] = [];
  const stops: TakeStop[] = [];
  const r = await runTakes<Job>(intent, shown, {
    prepare: async (i, index, terms) => {
      const env = envAt(index);
      const p = planOrder(env, i);
      if (!p.ok || !p.built || errors(p).length > 0) return { text: 'error', code: 'plan' };
      const kept = keepTermsPlan(p, i, (j) => planOrder(env, j), terms);
      return kept ? { built: kept.p.built, plan: kept.p } : { text: 'beyond terms', code: 'terms' };
    },
    send: async (job) => {
      sent.push(job.plan!);
      return answers.shift() ?? { status: 'submitted' };
    },
    stop: (s) => stops.push(s),
    show: () => {},
    words: { step: (k, n) => `Fill ${k + 1} of ${n}`, rest: 'Rest', retry: 'Retry', nothingNow: 'Nothing', stopTerms: (f) => `Stopped after ${f}`, stopGone: (f) => `Gone after ${f}` },
  });
  return { r, sent, stops };
}

const P = 250_000_000n;
const market = (amount: bigint): Intent => ({ type: 'market', side: 'buy', amount, slippageBps: 300n, take: true });

describe('a market order with Fill now', () => {
  it('never goes past the worst price shown: a dearer ask after the first step stops the run', async () => {
    // The visible book: 2 tokens at 2.50, then an attacker's ask at 50 times that.
    const cheap = ask(P, 2n * TOK);
    const attacker = ask(50n * P, 8n * TOK);
    const shown = planOrder(bookEnv([cheap, attacker]), market(10n * TOK));
    expect(shown.take!.total.amount).toBe(2n * TOK);
    expect(shown.take!.rest).toBe(8n * TOK);
    expect(shown.take!.restKind).toBe('none');
    const { r, sent } = await run(market(10n * TOK), shown, (i) => (i === 0 ? bookEnv([cheap, attacker]) : bookEnv([attacker])));
    expect(sent.every((p) => p.take && p.take.limit <= shown.take!.limit)).toBe(true);
    expect(sent.reduce((s, p) => s + p.take!.amount, 0n)).toBe(2n * TOK);
    expect(r.done).toBe(true);
  });

  it('a later step whose book moved past the worst price stops with a report', async () => {
    // Two steps shown (more asks than one transaction carries). Before the second, the last cheap
    // ask is gone and a dear one took its place.
    const per = takesPerTx({ token: M }, 'buy');
    const cheap = Array.from({ length: per + 1 }, () => ask(P, TOK));
    const dear = ask(50n * P, 8n * TOK);
    const shown = planOrder(bookEnv(cheap), market(BigInt(per + 1) * TOK));
    expect(shown.take!.steps).toHaveLength(2);
    const { sent, stops } = await run(market(BigInt(per + 1) * TOK), shown, (i) => (i === 0 ? bookEnv(cheap) : bookEnv([dear])));
    expect(sent).toHaveLength(1);
    expect(sent.every((p) => p.take && p.take.limit <= shown.take!.limit)).toBe(true);
    expect(stops).toEqual([{ text: `Stopped after ${BigInt(per) * TOK}`, code: 'terms' }]);
  });

  it('never fills more than the total shown, even when the book refills within the price', async () => {
    const first = ask(P, 2n * TOK);
    const later = ask(P, 5n * TOK);
    const shown = planOrder(bookEnv([first]), market(10n * TOK));
    expect(shown.take!.total.amount).toBe(2n * TOK);
    const { sent } = await run(market(10n * TOK), shown, (i) => (i === 0 ? bookEnv([first]) : bookEnv([later])));
    expect(sent.reduce((s, p) => s + p.take!.amount, 0n)).toBe(2n * TOK);
  });

  it('after a fill, two lost races stop the run instead of placing a covenant order', async () => {
    const per = takesPerTx({ token: M }, 'buy');
    const cheap = Array.from({ length: per + 2 }, () => ask(P, TOK));
    const shown = planOrder(bookEnv(cheap), market(BigInt(per + 2) * TOK));
    const lost: TakeSendOutcome = { status: 'stopped', send: { code: 'orphan' } };
    const { sent, stops } = await run(market(BigInt(per + 2) * TOK), shown, (i) => (i === 0 ? bookEnv(cheap) : bookEnv(cheap.slice(per))), [{ status: 'submitted' }, lost, lost]);
    expect(sent.every((p) => !!p.take)).toBe(true);
    expect(stops).toEqual([{ text: `Gone after ${BigInt(per) * TOK}`, code: 'take-gone' }]);
  });

  it('a limit order with Fill now still rests the rest at its limit', async () => {
    const a = ask(P, 2n * TOK);
    const limit: Intent = { type: 'limit', side: 'buy', amount: 5n * TOK, price: P, crossing: 'take' };
    const shown = planOrder(bookEnv([a]), limit);
    expect(shown.take!.restKind).toBe('limit');
    const { sent, stops } = await run(limit, shown, (i) => (i === 0 ? bookEnv([a]) : bookEnv([])));
    expect(stops).toEqual([]);
    expect(sent.map((p) => (p.take ? 'take' : p.disclosure?.limitPrice))).toEqual(['take', P]);
  });
});
