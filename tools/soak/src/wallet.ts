// A bot wallet: a local Schnorr key standing in for a browser wallet.
// Every transaction goes through the web app's own pipeline, exactly what a user's wallet does, so
// the soak exercises the production planners.
// The pipeline is: planner -> kob.build -> local signature -> kob.finalize -> kob.validate (script
// engine) -> node submit.
// Outpoints spent by our own not-yet-accepted transactions are reserved, so the next plan never
// double-spends them.
import { appendFileSync, mkdirSync } from 'node:fs';
import { join } from 'node:path';
import { signBuilt } from '@/testing/local-signer';
import { signAndSubmit, SignFlowError } from '@/wallet/sign';
import type { WalletAdapter } from '@/wallet/types';
import { createUtxoService, type UtxoService } from '@/data/utxos';
import { TokenTracker, type TokenRef } from '@/data/token-tracker';
import { feeChoiceOf } from '@/kob/fee-policy';
import type { BuiltTx, KeyUtxo, TokenUtxo } from '@/kob/types';
import type { Env, KeyEntry } from './env';
import { errText, logger, type Logger, type Stats } from './log';
import { FileStorage } from './file-storage';
import { gateRefusal } from './indexer-gate';
import { mempoolSpent, Mutex, splitPending, type MempoolEntriesLike } from './bots/pending';
import { kasMergeSelection } from './bots/consolidate-math';
import { pickFor } from '@/kob/fee-policy';
import { feeContext } from './fees';

const rootLog = logger('wallet');

export interface SubmitOutcome {
  ok: boolean;
  txid?: string;
  fee?: bigint;
  /** node / engine error text */
  error?: string;
  /** a lost race (input already spent / double spend in mempool): benign */
  conflict?: boolean;
  /** refused without submitting: the indexer is not following (see indexer-gate.ts) */
  paused?: boolean;
}

// Under a TN10 flood a submitted transaction can wait minutes in the mempool while the UTXO index
// still lists its inputs.
// The reservation must outlast that wait, or the market maker re-spends its own pending inputs.
const RESERVE_MS = 600_000;
/**
 * Inputs of a transaction the mempool refused because another transaction spends one of them. They
 * are skipped this long (the other spend lands).
 */
const CONFLICT_RESERVE_MS = 60_000;
/** how old the wallet's view of its own mempool transactions may be when it plans */
const MEMPOOL_TTL_MS = 10_000;
/**
 * How long one read of the key's token UTXOs of a token serves the next reads (well below the
 * indexer's own lag behind the chain).
 */
const TOKEN_UTXOS_TTL_MS = 5_000;

/**
 * One SDK client per process for the wallets' mempool reads (the web app's node wrapper has none).
 * Under a flood a transaction at the normal rate can wait up to an hour in the mempool, far beyond
 * RESERVE_MS.
 * Its inputs are skipped as long as the node holds it.
 */
// eslint-disable-next-line @typescript-eslint/no-explicit-any
let mempoolRpc: Promise<any> | null = null;
// eslint-disable-next-line @typescript-eslint/no-explicit-any
async function mempoolClient(env: Env): Promise<any> {
  const cur = mempoolRpc ? await mempoolRpc.catch(() => null) : null;
  if (cur?.isConnected) return cur;
  mempoolRpc = (async () => {
    // eslint-disable-next-line @typescript-eslint/no-explicit-any
    const sdk = env.sdk as any;
    const rpc = new sdk.RpcClient({ url: env.cfg.nodeUrl, networkId: env.cfg.network, encoding: sdk.Encoding.SerdeJson });
    await rpc.connect({ blockAsyncConnect: true, timeoutDuration: 15_000 });
    return rpc;
  })();
  return mempoolRpc;
}

const within = <T>(p: Promise<T>, ms: number): Promise<T> =>
  Promise.race([p, new Promise<T>((_, rej) => setTimeout(() => rej(new Error(`no answer within ${ms} ms`)), ms))]);

export class BotWallet {
  readonly name: string;
  readonly pk: string;
  readonly address: string;
  private readonly sk: string;
  private readonly utxos: UtxoService;
  readonly tracker: TokenTracker;
  private readonly reserved = new Map<string, number>();
  private readonly adapter: WalletAdapter;
  private readonly lock = new Mutex();
  /**
   * Outpoints this key's transactions in the node's mempool spend (refreshed at most every
   * MEMPOOL_TTL_MS by funding / tokenUtxos).
   */
  private inMempool = new Set<string>();
  private mempoolAt = 0;
  private mempoolRead: Promise<void> | null = null;
  private lastKasMerge = 0;
  /**
   * The last read of the key's token UTXOs per token (covenant id), shared for TOKEN_UTXOS_TTL_MS
   * (see `tokenUtxos`).
   */
  private readonly tokenReads = new Map<string, { at: number; p: Promise<TokenUtxo[]> }>();

