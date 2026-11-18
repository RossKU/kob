// The depth within 2 % of the price, per side. It is the one definition the web app computes
// itself, for a standalone or node-fed market (`stream-market.ts`). It is the same one the
// indexer's `/v1/stats` sends as `depth_2pct_bid_sompi` and `depth_2pct_ask_sompi`
// (crates/kobd/src/indexer/market.rs `depth_bands`). It is pure, exact integer math in sompi.

/**
 * One merged level of a side, with the price in sompi per `basis` base units and the amount in
 * base units.
 */
export interface BandLevel {
  price: bigint;
  amount: bigint;
}

/** The band around the reference price in percent, as CoinGecko's depth uses it. */
export const DEPTH_BAND_PERCENT = 2n;

/**
 * What the bids at or above 98 % of the reference price and the asks at or below 102 % of it are
 * worth, in sompi.
 *
 * Each level is worth amount x price / `basis`, and both bounds are inclusive. The reference is
 * the mid of the best bid and the best ask, or the one side that exists. The sides are best
 * first. A missing side is 0. It is null when there is no reference at all, as for an empty book
 * or a price that is not positive.
 */
export function depth2pctSides(asks: readonly BandLevel[], bids: readonly BandLevel[], basis: bigint): { ref: bigint; bid: bigint; ask: bigint } | null {
  const bestBid = bids[0]?.price;
  const bestAsk = asks[0]?.price;
  const ref = bestBid !== undefined && bestAsk !== undefined ? (bestAsk + bestBid) / 2n : (bestBid ?? bestAsk);
  if (ref === undefined || ref <= 0n || basis <= 0n) return null;
  let ask = 0n;
  let bid = 0n;
  for (const m of asks) if (m.price * 100n <= ref * (100n + DEPTH_BAND_PERCENT)) ask += (m.amount * m.price) / basis;
  for (const m of bids) if (m.price * 100n >= ref * (100n - DEPTH_BAND_PERCENT)) bid += (m.amount * m.price) / basis;
  return { ref, bid, ask };
}
