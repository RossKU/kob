// Canonical JSON and the hashes of the binding (`kaspa-exact-v2.md`, "Canonical request authorization"), byte for
// byte the same as `crates/kob-x402/src/canonical.rs` and the reference SDK (`stableStringify`): object keys in
// ascending UTF-16 code-unit order (the default JS sort), arrays in order, compact, integers only (no floats),
// no whitespace. `undefined` object members are skipped (as JSON.stringify does); other non-JSON values throw.

import { createHash } from 'node:crypto';
import { KobX402Error } from './errors.ts';

/** Deepest nesting accepted (resource bound, same as the Rust canonicalizer). */
export const MAX_DEPTH = 32;

export function canonicalJson(value: unknown): string {
  return write(value, 0, '$');
}

function write(v: unknown, depth: number, path: string): string {
  if (depth > MAX_DEPTH) throw bad(`${path}: nesting exceeds the canonicalization bound`);
  if (v === null) return 'null';
  switch (typeof v) {
    case 'boolean':
      return v ? 'true' : 'false';
    case 'string':
      return JSON.stringify(v);
    case 'number':
      if (!Number.isSafeInteger(v)) throw bad(`${path}: only safe integers are in the canonical JSON profile`);
      return String(v);
    case 'bigint':
      throw bad(`${path}: bigint must be written as a decimal string`);
    case 'object': {
      if (Array.isArray(v)) {
        return '[' + v.map((x, i) => write(x === undefined ? null : x, depth + 1, `${path}[${i}]`)).join(',') + ']';
      }
      const proto = Object.getPrototypeOf(v);
      if (proto !== Object.prototype && proto !== null) throw bad(`${path}: not a plain JSON object`);
      const rec = v as Record<string, unknown>;
      const parts: string[] = [];
      for (const k of Object.keys(rec).sort()) {
        const x = rec[k];
        if (x === undefined) continue;
        parts.push(JSON.stringify(k) + ':' + write(x, depth + 1, `${path}.${k}`));
      }
      return '{' + parts.join(',') + '}';
    }
    default:
      throw bad(`${path}: ${typeof v} is not JSON`);
  }
}

function bad(message: string): KobX402Error {
  return new KobX402Error('invalid_canonical_json', message);
}

export function sha256(bytes: Uint8Array | string): Uint8Array {
  return new Uint8Array(createHash('sha256').update(bytes).digest());
}

export function sha256Hex(bytes: Uint8Array | string): string {
  return createHash('sha256').update(bytes).digest('hex');
}

export function canonicalHashHex(value: unknown): string {
  return sha256Hex(canonicalJson(value));
}

/** `paymentRequirementsHash`: SHA-256 of the canonical JSON of the complete selected requirements. */
export function requirementsHash(requirements: unknown): string {
  return canonicalHashHex(requirements);
}

/**
 * The reference SDK's default normalized HTTP request fingerprint:
 * `sha256(canonical({method, url, body|null, paymentRequirementsHash}))`. Client and server must feed the same
 * `method`, `url` (see `normalizeUrl`) and `body` (see `normalizeBody`).
 */
export function httpRequestHash(method: string, url: string, body: unknown, requirementsHashHex: string): string {
  return canonicalHashHex({ method, url, body: body === undefined ? null : body, paymentRequirementsHash: requirementsHashHex });
}

/** The URL form both sides hash: WHATWG-normalized absolute href. */
export function normalizeUrl(url: string | URL): string {
  try {
    return new URL(url).href;
  } catch (e) {
    throw new KobX402Error('bad_request', `not an absolute URL: ${String(url)}`, { cause: e });
  }
}

export function normalizeMethod(method: string | undefined): string {
  return (method ?? 'GET').toUpperCase();
}

/**
 * The body form both sides hash. Absent or empty -> `null`; text that parses as canonical-profile JSON -> the
 * parsed value; other text -> the string; bytes that are not UTF-8 -> `"sha256:<hex>"`. The client applies it to
 * the bytes it sends, the server to the bytes it receives, so both see the same value.
 */
export function normalizeBody(body: string | Uint8Array | null | undefined): unknown {
  if (body === undefined || body === null) return null;
  let text: string;
  if (typeof body === 'string') {
    text = body;
  } else {
    if (body.byteLength === 0) return null;
    try {
      text = new TextDecoder('utf-8', { fatal: true }).decode(body);
    } catch {
      return 'sha256:' + sha256Hex(body);
    }
  }
  if (text.length === 0) return null;
  try {
    const parsed: unknown = JSON.parse(text);
    canonicalJson(parsed);
    // The fingerprint must bind what the merchant parses, whatever its parser: a body that the JS parser reads one way and another
    // parser reads another (duplicate keys: last wins here, first wins elsewhere) or that spells a number in a form the canonical
    // integer profile collapses (1e2, 1.0, -0, 100000000000000000001 beyond a double) is hashed as TEXT, i.e. byte for byte.
    if (!isStrictProfileJson(text)) return text;
    return parsed;
  } catch {
    return text;
  }
}

/**
 * Whether JSON `text` (already valid per `JSON.parse`) is unambiguous under the canonical profile: no duplicate object key (compared
 * after unescaping) and every number is a plain integer without a fraction, exponent or minus zero (`-?(0|[1-9][0-9]*)`, a safe integer).
 */
export function isStrictProfileJson(text: string): boolean {
  const stack: { keys: Set<string> | null; wantKey: boolean }[] = [];
  const n = text.length;
  let i = 0;
  while (i < n) {
    const c = text[i] as string;
    if (c === '"') {
      let j = i + 1;
      while (j < n && text[j] !== '"') j += text[j] === '\\' ? 2 : 1;
      const top = stack[stack.length - 1];
      if (top && top.keys && top.wantKey) {
        const key = JSON.parse(text.slice(i, j + 1)) as string;
        if (top.keys.has(key)) return false;
        top.keys.add(key);
        top.wantKey = false;
      }
      i = j + 1;
    } else if (c === '{') {
      stack.push({ keys: new Set(), wantKey: true });
      i++;
    } else if (c === '[') {
      stack.push({ keys: null, wantKey: false });
      i++;
    } else if (c === '}' || c === ']') {
      stack.pop();
      i++;
    } else if (c === ',') {
      const top = stack[stack.length - 1];
      if (top && top.keys) top.wantKey = true;
      i++;
    } else if (c === '-' || (c >= '0' && c <= '9')) {
      let j = i + 1;
      while (j < n && /[0-9.eE+-]/.test(text[j] as string)) j++;
      const tok = text.slice(i, j);
      if (!/^-?(0|[1-9][0-9]*)$/.test(tok) || tok === '-0' || !Number.isSafeInteger(Number(tok))) return false;
      i = j;
    } else {
      i++; // whitespace, ':', true / false / null
    }
  }
  return true;
}
