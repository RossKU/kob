import { describe, expect, it } from 'vitest';
import { initialTheme } from './theme';

describe('initialTheme', () => {
  it('a stored choice wins, then the OS preference, and dark when nothing is known', () => {
    expect(initialTheme('light', false)).toBe('light');
    expect(initialTheme('dark', true)).toBe('dark');
    expect(initialTheme(null, true)).toBe('light');
    expect(initialTheme(null, false)).toBe('dark');
    expect(initialTheme(null, null)).toBe('dark');
    expect(initialTheme('purple', null)).toBe('dark');
  });
});
