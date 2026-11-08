// Token logos from the address the token's creator gave (founder decision 2026-11-03), beside kascov.io's checked copy (data/kascov.ts).
//
// KOB stores and serves no third-party image (a cache would make the operator a publisher). A logo is loaded by the VIEWER'S BROWSER, straight from its
// source, and a report (data/logo-blocklist.ts) takes it down. The sources, in the order the app tries them:
//
//   1. kascov.io's saved copy, shown only when its bytes hash to what the creator committed to (data/kascov.ts): unchanged.
//   2. `ipfs://<cid>[/path]` (the creator's KCC-23 declaration, or kascov's claim): the BYTES are fetched from a public gateway (`AppConfig.ipfsGatewayUrl`,
//      default https://dweb.link/ipfs) and shown from a `blob:` URL. The bytes are checked against the CID where that is possible without a library
//      (`verifyIpfsBytes`: a CIDv1 raw leaf, or a one-block CIDv0 / dag-pb file with no path, i.e. an image of at most 256 KiB); a larger file or one
//      inside a directory is a many-block DAG, whose check needs the whole DAG, so there the gateway's content addressing is accepted (the gateway can
//      then serve other bytes of a valid image type; it cannot make the creator's address point elsewhere).
//   3. `https://` (the declaration, or kascov's claimed image when it has no saved copy): a plain <img src> with no referrer, no credentials, lazy,
//      in a fixed box, labelled "Image from the creator's site, not checked". The creator's host sends no CORS header, so the page cannot read the bytes:
//      nothing is checked, and that host sees the viewer's IP address and when the logo was looked at. The Settings switch turns this source off.
//      When the declaration carries an `image_hash` (SHA-256 of the picture file, KCC-0021's field), the page first tries to READ the bytes
//      (`fetch` in CORS mode, no credentials, no referrer): readable and equal to the hash, they are shown from a `blob:` URL as checked; readable and
//      different, the letter is shown with a note that the picture does not match the creation record (`createHttpsChecker`); not readable (no CORS
//      header, the usual case), it falls back to the unchecked <img> above. An `ipfs://` image with an `image_hash` is compared with it as well.
//   4. The letter avatar.
//
// SVG, AVIF, HEIC, JPEG XL, BMP, ICO and TIFF pictures are shown only from checked bytes (the hash is that of the original file), drawn to a WEBP first
// (data/svg-logo.ts). For every source: no `data:` URL, no host that is an IP literal, `localhost` or a name without a dot (an address inside the viewer's network).
import { readCapped, sha256Hex, type ImageMime } from './kascov';
import { refusedHost, type LogoRef } from './logo-url';
import { isPicture, shownPicture, type Rasterize } from './svg-logo';

export { parseLogoRef, refusedHost, safeHttpsLogo, type LogoRef } from './logo-url';

export { DEFAULT_IPFS_GATEWAY_URL } from '../config';

/** The largest picture file the app loads as bytes (and the Create page hashes): 1.5 MB. */
export const MAX_LOGO_BYTES = 1_500_000;

/** A declared `image_hash`: 64 lowercase hex characters, else null. */
export const asImageHash = (v: unknown): string | null => (typeof v === 'string' && /^[0-9a-f]{64}$/.test(v) ? v : null);

// ------------------------------------------------------------------------------------------------ IPFS: the CID against the bytes

const B58 = '123456789ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnopqrstuvwxyz';

function base58Decode(s: string): Uint8Array | null {
  let n = 0n;
  for (const c of s) {
    const i = B58.indexOf(c);
    if (i < 0) return null;
    n = n * 58n + BigInt(i);
  }
  const bytes: number[] = [];
  for (; n > 0n; n >>= 8n) bytes.unshift(Number(n & 0xffn));
  let zeros = 0;
  while (zeros < s.length && s[zeros] === '1') zeros++;
  return new Uint8Array([...new Array<number>(zeros).fill(0), ...bytes]);
}

function base32Decode(s: string): Uint8Array | null {
  const A = 'abcdefghijklmnopqrstuvwxyz234567';
  let bits = 0;
  let acc = 0;
  const out: number[] = [];
  for (const c of s) {
    const i = A.indexOf(c);
    if (i < 0) return null;
    acc = (acc << 5) | i;
    bits += 5;
    if (bits >= 8) {
      bits -= 8;
      out.push((acc >> bits) & 0xff);
      acc &= (1 << bits) - 1;
    }
  }
  return new Uint8Array(out);
}

