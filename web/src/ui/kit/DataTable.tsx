// The one table of the Portfolio and of a market page's panel. It has a header row with sortable
// column titles, rows of cells, and an expandable details area under a row. Where the table cannot
// be a table, the same rows are cards of label / value pairs. Balances, Open orders, Order history
// and Fills are all drawn with these parts, so they share one header style, one row height and
// one cell grammar. Numbers are right-aligned in tabular figures with the unit small and muted, a
// market is its icon and ticker, a side is in its colour, and a status is a chip. Sorting is the
// shared helper (sort.ts, SortHeader.tsx).
//
// The structure is decided by FIT, in one fixed order (`foldTable`).
// - The full table (fold 0).
// - The table folded: fold 1 has tighter columns, the Copy button as an icon and the time on two
//   lines, and fold 2 puts the value / total under the amount.
// - Cards.
//
// A phone (below 640 px, PHONE_QUERY) is always cards, and a card has no sort control there. The
// result depends only on the table's width, the fonts and what the rows say, never on what the
// table was before. It is measured again from fold 0 whenever one of them changes, such as a
// resize, the fonts loading, or rows arriving or changing. The same width therefore always gives
// the same structure.
import type { ComponentChildren, JSX } from 'preact';
import { useEffect, useLayoutEffect, useRef, useState } from 'preact/hooks';
import { t } from '../../i18n';
import { kasValueText, kasValueTitle } from '../../app/fiat';
import { useFiat } from '../../app/use-fiat';
import { formatKas } from '../../kob/units';
import { formatRowTimeShortUnix, formatRowTimeUnix, rowTimeTitleUnix } from './format';
import { PHONE_QUERY, useMedia } from '../shell/narrow';
import { SortButton, SortControl, ariaSortOf } from './SortHeader';
import type { SortColumn, SortState } from './sort';
import './data-table.css';

/**
 * How a column shows in a card.
 * - Balances use the `head` (the asset, line 1), its value at the right of it (`end`), label /
 *   value `pair`s (line 2, the words drawn before each figure) and the buttons (`action`).
 * - The order and fill lists (compact cards, data-table.css) use the `title` (line 1, the market
 *   and the side) and the `badge` (line 1, at the right, the time). The `pair`s (line 2) are the
 *   figures in the table's order, with no captions drawn, and each keeps its column's words as its
 *   accessible name. The `meter` (line 3) is the filled part. The `status` chip and the buttons
 *   (`action`, line 3 at the right) follow.
 * - The chevron is `toggle`. A column that is not shown is `hidden`, because it is in the details.
 */
export type DtCard = 'head' | 'end' | 'title' | 'badge' | 'pair' | 'wide' | 'meter' | 'status' | 'action' | 'toggle' | 'hidden';

export interface DtColumn<K extends string = string> extends SortColumn<K> {
  /** Numbers (amounts, prices, times of a column of figures) are right-aligned in the table. */
  right?: boolean;
  card?: DtCard;
}

/**
 * A column with no title, such as the buttons or the chevron. It takes its place in the grid and
 * nothing more.
 */
export interface DtPlain {
  key: string;
  plain: true;
}

/**
 * The folds of a table, in order (data-table.css). Fold 0 is the full table. Fold 1 is tighter,
 * with narrower columns, the Copy button as an icon and the time on two lines. Fold 2 puts the
 * value / total under the amount.
 */
const LAST_FOLD = 2;

const spills = (e: HTMLElement): boolean => e.scrollWidth > e.clientWidth + 1;

/**
 * Whether the table, as it is drawn now, holds its content. That means no row is wider than the
 * table (the columns' minimum widths together) and no cell is cut off.
 */
function holdsContent(el: HTMLElement): boolean {
  const head = el.querySelector<HTMLElement>('.dt-head');
  if (head && !head.hidden && spills(head)) return false;
  for (const row of Array.from(el.querySelectorAll<HTMLElement>('.dt-body .dt-row')).slice(0, 40)) if (spills(row)) return false;
  for (const c of Array.from(el.querySelectorAll<HTMLElement>('.dt-body .dt-c')).slice(0, 160)) if (c.dataset.card !== 'hidden' && spills(c)) return false;
  return true;
}

/**
 * Sets the table's structure (`data-fold`: 0, 1, 2 or cards). It is the first fold, in the fixed
 * order, in which the table holds its content, and cards on a phone and where no fold does. It is
 * always tried from fold 0, so the answer is a function of the room, the fonts and the rows only.
 * `last` remembers what they were, so a call with nothing changed costs a string compare. All of
 * it runs before the browser paints, so a table never shows a state it does not stay in.
 */
