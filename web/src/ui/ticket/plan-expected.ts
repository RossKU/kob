// What the planner claims about a placement, in the shape the pre-sign decoder compares against the
// transaction (`ExpectedSigning`). The claims come from the planned state and disclosure, never
// from the form text. `decodeSigning` re-derives everything from the transaction and blocks
// signing on any disagreement, so a bug in the planner, the builder or a tampered wasm cannot pass
// unnoticed.
import type { ExpectedSigning } from '../../kob/decode';
import type { Clock } from '../../kob/plan-types';
import type { OrderPlan } from '../../kob/plan-types';

export function expectedFromPlan(plan: OrderPlan, present?: Clock): ExpectedSigning {
  const at = present ? { present: { daa: present.daa, unixSeconds: present.unixSeconds, ...(present.witness ? { witness: present.witness } : {}) } } : {};
  const d = plan.disclosure;
  // A take creates no order: the tokens it trades, the KAS bound at the limit and the orders it
  // fills.
  const tk = plan.take;
  if (tk) {
    return {
      orders: [],
      take: { side: tk.side, token: tk.token, amount: tk.amount, bound: tk.bound, orders: tk.steps[0]!.fills.map((f) => f.covenantId) },
    };
  }
  return {
    orders: plan.states,
    ...at,
    ...(d ? { kasLocked: d.kasLocked, tokensEscrowed: d.tokensEscrowed } : {}),
  };
}
