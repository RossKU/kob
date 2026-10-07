// What the planner claims about a placement, in the shape the pre-sign decoder compares against the transaction (`ExpectedSigning`).
// The claims come from the planned STATE and disclosure, never from the form text; `decodeSigning` re-derives everything from the transaction and
// blocks signing on any disagreement (a bug in the planner, the builder or a tampered wasm can therefore not pass unnoticed).
import type { ExpectedSigning } from '../../kob/decode';
import type { OrderPlan } from '../../kob/plan-types';

export function expectedFromPlan(plan: OrderPlan): ExpectedSigning {
  const d = plan.disclosure;
  return {
    orders: plan.states,
    ...(d ? { kasLocked: d.kasLocked, tokensEscrowed: d.tokensEscrowed } : {}),
  };
}

/**
 * The warnings of the plan made at Review from fresh data that the plan the ticket showed did not have: what the confirmation must show because the
 * user has not seen it yet. A warning is new when its code was not shown, or when it is worse than the one shown (a larger gap: its `bps` or
 * `percent`), e.g. an acknowledged market start that has moved further from its reference.
 */
export function newWarnings(shown: Pick<OrderPlan, 'issues'> | null, fresh: Pick<OrderPlan, 'issues'>): OrderPlan['issues'] {
  const seen = new Map<string, number | null>();
  for (const i of shown?.issues ?? []) {
    const m = magnitude(i);
    const prev = seen.get(i.code);
    seen.set(i.code, prev === undefined ? m : prev === null || m === null ? prev ?? m : Math.max(prev, m));
  }
  return fresh.issues.filter((i) => {
    if (i.severity !== 'warning') return false;
    if (!seen.has(i.code)) return true;
    const before = seen.get(i.code) ?? null;
    const now = magnitude(i);
    return before !== null && now !== null && now > before;
  });
}

/** How large a finding is, in bps, when it carries a gap (`bps`, or a `percent` text); null otherwise. */
function magnitude(i: OrderPlan['issues'][number]): number | null {
  const p = (i.params ?? {}) as Record<string, unknown>;
  if (typeof p.bps === 'bigint') return Number(p.bps);
  if (p.percent !== undefined) {
    const n = Number(String(p.percent).replace(/,/g, ''));
    return Number.isFinite(n) ? n * 100 : null;
  }
  return null;
}
