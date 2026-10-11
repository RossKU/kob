import { useServices } from '../../app/context';
import { t } from '../../i18n';
import { indexerTrust } from '../../data/indexer-crosscheck';
import { Badge } from '../kit';

/** How many indexers back the data: one (answers are not cross-checked) or several (a disagreement is shown). Settings panel; the footer shows the short form. */
export function IndexerTrust() {
  const { indexer, verifiers } = useServices();
  const trust = indexerTrust(!!indexer, verifiers?.length ?? 0);
  return (
    <p class="small" data-testid="indexer-trust" data-trust={trust}>
      <Badge tone={trust === 'multi' ? 'ok' : 'warn'}>{t(`shell.indexers.badge.${trust}`)}</Badge>{' '}
      {t(`shell.indexers.${trust}`, { count: (verifiers?.length ?? 0) + 1 })}
    </p>
  );
}
