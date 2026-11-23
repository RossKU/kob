// The signing pipeline of a token creation (KCC-23), which is two transactions. The preparation
// declares the metadata, and the genesis spends its output 0.
//
// Both go through the chain runner (chain.ts). For each transaction the wallet signs every input,
// `kob.finalize` verifies each signature and assembles the scripts, and `kob.validate` runs the
// script engine. Then it goes to the node.
//
// - One approval where the wallet can give it. The private-key signer shows both transactions on
//   one confirmation (`signChain`), and both are signed before anything is sent.
// - An extension wallet signs one transaction per request, and looks every input up on its own
//   node (KasWare 0.11.5). The genesis spends an output nobody broadcast until the preparation is
//   sent. So the preparation is signed and sent first, and the wallet is asked for the genesis
//   once that output is visible to its node (visibility.ts). Declining the second request leaves
//   the preparation on chain. Its output 0, the genesis's carriers and fee, stays an ordinary coin
//   of the wallet, and only the first step's network fee is spent.
// - The genesis is planned before anything is signed. A version 1 transaction id does not depend
//   on signature scripts or compute budgets, so the outpoint it spends is known, and tightening the
//   preparation's budgets at finalize keeps it.
// - A creation record (data/creations.ts) is written once the preparation is signed and
//   validated, before it is sent. It holds the genesis as planned and, once signed, as signed. It
//   is removed once the genesis is accepted. If the page dies in between, the user declines the
//   second approval or the node refuses the genesis, the Create token page finishes it later
//   (`finishCreation`).
// - The record is untrusted input, because anything that writes this origin's localStorage can
//   plant one. `finishCreation` never signs its stored genesis. It plans the creation again
//   (creation-replan.ts), checks that plan as a fresh one (issuance-model.ts) and checks the
//   preparation's output 0 on the node. Then it signs the plan.
import type { NodeApi } from '../data/node';
import { NodeError, describeNodeError } from '../data/node-error';
import { genesisSigned, markPrepSent, removeCreation, saveCreation, type CreationRecord } from '../data/creations';
import type { StorageLike } from '../config';
import { spendsPreparation, type IssueCurve, type IssueDeclaration, type IssueDocs, type IssueResult, type IssueToken } from '../kob/issue';
import type { BuiltTx, SignedTx } from '../kob/types';
import { buildIssuanceModel, type IssuanceInput } from '../ui/confirm/issuance-model';
import { replanCreation } from './creation-replan';
import type { KobWasm } from '../kob/wasm';
import { ChainError, runSignChain, sendTx, sendTxRetrying } from './chain';
import { SignFlowError, type SignStage } from './sign';
import type { WalletAdapter } from './types';
import { noteSent } from './visibility';
import type { GenesisClaim } from '../signer/protocol';

/** The step of a creation a stage belongs to. */
export type CreationStep = 'prep' | 'genesis';

/** What a creation stage reports beyond its stage. */
export interface CreationStageInfo {
  txid?: string;
  step?: CreationStep;
  /**
   * Waiting until the wallet's node sees the preparation. The wallet is not asked for the genesis
   * yet.
   */
  waiting?: boolean;
}

/** The inputs of `signAndSubmitCreation`. */
export interface CreationArgs {
  kob: KobWasm;
  node: NodeApi;
  adapter: WalletAdapter;
  /** Step 1, the preparation. */
  prep: BuiltTx;
  /** Step 2, the genesis, spending `prep`'s output 0. */
  built: BuiltTx;
  network: string;
  /**
   * Maps a script public key to an address of the app's network. The visibility gate asks the node
   * by address.
   */
  addressOf: (spk: string) => string;
  /** What the creation record keeps to resume and to show the result. */
  record: { token: IssueToken; declaration: IssueDeclaration; docs: IssueDocs; curve?: IssueCurve; askSale?: { sell: string; price: string } };
  /** The creator's key, which owns the record. */
  maker: string;
  onStage?: (stage: SignStage, info?: CreationStageInfo) => void;
  /**
   * Called when the node took the preparation. Its inputs are spent from then on, whatever becomes
   * of the genesis, so the caller records that at once.
   */
  onPrepSent?: (prep: SignedTx) => void;
  timeoutMs?: number;
  /** The default is true when the adapter can `refresh`. */
  checkWallet?: boolean;
  /** The default is true, and only tests turn it off. */
  validate?: boolean;
  tighten?: boolean;
  /** Where the creation record goes. The default is localStorage, and null keeps none. */
  storage?: StorageLike | null;
  /**
   * How often and how far apart the genesis is offered while the node has not yet seen the
   * preparation. The default is 5 times, 1500 ms apart.
   */
  genesisAttempts?: number;
  genesisRetryMs?: number;
  /** The chain runner's waits (chain.ts). Tests shorten them. */
  visibleTimeoutMs?: number;
  visiblePollMs?: number;
  settleMs?: number;
  unknownRetryMs?: number;
  sleep?: (ms: number) => Promise<void>;
  now?: () => number;
}

