// Repeat ladder (several levels): one repeat IFD / IFO intent expands to N intents whose entry and exit prices are shifted by k * step.
//
// The protocol has no "ladder" order: every level is its own repeat order (its own covenant, its own transaction). This module is the pure
// part: it validates the ladder and produces the intents; the ticket plans and signs them one after the other (nothing is batched).
//   * buy-first levels go DOWN from the entry price, sell-first levels go UP (the usual grid: each level waits one step further from the market);
//   * every price of a level (entry, entry stop, exit take-profit, exit stop, exit stop-limit) shifts by the same amount, so the exit keeps its
//     distance from the entry and the profit per whole token is identical on every level;
//   * the step is on the tick (the form rounds it), so a shifted price stays on the tick whenever the original one is.
import type { Intent } from '../../kob/plan';
import type { LimitIntent } from '../../kob/intent-simple';
import type { ExitSpec, RepeatIfdIntent, RepeatIfoIntent } from '../../kob/intent-cond';
import type { OrderPlan } from '../../kob/plan-types';
import { quoteOf, roundToTick, safeRounding } from '../../kob/units';
import { MAX_LADDER_LEVELS, MAX_LIMIT_LADDER_LEVELS, MIN_LIMIT_LADDER_LEVELS, parseBps, parsePrice, parseWhole, type TicketCtx, type TicketForm } from './form-state';

export type RepeatIntent = RepeatIfdIntent | RepeatIfoIntent;

export type LadderErrorCode = 'notRepeat' | 'levels' | 'step' | 'tick' | 'price' | 'same' | 'collapse';
export interface LadderError {
  code: LadderErrorCode;
  /** 1-based level the problem is at (absent for the whole ladder) */
  level?: number;
  field?: string;
}

export interface LadderLevel {
  /** 1-based; level 1 is the typed order itself */
  level: number;
  /** the price shift of this level versus level 1, sompi per whole token (negative buy-first) */
  shift: bigint;
  /** the order of this level (a repeat IFD / IFO of a repeat ladder, a plain limit of a limit ladder) */
  intent: Intent;
  /** entry limit price, sompi per whole token */
  entryPrice: bigint;
  exitTakeProfit: bigint | null;
  exitStop: bigint | null;
}

export interface LadderResult {
  ok: boolean;
  levels: LadderLevel[];
  errors: LadderError[];
  /** base units over all levels (each level: the `amount` of one cycle) */
  totalAmount: bigint;
}

export interface LadderSetting {
  levels: number;
  /** price distance between two levels, sompi per whole token (> 0) */
  step: bigint;
}

/**
 * The ladder the form asks for: null when it is off (levels empty / 1), not a repeat type, or its numbers do not parse (the form reports
 * those through `buildIntent`).
 */
export function ladderOfForm(form: TicketForm, ctx: TicketCtx): LadderSetting | null {
  if (form.type !== 'repeatIfd' && form.type !== 'repeatIfo') return null;
  const lv = parseWhole(form.values['ladder.levels'] ?? '');
  if (!lv.ok || lv.value <= 1n || lv.value > MAX_LADDER_LEVELS) return null;
  const st = parsePrice(form.values['ladder.step'] ?? '', form.side, ctx, 'nearest');
  if (!st.ok) return null;
  return { levels: Number(lv.value), step: st.value.price };
}

const shiftPrice = (p: bigint | undefined, by: bigint): bigint | undefined => (p === undefined ? undefined : p + by);

