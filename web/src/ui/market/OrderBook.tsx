import { t } from '../../i18n';
import { tapeTime } from './market-model';
import type { BookRowView, BookView } from './orientation';

/** The subtle marker of a book that is not current: "stale since hh:mm:ss" (the time of the last good book on screen). Nothing when `since` is null. */
export function StaleSince(props: { since: number | null; 'data-testid'?: string }) {
  if (props.since === null) return null;
  return (
    <p class="small muted book-stale-since" role="status" title={t('market.book.staleSinceHint')} data-testid={props['data-testid'] ?? 'book-stale-since'}>
      {t('market.book.staleSince', { time: tapeTime(props.since) })}
    </p>
  );
}

export interface OrderBookProps {
  /** the book as drawn, in the shown orientation (orientation.ts) */
  view: BookView;
  /** rows reserved per side (the panel keeps its height while levels come and go) */
  rows: number;
  /** last trade price text and aggressor side, shown in the spread row */
  last?: { text: string; side: 'buy' | 'sell' | null } | null;
  /**
   * a click on a level prefills the ticket in NATIVE terms (what the order is): `buy` / `sell` the token at this price per whole token. Whichever
   * orientation is shown, the same level prefills the same order.
   */
  onPick?: (side: 'buy' | 'sell', price: bigint) => void;
  /** shown as KAS/TOKEN: the notes speak of KAS as the base asset */
  inverted?: boolean;
  /** display name of the token (the base asset of the native view) */
  token?: string;
}

const estimatedNote = (inverted: boolean | undefined, token: string | undefined): string => (inverted ? t('market.book.estimatedNoteInv', { ticker: token ?? '' }) : t('market.book.estimatedNote'));

function Row(props: { row: BookRowView; kind: 'ask' | 'bid'; onPick: OrderBookProps['onPick']; inverted?: boolean; token?: string }) {
  const { row, kind } = props;
  const label = t(kind === 'ask' ? 'market.book.pickAsk' : 'market.book.pickBid', { price: row.priceText });
  return (
    <button
      type="button"
      class={`book-row book-${kind}`}
      data-testid={`book-${kind}-row`}
      data-price={row.priceValue}
      data-amount={row.amount.toString()}
      data-orders={row.orders}
      data-native-price={row.nativePrice.toString()}
      aria-label={label}
      title={`${label} · ${t('market.book.ordersCount', { orders: row.orders })}`}
      onClick={() => props.onPick?.(row.pick, row.nativePrice)}
    >
      <i class="book-bar" style={{ width: `${row.depthPct}%` }} aria-hidden="true" />
      <span class="book-price">{row.priceText}</span>
      <span>
        {row.estimated ? <span class="est" title={estimatedNote(props.inverted, props.token)}>~</span> : null}
        {row.sizeText}
      </span>
      <span title={row.totalTitle}>{row.totalText}</span>
    </button>
  );
}

/** Aggregated order book: asks above (best at the bottom), the spread row with the last price, bids below; cumulative depth bars; price grouping. */
export function OrderBook(props: OrderBookProps) {
  const m = props.view;
  return (
    <div class="book ob" data-testid="order-book" style={{ '--ob-rows': String(props.rows) }}>
      <div class="ob-head" aria-hidden="true">
        <span>{m.head.price}</span>
        <span>{m.head.size}</span>
        <span>{m.head.total}</span>
      </div>
      <div class="book-side" data-testid="book-asks" role="list" aria-label={t('market.book.asks')}>
        {m.asks.length ? m.asks.map((r) => <Row key={r.key} row={r} kind="ask" onPick={props.onPick} inverted={props.inverted} token={props.token} />) : <div class="muted small center">{t('market.book.noAsks')}</div>}
      </div>
      <div class="ob-mid book-mid" data-testid="book-spread">
        <span class={`ob-last num ${props.last?.side === 'buy' ? 'buy' : props.last?.side === 'sell' ? 'sell' : ''}`} title={t('market.stats.last')} data-testid="book-last">
          {props.last ? props.last.text : '—'}
        </span>
        <span class="small muted">
          {t('market.book.mid')}{' '}
          <strong class="num" data-testid="book-mid" data-value={m.mid?.value ?? ''}>
            {m.mid ? m.mid.text : '—'}
          </strong>
          {' · '}
          {t('market.book.spread')} <strong class="num">{m.spread ? m.spread.text : '—'}</strong>
          {m.spread?.pct ? ` (${m.spread.pct}%)` : null}
        </span>
      </div>
      <div class="book-side" data-testid="book-bids" role="list" aria-label={t('market.book.bids')}>
        {m.bids.length ? m.bids.map((r) => <Row key={r.key} row={r} kind="bid" onPick={props.onPick} inverted={props.inverted} token={props.token} />) : <div class="muted small center">{t('market.book.noBids')}</div>}
      </div>
      <p class="ob-foot">
        {m.anyEstimated ? <>{`~ ${estimatedNote(props.inverted, props.token)} `}</> : null}
        {t('market.book.clickHint', { base: props.inverted ? 'KAS' : (props.token ?? '') })}
      </p>
    </div>
  );
}

/** The grouping selector of the book header: `options` are the steps of the shown orientation (native: KAS per token; inverted: tokens per KAS). */
export function BookGrouping(props: { options: { value: string; label: string }[]; value: string; onChange: (value: string) => void }) {
  return (
    <label class="ob-tools">
      <span class="sr-only">{t('market.book.groupLabel')}</span>
      <select
        class="select"
        data-testid="book-group"
        aria-label={t('market.book.groupLabel')}
        title={t('market.book.groupLabel')}
        value={props.value}
        onChange={(e) => props.onChange((e.currentTarget as HTMLSelectElement).value)}
      >
        {props.options.map((o) => (
          <option key={o.value} value={o.value}>
            {o.label}
          </option>
        ))}
      </select>
    </label>
  );
}
