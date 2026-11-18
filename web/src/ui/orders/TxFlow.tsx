import { createPortal } from 'preact/compat';
import { useEffect, useRef, useState } from 'preact/hooks';
import { useWallet } from '../../app/context';
import { t, tIssue } from '../../i18n';
import type { BuiltTx } from '../../kob/types';
import { formatKas } from '../../kob/units';
import { WalletError } from '../../wallet/types';
import { claimSessionRun } from './session-run';
import type { CancelPlan } from '../../kob/cancel';
import type { PlacementRecord } from '../../kob/records';
import { Banner, Button, IssueLine, Spinner, Stepper } from '../kit';
import { toasts } from '../kit/toast-store';
import { SendStatusLine, useSend } from '../confirm/SendStatus';
import { PHONE_TOAST_QUERY } from '../shell/narrow';
import { tCount } from './plural';
import { flowStepperItems, type SessionStep } from './flow-steps';

export interface FlowStep {
  /** The step's label in the stepper. */
  title: string;
  plan: CancelPlan;
  /** Placement-record label for orders the step creates, as amendments do. */
  label?: string;
  /** Covenant ids this step spends. Their records are dropped after the step is submitted. */
  spends: string[];
  /** One line under the heading while the step runs. For a merge it is the whole chain. */
  shownAs?: string;
  /** The toast once the step is submitted. The default is the cancel / sweep wording. */
  toast?: string;
  /** The step has no toast of its own. The flow's card reports it, as for a step of a merge run. */
  silent?: boolean;
  /**
   * The user confirmed this step in the app before the flow started. A merge run's confirmation
   * states every transaction. The send skips the app's own final confirmation, and the wallet still
   * asks for its approval.
   */
  preconfirmed?: boolean;
  /**
   * The wallet's creator locks a lock coin of this step pays to (ui/confirm/send.ts
   * `SendJob.locks`).
   */
  locks?: readonly { until: string; key: string }[];
}

export type StepState = 'pending' | 'preparing' | 'sending' | 'submitted' | 'cancelled' | 'failed';

export interface FlowOutcome {
  submitted: number;
  cancelled: number;
  failed: number;
}

export interface TxFlowProps {
  /** Shown above the steps: what is about to happen. */
  heading: string;
  steps: FlowStep[];
  /** Called after each submitted step to drop records and refresh. */
  onStepSubmitted(step: FlowStep, result: { txid: string; records: PlacementRecord[] }): void | Promise<void>;
  /** Called once when the flow is over, either all steps done or one stopped. */
  onDone(outcome: FlowOutcome): void;
  /**
   * A chained flow, such as a token merge. Step `index` may spend outputs of earlier steps, so it
   * is built again at its turn and replaces the planned step. `txids` holds the transaction id of
   * every step before it. A rejection ends the flow with its message.
   */
  next?(index: number, txids: readonly string[]): Promise<FlowStep>;
  /**
   * The card was dismissed, by its x once the flow is over or by itself a moment after every step
   * went through.
   */
  onClose?(): void;
  /**
   * The whole run as planned, for a merge every link, for a wallet that approves it once. That is
   * the private-key signer (wallet/types.ts `openSession`). The wallet checks every step against it
   * itself. A wallet without sessions asks once per step.
   */
  session?: BuiltTx[];
  /**
   * A step after the run and outside it, such as the sale of "Merge, then sell". It is shown last
   * in the stepper.
   */
  then?: { label: string; note: string };
}

/**
 * How long the card of a flow whose every step went through stays. Its toast says the same and
 * stays longer.
 */
const DONE_CLOSE_MS = 6000;
/** The same on a phone, where the card is the only notification on screen (kit/Toast.tsx). */
const DONE_CLOSE_PHONE_MS = 4000;