function varint(n: number): number[] {
  const out: number[] = [];
  let v = n;
  while (v >= 0x80) {
    out.push((v & 0x7f) | 0x80);
    v = Math.floor(v / 128);
  }
  out.push(v);
  return out;
}

/** The unsigned varint at `at`: [value, next index], or null. */
function readVarint(b: Uint8Array, at: number): [number, number] | null {
  let v = 0;
  let shift = 1;
  for (let i = at; i < b.length && i < at + 8; i++) {
    v += (b[i]! & 0x7f) * shift;
    if ((b[i]! & 0x80) === 0) return [v, i + 1];
    shift *= 128;
  }
  return null;
}

/** What a CID commits to: the SHA-256 digest and how the bytes were wrapped before hashing. Null for any other hash function or codec. */
export function parseCid(cid: string): { digest: Uint8Array; codec: 'raw' | 'dag-pb' } | null {
  if (cid.startsWith('Qm')) {
    const mh = base58Decode(cid);
    return mh && mh.length === 34 && mh[0] === 0x12 && mh[1] === 0x20 ? { digest: mh.slice(2), codec: 'dag-pb' } : null;
  }
  if (!cid.startsWith('b')) return null;
  const b = base32Decode(cid.slice(1));
  if (!b || b[0] !== 0x01) return null;
  const codec = readVarint(b, 1);
  if (!codec) return null;
  const fn = readVarint(b, codec[1]);
  if (!fn || fn[0] !== 0x12) return null;
  const len = readVarint(b, fn[1]);
  if (!len || len[0] !== 32 || b.length !== len[1] + 32) return null;
  const digest = b.slice(len[1]);
  return codec[0] === 0x55 ? { digest, codec: 'raw' } : codec[0] === 0x70 ? { digest, codec: 'dag-pb' } : null;
}

/** The largest file the default importer puts in ONE block (chunk size 256 KiB). */
const ONE_BLOCK = 262_144;

/** A one-block UnixFS file as dag-pb serialises it: PBNode{ Data: UnixFS{ Type: File, Data: bytes, filesize } }, no links. */
function unixfsFileBlock(file: Uint8Array): Uint8Array {
  const inner = [0x08, 0x02, 0x12, ...varint(file.length)];
  const tail = [0x18, ...varint(file.length)];
  const head = [0x0a, ...varint(inner.length + file.length + tail.length), ...inner];
  const out = new Uint8Array(head.length + file.length + tail.length);
  out.set(head, 0);
  out.set(file, head.length);
  out.set(tail, head.length + file.length);
  return out;
}

const sameBytes = (a: Uint8Array, b: Uint8Array): boolean => a.length === b.length && a.every((x, i) => x === b[i]);
const fromHex = (h: string): Uint8Array => new Uint8Array((h.match(/../g) ?? []).map((x) => parseInt(x, 16)));

/**
 * Whether `bytes` are what `cid` names: `true` (they hash to it), `false` (they do not), or `null` when this cannot be checked without the whole DAG
 * (a path inside a directory, a file of more than one block, a codec or hash this check does not know): the caller then relies on the gateway.
 */
export async function verifyIpfsBytes(cid: string, path: string, bytes: Uint8Array, sha256: (b: Uint8Array) => Promise<string | null> = sha256Hex): Promise<boolean | null> {
  if (path !== '') return null;
  const c = parseCid(cid);
  if (!c) return null;
  if (c.codec === 'dag-pb' && bytes.length > ONE_BLOCK) return null;
  // a CIDv1 raw leaf is the file itself; a dag-pb CID names the wrapped block (a file of one block). A file of at most 256 KiB that does not hash to its
  // dag-pb CID in that form is treated as forged (one imported with a smaller chunk size is many blocks and shows a letter instead: the safe direction).
  const hash = await sha256(c.codec === 'raw' ? bytes : unixfsFileBlock(bytes));
  if (hash === null) return null;
  return sameBytes(fromHex(hash), c.digest);
}

// ------------------------------------------------------------------------------------------------ IPFS: fetch through the gateway

