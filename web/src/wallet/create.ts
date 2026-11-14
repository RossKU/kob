// The signing pipeline of a TOKEN CREATION (KCC-23, two transactions): the preparation declaring the metadata, then the genesis spending its
// output 0. Both go through the chain runner (chain.ts): per transaction, the wallet signs every input -> kob.finalize verifies each signature
// and assembles the scripts -> kob.validate runs the script engine; then the node.
//
//   * One approval where the wallet can give it: the private-key signer shows both transactions on one confirmation (`signChain`), both are
//     signed before anything is sent.
//   * An extension wallet signs one transaction per request, and looks every input up on its own node (KasWare 0.11.5): the genesis spends an
//     output nobody broadcast until the preparation is sent. So the preparation is signed and SENT first; the wallet is asked for the genesis once
//     that output is visible to its node (visibility.ts). Declining the second request leaves the preparation on chain: its output 0 (the
//     genesis's carriers and fee) stays an ordinary coin of the wallet, and only the first step's network fee is spent.
//   * The genesis is planned before anything is signed: a version 1 transaction id does not depend on signature scripts or compute budgets, so
//     the outpoint it spends is known, and tightening the preparation's budgets at finalize keeps it.
//   * A creation record (data/creations.ts) is written once the preparation is signed and validated, before it is sent; it holds the genesis
//     as planned and, once signed, signed. It is removed once the genesis is accepted. If the page dies in between, the user declines the
//     second approval, or the node refuses the genesis, the Create token page finishes it later (`finishCreation`).
import type { NodeApi } from '../data/node';
import { NodeError, describeNodeError } from '../data/node-error';
import { genesisSigned, markPrepSent, removeCreation, saveCreation, type CreationRecord } from '../data/creations';
import type { StorageLike } from '../config';
import { spendsPreparation, type IssueDeclaration, type IssueDocs, type IssueToken } from '../kob/issue';
import type { BuiltTx, SignedTx } from '../kob/types';
import type { KobWasm } from '../kob/wasm';
import { ChainError, runSignChain, sendTx, sendTxRetrying } from './chain';
import { SignFlowError, type SignStage } from './sign';
import type { WalletAdapter } from './types';
import { noteSent } from './visibility';
import type { GenesisClaim } from '../signer/protocol';

export type CreationStep = 'prep' | 'genesis';

export interface CreationStageInfo {
  txid?: string;
  step?: CreationStep;
  /** waiting until the wallet's node sees the preparation (the wallet is not asked for the genesis yet) */
  waiting?: boolean;
}

