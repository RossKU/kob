import { useServices } from '../../app/context';
import { crossCheckIndexers } from '../../data/indexer-crosscheck';
import { t } from '../../i18n';
import type { Hex } from '../../kob/types';
import { Banner, useAsync } from '../kit';
import { useFailoverState } from '../shell/failover';

/**
 * With two or more indexers configured (`extraIndexerUrls`) the key facts of the token are read
 * from each and compared. They are the powers, the market price scale and the best ask and bid.
 * Any disagreement or unreachable verifier is shown. With a single indexer nothing is checked
 * here, and the footer says "single indexer, unverified". `refreshKey` re-runs the check when the
 * book moved. The checks are coalesced, so there are never two in flight.
 */
const shown = (code: string, v: string | null | undefined): string => (v == null ? '' : code === 'market-scale-differs' && /^10*$/.test(v) ? String(v.length - 1) : v);

export function CrossCheckBanner(props: { token: Hex; refreshKey?: number }) {
  const services = useServices();
  const { indexer } = services;
  // On a backup indexer the primary, while it answers, and the other further indexers are the
  // references. The set changes with the failover state.
  const fo = useFailoverState(services.failover);
  const verifiers = services.verifiers;
  const res = useAsync(
    async (signal) => (indexer && verifiers?.length ? crossCheckIndexers(indexer, verifiers, props.token, signal) : null),
    [indexer, verifiers, fo?.epoch, props.token],
    { refreshKey: props.refreshKey ?? 0 },
  );
  const r = res.data;
  if (!r || r.findings.length === 0) return null;
  return (
    <Banner tone="error" title={t('market.crosscheck.title')} data-testid="indexer-disagreement">
      <p>{t('market.crosscheck.lead')}</p>
      <ul>
        {r.findings.map((f, i) => (
          <li key={i} data-code={f.code}>
            {t(`market.crosscheck.${f.code}`, { other: f.other, primary: shown(f.code, f.primary), otherValue: shown(f.code, f.otherValue) })}
          </li>
        ))}
      </ul>
    </Banner>
  );
}
