export const sleep = (ms: number) => new Promise<void>((r) => setTimeout(r, ms));
export const KAS = 100_000_000n;
export const kas = (sompi: bigint | string | number): string => (Number(BigInt(sompi)) / 1e8).toFixed(4);
/** exponential inter-arrival with the given mean, clamped */
export const expDelay = (meanSec: number, min = 1, max = meanSec * 5): number => Math.min(max, Math.max(min, -Math.log(1 - Math.random()) * meanSec)) * 1000;
export const pick = <T>(xs: readonly T[]): T => xs[Math.floor(Math.random() * xs.length)];
export const randInt = (lo: number, hi: number): number => lo + Math.floor(Math.random() * (hi - lo + 1));
/** weighted choice over `weights` (name -> weight) */
export function weighted(weights: Record<string, number>): string {
  const entries = Object.entries(weights).filter(([, w]) => w > 0);
  const total = entries.reduce((s, [, w]) => s + w, 0);
  let r = Math.random() * total;
  for (const [k, w] of entries) {
    r -= w;
    if (r <= 0) return k;
  }
  return entries[entries.length - 1][0];
}
/** rounds `v` to a multiple of `tick` (nearest, at least one tick) */
export const onTick = (v: bigint, tick: bigint): bigint => {
  const r = ((v + tick / 2n) / tick) * tick;
  return r < tick ? tick : r;
};
export const bps = (v: bigint, b: number): bigint => (v * BigInt(Math.round(b * 100))) / 1_000_000n;

/** whole-token decimal text ("0.0005") -> base units (exact; more fraction digits than `decimals` is an error) */
export function unitsOf(text: string, decimals: number): bigint {
  const m = /^(\d+)(?:\.(\d+))?$/.exec(text.trim());
  if (!m) throw new Error(`not a decimal amount: ${text}`);
  const frac = m[2] ?? '';
  if (frac.length > decimals) throw new Error(`${text} has more than ${decimals} fraction digits`);
  return BigInt(m[1]) * 10n ** BigInt(decimals) + BigInt(frac.padEnd(decimals, '0') || '0');
}

/** the base units of `usd` dollars of a token worth `tokenUsd` per whole token (rounded down, at least 1) */
export function usdToUnits(usd: number, tokenUsd: number, decimals: number): bigint {
  if (!(usd > 0) || !(tokenUsd > 0)) throw new Error('usdToUnits: positive amounts only');
  // 12 significant digits of the whole-token amount, then exact scaling (no float above 2^53)
  const whole = usd / tokenUsd;
  const exp = Math.floor(Math.log10(whole));
  const digits = 12 - exp;
  const mant = BigInt(Math.round(whole * 10 ** Math.min(digits, 300)));
  const shift = decimals - Math.min(digits, 300);
  const v = shift >= 0 ? mant * 10n ** BigInt(shift) : mant / 10n ** BigInt(-shift);
  return v < 1n ? 1n : v;
}

/** a uniformly random amount of base units in [lo, hi] (any amount: protocol v3 orders are not rounded to a step) */
export function randUnits(lo: bigint, hi: bigint): bigint {
  if (hi <= lo) return lo;
  const span = hi - lo + 1n;
  // 53 random bits per draw are plenty for the soak's ranges; larger spans are scaled
  const r = BigInt(Math.floor(Math.random() * 2 ** 53));
  return lo + (span <= 1n << 53n ? r % span : (span * r) >> 53n);
}
