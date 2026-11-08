// SVG token logos (data/logos.ts, data/kascov.ts). Some real tokens declare an SVG picture. An SVG is accepted only on the CHECKED paths: its bytes were
// read and compared with what the creator committed to (`image_hash`, or the IPFS CID), always the hash of the ORIGINAL SVG file. It is then drawn once
// in an <img> (where an SVG runs no script and loads nothing else) onto a canvas and shown as a WEBP (PNG where the browser cannot encode WEBP), so the
// page never shows the SVG document itself. An SVG behind an address that cannot be read (no CORS header) is never shown unchecked.
import type { ImageMime } from './kascov';

/** The side of the picture an SVG is drawn at (a logo is shown at most at 64 CSS px; twice that for a dense screen). */
export const SVG_RASTER_PX = 128;

/** Whether `bytes` are an SVG document: UTF-8 text whose first element is `<svg` (after a BOM, an XML declaration, comments and a DOCTYPE). */
export function isSvg(bytes: Uint8Array): boolean {
  if (bytes.length < 5) return false;
  let s: string;
  try {
    s = new TextDecoder('utf-8', { fatal: true }).decode(bytes.subarray(0, Math.min(bytes.length, 4096)));
  } catch {
    // a multi-byte character cut at the 4 KiB boundary: decode a little less
    try {
      s = new TextDecoder('utf-8', { fatal: false }).decode(bytes.subarray(0, Math.min(bytes.length, 4096)));
    } catch {
      return false;
    }
  }
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
      const end = s.indexOf('>', i);
      if (end < 0) return false;
      i = end + 1;
    } else {
      return /^<svg[\s>/]/i.test(s.slice(i, i + 5));
    }
  }
  return false;
}

export interface Raster {
  bytes: Uint8Array;
  mime: ImageMime;
}

/** Draws an SVG's bytes at `px` x `px` (keeping its aspect, centred) and encodes it as WEBP, else PNG. Null when the browser cannot draw or encode it. */
export async function rasterizeSvg(bytes: Uint8Array, px = SVG_RASTER_PX): Promise<Raster | null> {
  if (typeof document === 'undefined' || typeof Image === 'undefined') return null;
  const url = URL.createObjectURL(new Blob([bytes as Uint8Array<ArrayBuffer>], { type: 'image/svg+xml' }));
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
    for (const mime of ['image/webp', 'image/png'] as const) {
      const blob = await new Promise<Blob | null>((ok) => canvas.toBlob(ok, mime));
      if (blob && blob.type === mime) return { bytes: new Uint8Array(await blob.arrayBuffer()), mime };
    }
    return null;
  } catch {
    // an SVG the browser cannot draw, or a canvas it will not read back
    return null;
  } finally {
    URL.revokeObjectURL(url);
  }
}
