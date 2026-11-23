// The signing pipeline every transaction of the app goes through. Its stages are:
//
// - built: `kob.build` made the transaction.
// - signing: the wallet signs each digest.
// - finalizing: `kob.finalize` verifies each signature against its digest and assembles the
//   scripts.
// - validating: `kob.validate` runs the covenants in the script engine, so nothing invalid is ever
//   broadcast.
// - submitting: the node RPC takes it.
// - submitted: the node returned `{ txid }`.
//
// The wallet only ever sees the unsigned transaction the user confirmed. What it returns is reduced
// to signatures, which must verify against digests kob-wasm computed from our own transaction. So
// a wallet that altered the transaction, signed with the wrong key or used a different sighash
// type is caught at `finalizing`, before anything is broadcast.
import type { BuiltTx, SignedTx, TxJson } from '../kob/types';
import type { KobWasm } from '../kob/wasm';
import type { NodeApi, NodeUtxo } from '../data/node';
import { NodeError, describeNodeError } from '../data/node-error';
import { markOwnAction } from '../data/fresh';
import { confirmInputsOnNode, ownPlainInputs } from '../kob/node-verify';
import { normalizeNetwork, spkStringToAddress, type KaspaSdk } from '../data/kaspa-sdk';
import type { LockHint, SignerNote } from '../signer/protocol';
import { WalletError, type WalletAdapter, type LiveSigning } from './types';
import { awaitVisible, freshInputs, needsVisibility, noteSent } from './visibility';

/** A stage of the signing pipeline, as `onStage` reports it. */
export type SignStage = 'signing' | 'finalizing' | 'validating' | 'submitting' | 'submitted';
/** A stage the pipeline can fail at. */
export type FailedStage = Exclude<SignStage, 'submitted'>;

/** A failure of the signing pipeline, with the stage it failed at and a code for the UI. */
export class SignFlowError extends Error {
  readonly stage: FailedStage;
  readonly cause: unknown;
  /**
   * The cause's `WalletError.code` or `NodeError.code`. Otherwise it is `signature` when finalize
   * refused, `validation` when the script engine refused, or `other`.
   */
  readonly code: string;
  constructor(stage: FailedStage, cause: unknown, message: string, code: string) {
    super(message);
    this.name = 'SignFlowError';
    this.stage = stage;
    this.cause = cause;
    this.code = code;
  }
  /**
   * Tells whether the user declined in the wallet, which is not an error to show as one.
   * `rejected-fresh` counts too, although the wallet may have refused on its own.
   */
  get rejectedByUser(): boolean {
    return this.stage === 'signing' && (this.code === 'rejected' || this.code === 'rejected-fresh');
  }
}

/**
 * The wallet declined a transaction that spends an output this page sent moments ago. A KasWare
 * that cannot find such an input yet shows only Reject, and that answers like a user rejection
 * (wallet/kasware.ts), so the app cannot tell which happened. `rejected-fresh` says both: nothing
 * was sent, and a few seconds later the same step can be asked for again.
 */
export const REJECTED_FRESH =
  'Your wallet did not sign this step, so nothing was sent for it. If your wallet said an input "is not an unspent output on this network" (KasWare then offers only Reject), its node had not caught up with your previous transaction yet: wait a few seconds and try again.';

/** Tells whether a wallet's refusal of `tx` is to be reported as `rejected-fresh`. */
export const isDeclinedFresh = (e: unknown, adapter: WalletAdapter, tx: Pick<TxJson, 'inputs'>, alsoFresh?: ReadonlyMap<string, number>): boolean =>
  e instanceof WalletError && e.code === 'rejected' && needsVisibility(adapter) && freshInputs(tx, alsoFresh ? { alsoFresh } : {}).length > 0;

