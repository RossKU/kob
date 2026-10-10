// Pure helpers of the bots' own-transaction bookkeeping (no `@/` imports: unit-tested with plain `node --test`).
//
// The indexer the bots read lags the chain (1 to 3 minutes under the TN10 flood): an order a bot just cancelled or amended is still listed
// with its old UTXO, and an order it just placed is not listed yet. Planning from that listing alone re-cancels / re-amends the same order
// (the node answers "already has this transaction" or "does not know one of the inputs"), retries a self-trade refusal against an order
// already being cancelled, and places a market maker's level again and again. A wallet knows its own pending transactions: these helpers
// apply them to the listing.

/** "txid:index" of an order view's current UTXO, or null when the view has none */
export function outpointOf(v: { current?: { txid: string; index: number } | null }): string | null {
  return v.current ? `${v.current.txid}:${v.current.index}` : null;
}

/**
 * Splits an order listing into the orders the wallet may act on and those whose current UTXO one of its own submitted transactions already
 * spends (`reserved`: the wallet's reserved outpoints; a cancel or amend not yet indexed, or a matcher's fill the mempool already holds).
 */
export function splitPending<T extends { current?: { txid: string; index: number } | null }>(views: readonly T[], reserved: ReadonlySet<string>): { live: T[]; pending: T[] } {
  const live: T[] = [];
  const pending: T[] = [];
  for (const v of views) {
    const k = outpointOf(v);
    (k !== null && reserved.has(k) ? pending : live).push(v);
  }
  return { live, pending };
}

/** The own resting orders a SELF_TRADE refusal names (one issue per crossing order), in order, without repeats. */
export function selfTradeIds(issues: readonly { code: string; params?: Record<string, unknown> }[]): string[] {
  const out: string[] = [];
  for (const i of issues) {
    if (i.code !== 'SELF_TRADE') continue;
    const id = i.params?.ownCovenantId;
    if (typeof id === 'string' && id && !out.includes(id)) out.push(id);
  }
  return out;
}

/**
 * Runs async sections one at a time, in call order. One per wallet: the market maker's three books, the fan-out and the merges of one key
 * run concurrently, and two plans read before either submits would pick the same funding UTXO ("already being spent by another
 * transaction"). A section that throws does not block the next one. Not re-entrant: a section must not wait for another of the same lock.
 */
export class Mutex {
  private tail: Promise<unknown> = Promise.resolve();

  run<T>(fn: () => Promise<T>): Promise<T> {
    // `tail` never rejects (the catch below), so `fn` starts once the previous section has settled either way
    const next = this.tail.then(fn);
    this.tail = next.catch(() => undefined);
    return next;
  }
}

/** a `getMempoolEntriesByAddresses` answer as the SDK returns it (only the fields read here) */
export interface MempoolEntriesLike {
  entries: { sending: { transaction: { inputs: { previousOutpoint: { transactionId: string; index: number } }[] } }[] }[];
}

/**
 * The outpoints ("txid:index") the node's mempool already spends in transactions sent from the queried addresses. Under a flood a
 * transaction at the normal fee rate can wait in the mempool far longer than the wallet's reservation (10-11: 45 to 60 minutes), and the
 * UTXO index lists its inputs all that time: re-spending one is refused ("already being spent by another transaction").
 */
export function mempoolSpent(r: MempoolEntriesLike): Set<string> {
  const out = new Set<string>();
  for (const e of r.entries ?? []) for (const s of e.sending ?? []) for (const i of s.transaction?.inputs ?? []) out.add(`${i.previousOutpoint.transactionId}:${i.previousOutpoint.index}`);
  return out;
}
