// Token issuance (`kob token issue` in the browser): a fixed-supply KCC-20 token on the KCC-20 reference program, KOB's standard 3 / 3
// build `KCC20Ref` by default or the reference's published public-mint build `KCC20PublicMint`. Form -> live validation ->
// debounced `planIssue` -> summary -> the shared pre-sign screen (ConfirmSign) -> tracker + result panel with the registry entry.
// All protocol logic is in kob/issue.ts (kob-wasm); this view only collects strings and renders findings.
import { useMemo, useState } from 'preact/hooks';
import { readFees } from '../../app/env';
import { useServices, useWallet } from '../../app/context';
import { t } from '../../i18n';
import { issueText } from '../../i18n/issue-text';
import { issueLimits, issuedTokenUtxos, validateIssueForm, type IssueLimits, type IssuePlan, type IssueProgramChoice } from '../../kob/issue';
import type { PlanIssue } from '../../kob/plan-types';
import { isKeyOwned } from '../../kob/token-state';
import type { Services } from '../../app/services';
import { ConfirmSign, type ConfirmResult } from '../confirm/ConfirmSign';
import { Badge, Banner, Button, ErrorBanner, Field, KeyValueList, Section, Segmented, Spinner, kasText, shortId, useAsync } from '../kit';
import {
  defaultCarrierText, emptyUiState, filterDecimals, filterOwner, filterTicker, formToIssueForm, groupByField, modelIssues, parseDecimalsText, remainingText,
  reviewSummary, shortfallOf, supplyPreview, walletRowIncluded, type IssueUiState,
} from './issue-model';
import { ResultPanel } from './ResultPanel';
import { useIssuePlan } from './use-issue-plan';
import './issue.css';

type Phase = { kind: 'form' } | { kind: 'confirm'; plan: IssuePlan } | { kind: 'result'; plan: IssuePlan; txid: string; trackFailed: boolean };
type Notice = { tone: 'info' | 'error'; text: string } | null;

