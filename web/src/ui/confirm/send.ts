// Sending a transaction the user asked for (an order of the ticket, or a cancel, amend or refund
// of My orders). The steps are the machine checks, the final confirmation (SendStatus.tsx, the
// signer table of signer/summary.ts), the wallet's own approval, and the node.
//
// Every check of the pre-sign decoder runs silently before the user is asked:
// - the wallet's account and network;
// - every input re-read on the node, except the wallet's own plain KAS coins, whose signature
//   commits their values (kob/node-verify.ts `ownPlainInputs`);
// - the decoder's reading of the built transaction compared with what the planner claims
//   (`expected`), so a planner, builder or wasm bug can never pass;
// - the identity of an open-list token;
// - the caller's own re-check (`gate`).
// Only a finding that makes the transaction wrong stops it, with one sentence. The decoder's
// warnings and notes are not shown and never stop anything.
import type { Services } from '../../app/services';
import type { WalletSession } from '../../app/context';
import { commitAccepted } from '../../app/commit';
import { readClock } from '../../app/env';
import { pendingSpends } from '../../app/pending-spends';
import { spkStringToAddress } from '../../data/kaspa-sdk';
import { t } from '../../i18n';
import { issueText } from '../../i18n/issue-text';
import { decodeSigning, type ExpectedSigning, type SigningSummary } from '../../kob/decode';
import { confirmInputsOnNode, outpointKey, ownPlainInputs } from '../../kob/node-verify';
import type { Clock } from '../../kob/plan-types';
import type { PlacementRecord } from '../../kob/records';
import { withExtraToken, type TokenInfo, type TokenRegistry } from '../../kob/registry';
import type { BuiltTx } from '../../kob/types';
import type { SignerNote } from '../../signer/protocol';
import { shortId } from '../kit/format';
import { openTokenIdentity, type Finding } from './confirm-model';

export interface SendJob {
  /** The unsigned transaction. */
  built: BuiltTx;
  /** What the planner claims. A decoded transaction that disagrees is never signed. */
  expected?: ExpectedSigning | undefined;
  /** Plan label stored in the placement records. */
  label?: string | undefined;
  /**
   * The order's open-list tokens (no registry entry, and on a pair either or both). The
   * transaction is decoded against the registry plus these.
   */
  openTokens?: readonly TokenInfo[] | undefined;
  /**
   * A check of the caller that runs alongside the node read (the ticket's re-check of the order
   * against the state at the click). Null keeps the transaction. `replace` puts another plan of the
   * same order in its place: the state at the click planned it differently, within the user's
   * terms, so it is checked and signed instead. `stop` says why the order no longer stands.
   */
  gate?: Promise<GateVerdict> | undefined;
  /**
   * The last DAA score at which the signed transaction may still be sent (an auction planned at the
   * click, wallet/sign.ts `auctionSubmitBy`).
   */
  submitByDaa?: bigint | null | undefined;
  /**
   * Plans the same order again with the current clock, on the same environment. It is for the
   * ticket's order, an auction whose start must not lie behind a long reading of the confirmation
   * or a slow signature. It returns the job to send instead, with its terms, inputs and fee the
   * same and only its times later, or why it cannot be (SendStatus.tsx). `signingMs` is how long
   * the attempt it follows took from the wallet's request to the clock check after the signature
   * (a `clock-moved`). The new start leaves at least that long again (ui/confirm/signing-time.ts
   * `retimeLead`).
   */
  retime?: ((o?: { signingMs?: number }) => Promise<SendJob | { stop: Blocked }>) | undefined;
  /**
   * Plans the same order again on the environment now: the market and the clock as the page has
   * them now, within the terms the ticket showed. The open final confirmation follows it while the
   * user reads it (SendStatus.tsx). It returns the job, or why there is none now.
   */
  replan?: (() => Promise<SendJob | { stop: Blocked }>) | undefined;
  /**
   * Non-blocking lines the final confirmation shows under the summary (an order below the minimum
   * order value, a price far through the market). They are worked out once from the plan of the
   * click, never updated while the confirmation is open, and never a reason to stop.
   */
  notes?: readonly string[] | undefined;
  /**
   * The same notes as codes and numbers for the private-key signer's confirmation, which words
   * them itself (signer/protocol.ts `SignerNote`).
   */
  signerNotes?: readonly SignerNote[] | undefined;
  /**
   * This job re-times the one the user confirmed just before. The private-key signer then signs it
   * without asking again when nothing got worse.
   */
  retimed?: boolean | undefined;
  /**
   * The user confirmed this transaction in the app already (a merge run's confirmation, which
   * states every transaction of the run). An external wallet is asked at once, without the app's
   * final confirmation. Every check above still runs and the wallet still asks for its own
   * approval.
   */
  preconfirmed?: boolean | undefined;
  /**
   * A step of the wallet's one-approval session (a merge run with the private-key signer,
   * wallet/types.ts `openSession`). The wallet signs it without a prompt while its own check finds
   * it within the plan the user allowed. Every check above still runs.
   */
  session?: string | undefined;
  /**
   * The wallet's creator locks this transaction pays a lock coin to (kob/creator-lock.ts, from
   * their verified declarations). The decoder takes a plain output to one of them, open and of the
   * wallet key, as the wallet's own. The private-key signer gets them as its hint.
   */
  locks?: readonly { until: string; key: string }[] | undefined;
}

