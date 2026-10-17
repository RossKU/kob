// The send flow of an order action as a hook (`useSend`) and its one-line status (`SendStatusLine`): checking -> the final confirmation ->
// waiting for the wallet -> sending -> submitted (txid, explorer link, then whether the network took it into a block), or one line saying why
// nothing was sent.
//
// The final confirmation is the one place the order's details are shown: the private-key signer's table (signer/summary.ts, decoded from the
// transaction: the net effect on the wallet, what is locked, the fee, then each order with its token and terms). The signer shows it in its own
// frame (its Approve signs); for an external wallet the app shows the same table and its "Continue in wallet" hands the transaction to the
// wallet, whose own popup follows. A user may read it for a while: an auction whose start passed by the time it is signed
// (`clock-moved`, wallet/sign.ts) is re-timed (`SendJob.retime`: the same terms, later times) and signed again, which the private-key signer
// does without asking again when nothing the user confirmed got worse (signer/retime.ts); the app re-times an external wallet's auction
// confirmed later than RETIME_AFTER_MS before it hands it over (an updated popup only if something got worse).
import { useEffect, useRef, useState } from 'preact/hooks';
import { useServices, useWallet } from '../../app/context';
import { signerTokenHints } from '../../app/token-hints';
import { explorerTxUrl } from '../../config';
import { t } from '../../i18n';
import type { SigningSummary } from '../../kob/decode';
import type { PlacementRecord } from '../../kob/records';
import { sameOrBetter } from '../../signer/retime';
import { summaryLines, type SignerSummary, type SummaryOrder } from '../../signer/summary';
import { expectedOutputs, signAndSubmit, waitForAcceptance } from '../../wallet/sign';
import { Button, CopyText, Modal, RawDetails, Spinner } from '../kit';
import { classifyFailure } from './confirm-flow';
import { checkBeforeSigning, recordAccepted, type SendJob } from './send';
import './send.css';

export type SendPhase = 'checking' | 'confirming' | 'signing' | 'sending' | 'submitted' | 'blocked' | 'rejected' | 'failed';

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
  /** confirming: the table the popup shows, and whether it is the updated one of a re-timed order */
  table?: SignerSummary;
  /** confirming: the wallet the transaction goes to next (the popup's header) */
  walletLabel?: string;
  updated?: boolean;
}

export type SendOutcome = { status: 'submitted'; txid: string; records: PlacementRecord[] } | { status: 'stopped'; send: SendStatus };

/** how long the status waits for the transaction to show up in a block */
const ACCEPTANCE_TIMEOUT_MS = 90_000;
/** an auction confirmed in the app's popup later than this after the popup opened is planned again with the current clock before the wallet gets it */
export const RETIME_AFTER_MS = 1_000;

const BUSY: ReadonlySet<SendPhase> = new Set(['checking', 'confirming', 'signing', 'sending']);
export const sendBusy = (s: SendStatus | null): boolean => !!s && BUSY.has(s.phase);

export interface Sender {
  status: SendStatus | null;
  /** checks, asks the wallet, sends; resolves once the node accepted the transaction (its block is awaited in the background) or it stopped */
  send(job: SendJob): Promise<SendOutcome>;
  /** shows a reason the caller itself stopped for (a plan that cannot be made) */
  stop(text: string, raw?: string, code?: string): void;
  clear(): void;
  /** the user's answer to the final confirmation the app shows (an external wallet) */
  decide(ok: boolean): void;
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
  // the open popup's answer, and the decoding the user confirmed last (a re-timed order of it needs no second confirmation)
  const answer = useRef<((ok: boolean) => void) | null>(null);
  const confirmedLast = useRef<SigningSummary | null>(null);
  useEffect(() => () => answer.current?.(false), []);
  const ask = (id: number, j: SendJob, decoded: SigningSummary, updated: boolean): Promise<boolean> => {
    const hints = signerTokenHints(services.kob, j.built, session.current.info?.pubkey ?? null, services.registry);
    const table = summaryLines(decoded, hints, services.kob);
    return new Promise((resolve) => {
      answer.current = resolve;
      set(id, { phase: 'confirming', table, walletLabel: session.current.info?.label ?? '', updated });
    });
  };

