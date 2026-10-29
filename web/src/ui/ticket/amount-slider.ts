// The amount slider of the order ticket: 0 to 100 % of what "Max" gives (max-amount.ts: a sell the free token balance, a buy the largest amount the
// balance covers after carriers, fees and the reserve), with snap points at 0 / 25 / 50 / 75 / 100 % and 1 % steps between them. Pure: amounts are
// token base units (so a percent of Max is rounded down to a whole base unit whatever the token's decimals), percents whole numbers 0..100.
//
// The order's minimum fill: a point whose amount would be below it is disabled (it shows why), and a drag into that range sets the minimum fill
// itself (the smallest amount the order can have); 0 % empties the amount box.

/** The snap points, percent of Max. */
export const SLIDER_POINTS = [0, 25, 50, 75, 100] as const;

/** A drag within this many percent of a tick snaps to it. */
export const SNAP_RANGE = 3;

/** Page Up / Page Down move the slider by this many percent. */
export const PAGE_STEP = 25;

/** The magnetic snap of a dragged thumb: `v` percent, or the tick within SNAP_RANGE percent of it. */
export function snapPct(v: number): number {
  for (const p of SLIDER_POINTS) if (Math.abs(v - p) <= SNAP_RANGE) return p;
  return v;
}

/** `pct` percent of `max`, rounded down to a whole base unit (100 % is exactly `max`). */
export function percentAmount(max: bigint, pct: number): bigint {
  if (max <= 0n || pct <= 0) return 0n;
  if (pct >= 100) return max;
  return (max * BigInt(Math.floor(pct))) / 100n;
}

/** The slider position of a typed amount: the nearest whole percent of `max` (0..100; above Max reads 100); null when there is no Max. */
export function amountPercent(amount: bigint | null, max: bigint | null): number | null {
  if (max === null || max <= 0n) return null;
  if (amount === null || amount <= 0n) return 0;
  if (amount >= max) return 100;
  const p = Number((amount * 200n + max) / (2n * max));
  return Math.min(100, Math.max(0, p));
}

/** Whether snap point `pct` is usable: its amount is not below the order's minimum fill (0 % always is: it empties the amount). */
export function pointEnabled(pct: number, max: bigint | null, minFill: bigint | null): boolean {
  if (max === null || max <= 0n) return false;
  if (pct <= 0) return true;
  const a = percentAmount(max, pct);
  return a > 0n && (minFill === null || a >= minFill);
}

/**
 * The amount (base units) the slider sets at `pct`: null = empty the box (0 %, or nothing to place). Below the minimum fill it is the minimum fill
 * (when Max allows it at all).
 */
export function pickAmount(pct: number, max: bigint | null, minFill: bigint | null): bigint | null {
  if (max === null || max <= 0n || pct <= 0) return null;
  const a = percentAmount(max, pct);
  if (minFill !== null && minFill > 0n && a < minFill) return minFill <= max ? minFill : null;
  return a > 0n ? a : null;
}
