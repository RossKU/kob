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
//
// While the confirmation is open it follows the market and the clock (`SendJob.replan`, every LIVE_MS): each plan is checked as strictly as the
// first and put in place of the shown one, so what is approved is what is shown. The private-key signer takes it as an update and checks it
// itself (signer/core.ts); the app's popup flashes the changed figures, says so and holds its button for UPDATE_HOLD_MS when one is worse for
// the user than what was shown. Their times are said from the node clock ("in 28 s").
import { useEffect, useRef, useState } from 'preact/hooks';
import { useServices, useWallet } from '../../app/context';
import { readClock } from '../../app/env';
import { signerTokenHints } from '../../app/token-hints';
import { explorerTxUrl } from '../../config';
import { t } from '../../i18n';
import type { SigningSummary } from '../../kob/decode';
import type { PlacementRecord } from '../../kob/records';
import { sameOrBetter } from '../../signer/retime';
import { summaryLines, type SignerSummary, type SummaryOrder } from '../../signer/summary';
import { DAA_PER_SECOND, untilWords } from '../../signer/words';
import { expectedOutputs, signAndSubmit, waitForAcceptance } from '../../wallet/sign';
import type { LiveSigning } from '../../wallet/types';
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
  /** confirming: the cells (`cellTexts` keys) the last plan changed (they flash), and its count */
  changed?: string[];
  rev?: number;
  /** confirming: the button is held until then (ms; a plan worse for the user than what was shown) */
  holdUntil?: number;
  /** confirming: the node's DAA score now (projected), for the times of the table */
  daa?: bigint | null;
}

export type SendOutcome = { status: 'submitted'; txid: string; records: PlacementRecord[] } | { status: 'stopped'; send: SendStatus };

/** how long the status waits for the transaction to show up in a block */
const ACCEPTANCE_TIMEOUT_MS = 90_000;
/** how long the line of a transaction the network accepted stays */
export const ACCEPTED_CLEAR_MS = 60_000;
/** an auction confirmed in the app's popup later than this after the popup opened is planned again with the current clock before the wallet gets it */
export const RETIME_AFTER_MS = 1_000;
/** how often the open final confirmation plans the order again (the market and the clock now) */
export const LIVE_MS = 1_500;
/** how long the button of the app's popup is held after a plan worse for the user than what was shown */
export const UPDATE_HOLD_MS = 500;
/** how often the node clock is read again for the times of the open confirmation (projected at 10 DAA a second in between) */
const CLOCK_READ_MS = 5_000;

const sleep = (ms: number) => new Promise<void>((r) => setTimeout(r, ms));

/** The texts of a table by cell (what changes between two plans is told by it). */
export function cellTexts(s: SignerSummary): Map<string, string> {
  const out = new Map<string, string>();
  s.lines.forEach((l, i) => out.set(`l:${i}`, `${l.label}=${l.value}`));
  out.set('fee', s.fee);
  const order = (o: SummaryOrder, key: string) => {
    out.set(`${key}:title`, o.title);
    // a start or an expiry moves with every plan: said afresh from the clock, not counted as a change
    o.rows.forEach((r, j) => out.set(`${key}:r:${j}`, r.daa ? `${r.label}=@` : `${r.label}=${r.value}`));
    if (o.exit) order(o.exit, `${key}:x`);
  };
  s.orders.forEach((o, i) => order(o, `o:${i}`));
  return out;
}

