// A crossing order filled directly from the wallet: the user is the taker of a kob-wasm `batch`
// (the CLI's `kob take`, crates/kob-cli trade/actions.rs). No executor is involved: each resting
// order is filled at its own price, an ask is paid its price less its tip (the tip is the taker's
// to keep), a bid pays its price plus its tip, and the user pays only the network fee.
//
// `planTake` picks the crossing orders of the node-confirmed book (`PlanEnv.bookOrders`), best price
// first and never beyond the user's limit, as many as one transaction's token slots allow, and
// builds that transaction. More orders are taken by later transactions, each planned at its turn
// (ui/ticket/OrderTicket.tsx). What no transaction can take rests as a plain limit.
import type { BookOrder, BookOrderUtxo, BookView, OrderPlan, PlanEnv, PlanIssue, TakeFacts, TakeFill, TakeStep } from './plan-types';
import type { BuiltTx, KeyUtxo, TokenUtxo } from './types';
import { baseKind, describeOrder } from './order-facts';
import { atFloor, buildWithCap, recordFee } from './fee-policy';
import { guardRequest } from './request-guard';
import { KobError } from './wasm';
import { DEFAULT_FEE_RESERVE, InsufficientFunds, selectFunding, selectTokens } from './funding';
import { MAX_TOKEN_INPUTS } from './guards';
import { KRON_MAX_OUTPUT_AMOUNT } from './token-state';
import { carrierOf, issue, str, usableTokenUtxos } from './orders/common';
import { ceilDiv } from './units';

/** The lock time of a taker: the node's DAA less this (the matchers' margin, CLI `LOCK_MARGIN_DAA`). */
export const TAKE_LOCK_MARGIN_DAA = 10n;
/** At most this many take transactions are planned for one order; what is left rests. */
export const MAX_TAKE_STEPS = 6;

/** One crossing order and what this order can take of it. */
interface Taken {
  fill: TakeFill;
  utxo: BookOrderUtxo;
  /** less than the order holds: its continuation must be a minable output */
  partial: boolean;
}

/** The lock time a take is built with. */
export const takeLockTime = (env: Pick<PlanEnv, 'clock'>): bigint => (env.clock.daa > TAKE_LOCK_MARGIN_DAA ? env.clock.daa - TAKE_LOCK_MARGIN_DAA : 0n);

/** How many resting orders one take transaction fills: a buy's asks are token inputs, a sell's bids token outputs (one more is its change). */
export function takesPerTx(env: Pick<PlanEnv, 'token'>, side: 'buy' | 'sell'): number {
  const s = env.token.slots;
  return side === 'buy' ? Math.max(1, Math.min(s.inputs, MAX_TOKEN_INPUTS)) : Math.max(1, s.outputs - 1);
}

const big = (v: string | number | bigint | undefined | null): bigint => (v === undefined || v === null ? 0n : BigInt(v));

/**
 * What the user can take of one resting order at `lock`, at most `want` base units, or null when it
 * cannot be taken: not a plain order of the opposite side, the user's own, not active, past its
 * expiry, beyond the limit, or a fill its quantity rules refuse.
 */
