// The token table the signer frame pins (audit 2026-10-30 G7): the covenant id, ticker and decimals of every token of the registry the build
// ships. The app sends its table with a signing request; the frame hashes it (canonical form below) and reads a listed token's ticker and
// decimals from the table only when the hash is the one pinned into the bundle at build time (vite.config.ts, from registry/tokens.json or the
// file `KOB_SIGNER_REGISTRY` names). Any other token is shown as unverified ("decimals assumed"), as one the registry does not list is
// (trusted-hints.ts). The hash is of a table, never of the app's word. Pure, as vite.config.ts hashes the registry file with it.
import { sha256 } from '@noble/hashes/sha2.js';
import type { Hex } from '../kob/types';

/** One token of the table. */
export interface RegistryRow {
  covenantId: Hex;
  ticker: string;
  decimals: number;
}

/** Rows ordered by covenant id, each as `[covenantId, ticker, decimals]`, in JSON: the one form that is hashed. */
export function canonicalTable(rows: readonly RegistryRow[]): string {
  const sorted = [...rows].sort((a, b) => (a.covenantId < b.covenantId ? -1 : a.covenantId > b.covenantId ? 1 : 0));
  return JSON.stringify(sorted.map((r) => [r.covenantId, r.ticker, r.decimals]));
}

const hex = (b: Uint8Array): string => Array.from(b, (x) => x.toString(16).padStart(2, '0')).join('');

/** The SHA-256 (hex) of the canonical table. */
export function tableHash(rows: readonly RegistryRow[]): string {
  return hex(sha256(new TextEncoder().encode(canonicalTable(rows))));
}

/** The table of a registry file (`registry/tokens.json`): the covenant id, ticker and decimals of each of its tokens. */
export function rowsOfRegistryJson(json: unknown): RegistryRow[] {
  const tokens = (json as { tokens?: unknown }).tokens;
  if (!Array.isArray(tokens)) throw new Error('the registry has no tokens array');
  return tokens.map((t: { covenant_id: Hex; ticker: string; decimals: number }) => ({ covenantId: t.covenant_id, ticker: t.ticker, decimals: t.decimals }));
}
