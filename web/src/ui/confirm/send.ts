// Sending a transaction the user asked for (an order of the ticket, a cancel / amend / refund of My orders): the machine checks, the wallet's
// own approval, the node.
//
// There is no confirmation screen in between: the ticket or the order row is where the user sees what they do, and the wallet's popup is the
// approval. Every check of the pre-sign decoder still runs, silently, before the wallet is asked: the wallet's account and network, every input
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
import { decodeSigning, type ExpectedSigning } from '../../kob/decode';
import { confirmInputsOnNode } from '../../kob/node-verify';
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
   * a check of the caller that runs alongside the node read (the ticket's re-check of the order against the state at the click): resolves to why
   * the order no longer stands, or null
   */
  gate?: Promise<string | null> | undefined;
  /** the last DAA score at which the signed transaction may still be sent (an auction planned at the click: wallet/sign.ts `auctionSubmitBy`) */
  submitByDaa?: bigint | null | undefined;
}

/** Why a transaction is not sent: a finding code and its sentence. */
export interface Blocked {
  code: string;
  text: string;
}

type WalletFacts = Pick<WalletSession, 'info' | 'adapter' | 'networkMismatch'>;

/** script public key -> address of the app's network (the node is asked by address) */
export const inputAddressOf = (services: Pick<Services, 'sdk' | 'config'>) => (spk: string): string => spkStringToAddress(services.sdk, spk, services.config.network);

/**
 * The checks of a transaction before the wallet is asked to sign it. Null: it may be signed; else the first finding that stops it. Never throws
 * for a finding; a failing node read is a finding (`inputs-unavailable`).
 */
export async function checkBeforeSigning(services: Services, wallet: WalletFacts, job: SendJob): Promise<Blocked | null> {
  const { built } = job;
  if (!wallet.info || !wallet.adapter) return { code: 'no-wallet', text: t('send.noWallet') };
  // the plan spends coins of the key it was built for: after an account switch it must not be signed (the plan is stale)
  const planned = built.sign.find((s) => s.pubkey !== wallet.info!.pubkey);
  if (planned) return { code: 'account-changed', text: t('send.accountChanged', { planned: shortId(planned.pubkey), now: shortId(wallet.info.pubkey) }) };
  if (wallet.networkMismatch) return { code: 'network', text: t('send.networkMismatch', { wallet: wallet.info.network, app: services.config.network }) };

  // the KAS value of every input re-read from the NODE (the decoder flags any input the node does not confirm), the caller's gate alongside
  const [facts, gate] = await Promise.all([
    confirmInputsOnNode(services.node, built.tx.inputs, inputAddressOf(services)).then((r) => r.facts, () => null),
    job.gate ?? Promise.resolve(null),
  ]);
  if (gate !== null) return { code: 'changed', text: gate };
  if (facts === null) return { code: 'inputs-unavailable', text: t('send.inputsUnavailable') };

  const registry = job.openToken ? withExtraToken(services.registry, job.openToken) : services.registry;
  // a dynamic fee is several times the relay floor in a busy market: the decoder's sanity ceiling follows the policy's own per-transaction cap
  const feePolicy = services.fees?.policy;
  const summary = decodeSigning({ kob: services.kob, built, maker: wallet.info.pubkey, registry, expected: job.expected, nodeInputs: facts, ...(feePolicy ? { feePolicy } : {}) });
  const found: Finding | undefined = [...summary.blocking, ...openTokenIdentity(summary, registry).blocking][0];
  if (found) return { code: found.code, text: found.text ?? issueText(found) };
  if (!summary.ok) return { code: 'decode', text: t('send.decodeFailed') };
  return null;
}

/**
 * After the node accepted `txid`: its inputs stay out of every later plan of this page until it is in a block, and the placement records are
 * saved (best effort: storage problems never turn an accepted transaction into an error).
 */
export async function recordAccepted(services: Services, wallet: Pick<WalletSession, 'info' | 'records'>, built: BuiltTx, txid: string, label?: string): Promise<{ records: PlacementRecord[]; clock: Clock }> {
  pendingSpends.add(built.tx.inputs);
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
