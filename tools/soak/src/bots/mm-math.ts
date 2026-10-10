// Pure ladder arithmetic of the market maker (no `@/` imports: unit-tested with plain `node --test`).

/**
 * How far (bps) a resting level may drift from where it belongs before it is amended. Without `requoteMinBps`: `requoteBps` for every level
 * (the wide ladder of the first runs). With it (a tight ladder, near-zero spreads): the level's own distance from the mid, at least
 * `requoteMinBps` and at most `requoteBps`, so an inner quote is moved before the reference passes it while the outer ones rarely move.
 */
export function requoteThreshold(c: { innerBps: number; stepBps: number; requoteBps: number; requoteMinBps?: number }, rank: number): number {
  if (c.requoteMinBps === undefined) return c.requoteBps;
  return Math.min(c.requoteBps, Math.max(c.requoteMinBps, c.innerBps + rank * c.stepBps));
}

/** a level the ladder wants: its rank (0 = innermost), price (sompi per whole token) and size (base units) */
export interface LadderTarget {
  rank: number;
  price: bigint;
  amount: bigint;
}

/**
 * A resting order of one side as the ladder sees it: `order` is what the market maker may amend or cancel, null for a HELD slot (an order of
 * its own pending transaction: just placed and not listed yet, or amended and listed with its old UTXO). A held slot keeps its level (no
 * duplicate placement) and is never acted on until the indexer catches up.
 */
export interface LadderSlot<O> {
  price: bigint;
  order: O | null;
}

export type LadderStep<O> = { kind: 'place'; target: LadderTarget } | { kind: 'amend'; order: O; target: LadderTarget } | { kind: 'cancel'; order: O };

/**
 * The transactions one side of the ladder needs this tick, at most `budget`: resting orders (best first) are matched to the targets by rank; a
 * missing level is placed, an order off its level by more than `thresholdBps(rank)` is amended, orders beyond the ladder are cancelled. Held
 * slots take their level and cost nothing.
 */
export function ladderSteps<O>(side: 'buy' | 'sell', targets: readonly LadderTarget[], slots: readonly LadderSlot<O>[], thresholdBps: (rank: number) => number, budget: number): LadderStep<O>[] {
  const have = [...slots].sort((a, b) => (side === 'buy' ? (b.price > a.price ? 1 : b.price < a.price ? -1 : 0) : a.price > b.price ? 1 : a.price < b.price ? -1 : 0));
  const out: LadderStep<O>[] = [];
  for (let i = 0; i < targets.length && out.length < budget; i++) {
    const t = targets[i]!;
    const cur = have[i];
    if (!cur) {
      out.push({ kind: 'place', target: t });
      continue;
    }
    if (cur.order === null || t.price <= 0n) continue;
    const off = Math.abs(Number(cur.price - t.price)) / Number(t.price);
    if (off * 10_000 > thresholdBps(t.rank)) out.push({ kind: 'amend', order: cur.order, target: t });
  }
  for (const extra of have.slice(targets.length)) {
    if (out.length >= budget) break;
    if (extra.order !== null) out.push({ kind: 'cancel', order: extra.order });
  }
  return out;
}
