// The one table of the Portfolio (and of a market page's panel): a header row with sortable column titles, rows of cells, an expandable details area under a row, and,
// where the table is narrow, the same rows as cards of label / value pairs (data-table.css: the table's own width decides, by a container query). Balances, Open orders,
// Order history and Fills are all drawn with these parts, so they share one header style, one row height, one cell grammar: numbers right-aligned in tabular figures with
// the unit small and muted, a market as its icon and ticker, a side in its colour, a status as a chip. Sorting is the shared helper (sort.ts, SortHeader.tsx).
import type { ComponentChildren, JSX } from 'preact';
import { useLayoutEffect, useRef, useState } from 'preact/hooks';
import { t } from '../../i18n';
import { kasValueText, kasValueTitle } from '../../app/fiat';
import { useFiat } from '../../app/use-fiat';
import { formatKas } from '../../kob/units';
import { formatRowTimeUnix, rowTimeTitleUnix } from './format';
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
  // a table whose cells do not hold their content (a wide font, a long translation, a long name) is cards instead, until the table is wider than it was then
  const root = useRef<HTMLDivElement>(null);
  const [width, setWidth] = useState(0);
  const [tightAt, setTightAt] = useState<number | null>(null);
  useLayoutEffect(() => {
    const el = root.current;
    if (!el || typeof ResizeObserver !== 'function') return;
    const measure = () => setWidth(Math.round(el.clientWidth));
    measure();
    const ro = new ResizeObserver(measure);
    ro.observe(el);
    return () => ro.disconnect();
  }, []);
  const forced = tightAt !== null && width <= tightAt + 24;
  useLayoutEffect(() => {
    const el = root.current;
    if (!el || forced || width === 0) return;
    const head = el.querySelector('.dt-head');
    if (!head || getComputedStyle(head).display === 'none') return; // narrow already: the cards of the container query
    for (const c of Array.from(el.querySelectorAll<HTMLElement>('.dt-body .dt-c')).slice(0, 160)) {
      if (c.scrollWidth > c.clientWidth + 1 && c.dataset.card !== 'hidden') {
        setTightAt(width);
        return;
      }
    }
  });
  return (
    <div ref={root} class={`dt dt-${props.grid}${forced ? ' dt-force-cards' : ''}${props.class ? ` ${props.class}` : ''}`} role="table" aria-label={props.label} data-testid={props['data-testid']}>
      {props.sortable && !props.headless ? <SortControl id={props.id} class="dt-sortctl" columns={props.cols.filter((c): c is DtColumn<K> => !('plain' in c))} state={props.sort} own={props.own} onSort={props.onSort} /> : null}
      <div class="dt-head" role="row" data-testid={`${props.id}-head`} hidden={props.headless}>
        {props.cols.map((c) =>
          'plain' in c ? (
            <span key={c.key} role="columnheader" aria-hidden="true" />
          ) : (
            <span key={c.key} role="columnheader" class={c.right ? 'dt-hr' : undefined} aria-sort={ariaSortOf(props.sort, c.key, props.own)}>
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
