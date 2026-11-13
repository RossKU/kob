// The wallet's own spends that this page has submitted and the node may still list. The node keeps
// an input in its UTXO set until the transaction is accepted into a block. Every planning
// environment drops these outpoints, so an order planned right after a submission, or from an
// environment read before it, never picks a coin that is already spent, whatever the age of the
// read it was planned from.
//
// The KAS change of those transactions, their plain P2PK outputs back to the key, counts as the
// key's funding at once, until the node lists it. A read made before the transaction reached a
// block does not list it. So the balance does not drop to the coins left unspent in between, and
// the next order may spend the change. The node accepts a transaction that spends an output of
// one in its mempool. `factOf` stands for the node's facts of such an input, which the page knows
// exactly because it built the transaction.
//
// The preparation output 0 of an unfinished token creation (data/creations.ts) is reserved for its
// genesis. While its record exists it never funds anything else, on any page; the Create token page
// offers to finish it.
import { reservedCreationOutpoints } from '../data/creations';
import { p2pkScriptPublicKey } from '../data/utxos';
import type { NodeInputFact } from '../kob/node-verify';
import type { Hex, KeyUtxo, TxJson, Utxo } from '../kob/types';

/**
 * How long a submitted input stays excluded. It is far beyond acceptance, which takes seconds. A
 * transaction the network dropped frees its coins after this.
 */
export const PENDING_SPEND_TTL_MS = 3 * 60_000;

const key = (transactionId: string, index: number): string => `${transactionId.toLowerCase()}:${index}`;

interface OwnOutput {
  utxo: KeyUtxo;
  scriptPublicKey: string;
  until: number;
}

/** The submitted spends of this page, and the change they return to the wallet's key. */
export class PendingSpends {
  private readonly spent = new Map<string, number>();
  /** The plain KAS outputs back to their key of the transactions submitted, by outpoint. */
  private readonly change = new Map<string, OwnOutput>();
  /** When the newest submission was made. 0 means none on this page. */
  lastAddedAt = 0;
  /** `reserved`: outpoints (`txid:index`, lower case) that are never funding (default: none; the page's registry: the unfinished creations) */
  constructor(private readonly now: () => number = Date.now, private readonly reserved: () => ReadonlySet<string> = () => new Set()) {}

  /** The inputs of a transaction the node just accepted into its mempool. */
  add(inputs: readonly { transactionId: string; index: number }[]): void {
    this.lastAddedAt = this.now();
    const until = this.lastAddedAt + PENDING_SPEND_TTL_MS;
    for (const i of inputs) this.spent.set(key(i.transactionId, i.index), until);
  }

  /** Whether a submitted transaction spends this outpoint and the exclusion has not expired. */
  has(transactionId: string, index: number): boolean {
    const k = key(transactionId, index);
    const until = this.spent.get(k);
    if (until === undefined) return false;
    if (until > this.now()) return true;
    this.spent.delete(k);
    return false;
  }

  /**
   * The KAS change of transaction `tx`, accepted by the node as `txid`, back to `pubkey`. It is
   * funding of that key until the node lists it.
   */
  addChange(tx: Pick<TxJson, 'outputs'>, txid: string, pubkey: Hex): void {
    const until = this.now() + PENDING_SPEND_TTL_MS;
    const spk = p2pkScriptPublicKey(pubkey);
    tx.outputs.forEach((o, index) => {
      if (o.covenant || o.scriptPublicKey.toLowerCase() !== spk) return;
      const utxo: KeyUtxo = { transactionId: txid.toLowerCase(), index, amount: o.value, covenantId: null, pubkey: pubkey.toLowerCase() };
      this.change.set(key(txid, index), { utxo, scriptPublicKey: o.scriptPublicKey, until });
    });
  }

  /**
   * The node's facts of an input that is the change of a submitted transaction. Null when it is
   * not one or is no longer held.
   */
  factOf(transactionId: string, index: number): NodeInputFact | null {
    const c = this.live(key(transactionId, index));
    return c ? { amount: BigInt(c.utxo.amount), scriptPublicKey: c.scriptPublicKey, covenantId: null } : null;
  }

  /**
   * The funding of `pubkey` that the node listed (`utxos`), without what a submitted transaction
   * spends, plus the change of the submitted ones that the node does not list. The result is
   * largest first, as the node's reader orders it, so an older read that lacks the change gets it
   * too, until the TTL.
   */
  funding<U extends KeyUtxo>(utxos: U[], pubkey: Hex): (U | KeyUtxo)[] {
    let reserved: ReadonlySet<string>;
    try {
      reserved = this.reserved();
    } catch {
      reserved = new Set();
    }
    const free = <T extends Pick<Utxo, 'transactionId' | 'index'>>(xs: T[]): T[] => (reserved.size === 0 ? xs : xs.filter((u) => !reserved.has(key(u.transactionId, u.index))));
    const kept = free(this.without(utxos));
    if (this.change.size === 0) return kept;
    const listed = new Set(utxos.map((u) => key(u.transactionId, u.index)));
    const pk = pubkey.toLowerCase();
    const extra: KeyUtxo[] = [];
    for (const [k, c] of [...this.change]) if (!listed.has(k) && !reserved.has(k) && this.live(k) && c.utxo.pubkey === pk && !this.has(c.utxo.transactionId, c.utxo.index)) extra.push(c.utxo);
    if (!extra.length) return kept;
    return [...kept, ...extra].sort((a, b) => {
      const d = BigInt(b.amount) - BigInt(a.amount);
      if (d !== 0n) return d > 0n ? 1 : -1;
      return a.transactionId === b.transactionId ? a.index - b.index : a.transactionId < b.transactionId ? -1 : 1;
    });
  }

  private live(k: string): OwnOutput | null {
    const c = this.change.get(k);
    if (!c) return null;
    if (c.until > this.now()) return c;
    this.change.delete(k);
    return null;
  }

  /** `utxos` without the ones a submitted transaction spends. It is the same array when none is. */
  without<U extends Pick<Utxo, 'transactionId' | 'index'>>(utxos: U[]): U[] {
    if (this.spent.size === 0) return utxos;
    const out = utxos.filter((u) => !this.has(u.transactionId, u.index));
    return out.length === utxos.length ? utxos : out;
  }
}

/**
 * The page's one registry. A send that the node accepted adds to it (ui/confirm/send.ts), and the
 * environment builders read it. The unfinished creations are reserved.
 */
export const pendingSpends = new PendingSpends(Date.now, () => reservedCreationOutpoints());
