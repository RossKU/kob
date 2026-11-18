// Token issuance (`kob token create` in the browser): a fixed-supply KCC-20 token on the KCC-20 reference program, by default the published
// public-mint build `KCC20PublicMint` (3 / 3, no minter) or, chosen under Advanced, the standalone 3 / 3 build `KCC20Ref`. Form -> live validation ->
// debounced `planIssue` -> summary -> the issuance pre-sign screen (ConfirmSign: the KCC-23 preparation and the genesis, one confirmation) ->
// tracker + result panel with the registry entry. A creation whose genesis did not go out (data/creations.ts) is offered again at the top:
// sent as it is when its genesis is signed, finished with one more wallet approval when it is not (the second approval was declined, or the
// page closed between the two: wallet/create.ts `finishCreation`). The record is untrusted: before the wallet is asked, the creation is planned
// again from it and checked as a fresh one, and shown on the same confirmation screen (`checkSavedCreation`).
// All protocol logic is in kob/issue.ts (kob-wasm); this view only collects strings and renders findings.
import { useEffect, useMemo, useState } from 'preact/hooks';

/** The app's phone breakpoint (market.css: max-width 759px), where the gate shows the one Connect of the header. */
const PHONE_QUERY = '(max-width: 759px)';
function usePhoneWidth(): boolean {
  const q = () => typeof matchMedia === 'function' && matchMedia(PHONE_QUERY).matches;
  const [phone, setPhone] = useState(q);
  useEffect(() => {
    if (typeof matchMedia !== 'function') return;
    const m = matchMedia(PHONE_QUERY);
    const on = () => setPhone(m.matches);
    m.addEventListener('change', on);
    on();
    return () => m.removeEventListener('change', on);
  }, []);
  return phone;
}
import { readFees } from '../../app/env';
import { rememberIssuedToken } from '../../data/issued-decimals';
import { adoptDeclaration } from '../../data/token-declarations';
import { useServices, useWallet } from '../../app/context';
import { t } from '../../i18n';
import { failureText } from '../notify/failure-toast';
import { issueText } from '../../i18n/issue-text';
import { issueLimits, issuedTokenUtxos, validateIssueForm, type IssueLimits, type IssuePlan, type IssueProgramChoice, type IssueResult } from '../../kob/issue';
import { creationIsOld, genesisSigned, listCreations, removeCreation, type CreationRecord } from '../../data/creations';
import { CREATION_INVALID, CREATION_SPENT, checkSavedCreation, finishCreation } from '../../wallet/create';
import { replanCreation } from '../../wallet/creation-replan';
import { SignFlowError } from '../../wallet/sign';
import { pendingSpends } from '../../app/pending-spends';
import { inputAddressOf, recordAccepted } from '../confirm/send';
import { expectedOutputs } from '../../wallet/sign';
import type { BuiltTx, Hex } from '../../kob/types';
import type { PlanIssue } from '../../kob/plan-types';
import type { CurveShape } from '../../kob/curve';
import { isKeyOwned } from '../../kob/token-state';
import type { Services } from '../../app/services';
import { ConnectBar } from '../shell/WalletButton';
import { visibilityCheckSupported } from '../../signer/seen';
import { privateKeySigningAllowed } from '../../app/mirror';
import { ConfirmSign, type ConfirmResult } from '../confirm/ConfirmSign';
import { pendingCreationItems } from '../confirm/creation-steps';
import { Banner, Button, CurveGraph, EmptyState, ErrorBanner, Field, FieldShell, HelpTip, KeyValueList, RawDetails, Section, Segmented, Spinner, Stepper, friendlyErrorText, kasText, shortId, showToast, useAsync } from '../kit';
import { formatUnits as baseUnitsText } from '../../kob/issue';
import { formatRowDate } from '../kit/format';
import {
  defaultCarrierText, emptyUiState, filterDecimals, filterOwner, filterTicker, formToIssueForm, groupByField, modelIssues, parseDecimalsText, remainingText,
  humanAmount, linkedSaleFields, saleOfRecord, reviewSummary, salePreview, shortfallOf, supplyPreview, walletRowIncluded, type IssueUiState, type SaleUi,
  LOCK_DAYS, chooseLock, lockUntilOf, type LockChoice,
} from './issue-model';
import { hashImageFile } from './image-file';
import { ResultPanel } from './ResultPanel';
import { useIssuePlan } from './use-issue-plan';
import './issue.css';

/** `resume`: the confirmation finishes this saved creation (`plan` is it, planned again) */
type Phase =
  | { kind: 'form' }
  | { kind: 'confirm'; plan: IssueResult & Pick<IssuePlan, 'sale'>; resume?: CreationRecord }
  | { kind: 'result'; plan: Pick<IssuePlan, 'token' | 'docs' | 'curve' | 'sale' | 'lock'>; txid: string; trackFailed: boolean };
/** `finish`: the covenant id of the creation the notice offers to finish (the same action as its banner) */
type Notice = { tone: 'info' | 'error'; text: string; finish?: Hex } | null;

/** Remembers the token outputs of the genesis in the tracker (a genesis has no token leader, so `trackFromBuilt` does not cover it). */
function trackIssued(services: Services, plan: Pick<IssueResult, 'built' | 'token'>): boolean {
  let ok = true;
  for (const u of issuedTokenUtxos(plan)) {
    // only plain key-owned outputs can be tracked: custody / hash-owned holders are the receiver's business
    if (!isKeyOwned(u.state)) continue;
    try {
      const added = services.tracker.add(u.state.owner, {
        transactionId: u.transactionId,
        index: u.index,
        tokenCovId: plan.token.covenantId,
        program: plan.token.program,
        state: u.state,
        carrier: u.amount,
      });
      const known = services.tracker.list(u.state.owner).some((i) => i.transactionId === u.transactionId && i.index === u.index);
      if (!added && !known) ok = false;
    } catch {
      ok = false;
    }
  }
  return ok;
}

