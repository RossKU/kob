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
 * The warnings of the plan made at Review from fresh data that the plan the ticket showed did not have (by code): what the confirmation must show
 * because the user has not seen it yet.
 */
export function newWarnings(shown: Pick<OrderPlan, 'issues'> | null, fresh: Pick<OrderPlan, 'issues'>): OrderPlan['issues'] {
  const seen = new Set((shown?.issues ?? []).map((i) => i.code));
  return fresh.issues.filter((i) => i.severity === 'warning' && !seen.has(i.code));
}
