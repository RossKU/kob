// Pure rules of the scenario checks (no I/O, no `@/` imports): what the bots' scenario records (run/state/scenarios.jsonl, src/scenarios.ts) and
// the x402 payer's retry records promise, checked against the indexer's views and events and the facilitator ledger.
//
//   ladder        every rung of a limit ladder is its own order, placed by its own transaction (distinct ids and txids, no parent), on the
//                 ladder's token and side, at its own price, the asks with the ladder's one size; the prices step away from the market. A rung
//                 amended after its placement (My orders' Amend of that order, in place: same id) holds the amend's price, not the ladder's.
//   ladderCancel  cancelling one rung touches no other rung (no other rung has an event in the cancel's transaction).
//   replace       a GTC renewal keeps every term of the order and only extends its expiry (expect = before but expiryDaa, which grows; a
//                 minimum fill above what partial fills left comes down to what is left, the same fills); a pair amend keeps the pair, side, size, tip and minimum fill and takes the new price (one that crosses the market: the crossing
//                 auction's slope and end price); on chain the replacement holds exactly the planned terms (fills, arming and custody aside).
//   cancel-all    every order of a cancel-all ends: cancelled, or spent by a fill that won the race; one still live at the very outpoint
//                 the cancel spent after the grace period is reported (warn: the cancel neither landed nor lost a race).
//   merge         a token merge ends in ONE output of the wallet holding exactly the inputs' total.
//   x402 retry    a payment rebuilt after an order conflict has at most one accepted attempt (every superseded attempt is not accepted);
//                 a payment whose first answer was lost and was resent is served from the merchant's memory (`replayed`), not as a new payment.
import { INV, type EventLike, type OrderLike, type Violation } from './types.ts';
import { replayLedger, type Payment } from './rules.ts';
import type { CancelAllRecord, LadderCancelRecord, LadderRecord, MergeRecord, ReplaceRecord } from '../scenarios.ts';

/** how long a record may wait for the indexer before it is given up as unverifiable (or, for a cancel, reported) */
export const SCENARIO_GRACE_MS = 30 * 60_000;

/** `done: false` = look again next round; `done: true` with `ok` and the violations found (and a note for the counters) */
export type Verdict = { done: false } | { done: true; ok: boolean; violations: Violation[]; note?: string };

const pending: Verdict = { done: false };
const okv = (note?: string): Verdict => ({ done: true, ok: true, violations: [], ...(note ? { note } : {}) });
const gone = (note: string): Verdict => ({ done: true, ok: false, violations: [], note });
const bad = (invariant: string, subject: string, detail: Record<string, unknown>, severity: 'error' | 'warn' = 'error'): Verdict => ({
  done: true,
  ok: false,
  violations: [{ invariant, severity, subject, detail }],
});

const isLive = (status: string): boolean => status === 'open' || status === 'partial';
const outpoint = (v: OrderLike): string | null => (v.current ? `${v.current.txid}:${v.current.index}` : null);

/** state fields fills, arming and custody moves change after placement: never compared with the planned replacement */
export const MUTABLE_FIELDS: ReadonlySet<string> = new Set(['amountLeft', 'custody', 'armed']);

/** what a pair amend keeps of the replaced order (PairState fields); its price is the new one */
export const PAIR_AMEND_KEEPS = ['maker', 'side', 'sCovId', 'tCovId', 'sScale', 'tScale', 'tip', 'tif', 'minFill', 'amountLeft', 'slope', 'interval', 'maxFill'] as const;

/**
 * A plain resting pair limit (no slope) whose new price crosses the market by the time the amend is planned is planned as a crossing auction (the
 * pair ticket's default `crossing: 'auction'`, as the app's Replace): from the touch to the new limit, so `slope` and `priceEnd` (and `activeFrom`)
 * are set. That is the replacement's own shape at its new price, not a term the amend lost; a schedule (`interval`, `maxFill`) still must not change.
 */
export function becameCrossingAuction(before: Record<string, string>, expect: Record<string, string>): boolean {
  const zero = (v: string | undefined) => v === undefined || String(v) === '0';
  return zero(before.slope) && !zero(expect.slope) && !zero(expect.priceEnd);
}

