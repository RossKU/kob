// A searchable choice of one asset: a plain drop-down does not scale past a few dozen tokens and has no search. The face is a button; the panel holds a search by
// ticker, name or covenant id and the matching tokens, each with its short id (tickers are not unique, the id is the identity). The value is always a covenant id (or
// `kas`). Keyboard: arrows and Enter in the search, Escape closes and puts the focus back on the face. The panel is fixed to the viewport and lives in the body, so a card
// or a row that cuts what overflows it cannot cut it and it never widens the page.
import type { ComponentChildren } from 'preact';
import { createPortal } from 'preact/compat';
import { useEffect, useId, useLayoutEffect, useMemo, useRef, useState } from 'preact/hooks';
import { t } from '../../i18n';
import { filterChoices, PICKER_ROWS, type AssetChoice } from './market-select';
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

const place = (el: HTMLElement) => {
  const b = el.getBoundingClientRect();
  const width = Math.min(352, window.innerWidth - 24);
  const top = b.bottom + 6;
  return { left: Math.max(12, Math.min(b.left, window.innerWidth - width - 12)), top, width, maxHeight: Math.max(180, window.innerHeight - top - 12) };
};

export function AssetPicker(props: AssetPickerProps) {
  const id = props['data-testid'];
  const [pos, setPos] = useState<ReturnType<typeof place> | null>(null);
  const [query, setQuery] = useState('');
  const [active, setActive] = useState(0);
  const listId = `ap-${useId()}`;
  const face = useRef<HTMLButtonElement>(null);
  const panel = useRef<HTMLDivElement>(null);
  const search = useRef<HTMLInputElement>(null);
  const list = useRef<HTMLUListElement>(null);
  const open = pos !== null;

  const names = useMemo(() => new Map(props.rows.map((r) => [r.covenantId.toLowerCase(), r.name])), [props.rows]);
  const found = useMemo(() => (open ? filterChoices(props.choices, props.rows, query) : []), [open, props.choices, props.rows, query]);
  const shown = found.slice(0, PICKER_ROWS);
  const current = props.choices.find((c) => c.value === props.value);

  const show = () => {
    setQuery('');
    setActive(0);
    setPos(place(face.current!));
  };
  const close = (refocus: boolean) => {
    setPos(null);
    if (refocus) face.current?.focus();
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

  // a layout effect: the listeners and the focus are there in the frame the panel appears. A scroll of the page moves the panel along with its face.
  useLayoutEffect(() => {
    if (!open) return;
    search.current?.focus({ preventScroll: true });
    const onDown = (e: Event) => {
      const el = e.target as Node;
      if (!panel.current?.contains(el) && !face.current?.contains(el)) close(false);
    };
    const follow = () => face.current && setPos(place(face.current));
    const onHash = () => close(false);
    document.addEventListener('pointerdown', onDown);
    window.addEventListener('scroll', follow);
    window.addEventListener('resize', follow);
    window.addEventListener('hashchange', onHash);
    return () => {
      document.removeEventListener('pointerdown', onDown);
      window.removeEventListener('scroll', follow);
      window.removeEventListener('resize', follow);
      window.removeEventListener('hashchange', onHash);
    };
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
        class="asset-select-face"
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
        <i class="asset-select-caret" aria-hidden="true">{'▾'}</i>
      </button>
      {pos
        ? createPortal(
            <div ref={panel} class="picker-panel" style={{ left: `${pos.left}px`, top: `${pos.top}px`, width: `${pos.width}px`, maxHeight: `${pos.maxHeight}px` }} data-testid={`${id}-panel`}>
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
                    class="picker-option"
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
                    {names.get(c.value) ? <span class="small muted picker-name">{names.get(c.value)}</span> : null}
                  </li>
                ))}
              </ul>
              {shown.length === 0 ? <p class="small muted picker-note" data-testid={`${id}-empty`}>{t('market.list.noMatch')}</p> : null}
              {found.length > shown.length ? <p class="small muted picker-note" data-testid={`${id}-more`}>{t('market.pick.more', { n: found.length - shown.length })}</p> : null}
            </div>,
            document.body,
          )
        : null}
    </span>
  );
}
