// The one table of the Portfolio (and of a market page's panel): a header row with sortable column titles, rows of cells, an expandable details area under a row, and,
// where the table cannot be a table, the same rows as cards of label / value pairs. Balances, Open orders, Order history and Fills are all drawn with these parts, so
// they share one header style, one row height, one cell grammar: numbers right-aligned in tabular figures with the unit small and muted, a market as its icon and
// ticker, a side in its colour, a status as a chip. Sorting is the shared helper (sort.ts, SortHeader.tsx).
//
// The structure is decided by FIT, in one fixed order (`foldTable`): the full table (fold 0), then the table folded (1: tighter columns, the Copy button an icon, the time on
// two lines; 2: the value / total under the amount), then cards. A phone (below 640 px, PHONE_QUERY) is always cards, and a card has no sort control there. The result
// depends only on the table's width, the fonts and what the rows say, never on what the table was before: it is measured again, from fold 0, whenever one of them changes
// (a resize, the fonts loading, rows arriving or changing), so the same width always gives the same structure.
import type { ComponentChildren, JSX } from 'preact';
import { useEffect, useLayoutEffect, useRef, useState } from 'preact/hooks';
import { t } from '../../i18n';
import { kasValueText, kasValueTitle } from '../../app/fiat';
import { useFiat } from '../../app/use-fiat';
import { formatKas } from '../../kob/units';
import { formatRowTimeUnix, rowTimeTitleUnix } from './format';
import { PHONE_QUERY, useMedia } from '../shell/narrow';
import { SortButton, SortControl, ariaSortOf } from './SortHeader';
import type { SortColumn, SortState } from './sort';
import './data-table.css';

/** How a column shows in a card: the `head` (the name, a line of its own: Balances), the `title` (the market, beside the status), a `badge` (the status), a label / value `pair` (two to a line), a `wide` pair (a line of its own), the buttons (`action`), the chevron (`toggle`), or not at all. */
export type DtCard = 'head' | 'title' | 'badge' | 'pair' | 'wide' | 'action' | 'toggle' | 'hidden';

export interface DtColumn<K extends string = string> extends SortColumn<K> {
  /** numbers (amounts, prices, times of a column of figures) are right-aligned in the table */
  right?: boolean;
  card?: DtCard;
}

/** A column with no title (the buttons, the chevron): it takes its place in the grid, nothing more. */
export interface DtPlain {
  key: string;
  plain: true;
}

/** The folds of a table, in order (data-table.css): 0 the full table, 1 tighter (narrower columns, the Copy button an icon, the time on two lines), 2 the value / total under the amount. */
const LAST_FOLD = 2;

const spills = (e: HTMLElement): boolean => e.scrollWidth > e.clientWidth + 1;

/** Whether the table, as it is drawn now, holds its content: no row wider than the table (the columns' minimum widths together), no cell cut off. */
function holdsContent(el: HTMLElement): boolean {
  const head = el.querySelector<HTMLElement>('.dt-head');
  if (head && !head.hidden && spills(head)) return false;
  for (const row of Array.from(el.querySelectorAll<HTMLElement>('.dt-body .dt-row')).slice(0, 40)) if (spills(row)) return false;
  for (const c of Array.from(el.querySelectorAll<HTMLElement>('.dt-body .dt-c')).slice(0, 160)) if (c.dataset.card !== 'hidden' && spills(c)) return false;
  return true;
}

/**
 * Sets the table's structure (`data-fold`: 0, 1, 2 or cards): the first fold, in the fixed order, in which the table holds its content; cards on a phone and where no fold does.
 * Always tried from fold 0, so the answer is a function of the room, the fonts and the rows only; `last` remembers what they were, so a call with nothing changed costs a string compare.
 * All of it runs before the browser paints: a table never shows a state it does not stay in.
 */
