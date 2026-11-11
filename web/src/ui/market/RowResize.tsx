// The drag handle under the trading row (book | chart | ticket; three columns, a fine pointer): a horizontal separator that sets the row's height (founder, 2026-11-11). The row is never shorter
// than its ticket's fit (`--tk-fit`, so the ticket has no empty room) and never taller than a screen's worth under the header and the market bar (with a floor of 400 px of room above the fit, so a
// short window can still be dragged). The height is `--mkt-row-h` on the grid (market.css gives the row that as its least height); the chart's box, the book's levels and the room under the ticket
// (Market trades) follow. Drag, ArrowUp / ArrowDown in 16 px steps, Home / End for the ends, double-click or Enter reset to the default. The choice is kept in this browser (clamped again on load and
// whenever the window or the ticket's fit changes). Live data never moves it: nothing here reads the book or the ticket's content.
import { useEffect, useLayoutEffect, useRef, useState } from 'preact/hooks';
import { t } from '../../i18n';

const KEY = 'kob.market.rowH';
const STEP = 16;
/** the least room the handle allows beyond the ticket's fit when the window is short */
const SLACK = 400;
const REM_TICKET = 34;

function stored(): number | null {
  try {
    const v = Number(localStorage.getItem(KEY));
    return Number.isFinite(v) && v > 0 ? v : null;
  } catch {
    return null;
  }
}
function store(v: number | null): void {
  try {
    if (v === null) localStorage.removeItem(KEY);
    else localStorage.setItem(KEY, String(Math.round(v)));
  } catch {
    /* no storage */
  }
}

/** The row's least height (the ticket's fit) and most, in px, for this grid. */
function bounds(grid: HTMLElement): { min: number; max: number } {
  const rem = parseFloat(getComputedStyle(document.documentElement).fontSize) || 16;
  const fit = parseFloat(grid.style.getPropertyValue('--tk-fit')) || 0;
  const min = Math.ceil(Math.max(REM_TICKET * rem, fit));
  const top = grid.getBoundingClientRect().top + window.scrollY;
  return { min, max: Math.max(min + SLACK, Math.floor(window.innerHeight - top)) };
}

/** `grid` returns the `.mkt-grid` element; renders the handle (a grid item placed over the gap under the first row). */
export function RowResize(props: { grid: () => HTMLElement | null }) {
  const [chosen, setChosen] = useState<number | null>(stored);
  const [range, setRange] = useState<{ min: number; max: number; now: number }>({ min: 0, max: 0, now: 0 });
  const dragging = useRef<{ y: number; h: number } | null>(null);
  const latest = useRef<number | null>(chosen);
  latest.current = chosen;

  /** The row as it is drawn now (the book's card spans it). */
  const actual = (grid: HTMLElement): number => grid.querySelector<HTMLElement>('.mkt-area-book')?.getBoundingClientRect().height ?? 0;
  /** Clamp the choice to the bounds of now, apply it to the grid, and publish the numbers the handle's aria values read. */
  const sync = () => {
    const grid = props.grid();
    if (!grid) return;
    const { min, max } = bounds(grid);
    const c = latest.current;
    const v = c === null ? null : Math.min(max, Math.max(min, c));
    if (v === null) grid.style.removeProperty('--mkt-row-h');
    else grid.style.setProperty('--mkt-row-h', `${v}px`);
    setRange((o) => {
      const now = Math.round(v ?? actual(grid));
      return o.min === min && o.max === max && o.now === now ? o : { min, max, now };
    });
  };
  useLayoutEffect(sync, [chosen]);
  useEffect(() => {
    const grid = props.grid();
    if (!grid) return;
    window.addEventListener('resize', sync);
    // the ticket measures its form after the page is drawn (`--tk-fit`, set on the grid's style): the bounds are read again then
    const mo = new MutationObserver(sync);
    mo.observe(grid, { attributes: true, attributeFilter: ['style'] });
    sync();
    return () => {
      window.removeEventListener('resize', sync);
      mo.disconnect();
      grid.style.removeProperty('--mkt-row-h');
    };
  }, []);
  useEffect(() => () => document.body.classList.remove('mkt-resizing'), []);

  const set = (v: number | null, keep = true) => {
    latest.current = v;
    setChosen(v);
    if (keep) store(v);
  };
  const clamp = (v: number) => Math.min(range.max, Math.max(range.min, v));

  const onDown = (e: PointerEvent) => {
    if (e.button !== 0) return;
    dragging.current = { y: e.clientY, h: range.now };
    (e.currentTarget as HTMLElement).setPointerCapture(e.pointerId);
    document.body.classList.add('mkt-resizing');
    e.preventDefault();
  };
  const onMove = (e: PointerEvent) => {
    const d = dragging.current;
    if (d) set(clamp(d.h + (e.clientY - d.y)), false);
  };
  const onUp = (e: PointerEvent) => {
    if (!dragging.current) return;
    dragging.current = null;
    document.body.classList.remove('mkt-resizing');
    store(latest.current);
    try {
      (e.currentTarget as HTMLElement).releasePointerCapture(e.pointerId);
    } catch {
      /* already released */
    }
  };
  const onKey = (e: KeyboardEvent) => {
    let next: number | null | undefined;
    if (e.key === 'ArrowUp') next = clamp(range.now - STEP);
    else if (e.key === 'ArrowDown') next = clamp(range.now + STEP);
    else if (e.key === 'Home') next = range.min;
    else if (e.key === 'End') next = range.max;
    else if (e.key === 'Enter') next = null;
    if (next === undefined) return;
    e.preventDefault();
    set(next);
  };

  return (
    <div
      class="mkt-resize"
      role="separator"
      aria-orientation="horizontal"
      aria-label={t('market.resize.label')}
      aria-valuemin={range.min}
      aria-valuemax={range.max}
      aria-valuenow={range.now}
      tabIndex={0}
      title={t('market.resize.hint')}
      data-testid="market-row-resize"
      onPointerDown={onDown}
      onPointerMove={onMove}
      onPointerUp={onUp}
      onPointerCancel={onUp}
      onDblClick={() => set(null)}
      onKeyDown={onKey}
    >
      <i aria-hidden="true" />
    </div>
  );
}
