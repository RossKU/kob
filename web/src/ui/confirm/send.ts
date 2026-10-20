// Sending a transaction the user asked for (an order of the ticket, a cancel / amend / refund of My orders): the machine checks, the final
// confirmation (SendStatus.tsx: the signer table of signer/summary.ts), the wallet's own approval, the node.
//
// Every check of the pre-sign decoder runs silently before the user is asked: the wallet's account and network, every input
// re-read on the NODE, the decoder's reading of the built transaction compared with what the planner claims (`expected`, so a planner, builder
// or wasm bug can never pass), the identity of an open-list token, and the caller's own re-check (`gate`). Only a finding that makes the
// transaction wrong stops it, with one sentence; the decoder's warnings and notes are not shown and never stop anything.
import type { Services } from '../../app/services';
import type { WalletSession } from '../../app/context';
import { commitAccepted } from '../../app/commit';
import { readClock } from '../../app/env';
import { pendingSpends } from '../../app/pending-spends';
import { spkStringToAddress } from '../../data/kaspa-sdk';
import { t } from '../../i18n';
import { issueText } from '../../i18n/issue-text';
import { decodeSigning, type ExpectedSigning, type SigningSummary } from '../../kob/decode';
import { confirmInputsOnNode, outpointKey } from '../../kob/node-verify';
import type { Clock } from '../../kob/plan-types';
import type { PlacementRecord } from '../../kob/records';
import { withExtraToken, type TokenInfo } from '../../kob/registry';
import type { BuiltTx } from '../../kob/types';
import { shortId } from '../kit/format';
import { openTokenIdentity, type Finding } from './confirm-model';

export interface SendJob {
  /** the unsigned transaction */
  built: BuiltTx;
  /** what the planner claims; a decoded transaction that disagrees is never signed */
  expected?: ExpectedSigning | undefined;
  /** plan label stored in the placement records */
  label?: string | undefined;
  /** the order trades an open-list token (no registry entry): the transaction is decoded against the registry plus that token */
  openToken?: TokenInfo | undefined;
  /**
   * a check of the caller that runs alongside the node read (the ticket's re-check of the order against the state at the click): null keeps the
   * transaction, `replace` puts another plan of the same order in its place (the state at the click planned it differently, within the user's
   * terms: it is checked and signed instead), `stop` says why the order no longer stands
   */
  gate?: Promise<GateVerdict> | undefined;
  /** the last DAA score at which the signed transaction may still be sent (an auction planned at the click: wallet/sign.ts `auctionSubmitBy`) */
  submitByDaa?: bigint | null | undefined;
  /**
   * plans the same order again with the current clock, on the same environment (the ticket's order: an auction whose start must not lie behind a
   * long reading of the confirmation or a slow signature): the job to send instead, its terms, inputs and fee the same and only its times later,
   * or why it cannot be (SendStatus.tsx). `signingMs`: how long the attempt it follows took from the wallet's request to the clock check after
   * the signature (a `clock-moved`): the new start leaves at least that long again (ui/confirm/signing-time.ts `retimeLead`)
   */
  retime?: ((o?: { signingMs?: number }) => Promise<SendJob | { stop: Blocked }>) | undefined;
  /**
   * plans the same order again on the environment now (the market and the clock as the page has them now, within the terms the ticket showed):
   * what the open final confirmation follows while the user reads it (SendStatus.tsx); the job, or why there is none now
   */
  replan?: (() => Promise<SendJob | { stop: Blocked }>) | undefined;
  /** this job re-times the one the user confirmed just before (the private-key signer then signs it without asking again when nothing got worse) */
  retimed?: boolean | undefined;
}

/** Why a transaction is not sent: a finding code and its sentence. */
export interface Blocked {
  code: string;
  text: string;
}

/** The outcome of a `SendJob.gate`. */
export type GateVerdict = null | { stop: Blocked } | { replace: Pick<SendJob, 'built' | 'expected' | 'submitByDaa' | 'retime'> };

/** The same inputs (outpoints) in the same order: the node facts read for one transaction serve the other. */
const sameInputs = (a: BuiltTx, b: BuiltTx): boolean =>
  a.tx.inputs.length === b.tx.inputs.length && a.tx.inputs.every((i, k) => i.transactionId === b.tx.inputs[k]!.transactionId && i.index === b.tx.inputs[k]!.index);

type WalletFacts = Pick<WalletSession, 'info' | 'adapter' | 'networkMismatch'>;

