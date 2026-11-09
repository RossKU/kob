// Rows per page and paging of the Portfolio's lists (Open orders, Order history, Fills). The lists are read whole (orders-data.ts: every open order, the history
// up to HISTORY_PAGES pages; the fills of every order that filled), so a page is a slice of what is loaded: the tab counts stay the totals.
import { useState } from 'preact/hooks';
import { t } from '../../i18n';
import { Button } from '../kit';
import { pageWindow, type PageWindow } from '../market/market-list';

export const PORTFOLIO_PAGE_SIZES = [10, 20, 50] as const;
export type PortfolioPageSize = (typeof PORTFOLIO_PAGE_SIZES)[number];
export const DEFAULT_PORTFOLIO_ROWS: PortfolioPageSize = 20;
const ROWS_KEY = 'kob.portfolio.rows';

/** A stored size, or the default for anything else. */
export const portfolioRowsOf = (raw: string | null | undefined): PortfolioPageSize => PORTFOLIO_PAGE_SIZES.find((n) => String(n) === raw) ?? DEFAULT_PORTFOLIO_ROWS;

/** The viewer's rows per page, kept in this browser (a blocked storage keeps it for the visit). */
export function usePortfolioRows(): [PortfolioPageSize, (n: PortfolioPageSize) => void] {
  const [size, setSize] = useState<PortfolioPageSize>(() => {
    try {
      return portfolioRowsOf(localStorage.getItem(ROWS_KEY));
    } catch {
      return DEFAULT_PORTFOLIO_ROWS;
    }
  });
  const set = (n: PortfolioPageSize) => {
    setSize(n);
    try {
      localStorage.setItem(ROWS_KEY, String(n));
    } catch {
      /* blocked storage */
    }
  };
  return [size, set];
}

/** The slice of `items` on page `page` (of `size` rows) and its window. */
export function pageOf<T>(items: readonly T[], size: number, page: number): { rows: T[]; window: PageWindow } {
  const window = pageWindow(items.length, size, page);
  return { rows: items.slice(window.from, window.to), window };
}

/** `‹ Newer   1–20 of 134   Older ›`, right-aligned under the list; nothing while the list is one page. `id`: the prefix of its test ids. */
export function PortfolioPager(props: { id: string; window: PageWindow; total: number; onPage: (page: number) => void }) {
  const w = props.window;
  if (w.pages <= 1) return null;
  return (
    <div class="pf-pager" data-testid={`${props.id}-pager`}>
      <Button small disabled={w.page === 0} onClick={() => props.onPage(w.page - 1)} data-testid={`${props.id}-newer`}>
        {t('orders.page.newer')}
      </Button>
      <span class="small muted num" data-testid={`${props.id}-showing`}>{t('market.list.showing', { from: w.from + 1, to: w.to, total: props.total })}</span>
      <Button small disabled={w.page >= w.pages - 1} onClick={() => props.onPage(w.page + 1)} data-testid={`${props.id}-older`}>
        {t('orders.page.older')}
      </Button>
    </div>
  );
}
