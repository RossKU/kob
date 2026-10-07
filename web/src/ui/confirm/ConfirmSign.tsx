// The PRE-SIGN CONFIRMATION SCREEN and signing flow shared by every transaction the app sends (placement, cancel, cancel-replace, cancel-all,
// refund, issuance). Wallet popups are blind to token semantics, so this dialog is the user's real check: it decodes the built transaction
// (`decodeSigning`, or the issuance verifier for a genesis), shows the WHOLE effect in plain words, blocks signing on any blocking finding,
// asks for an explicit acknowledgement, then runs `signAndSubmit` with a stage stepper and reports the accepted transaction.
import { useCallback, useEffect, useMemo, useRef, useState } from 'preact/hooks';
import { useServices, useWallet } from '../../app/context';
import { commitAccepted } from '../../app/commit';
import { readClock } from '../../app/env';
import { formatDateTime, has, t } from '../../i18n';
import { issueText, walletNoticeText } from '../../i18n/issue-text';
import { spkStringToAddress } from '../../data/kaspa-sdk';
import { confirmInputsOnNode, type NodeInputFacts } from '../../kob/node-verify';
import { decodeSigning, describeInputsForWallet, type ExpectedSigning } from '../../kob/decode';
import type { IssueToken } from '../../kob/issue';
import type { Clock } from '../../kob/plan-types';
import type { PlacementRecord } from '../../kob/records';
import { withExtraToken, type TokenInfo } from '../../kob/registry';
import type { BuiltTx } from '../../kob/types';
import { formatUtc } from '../../kob/daa';
import { expectedOutputs, signAndSubmit, waitForAcceptance, type SignStage } from '../../wallet/sign';
import { Banner, Button, CopyText, Modal, RawDetails, Spinner, copyToClipboard } from '../kit';
import { shortId } from '../kit/format';
import { OpenTokenCaution, PowersWarning } from '../market/TokenBadges';
import { buildConfirmModel, type Card, type ConfirmModel, type Finding, type Row, type Section } from './confirm-model';
import { buildIssuanceModel } from './issuance-model';
import { feeDisclosureOf } from './fee-disclosure';
import { classifyFailure, explorerTxUrl, stepStates, type Failure } from './confirm-flow';
import './confirm.css';

export type ConfirmResult = { status: 'cancelled' } | { status: 'submitted'; txid: string; records: PlacementRecord[] } | { status: 'failed'; message: string };