/** The inputs of `signAndSubmit`. */
export interface SignAndSubmitArgs {
  kob: KobWasm;
  node: NodeApi;
  adapter: WalletAdapter;
  built: BuiltTx;
  /** The network name, `mainnet` or `testnet-10`. Wallets that need it, such as Kastle, get it. */
  network: string;
  /**
   * Runs the in-wasm script engine before broadcasting. The default is true, and only tests turn it
   * off.
   */
  validate?: boolean;
  /**
   * Shrinks the compute budgets to what the scripts use, which lowers the fee. The default is true.
   */
  tighten?: boolean;
  onStage?: (stage: SignStage, info?: { txid?: string }) => void;
  /** The wallet popup timeout in ms. The default is `SIGN_TIMEOUT_MS`. */
  timeoutMs?: number;
  /**
   * Re-reads the wallet silently right before asking it to sign. It refuses when the wallet's
   * account or network is no longer the one the plan was built for. The default is true when the
   * adapter can `refresh`.
   */
  checkWallet?: boolean;
  /**
   * Maps a script public key to an address of the configured network.
   *
   * When given, every covenant or token input of the transaction is re-read from the node right
   * before the wallet is asked to sign. Any whose amount, script or covenant id the node does not
   * confirm aborts the flow. The KAS of unsigned covenant inputs is committed by no signature, so
   * the fee shown must be over chain values. The wallet's own plain KAS coins are not read: the
   * signature commits their values (kob/node-verify.ts `ownPlainInputs`).
   *
   * The order send (ui/confirm/send.ts) omits it. Its own pre-sign check has just read the same
   * inputs on the node, and the decoder stops any it does not confirm. Tests with synthetic chains omit it
   * too.
   */
  inputAddress?: (spk: string) => string;
  /**
   * The last DAA score at which this transaction may still be sent. It is set for an auction that
   * starts at the clock it was planned with (`auctionSubmitBy`).
   *
   * The node clock is read once the wallet has signed. A clock past it aborts the flow before
   * anything is broadcast (`clock-moved`). The price of the auction has then moved on while the
   * wallet waited for approval, so the caller plans it again with the current clock.
   */
  submitByDaa?: bigint;
  /** Set when the transaction re-times the one the wallet signed last (`SignOptions.retime`). */
  retime?: boolean;
  /**
   * The node's DAA score now, for the times the private-key signer says (`SignOptions.clockDaa`).
   */
  clockDaa?: bigint;
  /** The app's hints for the private-key signer's confirmation (`SignOptions.signerNotes`). */
  signerNotes?: SignerNote[];
  /**
   * A step of the wallet's one-approval session (`SignOptions.session`). Every check here still
   * runs.
   */
  session?: string;
  /**
   * The wallet's creator locks that a lock coin of this transaction pays to (`SignOptions.locks`).
   */
  locks?: LockHint[];
  /**
   * Set when the transaction may be replaced while the user reads it (`SignOptions.live`).
   * `submitByOf` gives the `submitByDaa` of a replacement the wallet signed, which is then the one
   * finalized, checked and sent.
   */
  live?: { signing: LiveSigning; submitByOf(built: BuiltTx): bigint | undefined };
  /**
   * Maps a script public key to an address of the app's network, for the visibility gate
   * (visibility.ts).
   *
   * An input that spends an output of a transaction this page sent moments ago must be visible to
   * an extension wallet's own node before the wallet is asked. That happens with a ladder, or with
   * an order right after another. Without this and without `inputAddress`, such a transaction is
   * not signed (`inputs-pending`).
   */
  addressOf?: (spk: string) => string;
  /** Called with true when the visibility gate starts waiting, and with false when it stops. */
  onWait?: (waiting: boolean) => void;
  /** The gate's limits. Tests shorten them. */
  visibility?: { timeoutMs?: number; pollMs?: number; settleMs?: number; sleep?: (ms: number) => Promise<void> };
}

/**
 * How far past its start an auction planned at the click may still be sent, in DAA. It is 2 s at
 * 10 per second.
 *
 * An auction of a market or marketable order starts `MARKET_ACTIVATION_DAA` after the clock it was
 * planned with, and moves its price from that start on. A wallet that keeps the request open
 * longer than that lead plus this margin returns a signature for an auction already this far down
 * its price path.
 */
export const AUCTION_START_MARGIN_DAA = 20n;

/**
 * How often, and how far apart, a wallet that answers "I cannot see an input" (`inputs-unknown`)
 * is asked. chain.ts does the same.
 */
const UNKNOWN_ATTEMPTS = 3;
const UNKNOWN_RETRY_MS = 2000;

/**
 * The `submitByDaa` of a transaction that creates `states`, the planner's `{ kind, state }` order
 * states.
 *
 * It is the earliest start, plus `AUCTION_START_MARGIN_DAA`, of an auction among them that starts
 * at or after `plannedAtDaa`, the clock it was planned with. An amended order that keeps an older
 * start does not count. It returns null when there is no such auction.
 */
