import type { ComponentChildren } from 'preact';
import type { Services } from '../../app/services';
import { t } from '../../i18n';
import { longId } from '../../kob/registry';
import { formatUnits } from '../../kob/units';
import { Amount, Badge, CopyText, KeyValueList, Section } from '../kit';
import type { TokenInfo } from '../../kob/registry';
import { KascovNote } from './KascovNote';
import { KascovLink, programFactText, programTitle } from './ProgramNote';
import { IdentityBlock } from './Fingerprint';
import { LogoSection } from './LogoReport';
import { MarketBar } from './MarketBar';
import { TokenNote } from './TokenNote';
import { decimalsNote } from '../../app/token-display';
import { FamilyChip, GenesisFailedBanner, PowersWarning, RiskChip, SharedTickerBarNote, SharedTickerNote, FactLabel, FactSentence, StatusBadges, powerName, riskReasons, tokenTitle } from './TokenBadges';
import { riskFactIds, templateDetail, type TemplateDetail, type TokenRow } from './token-model';
import type { BridgeMinter } from '../../data/genesis-check';
import { useDeclaration } from '../../app/use-declaration';
import { lockOpen, lockSharePercent, type VerifiedLock } from '../../kob/creator-lock';
import { formatRowDate, rowTimeTitle } from '../kit/format';

/**
 * The token's creator lock as its verified declaration states it (kob/creator-lock.ts). The line
 * gives what the genesis locked, its share of the supply when the supply is proven, and the day it
 * opens. After that day the same line says it is open. What is still locked is the chain's to tell.
 */
function creatorLockText(lock: VerifiedLock, decimals: number | null, ticker: string): string {
  const amount = `${decimals === null ? lock.amount : formatUnits(BigInt(lock.amount), decimals, { group: ',' })}${ticker ? ` ${ticker}` : ''}`;
  const pct = lockSharePercent(lock.amount, lock.supply);
  const date = formatRowDate(Number(lock.until));
  const key = lockOpen(lock.until) ? 'market.token.creatorLockOpen' : 'market.token.creatorLockValue';
  return pct === null ? t(`${key}NoShare`, { amount, date }) : t(key, { amount, pct, date });
}

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

/**
 * The registry's template check of a registry token, behind "Status and checks". It shows the
 * token's template hash against the one pinned in this build, the verdict, the review and the
 * slots.
 */
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
 * What a token page must say wherever it is read, never behind a tap. That is the shared-ticker
 * note, the cross-check findings between the indexer and the registry, the issuer powers, and why
 * a token cannot be traded. The fact badges and the program and decimals lines are not here. They
 * are badges with their sentence as the tooltip, and the tooltip of the token-family chip (token
 * page header, pair facts).
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
 * The token page's header. It holds the market bar (MarketBar.tsx), which is one row with the
 * picture and fingerprint, the pair selector, the flip and the star, the 24 h figures and the live
 * state. It also holds the warnings that must be seen. The "High risk" chip must not wait for a
 * tap into the details, so it stays in the bar beside the star. The covenant id with its copy
 * button, the name and where its facts come from, the link to kascov.io and the program chip are
 * in the token's details card (`TokenDetails`). The title is for screen readers, because the pair
 * selector already names the token.
 */
export function TokenHeader(props: {
  row: TokenRow;
  /** Another listed token uses this ticker. */
  sharedTicker?: boolean;
  /** The live state chip. */
  badges?: ComponentChildren;
  /** The star. */
  tools?: ComponentChildren;
  /** The pair selector (`BTC / KAS`, `KAS / TUSD`; MarketBar.tsx). */
  pair?: ComponentChildren;
  /** The 24 h figures (MarketStats.tsx). */
  stats?: ComponentChildren;
}) {
  const r = props.row;
  return (
    <div class="stack-sm" data-testid="token-header">
      <MarketBar
        identity={<IdentityBlock row={r} size="bar" data-testid="token-identity" />}
        pair={props.pair}
        tools={<>{props.tools}<RiskChip row={r} /></>}
        stats={props.stats}
        live={props.badges}
        note={<SharedTickerBarNote shared={!!props.sharedTicker} />}
      />
      <h1 class="sr-only" data-testid="token-title" data-covenant-id={r.covenantId}>
        {tokenTitle(r)}
      </h1>
      <TokenWarnings row={r} />
    </div>
  );
}

/**
 * Everything a token's chips leave out by default, one tap away, on the token page and the pair
 * page under the token's facts. It gives why the token is marked "High risk". It lists every fact
 * with its sentence, `Genesis not verified` and its reason included, because a normal token shows
 * no status chip for it. It also says where the token's name and decimals come from when that is
 * its on-chain declaration. It is not a rating, and nothing here says a token is safe. It renders
 * nothing for a token with none of these.
 */
