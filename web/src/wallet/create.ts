// The signing pipeline of a TOKEN CREATION (KCC-23, two transactions): the preparation declaring the metadata, then the genesis spending its
// output 0. Like `signAndSubmit` (sign.ts), per transaction: the wallet signs every input -> kob.finalize verifies each signature and assembles
// the scripts -> kob.validate runs the script engine; then the node.
//
//   * One approval where the wallet can give it: the private-key signer shows both transactions on one confirmation (`signChain`). An
//     extension wallet's API signs one transaction per request, so it asks twice (the preparation, then the genesis). Both are signed BEFORE
//     anything is sent: declining the second request sends nothing.
//   * The genesis is signed before the preparation is on chain: a version 1 transaction id does not depend on signature scripts or compute
//     budgets, so the outpoint it spends is known, and tightening the preparation's budgets at finalize keeps it.
//   * A creation record (data/creations.ts) is written once both are signed and validated, before the preparation is sent, and removed once the
//     genesis is accepted. If the page dies in between, or the node refuses the genesis, the Create token page sends the saved genesis again
//     (`resumeCreation`); the preparation's output 0 is the wallet's own P2PK coin meanwhile, never at risk.
import type { NodeApi } from '../data/node';
import { NodeError, describeNodeError } from '../data/node-error';
import { markPrepSent, removeCreation, saveCreation, type CreationRecord } from '../data/creations';
import type { StorageLike } from '../config';
import { spendsPreparation, type IssueDeclaration, type IssueDocs, type IssueToken } from '../kob/issue';
import type { BuiltTx, InputSignature, SignedTx, TxJson } from '../kob/types';
import type { KobWasm } from '../kob/wasm';
import { SignFlowError, assertWalletMatches, type SignStage } from './sign';
import { WalletError, type WalletAdapter } from './types';
import type { GenesisClaim } from '../signer/protocol';

export interface CreationArgs {
  kob: KobWasm;
  node: NodeApi;
  adapter: WalletAdapter;
  /** step 1: the preparation */
  prep: BuiltTx;
  /** step 2: the genesis, spending `prep`'s output 0 */
  built: BuiltTx;
  network: string;
  /** what the creation record keeps to resume and to show the result */
  record: { token: IssueToken; declaration: IssueDeclaration; docs: IssueDocs };
  /** the creator's key (the record's owner) */
  maker: string;
  onStage?: (stage: SignStage, info?: { txid?: string; step?: 'prep' | 'genesis' }) => void;
  /** the node took the preparation (its inputs are spent from now on, whatever becomes of the genesis): the caller records that at once */
  onPrepSent?: (prep: SignedTx) => void;
  timeoutMs?: number;
  /** default true when the adapter can `refresh` */
  checkWallet?: boolean;
  /** default true; only tests turn it off */
  validate?: boolean;
  tighten?: boolean;
  /** where the creation record goes (default: localStorage; null: none) */
  storage?: StorageLike | null;
  /** how often and how long apart the genesis is offered while the node has not yet seen the preparation (default 5 x 1500 ms) */
  genesisAttempts?: number;
  genesisRetryMs?: number;
  sleep?: (ms: number) => Promise<void>;
}

export interface CreationResult {
  prepTxid: string;
  /** the genesis */
  txid: string;
  prepSigned: SignedTx;
  signed: SignedTx;
  /** both fees, sompi */
  fee: bigint;
}

/** Node answers that say a transaction was not taken (the others leave it open). */
const DEFINITE = new Set<string>(['orphan', 'double-spend', 'fee', 'script', 'invalid', 'network-mismatch']);

const msg = (e: unknown) => (e instanceof Error ? e.message : String(e));

/** The signatures of both transactions: one approval through `signChain`, else one `signTx` each. */
async function signBoth(a: CreationArgs): Promise<[InputSignature[], InputSignature[]]> {
  const opts = { network: a.network, ...(a.timeoutMs !== undefined ? { timeoutMs: a.timeoutMs } : {}) };
  try {
    if (a.adapter.signChain) return await a.adapter.signChain([a.prep, a.built], { ...opts, genesisClaim: genesisClaimOf(a.record.token) });
    const prep = await a.adapter.signTx(a.prep, opts);
    const genesis = await a.adapter.signTx(a.built, opts);
    return [prep, genesis];
  } catch (e) {
    throw new SignFlowError('signing', e, msg(e), e instanceof WalletError ? e.code : 'other');
  }
}

/** What the genesis's token outputs are claimed to hold, for a wallet that checks them (the private-key signer). */
export function genesisClaimOf(token: IssueToken): GenesisClaim {
  return {
    program: token.program,
    extensionCommitment: token.extensionCommitment,
    outputs: token.outputs.map((o) => ({ index: o.index, amount: o.amount, owner: o.owner, ownerScheme: o.ownerScheme, borrowScheme: o.borrowScheme, borrowGuard: o.borrowGuard })),
  };
}