export function auctionSubmitBy(states: readonly { state: unknown }[], plannedAtDaa: bigint): bigint | null {
  let by: bigint | null = null;
  for (const s of states) {
    const o = s.state as { activeFrom?: unknown; slope?: unknown };
    if (typeof o.activeFrom !== 'string' || typeof o.slope !== 'string') continue;
    let start: bigint;
    let slope: bigint;
    try {
      start = BigInt(o.activeFrom);
      slope = BigInt(o.slope);
    } catch {
      continue;
    }
    if (slope <= 0n || start <= 0n || start < plannedAtDaa) continue;
    const at = start + AUCTION_START_MARGIN_DAA;
    if (by === null || at < by) by = at;
  }
  return by;
}

/**
 * Checks that the node clock is still at or before `by` (`SignAndSubmitArgs.submitByDaa`). A clock
 * that cannot be read counts as moved.
 */
async function assertClockBefore(node: NodeApi, by: bigint): Promise<void> {
  let daa: bigint;
  try {
    daa = (await node.getClock()).daa;
  } catch (e) {
    const err = e instanceof NodeError ? e : describeNodeError(e);
    throw new SignFlowError('validating', err, `The node clock could not be read to check the auction start (${err.message}). Nothing was sent.`, 'clock-moved');
  }
  if (daa > by) throw new SignFlowError('validating', null, `The auction would already be ${daa - by + AUCTION_START_MARGIN_DAA} DAA past its start (node clock ${daa}). Nothing was sent.`, 'clock-moved');
}

/** What `signAndSubmit` sent. */
export interface SignAndSubmitResult {
  /** The id returned by the node. */
  txid: string;
  /** The transaction sent, `built` or the replacement of it the wallet signed (`live`). */
  built: BuiltTx;
  signed: SignedTx;
  /** The network fee in sompi. */
  fee: bigint;
}

const msg = (e: unknown) => (e instanceof Error ? e.message : String(e));

/**
 * Checks that the wallet is still on the account and network the plan was built for.
 *
 * A wallet that switched since must not be asked to sign it. The switch may be an event the UI has
 * not handled yet, or a confirmation screen left open. The popup would show another account, or
 * the signature would be for a key the plan does not spend.
 */
export async function assertWalletMatches(adapter: WalletAdapter, built: BuiltTx, network: string): Promise<void> {
  let now;
  try {
    now = await adapter.refresh!();
  } catch (e) {
    throw new SignFlowError('signing', e, `The wallet no longer exposes an account (${msg(e)}). Nothing was sent.`, 'wallet-lost');
  }
  if (normalizeNetwork(now.network) !== normalizeNetwork(network)) {
    throw new SignFlowError('signing', null, `The wallet is on ${now.network}, the app on ${network}. Nothing was sent.`, 'network');
  }
  if (built.sign.some((s) => s.pubkey !== now.pubkey)) throw new SignFlowError('signing', null, 'The wallet account changed since this transaction was planned. Nothing was sent.', 'account-changed');
}

async function assertInputsOnNode(node: NodeApi, built: BuiltTx, toAddress: (spk: string) => string): Promise<void> {
  const skip = ownPlainInputs(built);
  if (skip.size === built.tx.inputs.length) return;
  let res;
  try {
    res = await confirmInputsOnNode(node, built.tx.inputs, toAddress, skip);
  } catch (e) {
    const err = e instanceof NodeError ? e : describeNodeError(e);
    throw new SignFlowError('signing', err, `The node could not confirm the inputs of this transaction (${err.message}). Nothing was sent.`, 'inputs-unconfirmed');
  }
  if (res.mismatches.length) {
    const m = res.mismatches[0];
    throw new SignFlowError('signing', null, `Input ${m.index} does not match the node (${m.detail}). The transaction was planned from stale or wrong data: build it again. Nothing was sent.`, 'inputs-changed');
  }
}

/**
 * The visibility gate of one transaction (visibility.ts). An extension wallet is asked only once
 * every input that spends an output of a transaction this page sent recently is visible to its own
 * node. It does nothing for the private-key signer and for inputs older than that. It is a wallet
 * need, not a safety check: KasWare refuses an input its node does not list.
 */
