// What the ticket says about the fills of an order that takes liquidity now (a market order, an IOC, a buy that crosses the book, a rising bid):
// the estimate next to its Fills box (`hint`: how many fills the visible book needs for the amount, at most `perTx` counterparties per
// transaction, and the carriers a buy deposits for them) and, when the fills cannot take the whole amount from the visible book, the line under
// the main fields (`short`): one transaction takes at most 3 asks, so a large market buy against many small asks fills only part and the rest
// returns. From the planner's `OrderPlan.fills` (guards.ts `estimateFills`); pure and DOM-free.
import { t } from '../../i18n';
import type { PlanFills } from '../../kob/plan-types';
import { formatKas, formatTokenAmount } from '../../kob/units';

export interface FillsNote {
  /** the line next to the Fills box (null when the order has no such box: an IOC) */
  hint: string | null;
  /** the warning under the main fields, null when the fills can take the whole amount the visible book holds */
  short: string | null;
  /**
   * what the user must know about the quantity of a multi-fill market order, under the main fields: the Fills value the minimum fill cut, and the
   * most the escrow may deliver above the typed amount (a bid is a KAS budget); null when neither applies
   */
  receive: string | null;
}

/**
 * The note of `f` (`box`: the form shows a Fills box for the order). A resting buy's estimate (`f.scope` 'crossing') is for the part that
 * crosses now. `amount` is the order's, in base units of the token (`decimals`, `ticker`). `inverted`: the market is shown as KAS/TOKEN, where
 * the chain's asks (selling the token for KAS) are the bids for KAS and the other way round, so the counterparties are named as that view shows them.
 */
export function fillsNote(f: PlanFills, c: { decimals: number; ticker: string; box: boolean; amount: bigint; inverted?: boolean }): FillsNote {
  const counterparty = c.inverted ? (f.counterparty === 'asks' ? 'bids' : 'asks') : f.counterparty;
  const tokens = (n: bigint): string => `${formatTokenAmount(n, c.decimals, { group: ',' })} ${c.ticker}`;
  const parts: string[] = [];
  if (f.needed === 0) parts.push(t('ticket.fills.nothing'));
  else parts.push(t(f.scope === 'crossing' ? 'ticket.fills.estimateCross' : 'ticket.fills.estimate', { needed: f.needed, perTx: f.perTx, counterparty }));
  parts.push(f.carrierDeposit > 0n ? t('ticket.fills.deposit', { fills: f.budgeted.toString(), deposit: formatKas(f.carrierDeposit, { group: ',' }) }) : t(c.inverted ? 'ticket.fills.noCarrierInv' : 'ticket.fills.noCarrier', { ticker: c.ticker }));
  // the Fills value the minimum fill cut down, and what the escrow can deliver beyond the typed amount (never more KAS than the budget)
  const quantity: string[] = [];
  if (f.requested !== undefined && f.requested > f.budgeted && f.minFill !== undefined) {
    quantity.push(t('ticket.fills.clamped', { requested: f.requested.toString(), fills: f.budgeted.toString(), minFill: tokens(f.minFill), amount: tokens(c.amount) }));
  }
  if (f.maxReceive !== undefined && f.maxReceive > c.amount) quantity.push(t('ticket.fills.maxReceive', { max: tokens(f.maxReceive), amount: tokens(c.amount) }));
  const lacking = f.needed > 0 && BigInt(f.needed) > f.budgeted && f.fillable < c.amount;
  let short: string | null = null;
  if (lacking) {
    const params = { perTx: f.perTx, counterparty, fills: f.budgeted.toString(), fillable: tokens(f.fillable), amount: tokens(c.amount) };
    short = f.budgeted === 1n ? t('ticket.fills.shortOneTx', params) + (c.box ? ` ${t('ticket.fills.raise')}` : '') : t('ticket.fills.short', params);
  }
  return { hint: c.box ? parts.join(' ') : null, short, receive: c.box && quantity.length > 0 ? quantity.join(' ') : null };
}
