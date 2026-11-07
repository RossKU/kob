import { readFileSync } from 'node:fs';
import { describe, expect, it } from 'vitest';
import { TYPE_GROUPS } from '../ticket/form-state';
import {
  dropdownKey, firstIndex, flatOptions, lastIndex, panelHeight, placePanel, startIndex, stepIndex, typeMatch, TYPE_PAUSE_MS,
  type DropdownEntry, type DropdownOption, type KeyResult, type KeyState,
} from './dropdown-model';

const o = (value: string, label = value, disabled = false): DropdownOption => ({ value, label, ...(disabled ? { disabled } : {}) });

/** Rows 0 Limit, 1 Market, 2 Stop (off), 3 Stop-limit, 4 Take-profit, 5 TWAP, 6 Trailing (off): disabled rows in the middle and at the end. */
const ENTRIES: DropdownEntry[] = [
  { group: 'Basic', options: [o('limit', 'Limit'), o('market', 'Market')] },
  { group: 'Trigger', options: [o('stop', 'Stop', true), o('stopLimit', 'Stop-limit'), o('takeProfit', 'Take-profit')] },
  o('twap', 'TWAP'),
  o('trail', 'Trailing', true),
];
const FLAT = flatOptions(ENTRIES);
const closed: KeyState = { open: false, active: -1, typed: '', typedAt: 0 };
const openAt = (active: number): KeyState => ({ ...closed, open: true, active });

describe('options and groups', () => {
  it('groups dissolve into one list in the order shown, a loose option keeps its place', () => {
    expect(FLAT.map((x) => x.value)).toEqual(['limit', 'market', 'stop', 'stopLimit', 'takeProfit', 'twap', 'trail']);
  });

  it('the order types keep their values and their order', () => {
    const entries = TYPE_GROUPS.map((g) => ({ group: g.group, options: g.types.map((v) => o(v)) }));
    expect(flatOptions(entries).map((x) => x.value)).toEqual(TYPE_GROUPS.flatMap((g) => g.types));
  });

  it('a step skips a disabled option and stays at the end of the list', () => {
    expect(stepIndex(FLAT, 1, 1)).toBe(3);
    expect(stepIndex(FLAT, 3, -1)).toBe(1);
    expect(stepIndex(FLAT, 5, 1)).toBe(5);
    expect(stepIndex(FLAT, 0, -1)).toBe(0);
  });

  it('first and last are the ones that can be chosen; none of them in a list of disabled options', () => {
    expect(firstIndex(FLAT)).toBe(0);
    expect(lastIndex(FLAT)).toBe(5);
    expect(firstIndex([o('a', 'a', true)])).toBe(-1);
    expect(lastIndex([o('a', 'a', true)])).toBe(-1);
    expect(lastIndex([])).toBe(-1);
  });

  it('the list opens on the chosen option; on the first it can if that one is disabled or not in the list', () => {
    expect(startIndex(FLAT, 'takeProfit')).toBe(4);
    expect(startIndex(FLAT, 'stop')).toBe(0);
    expect(startIndex(FLAT, 'gone')).toBe(0);
    expect(startIndex([o('a', 'a', true)], 'a')).toBe(-1);
  });
});

describe('typing', () => {
  it('one letter goes to the next option that starts with it, in either case, and wraps round', () => {
    expect(typeMatch(FLAT, 0, 'm')).toBe(1);
    expect(typeMatch(FLAT, 0, 'M')).toBe(1);
    expect(typeMatch(FLAT, 4, 't')).toBe(5);
    expect(typeMatch(FLAT, 5, 't')).toBe(4);
  });

  it('the same letter again walks on; a longer text stays where it still matches', () => {
    expect(typeMatch(FLAT, 4, 'tt')).toBe(5);
    expect(typeMatch(FLAT, 3, 'st')).toBe(3);
    expect(typeMatch(FLAT, 0, 'tw')).toBe(5);
  });

  it('a disabled option is passed over, and no match is -1', () => {
    expect(typeMatch(FLAT, 1, 's')).toBe(3);
    expect(typeMatch(FLAT, 0, 'trail')).toBe(-1);
    expect(typeMatch(FLAT, 0, 'x')).toBe(-1);
    expect(typeMatch([], -1, 'x')).toBe(-1);
  });
});