/**
 * Runs one or several transactions one after the other, each checked silently and approved in the
 * wallet (ui/confirm/send.ts) as soon as the flow starts. The action's button was the user's
 * request, and the wallet's popup is the approval. The stepper (kit/Stepper.tsx) shows every step
 * and one line for the current one. A step that stops, by a rejection in the wallet or a failed
 * check, ends the sequence, and steps not yet run are simply not run. Each step is valid alone,
 * since a merge's later step is built only at its turn from the outputs of the steps it follows, so
 * a partial sequence leaves everything consistent.
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
  const wallet = useWallet();
  // The wallet's one-approval session of this run. It is closed when the run ends or the card goes.
  const live = useRef<{ sid: string; close(): void } | null>(null);
  const [asking, setAsking] = useState(false);
  // The stepper's first step while the run has a session. It is known before the run starts, so the
  // card does not change shape.
  const [session, setSession] = useState<SessionStep>(() => (props.session?.length && wallet.adapter?.openSession && wallet.adapter.canOpenSession?.() ? 'asking' : 'none'));
  const [sessionLine, setSessionLine] = useState<string | null>(null);
  const release = useRef<(() => void) | null>(null);
  const closeSession = () => {
    const s = live.current;
    live.current = null;
    s?.close();
    release.current?.();
    release.current = null;
  };

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
      checkSession();
      if (sessionEnded) setSessionLine(t('orders.flow.sessionEndedDone'));
      // The run is over. The wallet's session ends before anything that follows it, such as a sale,
      // is asked for.
      closeSession();
      show();
      setIndex(list.length);
      finish(st);
    };
    const sent: string[] = [];
    const adapter = wallet.adapter;
    // The wallet ended the session part way, by a step off the plan or an expiry. That step and the
    // later ones were asked one by one.
    let sessionEnded = false;
    const checkSession = () => {
      const s = live.current;
      if (!s || !adapter?.sessionEnded?.(s.sid)) return;
      live.current = null;
      sessionEnded = true;
      setSessionLine(t('orders.flow.sessionEnded'));
    };
    if (props.session?.length && adapter?.openSession && adapter.canOpenSession?.()) {
      // One run under a session at a time. A second one would end the first one's session.
      release.current = claimSessionRun();
      if (!release.current) {
        st[0] = 'failed';
        setSession('failed');
        setMessage(t('orders.flow.sessionBusy'));
        return end();
      }
      // One approval for the whole plan in the wallet. A wallet that will not hold a session for it
      // asks per step.
      setAsking(true);
      try {
        const s = await adapter.openSession(props.session);
        // A wallet that will not hold one asks per step, so there is no approval step then.
        setSession(s ? 'open' : 'none');
        if (s) {
          live.current = { sid: s.sid, close: () => adapter.closeSession?.(s.sid) };
          setSessionLine(t('orders.flow.sessionOpen', { txs: s.txs, fee: formatKas(s.feeMax) }));
        }
      } catch (e) {
        setAsking(false);
        st[0] = e instanceof WalletError && e.code === 'rejected' ? 'cancelled' : 'failed';
        setSession(st[0] === 'cancelled' ? 'declined' : 'failed');
        if (st[0] === 'failed') setMessage(e instanceof Error ? e.message : String(e));
        return end();
      }
      setAsking(false);
      if (gone.current) return closeSession();
    }
    for (let i = 0; i < list.length; i++) {
      if (gone.current) return;
      if (i > 0 && props.next) {
        st[i] = 'preparing';
        show();
        try {
          list[i] = await props.next(i, [...sent]);
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
        // The reason on the card is the plan's first error. A step with no transaction always has
        // one.
        const issue = step.plan.issues.find((x) => x.severity === 'error');
        setMessage(issue ? `${t('orders.flow.cannotBuild')}: ${tIssue('orders.issue', issue)}` : t('orders.flow.cannotBuild'));
        return end();
      }
      const sid = live.current?.sid;
      const out = await sender.send({ built: step.plan.built, expected: step.plan.expected, label: step.label, ...(step.preconfirmed ? { preconfirmed: true } : {}), ...(sid ? { session: sid } : {}), ...(step.locks?.length ? { locks: step.locks } : {}) });
      checkSession();
      if (out.status !== 'submitted') {
        st[i] = out.send.phase === 'rejected' ? 'cancelled' : 'failed';
        return end();
      }
      st[i] = 'submitted';
      show();
      setTxids((m) => ({ ...m, [i]: out.txid }));
      await props.onStepSubmitted(step, out);
      sent.push(out.txid);
    }
    end();
  };

  useEffect(() => {
    void run();
    // Leaving the page mid-flow: no further step starts, and the wallet's session ends.
    return () => {
      gone.current = true;
      closeSession();
    };
  }, []);
  // While the card is on screen no new notification shows. They go to the bell only. An error still
  // shows and colours the bell.
  useEffect(() => toasts.hold(), []);

  // A long list scrolls in the card so that the running step stays in view.
  const list = useRef<HTMLOListElement>(null);
  useEffect(() => {
    list.current?.querySelector<HTMLElement>(`[data-testid="flow-step-${index}"]`)?.scrollIntoView?.({ block: 'nearest' });
  }, [index, states]);

  const running = index < steps.length;
  const step = steps[index];
  const preparing = states.some((s) => s === 'preparing');
  const anySubmitted = states.some((s) => s === 'submitted');
  const allThrough = !running && !message && states.every((s) => s === 'submitted');
  // The notifications at the top right are where a flow reports (kit/Toast.tsx), like the order
  // ticket's sends. It is never a panel in the page.
  const [host, setHost] = useState<HTMLElement | null>(null);
  useEffect(() => setHost(document.querySelector<HTMLElement>('[data-testid="toast-host"]')), []);
  useEffect(() => {
    if (!allThrough || !props.onClose) return;
    const phone = typeof matchMedia === 'function' && matchMedia(PHONE_TOAST_QUERY).matches;
    const timer = setTimeout(() => props.onClose?.(), phone ? DONE_CLOSE_PHONE_MS : DONE_CLOSE_MS);
    return () => clearTimeout(timer);
  }, [allThrough]);
  const items = flowStepperItems({
    titles: steps.map((s) => s.title),
    states,
    phase: sender.status?.phase ?? null,
    waiting: !!sender.status?.waiting,
    session,
    ...(props.then ? { then: props.then } : {}),
  });
  // The session's step comes before the run's steps.
  const lead = session === 'none' ? 0 : 1;
  // A step that failed, or that the wallet declined, is a failure the card shows in red. The app's
  // own Cancel of a step's confirmation is the user's choice and is not red.
  const declined = states.some((s) => s === 'cancelled') && sender.status?.code !== 'cancelled';
  const tone = running ? 'info' : message || declined || states.some((s) => s === 'failed') ? 'error' : anySubmitted ? 'ok' : 'info';

  const card = (
    <div class={`toast toast-${tone} send-toast tx-flow-toast`} data-testid="tx-flow">
      <div class="grow wrap-anywhere stack-sm">
      <strong class="tx-flow-title">{props.heading}</strong>
      {items.length > 1 ? (
        <Stepper
          steps={items}
          label={t('stepper.label')}
          listRef={list}
          data-testid="flow-progress"
          extra={(k) => {
            const i = k - lead;
            return txids[i] ? <code class="mono small">{txids[i].slice(0, 10)}{'…'}</code> : null;
          }}
        />
      ) : null}
      {running && step?.shownAs ? <p class="small" data-testid="flow-shown-as">{step.shownAs}</p> : null}
      {asking ? <p class="small" data-testid="flow-session-ask"><Spinner /> {t('orders.flow.sessionAsk')}</p> : null}
      {sessionLine ? <p class="small muted" data-testid="flow-session">{sessionLine}</p> : null}
      {preparing ? (
        <p class="small muted" data-testid="flow-preparing">
          <Spinner /> {t('orders.flow.preparing')}
        </p>
      ) : null}
      <SendStatusLine sender={sender} data-testid="flow-status" />
      {!running ? (
        <div class="stack-sm" data-testid="flow-result">
          {message ? <p class="flow-line bad" data-testid="flow-error">{message}</p> : null}
          {anySubmitted ? <p class="flow-line" data-testid="flow-done">{tCount('orders.flow.done', states.filter((s) => s === 'submitted').length)}</p> : <p class="flow-line muted" data-testid="flow-stopped">{t('orders.flow.stopped')}</p>}
          {states.some((s) => s === 'pending') ? <p class="flow-line warn">{tCount('orders.flow.remaining', states.filter((s) => s === 'pending').length)}</p> : null}
        </div>
      ) : null}
      </div>
      {!running && props.onClose ? (
        <Button small variant="ghost" onClick={() => props.onClose?.()} aria-label={t('common.dismiss')} data-testid="tx-flow-dismiss">
          {'×'}
        </Button>
      ) : null}
    </div>
  );
  return host ? createPortal(card, host) : card;
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
