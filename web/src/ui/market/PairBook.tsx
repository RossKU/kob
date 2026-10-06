// The token/token book of a pair: asks above (best at the bottom), the spread row, bids below. Three sources are drawn apart: `direct` (resting pair
// orders), `entry` (if-done pair entries resting at their limit) and `via KAS` (implied through the two KAS books in one transaction). A click on a
// level prefills the pair ticket: an ask means "buy BASE at this price", a bid "sell BASE at this price".
import { t } from '../../i18n';
import { spreadLine, type PairBookModel, type PairLevelRow } from './pair-model';

export interface PairBookProps {
  model: PairBookModel;
  base: string;
  quote: string;
  onPick?: (row: PairLevelRow) => void;
}

/** The price as a plain number: no thousands separators, no padding zeros (the column shows fixed decimals). */
export const plainPrice = (text: string): string => text.replace(/,/g, '').replace(/(\.\d*?)0+$/, '$1').replace(/\.$/, '');

const HINT: Record<string, string> = { direct: 'pair.book.directHint', entry: 'pair.book.entryHint', route: 'pair.book.routeHint' };

function Level(props: { row: PairLevelRow; p: PairBookProps }) {
  const { row, p } = props;
  const label = t(row.side === 'ask' ? 'pair.book.pickAsk' : 'pair.book.pickBid', { price: row.priceText, quote: p.quote, base: p.base });
  return (
    <button
      type="button"
      class={`book-row book-${row.side} pair-row pair-${row.source}`}
      data-testid="pair-level"
      data-side={row.side}
      data-source={row.source}
      data-price={plainPrice(row.priceText)}
      data-amount={row.amount.toString()}
      aria-label={label}
      title={`${label} · ${t(HINT[row.source] ?? 'pair.book.directHint', { orders: row.orders })}`}
      onClick={() => p.onPick?.(row)}
    >
      <i class="book-bar" style={{ width: `${Math.round(row.bar * 100)}%` }} aria-hidden="true" />
      <span class="book-price">{row.priceText}</span>
      <span>{row.amountText}</span>
      <span class="pair-src">
        {row.source === 'route' ? (
          <span class="pair-via" data-testid="pair-via-kas">{t('pair.book.viaKas')}</span>
        ) : row.source === 'entry' ? (
          <span class="pair-direct pair-entry-tag" data-testid="pair-entry">{t('pair.book.entry', { orders: row.orders })}</span>
        ) : (
          <span class="pair-direct">{t('pair.book.direct', { orders: row.orders })}</span>
        )}
      </span>
    </button>
  );
}

export function PairBook(props: PairBookProps) {
  const m = props.model;
  const line = spreadLine(m);
  return (
    <div class="book ob pair-book" data-testid="pair-book">
      <div class="ob-head" aria-hidden="true">
        <span>{t('pair.book.price', { quote: props.quote })}</span>
        <span>{t('pair.book.amount', { base: props.base })}</span>
        <span>{t('pair.book.source')}</span>
      </div>
      <div class="book-side book-asks" data-testid="pair-book-asks">
        {[...m.asks].reverse().map((r, i) => (
          <Level key={`a${i}`} row={r} p={props} />
        ))}
      </div>
      <div class="ob-mid book-mid small muted" data-testid="pair-spread">
        {t(line.key, line.params)}
      </div>
      <div class="book-side book-bids" data-testid="pair-book-bids">
        {m.bids.map((r, i) => (
          <Level key={`b${i}`} row={r} p={props} />
        ))}
      </div>
    </div>
  );
}
