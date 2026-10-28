// Hallmarks v1.0 (github.com/GBKS/hallmarks, spec CC0 1.0, reference code MIT, see LICENSE and UPSTREAM.md): a deterministic 5x7 symmetric dot
// pattern, two colours and three BIP-39 words from the SHA-256 of an identifier. Written from SPEC.md (sections 3.1 to 3.7, style "standard"),
// not copied from the reference code; it is held to the reference's own conformance vectors by hallmark.test.ts. Synchronous and dependency free
// (a list row renders it, so no async digest), the wordlist is the one the reference ships.
import { BIP39_ENGLISH } from './bip39-english';

export const COLUMNS = 5;
export const ROWS = 7;

/** One cell: 0 background, 1 primary dot, 2 accent dot (larger). */
export type Cell = 0 | 1 | 2;

export interface Oklch {
  L: number;
  C: number;
  h: number;
  /** sRGB, `#rrggbb` */
  hex: string;
}

export interface Hallmark {
  /** 7 rows of 5 cells, left-right symmetric */
  cells: Cell[][];
  background: Oklch;
  primary: Oklch;
  accent: Oklch;
  words: [string, string, string];
  /** the three words, one space between */
  wordsText: string;
}

// ------------------------------------------------------------------------------------------------ SHA-256 (FIPS 180-4)

const K = Uint32Array.from([
  0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4, 0xab1c5ed5, 0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3,
  0x72be5d74, 0x80deb1fe, 0x9bdc06a7, 0xc19bf174, 0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f, 0x4a7484aa, 0x5cb0a9dc, 0x76f988da,
  0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7, 0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967, 0x27b70a85, 0x2e1b2138, 0x4d2c6dfc, 0x53380d13,
  0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85, 0xa2bfe8a1, 0xa81a664b, 0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070,
  0x19a4c116, 0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3, 0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208,
  0x90befffa, 0xa4506ceb, 0xbef9a3f7, 0xc67178f2,
]);

const rotr = (x: number, n: number): number => (x >>> n) | (x << (32 - n));

export function sha256(data: Uint8Array): Uint8Array {
  const h = Uint32Array.from([0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab, 0x5be0cd19]);
  const padded = new Uint8Array(((data.length + 9 + 63) >>> 6) << 6);
  padded.set(data);
  padded[data.length] = 0x80;
  const view = new DataView(padded.buffer);
  view.setUint32(padded.length - 8, Math.floor((data.length * 8) / 0x100000000));
  view.setUint32(padded.length - 4, (data.length * 8) >>> 0);
  const w = new Uint32Array(64);
  for (let off = 0; off < padded.length; off += 64) {
    for (let i = 0; i < 16; i++) w[i] = view.getUint32(off + i * 4);
    for (let i = 16; i < 64; i++) {
      const s0 = rotr(w[i - 15]!, 7) ^ rotr(w[i - 15]!, 18) ^ (w[i - 15]! >>> 3);
      const s1 = rotr(w[i - 2]!, 17) ^ rotr(w[i - 2]!, 19) ^ (w[i - 2]! >>> 10);
      w[i] = (w[i - 16]! + s0 + w[i - 7]! + s1) | 0;
    }
    let [a, b, c, d, e, f, g, hh] = h as unknown as number[] as [number, number, number, number, number, number, number, number];
    for (let i = 0; i < 64; i++) {
      const t1 = (hh + (rotr(e, 6) ^ rotr(e, 11) ^ rotr(e, 25)) + ((e & f) ^ (~e & g)) + K[i]! + w[i]!) | 0;
      const t2 = ((rotr(a, 2) ^ rotr(a, 13) ^ rotr(a, 22)) + ((a & b) ^ (a & c) ^ (b & c))) | 0;
      hh = g;
      g = f;
      f = e;
      e = (d + t1) | 0;
      d = c;
      c = b;
      b = a;
      a = (t1 + t2) | 0;
    }
    h[0] = (h[0]! + a) | 0;
    h[1] = (h[1]! + b) | 0;
    h[2] = (h[2]! + c) | 0;
    h[3] = (h[3]! + d) | 0;
    h[4] = (h[4]! + e) | 0;
    h[5] = (h[5]! + f) | 0;
    h[6] = (h[6]! + g) | 0;
    h[7] = (h[7]! + hh) | 0;
  }
  const out = new Uint8Array(32);
  const ov = new DataView(out.buffer);
  for (let i = 0; i < 8; i++) ov.setUint32(i * 4, h[i]!);
  return out;
}

// ------------------------------------------------------------------------------------------------ pattern (spec 3.3, 3.6)