export interface IpfsFetchOptions {
  fetch?: typeof fetch;
  signal?: AbortSignal | undefined;
  /** default 10 s */
  timeoutMs?: number;
  /** default 1.5 MB */
  maxBytes?: number;
  sha256?: (b: Uint8Array) => Promise<string | null>;
  /** the creator's committed SHA-256 of the file (64 lowercase hex): the fetched bytes must equal it, else `hashMismatch` */
  imageHash?: string | null | undefined;
  onMismatch?: OnHashMismatch | undefined;
  /** draws a checked file that is not PNG, JPEG, WEBP or GIF to a raster picture (default `rasterize` of data/svg-logo.ts) */
  rasterize?: Rasterize;
}

export interface IpfsImage { bytes: Uint8Array; mime: ImageMime; checked: boolean; /** the bytes were compared with `imageHash` and are equal */ hashChecked?: boolean }

/** Called when the gateway served a picture that is not the file the creator's `image_hash` names (the picture is then not returned). */
export type OnHashMismatch = () => void;

/** The gateway URL of a reference (null when no gateway is configured, or the gateway is not https). */
export function ipfsGatewayUrl(gateway: string, ref: Extract<LogoRef, { kind: 'ipfs' }>): string | null {
  const g = gateway.trim().replace(/\/+$/, '');
  if (g === '') return null;
  try {
    const u = new URL(g);
    if (u.protocol !== 'https:' || u.username !== '' || u.password !== '' || refusedHost(u.hostname)) return null;
  } catch {
    return null;
  }
  return `${g}/${ref.cid}${ref.path}`;
}

/**
 * The image behind an `ipfs://` logo, fetched as bytes from `gateway` (redirects are followed: dweb.link answers with its per-CID subdomain). Null when the gateway
 * does not answer, the file is too large, is not a raster image by its magic bytes (no SVG, no HTML), or its bytes are NOT what the CID names where that can be
 * checked (`verifyIpfsBytes`). `checked`: the bytes were compared with the CID; `hashChecked`: with the creator's `image_hash` (a different file is not returned: `onMismatch`).
 */
export async function fetchIpfsImage(gateway: string, ref: Extract<LogoRef, { kind: 'ipfs' }>, o: IpfsFetchOptions = {}): Promise<IpfsImage | null> {
  const url = ipfsGatewayUrl(gateway, ref);
  if (!url || o.signal?.aborted) return null;
  const fetchFn = o.fetch ?? ((...a: Parameters<typeof fetch>) => fetch(...a));
  const ctl = new AbortController();
  const timer = setTimeout(() => ctl.abort(), o.timeoutMs ?? 10_000);
  const onAbort = () => ctl.abort();
  o.signal?.addEventListener('abort', onAbort, { once: true });
  try {
    const res = await fetchFn(url, { signal: ctl.signal, credentials: 'omit', referrerPolicy: 'no-referrer', redirect: 'follow' });
    if (!res.ok) return null;
    const bytes = await readCapped(res, o.maxBytes ?? 1_500_000);
    if (!bytes || bytes.length === 0) return null;
    if (!isPicture(bytes)) return null;
    const ok = await verifyIpfsBytes(ref.cid, ref.path, bytes, o.sha256);
    if (ok === false) return null;
    if (o.imageHash) {
      const got = await (o.sha256 ?? sha256Hex)(bytes);
      if (got === null) return null;
      if (got !== o.imageHash) {
        o.onMismatch?.();
        return null;
      }
    }
    const shown = await shownPicture(bytes, o.rasterize);
    if (!shown) return null;
    return { bytes: shown.bytes, mime: shown.mime, checked: ok === true, ...(o.imageHash ? { hashChecked: true } : {}) };
  } catch {
    return null;
  } finally {
    clearTimeout(timer);
    o.signal?.removeEventListener('abort', onAbort);
  }
}

export interface IpfsLoader {
  /** The blob URL of the logo of `ref`, null when there is none. With `imageHash` the bytes must equal it, else null (and `mismatched` is true). Never rejects. */
  load(gateway: string, ref: Extract<LogoRef, { kind: 'ipfs' }>, imageHash?: string | null): Promise<string | null>;
  /** Whether the gateway served another file than the one `imageHash` names (known after `load` resolved). */
  mismatched(gateway: string, ref: Extract<LogoRef, { kind: 'ipfs' }>, imageHash: string): boolean;
  clear(): void;
}

