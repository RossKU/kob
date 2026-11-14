// The market list's filters in one menu ("Filter ▾"): the Favorites and the Hidden views, the
// wallet's own tokens, and the tokens without a name. They were separate buttons above and below
// the list; one menu takes less room on a phone and says how many are on. The panel is the kit's
// drop-down panel (kit/Dropdown.tsx `usePickerPanel`): fixed to the window, so neither the card
// nor the table cuts it, and it never widens the page. Each entry is a checkbox of the menu; a
// choice closes the menu, as the row's "⋯" menu does.
import { createPortal } from 'preact/compat';
import { useLayoutEffect, useRef } from 'preact/hooks';
import { t } from '../../i18n';
import { usePickerPanel } from '../kit';
import { activeFilters, type FilterKey, type ListFilters } from './held-list';

export interface FilterItem {
  key: FilterKey;
  label: string;
  /** the entry keeps the test id its button had (`token-list-favorites`, ...) */
  testid: string;
}

export interface ListFilterMenuProps {
  filters: ListFilters;
  /**
   * the entries the menu offers now, in order (one that would change nothing is left out by the
   * caller)
   */
  items: readonly FilterItem[];
  onToggle: (key: FilterKey) => void;
  onClear: () => void;
}

/** The size the panel asks for: as wide as a long entry, as tall as every entry. */
const PANEL_WIDTH = 280;
const ROW = 44;
const FRAME = 18;

export function ListFilterMenu(props: ListFilterMenuProps) {
  const face = useRef<HTMLButtonElement>(null);
  const box = useRef<HTMLDivElement>(null);
  const n = activeFilters(props.filters);
  const rows = props.items.length + (n > 0 ? 1 : 0);
  const pane = usePickerPanel(face, box, () => ({ width: PANEL_WIDTH, height: rows * ROW + FRAME }));
  const { open, close } = pane;

  // The focus goes to the first entry when the menu opens, so the arrows and Escape work from the
  // keyboard at once.
  useLayoutEffect(() => {
    if (open) box.current?.querySelector<HTMLElement>('[role^="menuitem"]')?.focus({ preventScroll: true });
  }, [open]);

  const choose = (f: () => void) => {
    close(true);
    f();
  };
  const onKey = (e: KeyboardEvent) => {
    const entries = [...(box.current?.querySelectorAll<HTMLElement>('[role^="menuitem"]') ?? [])];
    const at = entries.indexOf(document.activeElement as HTMLElement);
    const go = (i: number) => entries[(i + entries.length) % entries.length]?.focus({ preventScroll: true });
    if (e.key === 'ArrowDown') go(at + 1);
    else if (e.key === 'ArrowUp') go(at - 1);
    else if (e.key === 'Home') go(0);
    else if (e.key === 'End') go(entries.length - 1);
    else if (e.key === 'Escape') close(true);
    else if (e.key === 'Tab') return close(false);
    else return;
    e.preventDefault();
    e.stopPropagation();
  };

  const count = n === 1 ? t('market.filter.active.one') : t('market.filter.active.other', { n });
  return (
    <span class="ml-filter">
      <button
        ref={face}
        type="button"
        class={`btn btn-sm ml-filter-face${n > 0 ? ' is-on' : ''}`}
        aria-haspopup="menu"
        aria-expanded={open ? 'true' : 'false'}
        aria-label={n > 0 ? `${t('market.filter.label')}: ${count}` : t('market.filter.label')}
        data-active={String(n)}
        data-testid="token-list-filter"
        onClick={() => (open ? close(false) : pane.show())}
      >
        {t('market.filter.button')}
        {n > 0 ? <span class="ml-filter-count" title={count} data-testid="token-list-filter-count">{n}</span> : null}
        <i class="picker-caret" aria-hidden="true">{'▾'}</i>
      </button>
      {open
        ? createPortal(
            <div ref={box} class="picker-panel ml-filter-panel" style={pane.style} data-testid="token-list-filter-panel">
              <div class="picker-list" role="menu" aria-label={t('market.filter.label')} onKeyDown={onKey}>
                {props.items.map((it) => (
                  <button
                    key={it.key}
                    type="button"
                    class="picker-option picker-option-row ml-filter-item"
                    role="menuitemcheckbox"
                    aria-checked={props.filters[it.key] ? 'true' : 'false'}
                    data-testid={it.testid}
                    onClick={() => choose(() => props.onToggle(it.key))}
                  >
                    <span class="picker-label">{it.label}</span>
                    <i class="picker-check" aria-hidden="true">{props.filters[it.key] ? '✓' : ''}</i>
                  </button>
                ))}
                {n > 0 ? (
                  <button type="button" class="picker-option picker-option-row ml-filter-item ml-filter-clear" role="menuitem" data-testid="token-list-filter-clear" onClick={() => choose(props.onClear)}>
                    <span class="picker-label">{t('market.filter.clear')}</span>
                  </button>
                ) : null}
              </div>
            </div>,
            pane.host(),
          )
        : null}
    </span>
  );
}
