// Balances the wallet does not show: free tokens (P2PK-owned, tracked locally / by the indexer; KRON tokens KOB cannot spend apart), tokens in 0x04 custody of live orders,
// stray tokens, and KAS locked in orders. Combines the pure helpers of kob/balances.ts with the indexer's stray list. Pure.
import type { OrderView, StrayView } from '../../data/indexer-types';
import { escrowedBalances, kasBalanceSummary, withFreeBalances, type TokenBalance } from '../../kob/balances';
import type { OrderSnapshot } from '../../kob/cancel';
import { TOKEN_PROGRAMS } from '../../kob/programs.generated';
import type { Hex, TokenProgram, TokenUtxo } from '../../kob/types';

export interface CombinedBalances {
  /** one entry per token that has free, escrowed or stray tokens or KAS locked (sorted by token id) */
  tokens: TokenBalance[];
  /**
   * `onTokens`: KAS on the wallet's own free token UTXOs (each fill delivers a token UTXO with its carrier and, for a buy, the price improvement
   * over the escrowed worst price): the wallet's, but spendable only by moving those UTXOs (Merge, a sale, a transfer); counted in `total`
   */
  kas: { free: bigint; locked: bigint; onTokens: bigint; total: bigint };
  /** strays that can never be recovered because their order is closed (`lost`): informational */
  lostStrayTokens: bigint;
}

/**
 * True when a balance's token program is not one KOB reviewed (a prototype or test-only layout, a third-party program pending review): the
 * balance is real (its script verified on the node under that pinned program) but is marked. `null` (unknown program) counts as unreviewed.
 */
export function unreviewedProgram(program: TokenProgram | null | undefined): boolean {
  return TOKEN_PROGRAMS.find((p) => p.name === program)?.review !== 'reviewed';
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
export function combineBalances(o: {
  maker: Hex;
  freeKas: bigint;
  freeTokens: readonly TokenUtxo[];
  liveOrders: readonly (OrderView | OrderSnapshot)[];
  strays: readonly StrayView[];
  /** the token program of each token (decides which KRON key-held type is spendable); absent: the family default */
  programs?: ReadonlyMap<Hex, TokenProgram>;
}): CombinedBalances {
  // views: the separate stray list is authoritative; snapshots (resolved from a node) never know strays
  const noStrays = o.liveOrders.map((v) => ('covenant_id' in v ? { ...v, strays: undefined } : { ...v, strays: [] }));
  const balances = withFreeBalances(escrowedBalances(noStrays), [...o.freeTokens], o.maker, (t) => o.programs?.get(t));
  const totals = strayTotals(o.strays);
  const by = new Map(balances.map((b) => [b.token, { ...b, strays: 0n }]));
  let lost = 0n;
  for (const [token, t] of totals) {
    lost += t.lost;
    const cur = by.get(token) ?? { token, free: 0n, notSpendable: 0n, escrowed: 0n, strays: 0n, kasLocked: 0n, kasEscrow: 0n, kasCarriers: 0n, orderCount: 0 };
    by.set(token, { ...cur, strays: t.recoverable });
  }
  const tokens = [...by.values()].filter((b) => b.free > 0n || b.notSpendable > 0n || b.escrowed > 0n || b.strays > 0n || b.kasLocked > 0n || b.orderCount > 0).sort((a, b) => (a.token < b.token ? -1 : a.token > b.token ? 1 : 0));
  const kas = kasBalanceSummary(o.freeKas, tokens);
  const onTokens = o.freeTokens.filter((u) => u.state.owner === o.maker).reduce((s, u) => s + big(u.amount), 0n);
  return { tokens, kas: { ...kas, onTokens, total: kas.total + onTokens }, lostStrayTokens: lost };
}

/**
 * The fixed fraction digits of the balance columns (the finest amount of each column, so the decimal points line up), over the tokens whose
 * decimals are known (`decimalsOf` returns `undefined` for a token without a registry entry). A token of unknown decimals is shown in base
 * units with no fraction: padding its base units to another token's fraction (`500,000.00000000`) would read as whole tokens.
 */
export function balanceColumnFractions(
  tokens: readonly TokenBalance[],
  decimalsOf: (token: Hex) => number | undefined,
  unitsFraction: (value: bigint, decimals: number) => number,
): { free: number; escrowed: number; strays: number; kas: number } {
  const out = { free: 0, escrowed: 0, strays: 0, kas: 0 };
  for (const b of tokens) {
    out.kas = Math.max(out.kas, unitsFraction(b.kasLocked, 8));
    const d = decimalsOf(b.token);
    if (d === undefined) continue;
    out.free = Math.max(out.free, unitsFraction(b.free, d));
    out.escrowed = Math.max(out.escrowed, unitsFraction(b.escrowed, d));
    out.strays = Math.max(out.strays, unitsFraction(b.strays, d));
  }
  return out;
}
