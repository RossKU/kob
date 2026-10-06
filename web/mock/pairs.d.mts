// Type declarations of the mock pair helpers (implementation: pairs.mjs). Only the surface tests use is typed.
import type { MockChain } from './chain.mjs';

/**
 * A pair order spec (prices B base units per WHOLE A, amounts base units of A, tips KAS sompi per whole A): kind, side ('ask' sells A, 'bid' buys
 * A), base / quote (ticker or covenant id), amount, price (KobPair / KobIfdPair), a conditional's stop / tp, an entry's entryStop / prefund /
 * rptAmount / exit, decay terms, minFill, maker, value, daa (backdate).
 */
export interface PairOrderSpec {
  kind?: 'KobPair' | 'KobCondPair' | 'KobIfdPair';
  side: 'ask' | 'bid' | 'sell' | 'buy';
  base: string;
  quote: string;
  amount?: bigint | number | string;
  price?: bigint | number | string;
  tip?: bigint | number | string;
  tif?: number;
  minFill?: bigint | number | string;
  maker?: string;
  activeFrom?: bigint | number;
  expiryDaa?: bigint | number;
  slope?: bigint | number | string;
  priceEnd?: bigint | number | string;
  decayStep?: bigint | number;
  interval?: bigint | number;
  maxFill?: bigint | number | string;
  stop?: bigint | number | string;
  tp?: bigint | number | string;
  slipBps?: number;
  trailStep?: bigint | number | string;
  trailGap?: bigint | number | string;
  trailWait?: number;
  minTouch?: bigint | number | string;
  minRestDaa?: number;
  bandDaa?: number;
  entryStop?: bigint | number | string;
  prefund?: bigint | number | string;
  rptAmount?: bigint | number | string;
  exit?: { tp?: bigint | number | string; stop?: bigint | number | string; slipBps?: number; trailStep?: bigint | number | string; trailGap?: bigint | number | string };
  value?: bigint | number | string;
  daa?: number;
}
export function pairOrderState(chain: MockChain, spec: PairOrderSpec): { any: { kind: string; state: Record<string, string> }; value: bigint };
export function seedPairOrder(chain: MockChain, spec: PairOrderSpec): { covenantId: string; [k: string]: any };
export function seedPair(chain: MockChain, opts?: { base?: string; quote?: unknown; maker?: string; quoteMid?: number; levels?: number }): { base: string; quote: string; orders: string[] };
export function quoteTokenSpec(over?: Record<string, unknown>): Record<string, unknown>;