/** script public key -> address of the app's network (the node is asked by address) */
export const inputAddressOf = (services: Pick<Services, 'sdk' | 'config'>) => (spk: string): string => spkStringToAddress(services.sdk, spk, services.config.network);

/** The wallet facts that stop any transaction: no wallet, another key than the plan's, another network. */
function walletBlock(services: Services, wallet: WalletFacts, built: BuiltTx): Blocked | null {
  if (!wallet.info || !wallet.adapter) return { code: 'no-wallet', text: t('send.noWallet') };
  // the plan spends coins of the key it was built for: after an account switch it must not be signed (the plan is stale)
  const planned = built.sign.find((s) => s.pubkey !== wallet.info!.pubkey);
  if (planned) return { code: 'account-changed', text: t('send.accountChanged', { planned: shortId(planned.pubkey), now: shortId(wallet.info.pubkey) }) };
  if (wallet.networkMismatch) return { code: 'network', text: t('send.networkMismatch', { wallet: wallet.info.network, app: services.config.network }) };
  return null;
}

/**
 * The checks of a transaction before the wallet is asked to sign it: the job to sign (the caller's gate may have replaced its plan), or the first
 * finding that stops it. Never throws for a finding; a failing node read is a finding (`inputs-unavailable`).
 */
export async function checkBeforeSigning(services: Services, wallet: WalletFacts, job: SendJob): Promise<{ blocked: Blocked } | { job: SendJob; decoded: SigningSummary }> {
  const early = walletBlock(services, wallet, job.built);
  if (early) return { blocked: early };
  // the change of a transaction this page submitted is not on the node's list until a block holds it: the page knows its facts (it built it)
  const nodeFacts = (b: BuiltTx) =>
    confirmInputsOnNode(services.node, b.tx.inputs, inputAddressOf(services)).then((r) => {
      for (const i of b.tx.inputs) {
        const k = outpointKey(i.transactionId, i.index);
        const own = r.facts.has(k) ? null : pendingSpends.factOf(i.transactionId, i.index);
        if (own) r.facts.set(k, own);
      }
      return r.facts;
    }, () => null);

  // the KAS value of every input re-read from the NODE (the decoder flags any input the node does not confirm), the caller's gate alongside
  const [facts0, gate] = await Promise.all([nodeFacts(job.built), job.gate ?? Promise.resolve(null)]);
  if (gate && 'stop' in gate) return { blocked: gate.stop };
  let final: SendJob = job;
  let facts = facts0;
  if (gate && 'replace' in gate) {
    // the state at the click planned the order into another transaction: that one is checked as strictly, and signed
    final = { ...job, ...gate.replace, gate: undefined };
    const again = walletBlock(services, wallet, final.built);
    if (again) return { blocked: again };
    if (!sameInputs(job.built, final.built)) facts = await nodeFacts(final.built);
  }
  if (facts === null) return { blocked: { code: 'inputs-unavailable', text: t('send.inputsUnavailable') } };
  const { built } = final;

  const registry = job.openToken ? withExtraToken(services.registry, job.openToken) : services.registry;
  // a dynamic fee is several times the relay floor in a busy market: the decoder's sanity ceiling follows the policy's own per-transaction cap
  const feePolicy = services.fees?.policy;
  const summary = decodeSigning({ kob: services.kob, built, maker: wallet.info!.pubkey, registry, expected: final.expected, nodeInputs: facts, ...(feePolicy ? { feePolicy } : {}) });
  const found: Finding | undefined = [...summary.blocking, ...openTokenIdentity(summary, registry).blocking][0];
  if (found) return { blocked: { code: found.code, text: found.text ?? issueText(found) } };
  if (!summary.ok) return { blocked: { code: 'decode', text: t('send.decodeFailed') } };
  return { job: final, decoded: summary };
}

/**
 * After the node accepted `txid`: its inputs stay out of every later plan of this page until it is in a block, and the placement records are
 * saved (best effort: storage problems never turn an accepted transaction into an error).
 */
export async function recordAccepted(services: Services, wallet: Pick<WalletSession, 'info' | 'records'>, built: BuiltTx, txid: string, label?: string): Promise<{ records: PlacementRecord[]; clock: Clock }> {
  pendingSpends.add(built.tx.inputs);
  // its change is the wallet's at once (the balance does not drop until the node lists it; the next order may spend it)
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
