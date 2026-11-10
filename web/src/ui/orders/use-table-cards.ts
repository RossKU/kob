// A table of the Portfolio that becomes cards where it has no room: the box around it is measured (its own width, not the screen's), the cards come below `from`
// px, and also where the table does not fit its box anyway (a wide font, a long translation) until the box is wider than it was then.
import { useLayoutEffect, useState } from 'preact/hooks';

/** `[ref, cards]`: `ref` goes on the box around the table (a callback: the box may come and go), `cards` says how to draw what is in it. */
export function useTableCards(from: number): [(el: HTMLDivElement | null) => void, boolean] {
  const [el, setEl] = useState<HTMLDivElement | null>(null);
  const [w, setW] = useState(0);
  const [tightAt, setTightAt] = useState<number | null>(null);
  useLayoutEffect(() => {
    if (!el || typeof ResizeObserver !== 'function') return;
    const measure = () => setW(Math.round(el.clientWidth));
    measure();
    const ro = new ResizeObserver(measure);
    ro.observe(el);
    return () => ro.disconnect();
  }, [el]);
  const cards = w > 0 && (w < from || (tightAt !== null && w <= tightAt + 24));
  useLayoutEffect(() => {
    if (cards) return;
    const wrap = el?.querySelector<HTMLElement>('.table-wrap');
    if (wrap && wrap.scrollWidth > wrap.clientWidth + 1) setTightAt(w);
  });
  return [setEl, cards];
}

/** The box width (px) from which the Portfolio's tables are tables. */
export const TABLE_FROM = 1060;
