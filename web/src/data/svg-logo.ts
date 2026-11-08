// Token logos in formats other than the four the page shows as they are (PNG, JPEG, WEBP, GIF; data/kascov.ts `sniffImage`): SVG (also gzip-compressed),
// AVIF, HEIC, JPEG XL, BMP, ICO and TIFF. Any of them is accepted only on the CHECKED paths (data/logos.ts, data/kascov.ts): its bytes were read and
// compared with what the creator committed to (`image_hash`, the IPFS CID or kascov.io's copy), always the hash of the ORIGINAL file. It is then drawn
// once in an <img> (where an SVG runs no script and loads nothing else) onto a canvas and shown as a WEBP (PNG where the browser cannot encode WEBP), so
// the page shows only a raster picture it made itself. Before any of it the file is searched for code (`pictureProblem`): an SVG with a script, an
// event handler, an external reference ..., or a raster file with HTML or script hidden in it, is never shown. A format this browser cannot decode (HEIC outside Safari, say) gives no picture: the letter.
import type { ImageMime } from './kascov';
import { sniffImage } from './kascov';

/** The side of the picture a file is drawn at (a logo is shown at most at 64 CSS px; twice that for a dense screen). */
export const RASTER_PX = 128;

/** The largest SVG a gzip-compressed one may expand to (a compressed file must not unfold into an unbounded document). */
export const MAX_SVG_BYTES = 1_500_000;

/** Formats drawn to a raster picture before they are shown. */
export type DrawnMime = 'image/svg+xml' | 'image/avif' | 'image/heic' | 'image/jxl' | 'image/bmp' | 'image/x-icon' | 'image/tiff';

/** Whether `bytes` are an SVG document: UTF-8 text whose first element is `<svg` (after a BOM, an XML declaration, comments and a DOCTYPE). */
export function isSvg(bytes: Uint8Array): boolean {
  if (bytes.length < 5) return false;
  const s = new TextDecoder('utf-8', { fatal: false }).decode(bytes.subarray(0, Math.min(bytes.length, 4096)));
  let i = s.charCodeAt(0) === 0xfeff ? 1 : 0;
  for (let guard = 0; guard < 64; guard++) {
    while (i < s.length && /\s/.test(s[i]!)) i++;
    if (s.startsWith('<?xml', i)) {
      const end = s.indexOf('?>', i);
      if (end < 0) return false;
      i = end + 2;
    } else if (s.startsWith('<!--', i)) {
      const end = s.indexOf('-->', i);
      if (end < 0) return false;
      i = end + 3;
    } else if (s.slice(i, i + 9).toUpperCase() === '<!DOCTYPE') {
      // a DOCTYPE with an internal subset (`[ <!ENTITY ...> ]`) ends at `]>`: the document is still an SVG, and `svgActiveContent` refuses the entities
      const gt = s.indexOf('>', i);
      const open = s.indexOf('[', i);
      const end = open >= 0 && (gt < 0 || open < gt) ? s.indexOf(']>', open) + 1 : gt;
      if (end <= 0) return false;
      i = end + 1;
    } else {
      return /^<svg[\s>/]/i.test(s.slice(i, i + 5));
    }
  }
  return false;
}

const ascii = (b: Uint8Array, at: number, n: number): string => String.fromCharCode(...b.subarray(at, at + n));

/** The ISO-BMFF brands of a file (`ftyp` box: the major brand and the compatible ones), lower case; empty for another file. */
function ftypBrands(b: Uint8Array): string[] {
  if (b.length < 16 || ascii(b, 4, 4) !== 'ftyp') return [];
  const size = Math.min(b.length, ((b[0]! << 24) | (b[1]! << 16) | (b[2]! << 8) | b[3]!) >>> 0);
  const out = [ascii(b, 8, 4)];
  for (let i = 16; i + 4 <= size; i += 4) out.push(ascii(b, i, 4));
  return out.map((x) => x.toLowerCase());
}

