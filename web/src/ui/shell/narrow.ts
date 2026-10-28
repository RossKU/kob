// Whether the header uses its narrow layout (a phone or a small tablet): the header is one row in both layouts, but the narrow one folds the tabs, the theme
// toggle and the status into a menu and offers one "Connect" button. The width is the one the desktop grid starts at (styles.css `@media (min-width: 900px)`).
//
// A desktop whose row does not fit (wide fonts, a long translation) is squeezed into the narrow layout too, so the header stays ONE row on any font: the
// header measures its wide row (`useSqueezeCheck`) and, when the tools wrapped to a second line, reports the window width here; the wide layout comes back
// when the window is well beyond that width.
import { useEffect, useLayoutEffect, useRef, useState } from 'preact/hooks';

export const NARROW_QUERY = '(max-width: 899px)';
/** How much wider than the width it did not fit at the window must grow before the wide row is tried again. */
const SQUEEZE_RELEASE_PX = 120;

const widthNarrow = (): boolean => typeof matchMedia === 'function' && matchMedia(NARROW_QUERY).matches;

let squeezedAt: number | null = null;
const listeners = new Set<() => void>();
const setSqueezedAt = (v: number | null): void => {
  if (v === squeezedAt) return;
  squeezedAt = v;
  listeners.forEach((l) => l());
};

/** True while the header is in its narrow layout: the window is narrower than 900 px, or the wide row did not fit; follows a resize or a rotation. */
export function useNarrow(): boolean {
  const [narrow, setNarrow] = useState(widthNarrow);
  const [squeezed, setSqueezed] = useState(squeezedAt !== null);
  useEffect(() => {
    const sync = () => setSqueezed(squeezedAt !== null);
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
  return narrow || squeezed;
}

/**
 * For the header: while the wide layout is shown, checks after every layout that its tools are still on the first line (not wrapped under the brand); if
 * they wrapped, the narrow layout takes over until the window grows `SQUEEZE_RELEASE_PX` beyond that width.
 */
export function useSqueezeCheck(root: { current: HTMLElement | null }, narrow: boolean): void {
  const wasNarrow = useRef(narrow);
  wasNarrow.current = narrow;
  useLayoutEffect(() => {
    const el = root.current;
    if (!el || narrow || typeof ResizeObserver !== 'function') return;
    const check = () => {
      const brand = el.querySelector<HTMLElement>('.brand');
      const tools = el.querySelector<HTMLElement>('.header-tools');
      if (!brand || !tools || widthNarrow()) return;
      if (tools.getBoundingClientRect().top >= brand.getBoundingClientRect().bottom - 4) setSqueezedAt(window.innerWidth);
    };
    check();
    const ro = new ResizeObserver(check);
    ro.observe(el);
    return () => ro.disconnect();
  }, [narrow]);
  useEffect(() => {
    const onResize = () => {
      if (squeezedAt !== null && (window.innerWidth >= squeezedAt + SQUEEZE_RELEASE_PX || widthNarrow())) setSqueezedAt(null);
    };
    window.addEventListener('resize', onResize);
    return () => window.removeEventListener('resize', onResize);
  }, []);
}
