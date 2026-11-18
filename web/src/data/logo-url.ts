// The rules for the address of a token logo (data/logos.ts, data/kascov.ts), meaning which
// addresses the app will ever load an image from. Pure, with no dependencies.
//
// For every source the address is https or `ipfs://`, with no credentials and no `data:` URL.
// The host is never an IP literal, `localhost`, a name without a dot or a common internal suffix,
// which would be an address inside the viewer's own network.

/**
 * A logo address the app may load. `svg` marks an address that names an SVG file. Such a file is
 * shown only once its bytes were read and checked (data/svg-logo.ts), never as an unchecked <img>.
 */
export type LogoRef = { kind: 'https'; url: string; svg?: true } | { kind: 'ipfs'; cid: string; path: string; svg?: true };

const MAX_URL = 2048;

/**
 * Whether the app never loads a logo from this host. That is an IP literal (any spelling the URL
 * parser normalised to one), `localhost`, a name without a dot or a common internal suffix.
 */
export function refusedHost(hostname: string): boolean {
  const h = hostname.toLowerCase().replace(/\.$/, '');
  if (h === '') return true;
  if (h.startsWith('[') || h.includes(':')) return true; // IPv6 literal
  if (/^\d+(\.\d+){0,3}$/.test(h) || /^0x[0-9a-f]+$/i.test(h)) return true; // IPv4 literal, in any form that is only digits
  if (h === 'localhost' || h.endsWith('.localhost')) return true;
  if (!h.includes('.')) return true;
  return /\.(local|internal|lan|home|corp|intranet)$/.test(h);
}

const SVG_PATH = /\.svgz?$/i;

/** An `https://` logo address, or null: not https, credentials, too long, SVG, a refused host. */
export function safeHttpsLogo(v: unknown): string | null {
  if (typeof v !== 'string' || v.length > MAX_URL || /[\u0000- \u007f]/.test(v)) return null;
  try {
    const u = new URL(v);
    if (u.protocol !== 'https:' || u.username !== '' || u.password !== '' || refusedHost(u.hostname)) return null;
    if (SVG_PATH.test(u.pathname)) return null;
    return u.href;
  } catch {
    return null;
  }
}

/**
 * An `https://` address of an SVG logo (`.svg` or the compressed `.svgz`), with the other rules of
 * `safeHttpsLogo`. Null otherwise.
 */
function httpsSvgLogo(v: string): string | null {
  if (v.length > MAX_URL || /[\u0000- \u007f]/.test(v)) return null;
  try {
    const u = new URL(v);
    if (u.protocol !== 'https:' || u.username !== '' || u.password !== '' || refusedHost(u.hostname)) return null;
    return SVG_PATH.test(u.pathname) ? u.href : null;
  } catch {
    return null;
  }
}

// CIDv0 (`Qm...`, base58btc of a sha2-256 multihash) or CIDv1 in base32 (`b...`).
const CID_RE = /^(Qm[1-9A-HJ-NP-Za-km-z]{44}|b[a-z2-7]{50,120})$/;
const IPFS_RE = /^ipfs:\/\/([A-Za-z0-9]+)((?:\/[A-Za-z0-9._~%-]+)*)\/?$/;

/**
 * What the app may load for a logo address, `https://...` or `ipfs://<cid>[/path]`. Anything else
 * is null, such as data:, http:, javascript: or a malformed URL.
 */
export function parseLogoRef(raw: unknown): LogoRef | null {
  if (typeof raw !== 'string') return null;
  const v = raw.trim();
  if (v.length === 0 || v.length > MAX_URL) return null;
  const https = safeHttpsLogo(v);
  if (https) return { kind: 'https', url: https };
  const svg = httpsSvgLogo(v);
  if (svg) return { kind: 'https', url: svg, svg: true };
  const m = IPFS_RE.exec(v);
  if (!m || !CID_RE.test(m[1]!)) return null;
  const path = m[2] ?? '';
  if (path.split('/').some((s) => s === '.' || s === '..') || path.length > 512) return null;
  return SVG_PATH.test(path) ? { kind: 'ipfs', cid: m[1]!, path, svg: true } : { kind: 'ipfs', cid: m[1]!, path };
}