/**
 * The registry plus the job's open-list tokens: what its transaction is decoded against and its
 * tokens' identities are checked with.
 */
export const registryOfJob = (registry: TokenRegistry, job: Pick<SendJob, 'openTokens'>): TokenRegistry => (job.openTokens ?? []).reduce(withExtraToken, registry);

/** Why a transaction is not sent: a finding code and its sentence. */
export interface Blocked {
  code: string;
  text: string;
}

/** The outcome of a `SendJob.gate`. */
export type GateVerdict = null | { stop: Blocked } | { replace: Pick<SendJob, 'built' | 'expected' | 'submitByDaa' | 'retime'> };

/**
 * The same inputs (outpoints) in the same order: the node facts read for one transaction serve the
 * other.
 */
const sameInputs = (a: BuiltTx, b: BuiltTx): boolean =>
  a.tx.inputs.length === b.tx.inputs.length && a.tx.inputs.every((i, k) => i.transactionId === b.tx.inputs[k]!.transactionId && i.index === b.tx.inputs[k]!.index);

type WalletFacts = Pick<WalletSession, 'info' | 'adapter' | 'networkMismatch'>;

/** Script public key -> address of the app's network (the node is asked by address). */
export const inputAddressOf = (services: Pick<Services, 'sdk' | 'config'>) => (spk: string): string => spkStringToAddress(services.sdk, spk, services.config.network);

/**
 * The wallet facts that stop any transaction: no wallet, another key than the plan's, another
 * network.
 */
function walletBlock(services: Services, wallet: WalletFacts, built: BuiltTx): Blocked | null {
  if (!wallet.info || !wallet.adapter) return { code: 'no-wallet', text: t('send.noWallet') };
  // The plan spends coins of the key it was built for. After an account switch it must not be
  // signed, because the plan is stale.
  const planned = built.sign.find((s) => s.pubkey !== wallet.info!.pubkey);
  if (planned) return { code: 'account-changed', text: t('send.accountChanged', { planned: planned.pubkey, now: wallet.info.pubkey }) };
  if (wallet.networkMismatch) return { code: 'network', text: t('send.networkMismatch', { wallet: wallet.info.network, app: services.config.network }) };
  return null;
}

/**
 * An input a transaction of this page already spends: accepted by the node, not yet in a block, so
 * the node still lists the coin. Examples are a second cancel of the same order and an amend right
 * after a cancel. The wallet's node sees it spent by a pending transaction and would refuse it as
 * a double spend, so it is never signed.
 */
function pendingSpent(built: BuiltTx): Blocked | null {
  const i = built.tx.inputs.find((x) => pendingSpends.has(x.transactionId, x.index));
  return i ? { code: 'input-pending-spent', text: t('send.alreadySpending') } : null;
}

