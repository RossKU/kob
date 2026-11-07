// What the market list and the activity log share: the page size control and the page turner.
import { t } from '../../i18n';
import { Button, Segmented } from '../kit';
import { LIST_PAGE_SIZES, type ListPageSize, type PageWindow } from './market-list';

/** `Rows 20 / 50 / 100`. `id`: the prefix of its test ids. */
export function RowsControl(props: { id: string; size: ListPageSize; onSize: (n: ListPageSize) => void }) {
  return (
    <div class="ml-size">
      <span class="small muted" aria-hidden="true">{t('market.list.rows')}</span>
      <Segmented<string>
        aria-label={t('market.list.rowsLabel')}
        value={String(props.size)}
        onChange={(v) => props.onSize(Number(v) as ListPageSize)}
        options={LIST_PAGE_SIZES.map((n) => ({ value: String(n), label: String(n), 'data-testid': `${props.id}-rows-${n}` }))}
        data-testid={`${props.id}-rows`}
      />
    </div>
  );
}

/** `‹ 1–20 of 48 ›`. Nothing when there is one page and no other to come. `id`: the prefix of its test ids. */
export function ListPager(props: { id: string; window: PageWindow; total: number; hasNext: boolean; loading?: boolean; onPage: (page: number) => void }) {
  const w = props.window;
  if (w.pages <= 1 && !props.hasNext) return null;
  return (
    <div class="ml-pager" data-testid={`${props.id}-pager`}>
      <Button small variant="secondary" disabled={w.page === 0} aria-label={t('market.list.prev')} title={t('market.list.prev')} onClick={() => props.onPage(w.page - 1)} data-testid={`${props.id}-prev`}>
        {'‹'}
      </Button>
      <span class="small muted" data-testid={`${props.id}-showing`}>{t('market.list.showing', { from: w.from + 1, to: w.to, total: props.total })}</span>
      <Button small variant="secondary" disabled={!props.hasNext} loading={props.loading} aria-label={t('market.list.next')} title={t('market.list.next')} onClick={() => props.onPage(w.page + 1)} data-testid={`${props.id}-next`}>
        {'›'}
      </Button>
    </div>
  );
}

/** A page was turned at the end of a long list: its start comes back into view (`scroll-margin-top` keeps it clear of the sticky header). */
export function showListStart(el: Element | null | undefined): void {
  if (el && el.getBoundingClientRect().top < 0) el.scrollIntoView({ block: 'start' });
}