/**
 * A renewal of an order with `amountLeft` (an ask, a conditional order) whose minimum fill is above what partial fills left plans the minimum
 * fill down to what is left (web amend.ts `buildReplacement`, kob-cli `replacement`: the wallet places no order with minFill > amount). The
 * order's fills stay the same: n >= minFill unless n = amountLeft (order-types.md, matcher.md 2.2), so with less than minFill left both admit
 * only the whole rest. A plain bid has no `amountLeft` (its quantity is its escrow) and keeps its minimum fill.
 */
export function minFillFitted(before: Record<string, string>, expect: Record<string, string>): boolean {
  if (before.amountLeft === undefined || before.minFill === undefined || expect.minFill === undefined) return false;
  const left = BigInt(before.amountLeft);
  return BigInt(before.minFill) > left && BigInt(expect.minFill) === left;
}

/** fields of `a` and `b` (both present) that differ, `skip` left out */
export function diffTerms(a: Record<string, string>, b: Record<string, string>, skip: ReadonlySet<string> = new Set()): string[] {
  const out: string[] = [];
  for (const k of Object.keys(a)) {
    if (skip.has(k) || b[k] === undefined) continue;
    if (String(a[k]) !== String(b[k])) out.push(k);
  }
  return out;
}

/**
 * The planned replacement against the order it replaced (no chain data): a renewal must keep every term and extend the expiry; a pair amend keeps
 * `PAIR_AMEND_KEEPS` (the size as `amountLeft` at the time of the amend). Null when the plan honours that.
 */
export function checkPlannedReplacement(rec: ReplaceRecord): Violation | null {
  const before = rec.before;
  if (rec.kind === 'renew') {
    const diff = diffTerms(before, rec.expect, new Set(['expiryDaa'])).filter((k) => !(k === 'minFill' && minFillFitted(before, rec.expect)));
    const oldExp = BigInt(before.expiryDaa ?? rec.oldExpiryDaa);
    const newExp = BigInt(rec.expect.expiryDaa ?? '0');
    if (diff.length || newExp <= oldExp) {
      return { invariant: INV.replace, severity: 'error', subject: `${rec.old.id}:renew-plan`, detail: { order: rec.old.id, txid: rec.txid, changed: diff, oldExpiryDaa: oldExp.toString(), newExpiryDaa: newExp.toString(), problem: diff.length ? 'the renewal changes terms other than the expiry' : 'the renewal does not extend the expiry' } };
    }
    return null;
  }
  const auction = becameCrossingAuction(before, rec.expect);
  const changed = PAIR_AMEND_KEEPS.filter((k) => before[k] !== undefined && rec.expect[k] !== undefined && String(before[k]) !== String(rec.expect[k]))
    .filter((k) => !(k === 'slope' && auction));
  if (changed.length) {
    return { invariant: INV.replace, severity: 'error', subject: `${rec.old.id}:amend-plan`, detail: { order: rec.old.id, txid: rec.txid, changed, problem: 'the pair amend changes the pair, side, size, tip or minimum fill' } };
  }
  return null;
}

/**
 * The replacement on chain: the order `rec.id` (the same id for an in-place amend) carries the planned terms. Its events tell whether the
 * replacement landed (an `amend` event in `rec.txid`, or a new order created by it) and whether a later amend replaced it again (`superseded`:
 * nothing left to compare). Terms come from the proven state; a closed order (no state) is compared on its view's price, tip and expiry.
 */