/** Expands `intent` into `levels` repeat intents (level 1 = `intent`). Pure; every violation is reported, nothing throws. */
export function expandLadder(intent: Intent, levels: number, step: bigint, tick: bigint): LadderResult {
  const errors: LadderError[] = [];
  if (!isRepeatIntent(intent)) return { ok: false, levels: [], errors: [{ code: 'notRepeat' }], totalAmount: 0n };
  if (!Number.isInteger(levels) || levels < 1 || BigInt(levels) > MAX_LADDER_LEVELS) errors.push({ code: 'levels', field: 'ladder.levels' });
  if (levels > 1 && step <= 0n) errors.push({ code: 'step', field: 'ladder.step' });
  if (tick > 0n && step % tick !== 0n) errors.push({ code: 'tick', field: 'ladder.step' });
  if (errors.length > 0) return { ok: false, levels: [], errors, totalAmount: 0n };

  const dir = intent.side === 'buy' ? -1n : 1n;
  const out: LadderLevel[] = [];
  for (let k = 0; k < levels; k++) {
    const shift = dir * BigInt(k) * step;
    const exit: ExitSpec = { ...intent.exit };
    const tp = shiftPrice(intent.exit.takeProfit, shift);
    const stop = shiftPrice(intent.exit.stop, shift);
    const limit = shiftPrice(intent.exit.stopLimit, shift);
    if (tp !== undefined) exit.takeProfit = tp;
    if (stop !== undefined) exit.stop = stop;
    if (limit !== undefined) exit.stopLimit = limit;
    const entry = { ...intent.entry, price: intent.entry.price + shift };
    const es = shiftPrice(intent.entry.stop, shift);
    if (es !== undefined) entry.stop = es;
    const prices: [string, bigint | undefined][] = [
      ['price', entry.price], ['entry.stop', es], ['exit.takeProfit', tp], ['exit.stop', stop], ['exit.stopLimit', limit],
    ];
    for (const [field, p] of prices) {
      if (p === undefined) continue;
      if (p <= 0n) errors.push({ code: 'price', level: k + 1, field });
      else if (tick > 0n && p % tick !== 0n) errors.push({ code: 'tick', level: k + 1, field });
    }
    out.push({
      level: k + 1,
      shift,
      intent: { ...intent, entry, exit } as RepeatIntent,
      entryPrice: entry.price,
      exitTakeProfit: tp ?? null,
      exitStop: stop ?? null,
    });
  }
  return { ok: errors.length === 0, levels: out, errors, totalAmount: BigInt(levels) * intent.amount };
}

export const isRepeatIntent = (i: Intent): i is RepeatIntent => (i as { type?: string }).type === 'repeatIfd' || (i as { type?: string }).type === 'repeatIfo';

export interface LadderTotals {
  /** base units over all levels */
  amount: bigint;
  /** KAS leaving the spendable balance into covenants over all levels, excluding network fees (sompi) */
  kasLocked: bigint;
  /** tokens moved into custody over all levels, base units */
  tokens: bigint;
}

/**
 * Budget of the whole ladder from the plan of level 1: carriers, reserves and prefund do not depend on the shift (an exit moves with its
 * entry, so the sell-first prefund is the same), only the escrow of a buy-first entry does (the amount at the price of each level: its shift is
 * negative, `amount * shift / scale` rounded toward zero). An estimate: every level is planned again, exactly, right before it is signed.
 */
export function ladderTotals(level1: OrderPlan, ladder: LadderResult): LadderTotals | null {
  const d = level1.disclosure;
  if (!d || !ladder.ok || ladder.levels.length === 0) return null;
  const first = ladder.levels[0]!;
  const amount = (first.intent as { amount: bigint }).amount;
  let kas = 0n;
  for (const l of ladder.levels) {
    const delta = d.side === 'buy' ? quoteOf(amount, l.shift < 0n ? -l.shift : l.shift, d.scale, 'down') : 0n;
    kas += d.kasLocked + (l.shift < 0n ? -delta : delta);
  }
  return { amount: ladder.totalAmount, kasLocked: kas, tokens: d.tokensEscrowed * BigInt(ladder.levels.length) };
}

// ------------------------------------------------------------------------------------------------ limit ladder
//
// Several plain limit orders of the same size at stepped prices, same side. Nothing is a new order kind: every level is a normal, independent limit
// order with its own transaction and signature (the builder creates one order per transaction), so a cancelled or filled level never touches another.
//   * `pct`: each level is `bps` basis points from the previous one, buy ladders go DOWN, sell ladders go UP (compounded, on the tick);
//   * `end`: the levels are evenly spaced from the start price to `end` (either direction).

export type LimitLadderSetting = { levels: number } & ({ mode: 'pct'; bps: bigint } | { mode: 'end'; end: bigint });

