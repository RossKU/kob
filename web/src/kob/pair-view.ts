// Adapter over the `pair` object of an indexer order view, see `GET /v1/orders/{id}` in
// docs/ops/executor.md 5.4. Read it through `pairOfView` only, so a missing field or a view without
// `pair` is handled in one place.
import type { OrderView, TokenUtxoView } from '../data/indexer-types';
import type { Hex } from './types';

/** One custody of a pair order's current state, in record order. */
export interface PairCustodyView {
  token: Hex;
  /** `base` is token A and `quote` is token B. */
  role: 'base' | 'quote';
  /** A decimal string in base units of `token`. */
  expected_amount: string;
  /** The live custody UTXO, present in single-order lookups and live orders of a `maker=` list. */
  utxo?: TokenUtxoView | null;
  ok?: boolean;
}

/** The `pair` object of a pair order's view: both tokens, the prices and the custodies. */
export interface PairOrderView {
  base: Hex;
  quote: Hex;
  base_family: 'kcc20' | 'kron';
  quote_family: 'kcc20' | 'kron';
  base_template_hash: Hex;
  quote_template_hash: Hex;
  base_scale: number;
  quote_scale: number;
  side: 'ask' | 'bid';
  /**
   * Price in B base units per whole A.
   * It is the price of a KobPair (the start of a decay), the limit of a KobIfdPair, or the
   * take-profit or limit leg of a KobCondPair. It is null when there is no such leg.
   */
  price: string | null;
  price_num?: string | null;
  price_den?: string | null;
  /** The stop of a conditional, or the `entryStop` of an entry. */
  stop_price?: string | null;
  /** The current auction price, else `price`. */
  quote_now?: string | null;
  amount_left?: string | null;
  /** The B prefund per whole A of a sell-first entry. */
  prefund?: string | null;
  delivery_carrier?: string | null;
  /** Empty once the order is closed. */
  custodies: PairCustodyView[];
}

const isObj = (v: unknown): v is Record<string, unknown> => typeof v === 'object' && v !== null && !Array.isArray(v);

/** The view's `pair` object, or null for a KAS-quoted order or a view without it. */
export function pairOfView(v: OrderView | null | undefined): PairOrderView | null {
  const p = v ? (v as unknown as { pair?: unknown }).pair : null;
  if (!isObj(p) || typeof p.base !== 'string' || typeof p.quote !== 'string') return null;
  return { ...(p as unknown as PairOrderView), custodies: Array.isArray(p.custodies) ? (p.custodies as PairCustodyView[]) : [] };
}
