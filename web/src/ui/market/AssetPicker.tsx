// A searchable choice of one asset: a plain drop-down does not scale past a few dozen tokens and has no search. The face is a button; the panel holds a search by
// ticker, name or covenant id and the matching tokens, each with its short id (tickers are not unique, the id is the identity). The value is always a covenant id (or
// `kas`). Keyboard: arrows and Enter in the search, Escape closes and puts the focus back on the face. The panel and its rows are the kit's drop-down (kit/Dropdown.tsx):
// fixed to the viewport and in the body, so a card or a row that cuts what overflows it cannot cut it and it never widens the page.
import type { ComponentChildren } from 'preact';
import { createPortal } from 'preact/compat';
import { useEffect, useId, useLayoutEffect, useMemo, useRef, useState } from 'preact/hooks';
import { t } from '../../i18n';
import { usePickerPanel } from '../kit';
import { filterChoices, PICKER_ROWS, pickerName, type AssetChoice } from './market-select';
import type { TokenRow } from './token-model';

export interface AssetPickerProps {
  value: string | null;
  choices: readonly AssetChoice[];
  /** the rows behind the choices: what the search looks through */
  rows: readonly TokenRow[];
  /** the accessible name */
  label: string;
  /** the face while nothing is chosen, or when the value is no choice */
  placeholder: ComponentChildren;
  onChange: (value: string) => void;
  /** a new number opens the panel (a first choice leads to the second) */
  openKey?: number;
  'data-testid': string;
}

/** The width the panel takes, and the height of the search and a full list. */
const PANEL = { width: 352, height: 420 };

export function AssetPicker(props: AssetPickerProps) {
  const id = props['data-testid'];
  const [query, setQuery] = useState('');
  const [active, setActive] = useState(0);
  const listId = `ap-${useId()}`;
  const face = useRef<HTMLButtonElement>(null);
  const panel = useRef<HTMLDivElement>(null);
  const search = useRef<HTMLInputElement>(null);
  const list = useRef<HTMLUListElement>(null);
  const pane = usePickerPanel(face, panel, () => PANEL);
  const { open, close } = pane;

  const names = useMemo(() => new Map(props.rows.map((r) => [r.covenantId.toLowerCase(), r.name])), [props.rows]);
  const found = useMemo(() => (open ? filterChoices(props.choices, props.rows, query) : []), [open, props.choices, props.rows, query]);
  const shown = found.slice(0, PICKER_ROWS);
  const current = props.choices.find((c) => c.value === props.value);

  const show = () => {
    setQuery('');
    setActive(0);
    pane.show();
  };
  const pick = (c: AssetChoice | undefined) => {
    if (!c) return;
    close(true);
    props.onChange(c.value);
  };

  useEffect(() => {
    if (props.openKey) show();
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [props.openKey]);

  // a layout effect: the focus is there in the frame the panel appears
  useLayoutEffect(() => {
    if (open) search.current?.focus({ preventScroll: true });
  }, [open]);

  useLayoutEffect(() => {
    list.current?.querySelector('[aria-selected="true"]')?.scrollIntoView({ block: 'nearest' });
  }, [active]);

  const onKey = (e: KeyboardEvent) => {
    if (e.key === 'ArrowDown' || e.key === 'ArrowUp') {
      e.preventDefault();
      if (shown.length) setActive((a) => (a + (e.key === 'ArrowDown' ? 1 : shown.length - 1)) % shown.length);
    } else if (e.key === 'Enter') {
      e.preventDefault();
      pick(shown[active]);
    } else if (e.key === 'Escape') {
      e.preventDefault();
      e.stopPropagation();
      close(true);
    } else if (e.key === 'Tab') {
      close(false);
    }
  };

  return (
    <span class="asset-select" data-value={props.value ?? ''}>
      <button
        ref={face}
        type="button"
        class="picker-face asset-select-face"
        translate={false}
        aria-label={props.label}
        title={current?.label ?? props.label}
        aria-haspopup="listbox"
        aria-expanded={open ? 'true' : 'false'}
        data-value={props.value ?? ''}
        data-testid={id}
        onClick={() => (open ? close(false) : show())}
      >
        {current?.short || current?.label || props.placeholder}
        <i class="picker-caret" aria-hidden="true">{'▾'}</i>
      </button>
      {open
        ? createPortal(
            <div ref={panel} class="picker-panel" style={pane.style} data-testid={`${id}-panel`}>
              <input
                ref={search}
                class="input picker-search"
                type="search"
                role="combobox"
                aria-expanded="true"
                aria-controls={listId}
                aria-autocomplete="list"
                aria-activedescendant={shown.length ? `${listId}-${active}` : undefined}
                aria-label={props.label}
                placeholder={t('market.pick.search')}
                autocomplete="off"
                spellcheck={false}
                value={query}
                onInput={(e) => {
                  setQuery((e.currentTarget as HTMLInputElement).value);
                  setActive(0);
                }}
                onKeyDown={onKey}
                data-testid={`${id}-search`}
              />
              <ul ref={list} id={listId} class="picker-list" role="listbox" aria-label={props.label}>
                {shown.map((c, i) => (
                  <li
                    key={c.value}
                    id={`${listId}-${i}`}
                    class={`picker-option picker-option-asset${i === active ? ' is-active' : ''}`}
                    title={c.label}
                    role="option"
                    aria-selected={i === active ? 'true' : 'false'}
                    data-value={c.value}
                    data-testid={`${id}-option-${c.value}`}
                    // the search keeps the focus: a press on a choice is not a blur of the panel
                    onPointerDown={(e) => e.preventDefault()}
                    onPointerMove={() => i !== active && setActive(i)}
                    onClick={() => pick(c)}
                  >
                    <span class="picker-label">{c.label}</span>
                    <span class="small muted picker-name">{pickerName(names.get(c.value), c.short)}</span>
                  </li>
                ))}
              </ul>
              {shown.length === 0 ? <p class="small muted picker-note" data-testid={`${id}-empty`}>{t('market.list.noMatch')}</p> : null}
              {found.length > shown.length ? <p class="small muted picker-note" data-testid={`${id}-more`}>{t('market.pick.more', { n: found.length - shown.length })}</p> : null}
            </div>,
            pane.host(),
          )
        : null}
    </span>
  );
}