function TokenStatusDetails(props: { row: TokenRow; services: Pick<Services, 'registry' | 'kob'>; note?: Pick<TokenInfo, 'decimalsSource' | 'decimals' | 'declarationConflicts'> | null | undefined }) {
  const r = props.row;
  // A token synthesised from the indexer has no registry template to compare with.
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

/**
 * The custody note of a token whose genesis check found it issued by a bridge. KOB pins the
 * BridgeMint by its hash, kUSDT's federated one and Kozmic's ZK one. The backing is on another
 * chain, and nothing here can check it. The note lists what the minter's state names, the signer
 * set and guardian or the bridge covenant. The list is unchecked, for a holder to compare with
 * what the issuer publishes. It is folded by default, like "Status and checks", because it would
 * fill the page.
 */
function BridgeCustodyNote(props: { minter: BridgeMinter | undefined }) {
  const zk = props.minter?.kind === 'zk';
  const f = props.minter?.facts ?? {};
  const facts: [string, string | undefined, string][] = zk
    ? [[t('market.bridge.covenant'), f.bridgeCovenantId, 'token-bridge-covenant']]
    : [[t('market.bridge.signerSet'), f.signerSet, 'token-bridge-signer-set'], [t('market.bridge.guardian'), f.guardian, 'token-bridge-guardian']];
  const shown = facts.filter((x): x is [string, string, string] => x[1] !== undefined);
  return (
    <Section title={t('market.bridge.title')} collapsible defaultOpen={false} data-testid="token-bridge-note">
      <p class="small" style="margin:0" data-testid="token-bridge-note-text">{t(zk ? 'market.bridge.zkText' : 'market.bridge.text')}</p>
      {shown.length > 0 ? (
        <KeyValueList compact items={shown.map(([label, value, id]) => ({ label, value: <CopyText value={value} ellipsis data-testid={id} /> }))} />
      ) : null}
    </Section>
  );
}

/**
 * Token facts at the bottom of the page: covenant id, program (what it runs and how that was
 * established), issuance by a bridge, decimals, price scale, tick and open orders. A registry
 * token's template checks are in "Status and checks".
 */
export function TokenDetails(props: { row: TokenRow; services: Pick<Services, 'registry' | 'kob'>; note?: Pick<TokenInfo, 'decimalsSource' | 'decimals' | 'declarationConflicts'> | null; /** the row with KOB's own genesis check applied (a token page): what the status section is printed from */ checked?: TokenRow }) {
  const r = props.row;
  // The token as the page names it. It is the header's row, with the names and icons kascov.io adds
  // merged into it.
  const named = props.checked ?? r;
  const g = (props.checked ?? r).genesis;
  // This comes from the genesis check only, the pinned BridgeMint in the token's genesis group. It
  // never comes from a name or a list.
  const bridge = g?.status === 'verified' && g.bridge === true;
  const minter = g?.status === 'verified' ? g.bridgeMinter : undefined;
  // The creator lock its verified declaration states. The page that shows the token asks for it,
  // and a known one is read here.
  const lock = useDeclaration(r.covenantId, false)?.lock ?? null;
  return (
    <div class="mkt-details" data-testid="token-details">
      <Section title={t('market.token.info')} class="mkt-card">
        <KeyValueList
          compact
          items={[
            {
              // The token's name and where it comes from, with the link to kascov.io. The source is
              // the registry, its declaration, this browser's issuance or kascov.io.
              label: t('market.token.name'),
              value: (
                <span class="tk-name-line">
                  <span data-testid="token-name">{named.name || r.name || '-'}</span>
                  <TokenNote token={props.note} />
                  <KascovNote row={named} />
                  <KascovLink row={named} />
                </span>
              ),
            },
            // The whole covenant id, the token's identity, with its copy button.
            { label: t('market.token.covenantId'), value: <CopyText value={r.covenantId} short={false} data-testid="token-covenant-id" /> },
            {
              // The program chip, with the program line. Its tooltip gives the program as KOB pins
              // it and where the decimals come from.
              label: t('market.program.label'),
              value: (
                <span class="tk-program">
                  <FamilyChip row={named} note={programTitle(named)} />
                  <span data-testid="token-program">{programFactText(props.checked ?? r)}</span>
                </span>
              ),
            },
            {
              label: t('market.token.issuance'),
              value: (
                <span data-testid="token-issuance">
                  {t(minter?.kind === 'zk' ? 'market.issuance.zkBridge' : 'market.issuance.bridge')}{' '}
                  <span class="small muted" data-testid="token-issuance-source">{t(minter?.kind === 'zk' ? 'market.issuance.zkNote' : 'market.issuance.federatedNote')}</span>
                </span>
              ),
              show: bridge,
            },
            {
              label: t('market.token.creatorLock'),
              value: lock ? <span title={rowTimeTitle(Number(lock.until))} data-testid="token-creator-lock">{creatorLockText(lock, r.decimals, r.ticker)}</span> : null,
              show: !!lock,
            },
            { label: t('market.token.decimals'), value: r.decimals ?? '-' },
            {
              label: t('market.token.scale'),
              // The standard scale is 10^decimals, at most 10^9. A token of more than 9 decimals
              // quotes per a part of a token.
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
      {bridge ? <BridgeCustodyNote minter={minter} /> : null}
      <TokenStatusDetails row={props.checked ?? r} services={props.services} note={props.note} />
      <LogoSection row={r} />
    </div>
  );
}

/**
 * The facts of both tokens of a pair, at the bottom of the pair page in the same panel as a token
 * market's. Each token has its title with the badges and warnings, then the covenant id, decimals,
 * price scale, tick, open orders and the program.
 */
export function PairDetails(props: { rows: readonly TokenRow[]; shared?: ReadonlySet<string>; services: Pick<Services, 'registry' | 'kob'> }) {
  return (
    <div class="stack" data-testid="pair-details">
      {props.rows.map((r) => (
        <div class="stack-sm" key={r.covenantId} data-testid="pair-token-details" data-token={r.covenantId}>
          <div class="mkt-title">
            <IdentityBlock row={r} data-testid="pair-token-identity" />
            <h2 class="token-name" style="margin:0;font-size:1.05rem" data-testid="pair-token-title">{tokenTitle(r)}</h2>
          </div>
          {/* the name, the id, the program chip and the sources of the token's facts are in its details card below */}
          <div class="mkt-badges">
            <StatusBadges row={r} family={false} />
          </div>
          <TokenWarnings row={r} sharedTicker={props.shared?.has(r.covenantId) ?? false} />
          <TokenDetails row={r} services={props.services} />
        </div>
      ))}
    </div>
  );
}