export function checkReplacement(rec: ReplaceRecord, view: OrderLike | null, events: EventLike[] | null, ageMs: number): Verdict {
  if (!view) return ageMs < SCENARIO_GRACE_MS ? pending : gone('not listed');
  const landedIn = rec.inPlace ? (events ?? []).find((e) => e.txid === rec.txid && e.kind === 'amend') : view.genesis?.txid === rec.txid ? ({ id: -1 } as EventLike) : undefined;
  if (!landedIn) {
    if (ageMs < SCENARIO_GRACE_MS) return pending;
    // the replacement never landed: the order was filled / amended by another transaction first (a lost race), or the transaction was dropped
    return gone('not landed');
  }
  const later = (events ?? []).some((e) => e.kind === 'amend' && e.txid !== rec.txid && e.id > landedIn.id);
  if (later) return okv('superseded');
  const state = view.state?.state ?? null;
  const subject = `${rec.id}:${rec.kind}`;
  if (state) {
    const diff = diffTerms(rec.expect, state, MUTABLE_FIELDS);
    if (diff.length) {
      return bad(INV.replace, subject, { order: rec.id, replaced: rec.old.id, txid: rec.txid, kind: rec.kind, fields: Object.fromEntries(diff.map((k) => [k, { planned: rec.expect[k], chain: state[k] }])), problem: 'the replacement on chain differs from the planned one' });
    }
    return okv();
  }
  const fields: [string, unknown][] = [['price', view.price], ['tip', view.tip], ['expiryDaa', view.expiry_daa]];
  const diff = fields.filter(([k, v]) => v !== null && v !== undefined && rec.expect[k] !== undefined && String(v) !== rec.expect[k]).map(([k]) => k);
  if (diff.length) return bad(INV.replace, subject, { order: rec.id, txid: rec.txid, kind: rec.kind, fields: diff, problem: 'the closed replacement differs from the planned one' });
  return okv('closed');
}

/**
 * The price a listed rung holds: a rung that was marketable when planned is placed as a taker at its own price; an auctioning one is bounded by
 * its end price. Its proven state, else (closed) the view's price; both follow in-place amends.
 */
function rungPrice(v: OrderLike): string | null {
  const st = v.state?.state;
  const price = st ? (st.slope && st.slope !== '0' ? st.priceEnd : st.price) : (v.price ?? null);
  return price === null || price === undefined ? null : String(price);
}

/** ids of the listed rungs whose price is not the planned one (their events tell whether a later amend moved them: `amendedAfter`) */
export function repricedRungs(rec: LadderRecord, views: (OrderLike | null)[]): string[] {
  return rec.rungs
    .filter((r, i) => {
      const v = views[i];
      const p = v ? rungPrice(v) : null;
      return p !== null && p !== r.price;
    })
    .map((r) => r.id);
}

/** an order's events show an amend after its placement in `placedIn` (an in-place amend keeps the id and moves the terms) */
export const amendedAfter = (events: EventLike[], placedIn: string): boolean => events.some((e) => e.kind === 'amend' && e.txid !== placedIn);

/**
 * The ladder's rungs as listed (`views[i]` for `rec.rungs[i]`, null while not listed). `amended`: ids of the rungs amended after their placement
 * (`amendedAfter`, a trader's later Amend of that order): their price is the amend's, it is not compared with the ladder's.
 */