  constructor(
    readonly env: Env,
    k: KeyEntry,
    name: string,
    readonly log: Logger,
    readonly stats: Stats,
  ) {
    this.name = name;
    this.sk = k.secretKey;
    this.pk = k.publicKey;
    this.address = k.address;
    this.utxos = createUtxoService({ node: env.node, sdk: env.sdk, network: env.cfg.network });
    const dir = join(env.cfg.runPath, 'state');
    mkdirSync(dir, { recursive: true });
    this.tracker = new TokenTracker({
      kob: env.kob,
      node: env.node,
      sdk: env.sdk,
      network: env.cfg.network,
      indexer: env.indexer,
      storage: new FileStorage(join(dir, `tracker-${name}.json`)),
      pendingGraceMs: 30 * 60_000,
    });
    const sk = this.sk;
    this.adapter = {
      id: 'kasware',
      label: `soak:${name}`,
      detect: () => true,
      connect: async () => ({ id: 'kasware', label: name, address: k.address, pubkey: k.publicKey, network: env.cfg.network, version: 'soak' }),
      signTx: async (built: BuiltTx) => signBuilt(built, [sk]),
      // A local key that signs what it is given, like the app's private-key signer. There is no
      // wallet node whose view of fresh inputs must be awaited.
      inputPolicy: 'offline',
    };
  }

  /**
   * Reads which outpoints this key's mempool transactions spend. A failed read keeps the previous
   * answer, and the reservations still apply.
   */
  private async refreshMempool(): Promise<void> {
    if (Date.now() - this.mempoolAt < MEMPOOL_TTL_MS) return;
    this.mempoolRead ??= (async () => {
      try {
        const rpc = await within(mempoolClient(this.env), 20_000);
        const r = (await within(rpc.getMempoolEntriesByAddresses({ addresses: [this.address], includeOrphanPool: true, filterTransactionPool: false }), 15_000)) as MempoolEntriesLike;
        this.inMempool = mempoolSpent(r);
        this.stats.set(`mempool_spent:${this.name}`, this.inMempool.size);
      } catch (e) {
        this.stats.inc('mempool_read_failed');
        this.log.debug('mempool read failed', { error: errText(e) });
      } finally {
        this.mempoolAt = Date.now();
        this.mempoolRead = null;
      }
    })();
    await this.mempoolRead;
  }

  private isReserved(txid: string, index: number): boolean {
    const k = `${txid}:${index}`;
    if (this.inMempool.has(k)) return true;
    const exp = this.reserved.get(k);
    if (exp === undefined) return false;
    if (exp < Date.now()) {
      this.reserved.delete(k);
      return false;
    }
    return true;
  }

  reserve(txid: string, index: number, ms = RESERVE_MS): void {
    this.reserved.set(`${txid}:${index}`, Date.now() + ms);
  }

  /**
   * Outpoints ("txid:index") currently reserved by this wallet's own submitted transactions, or
   * spent by its transactions in the mempool.
   */
  reservedKeys(): Set<string> {
    const now = Date.now();
    return new Set([...[...this.reserved].filter(([, exp]) => exp >= now).map(([k]) => k), ...this.inMempool]);
  }

  /**
   * Runs one plan-and-submit section of this key at a time (see pending.ts `Mutex`).
   * Every bot action that plans over the key's coins goes through here (place, cancel, amend,
   * cancel-all, fan-out, merge).
   * So concurrent loops of one key never pick the same input.
   */
  exclusive<T>(fn: () => Promise<T>): Promise<T> {
    return this.lock.run(fn);
  }

  /**
   * An order listing split into the orders this key may act on and those its own pending
   * transactions already spend (pending.ts).
   */
  pendingSplit<T extends { current?: { txid: string; index: number } | null }>(views: readonly T[]): { live: T[]; pending: T[] } {
    return splitPending(views, this.reservedKeys());
  }

  /** the orders of a listing this key may act on (its own pending cancels / amends left out) */
  actionable<T extends { current?: { txid: string; index: number } | null }>(views: readonly T[]): T[] {
    return this.pendingSplit(views).live;
  }

