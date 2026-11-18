// The signing pipeline of a chain, a list of transactions where step k+1 spends an output of step
// k. A token creation is one, with the KCC-23 preparation and then the genesis.
//
// Each step goes through what every transaction of the app goes through (sign.ts). The wallet
// signs, `kob.finalize` verifies every signature and assembles the scripts, `kob.validate` runs the
// script engine, and then the node takes it. There are two strategies:
//
// - Atomic, when the adapter has `signChain` and the chain has two steps. That adapter is the
//   private-key signer. One approval covers every step. All are signed, finalized and validated
//   before anything is sent, then sent in order.
// - Sequential, for every extension wallet. Step k is signed, finalized, validated, recorded
//   (`onSigned`) and sent before the wallet is asked for step k+1. Step k+1 is asked for only once
//   the output it spends is visible to the wallet's own node (visibility.ts). An extension wallet
//   looks every input up on its own node (KasWare 0.11.5), and an output of a transaction nobody
//   broadcast is never there.
//
// Nothing is sent that was not finalized and validated. A failure says how many steps the node took
// (`ChainError.sent`). Those are out for good.
import type { NodeApi } from '../data/node';
import { NodeError, describeNodeError } from '../data/node-error';
import type { BuiltTx, InputSignature, SignedTx, TxJson } from '../kob/types';
import type { KobWasm } from '../kob/wasm';
import { REJECTED_FRESH, SignFlowError, assertWalletMatches, isDeclinedFresh, type FailedStage, type SignStage } from './sign';
import { WalletError, type SignOptions, type WalletAdapter } from './types';
import { awaitVisible, needsVisibility, noteSent } from './visibility';

/** A failure of a chain, with the step that failed and how many steps were sent. */
export class ChainError extends SignFlowError {
  /** The step that failed, 0-based. */
  readonly step: number;
  /** How many steps the node took before the failure. Steps `0..sent-1` are out. */
  readonly sent: number;
  constructor(stage: FailedStage, cause: unknown, message: string, code: string, step: number, sent: number) {
    super(stage, cause, message, code);
    this.name = 'ChainError';
    this.step = step;
    this.sent = sent;
  }
}

/** What a chain stage reports beyond its stage. */
export interface ChainStageInfo {
  step: number;
  txid?: string;
  /**
   * The gate is waiting until the wallet's node sees the output this step spends. The wallet is
   * not asked yet.
   */
  waiting?: boolean;
}

/** The inputs of `runSignChain`. */
export interface ChainArgs {
  kob: KobWasm;
  node: NodeApi;
  adapter: WalletAdapter;
  network: string;
  /** The steps, in order. Step k+1 must spend an output of step k. */
  steps: BuiltTx[];
  /**
   * Maps a script public key to an address of the app's network. The visibility gate asks KOB's
   * node by address.
   */
  addressOf: (spk: string) => string;
  /**
   * The parents of the first step that an earlier page sent, mapped from txid to when. Their
   * outputs count as fresh too (visibility.ts `freshInputs`).
   */
  alsoFresh?: ReadonlyMap<string, number>;
  /** Passed to `signChain`. The private-key signer checks the genesis against it. */
  signChainOptions?: Pick<SignOptions, 'genesisClaim'>;
  timeoutMs?: number;
  /** The default is true when the adapter can `refresh`. */
  checkWallet?: boolean;
  /** The default is true, and only tests turn it off. */
  validate?: boolean;
  tighten?: boolean;
  /**
   * Receives every step signed so far, finalized and validated, before the newest is sent. In the
   * atomic strategy it is called once, with every step. The caller records what it needs to finish
   * the chain if the page dies. A throw stops the chain before anything more is sent.
   */
  onSigned?: (signed: SignedTx[]) => void;
  /** Called when the node took step k. */
  onSent?: (step: number, signed: SignedTx) => void;
  onStage?: (stage: SignStage, info: ChainStageInfo) => void;
  /**
   * Tells whether an `orphan` answer to step k is offered again, because the node has not seen its
   * parent yet. The default is every step after the first.
   */
  orphanRetry?: (step: number) => boolean;
  /** The orphan retries. The default is 5 tries, 1500 ms apart. */
  sendAttempts?: number;
  sendRetryMs?: number;
  /**
   * How often, and how far apart, a wallet that answers "I cannot see an input"
   * (`inputs-unknown`) is asked. The default is 3 times, 2000 ms apart.
   */
  unknownAttempts?: number;
  unknownRetryMs?: number;
  /** The visibility gate's limits (visibility.ts). */
  visibleTimeoutMs?: number;
  visiblePollMs?: number;
  settleMs?: number;
  sleep?: (ms: number) => Promise<void>;
  now?: () => number;
}