/** Remembers the token outputs of the genesis in the tracker (a genesis has no token leader, so `trackFromBuilt` does not cover it). */
function trackIssued(services: Services, plan: IssuePlan): boolean {
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
  const { kob, config } = services;

  const [state, setState] = useState<IssueUiState>(emptyUiState);
  const [touched, setTouched] = useState<ReadonlySet<string>>(new Set());
  const [phase, setPhase] = useState<Phase>({ kind: 'form' });
  const [notice, setNotice] = useState<Notice>(null);
  const [retry, setRetry] = useState(0);

  const set = (patch: Partial<IssueUiState>) => setState((s) => ({ ...s, ...patch }));
  const touch = (f: string) => setTouched((s) => (s.has(f) ? s : new Set(s).add(f)));

  // ---- limits (older wasm bindings do not export issue: say so instead of crashing)
  const limitsResult = useMemo<{ limits: IssueLimits | null; error: string | null }>(() => {
    try {
      return { limits: issueLimits(kob), error: null };
    } catch (e) {
      return { limits: null, error: e instanceof Error ? e.message : String(e) };
    }
  }, [kob]);
  const limits = limitsResult.limits;

  const pubkey = wallet.info?.pubkey ?? null;
  const usable = pubkey !== null && !wallet.networkMismatch;
  const registryTickers = useMemo(() => services.registry.tokens.map((x) => x.ticker), [services.registry]);

  // ---- funding
  const funding = useAsync(async () => (usable && pubkey ? services.utxos.fundingFor(pubkey) : null), [pubkey, usable, retry]);
  const fundingList = usable && pubkey ? (funding.data ?? null) : null;
  const spendable = fundingList ? fundingList.reduce((s, u) => s + BigInt(u.amount), 0n) : null;

  // ---- form -> findings
  const form = useMemo(() => formToIssueForm(state, pubkey), [state, pubkey]);
  const walletRow = walletRowIncluded(state);
  const decimals = parseDecimalsText(state.decimals);
  const findings = useMemo<PlanIssue[]>(() => (limits ? validateIssueForm(form, limits, registryTickers) : []), [form, limits, registryTickers]);
  const own = useMemo(() => modelIssues(state, pubkey, 0), [state, pubkey]);
  const blocked = findings.some((i) => i.severity === 'error') || own.some((i) => i.severity === 'error');

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
    enabled: limits !== null && !blocked && usable && phase.kind === 'form',
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
  const canReview = plan.status === 'ready' && phase.kind === 'form';

  const onClose = (r: ConfirmResult) => {
    const current = phase.kind === 'confirm' ? phase.plan : null;
    if (!current) return;
    if (r.status === 'submitted') {
      const trackFailed = !trackIssued(services, current);
      setNotice(null);
      setPhase({ kind: 'result', plan: current, txid: r.txid, trackFailed });
      return;
    }
    setPhase({ kind: 'form' });
    setRetry((n) => n + 1); // the spent UTXOs may have changed: read the funding again
    setNotice(r.status === 'failed' ? { tone: 'error', text: r.message } : { tone: 'info', text: t('issue.cancelled') });
  };

  const another = () => {
    setState(emptyUiState());
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
        <h1>{t('issue.title')}</h1>
        <p class="muted">{t('issue.intro')}</p>
      </div>

      <FixedSupplyNotes program={state.program} />

      {limitsResult.error ? <ErrorBanner error={limitsResult.error} data-testid="issue-limits-error" /> : null}

      {renderWalletGate()}

      <div class="stack" data-testid="issue-form">
        <Section title={t('issue.section.program')} data-testid="issue-program-section">
          <div class="stack-sm">
            <Segmented<IssueProgramChoice>
              aria-label={t('issue.section.program')}
              value={state.program}
              onChange={(v) => set({ program: v })}
              options={[
                { value: 'standard', label: t('issue.program.standard'), 'data-testid': 'issue-program-standard' },
                { value: 'public-mint', label: t('issue.program.publicMint'), 'data-testid': 'issue-program-public-mint' },
              ]}
              data-testid="issue-program"
            />
            <p class="muted" data-testid="issue-program-hint">
              {t(state.program === 'public-mint' ? 'issue.program.publicMint.hint' : 'issue.program.standard.hint')}
            </p>
          </div>
        </Section>

        <Section title={t('issue.section.token')}>
          <div class="issue-grid">
            <Field
              label={t('issue.field.name')}
              hint={t('issue.field.name.hint')}
              value={state.name}
              onValue={(v) => set({ name: v })}
              onBlur={() => touch('name')}
              error={errorOf('name', state.name)}
              data-testid="issue-name"
              autoComplete="off"
            />
            <Field
              label={t('issue.field.ticker')}
              hint={limits ? t('issue.field.ticker.hint', { min: limits.ticker.minLength, max: limits.ticker.maxLength }) : undefined}
              value={state.ticker}
              onValue={(v) => set({ ticker: filterTicker(v) })}
              onBlur={() => touch('ticker')}
              error={errorOf('ticker', state.ticker)}
              data-testid="issue-ticker"
              autoComplete="off"
              class="issue-mono"
            />
            <Field
              label={t('issue.field.decimals')}
              hint={limits ? t('issue.field.decimals.hint', { max: limits.maxDecimals }) : undefined}
              value={state.decimals}
              onValue={(v) => set({ decimals: filterDecimals(v) })}
              onBlur={() => touch('decimals')}
              error={errorOf('decimals', state.decimals)}
              inputMode="numeric"
              data-testid="issue-decimals"
            />
            {renderSupply()}
          </div>
        </Section>

        {renderHolders()}

        <Section title={t('issue.section.carrier')}>
          <Field
            label={t('issue.field.carrier')}
            hint={t('issue.field.carrier.hint', { default: limits ? defaultCarrierText(limits) : '10' })}
            value={state.carrier}
            placeholder={limits ? defaultCarrierText(limits) : ''}
            onValue={(v) => set({ carrier: v })}
            onBlur={() => touch('carrier')}
            error={errorOf('carrier', state.carrier)}
            inputMode="decimal"
            suffix={<span class="muted">KAS</span>}
            data-testid="issue-carrier"
          />
        </Section>

        <Section title={t('issue.section.meta')} collapsible defaultOpen={false}>
          <div class="issue-grid">
            <Field
              as="textarea"
              label={t('issue.field.description')}
              value={state.description}
              onValue={(v) => set({ description: v })}
              onBlur={() => touch('description')}
              error={errorOf('description', state.description)}
              rows={3}
              data-testid="issue-description"
            />
            <div class="stack-sm">
              <Field
                label={t('issue.field.icon')}
                hint={t('issue.field.icon.hint')}
                value={state.icon}
                onValue={(v) => set({ icon: v })}
                onBlur={() => touch('icon')}
                error={errorOf('icon', state.icon)}
                inputMode="url"
                data-testid="issue-icon"
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

      <Section title={t('issue.summary.title')} data-testid="issue-plan-section">
        {renderPlanArea()}
        {notice ? (
          <Banner tone={notice.tone === 'error' ? 'error' : 'info'} title={notice.tone === 'error' ? t('issue.failed.title') : undefined} data-testid="issue-notice">
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
          issue={{ token: phase.plan.token }}
          onClose={onClose}
        />
      ) : null}
    </div>
  );

  // ---------------------------------------------------------------------------------------------- parts: plain render functions, not components (a component type created per render would remount its inputs and lose focus)

  function reviewHint(): string {
    if (!wallet.info || wallet.networkMismatch) return t('issue.review.connect');
    if (blocked) return t('issue.review.fix');
    if (plan.status === 'invalid') return shortfall ? t('issue.review.connect') : t('issue.review.fix');
    return t('issue.review.wait');
  }

  function renderSupply() {
    const preview = supplyPreview(state);
    return (
      <Field
        label={t('issue.field.supply')}
        hint={
          <>
            <span>{t('issue.field.supply.hint')}</span>
            {preview ? <span class="issue-preview" data-testid="issue-supply-preview">{` ${t('issue.field.supply.preview', preview)}`}</span> : null}
          </>
        }
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
      <Section title={t('issue.section.holders')} data-testid="issue-holders">
        <p class="muted">{t('issue.holders.intro')}</p>
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
      </Section>
    );
  }

  function renderWalletGate() {
    if (!wallet.info) {
      return (
        <Banner
          tone="info"
          title={t('issue.connect.title')}
          data-testid="issue-connect"
          actions={
            <div class="row">
              {wallet.detected.map((a) => (
                <Button key={a.id} variant="primary" small loading={wallet.connecting} onClick={() => void wallet.connect(a.id)} data-testid={`issue-connect-${a.id}`}>
                  {t('issue.connect.button', { label: a.label })}
                </Button>
              ))}
            </div>
          }
        >
          <p>{t('issue.connect.body')}</p>
          {wallet.detected.length === 0 ? <p class="muted">{t('issue.connect.none')}</p> : null}
          {wallet.error ? <p class="field-error wrap-anywhere">{wallet.error}</p> : null}
        </Banner>
      );
    }
    if (wallet.networkMismatch) {
      return (
        <Banner tone="error" title={t('issue.network.title')} data-testid="issue-network-mismatch">
          {t('issue.network.body', { wallet: wallet.info.network, app: config.network })}
        </Banner>
      );
    }
    return (
      <div class="row issue-funds" data-testid="issue-funds">
        {funding.loading && spendable === null ? (
          <>
            <Spinner />
            <span class="muted">{t('issue.funds.loading')}</span>
          </>
        ) : funding.error ? (
          <ErrorBanner error={t('issue.funds.error')} onRetry={() => setRetry((n) => n + 1)} data-testid="issue-funds-error" />
        ) : spendable !== null ? (
          <span data-testid="issue-funds-balance">{t('issue.funds.balance', { kas: kasText(spendable) })}</span>
        ) : null}
      </div>
    );
  }

  function renderPlanArea() {
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
          <span class="wrap-anywhere">{plan.message}</span>
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
                        {t('issue.summary.holder', { owner: h.isWallet ? t('issue.summary.you') : shortId(h.owner, 8, 6), amount: `${h.human} ${summary.ticker}` })}
                      </li>
                    ))}
                  </ul>
                ),
                'data-testid': 'issue-summary-holders',
              },
              { label: t('issue.summary.program'), value: <code>{summary.program}</code> },
              {
                label: t('issue.summary.carriers'),
                value: t('issue.summary.carriersValue', { each: kasText(summary.carrierEach), count: summary.outputCount, total: kasText(summary.carrierTotal) }),
                'data-testid': 'issue-summary-carriers',
              },
              { label: t('issue.summary.fee'), value: t('issue.summary.feeValue', { fee: kasText(summary.fee) }), 'data-testid': 'issue-summary-fee' },
            ]}
          />
          <div class="row">
            <Badge tone="warn">{t('issue.result.unverified')}</Badge>
            <span class="muted">{t('issue.summary.fixed')}</span>
          </div>
        </div>
      );
    }
    return <p class="muted">{t('issue.review.fix')}</p>;
  }
}

/** The fixed-supply explanation block: what the token can and cannot do, and the reference program's unaudited status. */
function FixedSupplyNotes({ program }: { program: IssueProgramChoice }) {
  return (
    <Section title={t('issue.fixed.title')} data-testid="issue-fixed-supply">
      <ul class="issue-notes">
        <li>{t('issue.fixed.supply')}</li>
        <li>{t('issue.fixed.noBurn')}</li>
        <li data-testid="issue-fixed-program">{t(program === 'public-mint' ? 'issue.fixed.programPublicMint' : 'issue.fixed.program')}</li>
        <li>{t('issue.fixed.carrier')}</li>
        <li>{t('issue.fixed.custody')}</li>
      </ul>
      <Banner tone="warn" data-testid="issue-unaudited">
        {t('issue.fixed.unaudited')}
      </Banner>
    </Section>
  );
}
