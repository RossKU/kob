// WCAG 2 contrast of a text on its background, from colours as `getComputedStyle` gives them (e2e/ui/contrast.spec.ts is the gate; pure, no DOM).

/** The least contrast of a normal-size text (WCAG AA). */
export const MIN_TEXT_CONTRAST = 4.5;

/** Channels 0..255, alpha 0..1. */
export interface Rgba {
  r: number;
  g: number;
  b: number;
  a: number;
}

const OPAQUE_WHITE: Rgba = { r: 255, g: 255, b: 255, a: 1 };

const clamp = (v: number, hi: number): number => Math.min(hi, Math.max(0, v));

/** A channel or alpha as written: a number on `unit` (255 for a channel, 1 for alpha) or a percentage of it. */
const part = (s: string, unit: number): number => {
  const v = Number.parseFloat(s);
  return s.endsWith('%') ? (v / 100) * unit : v;
};

/** `#rgb`, `#rgba`, `#rrggbb`, `#rrggbbaa`, `rgb()`, `rgba()`, `color(srgb ...)` (what a browser computes a `color-mix()` to) and `transparent`; null for anything else. */
export function parseColor(text: string): Rgba | null {
  const s = text.trim().toLowerCase();
  if (s === 'transparent') return { r: 0, g: 0, b: 0, a: 0 };
  const hex = /^#([0-9a-f]{3,4}|[0-9a-f]{6}|[0-9a-f]{8})$/.exec(s)?.[1];
  if (hex) {
    const full = hex.length <= 4 ? [...hex].map((c) => c + c).join('') : hex;
    const at = (i: number) => Number.parseInt(full.slice(i, i + 2), 16);
    return { r: at(0), g: at(2), b: at(4), a: full.length === 8 ? at(6) / 255 : 1 };
  }
  const fn = /^(rgba?|color)\((.*)\)$/.exec(s);
  if (!fn) return null;
  const parts = fn[2]!.replace(/\s*\/\s*/, ' ').replace(/,/g, ' ').trim().split(/\s+/);
  const srgb = fn[1] === 'color';
  if (srgb && parts.shift() !== 'srgb') return null;
  if (parts.length < 3 || parts.length > 4) return null;
  const [r, g, b] = parts.slice(0, 3).map((p) => (srgb ? part(p, 1) * 255 : part(p, 255))) as [number, number, number];
  const a = parts[3] === undefined ? 1 : part(parts[3], 1);
  if ([r, g, b, a].some(Number.isNaN)) return null;
  return { r: clamp(r, 255), g: clamp(g, 255), b: clamp(b, 255), a: clamp(a, 1) };
}

/** `top` painted over `under` (source over). */
export function over(top: Rgba, under: Rgba): Rgba {
  const a = top.a + under.a * (1 - top.a);
  if (a === 0) return { r: 0, g: 0, b: 0, a: 0 };
  const mix = (t: number, u: number) => (t * top.a + u * under.a * (1 - top.a)) / a;
  return { r: mix(top.r, under.r), g: mix(top.g, under.g), b: mix(top.b, under.b), a };
}

/** The relative luminance of an opaque colour (WCAG 2). */
export function luminance(c: Rgba): number {
  const lin = (v: number) => {
    const s = v / 255;
    return s <= 0.04045 ? s / 12.92 : ((s + 0.055) / 1.055) ** 2.4;
  };
  return 0.2126 * lin(c.r) + 0.7152 * lin(c.g) + 0.0722 * lin(c.b);
}

/** The contrast of two opaque colours, 1 to 21. */
export function contrast(a: Rgba, b: Rgba): number {
  const [hi, lo] = [luminance(a), luminance(b)].sort((x, y) => y - x) as [number, number];
  return (hi + 0.05) / (lo + 0.05);
}

/**
 * The contrast of a text colour on a stack of background colours, nearest first (an element's own background, then its parents', up to the page; a browser's
 * canvas is white under them). A colour that cannot be read gives NaN, which fails any `>=` check.
 */
export function textContrast(color: string, backgrounds: readonly string[]): number {
  const fg = parseColor(color);
  const layers = backgrounds.map(parseColor);
  if (!fg || layers.some((l) => l === null)) return Number.NaN;
  const bg = (layers as Rgba[]).reduceRight((under, top) => over(top, under), OPAQUE_WHITE);
  return contrast(over(fg, bg), bg);
}
