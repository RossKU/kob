// Shared bot actions over the web app's planners: place any intent, cancel, amend (cancel-replace), cancel-all.
import { planOrder, type Intent } from '@/kob/plan';
import type { PairOrderPlan, TokenMarket } from '@/kob/plan-types';
import { errors, type BookView } from '@/kob/plan-types';
import { planCancel, planCancelAll, planCancelReplace, snapshotFromOrderView, type OrderSnapshot, type ReplacementSpec } from '@/kob/cancel';
import { buildReplacement } from '@/ui/orders/amend';
import { orderTypeKey } from '@/ui/orders/orders-model';
import { intentFromOrder } from '@/ui/orders/replace-intent';
import { replaceEnv, specOfPlan } from '@/ui/orders/replace-env';
import { buildMergeLink, mergeCarry, planMerge, type MergeToken } from '@/kob/merge';
import { describeOrder } from '@/kob/order-facts';
import { daaToUnix } from '@/kob/daa';
import { waitForAcceptance } from '@/wallet/sign';
import { spkStringToAddress } from '@/data/kaspa-sdk';
import type { TokenUtxo } from '@/kob/types';
import type { OrderView } from '@/data/indexer-types';
import type { Env } from '../env';
import { errText } from '../log';
import { cancelEnv, pairPlanEnv, planEnv, tokenMarket, tokenRef, withStartReferences, type Which } from '../market';
import type { BotWallet, SubmitOutcome } from '../wallet';
import { selfTradeIds } from './pending';
import { takerIntent } from './taker';

export interface PlaceResult extends SubmitOutcome {
  /** covenant ids of the orders the transaction creates */
  created?: string[];
  planIssue?: string;
  /** the parameters of that refusal (SELF_TRADE: `ownCovenantId`, the own resting order the plan would trade against) */
  planParams?: Record<string, unknown>;
  /** a SELF_TRADE refusal: every own resting order the plan would trade against */
  selfTrade?: string[];
}

/** the result of a refused plan (counted per code; a SELF_TRADE names every own order it would trade against) */
function refusal(w: BotWallet, plan: Parameters<typeof errors>[0], tag: string, what: string): PlaceResult {
  const errs = errors(plan);
  const code = errs[0]?.code ?? 'unknown';
  w.stats.inc(`plan_refused:${tag}:${code}`);
  w.log.debug(what, { tag, code, message: errs[0]?.message, params: errs[0]?.params });
  const own = selfTradeIds(errs as { code: string; params?: Record<string, unknown> }[]);
  return { ok: false, planIssue: code, planParams: errs[0]?.params as Record<string, unknown> | undefined, error: errs[0]?.message, ...(own.length ? { selfTrade: own } : {}) };
}

/**
 * Plans `intent` against the live book / wallet and submits it; plan refusals are counted per code (never thrown). `o.taker`: a limit that is
 * marketable against the planner's own book is placed as an IOC at the same price instead of a resting / auctioning limit (taker.ts).
 */
export function place(env: Env, w: BotWallet, intent: Intent, tag: string, o: { book?: BookView; mine?: OrderView[]; taker?: boolean } = {}, which?: Which): Promise<PlaceResult> {
  return w.exclusive(async (): Promise<PlaceResult> => {
    w.stats.inc(`try:${tag}`);
    let plan;
    try {
      const penv = await planEnv(env, w, o, which);
      if (o.taker) {
        const t = takerIntent(intent, penv.book);
        if (t.taker) w.stats.inc(`taker:${tag}`);
        intent = t.intent;
      }
      plan = planOrder(await withStartReferences(env, penv, intent.type), intent);
    } catch (e) {
      w.stats.inc(`plan_throw:${tag}`);
      w.log.warn('planner threw', { tag, error: errText(e) });
      return { ok: false, error: errText(e) };
    }
    if (!plan.ok || !plan.built) return refusal(w, plan, tag, 'plan refused');
    const created = plan.built.covenants.map((c) => c.covenantId);
    const r = await w.submit(plan.built, tag, { amount: (intent as { amount?: bigint }).amount, orders: created.length });
    if (r.ok) w.stats.inc(`placed:${tag}`);
    return { ...r, created };
  });
}