/** What a chain sent. */
export interface ChainResult {
  signed: SignedTx[];
  /** All fees together, in sompi. */
  fee: bigint;
}

const msg = (e: unknown) => (e instanceof Error ? e.message : String(e));

/** Tells whether `next` spends an output of `prev`. */
export const spendsOutputOf = (prev: Pick<BuiltTx, 'tx'>, next: Pick<BuiltTx, 'tx'>): boolean => next.tx.inputs.some((i) => i.transactionId === prev.tx.id);

/**
 * Sends `tx`. A node that already has it counts as sent. It throws a NodeError, or a SignFlowError
 * `txid-mismatch`.
 */
export async function sendTx(node: NodeApi, tx: TxJson): Promise<void> {
  try {
    const id = await node.submitTransaction(tx);
    if (id !== tx.id) throw new SignFlowError('submitting', null, `The node answered with transaction id ${String(id).slice(0, 16)}... for ${tx.id.slice(0, 16)}...: its answer cannot be trusted. Check the explorer before retrying.`, 'txid-mismatch');
  } catch (e) {
    if (e instanceof SignFlowError) throw e;
    const err = e instanceof NodeError ? e : describeNodeError(e);
    if (err.code !== 'already-known') throw err;
  }
}

/**
 * Runs `sendTx` again while the node answers `orphan`, which means it has not seen the parent yet.
 * It makes `attempts` tries, `retryMs` apart.
 */
export async function sendTxRetrying(node: NodeApi, tx: TxJson, attempts: number, retryMs: number, sleep: (ms: number) => Promise<void>): Promise<void> {
  for (let n = 1; ; n++) {
    try {
      await sendTx(node, tx);
      return;
    } catch (e) {
      if (!(e instanceof NodeError) || e.code !== 'orphan' || n >= attempts) throw e;
      await sleep(retryMs);
    }
  }
}

