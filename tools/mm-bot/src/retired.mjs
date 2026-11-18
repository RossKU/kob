// Retired token instances: recovery only.

/**
 * Cancel the orders (again after resendAfterMs), then merge; done with no order and at most one
 * UTXO.
 */
export function retiredPlan(live, freeUtxos, nowMs, resendAfterMs) {
  const actions = live
    .filter((o) => !(o.cancelSentAt != null && nowMs - o.cancelSentAt < resendAfterMs))
    .map((o) => ({ kind: 'cancel', id: o.id, side: o.side, reason: 'retired instance' }));
  if (live.length === 0 && freeUtxos > 1) actions.push({ kind: 'merge', reason: `retired instance: ${freeUtxos} UTXOs` });
  return { actions, done: live.length === 0 && freeUtxos <= 1 };
}

/** The log line once a retired instance is cleaned up. */
export function retiredMessage(id, freeBase, decimals) {
  const amount = decimals == null ? `${freeBase} base units of the token` : `${(freeBase / 10 ** decimals).toLocaleString('en-US', { maximumFractionDigits: decimals })} tokens`;
  return `retired instance ${id}: quotes pulled, ${amount} back in the wallet (moving them through the bridge is manual).`;
}
