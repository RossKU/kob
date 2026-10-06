// Pure helpers of the signing flow: the stage stepper, how a failure is classified (neutral / retry / re-plan) and explorer links.
import { t } from '../../i18n';
import { explorerTxUrl as configTxUrl, parseNetwork } from '../../config';
import { signFlowText } from '../../i18n/issue-text';
import { SignFlowError, type SignStage } from '../../wallet/sign';

/** Stages the stepper shows, in order (`submitted` is the end state, not a step). */
export const STAGES = ['signing', 'finalizing', 'validating', 'submitting'] as const;
export type FlowStage = (typeof STAGES)[number];

export type StepState = 'done' | 'active' | 'pending';

/** State of every step given the current stage (`null` = not started, `submitted` = all done). */
export function stepStates(current: SignStage | null): { stage: FlowStage; state: StepState }[] {
  const at = current === null ? -1 : current === 'submitted' ? STAGES.length : STAGES.indexOf(current);
  return STAGES.map((stage, i) => ({ stage, state: i < at ? 'done' : i === at ? 'active' : 'pending' }));
}

export type FailureKind =
  /** the user declined in the wallet: neutral, nothing happened */
  | 'rejected'
  /** nothing was broadcast and the same transaction may be tried again */
  | 'retry'
  /** the inputs changed or the transaction is invalid: it must be built again from fresh data */
  | 'replan'
  /** the node already has exactly this transaction: treat it as submitted */
  | 'known';

export interface Failure {
  kind: FailureKind;
  code: string;
  stage: string;
  text: string;
  /** the raw technical text (wallet / node / library), shown only behind "Details" */
  raw?: string;
}

const REPLAN_CODES = new Set(['orphan', 'double-spend', 'fee', 'script', 'invalid', 'validation', 'network-mismatch', 'account-changed', 'inputs-changed', 'inputs-unconfirmed']);

/** Classifies whatever `signAndSubmit` threw. Never throws. */
export function classifyFailure(e: unknown): Failure {
  if (e instanceof SignFlowError) {
    const text = signFlowText(e);
    if (e.rejectedByUser) return { kind: 'rejected', code: e.code, stage: e.stage, text };
    if (e.stage === 'submitting' && e.code === 'already-known') return { kind: 'known', code: e.code, stage: e.stage, text };
    if (REPLAN_CODES.has(e.code)) return { kind: 'replan', code: e.code, stage: e.stage, text };
    return { kind: 'retry', code: e.code, stage: e.stage, text };
  }
  const raw = e instanceof Error ? e.message : String(e);
  return { kind: 'retry', code: 'other', stage: 'unknown', text: t('confirm.failedOther', undefined), raw };
}

/** Block-explorer page of a transaction (kaspa.stream, `config.explorerUrl` overrides the base), or null for an unknown network or a malformed id. */
export function explorerTxUrl(network: string, txid: string, explorerUrl = ''): string | null {
  const n = parseNetwork(network);
  return n ? configTxUrl({ network: n, explorerUrl }, txid) : null;
}
