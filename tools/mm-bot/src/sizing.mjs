// Sizes as percentages of capitalUsd, converted at the live rates.

/** The cycle's sizes in whole units: { sizePerLevel, maxAskTokens, maxBidKas }. */
export function resolveSizing(capitalUsd, q, tokenUsd, kasUsd) {
  if (!(tokenUsd > 0) || !(kasUsd > 0)) throw new Error('sizing needs two positive USD rates');
  const usd = (pct) => (capitalUsd * pct) / 100;
  return {
    sizePerLevel: usd(q.levelSizePct) / tokenUsd,
    maxAskTokens: usd(q.maxAskPct) / tokenUsd,
    maxBidKas: usd(q.maxBidPct) / kasUsd,
  };
}

/**
 * The `quotes` config with the cycle's sizes filled in (what ladder.mjs, inventory.mjs and
 * funds.mjs take).
 */
export const sizedQuotes = (capitalUsd, q, tokenUsd, kasUsd) => ({ ...q, ...resolveSizing(capitalUsd, q, tokenUsd, kasUsd) });

/** A share of the capital base, in percent. */
export const pctOf = (usd, capitalUsd) => (capitalUsd > 0 ? (usd / capitalUsd) * 100 : 0);

/** The inventory in USD and % of the capital, with a one-line text. */
export function inventoryUsd(tokens, kas, rates, capitalUsd) {
  const t = tokens * rates.tokenUsd;
  const k = kas * rates.kasUsd;
  const r = (x) => Math.round(x).toLocaleString('en-US');
  const p = (x) => `${Math.round(pctOf(x, capitalUsd))}%`;
  return {
    tokenUsd: t, kasUsd: k, totalUsd: t + k,
    tokenPct: pctOf(t, capitalUsd), kasPct: pctOf(k, capitalUsd), totalPct: pctOf(t + k, capitalUsd),
    text: `token ${r(t)} USD (${p(t)}), KAS ${r(k)} USD (${p(k)}), total ${r(t + k)} USD (${p(t + k)}) of ${r(capitalUsd)} USD`,
  };
}
