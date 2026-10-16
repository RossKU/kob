// The wallet's own spends this page has submitted and the node may still list (it keeps an input in its UTXO set until the transaction is
// accepted into a block). Every planning environment drops these outpoints, so an order planned right after a submission (or from an
// environment read before it) never picks a coin that is already spent, whatever the age of the read it was planned from.
import type { Utxo } from '../kob/types';

/** How long a submitted input stays excluded: far beyond acceptance (seconds); a transaction the network dropped frees its coins after this. */
export const PENDING_SPEND_TTL_MS = 3 * 60_000;

const key = (transactionId: string, index: number): string => `${transactionId.toLowerCase()}:${index}`;

export class PendingSpends {
  private readonly spent = new Map<string, number>();
  /** when the newest submission was made (0: none on this page) */
  lastAddedAt = 0;
  constructor(private readonly now: () => number = Date.now) {}

  /** The inputs of a transaction the node just accepted into its mempool. */
  add(inputs: readonly { transactionId: string; index: number }[]): void {
    this.lastAddedAt = this.now();
    const until = this.lastAddedAt + PENDING_SPEND_TTL_MS;
    for (const i of inputs) this.spent.set(key(i.transactionId, i.index), until);
  }

  has(transactionId: string, index: number): boolean {
    const k = key(transactionId, index);
    const until = this.spent.get(k);
    if (until === undefined) return false;
    if (until > this.now()) return true;
    this.spent.delete(k);
    return false;
  }

  /** `utxos` without the ones a submitted transaction spends (the same array when none is). */
  without<U extends Pick<Utxo, 'transactionId' | 'index'>>(utxos: U[]): U[] {
    if (this.spent.size === 0) return utxos;
    const out = utxos.filter((u) => !this.has(u.transactionId, u.index));
    return out.length === utxos.length ? utxos : out;
  }
}

/** The page's one registry: a send that the node accepted adds to it (ui/confirm/send.ts), the environment builders read it. */
export const pendingSpends = new PendingSpends();
