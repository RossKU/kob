import type { ComponentChildren } from 'preact';
import type { Services } from '../../app/services';
import { t } from '../../i18n';
import { longId } from '../../kob/registry';
import { formatUnits } from '../../kob/units';
import { Amount, Badge, CopyText, KeyValueList, Section } from '../kit';
import type { TokenInfo } from '../../kob/registry';
import { KascovNote } from './KascovNote';
import { IdCopy } from './IdCopy';
import { KascovLink, programFactText, programTitle } from './ProgramNote';
import { IdentityBlock } from './Fingerprint';
import { LogoSection } from './LogoReport';
import { TokenNote } from './TokenNote';
import { decimalsNote } from '../../app/token-display';
import { GenesisFailedBanner, PowersWarning, SharedTickerNote, FactLabel, FactSentence, StatusBadges, powerName, riskReasons, tokenTitle } from './TokenBadges';
import { riskFactIds, templateDetail, type TemplateDetail, type TokenRow } from './token-model';

/** Cross-check findings between the indexer's token entry and the registry, translated. */
export function IndexerProblems(props: { codes: string[] }) {
  if (!props.codes.length) return null;
  return (
    <div class="banner banner-error" role="alert" data-testid="indexer-problems">
      <div class="banner-body">
        <div class="banner-title">{t('market.indexerProblems.title')}</div>
        <ul>
          {props.codes.map((c) => (
            <li key={c}>{t(`market.indexerProblems.${c}`)}</li>
          ))}
        </ul>
      </div>
    </div>
  );
}

/** The registry's template check of a registry token, behind "Status and checks": its template hash against the one pinned in this build, the verdict, the review and the slots. */
function TemplateChecks(props: { detail: TemplateDetail }) {
  const d = props.detail;
  return (
    <div data-testid="template-verification">
      <p class="small" style="margin:8px 0 4px"><strong>{t('market.tpl.title')}</strong></p>
      <KeyValueList
        compact
        items={[
          { label: t('market.tpl.id'), value: <code>{d.templateId}</code> },
          { label: t('market.tpl.registryHash'), value: <code class="wrap-anywhere" data-testid="tpl-registry-hash">{d.registryHash}</code> },
          { label: t('market.tpl.pinnedHash'), value: <code class="wrap-anywhere" data-testid="tpl-pinned-hash">{d.pinnedHash ?? t('market.tpl.notPinned')}</code> },
          { label: t('market.tpl.verdict'), value: <span data-testid="tpl-verdict">{d.matches ? t('market.tpl.matches') : t('market.tpl.differs')}</span> },
          { label: t('market.tpl.review'), value: d.reviewed === null ? '-' : d.reviewed ? t('market.tpl.reviewed') : t('market.tpl.pendingReview') },
          { label: t('market.tpl.slots'), value: `${d.slots.inputs} / ${d.slots.outputs}` },
          { label: t('market.tpl.problems'), value: d.problems.map((p) => t(`market.tpl.problem.${p}`)).join(', '), show: d.problems.length > 0 },
        ]}
      />
    </div>
  );
}

/**
 * What a token page must say wherever it is read, never behind a tap: the shared-ticker note, the cross-check findings between the indexer and the
 * registry, the issuer powers, and why a token cannot be traded. The fact badges and the program / decimals lines are not here: they are badges
 * with their sentence as the tooltip and the tooltip of the token-family chip (token page header, pair facts).
 */
export function TokenWarnings(props: { row: TokenRow; sharedTicker?: boolean }) {
  const r = props.row;
  return (
    <>
      <SharedTickerNote shared={!!props.sharedTicker} block data-testid="token-shared-ticker" />
      <IndexerProblems codes={r.indexerProblems} />
      <GenesisFailedBanner row={r} />
      <PowersWarning powers={r.powers} />
      {!r.tradable && r.reason && r.source !== 'kascov' ? (
        <p class="small" style="margin:0" data-testid="token-untradable">
          <strong>{t('market.notTradable')}</strong> {t(`market.reason.${r.reason}`)}
        </p>
      ) : null}
    </>
  );
}

/**
 * Token title bar (top of the page), compact so the trading content starts right below it: the identity block and the pair selector, one line with the
 * short covenant id (copy button), the name, where its facts come from, the link to kascov.io and the live state, one line of badges (the token
 * family, whose tooltip names the program and the decimals, and the facts, each with its sentence as the tooltip) and the warnings that must be seen. The
 * title itself is for screen readers: the pair selector already names the token.
 */
