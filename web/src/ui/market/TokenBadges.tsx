import { t } from '../../i18n';
import { Badge, Banner } from '../kit';
import { longId, type TokenInfo } from '../../kob/registry';
import { hasIssuerControl, KNOWN_POWERS, tokenLabel, type TokenBadge, type TokenRow } from './token-model';

/** One status badge: `delisted`, or `untradable` with the reason in its text and tooltip. */
export function StatusBadge(props: { badge: TokenBadge; 'data-testid'?: string }) {
  const b = props.badge;
  const reason = b.reason ? t(`market.reason.${b.reason}`) : undefined;
  return (
    <Badge tone={b.tone} title={reason} data-testid={props['data-testid'] ?? `badge-${b.kind}`}>
      {t(`market.badge.${b.kind}`)}
      {reason ? `: ${reason}` : ''}
    </Badge>
  );
}

export function StatusBadges(props: { row: Pick<TokenRow, 'badges' | 'covenantId'> & { info?: { family: 'kcc20' | 'kron' } | null; powers?: readonly string[] } }) {
  const r = props.row;
  return (
    <span class="row" data-testid={`token-badges-${r.covenantId}`} style="gap:4px">
      {r.info ? <Badge tone="neutral" title={t('market.badge.familyHint')} data-testid="badge-family">{t(`market.badge.family.${r.info.family}`)}</Badge> : null}
      {r.badges.map((b) => (
        <StatusBadge key={b.kind} badge={b} />
      ))}
      {hasIssuerControl(r.powers) ? <Badge tone="bad" title={t('market.powers.warning')} data-testid="badge-issuer-control">{t('market.badge.issuerControl')}</Badge> : null}
    </span>
  );
}

/** `TICKER (abcd...1234)`: never the ticker alone (a token outside the registry: `abcd...1234`). */
export function tokenTitle(row: Pick<TokenRow, 'ticker' | 'covenantId'> & { info?: TokenInfo | null }): string {
  return tokenLabel(row);
}

/** Translated name of one program power (an unknown power is shown as it came, already restricted to short lower-case words by the client). */
export const powerName = (p: string): string => ((KNOWN_POWERS as readonly string[]).includes(p) ? t(`market.power.${p}`) : p);

/**
 * Short note for a token whose ticker another listed token uses too: tickers are not unique, the covenant id is the identity. `block` renders a
 * paragraph (token page header, ticket), otherwise an inline line (a list row). Renders nothing for other tokens.
 */
export function SharedTickerNote(props: { shared: boolean; block?: boolean; 'data-testid'?: string }) {
  if (!props.shared) return null;
  const testid = props['data-testid'] ?? 'shared-ticker-note';
  return props.block ? (
    <p class="small" style="margin:0" role="note" data-testid={testid}>{t('market.sharedTicker')}</p>
  ) : (
    <div class="small muted" role="note" data-testid={testid}>{t('market.sharedTicker')}</div>
  );
}

/**
 * Notes for a token found through the indexer only (open list): its amounts are in base units, and the covenant id is all that identifies it. Shows the
 * full covenant id. Renders nothing for a registry token.
 */
export function OpenTokenNote(props: { token: (Pick<TokenInfo, 'covenantId' | 'openList'> & Partial<Pick<TokenInfo, 'openScale' | 'decimals'>>) | null | undefined; 'data-testid'?: string }) {
  const tk = props.token;
  if (!tk || tk.openList !== true) return null;
  return (
    <Banner tone="info" title={t('market.open.title', { id: longId(tk.covenantId) })} data-testid={props['data-testid'] ?? 'open-token-caution'}>
      {tk.openScale === 'entered' ? t('market.open.enteredScale', { decimals: tk.decimals ?? 0 }) : t('market.open.baseUnits')}
      <div class="small wrap-anywhere" data-testid="open-token-full-id">
        {t('market.open.fullId')} <code>{tk.covenantId}</code>
      </div>
    </Banner>
  );
}

/** The program can freeze or seize balances: escrowed orders can fail or be lost at the issuer's discretion. Renders nothing for other tokens. */
export function PowersWarning(props: { powers: readonly string[] | null | undefined; 'data-testid'?: string }) {
  if (!hasIssuerControl(props.powers)) return null;
  return (
    <Banner tone="warn" title={t('market.powers.title')} data-testid={props['data-testid'] ?? 'powers-warning'}>
      {t('market.powers.warning')}
    </Banner>
  );
}
