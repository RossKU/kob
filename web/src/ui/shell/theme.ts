// Colour theme: dark or light, chosen with the header toggle and remembered in localStorage. The
// default follows the OS preference, and is dark when the browser does not say. The theme is the
// `data-theme` attribute of <html>, and styles.css defines the palette of both.
import { useEffect, useState } from 'preact/hooks';

export type Theme = 'dark' | 'light';
export const THEME_KEY = 'kob.theme';

/**
 * A stored choice wins. Otherwise the OS preference applies (`prefersLight` true gives light), and
 * an unknown one gives dark, the screenshot-ready default.
 */
export function initialTheme(stored: string | null | undefined, prefersLight: boolean | null): Theme {
  if (stored === 'dark' || stored === 'light') return stored;
  return prefersLight === true ? 'light' : 'dark';
}

const listeners = new Set<(t: Theme) => void>();
let current: Theme | null = null;

function storage(): Storage | null {
  try {
    return typeof localStorage !== 'undefined' ? localStorage : null;
  } catch {
    return null;
  }
}

function osPrefersLight(): boolean | null {
  try {
    if (typeof matchMedia !== 'function') return null;
    if (matchMedia('(prefers-color-scheme: light)').matches) return true;
    if (matchMedia('(prefers-color-scheme: dark)').matches) return false;
  } catch {
    /* No media queries. */
  }
  return null;
}

function apply(t: Theme): void {
  if (typeof document === 'undefined') return;
  const el = document.documentElement;
  el.dataset.theme = t;
  el.style.colorScheme = t;
  document.querySelector('meta[name="theme-color"]')?.setAttribute('content', t === 'dark' ? '#070707' : '#fafafa');
}

/**
 * Applies the stored or preferred theme. Call it once before the first render, so the page never
 * flashes the other palette.
 */
export function initTheme(): Theme {
  let stored: string | null = null;
  try {
    stored = storage()?.getItem(THEME_KEY) ?? null;
  } catch {
    /* Blocked storage. */
  }
  current = initialTheme(stored, osPrefersLight());
  apply(current);
  return current;
}

export function getTheme(): Theme {
  return current ?? initTheme();
}

/** Switches and remembers the theme. */
export function setTheme(t: Theme): void {
  current = t;
  apply(t);
  try {
    storage()?.setItem(THEME_KEY, t);
  } catch {
    /* Blocked storage: the choice lasts for this page. */
  }
  for (const l of [...listeners]) l(t);
}

export function onThemeChange(fn: (t: Theme) => void): () => void {
  listeners.add(fn);
  return () => listeners.delete(fn);
}

/** The current theme, re-rendering on change. */
export function useTheme(): [Theme, (t: Theme) => void] {
  const [t, set] = useState<Theme>(getTheme());
  useEffect(() => onThemeChange(set), []);
  return [t, setTheme];
}
