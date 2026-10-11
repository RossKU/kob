import type { ComponentChildren } from 'preact';
import type { Services } from '../../app/services';
import { t } from '../../i18n';
import { longId } from '../../kob/registry';
import { Amount, Badge, CopyText, KeyValueList, Section } from '../kit';
import { PowersWarning, SharedTickerNote, StatusBadges, powerName, tokenTitle } from './TokenBadges';
import { shortCovenantId, templateDetail, type TokenRow } from './token-model';

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

function TemplateVerification(props: { row: TokenRow; services: Pick<Services, 'registry' | 'kob'> }) {
  const info = props.row.info;
  // a token synthesised from the indexer has no registry template to compare with
  if (!info || props.row.source !== 'registry') return null;
  const d = templateDetail(info, props.services.registry, props.services.kob.templates());
  return (
    <Section title={t('market.tpl.title')} collapsible defaultOpen={false} data-testid="template-verification">
      <KeyValueList
        compact
        items={[
          { label: t('market.tpl.id'), value: <code>{d.templateId}</code> },
          { label: t('market.tpl.registryHash'), value: <code class="wrap-anywhere" data-testid="tpl-registry-hash">{d.registryHash}</code> },
          { label: t('market.tpl.pinnedHash'), value: <code class="wrap-anywhere" data-testid="tpl-pinned-hash">{d.pinnedHash ?? t('market.tpl.notPinned')}</code> },
          {
            label: t('market.tpl.verdict'),
            value: (
              <Badge tone={d.matches ? 'ok' : 'bad'} data-testid="tpl-verdict">
                {d.matches ? t('market.tpl.matches') : t('market.tpl.differs')}
              </Badge>
            ),
          },
          { label: t('market.tpl.review'), value: d.reviewed === null ? '-' : d.reviewed ? t('market.tpl.reviewed') : t('market.tpl.pendingReview') },
          { label: t('market.tpl.slots'), value: `${d.slots.inputs} / ${d.slots.outputs}` },
          { label: t('market.tpl.problems'), value: d.problems.map((p) => t(`market.tpl.problem.${p}`)).join(', '), show: d.problems.length > 0 },
        ]}
      />
    </Section>
  );
}

/** The shared-ticker note, indexer warnings and why a token cannot be traded: shown under the title of a token market and in the facts of each token of a pair. */
export function TokenWarnings(props: { row: TokenRow; sharedTicker?: boolean }) {
  const r = props.row;
  return (
    <>
      <SharedTickerNote shared={!!props.sharedTicker} block data-testid="token-shared-ticker" />
      <IndexerProblems codes={r.indexerProblems} />
      <PowersWarning powers={r.powers} />
      {!r.tradable && r.reason ? (
        <p class="small" style="margin:0" data-testid="token-untradable">
          <strong>{t('market.notTradable')}</strong> {t(`market.reason.${r.reason}`)}
        </p>
      ) : null}
    </>
  );
}

/** Token title bar (top of the page): labelled name, badges, name, the shared-ticker note, indexer warnings, why it cannot be traded. */
export function TokenHeader(props: {
  row: TokenRow;
  /** another listed token uses this ticker */
  sharedTicker?: boolean;
  badges?: ComponentChildren;
  tools?: ComponentChildren;
  /** the pair selector (`BTC / KAS`, `KAS / TUSD`; MarketBar.tsx) */
  pair?: ComponentChildren;
}) {
  const r = props.row;
  return (
    <div class="stack-sm" data-testid="token-header">
      <div class="mkt-top">
        <div class="mkt-title">
          {props.pair}
          <h1 class="token-name" style="margin:0" data-testid="token-title" data-covenant-id={r.covenantId}>
            {tokenTitle(r)}
          </h1>
          <StatusBadges row={r} />
          <span class="small muted" title={r.covenantId} data-testid="token-short-id">{r.info && !r.info.openList ? shortCovenantId(r.covenantId) : longId(r.covenantId)}</span>
          {r.name ? <span class="mkt-name">{r.name}</span> : null}
          {props.badges}
        </div>
        {props.tools}
      </div>
      <TokenWarnings row={r} sharedTicker={props.sharedTicker} />
    </div>
  );
}

/** Token facts (bottom of the page): covenant id, decimals, price scale, tick, open orders, and the template verification. */
export function TokenDetails(props: { row: TokenRow; services: Pick<Services, 'registry' | 'kob'> }) {
  const r = props.row;
  return (
    <div class="grid-2" data-testid="token-details">
      <Section title={t('market.token.info')} class="mkt-card">
        <KeyValueList
          compact
          items={[
            { label: t('market.token.covenantId'), value: <CopyText value={r.covenantId} short={false} data-testid="token-covenant-id" /> },
            { label: t('market.token.decimals'), value: r.decimals ?? '-' },
            {
              label: t('market.token.scale'),
              value: r.scale !== null ? t('market.token.scaleValue', { scale: r.scale.toString() }) : '-',
              'data-testid': 'token-scale',
            },
            {
              label: t('market.token.tick'),
              value: r.tick !== null ? <Amount kind="kas" value={r.tick} unit={t('market.token.perToken', { ticker: r.ticker })} /> : '-',
              'data-testid': 'token-tick',
            },
            { label: t('market.token.powers'), value: <span data-testid="token-powers">{r.powers.map(powerName).join(', ')}</span>, show: r.powers.length > 0 },
            { label: t('market.token.openOrders'), value: r.openAsks !== null ? t('market.token.openOrdersValue', { asks: r.openAsks, bids: r.openBids ?? 0 }) : '-', show: r.index !== null },
          ]}
        />
      </Section>
      <TemplateVerification row={r} services={props.services} />
    </div>
  );
}

/**
 * The facts of both tokens of a pair (bottom of the pair page, the same panel as a token market's): per token its title with the badges
 * and warnings, then the covenant id, decimals, price scale, tick, open orders and the template verification.
 */
export function PairDetails(props: { rows: readonly TokenRow[]; shared?: ReadonlySet<string>; services: Pick<Services, 'registry' | 'kob'> }) {
  return (
    <div class="stack" data-testid="pair-details">
      {props.rows.map((r) => (
        <div class="stack-sm" key={r.covenantId} data-testid="pair-token-details" data-token={r.covenantId}>
          <div class="mkt-title">
            <h2 class="token-name" style="margin:0;font-size:1.05rem" data-testid="pair-token-title">{tokenTitle(r)}</h2>
            <StatusBadges row={r} />
            {r.name ? <span class="mkt-name">{r.name}</span> : null}
          </div>
          <TokenWarnings row={r} sharedTicker={props.shared?.has(r.covenantId) ?? false} />
          <TokenDetails row={r} services={props.services} />
        </div>
      ))}
    </div>
  );
}