/** What a finished creation sent. */
export interface CreationResult {
  prepTxid: string;
  /** The genesis. */
  txid: string;
  prepSigned: SignedTx;
  signed: SignedTx;
  /** Both fees, in sompi. */
  fee: bigint;
}

/** Node answers that say a transaction was not taken. The others leave it open. */
const DEFINITE = new Set<string>(['orphan', 'double-spend', 'fee', 'script', 'invalid', 'network-mismatch']);

/**
 * What the genesis's token outputs are claimed to hold, for a wallet that checks them. That is the
 * private-key signer.
 */
export function genesisClaimOf(token: IssueToken, curve?: IssueCurve): GenesisClaim {
  return {
    program: token.program,
    extensionCommitment: token.extensionCommitment,
    outputs: token.outputs.map((o) => ({ index: o.index, amount: o.amount, owner: o.owner, ownerScheme: o.ownerScheme, borrowScheme: o.borrowScheme, borrowGuard: o.borrowGuard })),
    ...(curve ? { curve: { index: curve.output, covenantId: curve.covenantId, state: curve.state } } : {}),
  };
}

/**
 * The error when the creation could not be finished now but its preparation is on chain. It is a
 * SignFlowError `genesis-pending`. It is never signed again on the confirmation screen, and the
 * Create token page finishes it. Its message says why: declined, the wallet could not see the
 * first step, or the node.
 */
function genesisPending(prepTxid: string, e: unknown): SignFlowError {
  const first = `The token details were recorded on chain (transaction ${prepTxid.slice(0, 16)}...), but the token itself was not created`;
  const after = 'The KAS of that first step stays in your wallet. Open Create token to finish the creation, or keep the KAS.';
  if (e instanceof SignFlowError && e.stage === 'signing') {
    if (e.rejectedByUser) {
      return new SignFlowError('signing', e, `${first}: the second approval was declined, so nothing more was sent. ${after} If your wallet said an input "is not an unspent output on this network" (KasWare then offers only Reject), wait a few seconds and finish it.`, 'genesis-pending');
    }
    return new SignFlowError('signing', e, `${first}: ${e.message} ${after}`, 'genesis-pending');
  }
  if (e instanceof SignFlowError && e.stage !== 'submitting') return new SignFlowError(e.stage, e, `${first}: ${e.message} ${after}`, 'genesis-pending');
  const err = e instanceof SignFlowError ? e : e instanceof NodeError ? e : describeNodeError(e);
  return new SignFlowError('submitting', err, `${first}: the node did not take the token's creation: ${err.message} ${after}`, 'genesis-pending');
}