function pickOf(env: PlanEnv, o: BookOrderUtxo, listed: bigint, side: 'buy' | 'sell', limit: bigint, want: bigint, lock: bigint): { fill: TakeFill; partial: boolean } | null {
  const st = o.order.state;
  const k = baseKind(st.kind);
  if (side === 'buy' ? k !== 'KobAsk' : k !== 'KobBid') return null;
  if (side === 'buy' && !o.custody) return null;
  let d;
  try {
    d = describeOrder(st, env.kob);
  } catch {
    return null;
  }
  if (d.maker.toLowerCase() === env.maker.toLowerCase()) return null;
  if (d.tokenCovId.toLowerCase() !== env.token.covenantId.toLowerCase() || d.scale !== env.token.scale) return null;
  if (d.activeFrom > lock || (d.expiryDaa > 0n && lock >= d.expiryDaa)) return null;
  const utxoDaa = big(o.order.blockDaaScore);
  const a = d.auction;
  // a schedule fills once per interval since its UTXO, at most maxFill a time
  if (a && a.interval > 0n && lock < utxoDaa + a.interval) return null;
  let price: bigint | null;
  try {
    price = a && a.slope > 0n ? env.kob.orderPriceAt(st, lock, utxoDaa) : d.price;
  } catch {
    price = null;
  }
  if (price === null || price <= 0n) return null;
  // the taker pays an ask its price less its tip, a bid pays the taker its price plus its tip
  const net = side === 'buy' ? price - d.tip : price + d.tip;
  if (side === 'buy' ? net > limit : net < limit) return null;
  let avail: bigint;
  try {
    // a bid's escrow buys more than its amount (its unused delivery carriers buy too): it is taken for the amount the book lists
    avail = side === 'buy' ? big(o.custody!.state.amount) : env.kob.bidBuyingPower(st, big(o.order.amount));
  } catch {
    return null;
  }
  if (side === 'sell' && listed < avail) avail = listed;
  if (avail <= 0n) return null;
  let n = want < avail ? want : avail;
  if (a && a.maxFill > 0n && n > a.maxFill) n = a.maxFill;
  // a FOK order fills whole or not at all
  if (d.tif === 'fok' && n !== avail) return null;
  try {
    if (!env.kob.fillOk(st, n, big(o.order.amount))) return null;
  } catch {
    return null;
  }
  let kas: bigint | null;
  try {
    kas = side === 'buy' ? env.kob.askProceeds(st, n, lock, utxoDaa) : env.kob.bidSpend(st, n, lock, utxoDaa);
  } catch {
    kas = null;
  }
  if (kas === null || kas <= 0n) return null;
  return { fill: { covenantId: o.covenantId, side: side === 'buy' ? 'ask' : 'bid', maker: d.maker, price, tip: d.tip, amount: n, kas }, partial: n < avail };
}

/** The rows of the side a `side` order takes, best price first, that the node confirmed. */
function rowsOf(env: PlanEnv, side: 'buy' | 'sell'): { u: BookOrderUtxo; listed: bigint }[] {
  const rows: BookOrder[] = (side === 'buy' ? env.book.askOrders : env.book.bidOrders) ?? [];
  const out: { u: BookOrderUtxo; listed: bigint }[] = [];
  const seen = new Set<string>();
  for (const r of rows) {
    const k = r.covenantId?.toLowerCase();
    if (!k || seen.has(k)) continue;
    seen.add(k);
    const u = env.bookOrders?.get(k);
    if (u) out.push({ u, listed: r.amount });
  }
  return out;
}

/** The crossing orders the book offers to `amount`, best first, grouped into transactions of `takesPerTx`. */
export function takeSteps(env: PlanEnv, side: 'buy' | 'sell', limit: bigint, amount: bigint): { steps: Taken[][]; rest: bigint } {
  const lock = takeLockTime(env);
  const per = takesPerTx(env, side);
  const picks: Taken[] = [];
  let left = amount;
  for (const { u, listed } of rowsOf(env, side)) {
    if (left <= 0n || picks.length >= per * MAX_TAKE_STEPS) break;
    const f = pickOf(env, u, listed, side, limit, left, lock);
    if (!f) continue;
    picks.push({ fill: f.fill, utxo: u, partial: f.partial });
    left -= f.fill.amount;
  }
  const steps: Taken[][] = [];
  for (let i = 0; i < picks.length; i += per) steps.push(picks.slice(i, i + per));
  return { steps, rest: left };
}

const stepOf = (p: Taken[]): TakeStep => ({ fills: p.map((x) => x.fill), amount: p.reduce((s, x) => s + x.fill.amount, 0n), kas: p.reduce((s, x) => s + x.fill.kas, 0n) });