describe('the keys', () => {
  const press = (s: KeyState, key: string, now = 1000, value = 'market'): KeyResult => dropdownKey(FLAT, value, s, key, now);

  it('closed: an arrow opens on the chosen option, Home and End on the first and the last', () => {
    expect(press(closed, 'ArrowDown')).toMatchObject({ open: true, active: 1, handled: true });
    expect(press(closed, 'ArrowUp')).toMatchObject({ open: true, active: 1, handled: true });
    expect(press(closed, 'Home')).toMatchObject({ open: true, active: 0, handled: true });
    expect(press(closed, 'End')).toMatchObject({ open: true, active: 5, handled: true });
  });

  it('closed: Enter and Space are left to the button, Escape and Tab to the page', () => {
    for (const key of ['Enter', ' ', 'Escape', 'Tab']) expect(press(closed, key)).toMatchObject({ open: false, handled: false });
  });

  it('closed: a letter opens the list on its option and chooses nothing; one that matches nothing does nothing', () => {
    const r = press(closed, 't');
    expect(r).toMatchObject({ open: true, active: 4, handled: true });
    expect(r.pick).toBeUndefined();
    expect(press(closed, 'x')).toMatchObject({ open: false, handled: false });
  });

  it('closed: a list with nothing to choose stays shut', () => {
    expect(dropdownKey([o('a', 'a', true)], 'a', closed, 'ArrowDown', 0)).toMatchObject({ open: false, handled: false });
  });

  it('open: the arrows move over the disabled options and stop at the ends, Home and End go to the ends', () => {
    expect(press(openAt(1), 'ArrowDown')).toMatchObject({ open: true, active: 3 });
    expect(press(openAt(3), 'ArrowUp')).toMatchObject({ open: true, active: 1 });
    expect(press(openAt(5), 'ArrowDown')).toMatchObject({ open: true, active: 5 });
    expect(press(openAt(0), 'ArrowUp')).toMatchObject({ open: true, active: 0 });
    expect(press(openAt(3), 'Home')).toMatchObject({ open: true, active: 0 });
    expect(press(openAt(3), 'End')).toMatchObject({ open: true, active: 5 });
  });

  it('open: Enter and Space choose the option the keys stand on and close', () => {
    for (const key of ['Enter', ' ']) {
      const r = press(openAt(3), key);
      expect(r).toMatchObject({ open: false, handled: true });
      expect(r.pick?.value).toBe('stopLimit');
    }
  });

  it('open: Enter on a list with nothing active closes and chooses nothing', () => {
    expect(press(openAt(-1), 'Enter').pick).toBeUndefined();
    expect(press(openAt(2), 'Enter').pick).toBeUndefined();
  });

  it('open: Escape closes without a choice and keeps the key from the dialog behind; Tab closes and goes on to the next control', () => {
    const esc = press(openAt(3), 'Escape');
    expect(esc).toMatchObject({ open: false, handled: true });
    expect(esc.pick).toBeUndefined();
    const tab = press(openAt(3), 'Tab');
    expect(tab).toMatchObject({ open: false, handled: false });
    expect(tab.pick).toBeUndefined();
  });

  it('open: another key is not used', () => {
    expect(press(openAt(3), 'F5')).toMatchObject({ open: true, active: 3, handled: false });
  });

  it('letters typed in time add up; after the pause a new search starts', () => {
    const first = press(openAt(0), 's', 1000);
    expect(first).toMatchObject({ active: 3, typed: 's', typedAt: 1000 });
    expect(press(first, 't', 1000 + TYPE_PAUSE_MS - 1)).toMatchObject({ active: 3, typed: 'st' });
    expect(press(first, 't', 1000 + TYPE_PAUSE_MS)).toMatchObject({ active: 4, typed: 't' });
  });

  it('a space inside a search is a letter of it, not a choice; after the pause it chooses again', () => {
    const list = [o('a', 'Good till date'), o('b', 'Good today')];
    const typed = dropdownKey(list, 'a', openAt(0), 'g', 1000);
    const spaced = dropdownKey(list, 'a', { ...typed, typed: 'good', typedAt: 1000 }, ' ', 1100);
    expect(spaced).toMatchObject({ open: true, typed: 'good ', handled: true });
    expect(spaced.pick).toBeUndefined();
    expect(dropdownKey(list, 'a', spaced, ' ', 1100 + TYPE_PAUSE_MS).pick?.value).toBe(list[spaced.active]!.value);
  });

  it('a letter that matches nothing while open keeps the place and is used', () => {
    expect(press(openAt(3), 'x')).toMatchObject({ open: true, active: 3, handled: true, typed: 'x' });
  });

  it('a move ends a search', () => {
    expect(press({ ...openAt(0), typed: 's', typedAt: 990 }, 'ArrowDown', 1000)).toMatchObject({ typed: '', typedAt: 0 });
  });
});

