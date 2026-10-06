// In-memory `NodeApi` for unit tests of everything above the node (UTXO services, the token tracker, the signing pipeline, planners).
// Behaves like a small node: a settable UTXO set, a settable clock, recorded submissions, and (by default) a submitted transaction
// SPENDS its inputs and CREATES its outputs, so tests can follow a chain of transactions. Failure injection: `rejectNext`.
import type { FeeEstimate } from '../kob/fee-policy';
import type { Hex, TxJson } from '../kob/types';
import type { NodeApi, NodeInfo, NodeUtxo } from '../data/node';
import { NodeError, describeNodeError } from '../data/node-error';

export interface MockNodeOptions {
  network?: string;
  daa?: bigint;
  unixSeconds?: bigint;
  rateMilli?: number | null;
  /**
   * Address of a kaspa-string-form script public key (`version + script`). Default: the string itself, so a test without the SDK can use
   * spk strings as addresses; pass `(spk) => spkStringToAddress(sdk, spk, network)` for real addresses.
   */
  addressOf?: (scriptPublicKey: string) => string;
  /** a submit whose inputs are not all in the set fails like a real node (`orphan`). Default true. */
  checkInputs?: boolean;
  /** apply accepted transactions to the UTXO set. Default true. */
  applyOnSubmit?: boolean;
}

const key = (txid: string, index: number) => `${txid}:${index}`;

export class MockNode implements NodeApi {
  readonly kind = 'http-mock' as const;
  network: string;
  daa: bigint;
  unixSeconds: bigint;
  rateMilli: number | null;
  /** every transaction accepted so far, in order */
  readonly submissions: TxJson[] = [];
  /** every submit attempt, accepted or not */
  readonly attempts: TxJson[] = [];
  /** number of `getUtxosByAddresses` calls and the address lists they carried */
  readonly utxoQueries: string[][] = [];
  /** what `getFeeEstimate` answers (null = no estimate: the fee policy pays the floor); `feeEstimateCalls` counts the reads */
  feeEstimate: FeeEstimate | null = null;
  feeEstimateCalls = 0;
  connected = false;
  private readonly utxos = new Map<string, NodeUtxo>();
  private readonly opts: Required<Pick<MockNodeOptions, 'checkInputs' | 'applyOnSubmit'>>;
  private readonly addressOf: (spk: string) => string;
  private rejections: unknown[] = [];
  private readonly unavailable = { on: false };

  constructor(o: MockNodeOptions = {}) {
    this.network = o.network ?? 'testnet-10';
    this.daa = o.daa ?? 1_000_000n;
    this.unixSeconds = o.unixSeconds ?? 1_790_000_000n;
    this.rateMilli = o.rateMilli === undefined ? 10_000 : o.rateMilli;
    this.addressOf = o.addressOf ?? ((s) => s);
    this.opts = { checkInputs: o.checkInputs ?? true, applyOnSubmit: o.applyOnSubmit ?? true };
  }

  // ---------------------------------------------------------------------------------------------- test controls

  setUtxos(list: NodeUtxo[]): this {
    this.utxos.clear();
    for (const u of list) this.addUtxo(u);
    return this;
  }

  addUtxo(u: Partial<NodeUtxo> & Pick<NodeUtxo, 'address' | 'transactionId' | 'index' | 'amount'>): NodeUtxo {
    const full: NodeUtxo = {
      scriptPublicKey: '',
      blockDaaScore: (this.daa - 100n).toString(),
      isCoinbase: false,
      covenantId: null,
      ...u,
    };
    this.utxos.set(key(full.transactionId, full.index), full);
    return full;
  }

  removeUtxo(txid: Hex, index: number): boolean {
    return this.utxos.delete(key(txid, index));
  }

  hasUtxo(txid: Hex, index: number): boolean {
    return this.utxos.has(key(txid, index));
  }

  all(): NodeUtxo[] {
    return [...this.utxos.values()];
  }

  /** Puts the inputs a transaction spends into the set (as the node would already know them), addressed via `addressOf`. */
  seedFromInputs(tx: TxJson): void {
    for (const i of tx.inputs) {
      this.addUtxo({
        address: i.utxo.address ?? this.addressOf(i.utxo.scriptPublicKey),
        transactionId: i.transactionId,
        index: i.index,
        amount: i.utxo.amount,
        scriptPublicKey: i.utxo.scriptPublicKey,
        blockDaaScore: i.utxo.blockDaaScore,
        isCoinbase: i.utxo.isCoinbase,
        covenantId: i.utxo.covenantId,
      });
    }
  }

  /** The next `submitTransaction` fails with this reason (a string is classified like a node message, an Error is passed through). */
  rejectNext(reason: string | Error, times = 1): this {
    for (let i = 0; i < times; i++) this.rejections.push(reason);
    return this;
  }

  /** Simulates a lost connection: every call throws an `unavailable` NodeError until switched off. */
  setUnavailable(on: boolean): this {
    this.unavailable.on = on;
    return this;
  }

  advance(daa: bigint, seconds?: bigint): this {
    this.daa += daa;
    this.unixSeconds += seconds ?? daa / 10n;
    return this;
  }

  // ---------------------------------------------------------------------------------------------- NodeApi

  private check(): void {
    if (this.unavailable.on) throw new NodeError('unavailable', 'The mock node is unavailable.');
  }

  async connect(): Promise<NodeInfo> {
    this.check();
    this.connected = true;
    return { network: this.network, virtualDaaScore: this.daa.toString(), serverVersion: 'mock', daaRateMilli: this.rateMilli };
  }

  async disconnect(): Promise<void> {
    this.connected = false;
  }

  async getClock(): Promise<{ daa: bigint; unixSeconds: bigint; rateMilli: number | null }> {
    this.check();
    return { daa: this.daa, unixSeconds: this.unixSeconds, rateMilli: this.rateMilli };
  }

  async getUtxosByAddresses(addresses: string[]): Promise<NodeUtxo[]> {
    this.check();
    this.utxoQueries.push([...addresses]);
    const want = new Set(addresses);
    return this.all().filter((u) => want.has(u.address)).map((u) => ({ ...u }));
  }

  async getFeeEstimate(): Promise<FeeEstimate | null> {
    this.feeEstimateCalls++;
    return this.feeEstimate;
  }

  async submitTransaction(tx: TxJson): Promise<string> {
    this.check();
    this.attempts.push(tx);
    const rejection = this.rejections.shift();
    if (rejection !== undefined) throw describeNodeError(rejection);
    if (this.opts.checkInputs) {
      for (const i of tx.inputs) {
        if (!this.utxos.has(key(i.transactionId, i.index))) {
          throw describeNodeError(`Rejected transaction ${tx.id}: transaction ${tx.id} is an orphan where orphan is disallowed`);
        }
      }
    }
    this.submissions.push(tx);
    if (this.opts.applyOnSubmit) {
      for (const i of tx.inputs) this.utxos.delete(key(i.transactionId, i.index));
      tx.outputs.forEach((o, index) => {
        this.addUtxo({
          address: this.addressOf(o.scriptPublicKey),
          transactionId: tx.id,
          index,
          amount: o.value,
          scriptPublicKey: o.scriptPublicKey,
          blockDaaScore: this.daa.toString(),
          covenantId: o.covenant?.covenantId ?? null,
        });
      });
    }
    return tx.id;
  }
}
