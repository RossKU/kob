import { useState } from 'preact/hooks';
import { t } from '../../i18n';
import { Dropdown, unitParts } from '../kit';
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
  /** slots per side: the levels, then blank slots (the panel keeps its height while levels come and go); the page's measure of what fits the card (book-rows.ts) */
  rows: number;
  /** last trade price text and aggressor side, shown in the spread row */
  last?: { text: string; side: 'buy' | 'sell' | null } | null;
  /**
   * a click on a level prefills the ticket in NATIVE terms (what the order is): `buy` / `sell` the token at this price per whole token. Whichever
   * orientation is shown, the same level prefills the same order.
   */
  onPick?: (side: 'buy' | 'sell', price: bigint) => void;
  /** goes on the asks' side: the page measures it to know how many levels fit (book-rows.ts) */
  fitRef?: (el: HTMLElement | null) => void;
  /** shown as KAS/TOKEN: the notes speak of KAS as the base asset */
  inverted?: boolean;
  /** display name of the token (the base asset of the native view) */
  token?: string;
}

const estimatedNote = (inverted: boolean | undefined, token: string | undefined): string => (inverted ? t('market.book.estimatedNoteInv', { ticker: token ?? '' }) : t('market.book.estimatedNote'));

/** The level the pointer (or the keyboard focus) is on: its side, its place in that side's list and where its row is in the book (for the read-out). */
interface Hover { kind: 'ask' | 'bid'; index: number; top: number }

function Row(props: { row: BookRowView; kind: 'ask' | 'bid'; index: number; hot: boolean; onPick: OrderBookProps['onPick']; onHover: (h: Hover | null, row?: BookRowView) => void; inverted?: boolean; token?: string }) {
  const { row, kind } = props;
  const label = t(kind === 'ask' ? 'market.book.pickAsk' : 'market.book.pickBid', { price: row.priceText });
  const enter = (e: Event) => {
    const el = e.currentTarget as HTMLElement;
    const book = el.closest('.book');
    if (!book) return;
    props.onHover({ kind, index: props.index, top: el.getBoundingClientRect().top - book.getBoundingClientRect().top }, row);
  };
  return (
    <button
      type="button"
      class={`book-row book-${kind}${props.hot ? ' is-cum' : ''}`}
      onMouseEnter={enter}
      onFocus={enter}
      onMouseLeave={() => props.onHover(null)}
      onBlur={() => props.onHover(null)}
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
export function BookFrame(props: { head?: { price: string; size: string; total: string }; rows: number; fitRef?: (el: HTMLElement | null) => void; 'data-testid'?: string }) {
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
        <span>{props.head ? unitParts(props.head.price) : null}</span>
        <span>{props.head?.size}</span>
        <span>{props.head?.total}</span>
      </div>
      <div class="book-side" ref={props.fitRef}>{side}</div>
      <div class="ob-mid book-mid" />
      <div class="book-side">{side}</div>
      <p class="ob-foot" />
    </div>
  );
}

/**
 * Aggregated order book: asks above (best at the bottom), the spread row with the last price, bids below; cumulative depth bars; price grouping. Each side
 * is exactly `rows` slots (as many as fit the card from 760 px, 12 on a phone: book-rows.ts) (levels, then blank slots: the asks are bottom-anchored, the bids top-anchored), the header, the spread row and the foot have
 * fixed heights: nothing in the book moves when a level appears, goes or changes its text.
 */
export function OrderBook(props: OrderBookProps) {
  const m = props.view;
  const blanksOf = (n: number) => Math.max(0, props.rows - n);
  // the hover read-out (review M-17): every level from the best price to the one under the pointer is tinted, and a small line says what taking them all would be (average price,
  // amount, value). Drawn over the book (absolute, no pointer events), so nothing moves.
  const [hover, setHover] = useState<(Hover & { row: BookRowView }) | null>(null);
  const onHover = (h: Hover | null, row?: BookRowView) => setHover(h && row ? { ...h, row } : null);
  // asks are listed best-last: the stretch from the best ask is the hovered row and everything below it; bids best-first: the hovered row and everything above it
  const hot = (kind: 'ask' | 'bid', i: number): boolean => !!hover && hover.kind === kind && (kind === 'ask' ? i >= hover.index : i <= hover.index);
  const cum = hover?.row.cum;
  return (
    <div class="book ob tk-in" data-testid="order-book" style={{ '--ob-rows': String(props.rows) }}>
      <div class="ob-head" aria-hidden="true">
        <span>{unitParts(m.head.price)}</span>
        <span>{m.head.size}</span>
        <span>{m.head.total}</span>
      </div>
      <div class="book-side" ref={props.fitRef} data-testid="book-asks" role="list" aria-label={t('market.book.asks')}>
        {Array.from({ length: blanksOf(m.asks.length) }, (_, i) => <Blank key={`ask-blank-${i}`} kind="ask" />)}
        {m.asks.map((r, i) => <Row key={r.key} row={r} kind="ask" index={i} hot={hot('ask', i)} onHover={onHover} onPick={props.onPick} inverted={props.inverted} token={props.token} />)}
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
        {m.bids.map((r, i) => <Row key={r.key} row={r} kind="bid" index={i} hot={hot('bid', i)} onHover={onHover} onPick={props.onPick} inverted={props.inverted} token={props.token} />)}
        {Array.from({ length: blanksOf(m.bids.length) }, (_, i) => <Blank key={`bid-blank-${i}`} kind="bid" />)}
        {m.bids.length ? null : <div class="ob-none muted small">{t('market.book.noBids')}</div>}
      </div>
      {/* one short line; the whole sentences are its title */}
      <p class="ob-foot" title={`${m.anyEstimated ? `~ ${estimatedNote(props.inverted, props.token)} ` : ''}${t('market.book.clickHint', { base: props.inverted ? 'KAS' : (props.token ?? '') })}`} data-testid="book-foot">
        {m.anyEstimated ? `~ ${t('market.book.footEstimated')} · ` : ''}
        {t('market.book.footClick')}
      </p>
      {hover && cum ? (
        <div class="ob-hover-tip" role="status" style={{ top: `${hover.top < 44 ? hover.top + 24 : hover.top - 30}px` }} data-testid="book-hover-tip">
          {t(props.inverted ? 'market.book.hoverInv' : 'market.book.hover', { avg: cum.avg, amount: cum.amount, total: cum.total, token: props.token ?? '' })}
        </div>
      ) : null}
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