/** the soak market of an order view (a pair order's token is its base token A) */
export function marketOfView(env: Env, view: OrderView) {
  return tokenMarket(env, view.token ?? undefined);
}

/**
 * Plans a PAIR order through the web planner (`planOrder` with a PairPlanEnv: every order type of the KAS ticket on the pair `base`/`quote`,
 * prices B base units per whole A) and submits it. Refusals are counted like `place`.
 */
export function placePair(env: Env, w: BotWallet, intent: Intent, tag: string, base: TokenMarket, quote: TokenMarket, o: { taker?: boolean } = {}): Promise<PlaceResult & { pair?: PairOrderPlan['pair'] }> {
  return w.exclusive(async (): Promise<PlaceResult & { pair?: PairOrderPlan['pair'] }> => {
    w.stats.inc(`try:${tag}`);
    let plan: PairOrderPlan;
    try {
      const penv = await pairPlanEnv(env, w, base, quote);
      if (o.taker) {
        const t = takerIntent(intent, penv.book, true);
        if (t.taker) w.stats.inc(`taker:${tag}`);
        intent = t.intent;
      }
      plan = planOrder(await withStartReferences(env, penv, intent.type), intent) as PairOrderPlan;
    } catch (e) {
      w.stats.inc(`plan_throw:${tag}`);
      w.log.warn('pair planner threw', { tag, error: errText(e) });
      return { ok: false, error: errText(e) };
    }
    if (!plan.ok || !plan.built) return refusal(w, plan, tag, 'pair plan refused');
    const created = plan.built.covenants.map((c) => c.covenantId);
    const r = await w.submit(plan.built, tag, { amount: (intent as { amount?: bigint }).amount, base: base.ticker, quote: quote.ticker, kind: plan.pair?.kind, orders: created.length });
    if (r.ok) w.stats.inc(`placed:${tag}`);
    return { ...r, created, pair: plan.pair };
  });
}

export function snapshot(view: OrderView): OrderSnapshot | null {
  try {
    return snapshotFromOrderView(view);
  } catch {
    return null;
  }
}

/**
 * The list endpoint (`/v1/orders`) has no custody / strays: spend an order only from its full, fresh view (`/v1/orders/{id}`). An order whose
 * listed UTXO this key's own pending transaction already spends (a cancel or amend the indexer has not caught up with) is gone for it.
 */
async function fresh(env: Env, w: BotWallet, view: OrderView): Promise<OrderView | null> {
  try {
    const v = await env.indexer.order(view.covenant_id);
    return v && (v.status === 'open' || v.status === 'partial') && w.actionable([v]).length ? v : null;
  } catch {
    return null;
  }
}

export function cancel(env: Env, w: BotWallet, listed: OrderView, tag = 'cancel'): Promise<SubmitOutcome> {
  return w.exclusive(async (): Promise<SubmitOutcome> => {
    const view = await fresh(env, w, listed);
    if (!view) return { ok: false, error: 'order is gone' };
    const snap = snapshot(view);
    if (!snap) return { ok: false, error: 'no snapshot' };
    const plan = planCancel(await cancelEnv(env, w, marketOfView(env, view)), snap);
    if (!plan.ok || !plan.built) {
      w.stats.inc(`plan_refused:${tag}:${errors(plan)[0]?.code ?? 'unknown'}`);
      return { ok: false, error: errors(plan)[0]?.message };
    }
    return w.submit(plan.built, tag, { order: view.covenant_id, contract: view.contract });
  });
}

