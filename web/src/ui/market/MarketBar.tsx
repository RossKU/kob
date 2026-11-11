// The common top of every market page: the market bar (one row: the token's picture and fingerprint, the pair as `BASE / QUOTE` where each side is a drop-down, the flip control, the
// star, then the 24 h figures and the live state), the frame of a market while it loads, and the page's shell. The same markup for a token's KAS market and for a token pair, so the
// page keeps its layout whichever quote is chosen.
import type { ComponentChildren } from 'preact';
import { useEffect, useMemo, useRef, useState } from 'preact/hooks';
import { t } from '../../i18n';
import { unregisteredId } from './token-model';
import { Button, Section } from '../kit';
import { AssetPicker } from './AssetPicker';
import { BookFrame } from './OrderBook';
import { openPair } from './market-nav';
import { KAS, assetChoices, type DisplayedPair } from './market-select';
import { tokenTitle } from './TokenBadges';
import { useChartExpanded } from './chart-expand';
import { RowResize } from './RowResize';
import { useMedia } from '../shell/narrow';
import type { TokenRow } from './token-model';

/** A token as an option of the selector. */
const choiceRow = (r: TokenRow) => ({ covenantId: r.covenantId, ticker: r.ticker, row: r });

/** `BASE / QUOTE`, both sides selectable. `onChange` gets the new displayed pair (market-select.ts decides where it leads). */
export function PairSelector(props: { pair: DisplayedPair; rows: readonly TokenRow[]; onChange: (next: DisplayedPair) => void }) {
  const rows = props.rows.map(choiceRow);
  const label = (r: (typeof rows)[number]) => tokenTitle(r.row);
  // a token without a ticker (the indexer's "") is named by its covenant id, like a token the list does not have
  const tickerOf = (id: string): string => (id === KAS ? 'KAS' : props.rows.find((r) => r.covenantId.toLowerCase() === id)?.ticker || unregisteredId(id));
  const { left, right } = props.pair;
  /** a side the list does not have (yet): its stand-in on the face */
  const stand = (v: string) => (v === KAS ? 'KAS' : v.slice(0, 8));
  return (
    <span class="pair-name pair-select" data-testid="market-pair" data-pair={`${tickerOf(left)}/${tickerOf(right)}`} data-left={left} data-right={right} role="group" aria-label={t('market.pair.label')}>
      <AssetPicker value={left} choices={assetChoices(rows, right, label)} rows={props.rows} label={t('market.pair.base')} placeholder={stand(left)} onChange={(v) => props.onChange({ left: v, right })} data-testid="market-base-select" />
      <span class="pair-sep" aria-hidden="true">{'/'}</span>
      <AssetPicker value={right} choices={assetChoices(rows, left, label)} rows={props.rows} label={t('market.pair.quote')} placeholder={stand(right)} onChange={(v) => props.onChange({ left, right: v })} data-testid="market-quote-select" />
    </span>
  );
}

/** The market list's way to a token pair: two tokens (no KAS: a token's KAS market is its row), and the second choice opens the pair. The choices are covenant ids. */
export function PairPicker(props: { rows: readonly TokenRow[] }) {
  const [base, setBase] = useState<string | null>(null);
  const [quote, setQuote] = useState<string | null>(null);
  const [openQuote, setOpenQuote] = useState(0);
  const all = useMemo(() => props.rows.map(choiceRow), [props.rows]);
  const choices = (other: string | null) => assetChoices(all, other ?? KAS, (r) => tokenTitle(r.row)).filter((c) => c.value !== KAS);
  const go = (left: string | null, right: string | null) => {
    if (left && right) openPair({ left, right });
  };
  return (
    <div class="ml-pair" role="group" aria-label={t('market.pick.label')} data-testid="pair-picker">
      <span class="small muted" aria-hidden="true">{t('market.pick.label')}</span>
      <AssetPicker
        value={base}
        choices={choices(quote)}
        rows={props.rows}
        label={t('market.pair.base')}
        placeholder={t('market.pick.base')}
        onChange={(v) => {
          setBase(v);
          if (quote) go(v, quote);
          else setOpenQuote((n) => n + 1);
        }}
        data-testid="pair-pick-base"
      />
      <span class="pair-sep" aria-hidden="true">{'/'}</span>
      <AssetPicker
        value={quote}
        choices={choices(base)}
        rows={props.rows}
        label={t('market.pair.quote')}
        placeholder={t('market.pick.quote')}
        openKey={openQuote}
        onChange={(v) => {
          setQuote(v);
          go(base, v);
        }}
        data-testid="pair-pick-quote"
      />
    </div>
  );
}