async function assertVisibleToWallet(a: SignAndSubmitArgs, built: BuiltTx): Promise<void> {
  if (!needsVisibility(a.adapter) || freshInputs(built.tx).length === 0) return;
  const addressOf = a.addressOf ?? a.inputAddress;
  if (!addressOf) throw new SignFlowError('signing', null, 'This transaction spends a coin your wallet may not see yet. Nothing was sent; try again in a few seconds.', 'inputs-pending');
  const v = a.visibility ?? {};
  const seen = await awaitVisible({
    adapter: a.adapter, node: a.node, tx: built.tx, addressOf,
    ...(a.onWait ? { onWait: a.onWait } : {}),
    ...(v.timeoutMs !== undefined ? { timeoutMs: v.timeoutMs } : {}),
    ...(v.pollMs !== undefined ? { pollMs: v.pollMs } : {}),
    ...(v.settleMs !== undefined ? { settleMs: v.settleMs } : {}),
    ...(v.sleep ? { sleep: v.sleep } : {}),
  });
  if (!seen.ok) {
    throw new SignFlowError('signing', null, `Your wallet cannot see a coin of this transaction yet (your last transaction is not on the network after ${Math.round(seen.waitedMs / 1000)} s). Nothing was sent; try again in a moment.`, 'inputs-pending');
  }
}

/**
 * Runs the signing pipeline for one transaction, from the wallet's signature to the node's txid.
 * Each failure throws a `SignFlowError` that names its stage.
 */
export async function signAndSubmit(a: SignAndSubmitArgs): Promise<SignAndSubmitResult> {
  const { kob, node, adapter, network } = a;
  let built = a.built;
  let submitByDaa = a.submitByDaa;
  const stage = (s: SignStage, info?: { txid?: string }) => {
    try {
      a.onStage?.(s, info);
    } catch {
      /* a UI callback must not abort the flow */
    }
  };

  stage('signing');
  // A transaction nobody signs (an automatic creator lock's release, kob/creator-lock.ts) never goes
  // to the wallet: it is finalized with no signature, and every check after it still runs.
  const unsigned = built.sign.length === 0;
  if (!unsigned && (a.checkWallet ?? true) && adapter.refresh) await assertWalletMatches(adapter, built, network);
  if (a.inputAddress) await assertInputsOnNode(node, built, a.inputAddress);
  if (!unsigned) await assertVisibleToWallet(a, built);
  let sigs: Awaited<ReturnType<WalletAdapter['signTx']>> = [];
  const sleep = a.visibility?.sleep ?? ((ms: number) => new Promise<void>((r) => setTimeout(r, ms)));
  for (let n = 1; !unsigned; n++) {
    try {
      sigs = await adapter.signTx(built, { network, timeoutMs: a.timeoutMs, ...(a.retime ? { retime: true } : {}), ...(a.clockDaa !== undefined ? { clockDaa: a.clockDaa } : {}), ...(a.signerNotes?.length ? { signerNotes: a.signerNotes } : {}), ...(a.locks?.length ? { locks: a.locks } : {}), ...(a.live ? { live: a.live.signing } : {}), ...(a.session ? { session: a.session } : {}) });
      break;
    } catch (e) {
      // The wallet's own node has not seen a coin of it yet after all, through a slow relay. It is
      // asked again a moment later, at most UNKNOWN_ATTEMPTS times in all.
      if (e instanceof WalletError && e.code === 'inputs-unknown' && n < UNKNOWN_ATTEMPTS) {
        await sleep(UNKNOWN_RETRY_MS);
        continue;
      }
      if (isDeclinedFresh(e, adapter, built.tx)) throw new SignFlowError('signing', e, REJECTED_FRESH, 'rejected-fresh');
      const code = e instanceof WalletError ? e.code : 'other';
      throw new SignFlowError('signing', e, msg(e), code);
    }
  }
  // When the wallet signed a replacement it was given while the user read, that one is finalized,
  // checked and sent.
  const replaced = a.live?.signing.signed;
  if (a.live && replaced && replaced !== built) {
    built = replaced;
    submitByDaa = a.live.submitByOf(replaced);
  }

  stage('finalizing');
  let signed: SignedTx;
  try {
    signed = kob.finalize(built, sigs, { tightenBudgets: a.tighten ?? true });
  } catch (e) {
    throw new SignFlowError('finalizing', e, `The wallet's signature does not match this transaction, nothing was sent. (${msg(e)})`, 'signature');
  }

  if (a.validate ?? true) {
    stage('validating');
    try {
      kob.validate(signed);
    } catch (e) {
      throw new SignFlowError('validating', e, `The transaction failed the script check and was not sent. (${msg(e)})`, 'validation');
    }
  }

  if (submitByDaa !== undefined) await assertClockBefore(node, submitByDaa);

  stage('submitting');
  let txid: string;
  try {
    txid = await node.submitTransaction(signed.tx);
  } catch (e) {
    const err = e instanceof NodeError ? e : describeNodeError(e);
    throw new SignFlowError('submitting', err, err.message, err.code);
  }
  // The node's answer is not trusted as the txid. It must be the id of the very transaction that
  // was signed and validated.
  if (txid !== signed.tx.id) {
    throw new SignFlowError('submitting', null, `The node answered with transaction id ${String(txid).slice(0, 16)}... but the signed transaction is ${signed.tx.id.slice(0, 16)}...: its answer cannot be trusted. Check the explorer before retrying.`, 'txid-mismatch');
  }
  // What this transaction changes is read past the indexer's cache from now on (data/fresh.ts).
  markOwnAction();
  // Its outputs are fresh. The next transaction spending one waits until the wallet's node sees
  // it. Where the wallet can, the transaction is handed to that node.
  void noteSent(adapter, signed.tx);
  stage('submitted', { txid });
  return { txid, built, signed, fee: BigInt(signed.fee.fee) };
}

