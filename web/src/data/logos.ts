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
//   4. The letter avatar.
//
// For every source: no SVG, no `data:` URL, no host that is an IP literal, `localhost` or a name without a dot (an address inside the viewer's network).
import { readCapped, sha256Hex, sniffImage, type ImageMime } from './kascov';
import { refusedHost, type LogoRef } from './logo-url';

export { parseLogoRef, refusedHost, safeHttpsLogo, type LogoRef } from './logo-url';

export { DEFAULT_IPFS_GATEWAY_URL } from '../config';

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
}

export interface IpfsImage { bytes: Uint8Array; mime: ImageMime; checked: boolean }

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
 * checked (`verifyIpfsBytes`). `checked`: the bytes were compared with the CID.
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
    const mime = sniffImage(bytes);
    if (!mime) return null;
    const ok = await verifyIpfsBytes(ref.cid, ref.path, bytes, o.sha256);
    if (ok === false) return null;
    return { bytes, mime, checked: ok === true };
  } catch {
    return null;
  } finally {
    clearTimeout(timer);
    o.signal?.removeEventListener('abort', onAbort);
  }
}

export interface IpfsLoader {
  /** The blob URL of the logo of `ref`, null when there is none. Never rejects. */
  load(gateway: string, ref: Extract<LogoRef, { kind: 'ipfs' }>): Promise<string | null>;
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
  const inflight = new Map<string, Promise<string | null>>();
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
    load(gateway, ref) {
      const key = `${gateway}|${ref.cid}${ref.path}`;
      const hit = done.get(key);
      if (hit) return Promise.resolve(hit);
      const running = inflight.get(key);
      if (running) return running;
      const failedAt = failed.get(key);
      if (failedAt !== undefined && now() - failedAt < failTtlMs) return Promise.resolve(null);
      // not tied to the caller's signal: a row scrolled away must not cancel what the next visit will use
      const p = slot(() => fetchIpfsImage(gateway, ref, { ...o, signal: undefined }))
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
    clear() {
      done.clear();
      failed.clear();
      inflight.clear();
    },
  };
}

/** The app's one IPFS logo loader: the session's blob URLs. */
export const ipfsLoader: IpfsLoader = createIpfsLoader();
