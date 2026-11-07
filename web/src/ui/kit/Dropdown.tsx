// The app's one drop-down, in place of the browser's: its list looks the same in both themes and on every system, and the same as the asset picker, which shares the panel
// and its styles (market/AssetPicker.tsx adds a search on top). The face is a button; the focus goes to the list while it is open and back to the button after. The panel
// is fixed to the window and lives outside the page's cards, so nothing cuts it: in the body, or in the dialog the button is in (a modal dialog hides the rest of the page
// from a screen reader, and stands above what is in the body). The keys and the placing are in dropdown-model.ts.
import type { RefObject } from 'preact';
import { createPortal } from 'preact/compat';
import { useEffect, useId, useLayoutEffect, useRef, useState } from 'preact/hooks';
import { dropdownKey, flatOptions, isGroup, panelHeight, placePanel, startIndex, type DropdownEntry, type DropdownOption, type PanelPlace } from './dropdown-model';

/** Narrower buttons (the book's grouping) still get a list that reads well. */
const MIN_PANEL_WIDTH = 200;

/**
 * The panel of a button: where it stands, what closes it (a press outside, a change of page), and that it follows the button when the page, a dialog or the window moves.
 * `want` is the size the panel would like, from the button's width.
 */
export function usePickerPanel(face: RefObject<HTMLElement>, panel: RefObject<HTMLElement>, want: (faceWidth: number) => { width: number; height: number }) {
  const [place, setPlace] = useState<PanelPlace | null>(null);
  const wanted = useRef(want);
  wanted.current = want;
  const open = place !== null;

  const fit = (): PanelPlace | null => {
    if (!face.current) return null;
    const b = face.current.getBoundingClientRect();
    return placePanel(b, { width: document.documentElement.clientWidth, height: window.innerHeight }, wanted.current(b.width));
  };
  const show = () => setPlace(fit());
  const close = (refocus: boolean) => {
    setPlace(null);
    if (refocus) face.current?.focus({ preventScroll: true });
  };

  // a layout effect: the listeners are there in the frame the panel appears
  useLayoutEffect(() => {
    if (!open) return;
    const onDown = (e: Event) => {
      const el = e.target as Element;
      if (panel.current?.contains(el) || face.current?.contains(el)) return;
      close(true);
      // the press that closes the list is spent on it: the dialog behind does not take it for a press on its backdrop as well
      if (el.classList?.contains('modal-backdrop')) el.addEventListener('mousedown', (m) => m.stopImmediatePropagation(), { capture: true, once: true });
    };
    const follow = () => setPlace(fit());
    // the list's own scroll moves nothing
    const onScroll = (e: Event) => {
      if (!panel.current?.contains(e.target as Node)) follow();
    };
    const onHash = () => close(false);
    document.addEventListener('pointerdown', onDown);
    // capturing: the scroll of a dialog or a card does not bubble
    window.addEventListener('scroll', onScroll, true);
    window.addEventListener('resize', follow);
    window.addEventListener('hashchange', onHash);
    return () => {
      document.removeEventListener('pointerdown', onDown);
      window.removeEventListener('scroll', onScroll, true);
      window.removeEventListener('resize', follow);
      window.removeEventListener('hashchange', onHash);
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [open]);

  const px = (n: number | undefined) => (n === undefined ? undefined : `${n}px`);
  return {
    open,
    show,
    close,
    /** where the panel is rendered */
    host: (): Element => face.current?.closest('[role="dialog"]') ?? document.body,
    style: place ? { left: px(place.left), width: px(place.width), maxHeight: px(place.maxHeight), top: px(place.top), bottom: px(place.bottom) } : undefined,
  };
}

export interface DropdownProps {
  value: string;
  /** options, and groups of options, in the order they are shown */
  options: readonly DropdownEntry[];
  onChange: (value: string) => void;
  /** id of the button: what a `<label for>` names (generated when missing) */
  id?: string;
  /** id of the visible label */
  labelledBy?: string;
  /** the accessible name where no label is on screen */
  label?: string;
  describedBy?: string;
  invalid?: boolean;
  disabled?: boolean;
  title?: string;
  /** on the button; its panel and options take `<id>-panel` and `<id>-option-<value>` */
  'data-testid'?: string;
}

export function Dropdown(props: DropdownProps) {
  const auto = useId();
  const id = props.id ?? `dd${auto}`;
  const listId = `${id}-list`;
  const testId = props['data-testid'];
  const face = useRef<HTMLButtonElement>(null);
  const panelBox = useRef<HTMLDivElement>(null);
  const list = useRef<HTMLDivElement>(null);
  const [active, setActive] = useState(-1);
  const typing = useRef({ typed: '', typedAt: 0 });
  const panel = usePickerPanel(face, panelBox, (w) => ({ width: Math.max(w, MIN_PANEL_WIDTH), height: panelHeight(props.options) }));
  const { open } = panel;

  const flat = flatOptions(props.options);
  const text = flat.find((o) => o.value === props.value)?.label ?? '';

  const show = (at: number) => {
    setActive(at);
    panel.show();
  };
  const choose = (o: DropdownOption | undefined) => {
    panel.close(true);
    if (o && o.value !== props.value) props.onChange(o.value);
  };

  const onKey = (e: KeyboardEvent) => {
    if (e.ctrlKey || e.metaKey || e.altKey) return;
    const r = dropdownKey(flat, props.value, { open, active, ...typing.current }, e.key, Date.now());
    typing.current = { typed: r.typed, typedAt: r.typedAt };
    if (r.handled) {
      e.preventDefault();
      e.stopPropagation();
    }
    if (open && !r.open) choose(r.pick);
    else if (!open && r.open) show(r.active);
    else if (open) setActive(r.active);
  };

  useLayoutEffect(() => {
    if (open) list.current?.focus({ preventScroll: true });
  }, [open]);
  useLayoutEffect(() => {
    if (open) list.current?.querySelector('.is-active')?.scrollIntoView({ block: 'nearest' });
  }, [open, active]);
  useEffect(() => {
    if (open && props.disabled) panel.close(false);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [props.disabled]);

  const row = (o: DropdownOption, i: number) => (
    <div
      key={o.value}
      id={`${listId}-${i}`}
      class={`picker-option picker-option-row${i === active ? ' is-active' : ''}`}
      role="option"
      aria-selected={o.value === props.value ? 'true' : 'false'}
      aria-disabled={o.disabled ? 'true' : undefined}
      data-value={o.value}
      data-testid={testId ? `${testId}-option-${o.value}` : undefined}
      onPointerMove={() => !o.disabled && i !== active && setActive(i)}
      onClick={() => !o.disabled && choose(o)}
    >
      <span class="picker-label">{o.label}</span>
      {o.value === props.value ? <i class="picker-check" aria-hidden="true">{'✓'}</i> : null}
    </div>
  );

  let at = 0;
  const rows = props.options.map((e, g) => {
    if (!isGroup(e)) return row(e, at++);
    const heading = `${listId}-g${g}`;
    return (
      <div key={heading} role="group" aria-labelledby={heading}>
        <div id={heading} class="picker-group" role="presentation">{e.group}</div>
        {e.options.map((o) => row(o, at++))}
      </div>
    );
  });

  // the name is the label and the value, what a screen reader says for a native control
  const name = props.labelledBy ? { 'aria-labelledby': `${props.labelledBy} ${id}` } : { 'aria-label': `${props.label ?? ''}: ${text}` };
  const listName = props.labelledBy ? { 'aria-labelledby': props.labelledBy } : { 'aria-label': props.label };
  return (
    <>
      <button
        ref={face}
        id={id}
        type="button"
        class="select picker-face"
        {...name}
        aria-haspopup="listbox"
        aria-expanded={open ? 'true' : 'false'}
        aria-controls={open ? listId : undefined}
        aria-describedby={props.describedBy}
        aria-invalid={props.invalid ? 'true' : undefined}
        title={props.title}
        disabled={props.disabled}
        data-value={props.value}
        data-testid={testId}
        onClick={() => (open ? panel.close(true) : show(startIndex(flat, props.value)))}
        onKeyDown={onKey}
      >
        {/* keyed: a page translator rewrites a text node in place, so a new text is a new element (Field.tsx) */}
        <span key={text} class="picker-face-text">{text}</span>
        <i class="picker-caret" aria-hidden="true">{'▾'}</i>
      </button>
      {open
        ? createPortal(
            <div ref={panelBox} class="picker-panel" style={panel.style} data-testid={testId ? `${testId}-panel` : undefined}>
              <div
                ref={list}
                id={listId}
                class="picker-list"
                role="listbox"
                tabIndex={-1}
                {...listName}
                aria-activedescendant={active >= 0 ? `${listId}-${active}` : undefined}
                onKeyDown={onKey}
              >
                {rows}
              </div>
            </div>,
            panel.host(),
          )
        : null}
    </>
  );
}
