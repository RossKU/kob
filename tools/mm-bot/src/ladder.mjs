// The target ladder. spreadPct is the whole bid-ask gap, so each best level sits spreadPct / 2 from fair.

/** The KAS (sompi) a bid of `amount` base units at `price` sompi per whole token escrows, without its carrier. */
export const bidCost = (amount, price, scale) => Math.ceil((amount * price) / scale);

/** Level i's distance from the fair value, a fraction: half the spread plus i steps. */
export const levelOffset = (q, i) => q.spreadPct / 200 + (i * q.levelStepPct) / 100;

/** The KAS (sompi) an order of `amount` base units is worth at `price`. */
export const orderValue = (amount, price, scale) => Math.floor((amount * price) / scale);

/** The target ladders, inside out: levels { side, level, price (sompi per token), amount (base units) }. */
export function buildLadder(fair, budgets, q, scale, shift = 0) {
  if (!(fair > 0)) throw new Error('the ladder needs a positive fair value');
  const size = Math.floor(q.sizePerLevel * scale);
  const minValue = Math.floor(q.minOrderKas * 1e8);
  const fairSompi = fair * 1e8;
  const asks = [];
  const bids = [];

  let tokensLeft = budgets.askTokens;
  for (let i = 0; i < q.levels && tokensLeft > 0; i++) {
    const price = Math.ceil(fairSompi * (1 + levelOffset(q, i)) * (1 + shift));
    const amount = Math.min(size, tokensLeft);
    if (orderValue(amount, price, scale) < minValue) break;
    asks.push({ side: 'sell', level: i, price, amount });
    tokensLeft -= amount;
  }

  let kasLeft = budgets.bidKas;
  for (let i = 0; i < q.levels && kasLeft > 0; i++) {
    const price = Math.floor(fairSompi * (1 - levelOffset(q, i)) * (1 + shift));
    if (price <= 0) break;
    const amount = Math.min(size, Math.floor((kasLeft * scale) / price));
    if (amount <= 0 || orderValue(amount, price, scale) < minValue) break;
    bids.push({ side: 'buy', level: i, price, amount });
    kasLeft -= bidCost(amount, price, scale);
  }

  assertNoSelfCross(bids, asks);
  return { bids, asks };
}

/** Refuses a ladder whose best bid is at or above its best ask: the bot would trade with itself. */
export function assertNoSelfCross(bids, asks) {
  if (!bids.length || !asks.length) return;
  const bestBid = Math.max(...bids.map((b) => b.price));
  const bestAsk = Math.min(...asks.map((a) => a.price));
  if (bestBid >= bestAsk) throw new Error(`self-cross refused: best bid ${bestBid} >= best ask ${bestAsk} sompi`);
}