export function checkLadder(rec: LadderRecord, views: (OrderLike | null)[], ageMs: number, amended: ReadonlySet<string> = new Set()): Verdict {
  const subject = `${rec.rungs[0]?.id ?? '?'}:ladder`;
  const ids = new Set(rec.rungs.map((r) => r.id));
  const txids = new Set(rec.rungs.map((r) => r.txid));
  if (ids.size !== rec.rungs.length || txids.size !== rec.rungs.length) {
    return bad(INV.ladder, subject, { rungs: rec.rungs, problem: 'two rungs of a ladder share an order or a transaction' });
  }
  for (let i = 1; i < rec.rungs.length; i++) {
    const a = BigInt(rec.rungs[i - 1]!.price);
    const b = BigInt(rec.rungs[i]!.price);
    if (rec.side === 'sell' ? b <= a : b >= a) {
      return bad(INV.ladder, subject, { level: i + 1, prices: rec.rungs.map((r) => r.price), side: rec.side, problem: 'the ladder prices do not step away from the market' });
    }
  }
  if (views.some((v) => v === null)) {
    if (ageMs < SCENARIO_GRACE_MS) return pending;
    if (views.every((v) => v === null)) return gone('not listed');
  }
  const problems: Record<string, unknown>[] = [];
  views.forEach((v, i) => {
    if (!v) return;
    const r = rec.rungs[i]!;
    const p: string[] = [];
    if (v.genesis?.txid && v.genesis.txid !== r.txid) p.push('created by another transaction');
    if (v.parent) p.push('has a parent');
    if (v.token && v.token !== rec.token) p.push('another token');
    if (v.side !== (rec.side === 'sell' ? 1 : 2)) p.push('another side');
    const price = rungPrice(v);
    if (price !== null && price !== r.price && !amended.has(r.id)) p.push(`price ${price} != ${r.price}`);
    if (rec.side === 'sell' && v.initial_amount != null && v.initial_amount !== rec.size) p.push(`size ${v.initial_amount} != ${rec.size}`);
    if (p.length) problems.push({ level: i + 1, id: r.id, problems: p });
  });
  if (problems.length) return bad(INV.ladder, subject, { token: rec.token, side: rec.side, size: rec.size, rungs: problems, problem: 'a ladder rung is not the independent order the ladder planned' });
  return okv(views.some((v) => v === null) ? 'partly listed' : rec.rungs.some((r) => amended.has(r.id)) ? 'amended' : undefined);
}

/**
 * One rung cancelled: once the cancel's transaction is indexed (an event of the cancelled rung in it), it must have touched no other rung (`events`
 * of every rung by id, the cancelled one included; null when unread). A cancel that lost the race to a fill is fine; one that neither landed nor
 * lost (the rung still live at the outpoint it spent after the grace period) is reported.
 */
export function checkLadderCancel(rec: LadderCancelRecord, cancelled: OrderLike | null, events: Map<string, EventLike[] | null>, ageMs: number): Verdict {
  const subject = `${rec.cancelled.id}:ladder-cancel`;
  const landed = (events.get(rec.cancelled.id) ?? []).some((e) => e.txid === rec.txid);
  if (!landed) {
    if (cancelled && isLive(cancelled.status) && outpoint(cancelled) === rec.cancelled.spends) {
      if (ageMs < SCENARIO_GRACE_MS) return pending;
      return bad(INV.cancelAll, subject, { order: rec.cancelled.id, txid: rec.txid, spends: rec.cancelled.spends, problem: 'the cancelled rung is still live at the outpoint its cancel spent' }, 'warn');
    }
    if (!cancelled || events.get(rec.cancelled.id) === null) return ageMs < SCENARIO_GRACE_MS ? pending : gone('not listed');
    return okv('raced');
  }
  if ([...events.values()].some((e) => e === null)) return ageMs < SCENARIO_GRACE_MS ? pending : gone('events unread');
  const touched = [...events].filter(([id, evs]) => id !== rec.cancelled.id && (evs ?? []).some((e) => e.txid === rec.txid)).map(([id]) => id);
  if (touched.length) return bad(INV.ladder, subject, { cancelled: rec.cancelled.id, txid: rec.txid, touched, problem: 'cancelling one ladder rung changed another rung' });
  return okv();
}

/** Every order of a cancel-all ends (`views` by order id, null while not listed). */
export function checkCancelAll(rec: CancelAllRecord, views: Map<string, OrderLike | null>, ageMs: number): Verdict {
  const stuck = rec.orders.filter((o) => {
    const v = views.get(o.id);
    return v && isLive(v.status) && outpoint(v) === o.spends;
  });
  if (!stuck.length) {
    const raced = rec.orders.filter((o) => {
      const v = views.get(o.id);
      return v && isLive(v.status);
    }).length;
    return okv(raced ? 'raced' : undefined);
  }
  if (ageMs < SCENARIO_GRACE_MS) return pending;
  return bad(INV.cancelAll, `${rec.txids[0] ?? '?'}:cancel-all`, { scope: rec.scope, txids: rec.txids, live: stuck.map((o) => o.id), problem: 'orders of a cancel-all are still live at the outpoints the cancel spent' }, 'warn');
}

