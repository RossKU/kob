import { useEffect, useRef, useState } from 'preact/hooks';
import { t, tIssue } from '../../i18n';
import type { CancelPlan } from '../../kob/cancel';
import { Banner, IssueLine, Section } from '../kit';
import { ConfirmSign, type ConfirmResult } from '../confirm/ConfirmSign';
import { withoutRaw } from '../../i18n/build-error';

export interface FlowStep {
  /** heading passed to ConfirmSign */
  title: string;
  plan: CancelPlan;
  /** placement-record label for orders the step creates (amendments) */
  label?: string;
  /** covenant ids this step spends: their records are dropped after the step is submitted */
  spends: string[];
}

export type StepState = 'pending' | 'confirming' | 'submitted' | 'cancelled' | 'failed';

export interface FlowOutcome {
  submitted: number;
  cancelled: number;
  failed: number;
}

export interface TxFlowProps {
  /** shown above the steps: what is about to happen */
  heading: string;
  steps: FlowStep[];
  /** called after each submitted step (drop records, refresh) */
  onStepSubmitted(step: FlowStep, result: Extract<ConfirmResult, { status: 'submitted' }>): void | Promise<void>;
  /** called once when the flow is over (all steps done, or the user stopped) */
  onDone(outcome: FlowOutcome): void;
}

/** Plan findings the user must explicitly accept before signing (the confirm screen shows them with their own checkbox). */
export const ACK_ISSUE_CODES: ReadonlySet<string> = new Set(['cancel.strays-abandoned', 'cancel.stray-other-extension', 'cancel.stray-other-token', 'cancel.strays-unknown', 'cancel.repeat-entry-only']);
export const ackTexts = (plan: CancelPlan): string[] => plan.issues.filter((i) => ACK_ISSUE_CODES.has(i.code)).map((i) => tIssue('orders.issue', withoutRaw(i)));

/** The plan's own findings (info / warning), translated; errors never get here (a plan with errors has no transaction). */
function PlanNotes({ plan }: { plan: CancelPlan }) {
  const notes = plan.issues.filter((i) => i.severity !== 'error');
  if (!notes.length) return null;
  return (
    <ul class="small" data-testid="flow-notes">
      {notes.map((i, n) => (
        <li key={n}><IssueLine prefix="orders.issue" issue={i} /></li>
      ))}
    </ul>
  );
}

/**
 * Runs one or several transactions one after the other, each through <ConfirmSign> (the pre-sign screen: the ONLY place anything is signed).
 * A progress list shows every step; stopping (cancel / failure) ends the sequence, and steps not yet run are simply not run: they spend disjoint
 * UTXOs, so a partial sequence leaves everything consistent.
 */
export function TxFlow(props: TxFlowProps) {
  const [states, setStates] = useState<StepState[]>(() => props.steps.map((_, i) => (i === 0 ? 'confirming' : 'pending')));
  const [index, setIndex] = useState(0);
  const [message, setMessage] = useState<string | null>(null);
  const [txids, setTxids] = useState<Record<number, string>>({});
  const done = useRef(false);

  const finish = (s: StepState[]) => {
    if (done.current) return;
    done.current = true;
    props.onDone({ submitted: s.filter((x) => x === 'submitted').length, cancelled: s.filter((x) => x === 'cancelled').length, failed: s.filter((x) => x === 'failed').length });
  };

  const onClose = async (step: FlowStep, result: ConfirmResult) => {
    const next = [...states];
    if (result.status === 'submitted') {
      next[index] = 'submitted';
      setTxids((m) => ({ ...m, [index]: result.txid }));
      await props.onStepSubmitted(step, result);
      if (index + 1 < props.steps.length) {
        next[index + 1] = 'confirming';
        setStates(next);
        setIndex(index + 1);
        return;
      }
      setStates(next);
      setIndex(props.steps.length);
      finish(next);
      return;
    }
    next[index] = result.status === 'cancelled' ? 'cancelled' : 'failed';
    if (result.status === 'failed') setMessage(result.message);
    setStates(next);
    setIndex(props.steps.length);
    finish(next);
  };

  // leaving the page mid-flow must still report (records / refresh are the caller's)
  useEffect(() => () => void (done.current = true), []);

  const running = index < props.steps.length;
  const step = props.steps[index];
  const anySubmitted = states.some((s) => s === 'submitted');

  return (
    <Section title={props.heading} data-testid="tx-flow">
      {props.steps.length > 1 ? (
        <ol class="progress" data-testid="flow-progress">
          {props.steps.map((s, i) => (
            <li key={i} data-testid={`flow-step-${i}`} data-state={states[i]}>
              <span class="pill-step" aria-hidden="true">
                {states[i] === 'submitted' ? '✓' : states[i] === 'failed' ? '✗' : states[i] === 'cancelled' ? '–' : i + 1}
              </span>
              <span>
                {s.title} - {t(`orders.flow.state.${states[i]}`)}
                {txids[i] ? <code class="mono small"> {txids[i].slice(0, 10)}{'…'}</code> : null}
              </span>
            </li>
          ))}
        </ol>
      ) : null}
      {running ? <PlanNotes plan={step.plan} /> : null}
      {running && step.plan.built ? (
        <>
          <p class="small muted">{t('orders.flow.race')}</p>
          <ConfirmSign
            key={index}
            built={step.plan.built}
            title={step.title}
            expected={step.plan.expected}
            label={step.label}
            acknowledge={ackTexts(step.plan)}
            onClose={(r) => void onClose(step, r)}
          />
        </>
      ) : null}
      {!running ? (
        <div class="stack-sm" data-testid="flow-result">
          {message ? <Banner tone="error" data-testid="flow-error">{message}</Banner> : null}
          {anySubmitted ? <Banner tone="ok" data-testid="flow-done">{t('orders.flow.done', { count: states.filter((s) => s === 'submitted').length })}</Banner> : <Banner tone="info">{t('orders.flow.stopped')}</Banner>}
          {states.some((s) => s === 'pending') ? <Banner tone="warn">{t('orders.flow.remaining', { count: states.filter((s) => s === 'pending').length })}</Banner> : null}
        </div>
      ) : null}
    </Section>
  );
}

/** Plan-level errors (no transaction could be built) as one banner with translated findings. */
export function PlanErrors(props: { plans: CancelPlan[]; failed?: { id: string; message: string }[] }) {
  const errors = props.plans.flatMap((p) => p.issues.filter((i) => i.severity === 'error'));
  if (!errors.length && !(props.failed && props.failed.length)) return null;
  return (
    <Banner tone="error" title={t('orders.flow.cannotBuild')} data-testid="flow-plan-errors">
      <ul>
        {errors.map((i, n) => (
          <li key={n}><IssueLine prefix="orders.issue" issue={i} /></li>
        ))}
        {(props.failed ?? []).map((f) => (
          <li key={f.id}>
            <code>{f.id.slice(0, 8)}{'…'}</code>: {f.message}
          </li>
        ))}
      </ul>
    </Banner>
  );
}
