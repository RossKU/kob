// The fingerprint of a covenant id: a small picture and three words that let a person tell two
// tokens apart, including two that share a ticker, without reading 64 hex characters. It is a
// comparison aid, not a commitment. The full id is what identifies a token.
//
// Everything that needs a fingerprint goes through `fingerprint()`. The algorithm behind it is
// Hallmarks v1.0 (src/third-party/hallmarks). It is the only thing to change if another
// Kaspa-ecosystem fingerprint is named, because the rest of the app reads only `cells`, the three
// colours and `words`.
import { hallmark, type Cell } from '../third-party/hallmarks/hallmark';

/** A covenant id's picture, colours and words. */
export interface Fingerprint {
  /** Rows of cells, left-right symmetric: 0 background, 1 dot, 2 accent dot. */
  cells: readonly (readonly Cell[])[];
  background: string;
  primary: string;
  accent: string;
  /** Three words that say the same, for a screen reader and for reading aloud. */
  words: readonly [string, string, string];
  wordsText: string;
}

/** Name of the algorithm in use. A tooltip may show it and a test pins it. */
export const FINGERPRINT_ALGORITHM = 'Hallmarks v1.0';

const cache = new Map<string, Fingerprint>();
const CACHE_MAX = 1024;

/** The fingerprint of a covenant id, hashed as lowercase hex (the form every id has in the app). */
export function fingerprint(covenantId: string): Fingerprint {
  const id = covenantId.trim().toLowerCase();
  const hit = cache.get(id);
  if (hit) return hit;
  const h = hallmark(id);
  const fp: Fingerprint = { cells: h.cells, background: h.background.hex, primary: h.primary.hex, accent: h.accent.hex, words: h.words, wordsText: h.wordsText };
  if (cache.size >= CACHE_MAX) cache.clear();
  cache.set(id, fp);
  return fp;
}
