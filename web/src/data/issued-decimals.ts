// The decimals of the tokens issued from this browser, per network: the issuer chose them, and nothing on chain records them. They only prefill
// the decimals field of a token's first order (`TokenPage`), where the user sees and confirms them. Per-browser convenience: a failed or
// blocked storage leaves the field empty, never another value.
import { defaultStorage, type StorageLike } from '../config';

const KEY = 'kob.issuedDecimals.v1';
/** Most entries kept (the oldest are dropped). */
const MAX_ENTRIES = 200;

type Memo = Record<string, Record<string, number>>;

const isDecimals = (d: unknown): d is number => typeof d === 'number' && Number.isInteger(d) && d >= 0 && d <= 18;

function read(storage: StorageLike | null): Memo {
  try {
    const raw = storage?.getItem(KEY);
    const v: unknown = raw ? JSON.parse(raw) : null;
    if (!v || typeof v !== 'object' || Array.isArray(v)) return {};
    const out: Memo = {};
    for (const [net, m] of Object.entries(v as Record<string, unknown>)) {
      if (!m || typeof m !== 'object' || Array.isArray(m)) continue;
      out[net] = Object.fromEntries(Object.entries(m as Record<string, unknown>).filter((e): e is [string, number] => /^[0-9a-f]{64}$/.test(e[0]) && isDecimals(e[1])));
    }
    return out;
  } catch {
    return {};
  }
}

/** Remembers the decimals of a token issued here. False when the storage is unavailable or the value is not one an issuance accepts. */
export function rememberIssuedDecimals(network: string, covenantId: string, decimals: number, storage: StorageLike | null = defaultStorage()): boolean {
  const id = covenantId.toLowerCase();
  if (!storage || !/^[0-9a-f]{64}$/.test(id) || !isDecimals(decimals)) return false;
  try {
    const memo = read(storage);
    const entries = Object.entries(memo[network] ?? {}).filter(([k]) => k !== id);
    entries.push([id, decimals]);
    memo[network] = Object.fromEntries(entries.slice(-MAX_ENTRIES));
    storage.setItem(KEY, JSON.stringify(memo));
    return true;
  } catch {
    return false;
  }
}

/** The decimals remembered for a token issued here on `network`, else null. */
export function issuedDecimals(network: string, covenantId: string, storage: StorageLike | null = defaultStorage()): number | null {
  return read(storage)[network]?.[covenantId.toLowerCase()] ?? null;
}
