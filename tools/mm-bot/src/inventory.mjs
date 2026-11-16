// Inventory limits and the optional skew, in base units and sompi.

/** What each side may commit: free plus the own orders, capped, the KAS reserve kept back. */
export function sideBudgets(bal, mine, q, scale) {
  const askCap = Math.floor(q.maxAskTokens * scale);
  const bidCap = Math.floor(q.maxBidKas * 1e8);
  const reserve = Math.floor(q.kasReserve * 1e8);
  const askTokens = Math.max(0, Math.min(askCap, bal.tokenFree + mine.askTokens));
  const bidKas = Math.max(0, Math.min(bidCap, bal.kasFree + mine.bidKas - reserve));
  return { askTokens, bidKas };
}

/** A price shift against the inventory, at most maxShift; 0 when the skew is off. */
export function skewShift(tokenValueKas, kasValue, skew) {
  if (!skew?.enabled || !(skew.maxShift > 0)) return 0;
  const total = tokenValueKas + kasValue;
  if (!(total > 0)) return 0;
  const share = tokenValueKas / total;
  const target = skew.targetTokenShare ?? 0.5;
  const span = share > target ? 1 - target : target;
  const x = span > 0 ? (share - target) / span : 0; // -1 (no token) .. +1 (all token)
  return -skew.maxShift * Math.max(-1, Math.min(1, x)) || 0; // || 0: no -0
}
