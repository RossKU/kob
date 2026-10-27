// The signing pipeline every transaction of the app goes through:
//
//   built (kob.build) -> signing (wallet signs each digest) -> finalizing (kob.finalize VERIFIES each signature against the digest and
//   assembles the scripts) -> validating (kob.validate: the script engine runs the covenants; nothing invalid is ever broadcast)
//   -> submitting (node RPC) -> submitted { txid }
//
// The wallet only ever sees the unsigned transaction the user confirmed. What it returns is reduced to signatures, which must verify
// against digests kob-wasm computed from OUR transaction: a wallet that altered the transaction, signed the wrong key or a different
// sighash type is caught at `finalizing`, before anything is broadcast.
import type { BuiltTx, SignedTx, TxJson } from '../kob/types';
import type { KobWasm } from '../kob/wasm';
import type { NodeApi, NodeUtxo } from '../data/node';
import { NodeError, describeNodeError } from '../data/node-error';
import { confirmInputsOnNode } from '../kob/node-verify';
import { normalizeNetwork, spkStringToAddress, type KaspaSdk } from '../data/kaspa-sdk';
import { WalletError, type WalletAdapter, type LiveSigning } from './types';

export type SignStage = 'signing' | 'finalizing' | 'validating' | 'submitting' | 'submitted';
export type FailedStage = Exclude<SignStage, 'submitted'>;

export class SignFlowError extends Error {
  readonly stage: FailedStage;
  readonly cause: unknown;
  /** `WalletError.code` / `NodeError.code` of the cause, or `signature` (finalize refused) / `validation` (script engine refused) / `other` */
  readonly code: string;
  constructor(stage: FailedStage, cause: unknown, message: string, code: string) {
    super(message);
    this.name = 'SignFlowError';
    this.stage = stage;
    this.cause = cause;
    this.code = code;
  }
  /** the user declined in the wallet: not an error to show as one */
  get rejectedByUser(): boolean {
    return this.stage === 'signing' && this.code === 'rejected';
  }
}

export interface SignAndSubmitArgs {
  kob: KobWasm;
  node: NodeApi;
  adapter: WalletAdapter;
  built: BuiltTx;
  /** network name (`mainnet` / `testnet-10`); wallets that need it (Kastle) get it */
  network: string;
  /** run the in-wasm script engine before broadcasting (default true; only tests turn it off) */
  validate?: boolean;
  /** shrink the compute budgets to what the scripts use, lowering the fee (default true) */
  tighten?: boolean;
  onStage?: (stage: SignStage, info?: { txid?: string }) => void;
  /** wallet popup timeout, ms (default: `SIGN_TIMEOUT_MS`) */
  timeoutMs?: number;
  /**
   * re-read the wallet (silently) right before asking it to sign and refuse when its account or network is no longer the one the plan was
   * built for (default true when the adapter can `refresh`)
   */
  checkWallet?: boolean;
  /**
   * script public key -> address of the configured network. When given, every input of the transaction is re-read from the NODE right before
   * the wallet is asked to sign, and any input whose amount, script or covenant id the node does not confirm aborts the flow (the KAS of unsigned
   * covenant inputs is committed by no signature, so the fee shown must be over chain values). The order send (ui/confirm/send.ts) omits it: its
   * own pre-sign check has just read every input on the node and the decoder stops any it does not confirm. Tests with synthetic chains omit it.
   */
  inputAddress?: (spk: string) => string;
  /**
   * the last DAA score at which this transaction may still be sent (an auction that starts at the clock it was planned with: `auctionSubmitBy`).
   * The node clock is read once the wallet has signed; a clock past it aborts the flow before anything is broadcast (`clock-moved`): the price
   * of the auction has moved on while the wallet was waiting for approval, so the caller plans it again with the current clock.
   */
  submitByDaa?: bigint;
  /** the transaction re-times the one the wallet signed last (`SignOptions.retime`) */
  retime?: boolean;
  /** the node's DAA score now, for the times the private-key signer says (`SignOptions.clockDaa`) */
  clockDaa?: bigint;
  /**
   * the transaction may be replaced while the user reads it (`SignOptions.live`): `submitByOf` gives the `submitByDaa` of a replacement the
   * wallet signed, which is then the one finalized, checked and sent
   */
  live?: { signing: LiveSigning; submitByOf(built: BuiltTx): bigint | undefined };
}

/**
 * How far (DAA, 2 s at 10 per second) past its start an auction planned at the click may still be sent. An auction of a market or marketable
 * order starts `MARKET_ACTIVATION_DAA` after the clock it was planned with and moves its price from that start on: a wallet that keeps the
 * request open longer than that lead plus this margin returns a signature for an auction already this far down its price path.
 */
