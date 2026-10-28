// The small muted notes of a row whose words came from kascov.io (in the style of TokenNote): where the name and ticker come from, and a disagreement
// with what KOB derived. Nothing for a row kascov added nothing visible to.
import { t } from '../../i18n';
import type { TokenRow } from './token-model';

/** `compact` (list rows): the source is the small marker "via kascov", the sentence is its tooltip and the legend above the list says it once. */
export function KascovNote(props: { row: Pick<TokenRow, 'covenantId' | 'ticker' | 'decimals' | 'kascov'>; compact?: boolean }) {
  const { covenantId, decimals, kascov: k, ticker } = props.row;
  if (!k) return null;
  const what = k.usedName && k.usedTicker ? 'nameTicker' : k.usedName ? 'name' : k.usedTicker ? 'ticker' : null;
  return (
    <>
      {what && props.compact ? (
        <span class="ml-via small muted" title={t(`market.kascov.source.${what}`)} data-testid={`kascov-source-${covenantId}`} data-what={what}>
          {t('market.kascov.via')}
        </span>
      ) : what ? (
        <span class="small muted token-note" title={t('market.kascov.sourceTitle')} data-testid={`kascov-source-${covenantId}`} data-what={what}>
          {' '}
          {t(`market.kascov.source.${what}`)}
        </span>
      ) : null}
      {k.tickerDiffers ? (
        <span class="small muted token-note" title={t('market.kascov.tickerDiffersTitle', { kascov: k.ticker ?? '', kob: ticker })} data-testid={`kascov-ticker-differs-${covenantId}`}>
          {' '}
          ({t('market.kascov.tickerDiffers', { ticker: k.ticker ?? '' })})
        </span>
      ) : null}
      {k.decimalsDiffers ? (
        <span class="small muted token-note" title={t('market.kascov.decimalsDiffersTitle', { kascov: k.decimals ?? '', kob: decimals ?? '' })} data-testid={`kascov-decimals-differ-${covenantId}`}>
          {' '}
          ({t('market.kascov.decimalsDiffers', { kascov: k.decimals ?? '', kob: decimals ?? '' })})
        </span>
      ) : null}
    </>
  );
}
