// The PRE-SIGN CONFIRMATION SCREEN of a token issuance (the Issue token page). Order actions have none (ui/confirm/send.ts): a genesis creates a
// token whose facts can never change, so this dialog decodes the built transaction with the issuance verifier, shows the WHOLE effect in plain
// words, blocks signing on any blocking finding, asks for an explicit acknowledgement, then runs `signAndSubmit` with a stage stepper and reports
// the accepted transaction.
import { useCallback, useEffect, useMemo, useRef, useState } from 'preact/hooks';
import { useServices, useWallet } from '../../app/context';
import { readClock } from '../../app/env';
import { formatDateTime, has, t } from '../../i18n';
import { issueText, walletNoticeText } from '../../i18n/issue-text';
import { describeInputsForWallet } from '../../kob/decode';
import type { IssueToken } from '../../kob/issue';
import type { Clock } from '../../kob/plan-types';
import type { PlacementRecord } from '../../kob/records';
import type { BuiltTx } from '../../kob/types';
import { formatUtc } from '../../kob/daa';
import { expectedOutputs, signAndSubmit, waitForAcceptance, type SignStage } from '../../wallet/sign';
import { Banner, Button, CopyText, Modal, RawDetails, Spinner, copyToClipboard } from '../kit';
import { shortId } from '../kit/format';
import type { ConfirmModel, Finding, Row, Section } from './confirm-model';
import { buildIssuanceModel } from './issuance-model';
import { feeDisclosureOf } from './fee-disclosure';
import { classifyFailure, explorerTxUrl, stepStates, type Failure } from './confirm-flow';
import { recordAccepted } from './send';
import './confirm.css';

export type ConfirmResult = { status: 'cancelled' } | { status: 'submitted'; txid: string; records: PlacementRecord[] } | { status: 'failed'; message: string };

export interface ConfirmSignProps {
  /** the unsigned genesis transaction to decode, show, sign and broadcast */
  built: BuiltTx;
  /** i18n key or literal heading of the action */
  title: string;
  /** label stored in the records of the transaction */
  label?: string;
  /** the token facts the planner claims: a genesis has no token inputs, so they are verified against the transaction */
  issue: { token: IssueToken };
  /** called once when the flow ends (cancelled, submitted or failed) */
  onClose(result: ConfirmResult): void;
}

type Phase = 'review' | 'busy' | 'submitted';
type Confirmed = 'waiting' | 'confirmed' | 'unconfirmed';

const tone = (t0: string): string => `cf-${t0}`;

function Rows(props: { rows: Row[]; testPrefix: string }) {
  if (props.rows.length === 0) return null;
  return (
    <dl class="cf-rows">
      {props.rows.map((r) => (
        <div class={`cf-row ${tone(r.tone)}`} key={r.id}>
          <dt>{r.label}</dt>
          <dd data-testid={`${props.testPrefix}-${r.id}`}>
            <span class="cf-value">{r.value}</span>
            {r.detail ? <span class="cf-detail">{r.detail}</span> : null}
          </dd>
        </div>
      ))}
    </dl>
  );
}

function SectionView(props: { section: Section }) {
  const s = props.section;
  return (
    <section class={`cf-section cf-section-${s.id}`} data-testid={`confirm-section-${s.id}`}>
      <h3>{s.title}</h3>
      {s.note ? <p class="cf-note">{s.note}</p> : null}
      <Rows rows={s.rows} testPrefix={`confirm-${s.id}`} />
    </section>
  );
}

const findingText = (f: Finding): string => f.text ?? issueText(f);

function FindingList(props: { items: Finding[]; testid: string; class: string }) {
  if (props.items.length === 0) return null;
  return (
    <ul class={`cf-findings ${props.class}`} data-testid={props.testid}>
      {props.items.map((f, n) => (
        <li key={`${f.code}-${n}`} data-code={f.code}>
          {findingText(f)}
        </li>
      ))}
    </ul>
  );
}

