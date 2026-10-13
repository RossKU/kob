// The traders' action weights (config `traders[].weights` and `token2.pairWeights` / `token3.pairWeights` override them per key; 0 switches an
// action off). Pure (no `@/` imports): read by bots/traders.ts and the unit tests.

export const DEFAULT_WEIGHTS: Record<string, number> = {
  market: 16,
  limit: 10,
  limitCross: 8,
  gtd: 5,
  day: 3,
  timed: 3,
  ioc: 8,
  fok: 4,
  streaming: 5,
  stopMarket: 6,
  stopLimit: 4,
  trailingStop: 4,
  takeProfit: 3,
  oco: 5,
  ifd: 4,
  ifo: 3,
  ifoStopEntry: 3,
  repeatIfd: 2,
  repeatIfo: 1,
  twap: 2,
  dca: 2,
  dutch: 2,
  close: 1,
  cancel: 7,
  amend: 5,
  cancelAll: 1,
  // scenarios of the 2026-10-13 gap review
  ladder: 3,
  ladderCancel: 2,
  renew: 2,
  mergeTokens: 1,
};

/**
 * The pair actions (an asset token A against TUSD B): every family of pair kinds. `limitSell` / `limitBuy` rest near the reference (and cross
 * each other: netting), `ioc` / `market` take (the route through the two KAS books, or resting pair orders), `stop` / `trailing` / `oco` are
 * KobCondPair orders the matcher arms from pair evidence (two KAS-book fills, or a resting pair order filled), `ifdBuy` / `ifdSell` / `repeatIfd`
 * KobIfdPair entries (buy-first and sell-first) whose fills create exits.
 */
export const DEFAULT_PAIR_WEIGHTS: Record<string, number> = {
  limitSell: 6,
  limitBuy: 6,
  ioc: 3,
  market: 3,
  dutch: 2,
  twap: 1,
  stop: 2,
  trailing: 2,
  oco: 2,
  ifdBuy: 2,
  ifdSell: 2,
  repeatIfd: 1,
  // the rest of the KAS ticket's order types, on the pair
  fok: 1,
  streaming: 1,
  close: 1,
  stopLimit: 1,
  takeProfit: 1,
  ifo: 1,
  ifoStopEntry: 1,
  repeatIfo: 1,
  cancel: 3,
  // scenarios of the 2026-10-13 gap review: dated pair limits, the pair amend (replace), the pair cancel-all
  gtd: 2,
  day: 1,
  timed: 2,
  amend: 2,
  cancelAll: 1,
};
