// Shared bot actions over the web app's planners: place any intent, cancel, amend (cancel-replace), cancel-all.
import { planOrder, type Intent } from '@/kob/plan';
import type { PairOrderPlan, TokenMarket } from '@/kob/plan-types';
import { errors, type BookView } from '@/kob/plan-types';
import { planCancel, planCancelAll, planCancelReplace, snapshotFromOrderView, type OrderSnapshot } from '@/kob/cancel';
import { buildReplacement } from '@/ui/orders/amend';
import type { OrderView } from '@/data/indexer-types';
import type { Env } from '../env';
import { errText } from '../log';
import { cancelEnv, pairPlanEnv, planEnv, tokenMarket, type Which } from '../market';
import type { BotWallet, SubmitOutcome } from '../wallet';

export interface PlaceResult extends SubmitOutcome {
  /** covenant ids of the orders the transaction creates */
  created?: string[];
  planIssue?: string;
  /** the parameters of that refusal (SELF_TRADE: `ownCovenantId`, the own resting order the plan would trade against) */
  planParams?: Record<string, unknown>;
}

/** Plans `intent` against the live book / wallet and submits it; plan refusals are counted per code (never thrown). */
export async function place(env: Env, w: BotWallet, intent: Intent, tag: string, o: { book?: BookView; mine?: OrderView[] } = {}, which?: Which): Promise<PlaceResult> {
  w.stats.inc(`try:${tag}`);
  let plan;
  try {
    plan = planOrder(await planEnv(env, w, o, which), intent);
  } catch (e) {
    w.stats.inc(`plan_throw:${tag}`);
    w.log.warn('planner threw', { tag, error: errText(e) });
    return { ok: false, error: errText(e) };
  }
  if (!plan.ok || !plan.built) {
    const errs = errors(plan);
    const code = errs[0]?.code ?? 'unknown';
    w.stats.inc(`plan_refused:${tag}:${code}`);
    w.log.debug('plan refused', { tag, code, message: errs[0]?.message, params: errs[0]?.params });
    return { ok: false, planIssue: code, error: errs[0]?.message };
  }
  const created = plan.built.covenants.map((c) => c.covenantId);
  const r = await w.submit(plan.built, tag, { amount: (intent as { amount?: bigint }).amount, orders: created.length });
  if (r.ok) w.stats.inc(`placed:${tag}`);
  return { ...r, created };
}

/** the soak market of an order view (a pair order's token is its base token A) */
export function marketOfView(env: Env, view: OrderView) {
  return tokenMarket(env, view.token ?? undefined);
}

/**
 * Plans a PAIR order through the web planner (`planOrder` with a PairPlanEnv: every order type of the KAS ticket on the pair `base`/`quote`,
 * prices B base units per whole A) and submits it. Refusals are counted like `place`.
 */
export async function placePair(env: Env, w: BotWallet, intent: Intent, tag: string, base: TokenMarket, quote: TokenMarket): Promise<PlaceResult & { pair?: PairOrderPlan['pair'] }> {
  w.stats.inc(`try:${tag}`);
  let plan: PairOrderPlan;
  try {
    plan = planOrder(await pairPlanEnv(env, w, base, quote), intent) as PairOrderPlan;
  } catch (e) {
    w.stats.inc(`plan_throw:${tag}`);
    w.log.warn('pair planner threw', { tag, error: errText(e) });
    return { ok: false, error: errText(e) };
  }
  if (!plan.ok || !plan.built) {
    const errs = errors(plan);
    const code = errs[0]?.code ?? 'unknown';
    w.stats.inc(`plan_refused:${tag}:${code}`);
    w.log.debug('pair plan refused', { tag, code, message: errs[0]?.message, params: errs[0]?.params });
    return { ok: false, planIssue: code, planParams: errs[0]?.params as Record<string, unknown> | undefined, error: errs[0]?.message };
  }
  const created = plan.built.covenants.map((c) => c.covenantId);
  const r = await w.submit(plan.built, tag, { amount: (intent as { amount?: bigint }).amount, base: base.ticker, quote: quote.ticker, kind: plan.pair?.kind, orders: created.length });
  if (r.ok) w.stats.inc(`placed:${tag}`);
  return { ...r, created, pair: plan.pair };
}

export function snapshot(view: OrderView): OrderSnapshot | null {
  try {
    return snapshotFromOrderView(view);
  } catch {
    return null;
  }
}

/** The list endpoint (`/v1/orders`) has no custody / strays: spend an order only from its full, fresh view (`/v1/orders/{id}`). */
async function fresh(env: Env, view: OrderView): Promise<OrderView | null> {
  try {
    const v = await env.indexer.order(view.covenant_id);
    return v && (v.status === 'open' || v.status === 'partial') ? v : null;
  } catch {
    return null;
  }
}

export async function cancel(env: Env, w: BotWallet, listed: OrderView, tag = 'cancel'): Promise<SubmitOutcome> {
  const view = await fresh(env, listed);
  if (!view) return { ok: false, error: 'order is gone' };
  const snap = snapshot(view);
  if (!snap) return { ok: false, error: 'no snapshot' };
  const plan = planCancel(await cancelEnv(env, w, marketOfView(env, view)), snap);
  if (!plan.ok || !plan.built) {
    w.stats.inc(`plan_refused:${tag}:${errors(plan)[0]?.code ?? 'unknown'}`);
    return { ok: false, error: errors(plan)[0]?.message };
  }
  return w.submit(plan.built, tag, { order: view.covenant_id, contract: view.contract });
}

/** amend a plain limit order to `price` (sompi per whole token; cancel + replacement in one transaction), optionally to `amount` base units */
export async function amend(env: Env, w: BotWallet, listed: OrderView, price: bigint, amount?: bigint): Promise<SubmitOutcome> {
  const view = await fresh(env, listed);
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
export async function cancelAll(env: Env, w: BotWallet, listed: OrderView[], tag = 'cancelAll'): Promise<number> {
  const views = (await Promise.all(listed.map((v) => fresh(env, v)))).filter((v): v is OrderView => !!v);
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