/** Mulberry32 seeded from 4 bytes (big endian); `next()` divides by 2^32 - 1 as the spec requires. */
function mulberry32(bytes: Uint8Array, offset: number): () => number {
  let state = ((bytes[offset]! << 24) | (bytes[offset + 1]! << 16) | (bytes[offset + 2]! << 8) | bytes[offset + 3]!) >>> 0;
  return () => {
    state = (state + 0x6d2b79f5) >>> 0;
    let t = state;
    t = Math.imul(t ^ (t >>> 15), t | 1);
    t ^= t + Math.imul(t ^ (t >>> 7), t | 61);
    return ((t ^ (t >>> 14)) >>> 0) / 4294967295;
  };
}

const HALF = 3;
const MAX_ATTEMPTS = 8;

function attempt(bytes: Uint8Array, offset: number): { cells: Cell[][]; filled: number } {
  const next = mulberry32(bytes, offset);
  const cells: Cell[][] = [];
  let filled = 0;
  for (let r = 0; r < ROWS; r++) {
    const half: Cell[] = [];
    for (let c = 0; c < HALF; c++) {
      const v = next();
      const cell: Cell = v < 0.5 ? 0 : v < 0.85 ? 1 : 2;
      if (cell !== 0) filled++;
      half.push(cell);
    }
    cells.push([half[0]!, half[1]!, half[2]!, half[1]!, half[0]!]);
  }
  return { cells, filled };
}

function pattern(bytes: Uint8Array): Cell[][] {
  for (let n = 0; n < MAX_ATTEMPTS; n++) {
    const a = attempt(bytes, (4 + n * 4) % 28);
    const ratio = a.filled / (ROWS * HALF);
    if (ratio >= 0.45 && ratio <= 0.75) return a.cells;
  }
  return attempt(bytes, 4).cells;
}

// ------------------------------------------------------------------------------------------------ colours (spec 3.2, 3.7, style standard)

const srgbEncode = (v: number): number => {
  const c = Math.max(0, Math.min(1, v));
  return c <= 0.0031308 ? 12.92 * c : 1.055 * Math.pow(c, 1 / 2.4) - 0.055;
};

function oklch(L: number, C: number, h: number): Oklch {
  const a = C * Math.cos((h * Math.PI) / 180);
  const b = C * Math.sin((h * Math.PI) / 180);
  const l = (L + 0.3963377774 * a + 0.2158037573 * b) ** 3;
  const m = (L - 0.1055613458 * a - 0.0638541728 * b) ** 3;
  const s = (L - 0.0894841775 * a - 1.291485548 * b) ** 3;
  const rgb = [
    4.0767416621 * l - 3.3077115913 * m + 0.2309699292 * s,
    -1.2684380046 * l + 2.6097574011 * m - 0.3413193965 * s,
    -0.0041960863 * l - 0.7034186147 * m + 1.707614701 * s,
  ].map((v) => Math.max(0, Math.min(255, Math.round(srgbEncode(v) * 255))).toString(16).padStart(2, '0'));
  return { L, C, h, hex: `#${rgb.join('')}` };
}

// ------------------------------------------------------------------------------------------------ words (spec 3.4)

function words(bytes: Uint8Array): [string, string, string] {
  const hi = bytes[27]! & 0x7f;
  const lo = ((bytes[28]! << 24) | (bytes[29]! << 16) | (bytes[30]! << 8) | bytes[31]!) >>> 0;
  const i1 = ((hi << 5) | (lo >>> 27)) & 0x7ff;
  const i2 = (lo >>> 11) & 0x7ff;
  const i3 = lo & 0x7ff;
  return [BIP39_ENGLISH[i1]!, BIP39_ENGLISH[i2]!, BIP39_ENGLISH[i3]!];
}

/** The hallmark (standard style) of an identifier: the UTF-8 bytes of `input` are hashed as they are (no trimming, no case folding). */
export function hallmark(input: string): Hallmark {
  const bytes = sha256(new TextEncoder().encode(input));
  const h1 = (((bytes[0]! << 8) | bytes[1]!) / 65536) * 360;
  const h2 = (h1 + 100 + (((bytes[2]! << 8) | bytes[3]!) / 65536) * 160) % 360;
  const w = words(bytes);
  return {
    cells: pattern(bytes),
    background: oklch(0.96, 0.025, h1),
    primary: oklch(0.52, 0.16, h1),
    accent: oklch(0.66, 0.18, h2),
    words: w,
    wordsText: w.join(' '),
  };
}

export const hex = (bytes: Uint8Array): string => Array.from(bytes, (b) => b.toString(16).padStart(2, '0')).join('');