/** The merge's record on its own: two or more distinct inputs, a total that is their sum, one to ten links. */
export function checkMergeRecord(rec: MergeRecord): Violation | null {
  const keys = new Set(rec.inputs.map((i) => `${i.txid}:${i.index}`));
  const sum = rec.inputs.reduce((s, i) => s + BigInt(i.amount), 0n);
  const p: string[] = [];
  if (rec.inputs.length < 2) p.push('fewer than two inputs');
  if (keys.size !== rec.inputs.length) p.push('an input merged twice');
  if (sum.toString() !== rec.total) p.push(`total ${rec.total} != sum of the inputs ${sum}`);
  if (rec.links.length < 1 || rec.links.length > 10) p.push(`${rec.links.length} links`);
  if (rec.final.txid !== rec.links[rec.links.length - 1]) p.push('the final output is not of the last link');
  return p.length ? { invariant: INV.merge, severity: 'error', subject: `${rec.final.txid}:merge-plan`, detail: { token: rec.ticker, bot: rec.bot, problems: p, problem: 'the merge plan does not merge its inputs into one output' } } : null;
}

/** The merged output as the indexer lists it (null while not found). */
export function checkMerge(rec: MergeRecord, out: { amount: string; owner: string } | null, ageMs: number): Verdict {
  if (!out) return ageMs < SCENARIO_GRACE_MS ? pending : gone('output not found');
  if (out.amount !== rec.total || out.owner.toLowerCase() !== rec.maker.toLowerCase()) {
    return bad(INV.merge, `${rec.final.txid}:${rec.final.index}`, { token: rec.ticker, bot: rec.bot, expected: rec.total, amount: out.amount, owner: out.owner, maker: rec.maker, links: rec.links, problem: 'the merged output does not hold the inputs\' total for the wallet' });
  }
  return okv();
}

/** An x402 payment of a retry scenario (`run/state/x402-payments.jsonl`, written by the payer). */
export interface RetryPayment extends Payment {
  /** `lost`: the merchant lost the first paid answer after settling; `drop`: it dropped the first paid request; `conflict`: the quoted bid was amended away */
  scenario?: string;
  attempts?: number;
  resends?: number;
  /** transaction ids of the superseded attempts */
  superseded?: string[];
  /** the merchant's `replayed` flag on the answer the payer got */
  replayed?: boolean;
}

export interface X402RetryResult {
  violations: Violation[];
  stats: { retryPayments: number; resent: number; rebuilt: number; notExercised: number };
}

export function checkX402Retries(payments: readonly RetryPayment[], ledgerText: string): X402RetryResult {
  const { last } = replayLedger(ledgerText);
  const v: Violation[] = [];
  const stats = { retryPayments: 0, resent: 0, rebuilt: 0, notExercised: 0 };
  for (const p of payments) {
    if (!p.scenario) continue;
    stats.retryPayments++;
    const resent = (p.resends ?? 0) > 0;
    const rebuilt = (p.attempts ?? 1) > 1;
    if (resent) stats.resent++;
    if (rebuilt) stats.rebuilt++;
    if (!resent && !rebuilt) stats.notExercised++;
    for (const t of p.superseded ?? []) {
      if (t === p.txid) {
        v.push({ invariant: INV.x402, severity: 'error', subject: `${p.txid}:superseded`, detail: { txid: p.txid, scenario: p.scenario, problem: 'the paid attempt is listed among its own superseded attempts' } });
        continue;
      }
      if (last.get(t)?.state === 'accepted') {
        v.push({ invariant: INV.x402, severity: 'error', subject: `${t}:twice`, detail: { paid: p.txid, superseded: t, scenario: p.scenario, problem: 'two attempts of one payment were both accepted' } });
      }
    }
    if (p.scenario === 'lost' && resent && p.replayed === false) {
      v.push({ invariant: INV.x402, severity: 'error', subject: `${p.txid}:replay`, detail: { txid: p.txid, resends: p.resends, problem: 'a resent payment whose first answer was lost was served as a new payment (not replayed)' } });
    }
  }
  return { violations: v, stats };
}