/** The format of a file the page draws before it shows it (magic bytes, never the address's extension), or null. `gzip`: maybe a compressed SVG. */
export function sniffDrawn(b: Uint8Array): DrawnMime | 'gzip' | null {
  if (isSvg(b)) return 'image/svg+xml';
  const brands = ftypBrands(b);
  if (brands.some((x) => x === 'avif' || x === 'avis')) return 'image/avif';
  if (brands.some((x) => ['heic', 'heix', 'hevc', 'hevx', 'heim', 'heis', 'mif1', 'msf1'].includes(x))) return 'image/heic';
  if (b.length >= 2 && b[0] === 0xff && b[1] === 0x0a) return 'image/jxl';
  if (b.length >= 12 && ascii(b, 4, 4) === 'JXL ' && b[0] === 0 && b[1] === 0 && b[2] === 0 && b[3] === 0x0c) return 'image/jxl';
  if (b.length >= 14 && b[0] === 0x42 && b[1] === 0x4d) return 'image/bmp';
  if (b.length >= 6 && b[0] === 0 && b[1] === 0 && (b[2] === 1 || b[2] === 2) && b[3] === 0 && (b[4]! | b[5]!) !== 0) return 'image/x-icon';
  if (b.length >= 8 && ((b[0] === 0x49 && b[1] === 0x49 && b[2] === 0x2a && b[3] === 0) || (b[0] === 0x4d && b[1] === 0x4d && b[2] === 0 && b[3] === 0x2a))) return 'image/tiff';
  if (b.length >= 3 && b[0] === 0x1f && b[1] === 0x8b && b[2] === 8) return 'gzip';
  return null;
}

/** Whether the page can show `bytes` at all (as they are, or drawn), from the magic bytes alone. */
export const isPicture = (b: Uint8Array): boolean => sniffImage(b) !== null || sniffDrawn(b) !== null;

export interface Raster {
  bytes: Uint8Array;
  mime: ImageMime;
}

/** Draws checked bytes of `mime` at `px` x `px` (keeping their aspect, centred), encoded as WEBP, else PNG. Null when the browser cannot decode or encode them. */
export async function rasterize(bytes: Uint8Array, mime: DrawnMime, px = RASTER_PX): Promise<Raster | null> {
  if (typeof document === 'undefined' || typeof Image === 'undefined' || typeof URL.createObjectURL !== 'function') return null;
  const url = URL.createObjectURL(new Blob([bytes as Uint8Array<ArrayBuffer>], { type: mime }));
  try {
    const img = new Image(px, px);
    img.decoding = 'async';
    img.src = url;
    await img.decode();
    const w = img.naturalWidth || px;
    const h = img.naturalHeight || px;
    const scale = Math.min(px / w, px / h);
    const canvas = document.createElement('canvas');
    canvas.width = px;
    canvas.height = px;
    const ctx = canvas.getContext('2d');
    if (!ctx) return null;
    const dw = w * scale;
    const dh = h * scale;
    ctx.drawImage(img, (px - dw) / 2, (px - dh) / 2, dw, dh);
    for (const out of ['image/webp', 'image/png'] as const) {
      const blob = await new Promise<Blob | null>((ok) => canvas.toBlob(ok, out));
      if (blob && blob.type === out) return { bytes: new Uint8Array(await blob.arrayBuffer()), mime: out };
    }
    return null;
  } catch {
    // a file this browser cannot decode, or a canvas it will not read back
    return null;
  } finally {
    URL.revokeObjectURL(url);
  }
}

/** Expands gzip bytes, at most `max` of output; null for anything else, a broken stream or more than `max`. */
export async function gunzipCapped(bytes: Uint8Array, max = MAX_SVG_BYTES): Promise<Uint8Array | null> {
  if (typeof DecompressionStream === 'undefined') return null;
  try {
    const reader = new Blob([bytes as Uint8Array<ArrayBuffer>]).stream().pipeThrough(new DecompressionStream('gzip')).getReader();
    const parts: Uint8Array[] = [];
    let n = 0;
    for (;;) {
      const { done, value } = await reader.read();
      if (done) break;
      n += value.length;
      if (n > max) {
        await reader.cancel();
        return null;
      }
      parts.push(value);
    }
    const out = new Uint8Array(n);
    let at = 0;
    for (const p of parts) {
      out.set(p, at);
      at += p.length;
    }
    return out;
  } catch {
    return null;
  }
}

// ------------------------------------------------------------------------------------------------ no code in a picture

/**
 * What an SVG document carries that is not a plain drawing, or null: a script, an event handler, a `javascript:` (or other script) address, an HTML
 * island (`foreignObject`, `iframe`, `object`, `embed`), an entity declaration (an XML bomb), an animation that changes an address, or a reference to
 * anything outside the file (`http(s):`, `//host`, another file, `@import`). Shown in an <img> none of this would run; a file with any of it is refused
 * all the same, so the app never shows a picture that was made to do more than be a picture.
 */