// --- Acceptance ---

/** An output to wait for in the node's UTXO set. */
export interface ExpectedOutput {
  /** The address the output pays to. The node indexes UTXOs by address. */
  address: string;
  transactionId: string;
  index: number;
}

/** The polling limits of `waitForAcceptance`. */
export interface AcceptanceOptions {
  timeoutMs?: number;
  pollMs?: number;
  /** Injectable, for deterministic tests. */
  sleep?: (ms: number) => Promise<void>;
  now?: () => number;
  signal?: AbortSignal;
}

/** The outcome of `waitForAcceptance`. */
export interface AcceptanceResult {
  /** Every expected output was seen in the node's UTXO set. */
  accepted: boolean;
  elapsedMs: number;
  found: NodeUtxo[];
  missing: ExpectedOutput[];
  /** The last node error seen while polling. Polling continues through transient errors. */
  lastError?: string;
}

/**
 * Polls the node until all `expected` outputs appear in its UTXO set, or the timeout hits. Their
 * appearance means the transaction was accepted into the DAG.
 *
 * The caller says what to look for, and `expectedOutputs()` derives it from a transaction. It
 * never throws for node hiccups.
 */
export async function waitForAcceptance(node: NodeApi, expected: ExpectedOutput | ExpectedOutput[], o: AcceptanceOptions = {}): Promise<AcceptanceResult> {
  const want = Array.isArray(expected) ? expected : [expected];
  const now = o.now ?? Date.now;
  const sleep = o.sleep ?? ((ms: number) => new Promise<void>((r) => setTimeout(r, ms)));
  const timeoutMs = o.timeoutMs ?? 120_000;
  const pollMs = o.pollMs ?? 1500;
  const t0 = now();
  const addresses = [...new Set(want.map((w) => w.address))];
  let found: NodeUtxo[] = [];
  let missing = want;
  let lastError: string | undefined;
  for (;;) {
    try {
      const utxos = await node.getUtxosByAddresses(addresses);
      const have = new Map(utxos.map((u) => [`${u.transactionId}:${u.index}`, u]));
      found = want.map((w) => have.get(`${w.transactionId}:${w.index}`)).filter((u): u is NodeUtxo => !!u);
      missing = want.filter((w) => !have.has(`${w.transactionId}:${w.index}`));
      if (missing.length === 0) return { accepted: true, elapsedMs: now() - t0, found, missing, lastError };
      lastError = undefined;
    } catch (e) {
      lastError = msg(e);
    }
    if (o.signal?.aborted || now() - t0 + pollMs > timeoutMs) return { accepted: false, elapsedMs: now() - t0, found, missing, lastError };
    await sleep(pollMs);
  }
}

/**
 * The outputs of `tx` to wait for, with their addresses derived through the SDK. `indices` picks
 * them, and all are taken when it is omitted.
 */
export function expectedOutputs(sdk: KaspaSdk, network: string, tx: TxJson, indices?: number[]): ExpectedOutput[] {
  const pick = indices ?? tx.outputs.map((_, i) => i);
  return pick.map((index) => {
    const out = tx.outputs[index];
    if (!out) throw new RangeError(`transaction has no output ${index}`);
    return { address: spkStringToAddress(sdk, out.scriptPublicKey, network), transactionId: tx.id, index };
  });
}