export function TokenHeader(props: {
  row: TokenRow;
  /** another listed token uses this ticker */
  sharedTicker?: boolean;
  badges?: ComponentChildren;
  tools?: ComponentChildren;
  /** the pair selector (`BTC / KAS`, `KAS / TUSD`; MarketBar.tsx) */
  pair?: ComponentChildren;
  /** where the token's facts come from its KCC-23 declaration: the marker next to its name (TokenNote.tsx), as on the wallet views */
  note?: Pick<TokenInfo, 'decimalsSource' | 'decimals' | 'declarationConflicts'> | null;
}) {
  const r = props.row;
  return (
    <div class="stack-sm" data-testid="token-header">
      {/* compact: the identity block at the left, two lines to its right: the pair (ticker / KAS and the flip) and the token's line (short id with copy, name,
          where its facts come from, the live state and the chips). No row of its own that is mostly empty. */}
      <div class="mkt-hd">
        <IdentityBlock row={r} size="lg" data-testid="token-identity" />
        <div class="mkt-hd-text">
          <div class="mkt-hd-line1">
            <div class="mkt-title mkt-title-fit">{props.pair}</div>
            {props.tools}
          </div>
          <h1 class="sr-only" data-testid="token-title" data-covenant-id={r.covenantId}>
            {tokenTitle(r)}
          </h1>
          <TokenLine row={r} note={props.note} badges={props.badges} />
        </div>
      </div>
      <TokenWarnings row={r} sharedTicker={props.sharedTicker} />
    </div>
  );
}

/**
 * One token's compact info line: the short covenant id with its copy button, the name (and where its facts come from), the link to kascov.io, whatever
 * `badges` the page adds (the live state) and the chips (the token family, whose tooltip names the program and the decimals, and the facts: KCC-20, the
 * genesis status ...). They flow on one wrapping line; `ticker` names the token first when two of these lines are read together (a pair).
 */
export function TokenLine(props: { row: TokenRow; note?: Pick<TokenInfo, 'decimalsSource' | 'decimals' | 'declarationConflicts'> | null | undefined; badges?: ComponentChildren; ticker?: boolean; 'data-testid'?: string }) {
  const r = props.row;
  return (
    <div class="mkt-meta small" data-testid={props['data-testid'] ?? 'token-meta'} data-token={r.covenantId}>
      {props.ticker ? <strong class="mkt-meta-ticker" data-testid="token-line-ticker">{r.ticker || longId(r.covenantId)}</strong> : null}
      <IdCopy value={r.covenantId} data-testid={props.ticker ? 'pair-token-short-id' : 'token-short-id'} />
      {r.name ? <span class="mkt-name" data-testid="token-name">{r.name}</span> : null}
      <TokenNote token={props.note} />
      <KascovNote row={r} />
      <KascovLink row={r} />
      {props.badges}
      <StatusBadges row={r} familyNote={programTitle(r)} />
    </div>
  );
}

/**
 * Everything a token's chips leave out by default, one tap away (token page and pair page, under the token's facts): why it is marked "High risk", every fact
 * with its sentence (`Genesis not verified` and its reason included: a normal token shows no status chip for it), and where its name and decimals come
 * from when that is its on-chain declaration. Not a rating: nothing here says a token is safe. Renders nothing for a token with none of these.
 */
function TokenStatusDetails(props: { row: TokenRow; services: Pick<Services, 'registry' | 'kob'>; note?: Pick<TokenInfo, 'decimalsSource' | 'decimals' | 'declarationConflicts'> | null | undefined }) {
  const r = props.row;
  // a token synthesised from the indexer has no registry template to compare with
  const tpl = r.info && r.source === 'registry' ? templateDetail(r.info, props.services.registry, props.services.kob.templates()) : null;
  const risks = riskReasons(r);
  const stated = new Set(riskFactIds(r));
  const facts = r.facts.filter((f) => !stated.has(f));
  const declared = r.declared === true ? { decimals: r.decimals } : props.note?.decimalsSource === 'declared' && !props.note.declarationConflicts?.length ? { decimals: props.note.decimals } : null;
  const other = props.note && !declared ? decimalsNote(props.note) : null;
  if (risks.length === 0 && facts.length === 0 && !declared && !other && !tpl) return null;
  return (
    <Section title={t('market.status.title')} collapsible defaultOpen={false} data-testid="token-status-details" summary={
      risks.length > 0 || tpl?.matches === false ? (
        <>
          {risks.length > 0 ? <Badge tone="warn" data-testid="status-summary">{t('market.badge.highRisk')}</Badge> : null}
          {tpl?.matches === false ? <Badge tone="bad" data-testid="tpl-summary">{t('market.tpl.summary.differs')}</Badge> : null}
        </>
      ) : undefined
    }>
      <ul class="tf-list" data-testid="token-status-list">
        {risks.map((x) => (
          <li key={x.key} data-reason={x.key} data-testid={`status-risk-${x.key}`}>
            <strong>{t('market.badge.highRisk')}: </strong>
            {x.text}
          </li>
        ))}
        {facts.map((id) => (
          <li key={id} data-fact={id} data-testid={`status-fact-${id}`}>
            <strong><FactLabel row={r} id={id} />: </strong>
            <FactSentence row={r} id={id} />
          </li>
        ))}
        {declared ? (
          <li data-testid="status-declared">
            <strong>{t('token.note.declared')}: </strong>
            {t('token.note.declaredTitle', { decimals: declared.decimals ?? '' })}
          </li>
        ) : null}
        {other ? (
          <li data-testid="status-decimals-note">
            <strong>{other.text}: </strong>
            {other.title}
          </li>
        ) : null}
      </ul>
      {tpl ? <TemplateChecks detail={tpl} /> : null}
    </Section>
  );
}