  /**
   * Merges this key's smallest plain KAS coins into one (consolidate-math `kasMergeSelection`).
   * It is a compound transaction of the official SDK's generator back to the key, at the LOW bucket
   * rate, at most once a minute per key.
   * Returns the transactions submitted. Never throws.
   */
  mergeKas(o: { above: number; maxInputs: number }): Promise<number> {
    if (o.above <= 0 || Date.now() - this.lastKasMerge < 60_000) return Promise.resolve(0);
    this.lastKasMerge = Date.now();
    return this.exclusive(async () => {
      try {
        await this.refreshMempool();
        const rpc = await within(mempoolClient(this.env), 20_000);
        const { entries } = (await within(rpc.getUtxosByAddresses({ addresses: [this.address] }), 30_000)) as { entries: unknown[] };
        // eslint-disable-next-line @typescript-eslint/no-explicit-any
        const coins = (entries as any[])
          .filter((e) => !e.isCoinbase && !e.entry?.covenantId)
          .map((e) => ({ transactionId: String(e.outpoint.transactionId), index: Number(e.outpoint.index), amount: BigInt(e.amount), e }));
        this.stats.set(`kas_utxos:${this.name}`, coins.length);
        const pick = kasMergeSelection(coins, { ...o, reserved: this.reservedKeys() });
        const total = pick.reduce((t, c) => t + c.amount, 0n);
        // One output of all but 1 KAS. The generator then spends every picked coin (with no output
        // it spends one).
        // The change (1 KAS less the fee) stays clear of the storage-mass dead band. A pile worth
        // less than 2 KAS waits.
        if (!pick.length || total < 2n * 100_000_000n) return 0;
        const rate = pickFor(await feeContext(this.env), 'low').rate;
        // eslint-disable-next-line @typescript-eslint/no-explicit-any
        const sdk = this.env.sdk as any;
        const { transactions } = await sdk.createTransactions({
          entries: pick.map((c) => c.e),
          outputs: [{ address: this.address, amount: total - 100_000_000n }],
          changeAddress: this.address,
          priorityFee: 0n,
          feeRate: Number(rate),
          networkId: this.env.cfg.network,
        });
        const key = new sdk.PrivateKey(this.sk);
        let n = 0;
        for (const tx of transactions) {
          tx.sign([key]);
          const id = await tx.submit(rpc);
          for (const i of tx.serializeToObject().inputs as { transactionId: string; index: number }[]) this.reserve(i.transactionId, i.index);
          n++;
          this.stats.inc('tx_ok');
          this.stats.inc('tx_ok:mergeKas');
          this.log.info('submitted', { what: 'mergeKas', txid: id, rate: rate.toString(), urgency: 'low', inputs: tx.serializeToObject().inputs.length, coins: coins.length });
        }
        return n;
      } catch (e) {
        this.stats.inc('tx_failed:mergeKas');
        this.log.warn('kas merge failed', { error: errText(e) });
        return 0;
      }
    });
  }

  /** spendable KAS UTXOs (mature, not reserved), largest first */
  async funding(): Promise<KeyUtxo[]> {
    const [all] = await Promise.all([this.utxos.fundingFor(this.pk), this.refreshMempool()]);
    return all.filter((u) => !this.isReserved(u.transactionId, u.index));
  }

  async kasBalance(): Promise<bigint> {
    return (await this.funding()).reduce((s, u) => s + BigInt(u.amount), 0n);
  }

  /**
   * Spendable token UTXOs of `token` (verified on the node, not reserved), largest first.
   * One read of the indexer (and the node) serves every call for TOKEN_UTXOS_TTL_MS.
   * A market-maker tick or a trader action reads them several times (merge check, inventory, each
   * plan).
   * A reused answer is safe: only this key spends its token UTXOs, and its own spends are filtered
   * as reservations at every call.
   */
  async tokenUtxos(token: TokenRef): Promise<TokenUtxo[]> {
    const k = token.covenantId.toLowerCase();
    let read = this.tokenReads.get(k);
    if (!read || Date.now() - read.at > TOKEN_UTXOS_TTL_MS) {
      const r = { at: Date.now(), p: this.tracker.tokenUtxosFor(this.pk, token) };
      // a failed read is not reused
      r.p.catch(() => {
        if (this.tokenReads.get(k) === r) this.tokenReads.delete(k);
      });
      this.tokenReads.set(k, r);
      read = r;
    }
    const [all] = await Promise.all([read.p, this.refreshMempool()]);
    return all.filter((u) => !this.isReserved(u.transactionId, u.index));
  }

  async tokenBalance(token: TokenRef): Promise<bigint> {
    return (await this.tokenUtxos(token)).reduce((s, u) => s + BigInt(u.state.amount), 0n);
  }