function foldTable(el: HTMLElement, last: { current: string }): void {
  const room = Math.round(el.clientWidth);
  if (room === 0) return; // not on screen (another tab): nothing to measure
  const phone = typeof matchMedia === 'function' && matchMedia(PHONE_QUERY).matches;
  if (!phone && !el.querySelector('.dt-body .dt-row')) return; // no rows (loading, nothing listed): nothing to measure, and the structure the rows had stays until they are back
  const state = `${room}|${phone}|${document.fonts?.status ?? ''}|${el.textContent ?? ''}`;
  if (state === last.current && el.dataset.fold) return;
  last.current = state;
  if (phone) {
    el.dataset.fold = 'cards';
    return;
  }
  for (let fold = 0; fold <= LAST_FOLD; fold++) {
    el.dataset.fold = String(fold);
    if (holdsContent(el)) return;
  }
  el.dataset.fold = 'cards';
}

export function DataTable<K extends string>(props: {
  /** The prefix of the sort test ids (`orders`, `wallet-fills`, `balances`). */
  id: string;
  /**
   * The table's column grid (data-table.css): `orders-open`, `orders-history`, `fills` or
   * `balances`.
   */
  grid: 'orders-open' | 'orders-history' | 'fills' | 'balances';
  label: string;
  cols: readonly (DtColumn<K> | DtPlain)[];
  sort: SortState<K> | null;
  /** The column the table's own order stands for (the time, newest first). None for Balances. */
  own: SortColumn<K> | null;
  onSort: (s: SortState<K> | null) => void;
  /** More than one row: the sort control of the card layout is offered. */
  sortable?: boolean;
  class?: string;
  'data-testid'?: string;
  /** No rows to head (loading, nothing listed): the header and the sort control stay away. */
  headless?: boolean;
  /** Attributes of the rows' box (`orders-list`). */
  body?: { class?: string; 'data-testid'?: string; 'data-live'?: string | number };
  children: ComponentChildren;
}) {
  const root = useRef<HTMLDivElement>(null);
  const probe = useRef<HTMLDivElement>(null);
  const phone = useMedia(PHONE_QUERY);
  const last = useRef('');
  // The structure (data-fold on the table, read by data-table.css) is set here, before the browser
  // paints. It is set after every render, and whenever the room, the fonts or the rows change.
  useLayoutEffect(() => {
    if (root.current) foldTable(root.current, last);
  });
  useEffect(() => {
    const el = root.current;
    if (!el) return;
    const again = () => foldTable(el, last);
    const ro = typeof ResizeObserver === 'function' ? new ResizeObserver(again) : null;
    // The table's width is watched on a probe of no height. Folding changes the table's height, and
    // a resize observer on the table itself would answer its own change.
    if (probe.current) ro?.observe(probe.current);
    // A row that changes by itself, such as the USD display or a live price, is not a render of the
    // table.
    const mo = typeof MutationObserver === 'function' ? new MutationObserver(again) : null;
    const body = el.querySelector('.dt-body');
    if (body) mo?.observe(body, { childList: true, subtree: true, characterData: true });
    const fonts = typeof document !== 'undefined' ? document.fonts : undefined;
    void fonts?.ready.then(again);
    fonts?.addEventListener?.('loadingdone', again);
    return () => {
      ro?.disconnect();
      mo?.disconnect();
      fonts?.removeEventListener?.('loadingdone', again);
    };
  }, []);
  return (
    <div ref={root} class={`dt dt-${props.grid}${props.grid === 'balances' ? '' : ' dt-list'}${props.class ? ` ${props.class}` : ''}`} role="table" aria-label={props.label} data-testid={props['data-testid']}>
      <div ref={probe} class="dt-probe" aria-hidden="true" />
      {props.sortable && !props.headless && !phone ? <SortControl id={props.id} class="dt-sortctl" columns={props.cols.filter((c): c is DtColumn<K> => !('plain' in c))} state={props.sort} own={props.own} onSort={props.onSort} /> : null}
      <div class="dt-head" role="row" data-testid={`${props.id}-head`} hidden={props.headless}>
        {props.cols.map((c) =>
          'plain' in c ? (
            <span key={c.key} role="columnheader" aria-hidden="true" />
          ) : (
            <span key={c.key} role="columnheader" class={`dt-h-${c.key}${c.right ? ' dt-hr' : ''}`} aria-sort={ariaSortOf(props.sort, c.key, props.own)}>
              <SortButton id={props.id} col={c} state={props.sort} own={props.own} onSort={props.onSort} />
            </span>
          ),
        )}
      </div>
      <div class={`dt-body${props.body?.class ? ` ${props.body.class}` : ''}`} role="rowgroup" data-testid={props.body?.['data-testid']} data-live={props.body?.['data-live']}>{props.children}</div>
    </div>
  );
}

