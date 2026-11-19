// Balances the wallet does not show. These are:
// - free tokens, which are P2PK-owned and tracked locally and by the indexer, where KRON tokens KOB
//   cannot spend are kept apart,
// - tokens in 0x04 custody of live orders,
// - tokens bought and held by mode-1 orders,
// - stray tokens,
// - KAS locked in orders.
// It combines the pure helpers of kob/balances.ts with the indexer's stray list. It is pure.
import type { OrderView, StrayView } from '../../data/indexer-types';
import { escrowedBalances, kasBalanceSummary, withFreeBalances, type TokenBalance } from '../../kob/balances';
import type { OrderSnapshot } from '../../kob/cancel';
import { TOKEN_PROGRAMS } from '../../kob/programs.generated';
import type { Hex, TokenProgram, TokenUtxo } from '../../kob/types';

export interface CombinedBalances {
  /**
   * One entry per token that has free, escrowed or stray tokens or KAS locked, sorted by token id.
   */
  tokens: TokenBalance[];
  /**
   * `onTokens` is the KAS on the wallet's own free token UTXOs. Each fill delivers a token UTXO
   * with its carrier and, for a buy, the price improvement over the escrowed worst price. It is the
   * wallet's, but spendable only by moving those UTXOs (Merge, a sale or a transfer). It is counted
   * in `total`.
   */
  kas: { free: bigint; locked: bigint; onTokens: bigint; total: bigint };
  /** Strays that can never be recovered because their order is closed (`lost`). Informational. */
  lostStrayTokens: bigint;
}

/**
 * True when a balance's token program is not one this build supports for trading, such as a
 * prototype or test-only layout or a third-party program whose engineering review is pending. The
 * balance is real, because its script verified on the node under that pinned program, but it is
 * marked. `null`, an unknown program, counts as unsupported.
 */
export function unreviewedProgram(program: TokenProgram | null | undefined): boolean {
  return TOKEN_PROGRAMS.find((p) => p.name === program)?.review !== 'reviewed';
}

/**
 * The program a balance row's token is under: the registry's, else the one the node verified its
 * free UTXOs, its creator lock's UTXOs or its orders under. A token held only in the wallet's
 * creator lock has no free UTXO and no order, so its lock names it.
 */
export function balanceProgram(
  token: Hex,
  o: { listed?: TokenProgram | null; free: ReadonlyMap<Hex, TokenProgram>; locked?: readonly { token: Hex; program: TokenProgram }[]; orders: ReadonlyMap<Hex, TokenProgram> },
): TokenProgram | null {
  return o.listed ?? o.free.get(token) ?? o.locked?.find((l) => l.token === token)?.program ?? o.orders.get(token) ?? null;
}

/**
 * The token program each live order names (`token_template_hash`, a pinned program's), by token. It
 * covers the program of a token held only in orders, which neither the registry nor a free UTXO
 * names. A whole new supply put on sale at its creation would otherwise read "unsupported program".
 */
export function orderTokenPrograms(views: readonly Pick<OrderView, 'token' | 'token_template_hash'>[]): Map<Hex, TokenProgram> {
  const out = new Map<Hex, TokenProgram>();
  for (const v of views) {
    const p = TOKEN_PROGRAMS.find((x) => x.templateHash === v.token_template_hash?.toLowerCase())?.name;
    if (v.token && p && !out.has(v.token)) out.set(v.token, p as TokenProgram);
  }
  return out;
}

const big = (v: string | number | bigint | null | undefined): bigint => (v === null || v === undefined ? 0n : BigInt(v));

/**
 * Per token: the sum of the live, unspent stray UTXOs' token amounts, split into recoverable, where
 * the order is live, and lost.
 */
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
 * `liveOrders` are the wallet's live orders. List views are fine, because a missing custody UTXO
 * falls back to the amount the order's state must hold. The strays of the separate stray list
 * replace any stray numbers inside the order views, so nothing is counted twice.
 */
export function combineBalances(o: {
  maker: Hex;
  freeKas: bigint;
  freeTokens: readonly TokenUtxo[];
  liveOrders: readonly (OrderView | OrderSnapshot)[];
  strays: readonly StrayView[];
  /**
   * The token program of each token. It decides which KRON key-held type is spendable. When absent,
   * the family default applies.
   */
  programs?: ReadonlyMap<Hex, TokenProgram>;
  /** What the wallet's creator locks hold, per token. A token held only so still has its row. */
  locked?: readonly { token: Hex; amount: bigint; until: bigint }[];
}): CombinedBalances {
  // The separate stray list is authoritative. Snapshots, resolved from a node, never know strays.
  const noStrays = o.liveOrders.map((v) => ('covenant_id' in v ? { ...v, strays: undefined } : { ...v, strays: [] }));
  const balances = withFreeBalances(escrowedBalances(noStrays), [...o.freeTokens], o.maker, (t) => o.programs?.get(t));
  const totals = strayTotals(o.strays);
  const by = new Map(balances.map((b) => [b.token, { ...b, strays: 0n }]));
  let lost = 0n;
  for (const [token, t] of totals) {
    lost += t.lost;
    const cur = by.get(token) ?? { token, free: 0n, notSpendable: 0n, escrowed: 0n, bought: 0n, boughtOrders: 0, strays: 0n, kasLocked: 0n, kasEscrow: 0n, kasCarriers: 0n, orderCount: 0 };
    by.set(token, { ...cur, strays: t.recoverable });
  }
  for (const l of o.locked ?? []) {
    if (l.amount <= 0n) continue;
    const cur = by.get(l.token) ?? { token: l.token, free: 0n, notSpendable: 0n, escrowed: 0n, bought: 0n, boughtOrders: 0, strays: 0n, kasLocked: 0n, kasEscrow: 0n, kasCarriers: 0n, orderCount: 0 };
    // One lock per token, because its declaration names one. The earliest time stands for several.
    const was = cur.locked;
    by.set(l.token, { ...cur, locked: { amount: (was?.amount ?? 0n) + l.amount, until: was && was.until < l.until ? was.until : l.until } });
  }
  const tokens = [...by.values()].filter((b) => b.free > 0n || b.notSpendable > 0n || b.escrowed > 0n || b.bought > 0n || b.strays > 0n || b.kasLocked > 0n || b.orderCount > 0 || (b.locked?.amount ?? 0n) > 0n).sort((a, b) => (a.token < b.token ? -1 : a.token > b.token ? 1 : 0));
  const kas = kasBalanceSummary(o.freeKas, tokens);
  const onTokens = o.freeTokens.filter((u) => u.state.owner === o.maker).reduce((s, u) => s + big(u.amount), 0n);
  return { tokens, kas: { ...kas, onTokens, total: kas.total + onTokens }, lostStrayTokens: lost };
}

/**
 * The fixed fraction digits of the balance columns, which are the finest amount of each column so
 * the decimal points line up. They are taken over the tokens whose decimals are known (`decimalsOf`
 * returns `undefined` for a token without a registry entry). A token of unknown decimals is shown
 * in base units with no fraction, because padding its base units to another token's fraction
 * (`500,000.00000000`) would read as whole tokens.
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
    out.escrowed = Math.max(out.escrowed, unitsFraction(b.escrowed + b.bought, d), unitsFraction(b.bought, d));
    out.strays = Math.max(out.strays, unitsFraction(b.strays, d));
  }
  return out;
}