/** amend a plain limit order to `price` (sompi per whole token; cancel + replacement in one transaction), optionally to `amount` base units */
export function amend(env: Env, w: BotWallet, listed: OrderView, price: bigint, amount?: bigint): Promise<SubmitOutcome> {
  return w.exclusive(() => amendNow(env, w, listed, price, amount));
}

async function amendNow(env: Env, w: BotWallet, listed: OrderView, price: bigint, amount?: bigint): Promise<SubmitOutcome> {
  const view = await fresh(env, w, listed);
  if (!view) return { ok: false, error: 'order is gone' };
  const snap = snapshot(view);
  if (!snap) return { ok: false, error: 'no snapshot' };
  const m = marketOfView(env, view);
  const r = buildReplacement(snap, 'limit', { price, ...(amount ? { amount } : {}) }, { tick: m.tick });
  if (!r.ok || !r.spec) {
    w.stats.inc(`plan_refused:amend:${r.issues[0]?.code ?? 'unknown'}`);
    return { ok: false, error: r.issues[0]?.message };
  }
  const plan = planCancelReplace(await cancelEnv(env, w, m), snap, r.spec);
  if (!plan.ok || !plan.built) {
    w.stats.inc(`plan_refused:amend:${errors(plan)[0]?.code ?? 'unknown'}`);
    w.log.warn('amend refused', { order: view.covenant_id, contract: view.contract, code: errors(plan)[0]?.code, message: errors(plan)[0]?.message });
    return { ok: false, error: errors(plan)[0]?.message };
  }
  return w.submit(plan.built, 'amend', { order: view.covenant_id, price });
}

/** cancel every given order (positions: entry + exits together), several per transaction */
export function cancelAll(env: Env, w: BotWallet, listed: OrderView[], tag = 'cancelAll'): Promise<number> {
  return w.exclusive(() => cancelAllNow(env, w, listed, tag));
}

async function cancelAllNow(env: Env, w: BotWallet, listed: OrderView[], tag: string): Promise<number> {
  const views = (await Promise.all(listed.map((v) => fresh(env, w, v)))).filter((v): v is OrderView => !!v);
  // one cancel-all per token (the cancel environment carries the maker's token UTXOs of one token)
  const byToken = new Map<string, OrderSnapshot[]>();
  for (const v of views) {
    const s = snapshot(v);
    if (!s) continue;
    const k = marketOfView(env, v).covenantId;
    byToken.set(k, [...(byToken.get(k) ?? []), s]);
  }
  const plans = [];
  for (const [tok, snaps] of byToken) plans.push(...planCancelAll(await cancelEnv(env, w, tok), snaps, { maxOrdersPerTx: 6 }));
  let ok = 0;
  for (const p of plans) {
    if (!p.ok || !p.built) {
      w.stats.inc(`plan_refused:${tag}:${errors(p)[0]?.code ?? 'unknown'}`);
      continue;
    }
    const r = await w.submit(p.built, tag, { orders: p.cancelIds.length });
    if (r.ok) ok += p.cancelIds.length;
  }
  return ok;
}

/**
 * The price of an order view in sompi per whole token: its current quote (an auction's price now) or its limit. The state price is per `scale`
 * base units; the soak's orders all have the token's standard scale 10^decimals (the indexer lists no other), so it is per whole token.
 */
export function priceOf(view: OrderView): bigint | null {
  const p = view.quote ?? view.price;
  return p == null ? null : BigInt(p);
}

// ------------------------------------------------------------------------------------------------ scenario actions (gap review 2026-10-13)

/** "txid:index" of an order view's current UTXO ('' when it has none) */
export const spendsOf = (v: OrderView): string => (v.current ? `${v.current.txid}:${v.current.index}` : '');

/** a proven order state as flat decimal strings (the checker's scenario records compare these) */
export const flatState = (state: unknown): Record<string, string> => Object.fromEntries(Object.entries((state ?? {}) as Record<string, unknown>).map(([k, v]) => [k, String(v)]));