/** The most KAS a buy pays for `amount` at `limit` (rounded up), the least a sell receives (rounded down). */
export function takeBound(side: 'buy' | 'sell', amount: bigint, limit: bigint, scale: bigint): bigint {
  return side === 'buy' ? ceilDiv(amount * limit, scale) : (amount * limit) / scale;
}

/** The `batch` request of one take transaction. */
export interface TakeRequest {
  action: 'batch';
  lockTime: string;
  legs: unknown[];
  updates: [];
  takerTokens: TokenUtxo[];
  taker: string;
  takerTokenCarrier: string;
  receivers: [];
  payments: [];
  funding: KeyUtxo[];
  change: string;
  records: [];
  fee?: { feeRate?: string; feeMode?: string };
}

const legOf = (p: Taken) => {
  const o = p.utxo.order;
  const order = { transactionId: o.transactionId, index: o.index, amount: o.amount, blockDaaScore: o.blockDaaScore ?? '0', covenantId: o.covenantId ?? p.utxo.covenantId, state: o.state.state };
  return p.fill.side === 'ask'
    ? { kind: 'ask', order, custody: p.utxo.custody, amount: str(p.fill.amount), t: null }
    : { kind: 'bid', order, amount: str(p.fill.amount), t: null };
};

const INSUFFICIENT_RE = /insufficient funds: need (\d+) sompi, have (\d+) sompi/;

/** The take transaction of `picks`, funded from the wallet, or the reason it cannot be built. */
function buildTake(env: PlanEnv, side: 'buy' | 'sell', picks: Taken[]): { built: BuiltTx; request: TakeRequest; carrier: bigint } | { issue: PlanIssue } {
  const amount = picks.reduce((s, x) => s + x.fill.amount, 0n);
  const kas = picks.reduce((s, x) => s + x.fill.kas, 0n);
  const carrier = carrierOf(env);
  let takerTokens: TokenUtxo[] = [];
  if (side === 'sell') {
    try {
      takerTokens = selectTokens(usableTokenUtxos(env), amount, Math.min(env.token.slots.inputs, MAX_TOKEN_INPUTS), env.token.program, {
        ...(env.token.family === 'kron' ? { changeCap: KRON_MAX_OUTPUT_AMOUNT } : {}), ...(env.freshTxids ? { fresh: env.freshTxids } : {}),
      });
    } catch (e) {
      if (e instanceof InsufficientFunds) {
        return { issue: e.kind === 'fragmented' ? issue('TOKEN_UTXOS_FRAGMENTED', { max: e.maxInputs ?? 0, needed: e.needed }) : issue('INSUFFICIENT_TOKENS', { needed: e.needed, have: e.have, shortfall: e.shortfall }) };
      }
      throw e;
    }
  }
  // a buy pays the makers and the carrier of its token output; a sell only the fee (its token inputs' KAS comes back)
  const need = side === 'buy' ? kas + carrier : 0n;
  const mk = (funding: KeyUtxo[]): TakeRequest => ({
    action: 'batch', lockTime: str(takeLockTime(env)), legs: picks.map(legOf), updates: [], takerTokens, taker: env.maker, takerTokenCarrier: str(carrier),
    receivers: [], payments: [], funding, change: env.maker, records: [],
    ...(env.feeRate !== undefined || env.feeMode ? { fee: { ...(env.feeRate !== undefined ? { feeRate: str(env.feeRate) } : {}), ...(env.feeMode ? { feeMode: env.feeMode } : {}) } } : {}),
  });
  const balance = env.funding.filter((u) => !u.covenantId).reduce((s, u) => s + BigInt(u.amount), 0n);
  let reserve = DEFAULT_FEE_RESERVE;
  for (let attempt = 0; attempt < 5; attempt++) {
    const room = balance > need ? balance - need : 0n;
    const r = reserve < room ? reserve : room;
    let funding: KeyUtxo[];
    try {
      funding = selectFunding(env.funding, need, r, undefined, env.freshTxids);
    } catch (e) {
      if (e instanceof InsufficientFunds) return { issue: e.kind === 'fragmented' ? issue('FUNDING_FRAGMENTED', { max: e.maxInputs ?? 0 }) : issue('INSUFFICIENT_KAS', { needed: e.needed, have: e.have, shortfall: e.shortfall }) };
      throw e;
    }
    try {
      const { built, request, rate } = buildWithCap(env.fees?.policy, mk(funding), (q) => env.kob.build(guardRequest(q, { maker: env.maker }) as never));
      recordFee(env, built, rate);
      return { built, request, carrier };
    } catch (e) {
      const msg = e instanceof KobError ? e.message : String(e);
      const m = INSUFFICIENT_RE.exec(msg);
      if (!m) return { issue: issue('BUILD_REJECTED', { reason: msg.replace(/^kob-wasm build: (invalid request: )?/, '') }) };
      const shortBy = BigInt(m[1]!) - BigInt(m[2]!);
      if (r >= room) return { issue: issue('INSUFFICIENT_KAS', { needed: balance + shortBy, have: balance, shortfall: shortBy }) };
      reserve = r + shortBy + 1_000_000n;
    }
  }
  return { issue: issue('BUILD_REJECTED', { reason: 'could not size the network fee' }) };
}

