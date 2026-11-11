// The sliding mark of a segmented control (the selected pill) and of a tab strip (the underline): ONE absolutely positioned element that is moved to the
// selected button by `transform` and `width`. It is out of the flow, so it never changes the size or the layout of the control it sits in (the app's
// no-layout-shift guarantees). The control itself keeps its own buttons: the indicator only paints under (a pill) or beside (an underline) the selected one.
//   - first paint: placed in a layout effect, before the browser paints, with the transition off: it never slides in from the corner
//   - the selection changes: the new place is set with the transition on, so it slides (CSS: `.slide-ind`, styles.css)
//   - a resize, a font or a label that changes the buttons' boxes: put at the new place at once (a ResizeObserver; sliding would lag behind the layout);
//     the exception is the moment right after a slide began: a pressed segment may get bolder and change its width, and its mark then keeps sliding to the new box
// The place is measured from the boxes (not offsetLeft): a segment is not a whole pixel wide on every width, and the mark must meet the pill's edges.
import type { RefObject } from 'preact';
import { useEffect, useLayoutEffect, useRef } from 'preact/hooks';

/** `pill`: the mark covers the selected button; `underline`: a bar as high as the CSS says, on the bottom edge of the selected button. */
export type SlideMode = 'pill' | 'underline';

/** A slide takes about this long (the longest of the tokens, --dur-slow; a little over). */
const SLIDE_MS = 250;

function place(root: HTMLElement, ind: HTMLElement, selected: HTMLElement | null, mode: SlideMode, animate: boolean): void {
  // no transition for a place that is not a move (first paint, resize); '' hands the property back to the stylesheet
  ind.style.transition = animate ? '' : 'none';
  if (!selected || selected.offsetWidth === 0) {
    // nothing selected, or the control is not on screen (display: none): the mark has no size until the next measure
    ind.style.width = '0px';
    return;
  }
  const rb = root.getBoundingClientRect();
  const sb = selected.getBoundingClientRect();
  // a scaled ancestor (a panel that is opening) must not skew the measure: the boxes are in screen pixels, the mark's in the control's own
  const k = root.offsetWidth > 0 && rb.width > 0 ? rb.width / root.offsetWidth : 1;
  const x = (sb.left - rb.left) / k - root.clientLeft + root.scrollLeft;
  let y = (sb.top - rb.top) / k - root.clientTop + root.scrollTop;
  if (mode === 'pill') ind.style.height = `${(sb.height / k).toFixed(2)}px`;
  else y += sb.height / k - ind.offsetHeight;
  ind.style.width = `${(sb.width / k).toFixed(2)}px`;
  ind.style.transform = `translate(${x.toFixed(2)}px, ${y.toFixed(2)}px)`;
}

/**
 * Keeps `ind` (an empty `.slide-ind` element inside `root`) under the element of `root` that matches `selector` (the selected button).
 * Runs after every render of the caller: the selection and the labels only change through one.
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
    // a slide only between two buttons that are both on the page: not the first paint, not a control that was rebuilt
    const animate = !!sel && !!last.current && last.current !== sel && last.current.isConnected;
    last.current = sel;
    if (animate) movedAt.current = performance.now();
    place(r, i, sel, mode, animate);
    if (typeof ResizeObserver === 'function') {
      observer.current ??= new ResizeObserver(() => place(r, i, r.querySelector<HTMLElement>(selector), mode, performance.now() - movedAt.current < SLIDE_MS));
      // observing an element twice is a no-op: new buttons join, the control itself is watched for its width
      observer.current.observe(r);
      for (const b of r.querySelectorAll('button')) observer.current.observe(b);
    }
  });
  useEffect(() => () => observer.current?.disconnect(), []);
}
