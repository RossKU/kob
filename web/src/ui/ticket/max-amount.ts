// "Max" of the order ticket: the largest amount the wallet can place, found by asking the planner (the exact builder cost: carriers, fee, escrow
// rounding) instead of a rule of thumb. A sell is the free token balance; a buy is a binary search on the amount, because the cost of a buy grows
// with the amount and the fee depends on the inputs it needs.
import type { OrderPlan, PairOrderPlan } from '../../kob/plan-types';
import { quoteOf } from '../../kob/units';

/** KAS kept back after a buy (a later cancel or amend pays its own fee): 0.1 KAS */
export const MAX_KAS_RESERVE = 10_000_000n;
/** base units of the quote token kept back per order of a pair buy (the escrow of a bid rounds per fill) */
export const MAX_QUOTE_SLACK = 4n;

const SHORT = new Set(['INSUFFICIENT_KAS', 'INSUFFICIENT_TOKENS', 'PAIR_INSUFFICIENT_TOKENS']);
/** the planner refused for lack of funds (KAS for the escrow, carriers or fee; tokens) */
export const fundsShort = (p: Pick<OrderPlan, 'issues'>): boolean => p.issues.some((i) => i.severity === 'error' && SHORT.has(i.code));

export interface MaxBuyInput {
  /** the plan of the order for an amount in base units (the form's order with that amount); null = it cannot be planned (a field is missing) */
  planAt(amount: bigint): OrderPlan | null;
  /** no amount above this can be afforded (the balance at the price with nothing reserved): the top of the search */
  upper: bigint;
  /** the amount a rule of thumb allows: used where the planner cannot say (the order fails a rule before funds are looked at) */
  conservative: bigint;
  kasBalance: bigint;
  /** a pair buy pays in the quote token: its balance */
  quoteBalance?: bigint;
  /** a ladder: the price of every order (state prices, first = the typed one); the amount is per order, the sum must be affordable */
  ladderPrices?: readonly bigint[] | null;
  /** KAS kept back, default `MAX_KAS_RESERVE` */
  reserve?: bigint;
}

const ceilDiv = (a: bigint, b: bigint): bigint => (a + b - 1n) / b;

/** Whether the order (or the whole ladder) planned as `plan` for `amount` leaves the reserve in the wallet. */
function fits(plan: OrderPlan | null, amount: bigint, i: MaxBuyInput): boolean {
  if (plan && fundsShort(plan)) return false;
  const d = plan?.disclosure;
  if (!plan || !plan.built || !d) return amount <= i.conservative;
  const prices = i.ladderPrices && i.ladderPrices.length > 1 ? i.ladderPrices : null;
  const count = BigInt(prices?.length ?? 1);
  const first = prices?.[0] ?? 0n;
  const fee = d.fee ?? 0n;
  const reserve = i.reserve ?? MAX_KAS_RESERVE;
  if (i.quoteBalance !== undefined) {
    // pair: the escrow is in the quote token (what the plan holds, scaled to each price of a ladder), the KAS is carriers and fees
    const escrow = (plan as PairOrderPlan).pair?.escrowB ?? 0n;
    let b = 0n;
    for (const p of prices ?? [first]) b += prices && first > 0n ? ceilDiv(escrow * p, first) : escrow;
    return b + MAX_QUOTE_SLACK * count <= i.quoteBalance && (d.kasLocked + fee) * count + reserve <= i.kasBalance;
  }
  // KAS market: the escrow is KAS; a ladder level at another price escrows `amount x (price - first price)` more (less when lower)
  let need = fee * count;
  for (const p of prices ?? [first]) {
    const delta = prices ? quoteOf(amount, p > first ? p - first : first - p, d.scale, 'down') : 0n;
    need += d.kasLocked + (p > first ? delta : -delta);
  }
  return need + reserve <= i.kasBalance;
}

/** The largest amount (base units, per order of a ladder) that `fits`; 0 when not even one base unit does. */
export function maxBuyAmount(i: MaxBuyInput): bigint {
  const ok = (n: bigint): boolean => {
    let plan: OrderPlan | null = null;
    try {
      plan = i.planAt(n);
    } catch {
      plan = null;
    }
    return fits(plan, n, i);
  };
  let lo = 0n;
  let hi = i.upper;
  while (lo < hi) {
    const mid = (lo + hi + 1n) / 2n;
    if (ok(mid)) lo = mid;
    else hi = mid - 1n;
  }
  return lo;
}
