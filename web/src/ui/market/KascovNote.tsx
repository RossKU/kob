// The small muted notes of a row whose words came from kascov.io (in the style of TokenNote): where the name and ticker come from, and a disagreement
// with what KOB derived (short warn badges, see KascovMismatch). Nothing for a row kascov added nothing visible to.
import { t } from '../../i18n';
import { TipBadge } from './TipBadge';
import type { KascovPriceNote } from './kascov-price';
import type { TokenRow } from './token-model';

/** `compact` (list rows): the source is the small marker "via kascov", the sentence is its tooltip and the legend above the list says it once. */
export function KascovNote(props: { row: Pick<TokenRow, 'covenantId' | 'ticker' | 'decimals' | 'kascov'>; compact?: boolean }) {
  const { covenantId, kascov: k } = props.row;
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
      {props.compact ? null : <KascovMismatch row={props.row} />}
    </>
  );
}

/**
 * The one muted third-party line under a market's bar: kascov.io lists a price that differs from KOB's own by more than 20% (kascov-price.ts; nothing renders without a listed
 * price or when the two agree). A tooltip says it is an outside reference and KOB's prices come from its own book.
 */
export function KascovPrice(props: { note: KascovPriceNote | null }) {
  const n = props.note;
  if (!n) return null;
  return (
    <p class="small muted kascov-price" title={n.title} data-testid="kascov-price-note" data-percent={n.percent}>
      {n.text}
    </p>
  );
}

/** Whether kascov.io disagrees with KOB about the ticker or the decimals of a row (what `KascovMismatch` shows). */
export const hasKascovMismatch = (row: Pick<TokenRow, 'kascov'>): boolean => !!row.kascov && (row.kascov.tickerDiffers || row.kascov.decimalsDiffers);

/**
 * The disagreements with KOB as short warn badges: "ticker differs", "decimals differ". The full sentence, with kascov's value and KOB's, is each badge's
 * tooltip and, on a tap or the keyboard, a small panel (TipBadge). A list row shows them on its badge line (the name line cuts what does not fit).
 */
export function KascovMismatch(props: { row: Pick<TokenRow, 'covenantId' | 'ticker' | 'decimals' | 'kascov'> }) {
  const { covenantId, decimals, kascov: k, ticker } = props.row;
  if (!k) return null;
  return (
    <>
      {k.tickerDiffers ? (
        <TipBadge
          label={t('market.kascov.tickerDiffers')}
          detail={t('market.kascov.tickerDiffersTitle', { kascov: k.ticker ?? '', kob: ticker })}
          data-testid={`kascov-ticker-differs-${covenantId}`}
          data-detail-testid={`kascov-ticker-differs-detail-${covenantId}`}
        />
      ) : null}
      {k.decimalsDiffers ? (
        <TipBadge
          label={t('market.kascov.decimalsDiffers')}
          detail={t('market.kascov.decimalsDiffersTitle', { kascov: k.decimals ?? '', kob: decimals ?? '' })}
          data-testid={`kascov-decimals-differ-${covenantId}`}
          data-detail-testid={`kascov-decimals-differ-detail-${covenantId}`}
        />
      ) : null}
    </>
  );
}