export function svgActiveContent(text: string): string | null {
  const s = text.replace(/\s+/g, ' ');
  const rules: [RegExp, string][] = [
    [/<\s*script\b/i, 'script'],
    [/[\s"'/]on[a-z]+\s*=/i, 'event handler'],
    [/(java|vb|live)script\s*:/i, 'script address'],
    [/data\s*:\s*(text\/html|application\/|image\/svg)/i, 'embedded document'],
    [/<\s*(foreignobject|iframe|object|embed|audio|video|handler|listener)\b/i, 'embedded content'],
    [/<!\s*entity\b/i, 'entity declaration'],
    [/<\s*(set|animate)\b[^>]*attributename\s*=\s*["']?\s*(xlink:)?href/i, 'animated address'],
    [/url\s*\(\s*["']?\s*(https?:|\/\/|file:)/i, 'external reference'],
    [/@import\b/i, 'style import'],
  ];
  for (const [re, why] of rules) if (re.test(s)) return why;
  // an address in the file may only point inside it (`#id`) or carry a picture (`data:image/png|jpeg|webp|gif;base64,`)
  for (const m of s.matchAll(/\b(href|src)\s*=\s*(["'])(.*?)\2/gi)) {
    const v = m[3]!.trim();
    if (v.startsWith('#') || /^data:image\/(png|jpe?g|webp|gif);base64,/i.test(v)) continue;
    return 'reference to another file';
  }
  for (const m of s.matchAll(/url\s*\(\s*["']?\s*([^"')\s]+)/gi)) {
    const v = m[1]!;
    if (v.startsWith('#') || /^data:image\/(png|jpe?g|webp|gif);base64,/i.test(v)) continue;
    return 'reference to another file';
  }
  return null;
}

const MARKERS = ['<script', 'javascript:', '<html', '<body', '<iframe', '<object', '<embed', '<?php', 'onerror=', 'onload='];

/**
 * Web code hidden in a raster file (a "polyglot": a picture that is also an HTML page or a script), or null. The bytes are searched as ASCII without
 * case for the markers of HTML, script and server code; a real picture's compressed data practically never spells one out.
 */
export function rasterHiddenCode(bytes: Uint8Array): string | null {
  let s = '';
  for (let i = 0; i < bytes.length; i += 8192) s += String.fromCharCode(...bytes.subarray(i, i + 8192));
  s = s.toLowerCase();
  for (const m of MARKERS) if (s.includes(m)) return m;
  return null;
}

/** Why a picture file must not be shown or declared, or null: `not a picture`, or the code found in it. A compressed SVG is expanded and checked as an SVG. */
export async function pictureProblem(bytes: Uint8Array): Promise<string | null> {
  const kind = sniffImage(bytes) ? 'raster' : sniffDrawn(bytes);
  if (kind === null) return 'not a picture';
  const text = (b: Uint8Array) => new TextDecoder('utf-8', { fatal: false }).decode(b);
  if (kind === 'image/svg+xml') return svgActiveContent(text(bytes));
  if (kind === 'gzip') {
    const svg = await gunzipCapped(bytes);
    if (!svg || !isSvg(svg)) return 'not a picture';
    return svgActiveContent(text(svg));
  }
  return rasterHiddenCode(bytes);
}

/** Draws a checked file to a raster picture; injectable for tests. */
export type Rasterize = (bytes: Uint8Array, mime: DrawnMime) => Promise<Raster | null>;

/**
 * What the page shows for CHECKED bytes (their hash was compared before): the bytes themselves for PNG, JPEG, WEBP and GIF (an animation stays one),
 * else the picture drawn from them (SVG, a gzip-compressed SVG, AVIF, HEIC, JPEG XL, BMP, ICO, TIFF). Null for anything else, a file with code in it
 * (`pictureProblem`) or a file the browser cannot draw.
 */
export async function shownPicture(bytes: Uint8Array, draw: Rasterize = rasterize): Promise<Raster | null> {
  if ((await pictureProblem(bytes)) !== null) return null;
  const mime = sniffImage(bytes);
  if (mime) return { bytes, mime };
  const kind = sniffDrawn(bytes);
  if (kind === null) return null;
  if (kind === 'gzip') {
    const svg = await gunzipCapped(bytes);
    return svg && isSvg(svg) ? draw(svg, 'image/svg+xml') : null;
  }
  return draw(bytes, kind);
}
