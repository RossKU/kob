// The fingerprint of a covenant id: the small picture and the three words that let a person tell two tokens apart (and see that two tokens that share
// a ticker are two) without reading 64 hex characters. A comparison aid, not a commitment: the full id is what identifies a token.
//
// Everything that needs a fingerprint goes through `fingerprint()`; the algorithm behind it (Hallmarks v1.0, src/third-party/hallmarks) is the only thing to
// change if another Kaspa-ecosystem fingerprint is named: the rest of the app reads `cells`, the three colours and `words`.
import { hallmark, type Cell } from '../third-party/hallmarks/hallmark';

export interface Fingerprint {
  /** rows of cells, left-right symmetric: 0 background, 1 dot, 2 accent dot */
  cells: readonly (readonly Cell[])[];
  background: string;
  primary: string;
  accent: string;
  /** three words that say the same, for a screen reader and for reading aloud */
  words: readonly [string, string, string];
  wordsText: string;
}

/** Name of the algorithm in use (a tooltip may say it; a test pins it). */
export const FINGERPRINT_ALGORITHM = 'Hallmarks v1.0';

const cache = new Map<string, Fingerprint>();
const CACHE_MAX = 1024;

/** The fingerprint of a covenant id. The id is hashed in its canonical form, lowercase hex (the form every id of the app has). */
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