/** Sends `tx`; a node that already has it counts as sent. */
async function send(node: NodeApi, tx: TxJson): Promise<void> {
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
 * Sends the genesis, again while the node answers that it does not know its input yet (the preparation just sent, not yet in the node's view):
 * `attempts` tries `retryMs` apart. Throws the last NodeError.
 */
async function sendGenesis(node: NodeApi, tx: TxJson, attempts: number, retryMs: number, sleep: (ms: number) => Promise<void>): Promise<void> {
  for (let n = 1; ; n++) {
    try {
      await send(node, tx);
      return;
    } catch (e) {
      if (!(e instanceof NodeError) || e.code !== 'orphan' || n >= attempts) throw e;
      await sleep(retryMs);
    }
  }
}

const genesisFailed = (prepTxid: string, e: unknown): SignFlowError => {
  const err = e instanceof NodeError ? e : describeNodeError(e);
  return new SignFlowError(
    'submitting',
    err,
    `The token details were recorded on chain (transaction ${prepTxid.slice(0, 16)}...), but the node did not take the token's creation: ${err.message} ` +
      'The KAS of that first step stays in your wallet. Open Create token again to finish the creation, or keep the KAS.',
    'genesis-pending',
  );
};

export async function signAndSubmitCreation(a: CreationArgs): Promise<CreationResult> {
  const { kob, node, prep, built } = a;
  const stage = (s: SignStage, info?: { txid?: string; step?: 'prep' | 'genesis' }) => {
    try {
      a.onStage?.(s, info);
    } catch {
      /* a UI callback must not abort the flow */
    }
  };
  const sleep = a.sleep ?? ((ms: number) => new Promise<void>((r) => setTimeout(r, ms)));

  stage('signing');
  if (!spendsPreparation(prep, built)) throw new SignFlowError('signing', null, 'The genesis does not spend the preparation of this creation. Nothing was sent.', 'other');
  if ((a.checkWallet ?? true) && a.adapter.refresh) {
    await assertWalletMatches(a.adapter, prep, a.network);
    await assertWalletMatches(a.adapter, built, a.network);
  }
  const [prepSigs, genesisSigs] = await signBoth(a);

  stage('finalizing');
  let prepSigned: SignedTx;
  let signed: SignedTx;
  try {
    prepSigned = kob.finalize(prep, prepSigs, { tightenBudgets: a.tighten ?? true });
    signed = kob.finalize(built, genesisSigs, { tightenBudgets: a.tighten ?? true });
  } catch (e) {
    throw new SignFlowError('finalizing', e, `The wallet's signature does not match this transaction, nothing was sent. (${msg(e)})`, 'signature');
  }
  // the ids the wallet signed for: the genesis must still spend the preparation as signed
  if (prepSigned.tx.id !== prep.tx.id || signed.tx.id !== built.tx.id || signed.tx.inputs[0]?.transactionId !== prepSigned.tx.id) {
    throw new SignFlowError('finalizing', null, 'The signed transactions do not chain as planned (the genesis must spend the preparation). Nothing was sent.', 'signature');
  }

  if (a.validate ?? true) {
    stage('validating');
    try {
      kob.validate(prepSigned);
      kob.validate(signed);
    } catch (e) {
      throw new SignFlowError('validating', e, `The transaction failed the script check and was not sent. (${msg(e)})`, 'validation');
    }
  }

  // the record first: from here on the page may die between the two
  const storage = a.storage === undefined ? undefined : a.storage;
  const record: CreationRecord = {
    network: a.network, maker: a.maker.toLowerCase(), createdAt: Date.now(), covenantId: a.record.token.covenantId, prepTxid: prepSigned.tx.id,
    genesisTxid: signed.tx.id, prep: prepSigned.tx, genesis: signed.tx, token: a.record.token, declaration: a.record.declaration, docs: a.record.docs,
    prepSent: false,
  };
  saveCreation(record, storage);

  stage('submitting', { step: 'prep' });
  try {
    await send(node, prepSigned.tx);
  } catch (e) {
    // a definite refusal: nothing went out, the record is of no use; an unclear failure (the connection) keeps it, the preparation may be in
    if (e instanceof SignFlowError || (e instanceof NodeError && DEFINITE.has(e.code))) removeCreation(a.network, record.covenantId, storage);
    if (e instanceof SignFlowError) throw e;
    const err = e as NodeError;
    throw new SignFlowError('submitting', err, err.message, err.code);
  }
  markPrepSent(a.network, record.covenantId, storage);
  try {
    a.onPrepSent?.(prepSigned);
  } catch {
    /* a UI callback must not abort the flow */
  }
  stage('submitting', { step: 'genesis', txid: prepSigned.tx.id });
  try {
    await sendGenesis(node, signed.tx, a.genesisAttempts ?? 5, a.genesisRetryMs ?? 1500, sleep);
  } catch (e) {
    if (e instanceof SignFlowError) throw e;
    throw genesisFailed(prepSigned.tx.id, e);
  }
  removeCreation(a.network, record.covenantId, storage);
  stage('submitted', { txid: signed.tx.id });
  return { prepTxid: prepSigned.tx.id, txid: signed.tx.id, prepSigned, signed, fee: BigInt(prepSigned.fee.fee) + BigInt(signed.fee.fee) };
}

/**
 * Finishes a saved creation: sends its preparation again (a node that has it, or has it in a block, refuses it: that is fine) and its signed
 * genesis as it is. Nothing is signed. The record is removed once the genesis is accepted. Throws a SignFlowError (`genesis-pending`) otherwise.
 */
export async function resumeCreation(
  node: NodeApi,
  rec: CreationRecord,
  opts: { storage?: StorageLike | null; attempts?: number; retryMs?: number; sleep?: (ms: number) => Promise<void> } = {},
): Promise<string> {
  const sleep = opts.sleep ?? ((ms: number) => new Promise<void>((r) => setTimeout(r, ms)));
  if (rec.genesis.inputs[0]?.transactionId !== rec.prepTxid) throw new SignFlowError('submitting', null, 'The saved genesis does not spend the saved preparation.', 'other');
  try {
    await send(node, rec.prep);
  } catch {
    /* already in a block, or spent: the genesis decides */
  }
  try {
    await sendGenesis(node, rec.genesis, opts.attempts ?? 3, opts.retryMs ?? 1500, sleep);
  } catch (e) {
    if (e instanceof SignFlowError) throw e;
    throw genesisFailed(rec.prepTxid, e);
  }
  removeCreation(rec.network, rec.covenantId, opts.storage);
  return rec.genesisTxid;
}
