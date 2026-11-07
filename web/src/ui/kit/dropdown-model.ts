// The logic of the kit's drop-down (Dropdown.tsx), without a page: the options with their groups, what each key does, and where the panel goes.
// An index is always one into the flat list of options; -1 is no option.

export interface DropdownOption {
  value: string;
  label: string;
  disabled?: boolean;
}

/** Options under a heading of their own. */
export interface DropdownGroup {
  group: string;
  options: readonly DropdownOption[];
}

export type DropdownEntry = DropdownOption | DropdownGroup;

export const isGroup = (e: DropdownEntry): e is DropdownGroup => 'group' in e;

/** The options in the order they are shown, groups dissolved. */
export const flatOptions = (entries: readonly DropdownEntry[]): DropdownOption[] => entries.flatMap((e) => (isGroup(e) ? [...e.options] : [e]));

const enabled = (o: DropdownOption | undefined): boolean => !!o && !o.disabled;

/** The next option that can be chosen from `from` in the direction `dir`; `from` itself at the end of the list (the keys do not wrap). */
export function stepIndex(flat: readonly DropdownOption[], from: number, dir: 1 | -1): number {
  for (let i = from + dir; i >= 0 && i < flat.length; i += dir) if (enabled(flat[i])) return i;
  return from;
}

export const firstIndex = (flat: readonly DropdownOption[]): number => stepIndex(flat, -1, 1);

export function lastIndex(flat: readonly DropdownOption[]): number {
  const i = stepIndex(flat, flat.length, -1);
  return i === flat.length ? -1 : i;
}

/** Where the list opens: on the chosen option, or the first that can be chosen when the chosen one cannot or there is none. */
export function startIndex(flat: readonly DropdownOption[], value: string): number {
  const i = flat.findIndex((o) => o.value === value);
  return enabled(flat[i]) ? i : firstIndex(flat);
}

/**
 * The option a typed text leads to. One letter, or the same letter again and again, walks the options that start with it from the one after `from`; a longer text stays
 * on `from` while it still matches. Wraps round; -1 when nothing matches.
 */
export function typeMatch(flat: readonly DropdownOption[], from: number, typed: string): number {
  const text = typed.toLowerCase();
  const walk = [...text].every((c) => c === text[0]);
  const want = walk ? text.slice(0, 1) : text;
  const start = walk ? from + 1 : Math.max(from, 0);
  for (let n = 0; n < flat.length; n++) {
    const i = (start + n) % flat.length;
    if (enabled(flat[i]) && flat[i]!.label.toLowerCase().startsWith(want)) return i;
  }
  return -1;
}

/** Letters typed further apart than this start a new search. */
export const TYPE_PAUSE_MS = 500;

export interface KeyState {
  open: boolean;
  /** the option the keys stand on */
  active: number;
  /** the letters of the search under way */
  typed: string;
  typedAt: number;
}

export interface KeyResult extends KeyState {
  /** the key was used: its default action does not run */
  handled: boolean;
  /** the option the key chose */
  pick?: DropdownOption;
}

/**
 * What one key does. Closed, the arrows, Home, End and a letter open the list; Enter and Space are left to the button, whose click opens it. A letter never chooses by
 * itself: an order form changes only on a choice the user can see. Open: arrows, Home, End and letters move, Enter and Space choose (a space inside a search under way is
 * a letter of it), Escape and Tab close without choosing, and Tab keeps its default action, the next control after the button.
 */
export function dropdownKey(flat: readonly DropdownOption[], value: string, s: KeyState, key: string, now: number): KeyResult {
  const moveTo = (active: number): KeyResult => ({ open: true, active, typed: '', typedAt: 0, handled: true });
  const shut = (handled: boolean, pick?: DropdownOption): KeyResult => ({ open: false, active: -1, typed: '', typedAt: 0, handled, ...(pick ? { pick } : {}) });
  const unused: KeyResult = { ...s, handled: false };
  const typing = s.typed !== '' && now - s.typedAt < TYPE_PAUSE_MS;

  if (key.length === 1 && (key !== ' ' || (s.open && typing))) {
    const typed = (typing ? s.typed : '') + key;
    const hit = typeMatch(flat, s.open ? s.active : startIndex(flat, value), typed);
    if (hit !== -1) return { open: true, active: hit, typed, typedAt: now, handled: true };
    return s.open ? { ...s, typed, typedAt: now, handled: true } : unused;
  }
  if (!s.open) {
    if (firstIndex(flat) === -1) return unused;
    if (key === 'ArrowDown' || key === 'ArrowUp') return moveTo(startIndex(flat, value));
    if (key === 'Home') return moveTo(firstIndex(flat));
    if (key === 'End') return moveTo(lastIndex(flat));
    return unused;
  }
  switch (key) {
    case 'ArrowDown':
      return moveTo(stepIndex(flat, s.active, 1));
    case 'ArrowUp':
      return moveTo(stepIndex(flat, s.active, -1));
    case 'Home':
      return moveTo(firstIndex(flat));
    case 'End':
      return moveTo(lastIndex(flat));
    case 'Enter':
    case ' ':
      return shut(true, enabled(flat[s.active]) ? flat[s.active] : undefined);
    case 'Escape':
      return shut(true);
    case 'Tab':
      return shut(false);
    default:
      return unused;
  }
}

const EDGE = 12;
const GAP = 6;
/** Below this there is no list to speak of on that side of the button. */
const MIN_ROOM = 120;
const ROW = 44;
const HEADING = 30;
const FRAME = 18;
/** `.picker-list` stops at 22rem. */
const LIST_MAX = 352;

/** The height the panel of these entries wants: an estimate that only decides the side it opens on. The list scrolls in whatever it gets. */
export const panelHeight = (entries: readonly DropdownEntry[]): number =>
  Math.min(LIST_MAX, flatOptions(entries).length * ROW + entries.filter(isGroup).length * HEADING) + FRAME;

export interface PanelPlace {
  left: number;
  width: number;
  maxHeight: number;
  /** under the button */
  top?: number;
  /** over it, from the bottom of the window: a list that shrinks stays against the button */
  bottom?: number;
}

/**
 * Where the panel of a button goes: in the window with a margin, wherever the button is. Under the button where the list fits or that side has the most room, over it
 * otherwise. A window with no room on either side gets the panel over the button itself.
 */
export function placePanel(
  face: { left: number; top: number; bottom: number },
  view: { width: number; height: number },
  want: { width: number; height: number },
): PanelPlace {
  const width = Math.min(want.width, view.width - 2 * EDGE);
  const left = Math.max(EDGE, Math.min(face.left, view.width - width - EDGE));
  const most = view.height - 2 * EDGE;
  const below = Math.min(most, view.height - EDGE - face.bottom - GAP);
  const above = Math.min(most, face.top - GAP - EDGE);
  const height = Math.min(want.height, most);
  if (Math.max(below, above) < Math.min(height, MIN_ROOM)) return { left, width, maxHeight: height, top: view.height - EDGE - height };
  if (below >= height || below >= above) return { left, width, maxHeight: below, top: view.height - EDGE - below };
  return { left, width, maxHeight: above, bottom: view.height - EDGE - above };
}
