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