/**
 * The book without the orders `ids` (lower-case covenant ids): an order taken, or being taken, is no
 * liquidity. The levels are summed again from the orders left.
 */
export function bookWithout(book: BookView, ids: ReadonlySet<string>): BookView {
  if (!ids.size) return book;
  const side = (levels: BookView['asks'], orders: BookOrder[] | undefined): { levels: BookView['asks']; orders: BookOrder[] | undefined } => {
    if (!orders) return { levels, orders };
    const kept = orders.filter((o) => !o.covenantId || !ids.has(o.covenantId.toLowerCase()));
    if (kept.length === orders.length) return { levels, orders };
    const out: BookView['asks'] = [];
    for (const o of kept) {
      const last = out[out.length - 1];
      if (last && last.price === o.price) {
        last.amount += o.amount;
        last.orders += 1;
      } else out.push({ price: o.price, amount: o.amount, orders: 1 });
    }
    return { levels: out, orders: kept };
  };
  const a = side(book.asks, book.askOrders);
  const b = side(book.bids, book.bidOrders);
  return { ...book, asks: a.levels, bids: b.levels, ...(a.orders ? { askOrders: a.orders } : {}), ...(b.orders ? { bidOrders: b.orders } : {}) };
}

/** The result of `planTake`: the plan of the first transaction, or why nothing can be taken (the caller places the order another way). */
export type TakeOutcome = { plan: OrderPlan } | { none: PlanIssue | null };

/** How a take ends: the rest rests as a limit (`rest` plans it), or it is not placed; a FOK fills all in one transaction or nothing. */
export interface TakeMode {
  restKind: 'limit' | 'none';
  rest?: (env: PlanEnv, amount: bigint) => OrderPlan | null;
  fok?: boolean;
}

/**
 * Plans the first take transaction of a `side` order of `amount` at `limit`, with the facts of every
 * later one and of the rest. `mode.rest(env, amount)` plans what is left as a resting order against
 * the book without the orders taken. The transaction builds with fewer orders when one fills more
 * than it can carry (never a FOK, which is refused instead).
 */
