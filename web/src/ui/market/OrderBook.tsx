import { t } from '../../i18n';
import { Dropdown } from '../kit';
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
      translate={false}
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
      <span class="book-price" title={row.priceTitle ?? undefined}>{row.priceShown}</span>
      <span title={row.sizeTitle ?? undefined}>
        {row.estimated ? <span class="est" title={estimatedNote(props.inverted, props.token)}>~</span> : null}
        {row.sizeText}
      </span>
      <span title={row.totalTitle}>{row.totalText}</span>
    </button>
  );
}

/** An empty level slot: the same height as a level, so the block of a side never changes size when levels come and go. */
function Blank(props: { kind: 'ask' | 'bid' }) {
  return <div class={`book-row book-blank book-${props.kind}`} aria-hidden="true" data-testid={`ob-blank-${props.kind}`} />;
}

/** The book's frame while its rows are held: the column titles (none until the token is known) and the rows' places, as high as the book, so nothing moves when the rows come. */
export function BookFrame(props: { head?: { price: string; size: string; total: string }; rows: number; 'data-testid'?: string }) {
  const side = Array.from({ length: props.rows }, (_, i) => (
    <div key={i} class="book-row book-blank" aria-hidden="true">
      <span class="skeleton ob-skel" />
      <span class="skeleton ob-skel" />
      <span class="skeleton ob-skel" />
    </div>
  ));
  return (
    <div class="book ob" role="status" aria-label={t('common.loading')} data-testid={props['data-testid']} style={{ '--ob-rows': String(props.rows) }}>
      <div class="ob-head" aria-hidden="true">
        <span>{props.head?.price}</span>
        <span>{props.head?.size}</span>
        <span>{props.head?.total}</span>
      </div>
      <div class="book-side">{side}</div>
      <div class="ob-mid book-mid" />
      <div class="book-side">{side}</div>
      <p class="ob-foot" />
    </div>
  );
}

/**
 * Aggregated order book: asks above (best at the bottom), the spread row with the last price, bids below; cumulative depth bars; price grouping. Each side
 * is exactly `rows` slots (levels, then blank slots: the asks are bottom-anchored, the bids top-anchored), the header, the spread row and the foot have
 * fixed heights: nothing in the book moves when a level appears, goes or changes its text.
 */
export function OrderBook(props: OrderBookProps) {
  const m = props.view;
  const blanksOf = (n: number) => Math.max(0, props.rows - n);
  return (
    <div class="book ob tk-in" data-testid="order-book" style={{ '--ob-rows': String(props.rows) }}>
      <div class="ob-head" aria-hidden="true">
        <span>{m.head.price}</span>
        <span>{m.head.size}</span>
        <span>{m.head.total}</span>
      </div>
      <div class="book-side" data-testid="book-asks" role="list" aria-label={t('market.book.asks')}>
        {Array.from({ length: blanksOf(m.asks.length) }, (_, i) => <Blank key={`ask-blank-${i}`} kind="ask" />)}
        {m.asks.map((r) => <Row key={r.key} row={r} kind="ask" onPick={props.onPick} inverted={props.inverted} token={props.token} />)}
        {m.asks.length ? null : <div class="ob-none muted small">{t('market.book.noAsks')}</div>}
      </div>
      <div class="ob-mid book-mid" data-testid="book-spread">
        <span class={`ob-last num ${props.last?.side === 'buy' ? 'buy' : props.last?.side === 'sell' ? 'sell' : ''}`} title={t('market.stats.last')} data-testid="book-last">
          {props.last ? props.last.text : '—'}
        </span>
        <span class="small muted">
          <span class="ob-mid-price">
            {t('market.book.mid')}{' '}
            <strong class="num" data-testid="book-mid" data-value={m.mid?.value ?? ''}>
              {m.mid ? m.mid.text : '—'}
            </strong>
            {' · '}
          </span>
          {t('market.book.spread')}{' '}
          {/* a narrow book keeps the percentage: it reads the same at every price */}
          {m.spread?.pct ? (
            <>
              <span class="ob-spread-abs"><strong class="num">{m.spread.text}</strong>{' ('}</span>
              <strong class="num">{`${m.spread.pct}%`}</strong>
              <span class="ob-spread-abs">)</span>
            </>
          ) : (
            <strong class="num">{m.spread ? m.spread.text : '—'}</strong>
          )}
        </span>
      </div>
      <div class="book-side" data-testid="book-bids" role="list" aria-label={t('market.book.bids')}>
        {m.bids.map((r) => <Row key={r.key} row={r} kind="bid" onPick={props.onPick} inverted={props.inverted} token={props.token} />)}
        {Array.from({ length: blanksOf(m.bids.length) }, (_, i) => <Blank key={`bid-blank-${i}`} kind="bid" />)}
        {m.bids.length ? null : <div class="ob-none muted small">{t('market.book.noBids')}</div>}
      </div>
      <p class="ob-foot">
        {m.anyEstimated ? <span>{`~ ${estimatedNote(props.inverted, props.token)} `}</span> : null}
        <span>{t('market.book.clickHint', { base: props.inverted ? 'KAS' : (props.token ?? '') })}</span>
      </p>
    </div>
  );
}

/** The grouping selector of the book header: `options` are the steps of the shown orientation (native: KAS per token; inverted: tokens per KAS). */
export function BookGrouping(props: { options: { value: string; label: string }[]; value: string; onChange: (value: string) => void }) {
  return (
    <div class="ob-tools">
      <Dropdown label={t('market.book.groupLabel')} title={t('market.book.groupLabel')} value={props.value} options={props.options} onChange={props.onChange} data-testid="book-group" />
    </div>
  );
}