export interface CreationArgs {
  kob: KobWasm;
  node: NodeApi;
  adapter: WalletAdapter;
  /** step 1: the preparation */
  prep: BuiltTx;
  /** step 2: the genesis, spending `prep`'s output 0 */
  built: BuiltTx;
  network: string;
  /** script public key -> address of the app's network (the visibility gate asks the node by address) */
  addressOf: (spk: string) => string;
  /** what the creation record keeps to resume and to show the result */
  record: { token: IssueToken; declaration: IssueDeclaration; docs: IssueDocs };
  /** the creator's key (the record's owner) */
  maker: string;
  onStage?: (stage: SignStage, info?: CreationStageInfo) => void;
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
  /** the chain runner's waits (chain.ts): tests shorten them */
  visibleTimeoutMs?: number;
  visiblePollMs?: number;
  settleMs?: number;
  unknownRetryMs?: number;
  sleep?: (ms: number) => Promise<void>;
  now?: () => number;
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

/** What the genesis's token outputs are claimed to hold, for a wallet that checks them (the private-key signer). */
export function genesisClaimOf(token: IssueToken): GenesisClaim {
  return {
    program: token.program,
    extensionCommitment: token.extensionCommitment,
    outputs: token.outputs.map((o) => ({ index: o.index, amount: o.amount, owner: o.owner, ownerScheme: o.ownerScheme, borrowScheme: o.borrowScheme, borrowGuard: o.borrowGuard })),
  };
}

/**
 * The creation could not be finished now, but its preparation is on chain: a SignFlowError `genesis-pending` (never signed again on the
 * confirmation screen; the Create token page finishes it). Its message says why: declined, the wallet could not see the first step, or the node.
 */
function genesisPending(prepTxid: string, e: unknown): SignFlowError {
  const first = `The token details were recorded on chain (transaction ${prepTxid.slice(0, 16)}...), but the token itself was not created`;
  const after = 'The KAS of that first step stays in your wallet. Open Create token to finish the creation, or keep the KAS.';
  if (e instanceof SignFlowError && e.stage === 'signing') {
    if (e.code === 'rejected') {
      return new SignFlowError('signing', e, `${first}: the second approval was declined. ${after} If your wallet showed a warning, wait a few seconds before you finish it.`, 'genesis-pending');
    }
    return new SignFlowError('signing', e, `${first}: ${e.message} ${after}`, 'genesis-pending');
  }
  if (e instanceof SignFlowError && e.stage !== 'submitting') return new SignFlowError(e.stage, e, `${first}: ${e.message} ${after}`, 'genesis-pending');
  const err = e instanceof SignFlowError ? e : e instanceof NodeError ? e : describeNodeError(e);
  return new SignFlowError('submitting', err, `${first}: the node did not take the token's creation: ${err.message} ${after}`, 'genesis-pending');
}

export async function signAndSubmitCreation(a: CreationArgs): Promise<CreationResult> {
  const { prep, built } = a;
  const stage = (s: SignStage, info?: CreationStageInfo) => {
    try {
      a.onStage?.(s, info);
    } catch {
      /* a UI callback must not abort the flow */
    }
  };
  const storage = a.storage === undefined ? undefined : a.storage;
  const now = a.now ?? Date.now;

  if (!spendsPreparation(prep, built)) {
    stage('signing');
    throw new SignFlowError('signing', null, 'The genesis does not spend the preparation of this creation. Nothing was sent.', 'other');
  }
  const base = {
    network: a.network, maker: a.maker.toLowerCase(), createdAt: now(), covenantId: a.record.token.covenantId, prepTxid: prep.tx.id,
    genesisTxid: built.tx.id, genesisBuilt: built, token: a.record.token, declaration: a.record.declaration, docs: a.record.docs,
  };
  let saved = false;
  let prepSent = false;
  let prepSentAt: number | undefined;
  try {
    const res = await runSignChain({
      kob: a.kob, node: a.node, adapter: a.adapter, network: a.network, steps: [prep, built], addressOf: a.addressOf,
      signChainOptions: { genesisClaim: genesisClaimOf(a.record.token) },
      ...(a.timeoutMs !== undefined ? { timeoutMs: a.timeoutMs } : {}),
      ...(a.checkWallet !== undefined ? { checkWallet: a.checkWallet } : {}),
      ...(a.validate !== undefined ? { validate: a.validate } : {}),
      ...(a.tighten !== undefined ? { tighten: a.tighten } : {}),
      sendAttempts: a.genesisAttempts ?? 5,
      sendRetryMs: a.genesisRetryMs ?? 1500,
      ...(a.visibleTimeoutMs !== undefined ? { visibleTimeoutMs: a.visibleTimeoutMs } : {}),
      ...(a.visiblePollMs !== undefined ? { visiblePollMs: a.visiblePollMs } : {}),
      ...(a.settleMs !== undefined ? { settleMs: a.settleMs } : {}),
      ...(a.unknownRetryMs !== undefined ? { unknownRetryMs: a.unknownRetryMs } : {}),
      ...(a.sleep ? { sleep: a.sleep } : {}),
      ...(a.now ? { now: a.now } : {}),
      // the record first: from here on the page may die between the two
      onSigned: (signed) => {
        const rec: CreationRecord = {
          ...base, prep: signed[0]!.tx, ...(signed[1] ? { genesis: signed[1].tx } : {}), prepSent, ...(prepSentAt !== undefined ? { prepSentAt } : {}),
        };
        saved = saveCreation(rec, storage) || saved;
      },
      onSent: (k, signed) => {
        if (k !== 0) return;
        prepSent = true;
        prepSentAt = now();
        markPrepSent(a.network, base.covenantId, storage, prepSentAt);
        try {
          a.onPrepSent?.(signed);
        } catch {
          /* a UI callback must not abort the flow */
        }
      },
      onStage: (s, info) => stage(s, { step: info.step === 0 ? 'prep' : 'genesis', ...(info.txid ? { txid: info.txid } : {}), ...(info.waiting !== undefined ? { waiting: info.waiting } : {}) }),
    });
    removeCreation(a.network, base.covenantId, storage);
    const [prepSigned, signed] = res.signed as [SignedTx, SignedTx];
    return { prepTxid: prepSigned.tx.id, txid: signed.tx.id, prepSigned, signed, fee: res.fee };
  } catch (e) {
    const sentSteps = e instanceof ChainError ? e.sent : prepSent ? 1 : 0;
    if (sentSteps >= 1) throw genesisPending(prep.tx.id, e);
    // nothing went out. A definite refusal of the preparation, or an answer that cannot be trusted: the record is of no use. An unclear failure
    // (the connection) keeps it: the preparation may be in
    if (e instanceof SignFlowError && e.stage === 'submitting') {
      const cause = e.cause;
      if (e.code === 'txid-mismatch' || (cause instanceof NodeError && DEFINITE.has(cause.code))) removeCreation(a.network, base.covenantId, storage);
    } else if (saved) {
      // signed and recorded but failed before the send (cannot happen after a successful record today): nothing went out
      removeCreation(a.network, base.covenantId, storage);
    }
    if (e instanceof ChainError) throw new SignFlowError(e.stage, e.cause, e.message, e.code);
    throw e;
  }
}

export interface FinishOptions {
  storage?: StorageLike | null;
  /** genesis offers while the node does not see the preparation (default 3 x 1500 ms) */
  attempts?: number;
  retryMs?: number;
  sleep?: (ms: number) => Promise<void>;
}

/**
 * Finishes a saved creation whose genesis is signed: sends its preparation again (a node that has it, or has it in a block, refuses it: that
 * is fine) and its signed genesis as it is. Nothing is signed. The record is removed once the genesis is accepted. Throws a SignFlowError
 * (`genesis-pending`) otherwise.
 */
export async function resumeCreation(node: NodeApi, rec: CreationRecord, opts: FinishOptions = {}): Promise<string> {
  const sleep = opts.sleep ?? ((ms: number) => new Promise<void>((r) => setTimeout(r, ms)));
  if (!genesisSigned(rec)) throw new SignFlowError('submitting', null, 'The saved genesis is not signed yet: your wallet must sign it.', 'genesis-unsigned');
  if (rec.genesis.inputs[0]?.transactionId !== rec.prepTxid) throw new SignFlowError('submitting', null, 'The saved genesis does not spend the saved preparation.', 'other');
  try {
    await sendTx(node, rec.prep);
  } catch {
    /* already in a block, or spent: the genesis decides */
  }
  try {
    await sendTxRetrying(node, rec.genesis, opts.attempts ?? 3, opts.retryMs ?? 1500, sleep);
  } catch (e) {
    throw genesisPending(rec.prepTxid, e);
  }
  removeCreation(rec.network, rec.covenantId, opts.storage);
  return rec.genesisTxid;
}

export interface FinishArgs extends FinishOptions {
  kob: KobWasm;
  node: NodeApi;
  /** the wallet of the record's key (it signs the genesis when the record holds none signed) */
  adapter: WalletAdapter;
  network: string;
  rec: CreationRecord;
  addressOf: (spk: string) => string;
  onStage?: (stage: SignStage, info?: CreationStageInfo) => void;
  timeoutMs?: number;
  checkWallet?: boolean;
  validate?: boolean;
  tighten?: boolean;
  /** how long the preparation, sent again, may take to be listed by the node (default 60 s, polled every 1 s) */
  prepWaitMs?: number;
  prepPollMs?: number;
  visibleTimeoutMs?: number;
  visiblePollMs?: number;
  settleMs?: number;
  unknownRetryMs?: number;
  now?: () => number;
}

/** The coin the genesis spends is gone (spent by another transaction): the creation cannot be finished. */
export const CREATION_SPENT = 'creation-spent';

/**
 * Finishes a saved creation (the Create token page, "Finish the creation"). A signed genesis is sent as it is (`resumeCreation`). An unsigned
 * one needs the wallet once: when the preparation's output 0 is in the node's UTXO set it is signed and sent at once; otherwise the saved
 * preparation is sent again and its output awaited first. A preparation the node refuses for good while its output is not listed means the
 * coin was spent elsewhere: SignFlowError `creation-spent` (only forgetting the record is left). Returns the genesis txid.
 */
export async function finishCreation(a: FinishArgs): Promise<string> {
  const { node, rec } = a;
  const sleep = a.sleep ?? ((ms: number) => new Promise<void>((r) => setTimeout(r, ms)));
  const now = a.now ?? Date.now;
  if (genesisSigned(rec)) return resumeCreation(node, rec, a);
  const built = rec.genesisBuilt;
  if (!built || !spendsPreparation({ tx: rec.prep } as BuiltTx, built) || built.tx.id !== rec.genesisTxid) {
    throw new SignFlowError('signing', null, 'The saved genesis does not spend the saved preparation.', 'other');
  }
  const out0 = rec.prep.outputs[0]!;
  const address = a.addressOf(out0.scriptPublicKey);
  const listed = async (): Promise<boolean> => (await node.getUtxosByAddresses([address])).some((u) => u.transactionId === rec.prepTxid && u.index === 0);

  // the preparation's output 0 must be on the node before the wallet is asked
  let sentAt = rec.prepSentAt ?? rec.createdAt;
  if (!(await listed().catch(() => false))) {
    try {
      await sendTx(node, rec.prep);
    } catch (e) {
      const err = e instanceof NodeError ? e : e instanceof SignFlowError ? e : describeNodeError(e);
      if (err instanceof NodeError && DEFINITE.has(err.code) && !(await listed().catch(() => false))) {
        throw new SignFlowError('submitting', err, 'The coin of the first step was spent, so this creation cannot be finished. Start a new one.', CREATION_SPENT);
      }
      // the connection, or a refusal while the output is listed after all (it reached a block meanwhile): try again later
      if (!(await listed().catch(() => false))) throw genesisPending(rec.prepTxid, err);
    }
    sentAt = now();
    markPrepSent(rec.network, rec.covenantId, a.storage, sentAt);
    // handed to the wallet's node too, where the wallet can take it (visibility.ts)
    void noteSent(a.adapter, rec.prep, a.now ? { now: a.now } : {});
    const wait = a.prepWaitMs ?? 60_000;
    const poll = a.prepPollMs ?? 1000;
    const t0 = now();
    for (;;) {
      if (await listed().catch(() => false)) break;
      if (now() - t0 + poll > wait) throw genesisPending(rec.prepTxid, new SignFlowError('signing', null, 'The first step is not on chain yet, so your wallet was not asked.', 'inputs-pending'));
      await sleep(poll);
    }
  }

  const res = await runSignChain({
    kob: a.kob, node, adapter: a.adapter, network: a.network, steps: [built], addressOf: a.addressOf,
    alsoFresh: new Map([[rec.prepTxid, sentAt]]),
    orphanRetry: () => true,
    sendAttempts: a.attempts ?? 3,
    sendRetryMs: a.retryMs ?? 1500,
    sleep,
    ...(a.now ? { now: a.now } : {}),
    ...(a.timeoutMs !== undefined ? { timeoutMs: a.timeoutMs } : {}),
    ...(a.checkWallet !== undefined ? { checkWallet: a.checkWallet } : {}),
    ...(a.validate !== undefined ? { validate: a.validate } : {}),
    ...(a.tighten !== undefined ? { tighten: a.tighten } : {}),
    ...(a.visibleTimeoutMs !== undefined ? { visibleTimeoutMs: a.visibleTimeoutMs } : {}),
    ...(a.visiblePollMs !== undefined ? { visiblePollMs: a.visiblePollMs } : {}),
    ...(a.settleMs !== undefined ? { settleMs: a.settleMs } : {}),
    ...(a.unknownRetryMs !== undefined ? { unknownRetryMs: a.unknownRetryMs } : {}),
    onSigned: ([genesis]) => {
      saveCreation({ ...rec, genesis: genesis!.tx, prepSent: true, prepSentAt: sentAt }, a.storage);
    },
    onStage: (s, info) => {
      try {
        a.onStage?.(s, { step: 'genesis', ...(info.txid ? { txid: info.txid } : {}), ...(info.waiting !== undefined ? { waiting: info.waiting } : {}) });
      } catch {
        /* a UI callback must not abort the flow */
      }
    },
  }).catch((e: unknown) => {
    // declined, or not visible yet: the record stays as it was (finish again later); the rest says what happened
    if (e instanceof ChainError) throw new SignFlowError(e.stage, e.cause, e.message, e.code);
    throw e;
  });
  removeCreation(rec.network, rec.covenantId, a.storage);
  return res.signed[0]!.tx.id;
}
