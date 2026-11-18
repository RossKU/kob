// The stepper of a flow card (TxFlow.tsx). It shows the steps' states as stepper items, with the
// private-key signer's one approval in front, as in a merge session, and a step that follows the
// run outside it behind, such as "Merge, then sell" with the sale.
import { t } from '../../i18n';
import type { StepperItem } from '../kit/stepper-model';
import type { SendPhase } from '../confirm/SendStatus';
import type { StepState } from './TxFlow';

/**
 * The run's one approval in the private-key signer: none, being asked, given, refused (an error) or
 * declined.
 */
export type SessionStep = 'none' | 'asking' | 'open' | 'failed' | 'declined';

export interface FlowStepperInput {
  titles: readonly string[];
  states: readonly StepState[];
  /** The running step's send (ui/confirm/SendStatus.tsx), which says what it waits for. */
  phase?: SendPhase | null;
  /** The send waits until the wallet sees the previous transaction. */
  waiting?: boolean;
  session?: SessionStep;
  /** After the run. It is current once every step went through, as "Sell" or "Confirm the sale". */
  then?: { label: string; note: string };
}

function sendingNote(phase: SendPhase | null | undefined, waiting: boolean, session: boolean): string {
  if (waiting) return t('stepper.note.waitWallet');
  if (phase === 'checking') return t('stepper.note.checking');
  if (phase === 'confirming') return t('stepper.note.confirm');
  if (phase === 'sending') return t('stepper.note.sending');
  return t(session ? 'stepper.note.signing' : 'stepper.note.sign');
}

export function flowStepperItems(a: FlowStepperInput): StepperItem[] {
  const session = a.session ?? 'none';
  const sessionFailed = session === 'failed' || session === 'declined';
  const items: StepperItem[] = [];
  if (session !== 'none') {
    items.push({
      label: t('stepper.step.approve'),
      testid: 'flow-session-step',
      data: session,
      ...(session === 'asking'
        ? { state: 'current' as const, busy: true, note: t('stepper.note.approveOnce') }
        : session === 'open'
          ? { state: 'done' as const }
          : { state: 'failed' as const, note: t(session === 'declined' ? 'stepper.note.declined' : 'stepper.note.failed') }),
    });
  }
  a.titles.forEach((label, i) => {
    const s = a.states[i] ?? 'pending';
    const base = { label, testid: `flow-step-${i}`, data: s };
    // The approval of the whole run comes first. No step runs before it, and one it stopped never
    // started.
    if (session === 'asking' || sessionFailed) return items.push({ ...base, state: 'waiting' });
    if (s === 'pending') return items.push({ ...base, state: 'waiting' });
    if (s === 'preparing') return items.push({ ...base, state: 'current', busy: true, note: t('stepper.note.preparing') });
    if (s === 'sending') return items.push({ ...base, state: 'current', busy: true, note: sendingNote(a.phase, !!a.waiting, session === 'open') });
    if (s === 'submitted') return items.push({ ...base, state: 'done' });
    return items.push({ ...base, state: 'failed', note: t(s === 'cancelled' ? 'stepper.note.declined' : 'stepper.note.failed') });
  });
  if (a.then) {
    const through = a.states.length > 0 && a.states.every((s) => s === 'submitted') && !sessionFailed;
    items.push({ label: a.then.label, testid: 'flow-then-step', ...(through ? { state: 'current', note: a.then.note } : { state: 'waiting' }) });
  }
  return items;
}
