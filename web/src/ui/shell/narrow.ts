// Whether the header uses its narrow layout (a phone or a small tablet): the header is one row in both layouts, but the narrow one folds the tabs, the theme
// toggle and the status into a menu and offers one "Connect" button. Below 720 px it always does; from 720 px on the wide row is tried and measured.
//
// A desktop whose row does not fit (wide fonts, a long translation) is squeezed into the narrow layout too, so the header stays ONE row on any font. The wide
// row is measured whenever it is shown (`useSqueezeCheck`): its natural width is remembered, and the wide layout comes back as soon as the room is at least
// that wide (a resize, a rotation, an unfolded phone, the fonts having loaded). Nothing needs the wide row to be on screen to know whether it would fit.
import { useEffect, useLayoutEffect, useState } from 'preact/hooks';

export const NARROW_QUERY = '(max-width: 719px)';
/** A desktop row that is not wide: the chip is shortened (styles.css has the matching spacing). */
export const TIGHT_QUERY = '(max-width: 1199px)';
/** A phone: the Portfolio page draws its fills as cards below this (orders/balances.css has the matching breakpoint for the strip and the balances). */
export const PHONE_QUERY = '(max-width: 639px)';
/**
 * A phone for the notifications (toasts): below 640 px, or a touch screen below 760 px (the same cut as the market page's Buy / Sell bar). The toasts sit at
 * the top under the header, one at a time (kit/Toast.tsx, styles.css `.toast-host[data-phone]`); a desktop shows them stacked at the top right under the header.
 */
export const PHONE_TOAST_QUERY = '(max-width: 639px), (pointer: coarse) and (max-width: 759px)';
/** Spare room asked for before the wide row is tried again after it did not fit. */
const SPARE_PX = 4;
const RETRY_PX = 8;

const widthNarrow = (): boolean => typeof matchMedia === 'function' && matchMedia(NARROW_QUERY).matches;

/** The natural width of the wide row as last measured (null: not measured yet) and whether the header is squeezed into the narrow layout now. */
let neededWide: number | null = null;
/** The room the wide row did not fit in last time: more room than that (by `RETRY_PX`) is worth trying again, whatever the stale measure says. */
let failedRoom = 0;
let failedWindow = 0;
let squeezed = false;
/** What the wide row held when it did not fit (`contentKey`), and whether the fonts were still loading then: either changing earns the wide row a new try. */
let squeezedKey = '';
let squeezedLoading = false;
const listeners = new Set<() => void>();
const setSqueezed = (v: boolean): void => {
  if (v === squeezed) return;
  squeezed = v;
  listeners.forEach((l) => l());
};

/** True while the window matches `query`; follows a resize or a rotation. */
export function useMedia(query: string): boolean {
  const get = () => typeof matchMedia === 'function' && matchMedia(query).matches;
  const [on, setOn] = useState(get);
  useEffect(() => {
    if (typeof matchMedia !== 'function') return;
    const q = matchMedia(query);
    const f = () => setOn(q.matches);
    f();
    q.addEventListener('change', f);
    return () => q.removeEventListener('change', f);
  }, [query]);
  return on;
}

/** True while the header is in its narrow layout: the window is narrower than 900 px, or the wide row did not fit; follows a resize or a rotation. */
export function useNarrow(): boolean {
  const [narrow, setNarrow] = useState(widthNarrow);
  const [sq, setSq] = useState(squeezed);
  useEffect(() => {
    const sync = () => setSq(squeezed);
    listeners.add(sync);
    sync();
    if (typeof matchMedia !== 'function') return () => void listeners.delete(sync);
    const q = matchMedia(NARROW_QUERY);
    const on = () => setNarrow(q.matches);
    on();
    q.addEventListener('change', on);
    return () => {
      listeners.delete(sync);
      q.removeEventListener('change', on);
    };
  }, []);
  return narrow || sq;
}

const px = (v: string): number => Number.parseFloat(v) || 0;

/** The room of the header row: the width inside the container's padding. */
function roomOf(el: HTMLElement): number {
  const cs = getComputedStyle(el);
  return el.clientWidth - px(cs.paddingLeft) - px(cs.paddingRight);
}

/** The natural width of the wide row: the brand, the tabs and the tools each at their content width, and the gaps between them. */
function wideWidth(el: HTMLElement): number | null {
  const brand = el.querySelector<HTMLElement>('.brand');
  const nav = el.querySelector<HTMLElement>('.nav');
  const tools = el.querySelector<HTMLElement>('.header-tools');
  if (!brand || !nav || !tools) return null;
  const gap = px(getComputedStyle(el).columnGap);
  // the brand counts as its logo only: its name is shown on top only when there is room for it (`useHeaderLayout`), never at the cost of the tabs; the
  // tabs and the tools at their content widths, whether the row is centred or packed (the narrow layout takes over only when even the packed row overflows)
  return rowWidths({ logo: brand.getBoundingClientRect().width - brandNameWidth(brand), name: 0, nav: nav.getBoundingClientRect().width, tools: toolsWidth(tools), gap }, false).packed;
}

/** The tools' natural width: their items and the gaps between them (not their box, which a centred row may stretch). */
function toolsWidth(tools: HTMLElement): number {
  const toolGap = px(getComputedStyle(tools).columnGap);
  const items = Array.from(tools.children) as HTMLElement[];
  return items.reduce((s, c) => s + c.getBoundingClientRect().width, 0) + toolGap * Math.max(0, items.length - 1);
}

/** The width the brand's name takes beside the logo (its text and the gap before it), 0 while it is hidden. */
function brandNameWidth(brand: HTMLElement): number {
  const name = brand.querySelector<HTMLElement>('span');
  if (!name || name.getBoundingClientRect().width <= 1) return 0;
  return name.getBoundingClientRect().width + px(getComputedStyle(brand).columnGap);
}

