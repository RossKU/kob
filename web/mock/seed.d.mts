import type { MockChain } from './chain.mjs';

/** A key spec (test key name such as "alice", or a 64-hex x-only pubkey) to a pubkey. */
export function resolveKey(v: string | undefined, dflt?: string): string;
/** Builds an AnyState (kob-wasm JSON) for a KobAsk / KobBid from a compact spec. */
export function buildOrderState(chain: MockChain, spec: Record<string, unknown>): { kind: string; state: Record<string, string> };
export function seedOneOrder(chain: MockChain, spec: Record<string, unknown>): any;
export function applyDefaultSeed(chain: MockChain): any;
export function applySeed(chain: MockChain, spec?: Record<string, unknown>): any;
export function openListTokenSpec(over?: Record<string, unknown>): Record<string, unknown>;
export function seedOpenListToken(chain: MockChain, over?: Record<string, unknown>, opts?: { mid?: number; levels?: number; maker?: string }): any;
export function defaultTokenSpec(): { ticker: string; name: string; covenant_id: string; program: string; extension_commitment: string | null; decimals: number; tick: number };
/** Sompi per whole token the default book is centred on. */
export const DEFAULT_MID: number;
/** The scale seeded orders of a token quote at (its standard scale, else its `order_scale`, else 1000). */
export function scaleOf(tok: { scale?: number | null; order_scale?: number | null }): bigint;

export interface HistorySpec {
  /** token covenant id or ticker; default the first token */
  token?: string;
  /** span of the history before now (default 48) */
  hours?: number;
  /** number of trades (default 600) */
  trades?: number;
  /** PRNG seed: the same seed gives the same trades (default 1) */
  seed?: number;
  /** sompi per whole token the walk ends at (default 2500000, the default seed's mid) */
  mid?: number;
  /** price step, sompi per whole token (default the token's tick, else 10000) */
  tick?: number;
}
export interface TradeLeg {
  side: 'ask' | 'bid';
  /** sompi per whole token */
  price: string | number;
  /** base units (default one whole token) */
  amount?: string | number | bigint;
  maker?: string;
  scale?: number;
  /** DAA between the order's placement and the trade (default 100); the older side rests */
  age_daa?: number;
}
export interface TradeSpec {
  token?: string;
  /** unix ms (not in the future); or `ago_ms` before now */
  ts?: number;
  ago_ms?: number;
  txid?: string;
  legs: TradeLeg[];
}
export function mulberry32(seed: number): () => number;
export function sortEventsByDaa(chain: MockChain): void;
export function seedTrade(chain: MockChain, spec: TradeSpec): { txid: string; ts: number; daa: number; orders: string[] };
export function seedHistory(chain: MockChain, spec?: HistorySpec): { token: string; trades: number; fills: number; from_ts: number; to_ts: number };
