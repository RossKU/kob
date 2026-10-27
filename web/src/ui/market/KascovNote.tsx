// The small muted notes of a row whose words came from kascov.io (in the style of TokenNote): where the name and ticker come from, and a disagreement
// with what KOB derived. Nothing for a row kascov added nothing visible to.
import { t } from '../../i18n';
import type { TokenRow } from './token-model';

export function KascovNote(props: { row: Pick<TokenRow, 'covenantId' | 'ticker' | 'decimals' | 'kascov'> }) {
  const { covenantId, decimals, kascov: k, ticker } = props.row;
  if (!k) return null;
  const what = k.usedName && k.usedTicker ? 'nameTicker' : k.usedName ? 'name' : k.usedTicker ? 'ticker' : null;
  return (
    <>
      {what ? (
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
