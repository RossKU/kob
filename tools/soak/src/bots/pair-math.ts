// Pure pair arithmetic of the pair-order bots (no `@/` imports: unit-tested with plain `node --test`).

export interface Rat {
  num: bigint;
  den: bigint;
}

const gcd = (a: bigint, b: bigint): bigint => {
  let x = a < 0n ? -a : a;
  let y = b < 0n ? -b : b;
  while (y) [x, y] = [y, x % y];
  return x;
};

export function reduceRat(r: Rat): Rat {
  const g = gcd(r.num, r.den) || 1n;
  return { num: r.num / g, den: r.den / g };
}

/**
 * The fair pair price, whole QUOTE tokens per whole BASE token, from the KAS references of the two tokens (sompi per whole token each):
 * perTokenBase / perTokenQuote.
 */
export function fairPairPrice(base: { perToken: bigint }, quote: { perToken: bigint }): Rat {
  return reduceRat({ num: base.perToken, den: quote.perToken });
}

/** r x (1 + b / 10^4), exact to 1/100 bps */
export const withBps = (r: Rat, b: number): Rat => reduceRat({ num: r.num * BigInt(1_000_000 + Math.round(b * 100)), den: r.den * 1_000_000n });

/** a rational as a decimal string with at most `digits` fraction digits, rounded `down` or `up` (trailing zeros trimmed) */
export function ratText(r: Rat, digits: number, round: 'down' | 'up'): string {
  const s = 10n ** BigInt(digits);
  const scaled = round === 'up' ? (r.num * s + r.den - 1n) / r.den : (r.num * s) / r.den;
  const whole = scaled / s;
  const frac = (scaled % s).toString().padStart(digits, '0').replace(/0+$/, '');
  return frac ? `${whole}.${frac}` : whole.toString();
}

/**
 * The rate two KAS references imply, B base units per WHOLE A (a pair order's price unit): `perTokenA x scale(B) / perTokenB` (sompi per whole A
 * over sompi per whole B), rounded down; null without a positive B reference.
 */
export function pairRateOf(perTokenA: bigint, perTokenB: bigint, scaleB: bigint): bigint | null {
  if (perTokenA <= 0n || perTokenB <= 0n || scaleB <= 0n) return null;
  return (perTokenA * scaleB) / perTokenB;
}

/** One level of the indexer's pair book (`price_num / price_den` B base units per A base unit). */
export interface PairLevelLike {
  price_num: string;
  price_den: string;
}

/**
 * The pair book's reference rate in B base units per whole A: the midpoint of its best ask and best bid (any source: direct, entry, route; one
 * side when only one exists), when it lies within `maxBps` of `fair`; null otherwise (a thin or stale book: the caller uses the fair rate).
 */
export function pairReference(view: { asks: PairLevelLike[]; bids: PairLevelLike[] } | null, scaleA: bigint, fair: bigint, maxBps: number): bigint | null {
  if (!view) return null;
  const whole = (l: PairLevelLike | undefined): bigint | null => {
    if (!l) return null;
    const num = BigInt(l.price_num);
    const den = BigInt(l.price_den);
    return den > 0n ? (num * scaleA) / den : null;
  };
  const a = whole(view.asks[0]);
  const b = whole(view.bids[0]);
  const mid = a !== null && b !== null ? (a + b) / 2n : (a ?? b);
  if (mid === null || mid <= 0n) return null;
  const dist = mid > fair ? mid - fair : fair - mid;
  return dist * 10_000n <= fair * BigInt(Math.round(maxBps)) ? mid : null;
}
