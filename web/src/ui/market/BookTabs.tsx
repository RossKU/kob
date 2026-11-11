// Below 1200 px the depth chart has no panel of its own: the order book's panel holds both, an "Order book | Depth" switch in its header (the choice remembered in
// this browser). The book is only hidden behind the depth chart (visibility, its body keeps its size and its state), never unmounted, so the panel does not change
// height and the book is as it was when it comes back; the depth chart is mounted while it is shown, so it measures the body it fills. From 1200 px there is no
// switch: the book is the column and the depth chart the panel under the chart (market.css).
import type { ComponentChildren } from 'preact';
import { useState } from 'preact/hooks';
import { t } from '../../i18n';
import { Segmented } from '../kit';

export type BookPanelView = 'book' | 'depth';

const VIEW_KEY = 'kob.book.view';

function storedView(): BookPanelView {
  try {
    if (localStorage.getItem(VIEW_KEY) === 'depth') return 'depth';
  } catch {
    /* no storage */
  }
  return 'book';
}

/** The view of the book's panel: `tabbed` below three columns (`wide` false), where `view` is the remembered choice; on three columns the book, always. */
export function useBookPanelView(wide: boolean): { tabbed: boolean; view: BookPanelView; setView: (v: BookPanelView) => void } {
  const [view, setViewState] = useState<BookPanelView>(storedView);
  const setView = (v: BookPanelView) => {
    setViewState(v);
    try {
      localStorage.setItem(VIEW_KEY, v);
    } catch {
      /* no storage */
    }
  };
  return { tabbed: !wide, view: wide ? 'book' : view, setView };
}

/** The "Order book | Depth" switch for the panel's header actions. */
export function BookViewSwitch(props: { view: BookPanelView; onChange: (v: BookPanelView) => void }) {
  return (
    <div class="book-tabs">
      <Segmented
        options={[
          { value: 'book' as BookPanelView, label: t('market.book.view.book'), 'data-testid': 'book-view-book' },
          { value: 'depth' as BookPanelView, label: t('market.book.view.depth'), 'data-testid': 'book-view-depth' },
        ]}
        value={props.view}
        onChange={props.onChange}
        aria-label={t('market.book.view')}
        data-testid="book-view"
      />
    </div>
  );
}

/** The class of the panel's section: what market.css keys the switch's header and the depth overlay on. */
export const bookPanelClass = (tabbed: boolean, view: BookPanelView): string => `mkt-area-book mkt-card${tabbed ? ' book-tabbed' : ''}${tabbed && view === 'depth' ? ' is-depth' : ''}`;

/** The panel's body: the book (hidden while the depth chart is shown) and, over it, the depth chart filling the body. Untabbed it is the book alone. */
export function BookPanelBody(props: { tabbed: boolean; view: BookPanelView; depth: ComponentChildren; children: ComponentChildren }) {
  if (!props.tabbed) return <>{props.children}</>;
  const showDepth = props.view === 'depth';
  return (
    <>
      <div class={showDepth ? 'book-main book-main-off' : 'book-main'}>{props.children}</div>
      {showDepth ? <div class="book-depth-pane" data-testid="book-depth-pane">{props.depth}</div> : null}
    </>
  );
}
