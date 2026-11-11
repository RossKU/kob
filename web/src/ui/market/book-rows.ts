// How many levels per side the order book shows: as many as fit the card's height (founder, 2026-11-11; on a tall screen the card was half blank under 12 rows). From 760 px the
// card is as tall as its grid row (market.css: it takes no height of its own, `contain: size`), its sides share what the header, the spread row and the foot leave (flex), and the
// asks' side is measured here. The measure is of a box whose height the GRID decided, never of what the book draws in it (a side is `flex: 1 1 0` with `overflow: hidden`): drawing
// more or fewer levels cannot change it, so there is no loop. A phone is one column with a card of its own height: always the 12 levels it was built for (the side's height is then
// the rows' own, `--ob-rows`).
import { useCallback, useEffect, useState } from 'preact/hooks';

export const BOOK_ROWS_MIN = 8;
export const BOOK_ROWS_MAX = 40;
/** the phone's fixed book, and the rows drawn until the card is measured */
export const BOOK_ROWS_PHONE = 12;
/** a level's row height (market.css `.ob .book-row`) */
export const BOOK_ROW_PX = 21;
/** What the page asks of its data source for the book: twice the most it can show, so a grouped book (levels merge into steps) still has levels for every slot (the indexer's cap is 200). */
export const BOOK_FETCH_DEPTH = BOOK_ROWS_MAX * 2;

const COLUMNS = '(min-width: 760px)';

/** The levels per side that fit a side of `heightPx` (the asks' box, equal to the bids'): between the least and the most. */
export const rowsFor = (heightPx: number): number => Math.min(BOOK_ROWS_MAX, Math.max(BOOK_ROWS_MIN, Math.floor((heightPx + 0.5) / BOOK_ROW_PX)));

/**
 * The rows per side for a book panel: `fitRef` goes on the asks' side element (a callback ref: the book comes and goes with its data, the frame draws the same box while it loads).
 * An element that is not drawn (hidden: height 0, the expanded chart hides the book) keeps the last count.
 */
export function useBookRows(): { rows: number; fitRef: (el: HTMLElement | null) => void } {
  const [rows, setRows] = useState(BOOK_ROWS_PHONE);
  const [el, setEl] = useState<HTMLElement | null>(null);
  const fitRef = useCallback((node: HTMLElement | null) => setEl(node), []);
  useEffect(() => {
    if (!el || typeof ResizeObserver === 'undefined' || typeof matchMedia !== 'function') return;
    const mq = matchMedia(COLUMNS);
    const read = () => {
      if (!mq.matches) return setRows(BOOK_ROWS_PHONE);
      const h = el.clientHeight;
      if (h > 0) setRows(rowsFor(h));
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