  /**
   * The final confirmation of an external wallet's job: the popup, and at a late Confirm of an auction its re-timed job (accepted without asking
   * again when nothing got worse, else shown once more). The job to hand to the wallet, or the status it stopped with.
   */
  const confirmInApp = async (id: number, job: SendJob, decoded: SigningSummary): Promise<SendJob | SendStatus> => {
    let current = job;
    let shown = decoded;
    if (job.retimed && confirmedLast.current && sameOrBetter(confirmedLast.current, shown)) return job;
    let updated = false;
    for (;;) {
      const openedAt = Date.now();
      if (!(await ask(id, current, shown, updated))) return { phase: 'rejected', code: 'cancelled', text: t('send.cancelled') };
      set(id, { phase: 'checking' });
      if (!current.retime || current.submitByDaa == null || Date.now() - openedAt <= RETIME_AFTER_MS) break;
      const next = await current.retime();
      if ('stop' in next) return { phase: 'blocked', ...next.stop };
      const again = await checkBeforeSigning(services, session.current, next).catch(() => null);
      if (!again) return { phase: 'blocked', code: 'check-failed', text: t('send.checkFailed') };
      if ('blocked' in again) return { phase: 'blocked', ...again.blocked };
      const same = sameOrBetter(shown, again.decoded);
      current = again.job;
      shown = again.decoded;
      if (same) break;
      updated = true;
    }
    confirmedLast.current = shown;
    return current;
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
    let final = checked.job;
    // the final confirmation: the private-key signer shows it in its frame, for any other wallet the app shows it first
    if (w.adapter!.id !== 'local-key') {
      const confirmed = await confirmInApp(id, final, checked.decoded);
      if ('phase' in confirmed) return stopped(confirmed);
      final = confirmed;
    }
    let txid: string;
    for (let attempt = 0; ; attempt++) {
      try {
        // no `inputAddress`: `checkBeforeSigning` has just re-read every input on the node (the decoder stops any it does not confirm); a second
        // read would only add a round trip before the wallet's popup
        const res = await signAndSubmit({
          kob: services.kob, node: services.node, adapter: w.adapter!, built: final.built, network: services.config.network,
          ...(final.submitByDaa != null ? { submitByDaa: final.submitByDaa } : {}),
          ...(final.retimed ? { retime: true } : {}),
          onStage: (s) => set(id, { phase: s === 'signing' ? 'signing' : 'sending' }),
        });
        txid = res.txid;
        break;
      } catch (e) {
        const f = classifyFailure(e);
        if (f.kind === 'rejected') return stopped({ phase: 'rejected', code: f.code, text: f.text });
        // the node already holds exactly this transaction (a double click, a retry after a lost answer): it IS submitted
        if (f.kind === 'known') {
          txid = final.built.tx.id;
          break;
        }
        // the auction's start passed while it was signed: the same order with later times, checked and signed again (the private-key signer
        // signs it without a prompt: signer/retime.ts); nothing was sent
        if (f.code !== 'clock-moved' || !final.retime || attempt >= CLOCK_RETRIES) return stopped({ phase: 'failed', code: f.code, text: f.text, raw: f.raw });
        set(id, { phase: 'checking' });
        const next = await final.retime();
        if ('stop' in next) return stopped({ phase: 'blocked', ...next.stop });
        const again = await checkBeforeSigning(services, w, { ...next, retimed: true }).catch(() => null);
        if (!again) return stopped({ phase: 'blocked', code: 'check-failed', text: t('send.checkFailed') });
        if ('blocked' in again) return stopped({ phase: 'blocked', ...again.blocked });
        final = again.job;
      }
    }
    const { built } = final;
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
    decide: (ok) => {
      const a = answer.current;
      answer.current = null;
      a?.(ok);
    },
  };
}

/**
 * How often an auction whose start passed by the time it was signed (`clock-moved`) is re-timed and signed again before the reason is shown:
 * each attempt gives it a longer lead (the ticket's RETIME_LEAD_DAA, 3 s more each time), so a slow link gets there within a few.
 */
const CLOCK_RETRIES = 3;

const PHASE_TEXT: Record<'checking' | 'confirming' | 'signing' | 'sending', string> = {
  checking: 'send.checking', confirming: 'send.confirming', signing: 'send.signing', sending: 'send.sending',
};