export function planTake(env: PlanEnv, side: 'buy' | 'sell', amount: bigint, limit: bigint, pre: PlanIssue[], mode: TakeMode): TakeOutcome {
  const { steps, rest: left } = takeSteps(env, side, limit, amount);
  if (!steps.length) return { none: mode.fok ? issue('TAKE_FOK_UNFILLED', { amount }) : null };
  // all or nothing: the whole amount in one transaction
  if (mode.fok && (left > 0n || steps.length > 1)) return { none: issue('TAKE_FOK_UNFILLED', { amount }) };
  let first = steps[0]!;
  let built: { built: BuiltTx; request: TakeRequest; carrier: bigint } | null = null;
  let failure: PlanIssue | null = null;
  // a transaction too big for its limits builds with one order less, down to one; a partial fill it refuses (the order's rest would be dust: KIP-9) is
  // not taken at all
  const refused = new Set<Taken>();
  while (first.length > 0) {
    const r = buildTake(env, side, first);
    if ('built' in r) {
      built = r;
      break;
    }
    failure = r.issue;
    // funds or tokens short: fewer orders do not help; a FOK cannot take fewer
    if (r.issue.code !== 'BUILD_REJECTED' || mode.fok) break;
    const last = first[first.length - 1]!;
    if (last.partial) refused.add(last);
    first = first.slice(0, -1);
  }
  if (!built) return { none: failure };
  const all = [first, ...steps.slice(1)].map((st) => st.filter((x) => !refused.has(x)));
  // the orders dropped from the first transaction are taken by the next one
  if (first.length < steps[0]!.length) all.splice(1, 0, steps[0]!.slice(first.length).filter((x) => !refused.has(x)));
  const stepFacts = all.filter((s) => s.length > 0).map(stepOf);
  const totalAmount = stepFacts.reduce((s, x) => s + x.amount, 0n);
  const takenIds = new Set(all.flat().map((p) => p.fill.covenantId.toLowerCase()));
  const restAmount = amount - totalAmount;
  const restPlan = restAmount > 0n && mode.rest ? mode.rest({ ...env, book: bookWithout(env.book, takenIds) }, restAmount) : null;
  const s0 = stepFacts[0]!;
  const fee = BigInt(built.built.fee.fee);
  const facts: TakeFacts = {
    side, token: env.token.covenantId, restKind: mode.restKind, limit, scale: env.token.scale, steps: stepFacts, amount: s0.amount, kas: s0.kas, fee, bound: takeBound(side, s0.amount, limit, env.token.scale),
    total: { amount: totalAmount, kas: stepFacts.reduce((s, x) => s + x.kas, 0n) }, rest: restAmount, restPlan, tokenCarrier: side === 'buy' ? built.carrier : 0n,
  };
  const avg = s0.amount > 0n ? (s0.kas * env.token.scale) / s0.amount : null;
  const issues = [...pre, issue('TAKE_NOW', { orders: s0.fills.length }), ...(restAmount > 0n && mode.restKind === 'none' ? [issue('TAKE_REST_NOT_PLACED', { rest: restAmount })] : [])];
  return {
    plan: {
      ok: true, issues, states: [], request: null, built: built.built, take: facts,
      disclosure: {
        side, tokenAmount: s0.amount, scale: env.token.scale, minFill: 1n, minTouch: null, limitPrice: limit, allInPrice: avg, allInTotal: s0.kas,
        expectedPrice: avg, worstPrice: limit, tip: 0n,
        carriers: side === 'buy' ? [{ kind: 'tokenCarrier', amount: built.carrier, count: 1, kept: true }] : [],
        kasLocked: 0n, tokensEscrowed: 0n, fee, refundTip: 0n, keeperTip: 0n,
        expiry: { kind: 'none', daa: null, approxUnixSeconds: null, deadlineUnixSeconds: null }, activatesAt: null, notes: ['takeNow'],
      },
    },
  };
}

/** Retries the build at the fee floor when the funds cannot pay the policy's rate (as `buildCreate` does). */
export function planTakeAtFloor(env: PlanEnv, ...a: Parameters<typeof planTake> extends [PlanEnv, ...infer R] ? R : never): TakeOutcome {
  const r = planTake(env, ...a);
  if ('plan' in r || r.none?.code !== 'INSUFFICIENT_KAS') return r;
  const low = atFloor(env);
  return low ? planTake(low, ...a) : r;
}