  /**
   * Every submitted transaction goes to run/state/txs.jsonl with its fee and an independent mass
   * computation (kob.masses of the signed transaction).
   * The checker verifies fee == feeRate x max(compute, normalized transient), with the recorded
   * feeRate in [relay floor, maxRate].
   * `urgency`, `feeSource`, `bucketFeerate`, `cappedFrom` and `overCap` are the fee policy's own
   * account of the rate.
   * They are null when the transaction was built without the policy. The checker does not rely on
   * them.
   */
  private recordTx(what: string, txid: string, signed: { tx: import('@/kob/types').TxJson; fee: { fee: string; minFee: string; feeRate: string; feeMode: string; changeOutput: number | null } }, choice: ReturnType<typeof feeChoiceOf> = null): void {
    try {
      const m = this.env.kob.masses(signed.tx);
      const policy = {
        urgency: choice?.urgency ?? null,
        feeSource: choice?.source ?? null,
        ...(choice?.reason ? { feeReason: choice.reason } : {}),
        ...(choice?.bucketFeerate !== undefined ? { bucketFeerate: choice.bucketFeerate } : {}),
        ...(choice?.cappedFrom !== undefined ? { cappedFrom: choice.cappedFrom.toString() } : {}),
        ...(choice?.overCap ? { overCap: true } : {}),
      };
      const line = { ts: Date.now(), bot: this.name, what, txid, fee: signed.fee.fee, minFee: signed.fee.minFee, feeRate: signed.fee.feeRate, feeMode: signed.fee.feeMode, ...policy, changeOutput: signed.fee.changeOutput, compute: m.compute, transientNormalized: m.transientNormalized, storage: m.storage, size: m.size, payload: ((signed.tx as { payload?: string }).payload?.length ?? 0) / 2 };
      appendFileSync(join(this.env.cfg.runPath, 'state', 'txs.jsonl'), JSON.stringify(line) + '\n');
    } catch (e) {
      this.log.warn('recordTx failed', { error: errText(e) });
    }
  }

  /**
   * Signs, finalizes, validates in the script engine and submits. Reserves the inputs on success.
   * Refused (paused: true) while the indexer gate is closed.
   */
  async submit(built: BuiltTx, what: string, fields: Record<string, unknown> = {}): Promise<SubmitOutcome> {
    // The one choke point of every bot transaction: nothing is placed, amended, cancelled, fanned
    // out or merged while the indexer lags.
    const refused = await gateRefusal(this.env.gate, what);
    if (refused) return refused;
    try {
      const r = await signAndSubmit({ kob: this.env.kob, node: this.env.node, adapter: this.adapter, built, network: this.env.cfg.network, checkWallet: false });
      for (const i of r.signed.tx.inputs) this.reserve(i.transactionId, i.index);
      this.stats.inc('tx_ok');
      this.stats.inc(`tx_ok:${what}`);
      this.stats.inc('fees_sompi', Number(r.fee));
      const choice = feeChoiceOf(built);
      this.log.info('submitted', { what, txid: r.txid, fee: r.fee, rate: r.signed.fee.feeRate, ...(choice ? { urgency: choice.urgency, feeSource: choice.source } : {}), ...fields });
      this.recordTx(what, r.txid, r.signed, choice);
      return { ok: true, txid: r.txid, fee: r.fee };
    } catch (e) {
      const text = e instanceof SignFlowError ? `${e.stage}: ${e.message}` : errText(e);
      // "Already has this transaction": the same transaction again (a plan from a listing that
      // still shows what it spent). It is benign, like a lost race.
      const conflict = /double spend|already spent|orphan|not found in the utxo|missing|RejectDoubleSpend|already in the mempool|is not in the utxo set|already being spent|already has this transaction/i.test(text);
      this.stats.inc(conflict ? 'tx_conflict' : 'tx_failed');
      this.stats.inc(`${conflict ? 'tx_conflict' : 'tx_failed'}:${what}`);
      // A rejected transaction's inputs may still be ours to use, so release nothing.
      // When the mempool already holds a spend of one of them (ours not yet indexed, or the
      // matcher's fill of an order), the next plans skip them for a minute.
      // That avoids losing the same race again.
      if (/already being spent|double spend|RejectDoubleSpend|already spent by transaction/i.test(text)) {
        for (const i of built.tx.inputs) this.reserve(i.transactionId, i.index, CONFLICT_RESERVE_MS);
      }
      (conflict ? this.log.info : this.log.warn)('submit failed', { what, conflict, error: text, ...fields });
      return { ok: false, error: text, conflict };
    }
  }
}

/**
 * One BotWallet per key and process. The input reservations live in the wallet object, so two
 * objects of one key would double-spend each other's coins.
 * Examples are the market maker of two books and the fan-out. Every bot takes its wallet from here.
 */
const wallets = new Map<string, BotWallet>();
export function walletFor(env: Env, name: string, stats: Stats, log?: Logger): BotWallet {
  let w = wallets.get(name);
  if (!w) {
    const k = env.keys[name];
    if (!k) throw new Error(`no key named ${name} in run/keys.json (run scripts/keygen.mjs)`);
    w = new BotWallet(env, k, name, log ?? rootLog.child(name), stats);
    wallets.set(name, w);
  }
  return w;
}