function foldTable(el: HTMLElement, last: { current: string }): void {
  const room = Math.round(el.clientWidth);
  if (room === 0) return; // not on screen (another tab): nothing to measure
  if (!el.querySelector('.dt-body .dt-row')) return; // no rows (loading, nothing listed): nothing to measure, and the structure the rows had stays until they are back
  const phone = typeof matchMedia === 'function' && matchMedia(PHONE_QUERY).matches;
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
  /** the prefix of the sort test ids (`orders`, `wallet-fills`, `balances`) */
  id: string;
  /** the table's column grid (data-table.css): `orders-open`, `orders-history`, `fills` or `balances` */
  grid: 'orders-open' | 'orders-history' | 'fills' | 'balances';
  label: string;
  cols: readonly (DtColumn<K> | DtPlain)[];
  sort: SortState<K> | null;
  /** the column the table's own order stands for (the time, newest first); none for Balances */
  own: SortColumn<K> | null;
  onSort: (s: SortState<K> | null) => void;
  /** more than one row: the sort control of the card layout is offered */
  sortable?: boolean;
  class?: string;
  'data-testid'?: string;
  /** no rows to head (loading, nothing listed): the header and the sort control stay away */
  headless?: boolean;
  /** attributes of the rows' box (`orders-list`) */
  body?: { class?: string; 'data-testid'?: string; 'data-live'?: string | number };
  children: ComponentChildren;
}) {
  const root = useRef<HTMLDivElement>(null);
  const probe = useRef<HTMLDivElement>(null);
  const phone = useMedia(PHONE_QUERY);
  const last = useRef('');
  // the structure (data-fold on the table, read by data-table.css) is set here, before the browser paints: after every render, and whenever the room, the fonts or the rows change
  useLayoutEffect(() => {
    if (root.current) foldTable(root.current, last);
  });
  useEffect(() => {
    const el = root.current;
    if (!el) return;
    const again = () => foldTable(el, last);
    const ro = typeof ResizeObserver === 'function' ? new ResizeObserver(again) : null;
    // the table's width is watched on a probe of no height: folding changes the table's height, and a resize observer on the table itself would answer its own change
    if (probe.current) ro?.observe(probe.current);
    // a row that changes by itself (the USD display, a live price) is not a render of the table
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
    <div ref={root} class={`dt dt-${props.grid}${props.class ? ` ${props.class}` : ''}`} role="table" aria-label={props.label} data-testid={props['data-testid']}>
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

/** One cell: its column key (the class `dt-c-<key>`), the title it has in a card (`label`), how a card shows it, and whether it is a figure. */
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

/** The one time format of the Portfolio's tables (Open orders, Order history, Trade history): `formatRowTime` (today HH:MM:SS, else YYYY-MM-DD HH:MM, 24-hour, the viewer's zone). unix seconds; null reads "—". */
export const timeText = (unix: number | bigint | null): string => formatRowTimeUnix(unix);

/** The tooltip of a time cell: the full timestamp with the viewer's zone (undefined for a missing time). */
export const timeTitle = (unix: number | bigint | null): string | undefined => rowTimeTitleUnix(unix) || undefined;

/**
 * The one market cell of the tables: the token's icon and the pair (a link to its market when it has one, in every table the same weight and treatment). In a card the side (and,
 * for an order, its kind) follow the pair on the header line ("Buy · Market"); the table has columns for them.
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

/** A figure and its unit: the number in a piece of its own (never broken), the unit small and muted after it (`unitTitle`: its tooltip, the covenant id of a token with no name of its own). */
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

/** Most fraction digits a KAS figure shows in a table or a strip (review M-03); the tooltip has the rest. */
export const KAS_FRACTION_MAX = 4;

/**
 * An amount of KAS: the one cell every KAS figure of the tables goes through. kas is in sompi, null reads "—". `fraction` is a fixed number of decimals (the finest of its column, so the
 * decimal points line up); without it the amount shows as it is with at most 4 decimals. The exact amount is the tooltip whenever the text is not it, and an amount that is there never reads
 * as zero (`<0.0001`). Where the USD display is on (Settings, app/use-fiat.ts) it reads as dollars, with its exact KAS figure in the tooltip, and has no unit.
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
  // a figure padded to its column's decimals ("40.7100") is the same amount: only a figure that lost digits needs the tooltip
  const lost = digits < 8 && props.kas % 10n ** BigInt(8 - digits) !== 0n;
  return <Num value={text} unit={props.unit === false ? null : 'KAS'} {...(lost ? { title: `${exact} KAS` } : {})} class={props.class} {...testid} />;
}

/** The chevron that opens a row's details: a button, `aria-expanded`, keyboard operable. */
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

// ------------------------------------------------------------------------------------------------ the details a viewer opened stay open

const opened = new Map<string, boolean>();

/**
 * Whether a row's details are open, kept for the visit under the row's own key (`table:id`): a refresh, a page of the list turned and back, or the panel drawn anew never closes
 * what the viewer opened (nor opens what they closed). `initial`: the first state of a row never opened or closed by the viewer.
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

/** Whether the viewer has opened or closed this row (so no automatic opening overrides it). */
export const hasOpenChoice = (key: string): boolean => opened.has(key);

/** The word of a toggle: show or hide the details. */
export const toggleLabel = (open: boolean): string => t(open ? 'orders.line.hide' : 'orders.line.show');
