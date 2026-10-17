// The wallet's own spends this page has submitted and the node may still list (it keeps an input in its UTXO set until the transaction is
// accepted into a block). Every planning environment drops these outpoints, so an order planned right after a submission (or from an
// environment read before it) never picks a coin that is already spent, whatever the age of the read it was planned from.
//
// The KAS change of those transactions (their plain P2PK outputs back to the key) counts as the key's funding at once, until the node lists it
// (a read made before the transaction reached a block does not): the balance does not drop to the coins left unspent in between, and the next
// order may spend the change (the node accepts a transaction spending an output of one in its mempool; `factOf` stands for the node's facts
// of such an input, which the page knows exactly: it built the transaction).
import { p2pkScriptPublicKey } from '../data/utxos';
import type { NodeInputFact } from '../kob/node-verify';
import type { Hex, KeyUtxo, TxJson, Utxo } from '../kob/types';

/** How long a submitted input stays excluded: far beyond acceptance (seconds); a transaction the network dropped frees its coins after this. */
export const PENDING_SPEND_TTL_MS = 3 * 60_000;

const key = (transactionId: string, index: number): string => `${transactionId.toLowerCase()}:${index}`;

interface OwnOutput {
  utxo: KeyUtxo;
  scriptPublicKey: string;
  until: number;
}

export class PendingSpends {
  private readonly spent = new Map<string, number>();
  /** the plain KAS outputs back to their key of the transactions submitted, by outpoint */
  private readonly change = new Map<string, OwnOutput>();
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

  /** The KAS change of transaction `tx` (accepted by the node as `txid`) back to `pubkey`: funding of that key until the node lists it. */
  addChange(tx: Pick<TxJson, 'outputs'>, txid: string, pubkey: Hex): void {
    const until = this.now() + PENDING_SPEND_TTL_MS;
    const spk = p2pkScriptPublicKey(pubkey);
    tx.outputs.forEach((o, index) => {
      if (o.covenant || o.scriptPublicKey.toLowerCase() !== spk) return;
      const utxo: KeyUtxo = { transactionId: txid.toLowerCase(), index, amount: o.value, covenantId: null, pubkey: pubkey.toLowerCase() };
      this.change.set(key(txid, index), { utxo, scriptPublicKey: o.scriptPublicKey, until });
    });
  }

  /** The node's facts of an input that is the change of a submitted transaction (null: not one, or no longer held). */
  factOf(transactionId: string, index: number): NodeInputFact | null {
    const c = this.live(key(transactionId, index));
    return c ? { amount: BigInt(c.utxo.amount), scriptPublicKey: c.scriptPublicKey, covenantId: null } : null;
  }

  /**
   * The funding of `pubkey` the node listed (`utxos`), without what a submitted transaction spends, with the change of the submitted ones the node
   * does not list (largest first, as the node's reader orders them; an older read lacking it gets it too, until the TTL).
   */
  funding<U extends KeyUtxo>(utxos: U[], pubkey: Hex): (U | KeyUtxo)[] {
    const kept = this.without(utxos);
    if (this.change.size === 0) return kept;
    const listed = new Set(utxos.map((u) => key(u.transactionId, u.index)));
    const pk = pubkey.toLowerCase();
    const extra: KeyUtxo[] = [];
    for (const [k, c] of [...this.change]) if (!listed.has(k) && this.live(k) && c.utxo.pubkey === pk && !this.has(c.utxo.transactionId, c.utxo.index)) extra.push(c.utxo);
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

  /** `utxos` without the ones a submitted transaction spends (the same array when none is). */
  without<U extends Pick<Utxo, 'transactionId' | 'index'>>(utxos: U[]): U[] {
    if (this.spent.size === 0) return utxos;
    const out = utxos.filter((u) => !this.has(u.transactionId, u.index));
    return out.length === utxos.length ? utxos : out;
  }
}

/** The page's one registry: a send that the node accepted adds to it (ui/confirm/send.ts), the environment builders read it. */
export const pendingSpends = new PendingSpends();
