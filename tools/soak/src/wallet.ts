// A bot wallet: a local Schnorr key standing in for a browser wallet. Every transaction goes through the web app's own pipeline
// (planner -> kob.build -> local signature -> kob.finalize -> kob.validate (script engine) -> node submit), exactly what a user's wallet
// does, so the soak exercises the production planners. Outpoints spent by our own not-yet-accepted transactions are reserved so the next
// plan never double-spends them.
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

// Under a TN10 flood a submitted transaction can wait minutes in the mempool while the UTXO index still lists its inputs (10-07: 3 min
// was too short, the market maker re-spent its own pending inputs).
const RESERVE_MS = 600_000;
/** inputs of a transaction the mempool refused because another transaction spends one of them: skipped this long (the other spend lands) */
const CONFLICT_RESERVE_MS = 60_000;

export class BotWallet {
  readonly name: string;
  readonly pk: string;
  readonly address: string;
  private readonly sk: string;
  private readonly utxos: UtxoService;
  readonly tracker: TokenTracker;
  private readonly reserved = new Map<string, number>();
  private readonly adapter: WalletAdapter;

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
    };
  }

  private isReserved(txid: string, index: number): boolean {
    const k = `${txid}:${index}`;
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

  /** outpoints ("txid:index") currently reserved by this wallet's own submitted transactions */
  reservedKeys(): Set<string> {
    const now = Date.now();
    return new Set([...this.reserved].filter(([, exp]) => exp >= now).map(([k]) => k));
  }

  /** spendable KAS UTXOs (mature, not reserved), largest first */
  async funding(): Promise<KeyUtxo[]> {
    const all = await this.utxos.fundingFor(this.pk);
    return all.filter((u) => !this.isReserved(u.transactionId, u.index));
  }

  async kasBalance(): Promise<bigint> {
    return (await this.funding()).reduce((s, u) => s + BigInt(u.amount), 0n);
  }

  async tokenUtxos(token: TokenRef): Promise<TokenUtxo[]> {
    const all = await this.tracker.tokenUtxosFor(this.pk, token);
    return all.filter((u) => !this.isReserved(u.transactionId, u.index));
  }

  async tokenBalance(token: TokenRef): Promise<bigint> {
    return (await this.tokenUtxos(token)).reduce((s, u) => s + BigInt(u.state.amount), 0n);
  }

  /**
   * Every submitted transaction goes to run/state/txs.jsonl with its fee and an INDEPENDENT mass computation (kob.masses of the signed
   * transaction): the checker verifies fee == feeRate x max(compute, normalized transient) with the recorded feeRate in [relay floor, maxRate].
   * `urgency` / `feeSource` / `bucketFeerate` / `cappedFrom` / `overCap` are the fee policy's own account of the rate (null when the transaction was
   * built without the policy); the checker does not rely on them.
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

  /** signs, finalizes, validates in the script engine and submits; reserves the inputs on success. Refused (paused: true) while the indexer gate is closed. */
  async submit(built: BuiltTx, what: string, fields: Record<string, unknown> = {}): Promise<SubmitOutcome> {
    // the one choke point of every bot transaction: nothing is placed, amended, cancelled, fanned out or merged while the indexer lags
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
      const conflict = /double spend|already spent|orphan|not found in the utxo|missing|RejectDoubleSpend|already in the mempool|is not in the utxo set|already being spent/i.test(text);
      this.stats.inc(conflict ? 'tx_conflict' : 'tx_failed');
      this.stats.inc(`${conflict ? 'tx_conflict' : 'tx_failed'}:${what}`);
      // a rejected transaction's inputs may still be ours to use; release nothing. When the mempool already holds a spend of one of them
      // (ours not yet indexed, or the matcher's fill of an order), the next plans skip them for a minute instead of losing the same race again.
      if (/already being spent|double spend|RejectDoubleSpend|already spent by transaction/i.test(text)) {
        for (const i of built.tx.inputs) this.reserve(i.transactionId, i.index, CONFLICT_RESERVE_MS);
      }
      (conflict ? this.log.info : this.log.warn)('submit failed', { what, conflict, error: text, ...fields });
      return { ok: false, error: text, conflict };
    }
  }
}

/**
 * One BotWallet per key and process: the input reservations live in the wallet object, so two objects of one key (the market maker of two
 * books, the fan-out) would double-spend each other's coins. Every bot takes its wallet from here.
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
