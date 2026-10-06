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
