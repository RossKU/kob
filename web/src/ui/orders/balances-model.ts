// Balances the wallet does not show: free tokens (P2PK-owned, tracked locally / by the indexer), tokens in 0x04 custody of live orders,
// stray tokens, and KAS locked in orders. Combines the pure helpers of kob/balances.ts with the indexer's stray list. Pure.
import type { OrderView, StrayView } from '../../data/indexer-types';
import { escrowedBalances, kasBalanceSummary, withFreeBalances, type TokenBalance } from '../../kob/balances';
import type { OrderSnapshot } from '../../kob/cancel';
import type { Hex, TokenUtxo } from '../../kob/types';

export interface CombinedBalances {
  /** one entry per token that has free, escrowed or stray tokens or KAS locked (sorted by token id) */
  tokens: TokenBalance[];
  kas: { free: bigint; locked: bigint; total: bigint };
  /** strays that can never be recovered because their order is closed (`lost`): informational */
  lostStrayTokens: bigint;
}

const big = (v: string | number | bigint | null | undefined): bigint => (v === null || v === undefined ? 0n : BigInt(v));

/** Per token: sum of the live (unspent) stray UTXOs' token amounts, split into recoverable (the order is live) and lost. */
export function strayTotals(strays: readonly StrayView[]): Map<Hex, { recoverable: bigint; lost: bigint }> {
  const out = new Map<Hex, { recoverable: bigint; lost: bigint }>();
  for (const s of strays) {
    if (s.spent) continue;
    const amount = big(s.state?.amount ?? s.amount);
    const cur = out.get(s.token) ?? { recoverable: 0n, lost: 0n };
    if (s.lost) cur.lost += amount;
    else cur.recoverable += amount;
    out.set(s.token, cur);
  }
  return out;
}

/**
 * `liveOrders`: the wallet's live orders (list views are fine: a missing custody UTXO falls back to the amount the order's state must hold).
 * The strays of the separate stray list REPLACE any stray numbers inside the order views, so nothing is counted twice.
 */
export function combineBalances(o: { maker: Hex; freeKas: bigint; freeTokens: readonly TokenUtxo[]; liveOrders: readonly (OrderView | OrderSnapshot)[]; strays: readonly StrayView[] }): CombinedBalances {
  // views: the separate stray list is authoritative; snapshots (resolved from a node) never know strays
  const noStrays = o.liveOrders.map((v) => ('covenant_id' in v ? { ...v, strays: undefined } : { ...v, strays: [] }));
  const balances = withFreeBalances(escrowedBalances(noStrays), [...o.freeTokens], o.maker);
  const totals = strayTotals(o.strays);
  const by = new Map(balances.map((b) => [b.token, { ...b, strays: 0n }]));
  let lost = 0n;
  for (const [token, t] of totals) {
    lost += t.lost;
    const cur = by.get(token) ?? { token, free: 0n, escrowed: 0n, strays: 0n, kasLocked: 0n, kasEscrow: 0n, kasCarriers: 0n, orderCount: 0 };
    by.set(token, { ...cur, strays: t.recoverable });
  }
  const tokens = [...by.values()].filter((b) => b.free > 0n || b.escrowed > 0n || b.strays > 0n || b.kasLocked > 0n || b.orderCount > 0).sort((a, b) => (a.token < b.token ? -1 : a.token > b.token ? 1 : 0));
  return { tokens, kas: kasBalanceSummary(o.freeKas, tokens), lostStrayTokens: lost };
}