/** A row and its details: the group the details open under. */
export function DtGroup(props: JSX.HTMLAttributes<HTMLDivElement> & { children: ComponentChildren }) {
  const { class: cls, children, ...rest } = props as JSX.HTMLAttributes<HTMLDivElement> & { class?: string; children: ComponentChildren };
  return (
    <div {...rest} class={`dt-group${cls ? ` ${cls}` : ''}`}>
      {children}
    </div>
  );
}

/** The cells of a row, on the table's grid. */
export function DtRow(props: JSX.HTMLAttributes<HTMLDivElement> & { children: ComponentChildren }) {
  const { class: cls, children, ...rest } = props as JSX.HTMLAttributes<HTMLDivElement> & { class?: string; children: ComponentChildren };
  return (
    <div {...rest} role="row" class={`dt-row${cls ? ` ${cls}` : ''}`}>
      {children}
    </div>
  );
}

/**
 * One cell: its column key (the class `dt-c-<key>`), the title it has in a card (`label`), how a
 * card shows it, and whether it is a figure.
 */
export function DtCell(props: { k: string; label?: string; card?: DtCard; num?: boolean; class?: string; title?: string; 'data-testid'?: string; children?: ComponentChildren }) {
  return (
    <div
      role="cell"
      class={`dt-c dt-c-${props.k}${props.num ? ' dt-num' : ''}${props.class ? ` ${props.class}` : ''}`}
      data-label={props.label}
      data-card={props.card ?? 'pair'}
      title={props.title}
      data-testid={props['data-testid']}
    >
      {props.children}
    </div>
  );
}

/**
 * The time of a card (a phone's list row): `MM-DD HH:MM` within this year, else `YYYY-MM-DD HH:MM`.
 */
export const timeShortText = (unix: number | bigint | null): string => formatRowTimeShortUnix(unix);

/**
 * The one time format of the Portfolio's tables (Open orders, Order history, Trade history). It is
 * `formatRowTime`: today HH:MM:SS, else YYYY-MM-DD HH:MM, 24-hour, in the viewer's zone. The input
 * is unix seconds, and null reads "—".
 */
export const timeText = (unix: number | bigint | null): string => formatRowTimeUnix(unix);

/**
 * The tooltip of a time cell: the full timestamp with the viewer's zone. It is undefined for a
 * missing time.
 */
export const timeTitle = (unix: number | bigint | null): string | undefined => rowTimeTitleUnix(unix) || undefined;

/**
 * The time cell of a list row. It shows the table's text (`timeText`: today HH:MM:SS, else the
 * date and the minute). In a card it shows the short form (`timeShortText`), drawn from
 * `data-short` by data-table.css while the full text is not drawn there. The full timestamp with
 * the zone is its tooltip.
 */
export function TimeCell(props: { unix: number | bigint | null; label: string; class?: string }) {
  return (
    <div role="cell" class={`dt-c dt-c-time dt-time${props.class ? ` ${props.class}` : ''}`} data-label={props.label} data-card="badge" data-short={timeShortText(props.unix)} title={timeTitle(props.unix)}>
      <span class="dt-time-full">{timeText(props.unix)}</span>
    </div>
  );
}

/**
 * The one market cell of the tables: the token's icon and the pair. The pair is a link to its
 * market when it has one, with the same weight and treatment in every table. In a card the side
 * and, for an order, its kind follow the pair on the header line ("Buy · Market"). The table has
 * columns for them.
 */
export function MarketCell(props: { name: string; href: string | undefined; identity: JSX.Element | null; side: 'buy' | 'sell'; kind?: string; class?: string; nameClass?: string }) {
  const parts = props.name.split('/').flatMap((part, i) => (i === 0 ? [part] : ['/', <wbr key={i} />, part]));
  return (
    <DtCell k="market" card="title" class={props.class}>
      <span class="dt-market">
        {props.identity}
        <span class={`dt-market-name${props.nameClass ? ` ${props.nameClass}` : ''}`}>{props.href ? <a href={props.href}>{parts}</a> : parts}</span>
      </span>
      <span class="dt-card-only dt-sub">
        <span class={props.side === 'buy' ? 'dt-side-buy' : 'dt-side-sell'}>{t(`common.side.${props.side}`)}</span>
        {props.kind ? ` · ${props.kind}` : ''}
      </span>
    </DtCell>
  );
}