/** what a cancel-all submitted: the orders of its successful transactions (with the outpoint each spent) and those transactions */
export interface CancelAllOutcome {
  orders: { id: string; spends: string }[];
  txids: string[];
}

/** cancel every given order like `cancelAll`, and say which orders the submitted transactions cancel (scenario records) */
export function cancelAllDetailed(env: Env, w: BotWallet, listed: OrderView[], tag: string): Promise<CancelAllOutcome> {
  return w.exclusive(async () => {
    const views = (await Promise.all(listed.map((v) => fresh(env, w, v)))).filter((v): v is OrderView => !!v);
    const byId = new Map(views.map((v) => [v.covenant_id, v]));
    const byToken = new Map<string, OrderSnapshot[]>();
    for (const v of views) {
      const s = snapshot(v);
      if (!s) continue;
      const k = marketOfView(env, v).covenantId;
      byToken.set(k, [...(byToken.get(k) ?? []), s]);
    }
    const out: CancelAllOutcome = { orders: [], txids: [] };
    for (const [tok, snaps] of byToken) {
      for (const p of planCancelAll(await cancelEnv(env, w, tok), snaps, { maxOrdersPerTx: 6 })) {
        if (!p.ok || !p.built) {
          w.stats.inc(`plan_refused:${tag}:${errors(p)[0]?.code ?? 'unknown'}`);
          continue;
        }
        const r = await w.submit(p.built, tag, { orders: p.cancelIds.length });
        if (!r.ok || !r.txid) continue;
        out.txids.push(r.txid);
        for (const id of p.cancelIds) {
          const v = byId.get(id);
          out.orders.push({ id, spends: v ? spendsOf(v) : '' });
        }
      }
    }
    return out;
  });
}

/** what a replacement (renewal, pair amend) submitted: the replaced order, the replacement's id and terms */
export interface ReplaceOutcome extends SubmitOutcome {
  old?: { id: string; spends: string; expiryDaa: string; state: Record<string, string> };
  id?: string;
  inPlace?: boolean;
  expect?: Record<string, string>;
}

/** submits a planned cancel-replace and describes it (the replacement's id: the old one when amended in place, else the created covenant) */
async function submitReplace(w: BotWallet, view: OrderView, snap: OrderSnapshot, plan: ReturnType<typeof planCancelReplace>, spec: ReplacementSpec, tag: string): Promise<ReplaceOutcome> {
  if (!plan.ok || !plan.built) {
    w.stats.inc(`plan_refused:${tag}:${errors(plan)[0]?.code ?? 'unknown'}`);
    return { ok: false, error: errors(plan)[0]?.message };
  }
  const inPlace = plan.issues.some((i) => i.code.startsWith('cancel.amend-in-place'));
  const created = plan.built.covenants.map((c) => c.covenantId);
  const id = inPlace ? view.covenant_id : (created.find((c) => c !== view.covenant_id) ?? created[0]);
  const r = await w.submit(plan.built, tag, { order: view.covenant_id, replacement: id, inPlace });
  const before = flatState(snap.order.state.state);
  return { ...r, old: { id: view.covenant_id, spends: spendsOf(view), expiryDaa: before.expiryDaa ?? '0', state: before }, id, inPlace, expect: flatState(spec.order.state) };
}

/**
 * GTC renewal through the web's renew path (My orders "Renew", `planAmendFor` with `renew`): the replacement spec from the CURRENT state with a
 * fresh 90-day expiry (amend.ts `buildReplacement`, kind `limit` for a plain ask / bid, `cond` for a stop / take-profit / OCO), then ONE atomic
 * cancel-replace (`planCancelReplace`, in place where the protocol allows).
 */
