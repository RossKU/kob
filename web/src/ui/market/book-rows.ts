// How many levels per side the order book shows. It is as many as fit the card's height.
//
// From 760 px the card is as tall as its grid row. market.css gives it no height of its own, with
// `contain: size`. Its sides share what the header, the spread row and the foot leave (flex), and
// the asks' side is measured here. The measure is of a box whose height the grid decided, never of
// what the book draws in it, because a side is `flex: 1 1 0` with `overflow: hidden`. Drawing more
// or fewer levels cannot change it, so there is no loop.
//
// A phone is one column with a card of its own height. It always shows the 12 levels it was built
// for, or 8 taller ones on a touch screen. The side's height is then the rows' own (`--ob-rows`).
import { useCallback, useEffect, useState } from 'preact/hooks';

export const BOOK_ROWS_MIN = 8;
export const BOOK_ROWS_MAX = 40;
/** The phone's fixed book, and the rows drawn until the card is measured. */
export const BOOK_ROWS_PHONE = 12;
/**
 * The phone's book on a touch screen. It has fewer, taller levels, because a row is a tap target of
 * at least 32 px.
 */
export const BOOK_ROWS_PHONE_TOUCH = 8;
/** A level's row height (market.css `.ob .book-row`). */
export const BOOK_ROW_PX = 21;
/**
 * A level's row height on a coarse pointer (market.css `@media (pointer: coarse)`: `--ob-row-h`).
 */
export const BOOK_ROW_PX_TOUCH = 36;
/**
 * What the page asks of its data source for the book. It is twice the most the book can show, so a
 * grouped book, where levels merge into steps, still has levels for every slot. The indexer's cap
 * is 200.
 */
export const BOOK_FETCH_DEPTH = BOOK_ROWS_MAX * 2;

const COLUMNS = '(min-width: 760px)';
const COARSE = '(pointer: coarse)';
const isCoarse = (): boolean => typeof matchMedia === 'function' && matchMedia(COARSE).matches;
const phoneRows = (): number => (isCoarse() ? BOOK_ROWS_PHONE_TOUCH : BOOK_ROWS_PHONE);

/**
 * The levels per side that fit a side of `heightPx`, which is the asks' box and equal to the bids'.
 * The count is between the least and the most.
 */
export const rowsFor = (heightPx: number, rowPx: number = BOOK_ROW_PX): number => Math.min(BOOK_ROWS_MAX, Math.max(BOOK_ROWS_MIN, Math.floor((heightPx + 0.5) / rowPx)));

/**
 * The rows per side for a book panel. `fitRef` goes on the asks' side element. It is a callback
 * ref, because the book comes and goes with its data and the frame draws the same box while it
 * loads. An element that is not drawn keeps the last count. That is the case with height 0, as
 * when the expanded chart hides the book.
 */
export function useBookRows(): { rows: number; fitRef: (el: HTMLElement | null) => void } {
  const [rows, setRows] = useState(phoneRows);
  const [el, setEl] = useState<HTMLElement | null>(null);
  const fitRef = useCallback((node: HTMLElement | null) => setEl(node), []);
  useEffect(() => {
    if (!el || typeof ResizeObserver === 'undefined' || typeof matchMedia !== 'function') return;
    const mq = matchMedia(COLUMNS);
    const read = () => {
      if (!mq.matches) return setRows(phoneRows());
      const h = el.clientHeight;
      if (h > 0) setRows(rowsFor(h, isCoarse() ? BOOK_ROW_PX_TOUCH : BOOK_ROW_PX));
    };
    read();
    const ro = new ResizeObserver(read);
    ro.observe(el);
    mq.addEventListener('change', read);
    return () => {
      ro.disconnect();
      mq.removeEventListener('change', read);
    };
  }, [el]);
  return { rows, fitRef };
}
