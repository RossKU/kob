import { t } from '../../i18n';
import { Badge, Banner } from '../kit';
import { longId, type TokenInfo } from '../../kob/registry';
import { hasIssuerControl, KNOWN_POWERS, shortCovenantId, tokenLabel, type TokenBadge, type TokenRow } from './token-model';

/** One status badge; `untradable` carries the reason in its text and tooltip. */
export function StatusBadge(props: { badge: TokenBadge; 'data-testid'?: string }) {
  const b = props.badge;
  const reason = b.reason ? t(`market.reason.${b.reason}`) : undefined;
  return (
    <Badge tone={b.tone} title={reason ?? (b.kind === 'unverified' ? t('market.badge.unverifiedHint') : b.kind === 'official' ? t('market.badge.officialHint') : undefined)} data-testid={props['data-testid'] ?? `badge-${b.kind}`}>
      {t(`market.badge.${b.kind}`)}
      {reason ? `: ${reason}` : ''}
    </Badge>
  );
}

export function StatusBadges(props: { row: Pick<TokenRow, 'badges' | 'source' | 'covenantId'> & { genesis?: TokenRow['genesis'] } & { info?: { family: 'kcc20' | 'kron' } | null; powers?: readonly string[] } }) {
  const r = props.row;
  return (
    <span class="row" data-testid={`token-badges-${r.covenantId}`} style="gap:4px">
      {r.info ? <Badge tone="neutral" title={t('market.badge.familyHint')} data-testid="badge-family">{t(`market.badge.family.${r.info.family}`)}</Badge> : null}
      {r.badges.map((b) => (
        <StatusBadge key={b.kind} badge={b} />
      ))}
      {r.genesis === 'verified' ? <Badge tone="info" title={t('market.badge.genesisVerifiedHint')} data-testid="badge-genesis-verified">{t('market.badge.genesis-verified')}</Badge> : null}
      {r.genesis === 'unverified' ? <Badge tone="warn" title={t('market.badge.genesisUnverifiedHint')} data-testid="badge-genesis-unverified">{t('market.badge.genesis-unverified')}</Badge> : null}
      {r.source !== 'registry' ? <Badge tone="warn" data-testid="badge-not-in-registry">{t('market.badge.notInRegistry')}</Badge> : null}
      {hasIssuerControl(r.powers) ? <Badge tone="bad" title={t('market.powers.warning')} data-testid="badge-issuer-control">{t('market.badge.issuerControl')}</Badge> : null}
    </span>
  );
}

/** `TICKER (abcd...1234) [state]` in the current language: never the ticker alone (a token outside the registry: `abcd...1234 [state]`). */
export function tokenTitle(row: Pick<TokenRow, 'ticker' | 'covenantId' | 'labelState'> & { customHash?: string | null }): string {
  return tokenLabel(row, t(`market.state.${row.labelState}`, { hash: row.customHash ?? '' }));
}

/** Translated name of one program power (an unknown power is shown as it came, already restricted to short lower-case words by the client). */
export const powerName = (p: string): string => ((KNOWN_POWERS as readonly string[]).includes(p) ? t(`market.power.${p}`) : p);

/** A token that KOB has not confirmed: known only from the indexer (open list) or a registry entry that is not verified. Tradable, with a caution next to every order. */
export const needsOpenCaution = (tk: Pick<TokenInfo, 'openList' | 'verified'>): boolean => tk.openList === true || !tk.verified;

/**
 * Caution for an unverified token (open list, or a registry entry that is not verified): not confirmed genuine, tickers collide; an open-list token also has
 * amounts in base units, a registry entry still pending review says so. Renders nothing for other tokens.
 */
export function OpenTokenCaution(props: { token: Pick<TokenInfo, 'covenantId' | 'openList' | 'verified' | 'status'> | null | undefined; 'data-testid'?: string }) {
  const tk = props.token;
  if (!tk || !needsOpenCaution(tk)) return null;
  return (
    <Banner tone="warn" title={t('market.open.title', { id: longId(tk.covenantId) })} data-testid={props['data-testid'] ?? 'open-token-caution'}>
      {t('market.open.caution')} {tk.openList ? t('market.open.baseUnits') : null}
      {tk.status === 'pending-review' ? ` ${t('market.open.pending')}` : null}
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