/**
 * The checks of a transaction before the wallet is asked to sign it. It returns the job to sign
 * (the caller's gate may have replaced its plan), or the first finding that stops it. It never
 * throws for a finding, and a failing node read is a finding (`inputs-unavailable`). A transaction
 * that spends only the wallet's own plain KAS coins does not ask the node.
 */
export async function checkBeforeSigning(services: Services, wallet: WalletFacts, job: SendJob): Promise<{ blocked: Blocked } | { job: SendJob; decoded: SigningSummary }> {
  const early = walletBlock(services, wallet, job.built) ?? pendingSpent(job.built);
  if (early) return { blocked: early };
  // The change of a transaction this page submitted is not on the node's list until a block holds
  // it. The page knows its facts, because it built it.
  const nodeFacts = (b: BuiltTx) =>
    confirmInputsOnNode(services.node, b.tx.inputs, inputAddressOf(services), ownPlainInputs(b)).then((r) => {
      for (const i of b.tx.inputs) {
        const k = outpointKey(i.transactionId, i.index);
        const own = r.facts.has(k) ? null : pendingSpends.factOf(i.transactionId, i.index);
        if (own) r.facts.set(k, own);
      }
      return r.facts;
    }, () => null);

  // The KAS value of every covenant or token input re-read from the node (the decoder flags any
  // the node does not confirm), with the caller's gate alongside.
  const [facts0, gate] = await Promise.all([nodeFacts(job.built), job.gate ?? Promise.resolve(null)]);
  if (gate && 'stop' in gate) return { blocked: gate.stop };
  let final: SendJob = job;
  let facts = facts0;
  if (gate && 'replace' in gate) {
    // The state at the click planned the order into another transaction. That one is checked as
    // strictly, and signed.
    final = { ...job, ...gate.replace, gate: undefined };
    const again = walletBlock(services, wallet, final.built) ?? pendingSpent(final.built);
    if (again) return { blocked: again };
    if (!sameInputs(job.built, final.built)) facts = await nodeFacts(final.built);
  }
  if (facts === null) return { blocked: { code: 'inputs-unavailable', text: t('send.inputsUnavailable') } };
  const { built } = final;

  const registry = registryOfJob(services.registry, job);
  // A dynamic fee is several times the relay floor in a busy market. The decoder's sanity ceiling
  // follows the policy's own per-transaction cap.
  const feePolicy = services.fees?.policy;
  const summary = decodeSigning({ kob: services.kob, built, maker: wallet.info!.pubkey, registry, expected: final.expected, nodeInputs: facts, ...(feePolicy ? { feePolicy } : {}), ...(job.locks?.length ? { locks: job.locks } : {}) });
  const found: Finding | undefined = [...summary.blocking, ...openTokenIdentity(summary, registry).blocking][0];
  if (found) return { blocked: { code: found.code, text: found.text ?? issueText(found) } };
  if (!summary.ok) return { blocked: { code: 'decode', text: t('send.decodeFailed') } };
  return { job: final, decoded: summary };
}

/**
 * After the node accepted `txid`, its inputs stay out of every later plan of this page until it is
 * in a block, and the placement records are saved. Saving is best effort, so storage problems
 * never turn an accepted transaction into an error.
 */
export async function recordAccepted(services: Services, wallet: Pick<WalletSession, 'info' | 'records'>, built: BuiltTx, txid: string, label?: string): Promise<{ records: PlacementRecord[]; clock: Clock }> {
  pendingSpends.add(built.tx.inputs);
  // Its change is the wallet's at once. The balance does not drop until the node lists it, and the
  // next order may spend it.
  if (wallet.info) pendingSpends.addChange(built.tx, txid, wallet.info.pubkey);
  let clock: Clock | null = null;
  try {
    clock = await readClock(services);
  } catch {
    /* the clock is only a label of the placement record */
  }
  const at: Clock = clock ?? { daa: 0n, unixSeconds: BigInt(Math.floor(Date.now() / 1000)), rateMilli: 10_000 };
  const maker = wallet.info?.pubkey ?? built.sign[0]?.pubkey ?? '';
  const records = await commitAccepted({ services, records: wallet.records, built, maker, txid, clock: at, label });
  return { records, clock: at };
}
