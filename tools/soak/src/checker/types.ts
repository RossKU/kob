// Minimal structural views of the indexer read API (docs/ops/executor.md Part B §5, protocol v3: base units, no lots) used by the checker's pure logic. They are subsets of
// web/src/data/indexer-types.ts, declared here so the pure modules have no `@/` import (unit-testable with plain `node --test`).

export type Hex = string;

/** `{kind, state}` of a proven order state: 64-bit fields are decimal strings, hashes hex. */
export interface StateLike {
  kind: string;
  state: Record<string, string>;
}

export interface OutpointLike {
  txid: Hex;
  index: number;
  value: string | null;
}

export interface OrderLike {
  covenant_id: Hex;
  contract: string;
  side: number;
  maker?: Hex | null;
  token?: Hex | null;
  /** base units per whole token (the price denominator) */
  scale?: number | null;
  min_fill?: string | null;
  price?: string | null;
  tip?: string | null;
  tif?: number | null;
  expiry_daa?: number | null;
  active_from?: number | null;
  /** base units at creation (decimal string; null for a bid) */
  initial_amount?: string | null;
  parent?: Hex | null;
  genesis?: { txid: Hex | null; daa: number };
  status: string;
  filled_amount?: string;
  amount_left?: string | null;
  current?: OutpointLike | null;
  state_known?: boolean;
  state?: StateLike | null;
  current_daa?: number | null;
  kill_daa?: number | null;
  repeat?: { role: string } | null;
  custody?: { expected_amount: string | null; utxo: { txid: Hex; index: number; amount: string } | null; ok: boolean };
  last_daa: number;
  children?: Hex[];
}

export interface EventLike {
  id: number;
  covenant_id: Hex;
  token?: Hex | null;
  block_seq?: number;
  daa: number;
  ts?: number;
  txid: Hex;
  kind: string;
  side: number | null;
  /** base units (decimal string) */
  amount: string | null;
  price: string | null;
  payout: string | null;
  closes: boolean;
  detail?: unknown;
}

export interface HealthLike {
  state: string;
  cursor_daa: number;
  node_daa: number | null;
  lag_daa: number | null;
  lag_seconds?: number | null;
  last_progress_unix_ms?: number;
  orders_total?: number;
  counters?: { fills_total?: number; orders_total?: number; orders_by_status?: Record<string, number> };
  last_error?: string | null;
}

export type Severity = 'error' | 'warn';

export interface Violation {
  invariant: string;
  severity: Severity;
  subject: string;
  detail: Record<string, unknown>;
}

/** invariant ids (incident `invariant` field) */
export const INV = {
  custody: 'custody',
  stray: 'stray',
  allIn: 'all-in',
  iocFok: 'ioc-fok',
  trigger: 'trigger',
  repeat: 'repeat',
  x402: 'x402',
  fee: 'fee',
  agree: 'indexers-agree',
  pair: 'pair',
  supply: 'supply',
  health: 'health',
} as const;