/** The limit ladder the form asks for; null when it is off, not a limit, or its numbers do not parse (`buildIntent` reports those). */
export function limitLadderOfForm(form: TicketForm, ctx: TicketCtx): LimitLadderSetting | null {
  if (form.type !== 'limit' || form.values['ladder.on'] !== 'true') return null;
  const lv = parseWhole(form.values['ladder.levels'] ?? '');
  if (!lv.ok || lv.value < MIN_LIMIT_LADDER_LEVELS || lv.value > MAX_LIMIT_LADDER_LEVELS) return null;
  const levels = Number(lv.value);
  const endText = (form.values['ladder.end'] ?? '').trim();
  if (endText !== '') {
    const end = parsePrice(endText, form.side, ctx);
    return end.ok ? { levels, mode: 'end', end: end.value.price } : null;
  }
  const bps = parseBps(form.values['ladder.pct'] ?? '');
  return bps !== null ? { levels, mode: 'pct', bps } : null;
}

/**
 * The prices of a limit ladder, level 1 = `start`. Rounded on the tick the way that never makes a limit worse (buy down, sell up). Returns an error
 * instead when the ladder cannot be built: a price at or below zero, levels that collapse onto the same tick, an end equal to the start.
 */
export function limitLadderPrices(start: bigint, side: 'buy' | 'sell', set: LimitLadderSetting, tick: bigint): { prices: bigint[] } | { error: LadderError } {
  const rounding = safeRounding(side);
  const snap = (p: bigint): bigint => (tick > 1n ? roundToTick(p, tick, rounding) : p);
  const n = BigInt(set.levels);
  const prices: bigint[] = [start];
  if (set.mode === 'end') {
    if (set.end === start) return { error: { code: 'same', field: 'ladder.end' } };
    for (let k = 1n; k < n; k++) prices.push(k === n - 1n ? snap(set.end) : snap(start + ((set.end - start) * k) / (n - 1n)));
  } else {
    const factor = side === 'buy' ? 10_000n - set.bps : 10_000n + set.bps;
    let cur = start;
    for (let k = 1n; k < n; k++) {
      cur = snap((cur * factor) / 10_000n);
      prices.push(cur);
    }
  }
  for (let k = 0; k < prices.length; k++) {
    if (prices[k]! <= 0n) return { error: { code: 'price', level: k + 1, field: set.mode === 'end' ? 'ladder.end' : 'ladder.pct' } };
    if (k > 0 && prices[k] === prices[k - 1]) return { error: { code: 'collapse', level: k + 1, field: set.mode === 'end' ? 'ladder.end' : 'ladder.pct' } };
  }
  return { prices };
}

/** Expands a limit intent into one limit intent per price of the ladder (level 1 = the typed order). Pure; every violation is reported, nothing throws. */
export function expandLimitLadder(intent: Intent, set: LimitLadderSetting, tick: bigint): LadderResult {
  if (intent.type !== 'limit') return { ok: false, levels: [], errors: [{ code: 'notRepeat' }], totalAmount: 0n };
  if (!Number.isInteger(set.levels) || BigInt(set.levels) < MIN_LIMIT_LADDER_LEVELS || BigInt(set.levels) > MAX_LIMIT_LADDER_LEVELS) {
    return { ok: false, levels: [], errors: [{ code: 'levels', field: 'ladder.levels' }], totalAmount: 0n };
  }
  const r = limitLadderPrices(intent.price, intent.side, set, tick);
  if ('error' in r) return { ok: false, levels: [], errors: [r.error], totalAmount: 0n };
  const levels: LadderLevel[] = r.prices.map((price, k) => ({
    level: k + 1,
    shift: price - intent.price,
    intent: { ...(intent as LimitIntent), price } as Intent,
    entryPrice: price,
    exitTakeProfit: null,
    exitStop: null,
  }));
  return { ok: true, levels, errors: [], totalAmount: BigInt(set.levels) * intent.amount };
}

/** Quote value of every level at its own price (`amount x price / scale`; a buy rounds down, a sell up, like the covenant) and their sum. */
export function limitLadderValues(ladder: LadderResult, side: 'buy' | 'sell', scale: bigint): { values: bigint[]; total: bigint } {
  const values = ladder.levels.map((l) => quoteOf((l.intent as { amount: bigint }).amount, l.entryPrice, scale, side === 'sell' ? 'up' : 'down'));
  return { values, total: values.reduce((a, b) => a + b, 0n) };
}