/**
 * The flip control, an icon-only button placed just left of the pair title: shows the market the other way round (a display flip for a token's KAS market,
 * a base/quote swap for a token pair). No visible text: the tooltip and the accessible name say "Flip pair" (`data-flip-to` names the pair it leads to).
 */
export function FlipButton(props: { pair: string; pressed?: boolean; onClick: () => void }) {
  return (
    <Button
      small
      variant="ghost"
      class="flip-btn"
      onClick={props.onClick}
      data-testid="market-flip"
      data-flip-to={props.pair}
      aria-pressed={props.pressed ? 'true' : 'false'}
      aria-label={t('market.flip')}
      title={t('market.flip')}
    >
      <svg class="flip-icon" viewBox="0 0 24 24" width="18" height="18" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true" focusable="false">
        <path d="M4 8h15" />
        <path d="M15 4l4 4-4 4" />
        <path d="M20 16H5" />
        <path d="M9 12l-4 4 4 4" />
      </svg>
    </Button>
  );
}

/**
 * The market bar (review H-02 / H-10, founder 2026-11-11): ONE row of 56 to 64 px instead of a title block and a stats card. At the left the token's picture and fingerprint (the
 * fingerprint is the anti-spoofing signal, so it stays), the pair selector with the flip control, the star (and a risk chip where a token has one); then the 24 h figures
 * (`MarketStats`: last price with its change, high, low, the two volumes, trades), then the live state. The figures scroll sideways inside the bar where they do not fit (no visible
 * scrollbar, the page never scrolls sideways); a phone puts them on a row of their own under the pair, so its first row is `[icon] TICKER / QUOTE ▾ ★ [Live]`. The covenant id, the
 * name and the program are in the token's details card, not here.
 */
export function MarketBar(props: {
  /** the token's picture and fingerprint (two of them on a pair page) */
  identity: ComponentChildren;
  /** the pair selector and the flip control */
  pair: ComponentChildren;
  /** the star and the risk chips */
  tools?: ComponentChildren;
  /** the 24 h figures (`MarketStats`) */
  stats: ComponentChildren;
  /** the live state chip */
  live?: ComponentChildren;
}) {
  return (
    <div class="mkt-bar" data-testid="market-bar">
      <div class="mkt-bar-id">
        {props.identity}
        <div class="mkt-bar-pair">{props.pair}</div>
        {props.tools}
      </div>
      <div class="mkt-bar-stats">{props.stats}</div>
      <div class="mkt-bar-live">{props.live}</div>
    </div>
  );
}

/** A market's frame while its token is looked up: the bar and the cards with their headings in their places. The ticket comes as soon as the token is known; it waits for nothing else. */
export function MarketFrame(props: { 'data-testid'?: string; bookTitle: string }) {
  const big = useChartExpanded();
  return (
    <div class="market-page" data-testid={props['data-testid']}>
      <div class="skeleton mkt-frame-bar" />
      <div class={big ? 'mkt-grid mkt-chart-big' : 'mkt-grid'}>
        <Section title={props.bookTitle} class="mkt-area-book mkt-card"><BookFrame rows={12} /></Section>
        <Section title={t('market.chart.title')} class="mkt-area-chart mkt-card"><div class="skeleton chart-box" /></Section>
        <div class="mkt-side">
          <div class="stack mkt-area-ticket">
            <Section title={t('market.ticket.title')} class="mkt-card"><div class="skeleton mkt-frame-ticket" /></Section>
          </div>
        </div>
      </div>
    </div>
  );
}

/**
 * The layout every market has: the market bar, notices, then the three columns on one row (order book with its Depth view | chart | ticket) and the facts below.
 * There is no panel of its own for the depth chart: it is the "Depth" view of the order book's panel at every width (BookTabs.tsx). A slot a market has no data for stays empty;
 * nothing moves.
 */