/** Loads `ipfs://` logos through a gateway, at most `limit` at once, one blob URL per (gateway, address), a failure not retried for `failTtlMs` (default 5 min). */
export function createIpfsLoader(o: IpfsFetchOptions & { limit?: number; failTtlMs?: number; now?: () => number; makeUrl?: (bytes: Uint8Array, mime: ImageMime) => string } = {}): IpfsLoader {
  const limit = Math.max(1, o.limit ?? 3);
  const failTtlMs = o.failTtlMs ?? 300_000;
  const now = o.now ?? Date.now;
  const makeUrl = o.makeUrl ?? ((bytes, mime) => URL.createObjectURL(new Blob([bytes as Uint8Array<ArrayBuffer>], { type: mime })));
  const done = new Map<string, string>();
  const failed = new Map<string, number>();
  const wrong = new Set<string>();
  const inflight = new Map<string, Promise<string | null>>();
  const keyOf = (gateway: string, ref: Extract<LogoRef, { kind: 'ipfs' }>, hash?: string | null) => `${gateway}|${ref.cid}${ref.path}|${hash ?? ''}`;
  let active = 0;
  const queue: (() => void)[] = [];
  const slot = async <T>(fn: () => Promise<T>): Promise<T> => {
    if (active >= limit) await new Promise<void>((resolve) => queue.push(resolve));
    active++;
    try {
      return await fn();
    } finally {
      active--;
      queue.shift()?.();
    }
  };
  return {
    load(gateway, ref, imageHash) {
      const key = keyOf(gateway, ref, imageHash);
      const hit = done.get(key);
      if (hit) return Promise.resolve(hit);
      const running = inflight.get(key);
      if (running) return running;
      const failedAt = failed.get(key);
      if (failedAt !== undefined && now() - failedAt < failTtlMs) return Promise.resolve(null);
      // not tied to the caller's signal: a row scrolled away must not cancel what the next visit will use
      const p = slot(() => fetchIpfsImage(gateway, ref, { ...o, signal: undefined, imageHash: imageHash ?? null, onMismatch: () => wrong.add(key) }))
        .then((img) => {
          if (!img) {
            failed.set(key, now());
            return null;
          }
          const url = makeUrl(img.bytes, img.mime);
          done.set(key, url);
          return url;
        })
        .finally(() => inflight.delete(key));
      inflight.set(key, p);
      return p;
    },
    mismatched: (gateway, ref, imageHash) => wrong.has(keyOf(gateway, ref, imageHash)),
    clear() {
      done.clear();
      failed.clear();
      wrong.clear();
      inflight.clear();
    },
  };
}

/** The app's one IPFS logo loader: the session's blob URLs. */
export const ipfsLoader: IpfsLoader = createIpfsLoader();

// ------------------------------------------------------------------------------------------------ https: read the bytes, compare with the hash

/**
 * What reading an `https://` logo and comparing it with the creator's `image_hash` gave:
 *   `match`      the bytes were readable (the host allows it with a CORS header) and hash to `image_hash`: shown from a blob URL as checked;
 *   `mismatch`   readable and a different file: the picture at the address is not the one the creation record names, nothing is shown;
 *   `refused`    readable and equal to the hash but not a picture (HTML ..., or an SVG the browser cannot draw): never shown;
 *   `unreadable` the page could not read it (no CORS header, offline, an error status, too large, no `crypto.subtle`): nothing could be checked,
 *                the caller falls back to the unchecked <img>.
 */
export type HttpsCheck = { kind: 'match'; bytes: Uint8Array; mime: ImageMime } | { kind: 'mismatch' | 'refused' | 'unreadable' };

export interface HttpsCheckOptions {
  fetch?: typeof fetch;
  signal?: AbortSignal | undefined;
  /** default 10 s */
  timeoutMs?: number;
  /** default MAX_LOGO_BYTES */
  maxBytes?: number;
  sha256?: (b: Uint8Array) => Promise<string | null>;
  /** draws a checked file that is not PNG, JPEG, WEBP or GIF to a raster picture (default `rasterize` of data/svg-logo.ts) */
  rasterize?: Rasterize;
}

/**
 * Reads an `https://` logo as bytes and compares them with `imageHash` (64 lowercase hex). The request carries no credentials and no referrer and runs in
 * CORS mode: a host that does not send `Access-Control-Allow-Origin` makes it fail, which is `unreadable` (the common case: the page then shows the
 * unchecked <img>, as for a logo with no hash). The URL is one `safeHttpsLogo` accepted.
 */
