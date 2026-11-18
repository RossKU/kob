// The token/token book of a pair. The asks are above, with the best at the bottom. The spread row
// is the token book's, with the last price, then mid and spread. The bids are below. The columns
// are price, amount and total (QUOTE), like the token book's.
//
// Three sources are drawn apart, by a bar at the row's left edge and not by a word on every row:
// - `direct`: resting pair orders.
// - `entry`: if-done pair entries resting at their limit.
// - `route`, `via KAS`: implied through the two KAS books in one transaction. Its price is in
//   italics.
// The legend under the book says it once, and a row's title names its source. A click on a level
// fills the pair ticket's price only, and the ticket keeps its side. The price is rounded for the
// clicked side.
import { t } from '../../i18n';
import { unitParts } from '../kit';
import { spreadLine, type PairBookModel, type PairLevelRow } from './pair-model';

export interface PairBookProps {
  model: PairBookModel;
  base: string;
  quote: string;
  onPick?: (row: PairLevelRow) => void;
  /** Slots per side, the levels the card holds (book-rows.ts). The model is built for as many. */
  rows?: number;
  /** Goes on the asks' side. The page measures it to know how many levels fit. */
  fitRef?: (el: HTMLElement | null) => void;
  /**
   * The pair's last price, which is the two KAS markets' implied price. It is shown in the spread
   * row like the token book's.
   */
  last?: string | null;
}

/**
 * The price as a plain number, with no thousands separators and no padding zeros. The column shows
 * fixed decimals.
 */
export const plainPrice = (text: string): string => text.replace(/,/g, '').replace(/(\.\d*?)0+$/, '$1').replace(/\.$/, '');

// The hint of a level's source. A count of orders has a `.one` and an `.other` text, and the
// route's has no count.
const HINT: Record<string, string> = { direct: 'pair.book.directHint', entry: 'pair.book.entryHint', route: 'pair.book.routeHint' };
const hintKey = (source: string, orders: number): string => {
  const key = HINT[source] ?? 'pair.book.directHint';
  return source === 'route' ? key : `${key}.${orders === 1 ? 'one' : 'other'}`;
};

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
      title={`${label} · ${t(hintKey(row.source, row.orders), { orders: row.orders })}`}
      onClick={() => p.onPick?.(row)}
    >
      <i class="book-bar" style={{ width: `${Math.round(row.bar * 100)}%` }} aria-hidden="true" />
      <span class="book-price" title={row.priceTitle ?? undefined}>{row.priceShown}</span>
      <span title={row.amountTitle ?? undefined}>{row.amountText}</span>
      <span title={row.totalTitle ?? undefined}>{row.totalText}</span>
    </button>
  );
}

export function PairBook(props: PairBookProps) {
  const m = props.model;
  const line = spreadLine(m);
  return (
    <div class="book ob pair-book tk-in" data-testid="pair-book" style={props.rows ? { '--ob-rows': String(props.rows) } : undefined}>
      <div class="ob-head" aria-hidden="true">
        <span>{unitParts(t('pair.book.price', { base: props.base, quote: props.quote }))}</span>
        <span>{t('pair.book.amount', { base: props.base })}</span>
        <span>{t('pair.book.total', { quote: props.quote })}</span>
      </div>
      <div class="book-side book-asks" ref={props.fitRef} data-testid="pair-book-asks">
        {[...m.asks].reverse().map((r, i) => (
          <Level key={`a${i}`} row={r} p={props} />
        ))}
      </div>
      {/* the token book's spread row: the last price, then mid and spread */}
      <div class="ob-mid book-mid">
        <span class="ob-last num" title={t('market.stats.last')} data-testid="pair-last">{props.last ?? '—'}</span>
        <span class="small muted" data-testid="pair-spread">
          {line.key === 'pair.book.spread' ? (
            <>
              <span class="ob-mid-price">{`${t('market.book.mid')} ${line.params.mid} · `}</span>
              {`${t('market.book.spread')} `}
              <span class="ob-spread-abs">{`${line.params.spread} (`}</span>
              {`${line.params.pct}%`}
              <span class="ob-spread-abs">)</span>
            </>
          ) : (
            t(line.key, line.params)
          )}
        </span>
      </div>
      <div class="book-side book-bids" data-testid="pair-book-bids">
        {m.bids.map((r, i) => (
          <Level key={`b${i}`} row={r} p={props} />
        ))}
      </div>
    </div>
  );
}
