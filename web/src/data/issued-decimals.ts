// The tokens issued from this browser, per network: their decimals, ticker and name, which the issuer chose and nothing on chain records
// (the genesis carries no metadata). They prefill the decimals field of a token's first order (`TokenPage`), where the user sees and confirms
// them, and name a token the registry does not list wherever it is shown (app/token-display.ts), marked unverified. Per-browser convenience: a
// failed or blocked storage leaves the field empty and the token unnamed, never another value.
import { defaultStorage, type StorageLike } from '../config';

const KEY = 'kob.issuedDecimals.v1';
/** Most entries kept (the oldest are dropped). */
const MAX_ENTRIES = 200;

/** What this browser knows of a token it issued. */
export interface IssuedToken {
  decimals: number;
  /** empty for an entry of an older build (decimals only) */
  ticker: string;
  name: string;
}

type Memo = Record<string, Record<string, IssuedToken>>;

const isDecimals = (d: unknown): d is number => typeof d === 'number' && Number.isInteger(d) && d >= 0 && d <= 18;
/** A ticker or name as a label: printable, short (an entry written by hand is dropped, not shown). */
const label = (v: unknown, max: number): string => (typeof v === 'string' && v.length <= max && /^[\x20-\x7e]*$/.test(v) ? v.trim() : '');

/** One entry: the decimals alone (older builds), or the issued token. */
function entryOf(v: unknown): IssuedToken | null {
  if (isDecimals(v)) return { decimals: v, ticker: '', name: '' };
  if (!v || typeof v !== 'object' || Array.isArray(v)) return null;
  const o = v as Record<string, unknown>;
  return isDecimals(o.decimals) ? { decimals: o.decimals, ticker: label(o.ticker, 24), name: label(o.name, 64) } : null;
}

function read(storage: StorageLike | null): Memo {
  try {
    const raw = storage?.getItem(KEY);
    const v: unknown = raw ? JSON.parse(raw) : null;
    if (!v || typeof v !== 'object' || Array.isArray(v)) return {};
    const out: Memo = {};
    for (const [net, m] of Object.entries(v as Record<string, unknown>)) {
      if (!m || typeof m !== 'object' || Array.isArray(m)) continue;
      out[net] = {};
      for (const [id, e] of Object.entries(m as Record<string, unknown>)) {
        const x = /^[0-9a-f]{64}$/.test(id) ? entryOf(e) : null;
        if (x) out[net]![id] = x;
      }
    }
    return out;
  } catch {
    return {};
  }
}

/** Remembers a token issued here. False when the storage is unavailable or the decimals are not ones an issuance accepts. */
export function rememberIssuedToken(network: string, covenantId: string, token: { decimals: number; ticker?: string; name?: string }, storage: StorageLike | null = defaultStorage()): boolean {
  const id = covenantId.toLowerCase();
  if (!storage || !/^[0-9a-f]{64}$/.test(id) || !isDecimals(token.decimals)) return false;
  try {
    const memo = read(storage);
    const entries = Object.entries(memo[network] ?? {}).filter(([k]) => k !== id);
    entries.push([id, { decimals: token.decimals, ticker: label(token.ticker ?? '', 24), name: label(token.name ?? '', 64) }]);
    memo[network] = Object.fromEntries(entries.slice(-MAX_ENTRIES));
    storage.setItem(KEY, JSON.stringify(memo));
    return true;
  } catch {
    return false;
  }
}

/** Remembers the decimals of a token issued here (no ticker or name). */
export function rememberIssuedDecimals(network: string, covenantId: string, decimals: number, storage: StorageLike | null = defaultStorage()): boolean {
  return rememberIssuedToken(network, covenantId, { decimals }, storage);
}

/** The token issued here on `network` under `covenantId`, else null. */
export function issuedToken(network: string, covenantId: string, storage: StorageLike | null = defaultStorage()): IssuedToken | null {
  return read(storage)[network]?.[covenantId.toLowerCase()] ?? null;
}

/** The decimals remembered for a token issued here on `network`, else null. */
export function issuedDecimals(network: string, covenantId: string, storage: StorageLike | null = defaultStorage()): number | null {
  return issuedToken(network, covenantId, storage)?.decimals ?? null;
}
