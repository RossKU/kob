// The PRE-SIGN CONFIRMATION SCREEN of a token creation (the Create token page). Order actions have none (ui/confirm/send.ts): a genesis creates a
// token whose facts can never change, so this dialog decodes the built transactions with the issuance verifier (the KCC-23 preparation and
// the genesis spending it), shows the WHOLE effect in plain words (a short summary first, the risk and the wallet notes one line each with their full
// text behind a "?", every verified section under "All details"), blocks signing on any blocking finding, asks for an explicit
// acknowledgement, then runs `signAndSubmitCreation` (wallet/create.ts: one approval for the private-key signer; an extension wallet signs and
// sends the preparation, then is asked for the genesis once its wallet node sees the preparation) with the signing stepper (Prepare, Create token), and reports the
// accepted genesis. A creation whose preparation went out but whose genesis did not (declined, not visible in time, refused) closes this
// screen as `pending`: the Create token page finishes it (its record, data/creations.ts) on this same screen (`resume`): the creation planned
// again from the record and checked as a fresh one, only the genesis signed (`finishCreation`).
import { useCallback, useEffect, useMemo, useRef, useState } from 'preact/hooks';
import { useServices, useWallet } from '../../app/context';
import { readClock } from '../../app/env';
import { has, t } from '../../i18n';
import { issueText, walletNoticeText } from '../../i18n/issue-text';
import { describeInputsForWallet } from '../../kob/decode';
import type { IssueCurve, IssueDeclaration, IssueDocs, IssueToken } from '../../kob/issue';
import type { Clock } from '../../kob/plan-types';
import type { PlacementRecord } from '../../kob/records';
import type { BuiltTx } from '../../kob/types';
import { formatUtc } from '../../kob/daa';
import { SignFlowError, expectedOutputs, signAndSubmit, waitForAcceptance, type SignStage } from '../../wallet/sign';
import { CREATION_INVALID, CREATION_SPENT, finishCreation, signAndSubmitCreation, type CreationStep } from '../../wallet/create';
import type { CreationRecord } from '../../data/creations';
import { pendingSpends } from '../../app/pending-spends';
import { Banner, Button, CopyText, HelpTip, Modal, RawDetails, Spinner, Stepper, copyToClipboard, unitParts } from '../kit';
import { rowTimeTitleUnix, shortId } from '../kit/format';
import type { BriefRow, ConfirmModel, Finding, Row, Section } from './confirm-model';
import { buildIssuanceModel } from './issuance-model';
import { feeDisclosureOf } from './fee-disclosure';
import { classifyFailure, explorerTxUrl, stepStates, type Failure } from './confirm-flow';
import { inputAddressOf, recordAccepted } from './send';
import { creationStepperItems } from './creation-steps';
import './confirm.css';

export type ConfirmResult =
  | { status: 'cancelled' }
  | { status: 'submitted'; txid: string; records: PlacementRecord[] }
  | { status: 'failed'; message: string; code?: string }
  /** the preparation is on chain, the genesis is not: the creation record finishes it (`declined`: the wallet did not sign the genesis) */
  | { status: 'pending'; message: string; declined?: boolean };

