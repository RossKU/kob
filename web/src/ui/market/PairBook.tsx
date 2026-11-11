// The token/token book of a pair: asks above (best at the bottom), the spread row (the token book's: the last price, then mid and spread), bids below; columns price, amount, total
// (QUOTE), like the token book's. Three sources are drawn apart, by a bar at the row's left edge and not by a word on every row (review M-09): `direct` (resting pair orders), `entry`
// (if-done pair entries resting at their limit) and `route` (`via KAS`: implied through the two KAS books in one transaction; its price in italics); the legend under the book says
// it once and a row's title names its source. A click on a level prefills the pair ticket: an ask means "buy BASE at this price", a bid "sell BASE at this price".
import { t } from '../../i18n';
import { spreadLine, type PairBookModel, type PairLevelRow } from './pair-model';

export interface PairBookProps {
  model: PairBookModel;
  base: string;
  quote: string;
  onPick?: (row: PairLevelRow) => void;
  /** slots per side (the levels the card holds: book-rows.ts; the model is built for as many) */
  rows?: number;
  /** goes on the asks' side: the page measures it to know how many levels fit */
  fitRef?: (el: HTMLElement | null) => void;
  /** the pair's last price (the two KAS markets' implied price), shown in the spread row like the token book's */
  last?: string | null;
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
        <span>{t('pair.book.price', { base: props.base, quote: props.quote })}</span>
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
