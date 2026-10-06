// Adapter over the `pair` object of an indexer order view (docs/ops/executor.md 5.4, `GET /v1/orders/{id}` of a pair order). Read through
// `pairOfView` only, so a field the doc does not fix (or a view of an older indexer without `pair`) is handled in one place.
import type { OrderView, TokenUtxoView } from '../data/indexer-types';
import type { Hex } from './types';

/** One custody of a pair order's current state, in record order (a sell-first entry: its A, then its B prefund). */
export interface PairCustodyView {
  token: Hex;
  /** `base` = A, `quote` = B */
  role: 'base' | 'quote';
  /** decimal string, base units of `token` */
  expected_amount: string;
  /** the live custody UTXO (single-order lookups and the live orders of a `maker=` list) */
  utxo?: TokenUtxoView | null;
  ok?: boolean;
}

/** The `pair` object of a pair order's view: both tokens, prices in B base units per whole A, custodies. */
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
  /** B base units per whole A: a KobPair's price (a decay's start), a KobIfdPair's limit, a KobCondPair's take-profit / limit leg (null without one) */
  price: string | null;
  price_num?: string | null;
  price_den?: string | null;
  /** a conditional's stop or an entry's entryStop */
  stop_price?: string | null;
  /** the auction price now, else `price` */
  quote_now?: string | null;
  amount_left?: string | null;
  /** a sell-first entry's B prefund per whole A */
  prefund?: string | null;
  delivery_carrier?: string | null;
  /** empty once closed */
  custodies: PairCustodyView[];
}

const isObj = (v: unknown): v is Record<string, unknown> => typeof v === 'object' && v !== null && !Array.isArray(v);

/** The view's `pair` object (a pair order), or null (a KAS-quoted order, or a view without it). */
export function pairOfView(v: OrderView | null | undefined): PairOrderView | null {
  const p = v ? (v as unknown as { pair?: unknown }).pair : null;
  if (!isObj(p) || typeof p.base !== 'string' || typeof p.quote !== 'string') return null;
  return { ...(p as unknown as PairOrderView), custodies: Array.isArray(p.custodies) ? (p.custodies as PairCustodyView[]) : [] };
}