export interface ConfirmSignProps {
  /** the unsigned genesis transaction to decode, show, sign and broadcast */
  built: BuiltTx;
  /** i18n key or literal heading of the action */
  title: string;
  /** label stored in the records of the transaction */
  label?: string;
  /** the token facts the planner claims: a genesis has no token inputs, so they are verified against the transaction */
  issue: { token: IssueToken; curve?: IssueCurve; askSale?: { sell: string; price: string } };
  /**
   * a creation under KCC-23: the preparation the genesis (`built`) spends, with what it declares; signed with the genesis (one approval where
   * the wallet can give it) and sent first. Absent: a bare genesis (signAndSubmit).
   */
  creation?: { prep: BuiltTx; declaration: IssueDeclaration; docs: IssueDocs };
  /**
   * finishes this saved creation instead (`creation` and `built` are it planned again, wallet/create.ts `checkSavedCreation`): its preparation is
   * signed already, only the genesis is asked for. Closes as `failed` with the code when the coin is spent or the record does not check out.
   */
  resume?: CreationRecord;
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
            <span class="cf-value">{unitParts(r.value, true)}</span>
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

/** The short summary: one row per fact, its explanation behind a "?" beside the label. */
function Brief(props: { rows: BriefRow[] }) {
  return (
    <section class="cf-brief" data-testid="confirm-brief" aria-labelledby="cf-brief-title">
      <h3 id="cf-brief-title" class="sr-only">{t('confirm.brief.title')}</h3>
      <dl class="cf-rows">
        {props.rows.map((r) => (
          <div class={`cf-row ${tone(r.tone)}`} key={r.id}>
            <dt>
              <span class="help-title">
                <span>{r.label}</span>
                {r.help?.length ? (
                  <HelpTip label={r.label} data-testid={`confirm-brief-${r.id}-help`}>
                    {r.help.map((h, n) => <p key={n}>{h}</p>)}
                  </HelpTip>
                ) : null}
              </span>
            </dt>
            <dd data-testid={`confirm-brief-${r.id}`}>
              <span class="cf-value">{unitParts(r.value, true)}</span>
              {r.detail ? <span class="cf-detail">{r.detail}</span> : null}
            </dd>
          </div>
        ))}
      </dl>
    </section>
  );
}

/** what the wallet asks and shows: the codes of the creation's wallet notes (one line on the screen, the full text behind its "?") */
const WALLET_CODES = ['walletTwice', 'resumed', 'walletBlind'];

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
  /** a creation with a wallet that asks once per transaction: which approval it shows now (confirm.chain.*) */
  const [chainNote, setChainNote] = useState<'prep' | 'wait' | 'genesis' | null>(null);
  /** a creation: which of its two transactions the stages are of, and whether the node took the preparation (the stepper) */
  const [chainAt, setChainAt] = useState<{ step: CreationStep | null; waiting: boolean; prepSent: boolean; tried: boolean }>({ step: null, waiting: false, prepSent: !!props.resume?.prepSent, tried: false });
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
    const creation = props.creation ? { prep: props.creation.prep, oneApproval: !!wallet.adapter?.signChain, resumed: !!props.resume } : {};
    const ask = props.issue.askSale ? { askSale: { sell: BigInt(props.issue.askSale.sell), price: BigInt(props.issue.askSale.price) } } : {};
    return buildIssuanceModel({ kob, built, token: props.issue.token, ...(props.issue.curve ? { curve: props.issue.curve } : {}), ...ask, maker, fee, ...creation, ...(feePolicy ? { feePolicy } : {}) }, notices);
  }, [built, props.issue, props.creation, maker, wallet.info?.id]);

  // problems that are not about the transaction itself
  const extraBlocking: Finding[] = [];
  if (!wallet.info || !wallet.adapter) extraBlocking.push({ code: 'no-wallet', severity: 'blocking', message: 'no wallet', text: t('confirm.noWallet') });
  else if (phase !== 'submitted' && [...built.sign, ...(props.creation?.prep.sign ?? [])].some((s) => s.pubkey !== wallet.info!.pubkey)) {
    // the plan spends coins of the key it was built for: after an account switch this screen must not sign it (the plan is stale)
    const planned = [...built.sign, ...(props.creation?.prep.sign ?? [])].find((s) => s.pubkey !== wallet.info!.pubkey)!.pubkey;
    extraBlocking.push({ code: 'account-changed', severity: 'blocking', message: 'account changed', text: t('confirm.accountChanged', { planned: shortId(planned), now: shortId(wallet.info.pubkey) }) });
  } else if (wallet.networkMismatch) {
    extraBlocking.push({ code: 'network', severity: 'blocking', message: 'network', text: t('confirm.networkMismatch', { wallet: wallet.info.network, app: config.network }) });
  }
  const blocking = [...extraBlocking, ...model.blocking];
  // the wallet's notes go on the wallet line; the rest (holders of other keys, fee notes) is in the summary's "?" and in the details
  const walletInfo = model.info.filter((f) => WALLET_CODES.includes(f.code));
  const otherInfo = model.info.filter((f) => !WALLET_CODES.includes(f.code));
  /** "Unaudited token program" (and how many other risk notes are behind the "?") */
  const riskWords = (): string => {
    const audited = model.warnings.some((w) => w.code === 'unaudited');
    const rest = model.warnings.length - (audited ? 1 : 0);
    if (!audited) return t('confirm.risk.notes');
    return rest > 0 ? `${t('confirm.risk.unaudited')} · ${t(rest === 1 ? 'confirm.risk.more.one' : 'confirm.risk.more.other', { count: rest })}` : t('confirm.risk.unaudited');
  };
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
      const onStage = (s: SignStage) => alive.current && setStage(s);
      const addressOf = inputAddressOf(services);
      if (props.resume) {
        // a saved creation: planned and checked again inside, the genesis signed once the node lists the coin of the first step
        const rec = props.resume;
        setChainAt((c) => ({ ...c, tried: true }));
        const id = await finishCreation({
          kob, node, adapter: wallet.adapter, network: config.network, rec, maker, addressOf,
          ...(services.fees?.policy ? { feePolicy: services.fees.policy } : {}),
          onStage: (s, info) => {
            if (!alive.current) return;
            setStage(s);
            setChainAt((c) => ({ ...c, step: 'genesis', prepSent: true, waiting: !!info?.waiting }));
            if (s === 'signing') setChainNote(info?.waiting ? 'wait' : 'genesis');
          },
        });
        setChainNote(null);
        await afterAccepted(id);
      } else if (props.creation) {
        const c = props.creation;
        const twice = !wallet.adapter.signChain;
        setChainAt({ step: null, waiting: false, prepSent: false, tried: true });
        const res = await signAndSubmitCreation({
          kob, node, adapter: wallet.adapter, prep: c.prep, built, network: config.network, maker, addressOf,
          onStage: (s, info) => {
            if (!alive.current) return;
            setStage(s);
            setChainAt((c) => ({ ...c, step: info?.step ?? c.step, waiting: !!info?.waiting }));
            if (!twice || s !== 'signing') return;
            setChainNote(info?.step === 'genesis' ? (info.waiting ? 'wait' : 'genesis') : 'prep');
          },
          record: { token: props.issue.token, declaration: c.declaration, docs: c.docs, ...(props.issue.curve ? { curve: props.issue.curve } : {}), ...(props.issue.askSale ? { askSale: props.issue.askSale } : {}) },
          // the preparation spends the wallet's coins even if the genesis then fails: no plan of this page may pick them meanwhile
          onPrepSent: (prep) => {
            if (alive.current) setChainAt((c) => ({ ...c, prepSent: true }));
            pendingSpends.add(prep.tx.inputs);
            if (wallet.info) pendingSpends.addChange(prep.tx, prep.tx.id, wallet.info.pubkey);
          },
        });
        setChainNote(null);
        await afterAccepted(res.txid);
      } else {
        const res = await signAndSubmit({ kob, node, adapter: wallet.adapter, built, network: config.network, onStage, addressOf });
        await afterAccepted(res.txid);
      }
    } catch (e) {
      if (!alive.current) return;
      setChainNote(null);
      // the preparation is on chain but the token is not: never signed again here, the Create token page finishes it
      if (e instanceof SignFlowError && e.code === 'genesis-pending') {
        const declined = e.cause instanceof SignFlowError && e.cause.rejectedByUser;
        finish({ status: 'pending', message: e.message, ...(declined ? { declined: true } : {}) });
        return;
      }
      if (props.resume && e instanceof SignFlowError && (e.code === CREATION_SPENT || e.code === CREATION_INVALID)) {
        finish({ status: 'failed', message: e.message, code: e.code });
        return;
      }
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

        {model.brief ? <Brief rows={model.brief} /> : null}

        {/* the risk notes: ONE line that stays in view before signing (a safety disclosure), the full text behind its "?" */}
        {model.warnings.length > 0 ? (
          <div class="cf-line cf-risk" data-testid="confirm-risk" data-codes={model.warnings.map((w) => w.code).join(' ')}>
            <span aria-hidden="true">{'\u26A0\uFE0E'}</span>
            <span>
              {riskWords()}
            </span>
            <span class="cf-sep" aria-hidden="true">·</span>
            <HelpTip label={t('confirm.risk.label')} data-testid="confirm-risk-help">
              <FindingList items={model.warnings} testid="confirm-warnings" class="cf-help-list" />
            </HelpTip>
          </div>
        ) : null}

        {/* what the wallet asks and shows: one line, the full notes (and what this wallet's popup shows) behind its "?" */}
        {walletInfo.length > 0 || model.notices.length > 0 ? (
          <div class="cf-line cf-wallet-line" data-testid="confirm-wallet-line" data-codes={walletInfo.map((f) => f.code).join(' ')}>
            <span>{t(walletInfo.some((f) => f.code === 'walletTwice') ? 'confirm.walletLine.twice' : walletInfo.some((f) => f.code === 'resumed') ? 'confirm.walletLine.resumed' : 'confirm.walletLine.blind')}</span>
            <span class="cf-sep" aria-hidden="true">·</span>
            <HelpTip label={t('confirm.walletLine.label')} data-testid="confirm-wallet-help">
              <FindingList items={walletInfo} testid="confirm-info" class="cf-help-list" />
              {model.notices.length > 0 ? (
                <div class="cf-wallet-notes" data-testid="confirm-wallet-notice">
                  <h4>{t('confirm.wallet.title', { wallet: wallet.info?.label ?? t('confirm.wallet.yours') })}</h4>
                  <p>{t(built.sign.length === 1 ? 'confirm.wallet.lead.one' : 'confirm.wallet.lead.other', { count: built.sign.length })}</p>
                  <ul>
                    {model.notices.map((n) => (
                      <li key={n.code} data-code={n.code}>
                        {walletNoticeText(n)}
                      </li>
                    ))}
                  </ul>
                </div>
              ) : null}
            </HelpTip>
          </div>
        ) : null}

        {/* every verified section in full: the token, the curve, what is recorded on chain, the net effect, the KAS locked, the fee, the wallet's flows */}
        <details class="cf-more" data-testid="confirm-details">
          <summary>{t('confirm.details.title')}</summary>
          <div class="cf-summary" data-testid="confirm-summary">
            <p class="cf-note">{model.intro}</p>
            {model.sections.filter((s) => !s.fold).map((s) => (
              <SectionView section={s} key={s.id} />
            ))}
            {/* the gross flows (the wallet's inputs and change) behind the net effect */}
            {model.sections.some((s) => s.fold) ? (
              <details class="cf-flows" data-testid="confirm-flows">
                <summary>{t('confirm.flows.title')}</summary>
                {model.sections.filter((s) => s.fold).map((s) => (
                  <SectionView section={s} key={s.id} />
                ))}
              </details>
            ) : null}
            {otherInfo.length > 0 ? <FindingList items={otherInfo} testid="confirm-other-info" class="cf-list-info" /> : null}
          </div>
        </details>

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

        {props.creation && (busy || phase === 'submitted' || (failure && chainAt.tried)) ? (
          <Stepper
            data-testid="create-stepper"
            label={t('stepper.label')}
            steps={creationStepperItems({
              stage: phase === 'submitted' ? 'submitted' : stage,
              step: chainAt.step,
              waiting: chainAt.waiting,
              prepSent: chainAt.prepSent,
              oneApproval: !!wallet.adapter?.signChain && !props.resume,
              stopped: failure && !busy ? (failure.kind === 'rejected' ? 'declined' : 'failed') : null,
            })}
          />
        ) : null}

        {busy && chainNote ? (
          <p class="cf-chain" data-testid="confirm-chain-step" data-step={chainNote} aria-live="polite">
            {t(`confirm.chain.${chainNote}`)}
          </p>
        ) : null}

        {!props.creation && (busy || phase === 'submitted') ? (
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
              {clock ? <p class="cf-note">{t('confirm.submittedAt', { time: `${rowTimeTitleUnix(clock.unixSeconds)} (${formatUtc(clock.unixSeconds)})` })}</p> : null}
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