describe('the panel', () => {
  const view = { width: 390, height: 844 };
  const want = { width: 300, height: 370 };
  const face = (top: number, left = 16, bottom = top + 38) => ({ left, top, bottom });
  /** The box of a panel of `height` at its place, in the window's coordinates. */
  const box = (p: ReturnType<typeof placePanel>, height: number) => {
    const h = Math.min(height, p.maxHeight);
    const top = p.top ?? view.height - p.bottom! - h;
    return { top, bottom: top + h, left: p.left, right: p.left + p.width };
  };

  it('opens under the button when the list fits', () => {
    expect(placePanel(face(100), view, want)).toEqual({ left: 16, width: 300, top: 144, maxHeight: 844 - 12 - 144 });
  });

  it('near the bottom of the window it opens over the button, anchored to it from below', () => {
    const p = placePanel(face(760), view, want);
    expect(p.top).toBeUndefined();
    expect(p.bottom).toBe(844 - 760 + 6);
    expect(p.maxHeight).toBe(760 - 6 - 12);
  });

  it('with too little room on both sides it takes the larger one and the list scrolls', () => {
    const small = { width: 390, height: 500 };
    const p = placePanel({ left: 16, top: 300, bottom: 338 }, small, want);
    expect(p).toMatchObject({ bottom: 500 - 300 + 6, maxHeight: 300 - 6 - 12 });
    const q = placePanel({ left: 16, top: 150, bottom: 188 }, small, want);
    expect(q).toMatchObject({ top: 194, maxHeight: 500 - 12 - 194 });
  });

  it('is never wider than the window, and a button at the right edge pulls it back in', () => {
    const wide = placePanel(face(100, 16), view, { width: 900, height: 200 });
    expect(wide).toMatchObject({ left: 12, width: 390 - 24 });
    const right = placePanel(face(100, 300), view, { width: 200, height: 200 });
    expect(right.left).toBe(390 - 200 - 12);
  });

  it('stays in the window wherever the button has gone', () => {
    for (const top of [-500, -30, 0, 200, 420, 800, 830, 1000, 3000]) {
      const b = box(placePanel(face(top), view, want), want.height);
      expect(b.top, `top at ${top}`).toBeGreaterThanOrEqual(12);
      expect(b.bottom, `bottom at ${top}`).toBeLessThanOrEqual(844 - 12);
      expect(b.left).toBeGreaterThanOrEqual(12);
      expect(b.right).toBeLessThanOrEqual(390 - 12);
    }
  });

  it('a window lower than a list and a button with no room either side: the panel goes over the button', () => {
    const low = { width: 844, height: 200 };
    const p = placePanel({ left: 16, top: 70, bottom: 108 }, low, want);
    expect(p).toEqual({ left: 16, width: 300, maxHeight: 176, top: 12 });
  });

  it('the height a list wants counts its rows and headings and stops at the height of the list', () => {
    expect(panelHeight([o('a'), o('b')])).toBe(2 * 44 + 18);
    expect(panelHeight([{ group: 'g', options: [o('a')] }])).toBe(44 + 30 + 18);
    expect(panelHeight(TYPE_GROUPS.map((g) => ({ group: g.group, options: g.types.map((v) => o(v)) })))).toBe(352 + 18);
  });
});

describe('the stylesheet', () => {
  const css = readFileSync(new URL('../../styles.css', import.meta.url), 'utf8');

  it('the list stops at the height panelHeight counts with', () => {
    expect(css).toMatch(/\.picker-list \{[^}]*max-height: 22rem/);
  });

  it('a row is as high as panelHeight says and a phone can press it', () => {
    expect(css).toMatch(/\.picker-option \{[^}]*min-height: 44px/);
  });

  it('the panel is fixed: a card or a scroll container does not cut it', () => {
    expect(css).toMatch(/\.picker-panel \{[^}]*position: fixed/);
  });
});