/** Signs and sends a chain, atomically or step by step as the wallet allows. */
export async function runSignChain(a: ChainArgs): Promise<ChainResult> {
  const { kob, node, adapter, network, steps } = a;
  const sleep = a.sleep ?? ((ms: number) => new Promise<void>((r) => setTimeout(r, ms)));
  const stage = (s: SignStage, info: ChainStageInfo) => {
    try {
      a.onStage?.(s, info);
    } catch {
      /* a UI callback must not abort the flow */
    }
  };
  const tighten = a.tighten ?? true;
  const opts: SignOptions = { network, ...(a.timeoutMs !== undefined ? { timeoutMs: a.timeoutMs } : {}) };
  let sent = 0;
  const fail = (s: FailedStage, cause: unknown, message: string, code: string, step: number) => new ChainError(s, cause, message, code, step, sent);

  if (steps.length === 0) throw fail('signing', null, 'Nothing to sign.', 'other', 0);
  for (let k = 1; k < steps.length; k++) {
    if (!spendsOutputOf(steps[k - 1]!, steps[k]!)) throw fail('signing', null, `Step ${k + 1} does not spend step ${k} of this chain. Nothing was sent.`, 'other', k);
  }
  const check = (a.checkWallet ?? true) && !!adapter.refresh;

  const finalize = (k: number, sigs: InputSignature[]): SignedTx => {
    const built = steps[k]!;
    let signed: SignedTx;
    try {
      signed = kob.finalize(built, sigs, { tightenBudgets: tighten });
    } catch (e) {
      throw fail('finalizing', e, `The wallet's signature does not match this transaction${sent ? '' : ', nothing was sent'}. (${msg(e)})`, 'signature', k);
    }
    // The id the wallet signed for must stay, because the next step spends it. A version 1 id does
    // not depend on signature scripts or budgets.
    if (signed.tx.id !== built.tx.id) throw fail('finalizing', null, 'The signed transaction is not the one planned (its id changed): the chain would break. Nothing more was sent.', 'signature', k);
    return signed;
  };
  const validate = (k: number, signed: SignedTx) => {
    if (!(a.validate ?? true)) return;
    try {
      kob.validate(signed);
    } catch (e) {
      throw fail('validating', e, `The transaction failed the script check and was not sent. (${msg(e)})`, 'validation', k);
    }
  };
  const record = (signed: SignedTx[], k: number) => {
    try {
      a.onSigned?.(signed);
    } catch (e) {
      throw fail('validating', e, `The transaction could not be recorded before sending, so it was not sent. (${msg(e)})`, 'other', k);
    }
  };
  const submit = async (k: number, signed: SignedTx) => {
    stage('submitting', { step: k, txid: signed.tx.id });
    try {
      const retry = a.orphanRetry ? a.orphanRetry(k) : k > 0;
      await sendTxRetrying(node, signed.tx, retry ? (a.sendAttempts ?? 5) : 1, a.sendRetryMs ?? 1500, sleep);
    } catch (e) {
      if (e instanceof SignFlowError) throw new ChainError(e.stage, e.cause, e.message, e.code, k, sent);
      const err = e instanceof NodeError ? e : describeNodeError(e);
      throw fail('submitting', err, err.message, err.code, k);
    }
    sent = k + 1;
    void noteSent(adapter, signed.tx, a.now ? { now: a.now } : {});
    try {
      a.onSent?.(k, signed);
    } catch {
      /* a UI callback must not abort the flow */
    }
  };

  // Atomic: one approval for every step, from the private-key signer.
  if (adapter.signChain && steps.length === 2) {
    stage('signing', { step: 0 });
    if (check) for (const b of steps) await assertWalletMatches(adapter, b, network);
    let sigs: [InputSignature[], InputSignature[]];
    try {
      sigs = await adapter.signChain([steps[0]!, steps[1]!], { ...opts, ...a.signChainOptions });
    } catch (e) {
      throw fail('signing', e, msg(e), e instanceof WalletError ? e.code : 'other', 0);
    }
    stage('finalizing', { step: 0 });
    const signed = [finalize(0, sigs[0]), finalize(1, sigs[1])];
    stage('validating', { step: 0 });
    signed.forEach((s, k) => validate(k, s));
    record(signed, 1);
    for (let k = 0; k < signed.length; k++) await submit(k, signed[k]!);
    stage('submitted', { step: signed.length - 1, txid: signed[signed.length - 1]!.tx.id });
    return { signed, fee: signed.reduce((s, x) => s + BigInt(x.fee.fee), 0n) };
  }

  // Sequential: sign, check, record and send. The next step comes only once its input is visible
  // to the wallet's node.
  const signed: SignedTx[] = [];
  for (let k = 0; k < steps.length; k++) {
    const built = steps[k]!;
    stage('signing', { step: k });
    // The account and the network may have changed while the previous step went out.
    if (check) {
      try {
        await assertWalletMatches(adapter, built, network);
      } catch (e) {
        if (e instanceof SignFlowError) throw new ChainError(e.stage, e.cause, e.message, e.code, k, sent);
        throw e;
      }
    }
    if (needsVisibility(adapter)) {
      const seen = await awaitVisible({
        adapter, node, tx: built.tx, addressOf: a.addressOf, sleep,
        ...(a.alsoFresh ? { alsoFresh: a.alsoFresh } : {}),
        ...(a.now ? { now: a.now } : {}),
        ...(a.visibleTimeoutMs !== undefined ? { timeoutMs: a.visibleTimeoutMs } : {}),
        ...(a.visiblePollMs !== undefined ? { pollMs: a.visiblePollMs } : {}),
        ...(a.settleMs !== undefined ? { settleMs: a.settleMs } : {}),
        onWait: (waiting) => stage('signing', { step: k, waiting }),
      });
      if (!seen.ok) {
        throw fail('signing', null, `Your wallet cannot see the coin this step spends yet (not on the network after ${Math.round(seen.waitedMs / 1000)} s), so it was not asked to sign.`, 'inputs-pending', k);
      }
    }
    let sigs: InputSignature[] | null = null;
    const attempts = a.unknownAttempts ?? 3;
    for (let n = 1; sigs === null; n++) {
      try {
        sigs = await adapter.signTx(built, opts);
      } catch (e) {
        // The wallet's node has not seen the input after all, through a slow relay. A moment later
        // it has.
        if (e instanceof WalletError && e.code === 'inputs-unknown' && n < attempts) {
          await sleep(a.unknownRetryMs ?? 2000);
          continue;
        }
        // Declined, perhaps because the wallet's node could not find the step's input yet. Nothing
        // was sent for this step.
        if (isDeclinedFresh(e, adapter, built.tx, a.alsoFresh)) throw fail('signing', e, REJECTED_FRESH, 'rejected-fresh', k);
        throw fail('signing', e, msg(e), e instanceof WalletError ? e.code : 'other', k);
      }
    }
    stage('finalizing', { step: k });
    const s = finalize(k, sigs);
    stage('validating', { step: k });
    validate(k, s);
    signed.push(s);
    record([...signed], k);
    await submit(k, s);
  }
  stage('submitted', { step: steps.length - 1, txid: signed[signed.length - 1]!.tx.id });
  return { signed, fee: signed.reduce((s, x) => s + BigInt(x.fee.fee), 0n) };
}
