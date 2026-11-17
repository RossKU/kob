// What the planner claims about a placement, in the shape the pre-sign decoder compares against the transaction (`ExpectedSigning`).
// The claims come from the planned STATE and disclosure, never from the form text; `decodeSigning` re-derives everything from the transaction and
// blocks signing on any disagreement (a bug in the planner, the builder or a tampered wasm can therefore not pass unnoticed).
import type { ExpectedSigning } from '../../kob/decode';
import type { OrderPlan } from '../../kob/plan-types';

export function expectedFromPlan(plan: OrderPlan): ExpectedSigning {
  const d = plan.disclosure;
  // a take creates no order: the tokens it trades, the KAS bound at the limit and the orders it fills
  const tk = plan.take;
  if (tk) {
    return {
      orders: [],
      take: { side: tk.side, token: tk.token, amount: tk.amount, bound: tk.bound, orders: tk.steps[0]!.fills.map((f) => f.covenantId) },
    };
  }
  return {
    orders: plan.states,
    ...(d ? { kasLocked: d.kasLocked, tokensEscrowed: d.tokensEscrowed } : {}),
  };
}
