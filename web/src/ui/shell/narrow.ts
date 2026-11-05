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
  const toolGap = px(getComputedStyle(tools).columnGap);
  const items = Array.from(tools.children) as HTMLElement[];
  const toolsWidth = items.reduce((s, c) => s + c.getBoundingClientRect().width, 0) + toolGap * Math.max(0, items.length - 1);
  // the brand counts as its logo only: its name is shown on top only when there is room for it (`useBrandName`), never at the cost of the tabs
  return brand.getBoundingClientRect().width - brandNameWidth(brand) + nav.getBoundingClientRect().width + toolsWidth + 2 * gap;
}

/** The width the brand's name takes beside the logo (its text and the gap before it), 0 while it is hidden. */
function brandNameWidth(brand: HTMLElement): number {
  const name = brand.querySelector<HTMLElement>('span');
  if (!name || name.getBoundingClientRect().width <= 1) return 0;
  return name.getBoundingClientRect().width + px(getComputedStyle(brand).columnGap);
}

/**
 * For the header's wide row on a desktop that is not wide (720-1199 px, where styles.css hides the brand's name by default): true while the row has room
 * for the name beside the logo on top of the tabs and the tools (a disconnected wallet's short buttons, a wider window), so the name is shown whenever it fits.
 */
export function useBrandName(root: { current: HTMLElement | null }, narrow: boolean): boolean {
  const [fits, setFits] = useState(false);
  useLayoutEffect(() => {
    const el = root.current;
    if (!el || narrow || typeof ResizeObserver !== 'function') return;
    const check = () => {
      const brand = el.querySelector<HTMLElement>('.brand');
      const name = brand?.querySelector<HTMLElement>('span');
      const needed = wideWidth(el);
      if (!brand || !name || needed === null) return;
      // the name's own width, also while it is clipped to 1 px (scrollWidth is its text's width)
      const nameWidth = name.scrollWidth + px(getComputedStyle(brand).columnGap);
      setFits(needed + nameWidth + SPARE_PX <= roomOf(el));
    };
    check();
    const ro = new ResizeObserver(check);
    ro.observe(el);
    for (const c of Array.from(el.querySelector('.header-tools')?.children ?? [])) ro.observe(c);
    void document.fonts?.ready.then(check);
    return () => ro.disconnect();
  }, [narrow]);
  return fits && !narrow;
}

/**
 * For the header. In the wide layout: after every layout and on every resize, measures the row; when it is wider than its room the narrow layout takes over and
 * the measured width and the room are remembered. In the narrow layout: on a resize, on the fonts loading and when the layout changes, the wide row is given
 * back when the room fits the remembered width or has grown by a few pixels since it failed (the wide check then confirms it, or squeezes again with the new measure).
 */
export function useSqueezeCheck(root: { current: HTMLElement | null }, narrow: boolean): void {
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
      retry();
      const ro = new ResizeObserver(retry);
      ro.observe(el);
      window.addEventListener('resize', retry);
      void document.fonts?.ready.then(retry);
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
        setSqueezed(true);
      }
    };
    check();
    const ro = new ResizeObserver(check);
    ro.observe(el);
    void document.fonts?.ready.then(check);
    return () => ro.disconnect();
  }, [narrow]);
}