export const AUCTION_START_MARGIN_DAA = 20n;

/**
 * The `submitByDaa` of a transaction that creates `states` (the planner's `{ kind, state }` order states): the earliest start (plus
 * `AUCTION_START_MARGIN_DAA`) of an auction among them that starts at or after `plannedAtDaa`, the clock it was planned with (an amended order
 * that keeps an older start is not one). Null: no such auction.
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

/** The node clock must still be at or before `by` (`SignAndSubmitArgs.submitByDaa`); a clock that cannot be read counts as moved. */
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

export interface SignAndSubmitResult {
  /** id returned by the node */
  txid: string;
  /** the transaction sent: `built`, or the replacement of it the wallet signed (`live`) */
  built: BuiltTx;
  signed: SignedTx;
  /** network fee, sompi */
  fee: bigint;
}

const msg = (e: unknown) => (e instanceof Error ? e.message : String(e));

/**
 * The plan was built for one account on one network. A wallet that switched since (an event the UI may not have handled yet, or a
 * confirmation screen left open) must not be asked to sign it: the popup would show another account, or the signature would be for a key
 * the plan does not spend.
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
  let res;
  try {
    res = await confirmInputsOnNode(node, built.tx.inputs, toAddress);
  } catch (e) {
    const err = e instanceof NodeError ? e : describeNodeError(e);
    throw new SignFlowError('signing', err, `The node could not confirm the inputs of this transaction (${err.message}). Nothing was sent.`, 'inputs-unconfirmed');
  }
  if (res.mismatches.length) {
    const m = res.mismatches[0];
    throw new SignFlowError('signing', null, `Input ${m.index} does not match the node (${m.detail}). The transaction was planned from stale or wrong data: build it again. Nothing was sent.`, 'inputs-changed');
  }
}

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
  if ((a.checkWallet ?? true) && adapter.refresh) await assertWalletMatches(adapter, built, network);
  if (a.inputAddress) await assertInputsOnNode(node, built, a.inputAddress);
  let sigs;
  try {
    sigs = await adapter.signTx(built, { network, timeoutMs: a.timeoutMs, ...(a.retime ? { retime: true } : {}), ...(a.clockDaa !== undefined ? { clockDaa: a.clockDaa } : {}), ...(a.live ? { live: a.live.signing } : {}) });
  } catch (e) {
    const code = e instanceof WalletError ? e.code : 'other';
    throw new SignFlowError('signing', e, msg(e), code);
  }
  // the wallet signed a replacement it was given while the user read: that one is finalized, checked and sent
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
  // the node's answer is not trusted as "the" txid: it must be the id of the very transaction that was signed and validated
  if (txid !== signed.tx.id) {
    throw new SignFlowError('submitting', null, `The node answered with transaction id ${String(txid).slice(0, 16)}... but the signed transaction is ${signed.tx.id.slice(0, 16)}...: its answer cannot be trusted. Check the explorer before retrying.`, 'txid-mismatch');
  }
  stage('submitted', { txid });
  return { txid, built, signed, fee: BigInt(signed.fee.fee) };
}

// ------------------------------------------------------------------------------------------------ acceptance

export interface ExpectedOutput {
  /** address the output pays to (the node indexes UTXOs by address) */
  address: string;
  transactionId: string;
  index: number;
}

export interface AcceptanceOptions {
  timeoutMs?: number;
  pollMs?: number;
  /** injectable for deterministic tests */
  sleep?: (ms: number) => Promise<void>;
  now?: () => number;
  signal?: AbortSignal;
}

export interface AcceptanceResult {
  /** every expected output was seen in the node's UTXO set */
  accepted: boolean;
  elapsedMs: number;
  found: NodeUtxo[];
  missing: ExpectedOutput[];
  /** last node error seen while polling (polling continues through transient errors) */
  lastError?: string;
}

/**
 * Polls the node until ALL `expected` outputs appear in its UTXO set (= the transaction was accepted into the DAG), or the timeout hits.
 * The caller says what to look for (`expectedOutputs()` derives it from a transaction). Never throws for node hiccups.
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

/** The outputs of `tx` to wait for (`indices`: all when omitted), with their addresses derived through the SDK. */
export function expectedOutputs(sdk: KaspaSdk, network: string, tx: TxJson, indices?: number[]): ExpectedOutput[] {
  const pick = indices ?? tx.outputs.map((_, i) => i);
  return pick.map((index) => {
    const out = tx.outputs[index];
    if (!out) throw new RangeError(`transaction has no output ${index}`);
    return { address: spkStringToAddress(sdk, out.scriptPublicKey, network), transactionId: tx.id, index };
  });
}