/** One order of the table: its title, token and terms (an if-done entry's committed exit nested below), as the signer frame draws it. */
function OrderTable(props: { order: SummaryOrder; testid: string }) {
  const o = props.order;
  return (
    <section class="send-order" data-testid={props.testid}>
      <h3><span key={o.title}>{o.title}</span></h3>
      <p class="send-token"><span key={o.token}>{o.token}</span></p>
      <dl class="send-lines">
        {o.rows.map((r, i) => (
          <div class="send-line" key={i}>
            <dt><span key={r.label}>{r.label}</span></dt>
            <dd><span key={r.value}>{r.value}</span></dd>
          </div>
        ))}
      </dl>
      {o.exit ? <OrderTable order={o.exit} testid={`${props.testid}-exit`} /> : null}
    </section>
  );
}

/**
 * The final confirmation the app shows before an external wallet gets the transaction: the private-key signer's table (the net effect, what is
 * locked, the fee, each order's terms), and Cancel / Continue in wallet.
 */
function ConfirmPopup(props: { table: SignerSummary; wallet: string; updated: boolean; onAnswer(ok: boolean): void }) {
  const s = props.table;
  return (
    <Modal
      title={<span>{props.wallet}</span>}
      onClose={() => props.onAnswer(false)}
      data-testid="confirm-order"
      footer={
        <>
          <Button onClick={() => props.onAnswer(false)} data-testid="confirm-order-cancel"><span>{t('common.cancel')}</span></Button>
          <Button variant="primary" onClick={() => props.onAnswer(true)} data-testid="confirm-order-ok" data-autofocus><span>{t('send.continueInWallet')}</span></Button>
        </>
      }
    >
      <div class="send-confirm">
        {props.updated ? <p class="send-updated" data-testid="confirm-order-updated"><span>{t('send.updated')}</span></p> : null}
        <h2 data-testid="confirm-order-title"><span key={s.title}>{s.title}</span></h2>
        <dl class="send-lines" data-testid="confirm-order-summary">
          {s.lines.map((l, i) => (
            <div class={`send-line ${l.dir ?? ''}`} key={i}>
              <dt><span key={l.label}>{l.label}</span></dt>
              <dd><span key={l.value}>{l.value}</span></dd>
            </div>
          ))}
          <div class="send-line send-fee">
            <dt><span>{t('signer.fee')}</span></dt>
            <dd data-testid="confirm-order-fee"><span key={s.fee}>{s.fee}</span></dd>
          </div>
        </dl>
        {s.orders.map((o, i) => (
          <OrderTable order={o} testid={`confirm-order-order-${i}`} key={i} />
        ))}
        {s.problems > 0 ? <p class="send-problems" data-testid="confirm-order-problems"><span>{t('signer.problems', { count: s.problems })}</span></p> : null}
      </div>
    </Modal>
  );
}

/**
 * One compact line: progress, the submitted transaction, or why nothing was sent. `lead` prefixes it (a ladder's "Level 2 of 3"). It also holds
 * the final confirmation popup of an external wallet while the send waits for it.
 */
export function SendStatusLine(props: { sender: Sender; lead?: string | null; 'data-testid'?: string }) {
  const { config } = useServices();
  const s = props.sender.status;
  if (!s) return null;
  const busy = sendBusy(s);
  const bad = s.phase === 'blocked' || s.phase === 'failed';
  let body;
  if (busy) {
    body = (
      <>
        <Spinner /> <span class="send-text" key={s.phase}>{t(PHASE_TEXT[s.phase as keyof typeof PHASE_TEXT])}</span>
      </>
    );
  } else if (s.phase === 'submitted' && s.txid) {
    const state = s.confirmed ?? 'waiting';
    body = (
      <>
        <span class="send-text" data-testid="tx-status" data-status={state === 'waiting' ? 'submitted' : state}>
          <span key={state}>{t(`send.submitted.${state}`)}</span>
        </span>{' '}
        <CopyText value={s.txid} href={explorerTxUrl(config, s.txid)} data-testid="tx-id" />
      </>
    );
  } else {
    body = (
      <>
        <span class="send-text" data-testid="send-error" data-code={s.code ?? ''}>
          <span key={s.text ?? ''}>{s.text ?? ''}</span>
        </span>
        <RawDetails text={s.raw} />
      </>
    );
  }
  return (
    <div class={`send send-${s.phase}`} data-testid={props['data-testid'] ?? 'send-status'} data-phase={s.phase} role={bad ? 'alert' : 'status'} aria-live="polite">
      {props.lead ? <span class="send-lead">{props.lead}</span> : null}
      {body}
      {s.phase === 'confirming' && s.table ? <ConfirmPopup table={s.table} wallet={s.walletLabel ?? ''} updated={!!s.updated} onAnswer={props.sender.decide} /> : null}
    </div>
  );
}