export function renew(env: Env, w: BotWallet, listed: OrderView, kind: 'limit' | 'cond', tag = 'renew'): Promise<ReplaceOutcome> {
  return w.exclusive(async (): Promise<ReplaceOutcome> => {
    const view = await fresh(env, w, listed);
    if (!view) return { ok: false, error: 'order is gone' };
    const snap = snapshot(view);
    if (!snap) return { ok: false, error: 'no snapshot' };
    const m = marketOfView(env, view);
    const clock = await env.node.getClock();
    const r = buildReplacement(snap, kind, { renew: true }, { tick: m.tick, kob: env.kob, nowDaa: clock.daa });
    if (!r.ok || !r.spec) {
      w.stats.inc(`plan_refused:${tag}:${r.issues.find((i) => i.severity === 'error')?.code ?? 'unknown'}`);
      return { ok: false, error: r.issues[0]?.message };
    }
    return submitReplace(w, view, snap, planCancelReplace(await cancelEnv(env, w, m), snap, r.spec), r.spec, tag);
  });
}

/**
 * Pair amend through the pair ticket's replace path (My orders "Replace" of a resting pair limit, ReplaceDialog): the order back to its ticket
 * intent (`intentFromOrder`), the new price, re-planned against the pair environment with the order's own custody counted (`replaceEnv`), and the
 * planned order as the replacement of ONE atomic cancel-replace (`planCancelReplace` with the maker's B UTXOs beside A's, as `planReplaceFor`).
 */
export function pairAmend(env: Env, w: BotWallet, listed: OrderView, base: TokenMarket, quote: TokenMarket, newPrice: (side: 'buy' | 'sell', price: bigint) => bigint, tag: string): Promise<ReplaceOutcome> {
  return w.exclusive(async (): Promise<ReplaceOutcome> => {
    const view = await fresh(env, w, listed);
    if (!view) return { ok: false, error: 'order is gone' };
    const snap = snapshot(view);
    if (!snap) return { ok: false, error: 'no snapshot' };
    const state = snap.order.state;
    const clock = await env.node.getClock();
    const desc = describeOrder(state, env.kob);
    const genesisDaa = BigInt(view.genesis.daa);
    const deadline = view.deadline != null ? BigInt(view.deadline) : null;
    const typeKey = orderTypeKey({ kind: state.kind, desc, deadline, genesisDaa: genesisDaa > 0n ? genesisDaa : null, nowDaa: clock.daa });
    const expiryKind = typeKey === 'limit-day' ? 'day' : typeKey === 'limit-gtd' ? 'gtd' : typeKey === 'limit' ? 'gtc' : 'none';
    const clk = { daa: clock.daa, unixSeconds: clock.unixSeconds, rateMilli: clock.rateMilli ?? 10_000 };
    const back = intentFromOrder(state, { typeKey, expiryKind, nowDaa: clock.daa, expiryUnix: expiryKind === 'gtd' ? daaToUnix(clk, desc.expiryDaa) : null, daaToUnix: (d) => daaToUnix(clk, d) }, env.kob);
    if (!back.ok || back.intent.type !== 'limit') {
      w.stats.inc(`plan_refused:${tag}:replace-${back.ok ? back.intent.type : back.reason}`);
      return { ok: false, error: 'not a resting pair limit' };
    }
    const intent = { ...back.intent, price: newPrice(back.intent.side, back.intent.price) };
    let plan;
    try {
      const penv = replaceEnv(await pairPlanEnv(env, w, base, quote), { id: view.covenant_id, entry: { view } }, state);
      plan = planOrder(penv, intent);
    } catch (e) {
      w.stats.inc(`plan_throw:${tag}`);
      return { ok: false, error: errText(e) };
    }
    const spec = plan.ok && errors(plan).length === 0 ? specOfPlan(plan) : null;
    if (!spec) {
      w.stats.inc(`plan_refused:${tag}:${errors(plan)[0]?.code ?? 'unknown'}`);
      return { ok: false, error: errors(plan)[0]?.message };
    }
    const cenv = await cancelEnv(env, w, base);
    const quoteUtxos = await w.tokenUtxos(tokenRef(env, quote));
    return submitReplace(w, view, snap, planCancelReplace({ ...cenv, tokenUtxos: [...(cenv.tokenUtxos ?? []), ...quoteUtxos] }, snap, spec), spec, tag);
  });
}

