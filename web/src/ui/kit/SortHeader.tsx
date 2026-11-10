// The pieces of a sortable list: the header button of a column, the narrow-card control ("Sort: column, direction": cards have no header), the stored choice.
// The state, the click cycle and the comparison are in sort.ts. Used by the Portfolio's tables and the activity log.
import type { ComponentChildren } from 'preact';
import { useState } from 'preact/hooks';
import { t } from '../../i18n';
import { ariaSortOf, chooseSort, firstDir, nextSort, parseSort, type SortColumn, type SortDir, type SortState } from './sort';
import './sort.css';

const STORE_PREFIX = 'kob.sort.';

/** The viewer's sort of the list stored under `name`, kept in this browser (a blocked storage keeps it for the visit); none = the list's own order. */
export function useSortState<K extends string>(name: string, keys: readonly K[]): [SortState<K> | null, (s: SortState<K> | null) => void] {
  const [state, setState] = useState<SortState<K> | null>(() => {
    try {
      return parseSort(localStorage.getItem(STORE_PREFIX + name), keys);
    } catch {
      return null;
    }
  });
  const set = (s: SortState<K> | null) => {
    setState(s);
    try {
      if (s) localStorage.setItem(STORE_PREFIX + name, JSON.stringify(s));
      else localStorage.removeItem(STORE_PREFIX + name);
    } catch {
      /* blocked storage */
    }
  };
  return [state, set];
}

/** A stable text of the state, for the key of a list's page (another sort starts at the first page). */
export const sortKeyOf = (s: SortState | null): string => (s ? `${s.key}:${s.dir}` : '-');

export { ariaSortOf };

/** A column header you can press: a real button (the cell around it carries `aria-sort`, `ariaSortOf`). `own`: the column the list's own order stands for. */
export function SortButton<K extends string>(props: {
  id: string;
  col: SortColumn<K>;
  state: SortState<K> | null;
  own: SortColumn<K> | null;
  onSort: (s: SortState<K> | null) => void;
  /** the label, where it is not the column's (a shorter one) */
  children?: ComponentChildren;
}) {
  const aria = ariaSortOf(props.state, props.col.key, props.own);
  const mark = aria === 'ascending' ? '▲' : aria === 'descending' ? '▼' : '';
  return (
    <button
      type="button"
      class={`sortbtn${aria !== 'none' ? ' active' : ''}`}
      onClick={() => props.onSort(nextSort(props.state, props.col, props.own))}
      title={t('common.sort.by', { column: props.col.label })}
      data-testid={`${props.id}-sort-${props.col.key}`}
      data-sort={aria}
    >
      <span>{props.children ?? props.col.label}</span>
      <span class="sortbtn-mark" aria-hidden="true">{mark}</span>
    </button>
  );
}

/** A column's direction in words, for the control's button. */
const dirText = (d: SortDir) => t(d === 'asc' ? 'common.sort.asc' : 'common.sort.desc');

/**
 * Where a list is cards (no header): `Sort [column v] [up/down]`, in the pill style of the rows control. It reads and writes the same state as the headers.
 */
export function SortControl<K extends string>(props: {
  id: string;
  columns: readonly SortColumn<K>[];
  state: SortState<K> | null;
  own: SortColumn<K> | null;
  onSort: (s: SortState<K> | null) => void;
  class?: string;
}) {
  const cur: SortState<K> | null = props.state ?? (props.own ? { key: props.own.key, dir: firstDir(props.own.kind) } : null);
  const first = props.columns[0];
  const key = (cur?.key ?? first?.key) as K;
  const dir: SortDir = cur?.dir ?? (first ? firstDir(first.kind) : 'desc');
  return (
    <div class={`sortctl${props.class ? ` ${props.class}` : ''}`} data-testid={`${props.id}-sortctl`}>
      <span class="small muted" aria-hidden="true">{t('common.sort')}</span>
      <div class="segmented">
        <select
          aria-label={t('common.sort.column')}
          value={key}
          onChange={(e) => {
            const k = (e.currentTarget as HTMLSelectElement).value as K;
            const col = props.columns.find((c) => c.key === k)!;
            props.onSort(chooseSort(k, firstDir(col.kind), props.own));
          }}
          data-testid={`${props.id}-sortctl-column`}
        >
          {props.columns.map((c) => <option key={c.key} value={c.key}>{c.label}</option>)}
        </select>
        <button
          type="button"
          aria-label={t('common.sort.direction', { dir: dirText(dir) })}
          title={dirText(dir)}
          onClick={() => props.onSort(chooseSort(key, dir === 'asc' ? 'desc' : 'asc', props.own))}
          data-testid={`${props.id}-sortctl-dir`}
          data-dir={dir}
        >
          {dir === 'asc' ? '▲' : '▼'}
        </button>
      </div>
    </div>
  );
}