/**
 * A figure and its unit. The number sits in a piece of its own that is never broken, and the unit
 * is small and muted after it. `unitTitle` is its tooltip, the covenant id of a token with no name
 * of its own.
 */
export function Num(props: { value: string; unit?: string | null; title?: string; unitTitle?: string; class?: string; 'data-testid'?: string }) {
  return (
    <span class={props.class} title={props.title} data-testid={props['data-testid']}>
      <span class="dt-val">{props.value}</span>
      {/* a short id is one piece, so the line may break BEFORE it (a narrow cell on a wide font) instead of the cell overflowing into card layout */}
      {props.unit && props.unitTitle ? <wbr /> : null}
      {props.unit ? <span class={`dt-unit${props.unitTitle ? " dt-unit-id" : ""}`} title={props.unitTitle}>{props.unit}</span> : null}
    </span>
  );
}

/** Most fraction digits a KAS figure shows in a table or a strip. The tooltip has the rest. */
export const KAS_FRACTION_MAX = 4;

/**
 * An amount of KAS: the one cell every KAS figure of the tables goes through. `kas` is in sompi,
 * and null reads "—". `fraction` is a fixed number of decimals, the finest of its column, so the
 * decimal points line up. Without it the amount shows as it is with at most 4 decimals. The exact
 * amount is the tooltip whenever the text is not it, and an amount that is there never reads as
 * zero (`<0.0001`). Where the USD display is on (Settings, app/use-fiat.ts) it reads as dollars,
 * with its exact KAS figure in the tooltip, and has no unit.
 */
export function MoneyCell(props: { kas: bigint | null; fraction?: number; unit?: boolean; class?: string; 'data-testid'?: string }) {
  const fiat = useFiat();
  if (props.kas === null) return <span class={props.class} data-testid={props['data-testid']}>—</span>;
  const testid = props['data-testid'] ? { 'data-testid': props['data-testid'] } : {};
  if (fiat) {
    return <Num value={kasValueText(props.kas, fiat)} title={kasValueTitle(props.kas, fiat)} class={props.class} {...testid} />;
  }
  const fixed = props.fraction !== undefined ? Math.min(props.fraction, 8) : undefined;
  let text = formatKas(props.kas, { group: ',', ...(fixed !== undefined ? { minFraction: fixed, maxFraction: fixed } : { maxFraction: KAS_FRACTION_MAX }) });
  const digits = fixed ?? KAS_FRACTION_MAX;
  if (props.kas > 0n && digits < 8 && /^[0.,]+$/.test(text)) text = digits > 0 ? `<0.${'0'.repeat(digits - 1)}1` : '<1';
  const exact = formatKas(props.kas, { group: ',' });
  // A figure padded to its column's decimals ("40.7100") is the same amount. Only a figure that
  // lost digits needs the tooltip.
  const lost = digits < 8 && props.kas % 10n ** BigInt(8 - digits) !== 0n;
  return <Num value={text} unit={props.unit === false ? null : 'KAS'} {...(lost ? { title: `${exact} KAS` } : {})} class={props.class} {...testid} />;
}

/** The chevron that opens a row's details: a button with `aria-expanded`, operable by keyboard. */
export function RowToggle(props: { open: boolean; onToggle: () => void; controls?: string; label: string; 'data-testid'?: string }) {
  return (
    <DtCell k="toggle" card="toggle">
      <button type="button" class="dt-chev" aria-expanded={props.open ? 'true' : 'false'} aria-controls={props.controls} aria-label={props.label} onClick={props.onToggle} data-testid={props['data-testid']}>
        <svg width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">
          <path d="M9 6l6 6-6 6" />
        </svg>
      </button>
    </DtCell>
  );
}

// ---- The details a viewer opened stay open

const opened = new Map<string, boolean>();

/**
 * Whether a row's details are open, kept for the visit under the row's own key (`table:id`). A
 * refresh, a page of the list turned and back, or the panel drawn anew never closes what the
 * viewer opened, and never opens what they closed. `initial` is the first state of a row never
 * opened or closed by the viewer.
 */
export function useOpenRow(key: string, initial: boolean): [boolean, (open: boolean) => void] {
  const [, render] = useState(0);
  const open = opened.has(key) ? opened.get(key)! : initial;
  const set = (v: boolean) => {
    opened.set(key, v);
    render((n) => n + 1);
  };
  return [open, set];
}

/** Whether the viewer has opened or closed this row, so no automatic opening overrides it. */
export const hasOpenChoice = (key: string): boolean => opened.has(key);

/** The word of a toggle: show or hide the details. */
export const toggleLabel = (open: boolean): string => t(open ? 'orders.line.hide' : 'orders.line.show');