/**
 * The widths of the wide row (pure): `packed` = the brand, the tabs and the tools side by side; `centred` = the tabs in the true centre of the row, so
 * each side takes the wider of the brand and the tools. `withName` adds the brand's name beside the logo.
 */
export function rowWidths(m: { logo: number; name: number; nav: number; tools: number; gap: number }, withName: boolean): { packed: number; centred: number } {
  const brand = m.logo + (withName ? m.name : 0);
  return { packed: brand + m.nav + m.tools + 2 * m.gap, centred: m.nav + 2 * Math.max(brand, m.tools) + 2 * m.gap };
}

/**
 * The wide row's arrangement (pure): the tabs centred when the row has room for that (without the brand's name), else packed after the brand; the brand's
 * name shown beside the logo whenever the chosen arrangement still has room for it.
 */
export function headerLayout(m: { logo: number; name: number; nav: number; tools: number; gap: number; room: number }): { centred: boolean; brandName: boolean } {
  const fits = (w: number) => w + SPARE_PX <= m.room;
  const centred = fits(rowWidths(m, false).centred);
  const withName = rowWidths(m, true);
  return { centred, brandName: centred ? fits(withName.centred) : fits(withName.packed) };
}

/**
 * For the header's wide row: whether its tabs sit in the true centre of the row (brand left, tools right) and whether the brand's name fits beside the logo
 * (styles.css hides it otherwise). Measured after every layout change of the row's parts (the
 * wallet's buttons, the fonts loading, a resize); the narrow layout (`useSqueezeCheck`) takes over only when even the packed row does not fit.
 */
export function useHeaderLayout(root: { current: HTMLElement | null }, narrow: boolean): { centred: boolean; brandName: boolean } {
  const [layout, setLayout] = useState({ centred: false, brandName: false });
  useLayoutEffect(() => {
    const el = root.current;
    if (!el || narrow || typeof ResizeObserver !== 'function') return;
    const check = () => {
      const brand = el.querySelector<HTMLElement>('.brand');
      const name = brand?.querySelector<HTMLElement>('span');
      const nav = el.querySelector<HTMLElement>('.nav');
      const tools = el.querySelector<HTMLElement>('.header-tools');
      if (!brand || !name || !nav || !tools) return;
      // the name's own width, also while it is clipped to 1 px (scrollWidth is its text's width)
      const nameWidth = name.scrollWidth + px(getComputedStyle(brand).columnGap);
      const row = { logo: brand.getBoundingClientRect().width - brandNameWidth(brand), name: nameWidth, nav: nav.getBoundingClientRect().width, tools: toolsWidth(tools), gap: px(getComputedStyle(el).columnGap), room: roomOf(el) };
      const next = headerLayout(row);
      setLayout((l) => (l.centred === next.centred && l.brandName === next.brandName ? l : next));
    };
    check();
    const ro = new ResizeObserver(check);
    ro.observe(el);
    for (const c of Array.from(el.querySelector('.header-tools')?.children ?? [])) ro.observe(c);
    void document.fonts?.ready.then(check);
    return () => ro.disconnect();
  }, [narrow]);
  return narrow ? { centred: false, brandName: false } : layout;
}


/**
 * For the header. In the wide layout: after every layout and on every resize, measures the row; when it is wider than its room the narrow layout takes over and
 * the measured width and the room are remembered. In the narrow layout: on a resize, on the fonts loading and when the layout changes, the wide row is given
 * back when the room fits the remembered width or has grown by a few pixels since it failed (the wide check then confirms it, or squeezes again with the new measure).
 */
export function useSqueezeCheck(root: { current: HTMLElement | null }, narrow: boolean, contentKey = ''): void {
  useLayoutEffect(() => {
    const el = root.current;
    if (!el || typeof ResizeObserver !== 'function') return;
    if (narrow) {
      // only a header squeezed by its own row can come back; below 720 px the media query decides
      const retry = () => {
        if (!squeezed || widthNarrow()) return;
        const room = roomOf(el);
        // (the container has a maximum width: a wider window with the same room still earns a new try)
        if ((neededWide !== null && room >= neededWide + SPARE_PX) || room >= failedRoom + RETRY_PX || window.innerWidth >= failedWindow + RETRY_PX) setSqueezed(false);
      };
      // the row it did not fit holds something else now (a wallet connected, the Trade tab shown), or the fonts it was measured in were not loaded yet: the
      // remembered width is stale, so the wide row is tried again (and measured anew) instead of waiting for a resize
      const fresh = () => {
        if (squeezed && !widthNarrow()) setSqueezed(false);
      };
      if (squeezed && contentKey !== squeezedKey) fresh();
      else retry();
      const ro = new ResizeObserver(retry);
      ro.observe(el);
      window.addEventListener('resize', retry);
      void document.fonts?.ready.then(() => (squeezedLoading ? fresh() : retry()));
      return () => {
        ro.disconnect();
        window.removeEventListener('resize', retry);
      };
    }
    const check = () => {
      if (widthNarrow()) return;
      const needed = wideWidth(el);
      if (needed === null) return;
      neededWide = needed;
      const room = roomOf(el);
      if (needed > room + 0.5) {
        failedRoom = room;
        failedWindow = window.innerWidth;
        squeezedKey = contentKey;
        squeezedLoading = document.fonts?.status === 'loading';
        setSqueezed(true);
      }
    };
    check();
    const ro = new ResizeObserver(check);
    ro.observe(el);
    void document.fonts?.ready.then(check);
    return () => ro.disconnect();
  }, [narrow, contentKey]);
}