/** what a token merge did: the inputs it merged, the links submitted and the output it ended with (null when nothing was submitted) */
export interface MergeOutcome {
  inputs: TokenUtxo[];
  links: string[];
  final: { txid: string; index: number } | null;
  before: number;
  issue?: string;
}

/**
 * "Merge tokens" through the web's merge planner (kob/merge.ts, My orders' merge flow): `planMerge` over the wallet's plain UTXOs of `m` (smallest
 * first; KOB's standard program: 3 -> 1, then 1 + 2 -> 1, ... up to 10 links), link 0 submitted as planned, every next link rebuilt once the previous
 * one's output is accepted (read from the NODE, as the app's chained Merge did before it became one transaction per click), with fresh KAS UTXOs. Only one link holds the wallet lock at a time.
 */
export async function mergeTokens(env: Env, w: BotWallet, m: TokenMarket, tag = 'mergeTokens'): Promise<MergeOutcome> {
  const token: MergeToken = { covenantId: m.covenantId, program: m.program, templateHash: m.templateHash, extensionCommitment: m.family === 'kron' ? null : m.extensionCommitment, slots: { inputs: m.slots.inputs } };
  const out: MergeOutcome = { inputs: [], links: [], final: null, before: 0 };
  const first = await w.exclusive(async () => {
    const cenv = await cancelEnv(env, w, m);
    const plan = planMerge(cenv, token, cenv.tokenUtxos ?? [], w.reservedKeys());
    out.before = plan.before;
    if (!plan.ok) {
      w.stats.inc(`plan_refused:${tag}:${plan.issues[0]?.code ?? 'unknown'}`);
      out.issue = plan.issues[0]?.code;
      return null;
    }
    const l0 = plan.links[0]!;
    const r = await w.submit(l0.built, tag, { token: m.ticker, link: 1, links: plan.links.length, inputs: l0.inputs.length });
    if (!r.ok || !r.txid) return null;
    out.inputs.push(...l0.fresh);
    out.links.push(r.txid);
    out.final = { txid: r.txid, index: l0.output.index };
    return { plan, prev: l0, txid: r.txid };
  });
  if (!first) return out;
  let { prev, txid } = first;
  for (let k = 1; k < first.plan.groups.length; k++) {
    const o = prev.built.tx.outputs[prev.output.index]!;
    const acc = await waitForAcceptance(env.node, { address: spkStringToAddress(env.sdk, o.scriptPublicKey, env.cfg.network), transactionId: txid, index: prev.output.index }, { timeoutMs: 90_000 });
    const found = acc.found[0];
    if (!acc.accepted || !found) {
      w.stats.inc(`${tag}_link_not_accepted`);
      break;
    }
    const group = first.plan.groups[k]!;
    const carryFrom = prev;
    const carryTxid = txid;
    const next = await w.exclusive(async () => {
      const carry = mergeCarry(carryFrom, carryTxid, token, found);
      const link = buildMergeLink(await cancelEnv(env, w, m), token, [carry, ...group], group);
      if ('error' in link) {
        w.stats.inc(`plan_refused:${tag}:${link.error.code}`);
        return null;
      }
      const r = await w.submit(link.built, tag, { token: m.ticker, link: k + 1, links: first.plan.links.length, inputs: link.inputs.length });
      return r.ok && r.txid ? { link, txid: r.txid } : null;
    });
    if (!next) break;
    out.inputs.push(...group);
    out.links.push(next.txid);
    out.final = { txid: next.txid, index: next.link.output.index };
    prev = next.link;
    txid = next.txid;
  }
  return out;
}
