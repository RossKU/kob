// The send flow of an order action as a hook (`useSend`) and its one-line status (`SendStatusLine`): checking -> waiting for the wallet's approval
// -> sending -> submitted (txid, explorer link, then whether the network took it into a block), or one line saying why nothing was sent.
import { useEffect, useRef, useState } from 'preact/hooks';
import { useServices, useWallet } from '../../app/context';
import { explorerTxUrl } from '../../config';
import { t } from '../../i18n';
import type { PlacementRecord } from '../../kob/records';
import { expectedOutputs, signAndSubmit, waitForAcceptance } from '../../wallet/sign';
import { CopyText, RawDetails, Spinner } from '../kit';
import { classifyFailure } from './confirm-flow';
import { checkBeforeSigning, recordAccepted, type SendJob } from './send';
import './send.css';

export type SendPhase = 'checking' | 'signing' | 'sending' | 'submitted' | 'blocked' | 'rejected' | 'failed';

export interface SendStatus {
  phase: SendPhase;
  /** blocked / rejected / failed: the one sentence */
  text?: string;
  /** failed: the raw wallet / node text, behind "Details" */
  raw?: string | undefined;
  /** blocked / failed: the finding or failure code */
  code?: string;
  /** submitted */
  txid?: string;
  /** submitted: whether the network has taken it into a block yet */
  confirmed?: 'waiting' | 'confirmed' | 'unconfirmed';
}

export type SendOutcome = { status: 'submitted'; txid: string; records: PlacementRecord[] } | { status: 'stopped'; send: SendStatus };

/** how long the status waits for the transaction to show up in a block */
const ACCEPTANCE_TIMEOUT_MS = 90_000;

const BUSY: ReadonlySet<SendPhase> = new Set(['checking', 'signing', 'sending']);
export const sendBusy = (s: SendStatus | null): boolean => !!s && BUSY.has(s.phase);

export interface Sender {
  status: SendStatus | null;
  /** checks, asks the wallet, sends; resolves once the node accepted the transaction (its block is awaited in the background) or it stopped */
  send(job: SendJob): Promise<SendOutcome>;
  /** shows a reason the caller itself stopped for (a plan that cannot be made) */
  stop(text: string, raw?: string, code?: string): void;
  clear(): void;
}

export function useSend(): Sender {
  const services = useServices();
  const wallet = useWallet();
  // the latest wallet session: a ladder sends several transactions from one click
  const session = useRef(wallet);
  session.current = wallet;
  const [status, setStatus] = useState<SendStatus | null>(null);
  const seq = useRef(0);
  const alive = useRef(true);
  const abort = useRef(new AbortController());
  useEffect(() => {
    alive.current = true;
    return () => {
      alive.current = false;
      abort.current.abort();
    };
  }, []);
  // a status belongs to one send: a later one (the next level of a ladder) is never overwritten by an earlier one still waiting for its block
  const set = (id: number, s: SendStatus | null) => {
    if (alive.current && seq.current === id) setStatus(s);
  };

  const send = async (job: SendJob): Promise<SendOutcome> => {
    const id = ++seq.current;
    const stopped = (s: SendStatus): SendOutcome => {
      set(id, s);
      return { status: 'stopped', send: s };
    };
    set(id, { phase: 'checking' });
    const w = session.current;
    let checked;
    try {
      checked = await checkBeforeSigning(services, w, job);
    } catch (e) {
      return stopped({ phase: 'blocked', code: 'check-failed', text: t('send.checkFailed'), raw: e instanceof Error ? e.message : String(e) });
    }
    if ('blocked' in checked) return stopped({ phase: 'blocked', ...checked.blocked });
    // the job checked (the ticket's re-check may have put the plan of the state at the click in its place) is the one signed
    const final = checked.job;
    const { built } = final;
    let txid: string;
    try {
      // no `inputAddress`: `checkBeforeSigning` has just re-read every input on the node (the decoder stops any it does not confirm); a second
      // read would only add a round trip before the wallet's popup
      const res = await signAndSubmit({
        kob: services.kob, node: services.node, adapter: w.adapter!, built, network: services.config.network,
        ...(final.submitByDaa != null ? { submitByDaa: final.submitByDaa } : {}),
        onStage: (s) => set(id, { phase: s === 'signing' ? 'signing' : 'sending' }),
      });
      txid = res.txid;
    } catch (e) {
      const f = classifyFailure(e);
      if (f.kind === 'rejected') return stopped({ phase: 'rejected', code: f.code, text: f.text });
      // the node already holds exactly this transaction (a double click, a retry after a lost answer): it IS submitted
      if (f.kind !== 'known') return stopped({ phase: 'failed', code: f.code, text: f.text, raw: f.raw });
      txid = built.tx.id;
    }
    const { records } = await recordAccepted(services, w, built, txid, final.label);
    set(id, { phase: 'submitted', txid, confirmed: 'waiting' });
    void (async () => {
      let ok = false;
      try {
        ok = (await waitForAcceptance(services.node, expectedOutputs(services.sdk, services.config.network, built.tx), { timeoutMs: ACCEPTANCE_TIMEOUT_MS, signal: abort.current.signal })).accepted;
      } catch {
        ok = false;
      }
      set(id, { phase: 'submitted', txid, confirmed: ok ? 'confirmed' : 'unconfirmed' });
    })();
    return { status: 'submitted', txid, records };
  };

  return {
    status,
    send,
    stop: (text, raw, code = 'plan') => set(++seq.current, { phase: 'blocked', code, text, raw }),
    clear: () => set(++seq.current, null),
  };
}

const PHASE_TEXT: Record<'checking' | 'signing' | 'sending', string> = { checking: 'send.checking', signing: 'send.signing', sending: 'send.sending' };

/** One compact line: progress, the submitted transaction, or why nothing was sent. `lead` prefixes it (a ladder's "Level 2 of 3"). */
export function SendStatusLine(props: { status: SendStatus | null; lead?: string | null; 'data-testid'?: string }) {
  const { config } = useServices();
  const s = props.status;
  if (!s) return null;
  const busy = sendBusy(s);
  const bad = s.phase === 'blocked' || s.phase === 'failed';
  let body;
  if (busy) {
    body = (
      <>
        <Spinner /> <span class="send-text">{t(PHASE_TEXT[s.phase as keyof typeof PHASE_TEXT])}</span>
      </>
    );
  } else if (s.phase === 'submitted' && s.txid) {
    const state = s.confirmed ?? 'waiting';
    body = (
      <>
        <span class="send-text" data-testid="tx-status" data-status={state === 'waiting' ? 'submitted' : state}>
          {t(`send.submitted.${state}`)}
        </span>{' '}
        <CopyText value={s.txid} href={explorerTxUrl(config, s.txid)} data-testid="tx-id" />
      </>
    );
  } else {
    body = (
      <>
        <span class="send-text" data-testid="send-error" data-code={s.code ?? ''}>
          {s.text ?? ''}
        </span>
        <RawDetails text={s.raw} />
      </>
    );
  }
  return (
    <div class={`send send-${s.phase}`} data-testid={props['data-testid'] ?? 'send-status'} data-phase={s.phase} role={bad ? 'alert' : 'status'} aria-live="polite">
      {props.lead ? <span class="send-lead">{props.lead}</span> : null}
      {body}
    </div>
  );
}