export function MarketShell(props: {
  'data-testid': string;
  attrs?: Record<string, string>;
  /** the market bar and what must be seen with it (warnings) */
  header: ComponentChildren;
  notices?: ComponentChildren;
  book: ComponentChildren;
  chart: ComponentChildren;
  ticket: ComponentChildren;
  /** the wallet's orders, fills and balances (OrdersView on this market): under the three columns, after the ticket on a phone */
  mine?: ComponentChildren;
  /** the activity log of the market: the full width under the grid, above the facts */
  activity?: ComponentChildren;
  details?: ComponentChildren;
}) {
  const root = useRef<HTMLDivElement>(null);
  const grid = useRef<HTMLDivElement>(null);
  // the drag handle under the row (three columns, a fine pointer): it sets the row's height (RowResize.tsx)
  const resizable = useMedia('(min-width: 1200px) and (pointer: fine)');
  // the chart's "Expand" choice (PriceChart's button): three columns, the chart takes the book's column too; otherwise the chart is taller (market.css)
  const big = useChartExpanded();
  // the Buy | Sell bar of a phone (the ticket is below the book and the chart): shown while the ticket is off screen
  const [ticketAway, setTicketAway] = useState(true);
  useEffect(() => {
    const el = root.current?.querySelector('.mkt-area-ticket');
    if (!el || typeof IntersectionObserver === 'undefined') return;
    const io = new IntersectionObserver((entries) => setTicketAway(!entries.some((e) => e.isIntersecting)));
    io.observe(el);
    return () => io.disconnect();
  }, []);
  // a press takes the ticket's side (its own switch is pressed, so the form and the ticket's state follow exactly as for a click there) and brings the ticket on screen
  const jumpToTicket = (side: 'buy' | 'sell') => {
    const area = root.current?.querySelector<HTMLElement>('.mkt-area-ticket');
    if (!area) return;
    area.querySelector<HTMLElement>(`[data-testid="order-side-${side}"]`)?.click();
    const calm = typeof matchMedia === 'function' && matchMedia('(prefers-reduced-motion: reduce)').matches;
    area.scrollIntoView({ behavior: calm ? 'auto' : 'smooth', block: 'start' });
    // the amount box takes the cursor (a phone opens its keyboard); no scroll of its own, the one above already runs
    area.querySelector<HTMLElement>('[data-testid="order-amount"]')?.focus({ preventScroll: true });
  };
  return (
    <div class="market-page" data-testid={props['data-testid']} ref={root} {...props.attrs}>
      {props.header}
      {props.notices}
      <div class={`mkt-grid${props.mine ? ' mkt-has-mine' : ''}${big ? ' mkt-chart-big' : ''}`} ref={grid}>
        {props.book}
        {props.chart}
        {/* two columns: the ticket is a column of its own beside the book (market.css); otherwise its children are grid items */}
        <div class="mkt-side">{props.ticket}</div>
        {props.mine ? <div class="mkt-area-mine">{props.mine}</div> : null}
        {resizable ? <RowResize grid={() => grid.current} /> : null}
      </div>
      {props.activity}
      {/* the facts take the full width under the grid (never a column of it, so no column is left empty beside them); DOM order = reading order */}
      {props.details ? <div class="mkt-area-details">{props.details}</div> : null}
      {/* phone only (market.css): Buy | Sell at the bottom edge, each takes the ticket's side and jumps to it; gone while the ticket is on screen */}
      {ticketAway ? (
        <div class="mkt-trade-bar" role="group" aria-label={t('market.ticket.jump')} data-testid="market-trade-bar">
          <button type="button" class="mkt-trade-btn" data-side="buy" onClick={() => jumpToTicket('buy')} data-testid="market-trade-buy">
            <span class="btn-label">{t('market.trade.buy')}</span>
          </button>
          <button type="button" class="mkt-trade-btn" data-side="sell" onClick={() => jumpToTicket('sell')} data-testid="market-trade-sell">
            <span class="btn-label">{t('market.trade.sell')}</span>
          </button>
        </div>
      ) : null}
    </div>
  );
}
