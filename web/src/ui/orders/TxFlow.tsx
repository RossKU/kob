import { useEffect, useRef, useState } from 'preact/hooks';
import { t } from '../../i18n';
import type { CancelPlan } from '../../kob/cancel';
import type { PlacementRecord } from '../../kob/records';
import { Banner, IssueLine, Section, Spinner } from '../kit';
import { SendStatusLine, useSend } from '../confirm/SendStatus';

export interface FlowStep {
  /** the step in the progress list */
  title: string;
  plan: CancelPlan;
  /** placement-record label for orders the step creates (amendments) */
  label?: string;
  /** covenant ids this step spends: their records are dropped after the step is submitted */
  spends: string[];
  /** one line under the heading while the step runs (a merge: the whole chain) */
  shownAs?: string;
  /** the toast once the step is submitted (default: the cancel / sweep wording) */
  toast?: string;
}

export type StepState = 'pending' | 'preparing' | 'sending' | 'submitted' | 'cancelled' | 'failed';

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
  onStepSubmitted(step: FlowStep, result: { txid: string; records: PlacementRecord[] }): void | Promise<void>;
  /** called once when the flow is over (all steps done, or one stopped) */
  onDone(outcome: FlowOutcome): void;
  /**
   * A CHAINED flow (a token merge): step `index` spends an output of step `index - 1`, so it is built again from the node once that step was
   * submitted (`prev`) and replaces the planned step. A rejection ends the flow with its message.
   */
  next?(index: number, prev: { txid: string }): Promise<FlowStep>;
}

/**
 * Runs one or several transactions one after the other, each checked silently and approved in the wallet (ui/confirm/send.ts) as soon as the flow
 * starts: the action's button was the user's request, the wallet's popup is the approval. A progress list shows every step and one line the
 * current one; a step that stops (rejected in the wallet, a failed check) ends the sequence, and steps not yet run are simply not run: they spend
 * disjoint UTXOs, so a partial sequence leaves everything consistent.
 */
export function TxFlow(props: TxFlowProps) {
  const sender = useSend();
  const [steps, setSteps] = useState<FlowStep[]>(props.steps);
  const [states, setStates] = useState<StepState[]>(() => props.steps.map((_, i) => (i === 0 ? 'sending' : 'pending')));
  const [index, setIndex] = useState(0);
  const [message, setMessage] = useState<string | null>(null);
  const [txids, setTxids] = useState<Record<number, string>>({});
  const done = useRef(false);
  const gone = useRef(false);

  const finish = (s: StepState[]) => {
    if (done.current) return;
    done.current = true;
    props.onDone({ submitted: s.filter((x) => x === 'submitted').length, cancelled: s.filter((x) => x === 'cancelled').length, failed: s.filter((x) => x === 'failed').length });
  };

  const run = async () => {
    const list = [...props.steps];
    const st: StepState[] = list.map((_, i) => (i === 0 ? 'sending' : 'pending'));
    const show = () => setStates([...st]);
    const end = () => {
      show();
      setIndex(list.length);
      finish(st);
    };
    let prev: string | null = null;
    for (let i = 0; i < list.length; i++) {
      if (gone.current) return;
      if (i > 0 && props.next && prev) {
        st[i] = 'preparing';
        show();
        try {
          list[i] = await props.next(i, { txid: prev });
          setSteps([...list]);
        } catch (e) {
          st[i] = 'failed';
          setMessage(e instanceof Error ? e.message : String(e));
          return end();
        }
      }
      const step = list[i]!;
      st[i] = 'sending';
      setIndex(i);
      show();
      if (!step.plan.built) {
        st[i] = 'failed';
        return end();
      }
      const out = await sender.send({ built: step.plan.built, expected: step.plan.expected, label: step.label });
      if (out.status !== 'submitted') {
        st[i] = out.send.phase === 'rejected' ? 'cancelled' : 'failed';
        return end();
      }
      st[i] = 'submitted';
      show();
      setTxids((m) => ({ ...m, [i]: out.txid }));
      await props.onStepSubmitted(step, out);
      prev = out.txid;
    }
    end();
  };

  useEffect(() => {
    void run();
    // leaving the page mid-flow: no further step starts
    return () => void (gone.current = true);
  }, []);

  const running = index < steps.length;
  const step = steps[index];
  const preparing = states.some((s) => s === 'preparing');
  const anySubmitted = states.some((s) => s === 'submitted');

  return (
    <Section title={props.heading} data-testid="tx-flow">
      {steps.length > 1 ? (
        <ol class="progress" data-testid="flow-progress">
          {steps.map((s, i) => (
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
      {running && step?.shownAs ? <p class="small" data-testid="flow-shown-as">{step.shownAs}</p> : null}
      {preparing ? (
        <p class="small muted" data-testid="flow-preparing">
          <Spinner /> {t('orders.flow.preparing')}
        </p>
      ) : null}
      <SendStatusLine status={sender.status} data-testid="flow-status" />
      {!running ? (
        <div class="stack-sm" data-testid="flow-result">
          {message ? <Banner tone="error" data-testid="flow-error">{message}</Banner> : null}
          {anySubmitted ? <Banner tone="ok" data-testid="flow-done">{t('orders.flow.done', { count: states.filter((s) => s === 'submitted').length })}</Banner> : <Banner tone="info" data-testid="flow-stopped">{t('orders.flow.stopped')}</Banner>}
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