/**
 * Signs and sends both transactions of a token creation, keeping a creation record in between.
 * Once the preparation is sent, every failure is a SignFlowError `genesis-pending`.
 */
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
    ...(a.record.curve ? { curve: a.record.curve } : {}), ...(a.record.askSale ? { askSale: a.record.askSale } : {}),
  };
  let saved = false;
  let prepSent = false;
  let prepSentAt: number | undefined;
  try {
    const res = await runSignChain({
      kob: a.kob, node: a.node, adapter: a.adapter, network: a.network, steps: [prep, built], addressOf: a.addressOf,
      signChainOptions: { genesisClaim: genesisClaimOf(a.record.token, a.record.curve) },
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
      // The record comes first, because from here on the page may die between the two.
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
    // Nothing went out. After a definite refusal of the preparation, or an answer that cannot be
    // trusted, the record is of no use. An unclear failure, such as the connection, keeps it,
    // because the preparation may be in.
    if (e instanceof SignFlowError && e.stage === 'submitting') {
      const cause = e.cause;
      if (e.code === 'txid-mismatch' || (cause instanceof NodeError && DEFINITE.has(cause.code))) removeCreation(a.network, base.covenantId, storage);
    } else if (saved) {
      // Signed and recorded but failed before the send, which cannot happen after a successful
      // record. Nothing went out.
      removeCreation(a.network, base.covenantId, storage);
    }
    if (e instanceof ChainError) throw new SignFlowError(e.stage, e.cause, e.message, e.code);
    throw e;
  }
}

/** How `resumeCreation` sends a saved creation. */
export interface FinishOptions {
  storage?: StorageLike | null;
  /**
   * The genesis offers while the node does not see the preparation. The default is 3 times, 1500
   * ms apart.
   */
  attempts?: number;
  retryMs?: number;
  sleep?: (ms: number) => Promise<void>;
}

/**
 * Finishes a saved creation whose genesis is signed. Nothing is signed.
 *
 * It sends its preparation again, and a node that has it, or has it in a block, refuses it, which
 * is fine. Then it sends its signed genesis as it is. The record is removed once the genesis is
 * accepted. Otherwise it throws a SignFlowError `genesis-pending`.
 */
export async function resumeCreation(node: NodeApi, rec: CreationRecord, opts: FinishOptions = {}): Promise<string> {
  const sleep = opts.sleep ?? ((ms: number) => new Promise<void>((r) => setTimeout(r, ms)));
  if (!genesisSigned(rec)) throw new SignFlowError('submitting', null, 'The saved genesis is not signed yet: your wallet must sign it.', 'genesis-unsigned');
  if (rec.genesis.inputs[0]?.transactionId !== rec.prepTxid) throw new SignFlowError('submitting', null, 'The saved genesis does not spend the saved preparation.', 'other');
  try {
    await sendTx(node, rec.prep);
  } catch {
    /* Already in a block, or spent. The genesis decides. */
  }
  try {
    await sendTxRetrying(node, rec.genesis, opts.attempts ?? 3, opts.retryMs ?? 1500, sleep);
  } catch (e) {
    throw genesisPending(rec.prepTxid, e);
  }
  removeCreation(rec.network, rec.covenantId, opts.storage);
  return rec.genesisTxid;
}

/** The inputs of `finishCreation`. */
export interface FinishArgs extends FinishOptions {
  kob: KobWasm;
  node: NodeApi;
  /** The wallet of the record's key. It signs the genesis when the record holds none signed. */
  adapter: WalletAdapter;
  /** That wallet's x-only key. A record of another key is refused. */
  maker: string;
  /** The wallet's fee policy, for the fee checks of a fresh creation (ConfirmSign). */
  feePolicy?: IssuanceInput['feePolicy'];
  network: string;
  rec: CreationRecord;
  addressOf: (spk: string) => string;
  onStage?: (stage: SignStage, info?: CreationStageInfo) => void;
  timeoutMs?: number;
  checkWallet?: boolean;
  validate?: boolean;
  tighten?: boolean;
  /**
   * How long the preparation, sent again, may take to be listed by the node. The default is 60 s,
   * polled every 1 s.
   */
  prepWaitMs?: number;
  prepPollMs?: number;
  visibleTimeoutMs?: number;
  visiblePollMs?: number;
  settleMs?: number;
  unknownRetryMs?: number;
  now?: () => number;
}

/**
 * The code when the coin the genesis spends is gone, spent by another transaction. The creation
 * cannot be finished.
 */
export const CREATION_SPENT = 'creation-spent';
/**
 * The code when the saved record does not plan into a valid creation of this wallet. Nothing is
 * signed or sent.
 */
export const CREATION_INVALID = 'creation-invalid';

/**
 * Plans a saved creation again and checks it as a fresh one. It returns the plan to sign, or
 * throws a SignFlowError `creation-invalid` with nothing signed.
 *
 * The stored genesis is never used. The plan must reproduce the record's ids and token, and pass
 * every blocking check of the issuance screen.
 */
export function checkSavedCreation(kob: KobWasm, rec: CreationRecord, maker: string, feePolicy?: IssuanceInput['feePolicy']): IssueResult {
  const invalid = (why: string) => new SignFlowError('signing', null, `This saved creation is not valid (${why}). Nothing was signed or sent. Forget it.`, CREATION_INVALID);
  const r = replanCreation(kob, rec, maker);
  if (!r.ok) throw invalid(r.reason);
  const { prep, built, token, curve } = r.plan;
  const model = buildIssuanceModel({ kob, built, prep, token, ...(curve ? { curve } : {}), maker: maker.toLowerCase(), resumed: true, ...(feePolicy ? { feePolicy } : {}) });
  const bad = model.blocking[0];
  if (bad) throw invalid(bad.text ?? bad.code);
  return r.plan;
}


/**
 * Whether the record's genesis is on chain, as the node lists the addresses of its outputs: one of
 * its outputs, or a token output of its covenant, is unspent there. A read that fails says no.
 */
async function genesisOnChain(node: NodeApi, rec: CreationRecord, built: BuiltTx, addressOf: (spk: string) => string): Promise<boolean> {
  try {
    const addresses = [...new Set(built.tx.outputs.map((o) => addressOf(o.scriptPublicKey)))];
    const id = rec.covenantId.toLowerCase();
    return (await node.getUtxosByAddresses(addresses)).some((u) => u.transactionId === rec.genesisTxid || u.covenantId?.toLowerCase() === id);
  } catch {
    return false;
  }
}
/**
 * Finishes a saved creation, for "Finish the creation" on the Create token page. It returns the
 * genesis txid.
 *
 * The record is planned again and checked first (`checkSavedCreation`). A record that does not
 * check out is a SignFlowError `creation-invalid`.
 *
 * A signed genesis is sent as it is (`resumeCreation`). An unsigned one needs the wallet once. When
 * the preparation's output 0 is in the node's UTXO set, it is signed and sent at once. Otherwise
 * the saved preparation is sent again and its output awaited first. A preparation the node refuses
 * for good while its output is not listed means the coin was spent elsewhere. That is a
 * SignFlowError `creation-spent`, and only forgetting the record is left, unless the genesis itself
 * spent it and is on chain: then the creation is done and its txid is returned.
 */
export async function finishCreation(a: FinishArgs): Promise<string> {
  const { node, rec } = a;
  const sleep = a.sleep ?? ((ms: number) => new Promise<void>((r) => setTimeout(r, ms)));
  const now = a.now ?? Date.now;
  // Planned again from the record's facts. Its stored genesis is never signed.
  const plan = checkSavedCreation(a.kob, rec, a.maker, a.feePolicy);
  if (genesisSigned(rec)) return resumeCreation(node, rec, a);
  const built: BuiltTx = plan.built;
  // The coin the genesis spends, as the node lists it, with the wallet's own key and the planned
  // amount.
  const out0 = plan.prep.tx.outputs[0]!;
  const address = a.addressOf(out0.scriptPublicKey);
  const listed = async (): Promise<boolean> =>
    (await node.getUtxosByAddresses([address])).some((u) => u.transactionId === rec.prepTxid && u.index === 0 && BigInt(u.amount) === BigInt(out0.value));

  // The preparation's output 0 must be on the node before the wallet is asked. This wait is a
  // wallet need, not a safety check: KasWare refuses a child of a parent its node never saw.
  let sentAt = rec.prepSentAt ?? rec.createdAt;
  if (!(await listed().catch(() => false))) {
    try {
      await sendTx(node, rec.prep);
    } catch (e) {
      const err = e instanceof NodeError ? e : e instanceof SignFlowError ? e : describeNodeError(e);
      if (err instanceof NodeError && DEFINITE.has(err.code) && !(await listed().catch(() => false))) {
        // The coin may be spent by this very genesis, sent before the page closed: then the token
        // exists and the creation is done. Starting over would make a second token.
        if (await genesisOnChain(node, rec, built, a.addressOf)) return rec.genesisTxid;
        throw new SignFlowError('submitting', err, 'The coin of the first step was spent, so this creation cannot be finished. Start a new one.', CREATION_SPENT);
      }
      // The connection failed, or the node refused while the output is listed after all because
      // it reached a block meanwhile. Unless it is listed now, try again later.
      if (!(await listed().catch(() => false))) throw genesisPending(rec.prepTxid, err);
    }
    sentAt = now();
    markPrepSent(rec.network, rec.covenantId, a.storage, sentAt);
    // It is handed to the wallet's node too, where the wallet can take it (visibility.ts).
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
    // Declined, or not visible yet. The record stays as it was, to finish again later, and the
    // error says what happened.
    if (e instanceof ChainError) throw new SignFlowError(e.stage, e.cause, e.message, e.code);
    throw e;
  });
  removeCreation(rec.network, rec.covenantId, a.storage);
  return res.signed[0]!.tx.id;
}