export async function checkHttpsImage(url: string, imageHash: string, o: HttpsCheckOptions = {}): Promise<HttpsCheck> {
  if (o.signal?.aborted) return { kind: 'unreadable' };
  const fetchFn = o.fetch ?? ((...a: Parameters<typeof fetch>) => fetch(...a));
  const ctl = new AbortController();
  const timer = setTimeout(() => ctl.abort(), o.timeoutMs ?? 10_000);
  const onAbort = () => ctl.abort();
  o.signal?.addEventListener('abort', onAbort, { once: true });
  try {
    const res = await fetchFn(url, { mode: 'cors', credentials: 'omit', referrerPolicy: 'no-referrer', signal: ctl.signal });
    if (!res.ok) return { kind: 'unreadable' };
    const bytes = await readCapped(res, o.maxBytes ?? MAX_LOGO_BYTES);
    if (!bytes || bytes.length === 0) return { kind: 'unreadable' };
    const got = await (o.sha256 ?? sha256Hex)(bytes);
    if (got === null) return { kind: 'unreadable' };
    if (got !== imageHash) return { kind: 'mismatch' };
    // the ORIGINAL bytes hash to the commitment: shown as they are, or as the picture drawn from them (SVG, AVIF ...)
    const r = await shownPicture(bytes, o.rasterize);
    return r ? { kind: 'match', bytes: r.bytes, mime: r.mime } : { kind: 'refused' };
  } catch {
    return { kind: 'unreadable' };
  } finally {
    clearTimeout(timer);
    o.signal?.removeEventListener('abort', onAbort);
  }
}

export type HttpsOutcome = { kind: 'match'; src: string } | { kind: 'mismatch' | 'refused' | 'unreadable' };

export interface HttpsChecker {
  /** The outcome for `url` against `imageHash` (once per pair per session; an unreadable one again after `failTtlMs`). Never rejects. A match carries a blob URL. */
  check(url: string, imageHash: string): Promise<HttpsOutcome>;
  clear(): void;
}

/** Checks `https://` logos against their hashes, at most `limit` at once (default 3), remembering outcomes. */
export function createHttpsChecker(o: HttpsCheckOptions & { limit?: number; failTtlMs?: number; now?: () => number; makeUrl?: (bytes: Uint8Array, mime: ImageMime) => string } = {}): HttpsChecker {
  const limit = Math.max(1, o.limit ?? 3);
  const failTtlMs = o.failTtlMs ?? 300_000;
  const now = o.now ?? Date.now;
  const makeUrl = o.makeUrl ?? ((bytes, mime) => URL.createObjectURL(new Blob([bytes as Uint8Array<ArrayBuffer>], { type: mime })));
  const done = new Map<string, { out: HttpsOutcome; at: number }>();
  const inflight = new Map<string, Promise<HttpsOutcome>>();
  let active = 0;
  const queue: (() => void)[] = [];
  const slot = async <T>(fn: () => Promise<T>): Promise<T> => {
    if (active >= limit) await new Promise<void>((resolve) => queue.push(resolve));
    active++;
    try {
      return await fn();
    } finally {
      active--;
      queue.shift()?.();
    }
  };
  return {
    check(url, imageHash) {
      const key = `${url}|${imageHash}`;
      const hit = done.get(key);
      if (hit && (hit.out.kind !== 'unreadable' || now() - hit.at < failTtlMs)) return Promise.resolve(hit.out);
      const running = inflight.get(key);
      if (running) return running;
      const p = slot(() => checkHttpsImage(url, imageHash, { ...o, signal: undefined }))
        .then((r): HttpsOutcome => {
          const out: HttpsOutcome = r.kind === 'match' ? { kind: 'match', src: makeUrl(r.bytes, r.mime) } : { kind: r.kind };
          done.set(key, { out, at: now() });
          return out;
        })
        .finally(() => inflight.delete(key));
      inflight.set(key, p);
      return p;
    },
    clear() {
      done.clear();
      inflight.clear();
    },
  };
}

/** The app's one checker for `https://` logos with a committed hash. */
export const httpsChecker: HttpsChecker = createHttpsChecker();
