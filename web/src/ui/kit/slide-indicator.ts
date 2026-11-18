// The sliding mark of a segmented control (the selected pill) and of a tab strip (the underline).
// It is ONE absolutely positioned element that is moved to the selected button by `transform` and
// `width`. It is out of the flow, so it never changes the size or the layout of the control it
// sits in (the app's no-layout-shift guarantees). The control keeps its own buttons. The
// indicator only paints under (a pill) or beside (an underline) the selected one.
// - First paint: it is placed in a layout effect, before the browser paints, with the transition
//   off, so it never slides in from the corner.
// - The selection changes: the new place is set with the transition on, so it slides (CSS:
//   `.slide-ind`, styles.css).
// - A resize, a font or a label that changes the buttons' boxes: it is put at the new place at
//   once by a ResizeObserver, because sliding would lag behind the layout. The exception is the
//   moment right after a slide began. A pressed segment may get bolder and change its width, and
//   its mark then keeps sliding to the new box.
//
// The place is measured from the boxes, not `offsetLeft`. A segment is not a whole pixel wide on
// every width, and the mark must meet the pill's edges.
import type { RefObject } from 'preact';
import { useEffect, useLayoutEffect, useRef } from 'preact/hooks';

/**
 * `pill`: the mark covers the selected button. `underline`: a bar as high as the CSS says, on the
 * bottom edge of the selected button.
 */
export type SlideMode = 'pill' | 'underline';

/** A slide takes about this long: the longest of the tokens (--dur-slow), and a little over. */
const SLIDE_MS = 250;

function place(root: HTMLElement, ind: HTMLElement, selected: HTMLElement | null, mode: SlideMode, animate: boolean): void {
  // No transition for a place that is not a move (first paint, resize). '' hands the property back
  // to the stylesheet.
  ind.style.transition = animate ? '' : 'none';
  if (!selected || selected.offsetWidth === 0) {
    // Nothing is selected, or the control is not on screen (display: none). The mark has no size
    // until the next measure.
    ind.style.width = '0px';
    return;
  }
  const rb = root.getBoundingClientRect();
  const sb = selected.getBoundingClientRect();
  // A scaled ancestor, such as a panel that is opening, must not skew the measure. The boxes are in
  // screen pixels, and the mark's are in the control's own.
  const k = root.offsetWidth > 0 && rb.width > 0 ? rb.width / root.offsetWidth : 1;
  const x = (sb.left - rb.left) / k - root.clientLeft + root.scrollLeft;
  let y = (sb.top - rb.top) / k - root.clientTop + root.scrollTop;
  if (mode === 'pill') ind.style.height = `${(sb.height / k).toFixed(2)}px`;
  else y += sb.height / k - ind.offsetHeight;
  ind.style.width = `${(sb.width / k).toFixed(2)}px`;
  ind.style.transform = `translate(${x.toFixed(2)}px, ${y.toFixed(2)}px)`;
}

/**
 * Keeps `ind` (an empty `.slide-ind` element inside `root`) under the element of `root` that
 * matches `selector` (the selected button). It runs after every render of the caller, as the
 * selection and the labels only change through one.
 */
export function useSlideIndicator(root: RefObject<HTMLElement>, ind: RefObject<HTMLElement>, selector: string, mode: SlideMode): void {
  const last = useRef<HTMLElement | null>(null);
  const movedAt = useRef(-Infinity);
  const observer = useRef<ResizeObserver | null>(null);
  useLayoutEffect(() => {
    const r = root.current;
    const i = ind.current;
    if (!r || !i) return;
    const sel = r.querySelector<HTMLElement>(selector);
    // A slide happens only between two buttons that are both on the page. The first paint and a
    // control that was rebuilt do not slide.
    const animate = !!sel && !!last.current && last.current !== sel && last.current.isConnected;
    last.current = sel;
    if (animate) movedAt.current = performance.now();
    place(r, i, sel, mode, animate);
    if (typeof ResizeObserver === 'function') {
      observer.current ??= new ResizeObserver(() => place(r, i, r.querySelector<HTMLElement>(selector), mode, performance.now() - movedAt.current < SLIDE_MS));
      // Observing an element twice is a no-op. New buttons join, and the control itself is watched
      // for its width.
      observer.current.observe(r);
      for (const b of r.querySelectorAll('button')) observer.current.observe(b);
    }
  });
  useEffect(() => () => observer.current?.disconnect(), []);
}