export interface ConfirmSignProps {
  /** the unsigned transaction to decode, show, sign and broadcast */
  built: BuiltTx;
  /** i18n key or literal heading of the action ("Place limit sell", "Cancel order", ...) */
  title: string;
  /** what the planner claims; a mismatch with the decoded tx blocks signing */
  expected?: ExpectedSigning;
  /** plan label stored in the placement records */
  label?: string;
  /** template capabilities of the token program of the order (indexer `powers`): freeze / seize show a warning above the summary */
  tokenPowers?: readonly string[];
  /** the order trades an open-list token (no registry entry): a caution line is shown and the token is labelled with its covenant id */
  openToken?: TokenInfo;
  /** an unverified registry token (not synthesised): only the caution is shown, the registry already knows it */
  cautionToken?: TokenInfo;
  /** issuance (genesis) transaction: it has no token inputs, so the order decoder cannot describe it; the token facts the planner claims are verified against the tx instead */
  issue?: { token: IssueToken };
  /**
   * consequences the planner had to accept to build this transaction at all (e.g. stray token UTXOs that do not fit and are abandoned): shown
   * as a warning above the summary, and signing needs a separate, explicit acknowledgement of them
   */
  acknowledge?: readonly string[];
  /** fired as soon as the node accepted the transaction (the dialog stays open showing the status until the user closes it) */
  onSubmitted?(r: { txid: string; records: PlacementRecord[] }): void;
  /** called once when the flow ends (cancelled, submitted or failed) */
  onClose(result: ConfirmResult): void;
  /**
   * the order in the words of the market as it is shown (an inverted KAS market, KAS/TOKEN: what is given and received in KAS and the token),
   * stated above the decoded transaction, which stays the authority
   */
  shownAs?: string | undefined;
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

function OrderCard(props: { card: Card; prefix: string; nested?: boolean }) {
  const c = props.card;
  return (
    <article class={`cf-card${c.flagged ? ' cf-flagged' : ''}${props.nested ? ' cf-nested' : ''}`} data-testid={`${props.prefix}-${c.id}`}>
      <header class="cf-card-head">
        <h4>{c.title}</h4>
        <span class={`cf-badge${c.flagged ? ' cf-badge-bad' : ''}`}>{c.badge}</span>
      </header>
      <Rows rows={c.rows} testPrefix={`${props.prefix}-${c.id}`} />
      {c.children.map((x) => (
        <OrderCard card={x} prefix={props.prefix} nested key={x.id} />
      ))}
    </article>
  );
}

function SectionView(props: { section: Section }) {
  const s = props.section;
  return (
    <section class={`cf-section cf-section-${s.id}`} data-testid={`confirm-section-${s.id}`}>
      <h3>{s.title}</h3>
      {s.note ? <p class="cf-note">{s.note}</p> : null}
      <Rows rows={s.rows} testPrefix={`confirm-${s.id}`} />
      {s.cards.map((c) => (
        <OrderCard card={c} prefix={`confirm-${s.id}`} key={c.id} />
      ))}
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
  // an open-list token is not in the registry: the screen decodes against the registry plus that token (labelled unverified)
  const registry = useMemo(() => (props.openToken ? withExtraToken(services.registry, props.openToken) : services.registry), [services.registry, props.openToken]);

  const [clock, setClock] = useState<Clock | null>(null);
  const [ack, setAck] = useState(false);
  const [ackPlan, setAckPlan] = useState(false);
  const mustAck = props.acknowledge ?? [];
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

  // the KAS value of every input is re-read from the NODE: the decoder flags any input the node does not confirm, and signing
  // stays disabled until the check has run. A genesis (issuance) only spends the maker's own P2PK coins, which the signature commits to.
  const inputAddress = useCallback((spk: string) => spkStringToAddress(sdk, spk, config.network), [sdk, config.network]);
  const [nodeInputs, setNodeInputs] = useState<NodeInputFacts | 'pending' | 'error'>('pending');
  useEffect(() => {
    if (props.issue) return;
    let live = true;
    setNodeInputs('pending');
    confirmInputsOnNode(node, built.tx.inputs, inputAddress).then(
      (r) => live && setNodeInputs(r.facts),
      () => live && setNodeInputs('error'),
    );
    return () => {
      live = false;
    };
  }, [built, props.issue, node, inputAddress]);

  const model: ConfirmModel = useMemo(() => {
    const notices = describeInputsForWallet(built, wallet.info?.id).notices;
    // the fee rate, its bucket and the notes (fallback to the minimum, caps): what the dynamic fee policy decided for this transaction
    const fee = feeDisclosureOf(built);
    // a dynamic fee is several times the relay floor in a busy market: the decoder's sanity ceiling follows the policy's own per-transaction cap
    const feePolicy = services.fees?.policy;
    if (props.issue) return buildIssuanceModel({ kob, built, token: props.issue.token, maker, fee, ...(feePolicy ? { feePolicy } : {}) }, notices);
    const summary = decodeSigning({ kob, built, maker, registry, expected: props.expected, nodeInputs: typeof nodeInputs === 'string' ? undefined : nodeInputs, ...(feePolicy ? { feePolicy } : {}) });
    return buildConfirmModel(summary, { registry, clock, fee }, notices);
  }, [built, props.expected, props.issue, maker, registry, clock, wallet.info?.id, nodeInputs]);

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
  if (!props.issue && nodeInputs === 'pending') extraBlocking.push({ code: 'inputs-checking', severity: 'blocking', message: 'checking inputs', text: t('confirm.inputsChecking') });
  if (!props.issue && nodeInputs === 'error') extraBlocking.push({ code: 'inputs-unavailable', severity: 'blocking', message: 'inputs unavailable', text: t('confirm.inputsUnavailable') });
  const blocking = [...extraBlocking, ...model.blocking];
  const busy = phase === 'busy';
  const canSign = blocking.length === 0 && model.canSign && ack && (mustAck.length === 0 || ackPlan) && phase === 'review';

  const finish = useCallback(
    (r: ConfirmResult) => {
      if (closed.current) return;
      closed.current = true;
      props.onClose(r);
    },
    [props.onClose],
  );

  const afterAccepted = async (id: string): Promise<void> => {
    let c = clock;
    try {
      c = await readClock(services);
    } catch {
      /* the clock is only a label of the placement record */
    }
    const at: Clock = c ?? { daa: 0n, unixSeconds: BigInt(Math.floor(Date.now() / 1000)), rateMilli: 10_000 };
    const records = await commitAccepted({ services, records: wallet.records, built, maker, txid: id, clock: at, label: props.label });
    if (!alive.current) return;
    result.current = { txid: id, records };
    setTxid(id);
    setPhase('submitted');
    setConfirmed('waiting');
    try {
      props.onSubmitted?.({ txid: id, records });
    } catch {
      /* a parent's refresh must not disturb the status */
    }
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
      const res = await signAndSubmit({ kob, node, adapter: wallet.adapter, built, network: config.network, inputAddress: props.issue ? undefined : inputAddress, onStage: (s) => alive.current && setStage(s) });
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
      setAckPlan(false);
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

        <OpenTokenCaution token={props.openToken ?? props.cautionToken} data-testid="confirm-open-caution" />
        <PowersWarning powers={props.tokenPowers} data-testid="confirm-powers-warning" />

        {mustAck.length > 0 ? (
          <Banner tone="warn" title={t('confirm.planAck.title')} data-testid="confirm-plan-warnings">
            <ul>
              {mustAck.map((x, i) => (
                <li key={i}>{x}</li>
              ))}
            </ul>
            {phase !== 'submitted' ? (
              <label class="cf-ack" data-testid="confirm-plan-ack-label">
                <input
                  type="checkbox"
                  checked={ackPlan}
                  disabled={busy}
                  onChange={(e) => setAckPlan((e.currentTarget as HTMLInputElement).checked)}
                  data-testid="confirm-plan-ack"
                />
                <span>{t('confirm.planAck.accept')}</span>
              </label>
            ) : null}
          </Banner>
        ) : null}

        {props.shownAs ? (
          <p class="cf-shown-as" data-testid="confirm-shown-as">
            {props.shownAs}
          </p>
        ) : null}
        <div class="cf-summary" data-testid="confirm-summary">
          {model.sections.map((s) => (
            <SectionView section={s} key={s.id} />
          ))}
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
