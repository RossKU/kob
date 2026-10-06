import { useServices } from '../../app/context';
import { crossCheckIndexers } from '../../data/indexer-crosscheck';
import { t } from '../../i18n';
import type { Hex } from '../../kob/types';
import { Banner, useAsync } from '../kit';

/**
 * With two or more indexers configured (`extraIndexerUrls`) the key facts of the token (standing, powers, best ask / bid) are read from each and
 * compared; any disagreement or unreachable verifier is shown. With a single indexer nothing is checked here (the footer says "single indexer,
 * unverified"). `refreshKey` re-runs the check when the book moved.
 */
export function CrossCheckBanner(props: { token: Hex; refreshKey?: number }) {
  const { indexer, verifiers } = useServices();
  const res = useAsync(
    async (signal) => (indexer && verifiers?.length ? crossCheckIndexers(indexer, verifiers, props.token, signal) : null),
    [indexer, verifiers, props.token, props.refreshKey ?? 0],
  );
  const r = res.data;
  if (!r || r.findings.length === 0) return null;
  return (
    <Banner tone="error" title={t('market.crosscheck.title')} data-testid="indexer-disagreement">
      <p>{t('market.crosscheck.lead')}</p>
      <ul>
        {r.findings.map((f, i) => (
          <li key={i} data-code={f.code}>
            {t(`market.crosscheck.${f.code}`, { other: f.other, primary: f.primary ?? '', otherValue: f.otherValue ?? '' })}
          </li>
        ))}
      </ul>
    </Banner>
  );
}