export function IssueView() {
  const services = useServices();
  const wallet = useWallet();
  const phoneWidth = usePhoneWidth();
  const { kob, config } = services;

  const [state, setState] = useState<IssueUiState>(emptyUiState);
  const [touched, setTouched] = useState<ReadonlySet<string>>(new Set());
  const [phase, setPhase] = useState<Phase>({ kind: 'form' });
  const [notice, setNotice] = useState<Notice>(null);
  // a failed creation, or one whose token step was not sent (its details are on chain): also a red toast at the top right
  useEffect(() => {
    if (notice && (notice.tone === 'error' || notice.finish)) showToast(failureText(t(notice.tone === 'error' ? 'issue.failed.title' : 'issue.toast.notFinished'), notice.text), 'error');
  }, [notice]);
  const [retry, setRetry] = useState(0);
  const [fileError, setFileError] = useState<string | null>(null);
  // a small preview of the picked picture (an object URL of the file the browser already holds; nothing is uploaded), released when it is replaced or the view goes away
  const [preview, setPreview] = useState<string | null>(null);
  useEffect(() => () => void (preview && URL.revokeObjectURL(preview)), [preview]);

  const shownPreview = state.imageFile !== '' ? preview : null;
  const set = (patch: Partial<IssueUiState>) => setState((s) => ({ ...s, ...patch }));
  const touch = (f: string) => setTouched((s) => (s.has(f) ? s : new Set(s).add(f)));

  // the picture file the creator picked: the browser hashes its bytes (nothing is uploaded); an unusable file leaves the hash as it was
  const pickImage = async (input: HTMLInputElement) => {
    const file = input.files?.[0];
    if (!file) return;
    const r = await hashImageFile(file);
    input.value = '';
    if (r.ok) {
      setFileError(null);
      set({ imageHash: r.hash, imageFile: file.name });
      setPreview(URL.createObjectURL(file));
    } else {
      setFileError(t(`issue.imageFile.err.${r.reason}`));
    }
  };

  // ---- limits (older wasm bindings do not export issue: say so instead of crashing)
  const limitsResult = useMemo<{ limits: IssueLimits | null; error: string | null }>(() => {
    try {
      return { limits: issueLimits(kob), error: null };
    } catch (e) {
      return { limits: null, error: e instanceof Error ? e.message : String(e) };
    }
  }, [kob]);
  const limits = limitsResult.limits;

  // the one plain line on what the program can do, from its registry template's powers (the creation makes no minter: a public-mint program's
  // mint power has no holder); nothing when the registry does not describe the program
  // (`powers`: an authority holds powers over the tokens, a warning that stays on the page; the plain fixed-supply line is in the title's help)
  const fixedLine = useMemo<{ text: string; powers: boolean } | null>(() => {
    if (!limits) return null;
    const id = state.program === '3x3' ? limits.registryTemplateId : limits.publicMint.registryTemplateId;
    const tpl = services.registry.templates.find((x) => x.id === id);
    if (!tpl) return null;
    const powers = (tpl.capabilities ?? []).filter((c) => c !== 'public-mint');
    return powers.length === 0 ? { text: t('issue.fixedLine'), powers: false } : { text: t('issue.powersLine', { powers: powers.join(', ') }), powers: true };
  }, [limits, state.program, services.registry]);

  const pubkey = wallet.info?.pubkey ?? null;
  const usable = pubkey !== null && !wallet.networkMismatch;
  const registryTickers = useMemo(() => services.registry.tokens.map((x) => x.ticker), [services.registry]);

  // ---- creations of this wallet whose genesis did not go out (read below)
  const [pending, setPending] = useState<CreationRecord[]>([]);

  // ---- funding
  const funding = useAsync(async () => (usable && pubkey ? services.utxos.fundingFor(pubkey) : null), [pubkey, usable, retry]);
  // the preparation output of a creation left unfinished stays reserved for its genesis (sent again from the banner below), never new funding
  const reserved = useMemo(() => new Set(pubkey ? listCreations(config.network, pubkey).map((r) => `${r.prepTxid}:0`) : []), [pubkey, config.network, retry, phase.kind]);
  // and a coin a transaction of this page just spent is not funding either (the node lists it until a block holds that transaction)
  const fundingList = useMemo(
    () => (usable && pubkey ? (funding.data ? pendingSpends.without(funding.data).filter((u) => !reserved.has(`${u.transactionId}:${u.index}`)) : null) : null),
    [usable, pubkey, funding.data, reserved],
  );
  const spendable = fundingList ? fundingList.reduce((s, u) => s + BigInt(u.amount), 0n) : null;

  // ---- form -> findings
  const form = useMemo(() => {
    const f = formToIssueForm(state, pubkey);
    // a blank one price is the default shown in its field (10,000 KAS over the amount)
    if (f.sale?.mode === 'ask' && f.sale.price.trim() === '') {
      const p = salePreview(kob, state)?.defaultPrice;
      if (p) return { ...f, sale: { ...f.sale, price: baseUnitsText(p, 8) } };
    }
    return f;
  }, [state, pubkey, kob]);
  const walletRow = walletRowIncluded(state);
  const decimals = parseDecimalsText(state.decimals);
  const findings = useMemo<PlanIssue[]>(() => (limits ? validateIssueForm(form, limits, registryTickers) : []), [form, limits, registryTickers]);
  const own = useMemo(() => modelIssues(state, pubkey, 0), [state, pubkey]);
  // "After creation": the split, and a curve's table, graph and numbers, while the form is typed
  const sale = useMemo(() => salePreview(kob, state), [kob, state]);
  const saleBlocked = !!sale && (sale.issues.length > 0 || sale.curveError !== null || (state.sale.mode !== 'keep' && !sale.parsed));
  const blocked = findings.some((i) => i.severity === 'error') || own.some((i) => i.severity === 'error') || saleBlocked;

  // ---- fee policy: the node's estimate (a cached read that never fails: no estimate = the floor)
  const fees = useAsync(async () => readFees(services), [retry]);

  const plan = useIssuePlan({
    kob,
    form,
    maker: pubkey,
    network: config.network,
    funding: fundingList,
    fees: fees.data ?? null,
    registryTickers,
    // a creation left unfinished comes first: no new one is planned beside it (its coin is reserved, and two "Review and sign" would confuse)
    enabled: limits !== null && !blocked && usable && phase.kind === 'form' && pending.length === 0,
    retry,
  });

  // ---- rendering helpers
  const ctx = { tokenDecimals: Number.isNaN(decimals) ? undefined : decimals, tokenTicker: form.ticker || undefined };
  const say = (i: PlanIssue) => issueText(i, ctx);
  const byField = groupByField(findings, walletRow);
  const visible = (field: string, value: string) => touched.has(field) || value !== '';
  const errorOf = (field: string, value: string): string | null => {
    if (!visible(field, value)) return null;
    const errs = (byField.get(field) ?? []).filter((i) => i.severity === 'error');
    return errs.length ? errs.map(say).join(' ') : null;
  };
  const general = [...(byField.get('') ?? []), ...(byField.get('holders') ?? [])];
  const planIssues: PlanIssue[] = plan.status === 'invalid' ? plan.issues : [];
  const shortfall = planIssues.map(shortfallOf).find((x) => x !== null) ?? null;

  const summary = plan.status === 'ready' ? reviewSummary(plan.plan, pubkey) : null;
  const canReview = plan.status === 'ready' && phase.kind === 'form' && pending.length === 0;

  // ---- creations of this wallet whose genesis did not go out: offered again (a genesis already on chain finishes them silently)
  const [resuming, setResuming] = useState<string | null>(null);
  const [resumeError, setResumeError] = useState<{ id: string; text: string } | null>(null);
  // a creation that failed, or stopped part way, is said at the top right in red too (founder 2026-11-17); the notice and the banner keep the details
  useEffect(() => {
    if (resumeError) showToast(failureText(t('issue.failed.title'), friendlyErrorText(resumeError.text)), 'error');
  }, [resumeError]);
  /** what the wallet is asked for while a creation is finished (confirm.chain.*) */
  const [resumeNote, setResumeNote] = useState<'wait' | 'genesis' | null>(null);
  /** creations that cannot be finished: the first step's coin was spent elsewhere, or the record does not check out; only forgetting is left */
  const [stopped, setStopped] = useState<ReadonlyMap<string, 'spent' | 'invalid'>>(new Map());
  const stop = (id: string, code: string | undefined) => {
    const why = code === CREATION_SPENT ? 'spent' : code === CREATION_INVALID ? 'invalid' : null;
    if (why) setStopped((xs) => new Map(xs).set(id, why));
  };
  const genesisTx = (rec: CreationRecord) => rec.genesis ?? rec.genesisBuilt!.tx;
  const completed = (rec: CreationRecord, txid: string, show: boolean) => {
    const plan = { built: { tx: genesisTx(rec) } as BuiltTx, token: rec.token };
    const trackFailed = !trackIssued(services, plan);
    rememberIssuedToken(config.network, rec.token.covenantId, { decimals: rec.token.decimals, ticker: rec.token.ticker, name: rec.token.name });
    adoptDeclaration(kob, config.network, rec.token.covenantId, rec.docs.launchProof);
    removeCreation(config.network, rec.covenantId);
    setPending((xs) => xs.filter((x) => x.covenantId !== rec.covenantId));
    // the curve the genesis placed: its placement record (as an accepted creation saves it), and the result's line
    if (rec.curve) void recordAccepted(services, wallet, { tx: genesisTx(rec), sign: [] } as unknown as BuiltTx, txid).catch(() => undefined);
    const sale = saleOfRecord(rec);
    if (show) setPhase({ kind: 'result', plan: { token: rec.token, docs: rec.docs, ...(rec.curve ? { curve: rec.curve } : {}), ...(sale ? { sale } : {}) }, txid, trackFailed });
  };
  useEffect(() => {
    if (!usable || !pubkey) {
      setPending([]);
      return;
    }
    const recs = listCreations(config.network, pubkey);
    setPending(recs);
    let alive = true;
    // a record whose genesis is already on chain (the page died after sending it) is finished, not offered; only one that plans again exactly
    void (async () => {
      for (const rec of recs) {
        try {
          if (!replanCreation(kob, rec, pubkey).ok) continue;
          const outs = expectedOutputs(services.sdk, config.network, genesisTx(rec), [0]);
          const found = await services.node.getUtxosByAddresses([outs[0]!.address]);
          if (alive && found.some((u) => u.transactionId === rec.genesisTxid && u.index === 0)) completed(rec, rec.genesisTxid, false);
        } catch {
          /* offered as it is */
        }
      }
    })();
    return () => {
      alive = false;
    };
  }, [pubkey, usable, config.network, retry]);
  const resume = async (rec: CreationRecord) => {
    setResuming(rec.covenantId);
    setResumeError(null);
    try {
      if (!pubkey || (!genesisSigned(rec) && !wallet.adapter)) throw new Error(t('issue.pending.needWallet'));
      if (!genesisSigned(rec)) {
        // the wallet signs: planned again, checked, then the confirmation screen (it runs `finishCreation`)
        setPhase({ kind: 'confirm', plan: checkSavedCreation(kob, rec, pubkey, services.fees?.policy), resume: rec });
        return;
      }
      const txid = await finishCreation({
        kob, node: services.node, adapter: wallet.adapter!, network: config.network, rec, maker: pubkey, addressOf: inputAddressOf(services),
        ...(services.fees?.policy ? { feePolicy: services.fees.policy } : {}),
        onStage: (s, info) => setResumeNote(s === 'signing' ? (info?.waiting ? 'wait' : 'genesis') : null),
      });
      completed(rec, txid, true);
    } catch (e) {
      if (e instanceof SignFlowError) stop(rec.covenantId, e.code);
      setResumeError({ id: rec.covenantId, text: e instanceof Error ? e.message : String(e) });
    } finally {
      setResuming(null);
      setResumeNote(null);
      // the attempt may have sent the first step: the record says so, and its stepper follows
      if (pubkey) setPending(listCreations(config.network, pubkey));
    }
  };
  const discard = (rec: CreationRecord) => {
    removeCreation(config.network, rec.covenantId);
    setPending((xs) => xs.filter((x) => x.covenantId !== rec.covenantId));
  };

  const onClose = (r: ConfirmResult) => {
    const current = phase.kind === 'confirm' ? phase.plan : null;
    if (!current) return;
    const resumed = phase.kind === 'confirm' ? phase.resume : undefined;
    if (resumed && r.status === 'submitted') return completed(resumed, r.txid, true);
    if (resumed && r.status === 'failed') {
      // spent elsewhere, or the record no longer checks out: said on its banner
      stop(resumed.covenantId, r.code);
      setResumeError({ id: resumed.covenantId, text: r.message });
      setPhase({ kind: 'form' });
      setRetry((n) => n + 1);
      return;
    }
    if (r.status === 'submitted') {
      const trackFailed = !trackIssued(services, current);
      // the issuer's own decimals, ticker and name: they prefill (never set) the decimals of the token's first order in this browser, and name the
      // token, marked unverified, wherever the registry does not (app/token-display.ts)
      rememberIssuedToken(config.network, current.token.covenantId, { decimals: current.token.decimals, ticker: current.token.ticker, name: current.token.name });
      // the token's facts as it recorded them on chain: shown at once, without waiting for an indexer (verified like any proof)
      adoptDeclaration(kob, config.network, current.token.covenantId, current.docs.launchProof);
      setNotice(null);
      setPhase({ kind: 'result', plan: current, txid: r.txid, trackFailed });
      return;
    }
    setPhase({ kind: 'form' });
    setRetry((n) => n + 1); // the spent UTXOs may have changed: read the funding again (and the creations left unfinished)
    // `pending`: the first step is on chain, the token is not: its record is offered below ("Finish the creation")
    // the creation to finish is offered at the top: brought into view
    if (r.status === 'pending') setTimeout(() => document.querySelector('[data-testid="issue-pending"]')?.scrollIntoView({ block: 'center' }), 0);
    setNotice(
      r.status === 'failed'
        ? { tone: 'error', text: r.message }
        : r.status === 'pending'
          ? { tone: 'info', text: t(r.declined ? 'issue.pending.declined' : 'issue.pending.continued'), finish: current.token.covenantId }
          : { tone: 'info', text: t('issue.cancelled') },
    );
  };

  const setSale = (patch: Partial<SaleUi>) => setState((s) => ({ ...s, sale: { ...s.sale, ...patch } }));
  const setLock = (choice: LockChoice) => setState((s) => ({ ...s, lock: chooseLock(choice, s.lock) }));

  const another = () => {
    setState(emptyUiState());
    setPreview(null);
    setTouched(new Set());
    setNotice(null);
    setPhase({ kind: 'form' });
    setRetry((n) => n + 1);
  };

  // ---- result
  if (phase.kind === 'result') {
    return (
      <div class="issue-page stack" data-testid="issue-view">
        <h1>{t('issue.title')}</h1>
        <ResultPanel plan={phase.plan} txid={phase.txid} walletKey={pubkey} trackFailed={phase.trackFailed} onAnother={another} />
      </div>
    );
  }

  return (
    <div class="issue-page stack" data-testid="issue-view">
      <div>
        {/* one line under the title; the full introduction and the fixed-supply line are behind its "?" */}
        <div class="help-title">
          <h1>{t('issue.title')}</h1>
          <HelpTip label={t('issue.title')} data-testid="issue-intro-help">
            <p data-testid="issue-intro-long">{t('issue.intro', { approvals: wallet.adapter ? t(wallet.adapter.signChain ? 'issue.intro.one' : 'issue.intro.two') : '' })}</p>
            {fixedLine && !fixedLine.powers ? <p data-testid="issue-fixed-line">{fixedLine.text}</p> : null}
          </HelpTip>
        </div>
        <p class="muted" data-testid="issue-intro">{t('issue.intro.short')}</p>
      </div>

      {fixedLine?.powers ? <p class="issue-fixed-line" data-testid="issue-fixed-line">{fixedLine.text}</p> : null}

      {limitsResult.error ? <ErrorBanner error={limitsResult.error} data-testid="issue-limits-error" /> : null}

      {renderWalletGate()}

      {phase.kind === 'form' ? pending.map((rec) => (
        <Banner
          key={rec.covenantId}
          tone="warn"
          title={t('issue.pending.title', { ticker: rec.token.ticker })}
          data-testid="issue-pending"
          actions={
            <div class="row">
              {stopped.has(rec.covenantId) ? null : (
                <Button variant="primary" small loading={resuming === rec.covenantId} disabled={resuming !== null} onClick={() => void resume(rec)} data-testid="issue-pending-send">
                  {t(genesisSigned(rec) ? 'issue.pending.send' : 'issue.pending.finish')}
                </Button>
              )}
              <Button small disabled={resuming !== null} onClick={() => discard(rec)} data-testid="issue-pending-discard">{t('issue.pending.discard')}</Button>
            </div>
          }
        >
          {stopped.has(rec.covenantId) ? (
            stopped.get(rec.covenantId) === 'spent' ? <p data-testid="issue-pending-spent">{t('issue.pending.spent')}</p> : <p data-testid="issue-pending-invalid">{t('issue.pending.invalid')}</p>
          ) : (
            <>
              <p data-testid="issue-pending-body">
                {genesisSigned(rec)
                  ? t('issue.pending.body', { ticker: rec.token.ticker, sent: rec.prepSent ? t('issue.pending.sent') : '' })
                  : t(rec.prepSent ? 'issue.pending.bodyUnsigned' : 'issue.pending.bodyUnsent', { ticker: rec.token.ticker })}
              </p>
              <Stepper
                data-testid="issue-pending-stepper"
                label={t('stepper.label')}
                steps={pendingCreationItems({
                  prepSent: rec.prepSent,
                  signed: genesisSigned(rec),
                  resuming: resuming === rec.covenantId,
                  note: resuming === rec.covenantId ? resumeNote : null,
                  failed: resumeError?.id === rec.covenantId,
                })}
              />
              <p class={creationIsOld(rec) ? 'field-error' : 'muted'} data-testid="issue-pending-window" data-old={creationIsOld(rec) ? 'true' : 'false'}>
                {t(creationIsOld(rec) ? 'issue.pending.old' : 'issue.pending.window')}
              </p>
              {resuming === rec.covenantId && resumeNote ? <p class="muted" data-testid="issue-pending-step" data-step={resumeNote}>{t(`confirm.chain.${resumeNote}`)}</p> : null}
            </>
          )}
          {resumeError?.id === rec.covenantId ? (
            <div class="field-error wrap-anywhere" data-testid="issue-pending-error">
              {/* an invalid record is said above: only the reason here */}
              {stopped.get(rec.covenantId) === 'invalid' ? null : t('issue.pending.failed', { reason: friendlyErrorText(resumeError.text) })}
              <RawDetails text={resumeError.text} />
            </div>
          ) : null}
        </Banner>
      )) : null}

      <div class="stack" data-testid="issue-form">
        <Section title={t('issue.section.token')} data-testid="issue-basic">
          <div class="issue-grid">
            <Field
              label={t('issue.field.name')}
              help={<HelpTip label={t('issue.field.name')} data-testid="issue-name-help">{t('issue.field.name.hint')}</HelpTip>}
              value={state.name}
              onValue={(v) => set({ name: v })}
              onBlur={() => touch('name')}
              error={errorOf('name', state.name)}
              data-testid="issue-name"
              autoComplete="off"
            />
            <Field
              label={t('issue.field.ticker')}
              help={limits ? <HelpTip label={t('issue.field.ticker')} data-testid="issue-ticker-help">{t('issue.field.ticker.hint', { min: limits.ticker.minLength, max: limits.ticker.maxLength })}</HelpTip> : undefined}
              value={state.ticker}
              onValue={(v) => set({ ticker: filterTicker(v) })}
              onBlur={() => touch('ticker')}
              error={errorOf('ticker', state.ticker)}
              data-testid="issue-ticker"
              autoComplete="off"
              class="issue-mono"
            />
            {/* a ticker another registry token uses (or one like it) is a warning, never a block: the covenant id is the identity */}
            {visible('ticker', state.ticker)
              ? (byField.get('ticker') ?? [])
                  .filter((i) => i.severity === 'warning')
                  .map((i) => (
                    <Banner key={i.code} tone="warn" data-testid={`issue-issue-${i.code}`}>
                      {say(i)}
                    </Banner>
                  ))
              : null}
            {renderSupply()}
            <Field
              label={t('issue.field.decimals')}
              help={limits ? <HelpTip label={t('issue.field.decimals')} data-testid="issue-decimals-help">{t('issue.field.decimals.hint', { max: limits.maxDecimals })}</HelpTip> : undefined}
              value={state.decimals}
              onValue={(v) => set({ decimals: filterDecimals(v) })}
              onBlur={() => touch('decimals')}
              error={errorOf('decimals', state.decimals)}
              inputMode="numeric"
              data-testid="issue-decimals"
            />
          </div>
          <div class="issue-grid issue-details">
            <Field
              label={t('issue.field.icon')}
              help={<HelpTip label={t('issue.field.icon')} data-testid="issue-icon-help">{t('issue.field.icon.hint')}</HelpTip>}
              value={state.icon}
              onValue={(v) => set({ icon: v })}
              onBlur={() => touch('icon')}
              error={errorOf('icon', state.icon)}
              inputMode="url"
              data-testid="issue-icon"
            />
            <FieldShell
              label={t('issue.field.imageFile')}
              help={<HelpTip label={t('issue.field.imageFile')} data-testid="issue-image-file-help">{t('issue.field.imageHash.hint')}</HelpTip>}
              error={fileError ?? errorOf('imageHash', state.imageHash)}
            >
              {({ id, describedBy, invalid }) => (
                <>
                  {/* the native "Choose File | No file chosen" control is hidden (visually only: it keeps its focus, its label and its keyboard use) and a button with the
                      file name and a small preview stands in for it; the button shows the focus ring while the hidden input has focus */}
                  <div class="issue-file">
                    <input
                      id={id}
                      class="issue-file-input"
                      type="file"
                      accept="image/png,image/jpeg,image/webp,image/gif,image/avif,image/svg+xml,.svgz,image/heic,image/jxl,image/bmp,image/x-icon,image/tiff"
                      aria-describedby={describedBy}
                      aria-invalid={invalid ? 'true' : undefined}
                      data-testid="issue-image-file"
                      onChange={(e) => void pickImage(e.currentTarget as HTMLInputElement)}
                    />
                    <label for={id} class="btn issue-file-button" data-testid="issue-image-file-button">
                      {shownPreview ? <img class="issue-file-preview" src={shownPreview} alt="" width="32" height="32" data-testid="issue-image-preview" /> : null}
                      <span class="btn-label">{t(state.imageFile !== '' ? 'issue.field.imageFile.change' : 'issue.field.imageFile.pick')}</span>
                    </label>
                    <span class="small muted wrap-anywhere" data-testid="issue-image-file-name">{state.imageFile !== '' ? state.imageFile : t('issue.field.imageFile.none')}</span>
                  </div>
                  {state.imageHash !== '' ? (
                    <div class="small muted" data-testid="issue-image-hash-note">
                      {t('issue.imageHash.set', { hash: state.imageHash.slice(0, 16), file: state.imageFile })}{' '}
                      <Button
                        small
                        variant="ghost"
                        onClick={() => {
                          setFileError(null);
                          setPreview(null);
                          set({ imageHash: '', imageFile: '' });
                        }}
                        data-testid="issue-image-hash-clear"
                      >
                        {t('issue.imageHash.clear')}
                      </Button>
                    </div>
                  ) : null}
                </>
              )}
            </FieldShell>
            <details class="small" data-testid="issue-image-hash-advanced">
              <summary>{t('issue.imageHash.advanced')}</summary>
              <Field
                label={t('issue.field.imageHash')}
                hint={t('issue.field.imageHash.advancedHint')}
                value={state.imageHash}
                onValue={(v) => {
                  setFileError(null);
                  set({ imageHash: v.replace(/\s+/g, '').toLowerCase().slice(0, 64), imageFile: '' });
                }}
                onBlur={() => touch('imageHash')}
                autoComplete="off"
                class="issue-mono"
                data-testid="issue-image-hash"
              />
            </details>
            <Field
              as="textarea"
              label={t('issue.field.description')}
              value={state.description}
              onValue={(v) => set({ description: v })}
              onBlur={() => touch('description')}
              error={errorOf('description', state.description)}
              rows={2}
              data-testid="issue-description"
            />
            <Field
              label={t('issue.field.website')}
              hint={t('issue.field.website.hint')}
              value={state.website}
              onValue={(v) => set({ website: v })}
              onBlur={() => touch('website')}
              error={errorOf('website', state.website)}
              inputMode="url"
              data-testid="issue-website"
            />
            <Field
              label={t('issue.field.x')}
              hint={t('issue.field.x.hint')}
              value={state.x}
              onValue={(v) => set({ x: v.trim() })}
              onBlur={() => touch('x')}
              error={errorOf('x', state.x)}
              autoComplete="off"
              data-testid="issue-x"
            />
            <Field
              label={t('issue.field.telegram')}
              hint={t('issue.field.telegram.hint')}
              value={state.telegram}
              onValue={(v) => set({ telegram: v.trim() })}
              onBlur={() => touch('telegram')}
              error={errorOf('telegram', state.telegram)}
              autoComplete="off"
              data-testid="issue-telegram"
            />
          </div>
        </Section>

        {renderSale()}

        <Section title={t('issue.section.advanced')} collapsible defaultOpen={false} data-testid="issue-advanced">
          <div class="stack">
            {renderHolders()}
            <div class="stack-sm" data-testid="issue-carrier-section">
              <h3>{t('issue.section.carrier')}</h3>
              <Field
                label={t('issue.field.carrier')}
                help={
                  <HelpTip label={t('issue.field.carrier')} data-testid="issue-carrier-help">
                    <p>{t('issue.field.carrier.hint', { default: limits ? defaultCarrierText(limits) : '10' })}</p>
                    <p>{t('issue.fixed.carrier')}</p>
                    <p>{t('issue.fixed.custody')}</p>
                  </HelpTip>
                }
                value={state.carrier}
                placeholder={limits ? defaultCarrierText(limits) : ''}
                onValue={(v) => set({ carrier: v })}
                onBlur={() => touch('carrier')}
                error={errorOf('carrier', state.carrier)}
                inputMode="decimal"
                suffix={<span class="muted">KAS</span>}
                data-testid="issue-carrier"
              />
            </div>
            <div class="stack-sm" data-testid="issue-program-section">
              <div class="help-title">
                <h3>{t('issue.section.program')}</h3>
                <HelpTip label={t('issue.section.program')} data-testid="issue-program-help">
                  <p data-testid="issue-program-hint">{t(state.program === '3x3' ? 'issue.program.threeByThree.hint' : 'issue.program.publicMint.hint')}</p>
                  <p data-testid="issue-fixed-program">{t(state.program === '3x3' ? 'issue.fixed.program' : 'issue.fixed.programPublicMint')}</p>
                </HelpTip>
              </div>
              <Segmented<IssueProgramChoice>
                aria-label={t('issue.section.program')}
                value={state.program}
                onChange={(v) => set({ program: v })}
                options={[
                  { value: 'public-mint', label: t('issue.program.publicMint'), 'data-testid': 'issue-program-public-mint' },
                  { value: '3x3', label: t('issue.program.threeByThree'), 'data-testid': 'issue-program-3x3' },
                ]}
                data-testid="issue-program"
              />
            </div>
          </div>
        </Section>

        {general.length > 0 ? (
          <div class="stack-sm" data-testid="issue-issues">
            {general.map((i, n) => (
              <Banner key={`${i.code}-${n}`} tone={i.severity === 'error' ? 'error' : 'warn'} data-testid={`issue-issue-${i.code}`}>
                {say(i)}
              </Banner>
            ))}
          </div>
        ) : null}
      </div>

      {/* the spendable KAS: a small figure beside the Summary heading, next to what the creation costs */}
      <Section
        title={t('issue.summary.title')}
        actions={usable && spendable !== null && !funding.error ? <span class="small muted" data-testid="issue-funds-balance">{t('issue.funds.balance', { kas: kasText(spendable) })}</span> : undefined}
        data-testid="issue-plan-section"
      >
        {renderPlanArea()}
        {notice ? (
          <Banner tone={notice.tone === 'error' ? 'error' : 'info'} title={notice.tone === 'error' ? t('issue.failed.title') : undefined} actions={renderNoticeFinish()} data-testid="issue-notice">
            <span class="wrap-anywhere">{notice.text}</span>
          </Banner>
        ) : null}
        <div class="row issue-actions">
          <Button variant="primary" disabled={!canReview} onClick={() => plan.status === 'ready' && setPhase({ kind: 'confirm', plan: plan.plan })} data-testid="issue-review">
            {t('issue.review.button')}
          </Button>
          {!canReview ? <span class="muted" data-testid="issue-review-hint">{reviewHint()}</span> : null}
        </div>
      </Section>

      {phase.kind === 'confirm' ? (
        <ConfirmSign
          built={phase.plan.built}
          title={t('issue.confirm.title', { ticker: phase.plan.token.ticker })}
          label={t('issue.confirm.label', { ticker: phase.plan.token.ticker })}
          issue={{
            token: phase.plan.token,
            ...(phase.plan.curve ? { curve: phase.plan.curve } : {}),
            ...(phase.plan.sale?.mode === 'ask' ? { askSale: { sell: phase.plan.sale.sell.toString(), price: (phase.plan.sale.price ?? 0n).toString() } } : {}),
          }}
          creation={{ prep: phase.plan.prep, declaration: phase.plan.declaration, docs: phase.plan.docs }}
          {...(phase.resume ? { resume: phase.resume } : {})}
          onClose={onClose}
        />
      ) : null}
    </div>
  );

  // ---------------------------------------------------------------------------------------------- parts: plain render functions, not components (a component type created per render would remount its inputs and lose focus)

  /** The notice's own "Finish the creation": the same action as the banner of the creation left unfinished. */
  function renderNoticeFinish() {
    const rec = notice?.finish ? pending.find((x) => x.covenantId === notice.finish) : undefined;
    if (!rec || stopped.has(rec.covenantId)) return undefined;
    return (
      <Button variant="primary" small loading={resuming === rec.covenantId} disabled={resuming !== null} onClick={() => void resume(rec)} data-testid="issue-notice-finish">
        {t(genesisSigned(rec) ? 'issue.pending.send' : 'issue.pending.finish')}
      </Button>
    );
  }

  function reviewHint(): string {
    if (!wallet.info || wallet.networkMismatch) return t('issue.review.connect');
    if (pending.length > 0) return t('issue.review.unfinished');
    if (blocked) return t('issue.review.fix');
    if (plan.status === 'invalid') return shortfall ? t('issue.review.connect') : t('issue.review.fix');
    return t('issue.review.wait');
  }

  /** One line of the review: what happens after creation. */
  function saleSummaryText(p: IssuePlan): string {
    const s = p.sale!;
    const words = { sell: humanAmount(s.sell, p.token.decimals), ticker: p.token.ticker };
    if (s.mode === 'curve' && s.curve) {
      return t('issue.summary.saleCurve', { ...words, levels: s.curve.numbers.levels, first: kasText(s.curve.numbers.first), last: kasText(s.curve.numbers.last) });
    }
    return t('issue.summary.saleAsk', { ...words, price: kasText(s.price ?? 0n) });
  }

  /** "After creation": keep all, sell at one price, sell on a curve; a curve's fields, graph and numbers. */
  function renderSale() {
    const u = state.sale;
    const linked = linkedSaleFields(state);
    const ticker = form.ticker || t('issue.sale.tokens');
    const errOf = (field: string) => {
      const xs = (sale?.issues ?? []).filter((i) => i.field === `sale.${field}`);
      return xs.length ? xs.map(say).join(' ') : null;
    };
    const choice = (mode: SaleUi['mode']) => (
      <label class={`issue-after-option${u.mode === mode ? ' is-on' : ''}`} data-testid={`issue-after-${mode}`}>
        <input type="radio" name="issue-after" value={mode} checked={u.mode === mode} onChange={() => setSale({ mode })} />
        <span class="issue-after-text">
          <strong>{t(`issue.after.${mode}`)}</strong>
          <span class="small muted">{t(`issue.after.${mode}.hint`)}</span>
        </span>
      </label>
    );
    const n = sale?.curve?.numbers ?? null;
    const chromatic = u.shape === 'chromatic';
    return (
      <Section title={t('issue.section.after')} data-testid="issue-after">
        <div class="issue-after-options" role="radiogroup" aria-label={t('issue.section.after')}>
          {choice('keep')}
          {choice('ask')}
          {choice('curve')}
        </div>
        {u.mode === 'keep' ? null : (
          <div class="stack-sm issue-sale" data-testid="issue-sale">
            <div class="issue-grid">
              <Field
                label={t('issue.sale.keep')}
                value={linked.keepPct}
                onValue={(v) => setSale({ keepPct: v.trim(), lead: 'keep' })}
                error={errOf('keepPct')}
                inputMode="decimal"
                suffix={<span class="muted">%</span>}
                data-testid="sale-keep"
              />
              <Field
                label={t('issue.sale.amount')}
                value={linked.amount}
                onValue={(v) => setSale({ amount: v.trim(), lead: 'amount' })}
                error={errOf('amount')}
                inputMode="decimal"
                suffix={<span class="muted">{ticker}</span>}
                data-testid="sale-amount"
              />
            </div>
            {sale?.parsed ? (
              <p class="issue-sale-split" data-testid="sale-split">
                {sale.parsed.keep > 0n
                  ? t('issue.sale.split', { keep: sale.keepText, sell: sale.sellText, ticker })
                  : t('issue.sale.splitAll', { sell: sale.sellText, ticker })}
              </p>
            ) : null}
            {u.mode === 'ask' ? (
              <div class="issue-grid">
                <Field
                  label={t('issue.sale.price')}
                  value={u.price}
                  placeholder={sale?.defaultPrice ? kasText(sale.defaultPrice) : ''}
                  onValue={(v) => setSale({ price: v.trim() })}
                  error={errOf('price')}
                  inputMode="decimal"
                  suffix={<span class="muted">KAS</span>}
                  data-testid="sale-price"
                />
                <div class="issue-sale-total" data-testid="sale-total-ask">
                  <span class="small muted">{t('issue.sale.total')}</span>
                  <strong>{sale?.parsed ? `${kasText(sale.parsed.total)} KAS` : '-'}</strong>
                </div>
              </div>
            ) : (
              <>
                <div class="issue-lock-row" data-testid="sale-shape-row">
                  <span class="help-title">
                    <strong>{t('issue.sale.shape')}</strong>
                    <HelpTip label={t('issue.sale.shape')} data-testid="sale-shape-help">{t('issue.sale.shape.hint')}</HelpTip>
                  </span>
                  <Segmented<CurveShape>
                    aria-label={t('issue.sale.shape')}
                    value={u.shape}
                    onChange={(v) => setSale({ shape: v })}
                    options={[
                      { value: 'linear', label: t('issue.sale.shape.linear'), 'data-testid': 'sale-shape-linear' },
                      { value: 'chromatic', label: t('issue.sale.shape.chromatic'), 'data-testid': 'sale-shape-chromatic' },
                    ]}
                    data-testid="sale-shape"
                  />
                </div>
                <div class="issue-grid issue-grid-3">
                  {chromatic ? (
                    <Field label={t('issue.sale.octaves')} value={u.octaves} onValue={(v) => setSale({ octaves: v.trim() })} error={errOf('octaves')} inputMode="numeric" data-testid="sale-octaves" />
                  ) : (
                    <Field label={t('issue.sale.levels')} value={u.levels} onValue={(v) => setSale({ levels: v.trim() })} error={errOf('levels')} inputMode="numeric" data-testid="sale-levels" />
                  )}
                  <Field
                    label={t('issue.sale.total')}
                    value={u.totalKas}
                    onValue={(v) => setSale({ totalKas: v.trim() })}
                    error={errOf('totalKas')}
                    inputMode="decimal"
                    suffix={<span class="muted">KAS</span>}
                    data-testid="sale-total"
                  />
                  {chromatic ? (
                    // the levels and the top of the octaves typed (12 x N + 1 levels, the top 2^N times the first)
                    <div class="issue-sale-total" data-testid="sale-octaves-range">
                      <span class="small muted">{t('issue.sale.levels')}</span>
                      <strong>{sale?.parsed?.octaves ? t('issue.sale.octaves.range', { levels: sale.parsed.levels, times: 2 ** sale.parsed.octaves }) : '-'}</strong>
                    </div>
                  ) : (
                    <Field
                      label={t('issue.sale.multiple')}
                      value={u.multiple}
                      onValue={(v) => setSale({ multiple: v.trim() })}
                      error={errOf('multiple')}
                      inputMode="decimal"
                      suffix={<span class="muted">{t('issue.sale.multiple.suffix')}</span>}
                      data-testid="sale-multiple"
                    />
                  )}
                </div>
                {sale?.curveError ? <p class="field-error" data-testid="sale-curve-error">{friendlyErrorText(sale.curveError)}</p> : null}
                {sale?.curve && n ? (
                  <div class="issue-curve" data-testid="sale-curve">
                    <CurveGraph steps={sale.curve.steps} label={t('issue.sale.graph')} axis={[t('issue.sale.axis0'), t('issue.sale.axis1')]} data-testid="sale-graph" />
                    <dl class="issue-curve-numbers" data-testid="sale-numbers">
                      <div><dt>{t('issue.sale.first')}</dt><dd data-testid="sale-first">{kasText(n.first)} KAS</dd></div>
                      <div><dt>{t('issue.sale.last')}</dt><dd data-testid="sale-last">{kasText(n.last)} KAS</dd></div>
                      <div><dt>{t('issue.sale.average')}</dt><dd data-testid="sale-average">{kasText(n.average)} KAS</dd></div>
                      <div><dt>{t('issue.sale.realised')}</dt><dd data-testid="sale-realised">{kasText(n.total)} KAS</dd></div>
                    </dl>
                  </div>
                ) : null}
                <p class="small muted">{t('issue.sale.note')}</p>
              </>
            )}
          </div>
        )}
        {renderLock()}
      </Section>
    );
  }

  /** "Lock my share until": one compact row under the choices (what it locks and how it opens are behind its "?"). */
  function renderLock() {
    const l = state.lock;
    const until = lockUntilOf(l);
    const err = errorOf('lock', l.choice === 'none' ? '' : 'x');
    const today = formatRowDate(Date.now());
    return (
      <div class="issue-lock" data-testid="issue-lock">
        <div class="issue-lock-row">
          <span class="help-title">
            <label for="issue-lock-choice">{t('issue.lock.label')}</label>
            <HelpTip label={t('issue.lock.label')} data-testid="issue-lock-help">
              <p>{t(state.sale.mode === 'keep' ? 'issue.lock.helpKeep' : 'issue.lock.helpSale')}</p>
              <p>{t('issue.lock.helpHow')}</p>
              <p>{t('issue.lock.helpUnlock')}</p>
            </HelpTip>
          </span>
          <select id="issue-lock-choice" class="select issue-lock-select" value={l.choice} onChange={(e) => setLock((e.currentTarget as HTMLSelectElement).value as LockChoice)} data-testid="issue-lock-choice">
            <option value="none">{t('issue.lock.none')}</option>
            {LOCK_DAYS.map((d) => <option key={d} value={d}>{t('issue.lock.days', { days: d })}</option>)}
            <option value="date">{t('issue.lock.date')}</option>
          </select>
          {l.choice === 'date' ? (
            <input
              type="date"
              class="input issue-lock-date"
              min={today}
              value={l.date}
              aria-label={t('issue.lock.date')}
              aria-invalid={err ? 'true' : undefined}
              onInput={(e) => setState((s) => ({ ...s, lock: { ...s.lock, date: (e.currentTarget as HTMLInputElement).value } }))}
              data-testid="issue-lock-date"
            />
          ) : null}
          {l.choice !== 'none' && /^\d+$/.test(until) && until !== '0' ? <span class="small muted" data-testid="issue-lock-until">{t('issue.lock.until', { date: formatRowDate(Number(until)) })}</span> : null}
        </div>
        {err ? <p class="field-error" data-testid="issue-lock-error">{err}</p> : null}
      </div>
    );
  }

  function renderSupply() {
    const preview = supplyPreview(state);
    return (
      <Field
        label={t('issue.field.supply')}
        help={<HelpTip label={t('issue.field.supply')} data-testid="issue-supply-help">{t('issue.field.supply.hint')}</HelpTip>}
        hint={preview ? <span class="issue-preview" data-testid="issue-supply-preview">{t('issue.field.supply.preview', preview)}</span> : undefined}
        value={state.supply}
        onValue={(v) => set({ supply: v.trim() })}
        onBlur={() => touch('supply')}
        error={errorOf('supply', state.supply)}
        inputMode="decimal"
        data-testid="issue-supply"
      />
    );
  }

  function renderHolders() {
    const rem = remainingText(state);
    const count = walletRow || state.extras.length === 0 ? state.extras.length + 1 : state.extras.length;
    return (
      <div class="stack-sm" data-testid="issue-holders">
        <div class="help-title">
          <h3>{t('issue.section.holders')}</h3>
          <HelpTip label={t('issue.section.holders')} data-testid="issue-holders-help">{t('issue.holders.intro')}</HelpTip>
        </div>
        <div class="stack-sm">
          <div class="issue-holder issue-holder-you" data-testid="issue-holder-row-0">
            <Field
              label={t('issue.holders.you')}
              value={pubkey ?? ''}
              placeholder={t('issue.connect.title')}
              readOnly
              data-testid="issue-holder-0-owner"
              class="issue-mono"
            />
            <Field
              label={t('issue.holders.yourShare')}
              value={rem}
              readOnly
              error={errorOf('holder-0-amount', 'x')}
              data-testid="issue-holder-0-amount"
              suffix={<span class="muted">{form.ticker}</span>}
            />
            <span />
          </div>
          {state.extras.map((h, n) => {
            const row = n + 1;
            const patch = (p: Partial<{ owner: string; amount: string }>) => set({ extras: state.extras.map((x, k) => (k === n ? { ...x, ...p } : x)) });
            return (
              <div class="issue-holder" key={row} data-testid={`issue-holder-row-${row}`}>
                <Field
                  label={t('issue.holders.owner')}
                  value={h.owner}
                  onValue={(v) => patch({ owner: filterOwner(v) })}
                  onBlur={() => touch(`holder-${row}-owner`)}
                  error={errorOf(`holder-${row}-owner`, h.owner)}
                  data-testid={`issue-holder-${row}-owner`}
                  class="issue-mono"
                />
                <Field
                  label={t('issue.holders.amount')}
                  value={h.amount}
                  onValue={(v) => patch({ amount: v.trim() })}
                  onBlur={() => touch(`holder-${row}-amount`)}
                  error={errorOf(`holder-${row}-amount`, h.amount)}
                  inputMode="decimal"
                  data-testid={`issue-holder-${row}-amount`}
                />
                <Button variant="ghost" onClick={() => set({ extras: state.extras.filter((_, k) => k !== n) })} data-testid={`issue-holder-${row}-remove`} aria-label={`${t('issue.holders.remove')} ${row}`}>
                  {t('issue.holders.remove')}
                </Button>
              </div>
            );
          })}
        </div>
        {state.extras.length > 0 ? <p class="muted small" data-testid="issue-owner-hint">{t('issue.holders.owner.hint')}</p> : null}
        <div class="row issue-holders-foot">
          <Button onClick={() => set({ extras: [...state.extras, { owner: '', amount: '' }] })} data-testid="issue-holder-add">{t('issue.holders.add')}</Button>
          <span class="muted" data-testid="issue-holder-count">{t('issue.holders.count', { count })}</span>
        </div>
        {state.extras.length > 0 && !walletRow ? <p class="muted">{t('issue.holders.walletDrops')}</p> : null}
        {own.length > 0 ? (
          <div class="stack-sm" data-testid="issue-holders-issues">
            {own.map((i) => (
              <Banner key={i.code} tone={i.severity === 'error' ? 'error' : 'warn'} data-testid={`issue-issue-${i.code}`}>
                {t(`issue.model.${i.code}`, i.params)}
              </Banner>
            ))}
          </div>
        ) : null}
      </div>
    );
  }

  function renderWalletGate() {
    if (!wallet.info) {
      // the kit's one disconnected state (review M-12): a sentence and the connect buttons as its action; the notes about a missing extension sit under it
      return (
        <div class="stack-sm" data-testid="issue-connect">
          <EmptyState
            icon="wallet"
            class="empty-state-wide"
            data-testid="issue-connect-state"
            action={
              // a phone, or a desktop with no extension detected: the one Connect of the header (the extension if there is one, else the private key entry);
              // a desktop with extensions: a button for each, and the private key beside them
              phoneWidth || wallet.detected.length === 0 ? (
                <ConnectBar gate />
              ) : (
                <>
                  {wallet.detected.map((a) => (
                    <Button key={a.id} small variant="primary" class="btn-pill" loading={wallet.connecting} onClick={() => void wallet.connect(a.id)} data-testid={`issue-connect-${a.id}`}>
                      {t('issue.connect.button', { label: a.label })}
                    </Button>
                  ))}
                  {/* the private key beside the extensions, as the header's pick offers it (WalletButton.tsx) */}
                  {privateKeySigningAllowed() && visibilityCheckSupported() ? (
                    <Button onClick={() => wallet.openPrivateKey()} data-testid="issue-connect-local-key">
                      {t('shell.wallet.connectKey')}
                    </Button>
                  ) : null}
                </>
              )
            }
          >
            {t('issue.connect.body')}
          </EmptyState>
          {wallet.detected.length === 0 && !privateKeySigningAllowed() ? (
            <p class="muted small issue-connect-note" data-testid="issue-connect-mirror">{t('signer.mirrorOff')}</p>
          ) : wallet.detected.length === 0 && visibilityCheckSupported() ? (
            <p class="muted small issue-connect-note" data-testid="issue-connect-none">{t('issue.connect.none')}</p>
          ) : null}
          {wallet.error ? (
            <div class="field-error wrap-anywhere">
              {friendlyErrorText(wallet.error)}
              <RawDetails text={wallet.error} />
            </div>
          ) : null}
        </div>
      );
    }
    if (wallet.networkMismatch) {
      return (
        <Banner tone="error" title={t('issue.network.title')} data-testid="issue-network-mismatch">
          {t('issue.network.body', { wallet: wallet.info.network, app: config.network })}
        </Banner>
      );
    }
    // the balance itself is beside the Summary heading: here only its loading and its error
    if (!(funding.loading && spendable === null) && !funding.error) return null;
    return (
      <div class="row issue-funds" data-testid="issue-funds">
        {funding.loading && spendable === null ? (
          <>
            <Spinner />
            <span class="muted">{t('issue.funds.loading')}</span>
          </>
        ) : funding.error ? (
          <ErrorBanner error={t('issue.funds.error')} onRetry={() => setRetry((n) => n + 1)} data-testid="issue-funds-error" />
        ) : null}
      </div>
    );
  }

  function renderPlanArea() {
    // nothing is planned while a creation is left unfinished (the hint under the button says why)
    if (phase.kind === 'form' && pending.length > 0) return null;
    if (plan.status === 'pending') {
      return (
        <div class="loading-row" data-testid="issue-plan-pending">
          <Spinner />
          <span>{t('issue.plan.pending')}</span>
        </div>
      );
    }
    if (plan.status === 'error') {
      return (
        <Banner tone="error" title={t('issue.plan.errorTitle')} data-testid="issue-plan-error" actions={<Button small onClick={() => setRetry((n) => n + 1)} data-testid="issue-plan-retry">{t('issue.plan.retry')}</Button>}>
          <span class="wrap-anywhere">{friendlyErrorText(plan.message)}</span>
          <RawDetails text={plan.message} />
        </Banner>
      );
    }
    if (plan.status === 'invalid') {
      return (
        <div class="stack-sm" data-testid="issue-plan-issues">
          {plan.issues.map((i, n) => {
            const sf = shortfallOf(i);
            return (
              <Banner key={`${i.code}-${n}`} tone={i.severity === 'error' ? 'error' : 'warn'} data-testid={`issue-issue-${i.code}`}>
                {sf
                  ? sf.missing > 0n
                    ? t('issue.funds.short', { missing: kasText(sf.missing), need: kasText(sf.need), have: kasText(sf.have) })
                    : t('issue.funds.shortFee', { need: kasText(sf.need), have: kasText(sf.have) })
                  : say(i)}
              </Banner>
            );
          })}
        </div>
      );
    }
    if (plan.status === 'ready' && summary) {
      return (
        <div class="stack-sm" data-testid="issue-summary">
          <KeyValueList
            items={[
              { label: t('issue.summary.name'), value: <span class="wrap-anywhere">{summary.name}</span>, 'data-testid': 'issue-summary-name' },
              { label: t('issue.summary.ticker'), value: <strong>{summary.ticker}</strong>, 'data-testid': 'issue-summary-ticker' },
              { label: t('issue.summary.decimals'), value: summary.decimals, 'data-testid': 'issue-summary-decimals' },
              { label: t('issue.summary.supply'), value: `${summary.supplyHuman} ${summary.ticker}`, 'data-testid': 'issue-summary-supply' },
              {
                label: t('issue.summary.holders'),
                value: (
                  <ul class="issue-holder-list">
                    {summary.holders.map((h) => (
                      <li key={h.index} class="wrap-anywhere">
                        {t('issue.summary.holder', { owner: h.isWallet ? t('issue.summary.you') : h.isCurve ? t('issue.summary.holderCurve') : h.lockedUntil !== undefined ? t('issue.summary.holderLocked', { date: formatRowDate(Number(h.lockedUntil)) }) : shortId(h.owner, 14), amount: `${h.human} ${summary.ticker}` })}
                      </li>
                    ))}
                  </ul>
                ),
                'data-testid': 'issue-summary-holders',
              },
              ...(plan.status === 'ready' && plan.plan.sale ? [{ label: t('issue.summary.sale'), value: saleSummaryText(plan.plan), 'data-testid': 'issue-summary-sale' }] : []),
              ...(summary.lock ? [{ label: t('issue.summary.lock'), value: t('issue.summary.lockValue', { amount: `${summary.lock.human} ${summary.ticker}`, date: formatRowDate(Number(summary.lock.until)) }), 'data-testid': 'issue-summary-lock' }] : []),
              { label: t('issue.summary.program'), value: <code>{summary.program}</code> },
              {
                label: t('issue.summary.carriers'),
                value: t('issue.summary.carriersValue', { each: kasText(summary.carrierEach), count: summary.outputCount, total: kasText(summary.carrierTotal) }),
                'data-testid': 'issue-summary-carriers',
              },
              { label: t('issue.summary.fee'), value: t('issue.summary.feeValue', { fee: kasText(summary.fee) }), 'data-testid': 'issue-summary-fee' },
            ]}
          />
          <p class="muted">{t('issue.summary.fixed')}</p>
        </div>
      );
    }
    return <EmptyState icon="list" compact data-testid="issue-summary-empty">{t('issue.summary.empty')}</EmptyState>;
  }
}

/** The fixed-supply explanation block: what the token can and cannot do, and the reference program's unaudited status. */