/** Token facts (bottom of the page): covenant id, program (what it runs and how that was established), decimals, price scale, tick, open orders; a registry token's template checks are in "Status and checks". */
export function TokenDetails(props: { row: TokenRow; services: Pick<Services, 'registry' | 'kob'>; note?: Pick<TokenInfo, 'decimalsSource' | 'decimals' | 'declarationConflicts'> | null; /** the row with KOB's own genesis check applied (a token page): what the status section is printed from */ checked?: TokenRow }) {
  const r = props.row;
  return (
    <div class="mkt-details" data-testid="token-details">
      <Section title={t('market.token.info')} class="mkt-card">
        <KeyValueList
          compact
          items={[
            { label: t('market.token.covenantId'), value: <CopyText value={r.covenantId} short={false} data-testid="token-covenant-id" /> },
            { label: t('market.program.label'), value: <span data-testid="token-program">{programFactText(props.checked ?? r)}</span> },
            { label: t('market.token.decimals'), value: r.decimals ?? '-' },
            {
              label: t('market.token.scale'),
              // the standard scale is 10^decimals, at most 10^9: a token of more than 9 decimals quotes per a part of a token
              value:
                r.scale === null
                  ? '-'
                  : r.decimals !== null && r.scale !== 10n ** BigInt(r.decimals)
                    ? t('market.token.scaleValuePart', { part: formatUnits(r.scale, r.decimals), ticker: r.ticker || longId(r.covenantId) })
                    : t('market.token.scaleValue', { ticker: r.ticker || longId(r.covenantId), decimals: r.decimals ?? (r.scale.toString().length - 1) }),
              'data-testid': 'token-scale',
            },
            {
              label: t('market.token.tick'),
              value: r.tick !== null ? <Amount kind="kas" value={r.tick} unit={t('market.token.perToken', { ticker: r.ticker })} /> : '-',
              'data-testid': 'token-tick',
              show: r.tick !== null, // an unknown tick is no row, not a dash
            },
            { label: t('market.token.powers'), value: <span data-testid="token-powers">{r.powers.map(powerName).join(', ')}</span>, show: r.powers.length > 0 },
            { label: t('market.token.openOrders'), value: r.openAsks !== null ? t('market.token.openOrdersValue', { asks: r.openAsks, bids: r.openBids ?? 0 }) : t('market.token.openOrdersUnknown'), show: r.index !== null },
          ]}
        />
      </Section>
      <TokenStatusDetails row={props.checked ?? r} services={props.services} note={props.note} />
      <LogoSection row={r} />
    </div>
  );
}

/**
 * The facts of both tokens of a pair (bottom of the pair page, the same panel as a token market's): per token its title with the badges
 * and warnings, then the covenant id, decimals, price scale, tick, open orders and the program.
 */
export function PairDetails(props: { rows: readonly TokenRow[]; shared?: ReadonlySet<string>; services: Pick<Services, 'registry' | 'kob'> }) {
  return (
    <div class="stack" data-testid="pair-details">
      {props.rows.map((r) => (
        <div class="stack-sm" key={r.covenantId} data-testid="pair-token-details" data-token={r.covenantId}>
          <div class="mkt-title">
            <IdentityBlock row={r} data-testid="pair-token-identity" />
            <h2 class="token-name" style="margin:0;font-size:1.05rem" data-testid="pair-token-title">{tokenTitle(r)}</h2>
            {r.name ? <span class="mkt-name">{r.name}</span> : null}
          </div>
          <div class="mkt-badges">
            <StatusBadges row={r} familyNote={programTitle(r)} />
          </div>
          <TokenWarnings row={r} sharedTicker={props.shared?.has(r.covenantId) ?? false} />
          <TokenDetails row={r} services={props.services} />
        </div>
      ))}
    </div>
  );
}
