// A click on the depth chart as the order it prefills in the ticket: the chart's x is a price (a float, chart coordinates), the ticket takes a state price
// (a bigint) and a side in NATIVE terms, exactly what a click on a book row prefills (OrderBook `onPick`, PairBook `pickOf`). The side is the book's rule, an
// ask a buy and a bid a sell; on a flipped (KAS/TOKEN) market the chart's bid side is the native ask side, so there the same rule reads the other way round,
// as it does in the flipped book (`BookRowView.pick`). The price is rounded so the order reaches what the pointer points at: a buy up, a sell down (as
// `pickOf`), to the market's tick where it has one (the steps the book's grouping is made of).

/** A positive float as an exact ratio of its 15 significant digits (a float is a chart coordinate: more digits are noise). Null for anything else. */
function floatRatio(x: number): { num: bigint; den: bigint } | null {
  if (!Number.isFinite(x) || x <= 0) return null;
  const [m, e] = x.toExponential(14).split('e');
  const mant = BigInt(m!.replace('.', ''));
  const exp = Number(e) - 14;
  return exp >= 0 ? { num: mant * 10n ** BigInt(exp), den: 1n } : { num: mant, den: 10n ** BigInt(-exp) };
}

const ceilDiv = (a: bigint, b: bigint): bigint => (a + b - 1n) / b;

/** `num / den` as a whole multiple of `step` (at least one step), rounded up or down. */
function onStep(num: bigint, den: bigint, step: bigint, mode: 'up' | 'down'): bigint {
  const k = mode === 'up' ? ceilDiv(num, den * step) : num / (den * step);
  return (k > 0n ? k : 1n) * step;
}

export interface DepthPick {
  side: 'buy' | 'sell';
  price: bigint;
}

/**
 * The prefill of a click at `price` (the chart's axis: KAS per token, or tokens per KAS on a flipped market) on the `side` of the mid it fell on. `scale`
 * and `decimals` are the market's (a state price is sompi per `scale` base units), `tick` its price step (1 where it has none). Null when the price is no
 * positive number.
 */
export function tokenDepthPick(side: 'bid' | 'ask', price: number, c: { inverted: boolean; decimals: number; scale: bigint; tick: bigint }): DepthPick | null {
  const r = floatRatio(price);
  if (!r) return null;
  const buy = (side === 'ask') !== c.inverted;
  // KAS per whole token (the flipped axis is its inverse) as sompi per `scale` base units: x 1e8 x scale / 10^decimals
  const k = 10n ** 8n * c.scale;
  const d = 10n ** BigInt(c.decimals);
  const num = c.inverted ? r.den * k : r.num * k;
  const den = c.inverted ? r.num * d : r.den * d;
  return { side: buy ? 'buy' : 'sell', price: onStep(num, den, c.tick > 0n ? c.tick : 1n, buy ? 'up' : 'down') };
}

/**
 * The same for a token/token pair: `price` is whole QUOTE per whole BASE, the state price QUOTE base units per `base.scale` BASE base units (`pickOf`). The
 * pair ticket has no tick of its own here, as the pair book has none.
 */
export function pairDepthPick(side: 'bid' | 'ask', price: number, base: { decimals: number; scale: bigint }, quote: { decimals: number }): DepthPick | null {
  const r = floatRatio(price);
  if (!r) return null;
  const buy = side === 'ask';
  return { side: buy ? 'buy' : 'sell', price: onStep(r.num * 10n ** BigInt(quote.decimals) * base.scale, r.den * 10n ** BigInt(base.decimals), 1n, buy ? 'up' : 'down') };
}