const changedCells = (a: SignerSummary, b: SignerSummary): string[] => {
  const before = cellTexts(a);
  return [...cellTexts(b)].filter(([k, v]) => before.get(k) !== v).map(([k]) => k);
};

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
  const tableOf = (j: SendJob, decoded: SigningSummary): SignerSummary =>
    summaryLines(decoded, signerTokenHints(services.kob, j.built, session.current.info?.pubkey ?? null, services.registry, services.config.network), services.kob);

  /**
   * Plans `job` again every LIVE_MS while `running()`, checks each plan as strictly as the first (`checkBeforeSigning`) and hands each that passes
   * to `onPlan`; a plan that cannot be made or does not pass is skipped (the shown one stays).
   */
  const follow = (job: SendJob, running: () => boolean, onPlan: (j: SendJob, decoded: SigningSummary) => void): void => {
    if (!job.replan) return;
    const replan = job.replan;
    void (async () => {
      while (running()) {
        await sleep(LIVE_MS);
        if (!running()) return;
        try {
          const next = await replan();
          if (!running() || 'stop' in next) continue;
          const again = await checkBeforeSigning(services, session.current, next);
          if (!running() || 'blocked' in again) continue;
          onPlan(again.job, again.decoded);
        } catch {
          /* the next round tries again */
        }
      }
    })();
  };

  /** The node's DAA score every second while `running()` (read every CLOCK_READ_MS, projected in between), for the times of the confirmation. */
  const followClock = (running: () => boolean, onDaa: (daa: bigint) => void): void => {
    void (async () => {
      let base: { daa: bigint; at: number } | null = null;
      while (running()) {
        if (!base || Date.now() - base.at >= CLOCK_READ_MS) {
          try {
            base = { daa: (await readClock(services)).daa, at: Date.now() };
          } catch {
            /* projected from the last read */
          }
        }
        if (!running()) return;
        if (base) onDaa(base.daa + (BigInt(Date.now() - base.at) * DAA_PER_SECOND) / 1000n);
        await sleep(1000);
      }
    })();
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
      // the popup follows the market and the clock while it is open: what Continue hands over is what it shows
      let plannedAt = Date.now();
      let open = true;
      let baseline = shown;
      let view: SendStatus = { phase: 'confirming', table: tableOf(current, shown), walletLabel: session.current.info?.label ?? '', updated, changed: [], rev: 0, holdUntil: 0, daa: null };
      const draw = () => open && set(id, view);
      follow(current, () => open, (j, d) => {
        const worse = !sameOrBetter(baseline, d);
        if (worse) baseline = d;
        const table = tableOf(j, d);
        const changed = changedCells(view.table!, table);
        current = j;
        shown = d;
        plannedAt = Date.now();
        // the table follows every plan (its times too); only the figures that changed flash
        view = {
          ...view, table, changed, rev: (view.rev ?? 0) + (changed.length ? 1 : 0), updated: !!view.updated || worse,
          holdUntil: worse ? Date.now() + UPDATE_HOLD_MS : (view.holdUntil ?? 0),
        };
        draw();
      });
      followClock(() => open, (daa) => {
        view = { ...view, daa };
        draw();
      });
      const ok = await new Promise<boolean>((resolve) => {
        answer.current = resolve;
        draw();
      });
      open = false;
      if (!ok) return { phase: 'rejected', code: 'cancelled', text: t('send.cancelled') };
      set(id, { phase: 'checking' });
      // a popup that follows the market is as fresh as its last plan (re-timed only when that is older than two rounds); one that does not is
      // re-timed when confirmed later than RETIME_AFTER_MS
      if (!current.retime || current.submitByDaa == null || Date.now() - plannedAt <= (current.replan ? 2 * LIVE_MS : RETIME_AFTER_MS)) break;
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
      // the private-key signer follows the market and the clock while the user reads (its own check decides how each update is shown)
      const jobs = new Map<string, SendJob>([[final.built.tx.id, final]]);
      let reading = true;
      const live: LiveSigning | null =
        w.adapter!.id === 'local-key' && !final.retimed
          ? {
              attach: (to) => {
                follow(final, () => reading, (j) => {
                  jobs.set(j.built.tx.id, j);
                  to.replace(j.built);
                });
                followClock(() => reading, (daa) => to.clock(daa));
              },
            }
          : null;
      try {
        // no `inputAddress`: `checkBeforeSigning` has just re-read every input on the node (the decoder stops any it does not confirm); a second
        // read would only add a round trip before the wallet's popup
        const res = await signAndSubmit({
          kob: services.kob, node: services.node, adapter: w.adapter!, built: final.built, network: services.config.network,
          ...(final.submitByDaa != null ? { submitByDaa: final.submitByDaa } : {}),
          ...(final.retimed ? { retime: true } : {}),
          ...(live ? { live: { signing: live, submitByOf: (b) => jobs.get(b.tx.id)?.submitByDaa ?? undefined } } : {}),
          onStage: (s) => {
            if (s !== 'signing') reading = false;
            set(id, { phase: s === 'signing' ? 'signing' : 'sending' });
          },
        });
        reading = false;
        final = jobs.get(res.built.tx.id) ?? final;
        txid = res.txid;
        break;
      } catch (e) {
        reading = false;
        // what was signed (an update of the request) is what a re-timing starts from
        if (live?.signed) final = jobs.get(live.signed.tx.id) ?? final;
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
      // an accepted transaction's line goes after ACCEPTED_CLEAR_MS (unless a later send replaced it); an unconfirmed one or an error stays
      if (ok) setTimeout(() => set(id, null), ACCEPTED_CLEAR_MS);
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
function OrderTable(props: { order: SummaryOrder; testid: string; cell: string; live: Live }) {
  const o = props.order;
  return (
    <section class="send-order" data-testid={props.testid}>
      <h3><span key={o.title}>{o.title}</span></h3>
      <p class="send-token"><span key={o.token}>{o.token}</span></p>
      <dl class="send-lines">
        {o.rows.map((r, i) => (
          <div class={`send-line${props.live.flash(`${props.cell}:r:${i}`)}`} key={i}>
            <dt><span key={r.label}>{r.label}</span></dt>
            <dd {...(r.daa ? { 'data-daa': r.daa } : {})}>{r.daa ? <TimeValue daa={BigInt(r.daa)} value={r.value} now={props.live.daa} /> : <span key={r.value}>{r.value}</span>}</dd>
          </div>
        ))}
      </dl>
      {o.exit ? <OrderTable order={o.exit} testid={`${props.testid}-exit`} cell={`${props.cell}:x`} live={props.live} /> : null}
    </section>
  );
}

/** What the open popup follows: the cells to flash (their class) and the node's DAA score now. */
interface Live {
  flash(cell: string): string;
  daa: bigint | null;
}

/** A start or an expiry: "in 28 s" from the clock, the DAA score below; the row's own text without a clock. */
function TimeValue(props: { daa: bigint; value: string; now: bigint | null }) {
  if (props.now === null) return <span key={props.value}>{props.value}</span>;
  const rel = untilWords(props.daa, props.now, {
    now: t('signer.rel.now'),
    s: (n) => t('signer.rel.s', { n }),
    min: (n) => t('signer.rel.min', { n }),
    h: (n) => t('signer.rel.h', { n }),
    d: (n) => t('signer.rel.d', { n }),
  });
  return (
    <>
      <span class="send-rel" key={rel}>{rel}</span>
      <span class="send-daa" key={props.daa.toString()}>{t('signer.rel.daa', { daa: props.daa.toString() })}</span>
    </>
  );
}

/**
 * The final confirmation the app shows before an external wallet gets the transaction: the private-key signer's table (the net effect, what is
 * locked, the fee, each order's terms), and Cancel / Continue in wallet.
 */
function ConfirmPopup(props: { status: SendStatus; onAnswer(ok: boolean): void }) {
  const st = props.status;
  const s = st.table!;
  // held after a plan worse for the user: re-rendered when the hold ends
  const [, tick] = useState(0);
  const held = (st.holdUntil ?? 0) > Date.now();
  useEffect(() => {
    if (!held) return;
    const timer = setTimeout(() => tick((n) => n + 1), (st.holdUntil ?? 0) - Date.now() + 10);
    return () => clearTimeout(timer);
  }, [st.holdUntil, held]);
  const changed = new Set(st.changed ?? []);
  // a changed cell flashes: its class carries the plan's count, so the next change restarts it
  const live: Live = { flash: (cell) => (changed.has(cell) ? ` send-changed send-changed-${(st.rev ?? 0) % 2}` : ''), daa: st.daa ?? null };
  return (
    <Modal
      title={<span>{st.walletLabel ?? ''}</span>}
      onClose={() => props.onAnswer(false)}
      data-testid="confirm-order"
      footer={
        <>
          <Button onClick={() => props.onAnswer(false)} data-testid="confirm-order-cancel"><span>{t('common.cancel')}</span></Button>
          <Button variant="primary" onClick={() => props.onAnswer(true)} disabled={held} data-testid="confirm-order-ok" data-autofocus><span>{t('send.continueInWallet')}</span></Button>
        </>
      }
    >
      <div class="send-confirm">
        {st.updated ? <p class="send-updated" data-testid="confirm-order-updated"><span>{t('send.updated')}</span></p> : null}
        <h2 data-testid="confirm-order-title"><span key={s.title}>{s.title}</span></h2>
        <dl class="send-lines" data-testid="confirm-order-summary">
          {s.lines.map((l, i) => (
            <div class={`send-line ${l.dir ?? ''}${live.flash(`l:${i}`)}`} key={i}>
              <dt><span key={l.label}>{l.label}</span></dt>
              <dd><span key={l.value}>{l.value}</span></dd>
            </div>
          ))}
          <div class={`send-line send-fee${live.flash('fee')}`}>
            <dt><span>{t('signer.fee')}</span></dt>
            <dd data-testid="confirm-order-fee"><span key={s.fee}>{s.fee}</span></dd>
          </div>
        </dl>
        {s.orders.map((o, i) => (
          <OrderTable order={o} testid={`confirm-order-order-${i}`} cell={`o:${i}`} live={live} key={i} />
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
    <div class={`send send-${s.phase}`} data-testid={props['data-testid'] ?? 'send-status'} data-phase={s.phase} {...(s.confirmed ? { 'data-confirmed': s.confirmed } : {})} role={bad ? 'alert' : 'status'} aria-live="polite">
      {props.lead ? <span class="send-lead">{props.lead}</span> : null}
      {body}
      {s.phase === 'confirming' && s.table ? <ConfirmPopup status={s} onAnswer={props.sender.decide} /> : null}
    </div>
  );
}
