// The Create page's picture file: the browser reads the file the creator picked, checks it the way the app checks pictures elsewhere (a raster
// image of at most MAX_LOGO_BYTES, never an SVG: data/logos.ts, data/kascov.ts) and returns the SHA-256 of its exact bytes, which the token
// declares as `image_hash` (docs/spec/token-metadata.md 2). The file itself is not uploaded or stored anywhere: only its hash is kept.
import { sha256Hex, sniffImage } from '../../data/kascov';
import { MAX_LOGO_BYTES } from '../../data/logos';

export type ImageFileFailure = 'empty' | 'size' | 'svg' | 'type' | 'crypto' | 'read';

export type ImageFileResult = { ok: true; hash: string; bytes: number } | { ok: false; reason: ImageFileFailure };

/** The part of a `File` this needs (so tests can pass a `Blob`). */
export interface PickedFile extends Blob {
  readonly name?: string;
}

/** Checks a picked picture file and hashes its bytes: 64 lowercase hex characters, or the reason it is refused. */
export async function hashImageFile(file: PickedFile): Promise<ImageFileResult> {
  if (file.size === 0) return { ok: false, reason: 'empty' };
  if (/^image\/svg/i.test(file.type) || /\.svgz?$/i.test(file.name ?? '')) return { ok: false, reason: 'svg' };
  if (file.size > MAX_LOGO_BYTES) return { ok: false, reason: 'size' };
  let bytes: Uint8Array;
  try {
    bytes = new Uint8Array(await file.arrayBuffer());
  } catch {
    return { ok: false, reason: 'read' };
  }
  if (!sniffImage(bytes)) return { ok: false, reason: 'type' };
  const hash = await sha256Hex(bytes);
  return hash === null ? { ok: false, reason: 'crypto' } : { ok: true, hash, bytes: bytes.length };
}