export function ConfirmSign(props: ConfirmSignProps) {
  const services = useServices();
  const wallet = useWallet();
  const { built } = props;
  const { kob, node, sdk, config } = services;

  const [clock, setClock] = useState<Clock | null>(null);
  const [ack, setAck] = useState(false);
  const [phase, setPhase] = useState<Phase>('review');
  const [stage, setStage] = useState<SignStage | null>(null);
  const [failure, setFailure] = useState<Failure | null>(null);
  const [txid, setTxid] = useState<string | null>(null);
  const [confirmed, setConfirmed] = useState<Confirmed>('waiting');
  const [copied, setCopied] = useState(false);
  const result = useRef<{ txid: string; records: PlacementRecord[] } | null>(null);
  const closed = useRef(false);
  const alive = useRef(true);
  const abort = useRef(new AbortController());

  useEffect(() => {
    alive.current = true;
    readClock(services).then((c) => alive.current && setClock(c), () => undefined);
    return () => {
      alive.current = false;
      abort.current.abort();
    };
  }, []);

  const maker = wallet.info?.pubkey ?? built.sign[0]?.pubkey ?? '';

  const model: ConfirmModel = useMemo(() => {
    const notices = describeInputsForWallet(built, wallet.info?.id).notices;
    // the fee rate, its bucket and the notes (fallback to the minimum, caps): what the dynamic fee policy decided for this transaction
    const fee = feeDisclosureOf(built);
    // a dynamic fee is several times the relay floor in a busy market: the verifier's sanity ceiling follows the policy's own per-transaction cap
    const feePolicy = services.fees?.policy;
    return buildIssuanceModel({ kob, built, token: props.issue.token, maker, fee, ...(feePolicy ? { feePolicy } : {}) }, notices);
  }, [built, props.issue, maker, wallet.info?.id]);

  // problems that are not about the transaction itself
  const extraBlocking: Finding[] = [];
  if (!wallet.info || !wallet.adapter) extraBlocking.push({ code: 'no-wallet', severity: 'blocking', message: 'no wallet', text: t('confirm.noWallet') });
  else if (phase !== 'submitted' && built.sign.some((s) => s.pubkey !== wallet.info!.pubkey)) {
    // the plan spends coins of the key it was built for: after an account switch this screen must not sign it (the plan is stale)
    const planned = built.sign.find((s) => s.pubkey !== wallet.info!.pubkey)!.pubkey;
    extraBlocking.push({ code: 'account-changed', severity: 'blocking', message: 'account changed', text: t('confirm.accountChanged', { planned: shortId(planned), now: shortId(wallet.info.pubkey) }) });
  } else if (wallet.networkMismatch) {
    extraBlocking.push({ code: 'network', severity: 'blocking', message: 'network', text: t('confirm.networkMismatch', { wallet: wallet.info.network, app: config.network }) });
  }
  const blocking = [...extraBlocking, ...model.blocking];
  const busy = phase === 'busy';
  const canSign = blocking.length === 0 && model.canSign && ack && phase === 'review';

  const finish = useCallback(
    (r: ConfirmResult) => {
      if (closed.current) return;
      closed.current = true;
      props.onClose(r);
    },
    [props.onClose],
  );

  const afterAccepted = async (id: string): Promise<void> => {
    // the node keeps listing these inputs until the transaction is in a block (no plan of this page may pick them meanwhile); records saved
    const { records } = await recordAccepted(services, wallet, built, id, props.label);
    if (!alive.current) return;
    result.current = { txid: id, records };
    setTxid(id);
    setPhase('submitted');
    setConfirmed('waiting');
    try {
      const outs = expectedOutputs(sdk, config.network, built.tx);
      const res = await waitForAcceptance(node, outs, { timeoutMs: 90_000, signal: abort.current.signal });
      if (alive.current) setConfirmed(res.accepted ? 'confirmed' : 'unconfirmed');
    } catch {
      if (alive.current) setConfirmed('unconfirmed');
    }
  };

  const sign = async () => {
    if (!canSign || !wallet.adapter) return;
    setPhase('busy');
    setFailure(null);
    setStage('signing');
    try {
      const res = await signAndSubmit({ kob, node, adapter: wallet.adapter, built, network: config.network, onStage: (s) => alive.current && setStage(s) });
      await afterAccepted(res.txid);
    } catch (e) {
      if (!alive.current) return;
      const f = classifyFailure(e);
      if (f.kind === 'known') {
        // the node already holds exactly this transaction (a double click, a retry after a lost answer): it IS submitted
        await afterAccepted(built.tx.id);
        return;
      }
      setFailure(f);
      setPhase('review');
      setStage(null);
      // an acknowledgement belongs to one attempt: the user looks again before signing again
      setAck(false);
    }
  };

  const close = () => {
    if (busy) return;
    if (phase === 'submitted' && result.current) finish({ status: 'submitted', ...result.current });
    else finish({ status: 'cancelled' });
  };

  const explorer = txid ? explorerTxUrl(config.network, txid, config.explorerUrl) : null;
  const title = has(props.title) ? t(props.title) : props.title;
  const steps = stepStates(phase === 'submitted' ? 'submitted' : stage);

  return (
    <Modal title={title} large dismissable={!busy} hideClose={busy} onClose={close} data-testid="confirm-screen">
      <div class="cf" data-phase={phase} data-kind={model.kind}>
        <p class="cf-intro">{model.intro}</p>

        {blocking.length > 0 ? (
          <div class="cf-blocking" data-testid="confirm-blocking" role="alert">
            <h3>{t('confirm.blockingTitle')}</h3>
            <p>{t('confirm.blockingText')}</p>
            <FindingList items={blocking} testid="confirm-blocking-list" class="cf-list-bad" />
          </div>
        ) : null}

        {failure && failure.kind !== 'rejected' ? (
          <Banner tone="error" title={t('confirm.failedTitle')} data-testid="confirm-error">
            <p>{failure.text}</p>
            <RawDetails text={failure.raw} />
            {failure.kind === 'replan' ? <p>{t('confirm.replanHint')}</p> : null}
          </Banner>
        ) : null}
        {failure && failure.kind === 'rejected' ? (
          <Banner tone="info" title={t('confirm.rejectedTitle')} data-testid="confirm-rejected">
            <p>{failure.text}</p>
          </Banner>
        ) : null}

        <div class="cf-summary" data-testid="confirm-summary">
          {model.sections.filter((s) => !s.fold).map((s) => (
            <SectionView section={s} key={s.id} />
          ))}
          {/* the gross flows (the wallet's inputs and change) behind the net effect above */}
          {model.sections.some((s) => s.fold) ? (
            <details class="cf-flows" data-testid="confirm-flows">
              <summary>{t('confirm.flows.title')}</summary>
              {model.sections.filter((s) => s.fold).map((s) => (
                <SectionView section={s} key={s.id} />
              ))}
            </details>
          ) : null}
        </div>

        <FindingList items={model.warnings} testid="confirm-warnings" class="cf-list-warn" />
        <FindingList items={model.info} testid="confirm-info" class="cf-list-info" />

        {model.notices.length > 0 ? (
          <aside class="cf-wallet" data-testid="confirm-wallet-notice">
            <h3>{t('confirm.wallet.title', { wallet: wallet.info?.label ?? t('confirm.wallet.yours') })}</h3>
            <p>{t('confirm.wallet.lead', { count: built.sign.length })}</p>
            <ul>
              {model.notices.map((n) => (
                <li key={n.code} data-code={n.code}>
                  {walletNoticeText(n)}
                </li>
              ))}
            </ul>
          </aside>
        ) : null}

        <details class="cf-advanced" data-testid="confirm-advanced">
          <summary>{t('confirm.advanced.title')}</summary>
          <dl class="cf-rows">
            <div class="cf-row">
              <dt>{t('confirm.advanced.txid')}</dt>
              <dd class="mono wrap-anywhere" data-testid="confirm-txid">{model.advanced.txid}</dd>
            </div>
            <div class="cf-row">
              <dt>{t('confirm.advanced.signatures')}</dt>
              <dd>{model.advanced.signatures}</dd>
            </div>
            <div class="cf-row">
              <dt>{t('confirm.advanced.fee')}</dt>
              <dd>{t('confirm.advanced.feeValue', model.advanced.fee)}</dd>
            </div>
            <div class="cf-row">
              <dt>{t('confirm.advanced.feeMode')}</dt>
              <dd data-testid="confirm-fee-mode">{t(model.advanced.fee.mode === 'priority' ? 'confirm.advanced.feeModePriority' : 'confirm.advanced.feeModeRelay', model.advanced.fee)}</dd>
            </div>
          </dl>
          <h4>{t('confirm.advanced.inputs')}</h4>
          <ol class="cf-io" start={0}>
            {model.advanced.inputs.map((i) => (
              <li key={i.index} class={i.willSign ? 'cf-sign' : ''}>
                {i.text}
                {i.willSign ? <span class="cf-badge">{t('confirm.advanced.signed')}</span> : null}
              </li>
            ))}
          </ol>
          <h4>{t('confirm.advanced.outputs')}</h4>
          <ol class="cf-io" start={0}>
            {model.advanced.outputs.map((o) => (
              <li key={o.index} class={o.flagged ? 'cf-flag' : ''}>
                {o.text}
              </li>
            ))}
          </ol>
          {model.advanced.payload.length > 0 ? (
            <>
              <h4>{t('confirm.advanced.payload')}</h4>
              <ul class="cf-io">
                {model.advanced.payload.map((p, n) => (
                  <li key={n}>{p}</li>
                ))}
              </ul>
            </>
          ) : null}
          <Button
            small
            data-testid="confirm-copy-tx"
            onClick={async () => {
              setCopied(await copyToClipboard(JSON.stringify(built.tx, null, 2)));
              setTimeout(() => alive.current && setCopied(false), 1500);
            }}
          >
            {copied ? t('common.copied') : t('confirm.advanced.copyTx')}
          </Button>
        </details>

        {busy || phase === 'submitted' ? (
          <ol class="cf-stages" data-testid="confirm-stages" data-stage={stage ?? ''} aria-live="polite">
            {steps.map((s) => (
              <li key={s.stage} class={`cf-stage cf-stage-${s.state}`} data-stage={s.stage} data-state={s.state}>
                {s.state === 'active' ? <Spinner /> : <span class="cf-tick" aria-hidden="true">{s.state === 'done' ? '✓' : '•'}</span>}
                {t(`confirm.stage.${s.stage}`)}
              </li>
            ))}
          </ol>
        ) : null}

        {phase === 'submitted' && txid ? (
          <div class="cf-result" data-testid="tx-result">
            <Banner tone={confirmed === 'confirmed' ? 'ok' : 'info'} title={t('confirm.submittedTitle')}>
              <p data-testid="tx-status" data-status={confirmed === 'waiting' ? 'submitted' : confirmed}>
                {confirmed === 'confirmed' ? t('confirm.status.confirmed') : confirmed === 'unconfirmed' ? t('confirm.status.unconfirmed') : t('confirm.status.submitted')}
              </p>
              <p class="cf-txid">
                <span>{t('confirm.advanced.txid')}: </span>
                <CopyText value={txid} short={false} data-testid="tx-id" />
              </p>
              {explorer ? (
                <p>
                  <a href={explorer} target="_blank" rel="noopener noreferrer" data-testid="tx-explorer">
                    {t('confirm.explorer')}
                  </a>
                </p>
              ) : null}
              {clock ? <p class="cf-note">{t('confirm.submittedAt', { time: `${formatDateTime(clock.unixSeconds)} (${formatUtc(clock.unixSeconds)})` })}</p> : null}
            </Banner>
          </div>
        ) : null}

        {phase !== 'submitted' ? (
          <label class="cf-ack" data-testid="confirm-ack-label">
            <input
              type="checkbox"
              checked={ack}
              disabled={blocking.length > 0 || busy}
              onChange={(e) => setAck((e.currentTarget as HTMLInputElement).checked)}
              data-testid="confirm-ack"
            />
            <span>{t('confirm.ack')}</span>
          </label>
        ) : null}

        <div class="cf-actions">
          {phase === 'submitted' ? (
            <Button variant="primary" onClick={close} data-testid="confirm-close" data-autofocus>
              {t('common.close')}
            </Button>
          ) : (
            <>
              <Button onClick={close} disabled={busy} data-testid="confirm-cancel">
                {t('common.cancel')}
              </Button>
              {failure?.kind === 'replan' ? (
                <Button onClick={() => finish({ status: 'failed', message: failure.text })} data-testid="confirm-replan">
                  {t('confirm.replan')}
                </Button>
              ) : null}
              <Button variant="primary" onClick={sign} disabled={!canSign || failure?.kind === 'replan'} loading={busy} data-testid="confirm-sign">
                {failure && failure.kind !== 'replan' ? t('confirm.retry') : t('confirm.sign')}
              </Button>
            </>
          )}
        </div>
      </div>
    </Modal>
  );
}
