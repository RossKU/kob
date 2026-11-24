// Response shapes of the KOB indexer read API (`/v1`, crates/kobd, docs/ops/executor.md Part B 5).
//
// Conventions of the API:
// - JSON with snake_case keys. Ids, hashes and keys are lower-case hex.
// - Every sompi and token amount is a decimal string. DAA scores, counts and scales are plain
//   numbers.
// - Protocol v3 (docs/ops/executor.md 5.0) gives quantities in token base units, and prices and
//   tips in sompi per whole token, which is `scale` base units of the order's token.
// - Every chain-derived view carries `confirmations` and `settled` (>= settle_depth_daa).
// Fields that not every indexer serves are optional here, and the client tolerates their absence.
import type { AnyState, Family, Hex, TokenState } from '../kob/types';

/** One page of a cursor-paged list. */
export interface Page<T> { items: T[]; next_cursor: string | null }

/** `GET /v1/health`: the indexer's sync state against its node. */
export interface HealthView {
  state: 'starting' | 'following' | 'catching_up' | 'node_unavailable' | 'gap' | 'stopped' | string;
  /**
   * The follower made no progress for 10 minutes while its state still says it follows. Older
   * indexers omit it, which means false.
   */
  stale?: boolean;
  /** Why the follower stopped (state `stopped`). */
  halt_reason?: string | null;
  ok: boolean;
  network: string;
  node_version?: string | null;
  cursor_hash?: Hex | null;
  cursor_daa: number;
  /** Verified header time (Unix ms) of the cursor's chain block; null until known. Older indexers omit it. */
  cursor_time_unix_ms?: number | null;
  /** Chain proofs are off: the cursor block's time is the node's word. */
  chain_proofs_off?: boolean;
  /** The node's virtual DAA score at the last poll. */
  node_daa: number | null;
  lag_daa: number | null;
  lag_seconds: number | null;
  settle_depth_daa: number;
  last_error?: string | null;
  alarms?: unknown[];
  /**
   * Whether the store holds every order the node's history still shows. It is false during a fast
   * start or for a store that began late. Older executors omit it.
   */
  book_complete?: boolean;
  /**
   * Where this store's history begins, when that is not the node's pruning point. Absent for a
   * store that read the whole retained history.
   */
  history_from_daa?: number;
  /**
   * The KOB1 payload version the executor reads and writes (docs/api.md section 3.1). Older
   * executors omit it.
   */
  payload_version?: number;
}

/**
 * Capabilities of a token's program template that can act on holders or supply. `freeze` and
 * `seize` put escrowed orders at the issuer's discretion.
 */
export type TokenPower = 'mint-authority' | 'public-mint' | 'burn' | 'freeze' | 'seize' | 'blacklist';

/** One item of `GET /v1/tokens`. */
export interface IndexerTokenView {
  /**
   * The registry ticker. It is an empty string for a token that is not in the registry, which the
   * UI shows by its short covenant id instead.
   */
  ticker: string;
  covenant_id: Hex;
  template_hash: Hex | null;
  extension_commitment: Hex | null;
  /**
   * Set when order placements name several token programs for this covenant id. The indexer holds
   * no token output, so it names none: `template_hash` is null and `candidate_templates` lists the
   * claims. The program is then proven from a token output the node confirms, or the token is not
   * traded here. Absent otherwise.
   */
  program_ambiguous?: boolean;
  candidate_templates?: Hex[];
  /**
   * The token's standard scale, `10^decimals` and at most 10^9. An order is listed only when its
   * `scale` equals it. Null without decimals.
   */
  scale?: number | null;
  /**
   * For a token without decimals, the scale of its earliest order, open or ended. The first
   * order sets it, and it stays the market's after every order of it has ended. Null with decimals
   * and before any order. Older executors omit it.
   */
  market_scale?: number | null;
  decimals: number | null;
  open_asks: number;
  open_bids: number;
  /** Template capabilities, empty for most tokens. The client drops unknown values. */
  powers?: string[];
  /**
   * Facts the indexer derives from the registry's genesis record (matcher.md 1.2a), namely
   * `genesis_unverified` and `live_mint_authority`. Older executors omit it.
   */
  warnings?: string[];
  /** The audited program template of the token, null when unknown. */
  template_id?: string | null;
  family?: 'kcc20' | 'kron';
  /**
   * Set when the indexer holds a KCC-23 declaration of the token
   * (`GET /v1/tokens/{id}/declaration`, which the client verifies). Older executors omit it.
   */
  declaration?: true;
}

/**
 * An aggregated level of orders with the same state `price` and `scale`. `price` is sompi per
 * `scale` base units and `amount` is the summed base units. `amount_estimated` is set when the
 * level holds a bid, whose amount is its buying power.
 */
export interface LevelView { price: string; amount: string; amount_estimated: boolean; orders: number; scale: number }

/** `OrderView.curve`: a stepped sell curve as `GET /v1/orders/{id}` serves it. */
export interface CurveView {
  /** The current level, as a table index. */
  level: number;
  levels: number;
  /** Base units per level, and in all. */
  level_size: string;
  total: string;
  /** Sompi per whole token of the current level. Null when sold out. */
  price_now: string | null;
  /** The most one fill takes from here, the rest of 8 levels, in base units. */
  max_fill: string;
  /**
   * Created in its token's genesis and not yet filled or amended. `state.tokenCovId` is zero and
   * `token` is the custody's.
   */
  unbound: boolean;
  /** The levels left, the current one first. */
  remaining: { level: number; price: string; amount: string }[];
}

/** One order row of an unaggregated `GET /v1/books/{token}`. */
export interface BookOrderView {
  covenant_id: Hex; contract: string; maker: Hex | null; price: string;
  /** Priority tip, sompi per whole token. */
  tip: string | null;
  /** Smallest fill, in base units. */
  min_fill: string | null;
  scale: number | null;
  /** Base units left. For a bid it is its buying power, and `amount_estimated` is set. */
  amount_left: string | null;
  /**
   * What one fill can take from the order now, in the unit of `amount_left`: a TWAP or DCA order's
   * slice, an if-done bid's buying power after its carriers. Older executors omit it.
   */
  fillable_now?: string | null;
  amount_estimated: boolean;
  status: string; cur_value: string | null; expiry_daa: number | null; expired: boolean; genesis_daa: number;
  confirmations: number | null; settled: boolean;
  /**
   * The current price of a decay or rise auction at the node's DAA score, else `price`. Older
   * executors omit it.
   */
  quote?: string | null;
  /**
   * See `OrderView.possibly_frozen`. The server omits such orders from the book, so this is
   * normally false or absent.
   */
  possibly_frozen?: boolean;
  /**
   * A `KobCurveAsk` level, as its table index. There is one row per remaining level, all with the
   * curve's `covenant_id`.
   */
  curve_level?: number;
}

/**
 * Where a level of a pair book comes from (docs/ops/executor.md 5.4).
 * - `direct`: resting `KobPair` orders.
 * - `entry`: `KobIfdPair` entries resting at their limit, including a stop entry once armed.
 * - `route`: quotes implied through the two KAS books. They are indicative, without minimum fills,
 *   rounding or fees.
 */
export type PairLevelSource = 'direct' | 'entry' | 'route';
/**
 * One level of `GET /v1/pairs/{base}/{quote}/book`. The price is the exact rational
 * `price_num / price_den` QUOTE base units per BASE base unit. `amount` is in BASE base units. All
 * are decimal strings. The app adds `reversed` (ui/market/pair-model.ts `mergeOrientations`) to a
 * level of the other orientation's book, whose price it inverted.
 */
export interface PairLevelView { source: PairLevelSource; price_num: string; price_den: string; amount: string; orders: number; reversed?: true }
/**
 * `GET /v1/pairs/{base}/{quote}/book`. Asks buy BASE with QUOTE, ascending. Bids sell BASE for
 * QUOTE, descending.
 */
export interface PairBookView { base: Hex; quote: Hex; daa_score: number; asks: PairLevelView[]; bids: PairLevelView[] }
/**
 * One entry of `GET /v1/pairs?token=`, an oriented pair with at least one listed live pair order.
 * It counts:
 * - live `KobPair` asks and bids,
 * - sell-first and buy-first `KobIfdPair` entries,
 * - live `KobCondPair` orders, meaning stops, take-profits, OCO and exits.
 */
export interface PairSummaryView { base: Hex; quote: Hex; direct_asks: number; direct_bids: number; entry_asks: number; entry_bids: number; conditionals: number }

/**
 * An exact rate of a pair candle. `value` = floor(num / den) B base units per `price_basis` base
 * units of A.
 */
export interface PairRateView { value: string; num: string; den: string }
/**
 * One pair candle, derived from the two KAS series and never from pair fills.
 * - o = open(A) / open(B)
 * - c = close(A) / close(B)
 * - h = high(A) / low(B)
 * - l = low(A) / high(B)
 */
export interface PairCandleView {
  /** Bucket start, unix ms (UTC). */
  t: number;
  o: PairRateView; h: PairRateView; l: PairRateView; c: PairRateView;
  /** Whether A and B traded in KAS in the bucket. A side that did not carries its last close on. */
  a_traded: boolean; b_traded: boolean;
  /** Pair volume of the bucket from pair-order fills, in base units of A and B, and the count. */
  pair_volume_a: string; pair_volume_b: string; pair_fills: number;
}
/**
 * `GET /v1/pairs/{base}/{quote}/candles`. Rates are B base units per `price_basis` base units of
 * A, which is A's KAS price basis.
 */
export interface PairCandlesView {
  base: Hex; quote: Hex; interval: CandleInterval;
  price_basis: string; quote_price_basis: string;
  decimals: number | null; quote_decimals: number | null;
  /** Always `kas_books`. */
  price_source: string;
  items: PairCandleView[];
}
/**
 * How a pair fill was filled.
 * - `route`: the transaction also filled KAS-book orders of A or B.
 * - `netting`: against an opposite pair order.
 * - `inventory`: from the filler's own tokens.
 */
export type PairCounterparty = 'route' | 'netting' | 'inventory';
/**
 * One pair-order fill. It counts as volume only. `price_source` is always `none`, because a pair
 * fill never sets a price.
 */
export interface PairFillView {
  id: number; txid: Hex; ts: number; daa: number;
  /**
   * The pair of the filled order, added by the app (activity-model.ts `mergePairFills`) when it is
   * the other orientation of the view's pair.
   */
  base?: Hex; quote?: Hex;
  /** The filled pair order and its kind. */
  order: Hex; contract: string;
  /** `ask` when the order sold A, `bid` when it bought A. */
  side: 'ask' | 'bid';
  /** Base units of A filled, and base units of B the maker received (ask) or paid (bid). */
  amount_a: string; amount_b: string | null;
  /** The order's quote at the fill, as B per whole A (`a_scale` base units) and per A base unit. */
  price: string | null; price_num: string | null; price_den: string | null; a_scale: number;
  tip_kas: string | null;
  counterparty: PairCounterparty | string;
  price_source: string;
  confirmations: number | null; settled: boolean;
}
/** `GET /v1/pairs/{base}/{quote}/fills`, newest first, with the 24 h pair volume. */
export interface PairFillsView {
  base: Hex; quote: Hex;
  volume_24h: { amount_a: string; amount_b: string; fills: number };
  items: PairFillView[];
  next_cursor: string | null;
}

/**
 * A custody of a pair order (`OrderView.pair.custodies`). They come in record order, so a
 * sell-first entry lists its A, then its B prefund.
 */
export interface PairCustodyView {
  token: Hex;
  /** `base` for A, `quote` for B. */
  role: 'base' | 'quote' | string;
  expected_amount: string;
  /** Served by single-order lookups and for the live orders of a `maker=` list. */
  utxo?: TokenUtxoView | null;
  ok?: boolean;
}
/**
 * The `pair` object of a pair order's `GET /v1/orders/{id}` view, with prices in B base units per
 * whole A.
 */
export interface PairOrderView {
  base: Hex; quote: Hex;
  base_family: Family | null; quote_family: Family | null;
  base_template_hash: Hex; quote_template_hash: Hex;
  base_scale: number; quote_scale: number;
  /** `ask` sells A, as a sell-first entry does. `bid` buys A, as a buy-first entry does. */
  side: 'ask' | 'bid';
  /**
   * The price of a KobPair (a decay's start), the entry limit of a KobIfdPair, or the take-profit
   * or limit leg of a KobCondPair (null without one).
   */
  price: string | null; price_num: string | null; price_den: string | null;
  /** The stop of a KobCondPair or the entry stop of a KobIfdPair. Absent otherwise. */
  stop_price?: string | null;
  /** The auction price now, else `price`. */
  quote_now: string | null;
  /** Base units of A still to trade. Null once closed. */
  amount_left: string | null;
  /** The B prefund per whole A of a sell-first entry. */
  prefund?: string | null;
  /** KAS on each token delivery. */
  delivery_carrier: string;
  custodies: PairCustodyView[];
}

/** `detail.pair` of a pair fill event. No price is ever recorded from it (`price_source` none). */
export interface PairEventDetail {
  side: 'ask' | 'bid'; base: Hex; quote: Hex; a_scale: number;
  amount_a: string; amount_b: string | null;
  price: string | null; price_num: string | null; price_den: string | null;
  tip_kas: string | null; counterparty: PairCounterparty | string; price_source: string;
}
/**
 * `detail.evidence` of a pair conditional's arm, trail or triggered fill. Mode 0 is two KAS-book
 * fills with quotes `a` and `b`. Mode 1 is a resting KobPair at `price`.
 */
export interface PairEvidenceDetail { mode: 0 | 1 | number; inputs?: number[]; orders?: (Hex | null)[]; a?: string; b?: string; price?: string }

/** `GET /v1/books/{token}`: levels or single orders of one token's book. */
export interface BookView {
  token: Hex;
  aggregated: boolean;
  node_daa: number | null;
  /** Ascending by price. */
  asks: (LevelView | BookOrderView)[];
  /** Descending by price. */
  bids: (LevelView | BookOrderView)[];
}

export type OrderStatus = 'open' | 'partial' | 'filled' | 'cancelled' | 'refunded' | 'killed' | 'closed' | 'unknown';

export interface OutpointView { txid: Hex; index: number; value: string | null }

/** A token UTXO as the indexer serves it. */
export interface TokenUtxoView {
  txid: Hex; index: number; token: Hex; owner: Hex;
  /** Token base units. */
  amount: string;
  /** KAS carrier, in sompi. */
  value: string;
  /**
   * One of:
   * - `owned`: held by a user key (KCC-20 scheme 0, or KRON address presence),
   * - `custody`: an order's exact custody,
   * - `stray`: sent to an order id from outside.
   */
  role: string;
  created_daa: number; spent: boolean; spent_txid: Hex | null;
  /**
   * The decoded token state of either family, which spending the UTXO needs. The app can also
   * rebuild custody states from the owner, the amount and the extension commitment.
   */
  state?: TokenState;
  /** Token program family of the UTXO's token. */
  family?: Family;
  /**
   * The kob-wasm token program name (`KCC20Ref_8x8`, `KronToken2433`, ...) the indexer proved
   * `state` under. Stray views carry it too, because moving a foreign stray needs it.
   * `template_hash` is served by `GET /v1/token-utxos` only.
   */
  program?: string;
  template_hash?: Hex;
  /**
   * A foreign stray (matcher.md 1.2), of a token other than the order's own. For a pair order it
   * is neither of its two tokens.
   */
  foreign?: boolean;
  /** `owner_scheme` (KCC-20) or `id_type` (KRON) of the state. */
  owner_kind?: number;
  /** The state span as hex. Served by `GET /v1/token-utxos` only. */
  state_hex?: Hex;
  confirmations: number | null; settled: boolean;
}

/** The auction of an order with a moving price. */
export interface AuctionView {
  /**
   * `pair_*` kinds are a pair order's auctions: a decaying ask, a rising bid, a triggered stop or
   * a stop entry. Their prices are B base units per whole A, not KAS prices.
   */
  kind: 'decay' | 'rise' | 'stop' | 'entry' | 'pair_decay' | 'pair_rise' | 'pair_stop' | 'pair_entry';
  origin_daa: number; start_price: string; end_price: string; current_price: string; elapsed_daa: number; complete: boolean;
}

/** The repeat link of an entry or exit order. */
export interface RepeatView {
  role: 'entry' | 'exit';
  /**
   * For an entry, `rptAmount` and the base units it can still re-arm. `rptAmount` is 1 + the base
   * units of re-arms left, and 0 means no repeat.
   */
  rpt_amount?: string | null; rearm_amount?: string | null;
  parent?: Hex | null; rpt_until?: string | null;
  /** For an exit, the entry's rate returned on a merge, in sompi per whole token. */
  rpt_price?: string | null;
}

/** An order's custody and whether it holds the expected amount. */
export interface CustodyView { expected_amount: string | null; utxo: TokenUtxoView | null; ok: boolean }

/** `GET /v1/orders/{id}` and the items of `GET /v1/orders`. */
export interface OrderView {
  covenant_id: Hex;
  /**
   * KobAsk, KobBid, KobCondAsk, KobCondBid, KobIfdBid or KobIfdAsk (KRON: ...Kron), or KobPair,
   * KobCondPair or KobIfdPair.
   */
  contract: string;
  template_hash: Hex;
  family: number;
  /** 1 sells the token, 2 buys the token. */
  side: 1 | 2;
  maker: Hex | null;
  token: Hex | null;
  token_template_hash: Hex | null;
  extension_commitment?: Hex | null;
  /** Base units per whole token, the price denominator. */
  scale: number | null;
  /** Smallest fill, in base units. */
  min_fill: string | null;
  /** Sompi per whole token. */
  price: string | null; tip: string | null; tif: number | null;
  expiry_daa: number | null; active_from: number | null;
  /**
   * For bids, the budget rate `pMax + tip` (sompi per whole token) at which a fill consumes the
   * escrow.
   */
  in_book: boolean; budget_rate: string | null; reserve: string | null;
  /** Base units at placement. For a bid it is its buying power then. */
  initial_amount: string | null;
  listed: boolean; unlisted_reason: string | null;
  origin: string;
  parent: Hex | null;
  genesis: { txid: Hex | null; out: number | null; block_seq: number; daa: number; confirmations: number | null; settled: boolean };
  status: OrderStatus;
  /** Base units filled so far. */
  filled_amount: string;
  /**
   * Base units left. For a bid it is its buying power, and `amount_estimated` is set. Null once
   * closed.
   */
  amount_left: string | null;
  amount_estimated: boolean;
  /** The current outpoint of the covenant UTXO, which a cancel spends. */
  current: OutpointView | null;
  state_known: boolean;
  /** The decoded state of the current UTXO, if proven, as `{kind, state}` with 64-bit strings. */
  state?: AnyState | null;
  /**
   * The placement state of an order without a current state, because it is closed or not proven.
   * It is for showing its type and terms only.
   */
  placed_state?: AnyState | null;
  current_daa?: number | null;
  deadline?: number | null;
  deadline_passed?: boolean;
  refund_due_daa?: number | null;
  kill_daa?: number | null;
  auction?: AuctionView | null;
  quote?: string | null;
  repeat?: RepeatView | null;
  /**
   * The sell custody of an ask. For a non-pair bid holding bought tokens it is its bought-custody,
   * the same view as `bought_custody`.
   */
  custody?: CustodyView;
  /**
   * The delivery of a delivering kind: `wallet` (mode 0), `order` (mode 1) or `topup` (mode 2).
   * Older executors omit it.
   */
  delivery?: 'wallet' | 'order' | 'topup';
  delivery_mode?: number;
  /** Mode 1: the base units in the bought-custody now, "0" after a claim. */
  bought?: string;
  topup_tip?: string;
  recv_threshold?: string;
  /** Mode 1: the bought-custody, checked against `bought`. */
  bought_custody?: CustodyView;
  strays?: TokenUtxoView[];
  /** Present only when the order has more live strays than `strays` lists: how many (up to 10,000). */
  strays_total?: number;
  /**
   * The web's own marker (never kobd's): the view has no stray list at all, as the standalone
   * stream book's (R9B-L3). A cancel then states that strays could not be checked.
   */
  strays_unread?: boolean;
  last_block: number; last_daa: number; confirmations: number | null; settled: boolean;
  expired: boolean;
  children?: Hex[];
  /**
   * The pair, side, prices (B base units per whole A) and custodies of a pair order (`KobPair`,
   * `KobCondPair`, `KobIfdPair`). Such an order has a null `price` and `quote`, and its tip is KAS.
   * `orders?token=` lists a pair order under both its tokens.
   */
  pair?: PairOrderView | null;
  /** The levels of a `KobCurveAsk` (docs/design/curve-ask-spec.md) from the current state. */
  curve?: CurveView;
  /**
   * Set when the executor's pre-simulation of the order's next fill failed. The token program may
   * have frozen or blacklisted the order or its owner. Such an order is not in the book depth.
   * Older executors omit it.
   */
  possibly_frozen?: boolean;
  /**
   * Warnings of the order's tokens, such as `genesis_unverified` (matcher.md 1.2a), as on
   * `/v1/tokens`. Older executors omit it.
   */
  warnings?: string[];
  /**
   * Set when the order is under a retired template of the cancel-only table, named by
   * `template_hash`. Only its maker's cancel is built. Only the single-order view serves it.
   * Older executors answer 404 for such an order.
   */
  cancel_only?: boolean;
}

/** One event of `GET /v1/orders/{id}/events` and `GET /v1/fills`. */
export interface EventView {
  id: number; covenant_id: Hex; block_seq: number; daa: number; ts: number; txid: Hex; tx_pos: number;
  /** fill, cancel, refund, genesis, arm, trail, ... */
  kind: string;
  /** `amount` is the base units filled by a fill event. */
  token: Hex | null; side: number | null; amount: string | null; price: string | null; payout: string | null; closes: boolean;
  detail: unknown; confirmations: number | null; settled: boolean;
}

/** One item of `GET /v1/strays`. */
export interface StrayView extends TokenUtxoView { order_status: string | null; maker: Hex | null; lost: boolean }

/** WebSocket frames: `{channel, type, data}` feed messages and `{type, data}` control replies. */
export type WsChannel = 'health' | 'reorg' | 'fills' | `fills:${string}` | `book:${string}` | `order:${string}`;
export interface WsFrame { channel?: string; type: string; data?: unknown }

// --- Additions of the data layer ---

/** `/v1/token-events` item: first sighting of a token identity. */
export interface TokenEventView {
  token: Hex;
  template_hash: Hex | null;
  extension_commitment: Hex | null;
  kind: string;
  txid: Hex;
  daa: number;
}

/** Body of every non-2xx answer: `{"error":{"code":..,"message":..}}`. */
export interface ApiErrorBody { error: { code: string; message: string } }

/**
 * `fill` frame data of the WebSocket feed, the indexer's `FillNotice`. `price`, `amount` and
 * `payout` are decimal strings as in REST, because a 64-bit value above 2^53 is not a safe JSON
 * number.
 */
export interface FillNoticeView {
  order: Hex;
  token: Hex | null;
  /** 1 when an ask (sell) was filled, 2 when a bid (buy) was filled. */
  side: number;
  price: string | null;
  /** Base units filled. */
  amount: string;
  payout: string | null;
  txid: Hex;
  block: Hex;
  daa: number;
}

/** `book:<token>` and `order:<id>` notice frames. Clients refetch the REST resource. */
export interface BookNotice { token: Hex; cursor_daa: number }
export interface OrderNotice { covenant_id: Hex; cursor_daa: number }
/** `health` channel: a `cursor` frame per committed batch, a `health` frame per keepalive tick. */
export interface HealthCursorNotice { cursor_hash: Hex | null; cursor_daa: number; added_blocks: number; reverted_blocks: number }
export interface HealthTickNotice { state: string; cursor_daa: number; node_daa: number | null; lag_daa: number | null }
export interface ReorgNotice { reverted_blocks: number; added_blocks: number; cursor_daa: number }

// --- Market data: trades, candles, stats, depth ---
//
// Every price below is sompi per `price_basis` token base units. `price_basis` is the token's
// standard scale `10^decimals`, so sompi per whole token, when the registry knows its decimals.
// Otherwise it is the scale of the token's first order.
// KAS per whole token = price / 1e8 * 10^decimals / price_basis.
// Amounts are token base units and quotes are sompi, all as decimal strings. Timestamps are unix
// ms. Every view also carries `decimals`, null without a registry entry.

/**
 * One trade, made of every fill event of one transaction on one token. It is priced at the
 * resting side's VWAP, and `side` is the aggressor's.
 */
export interface TradeView {
  id: number;
  txid: Hex;
  ts: number;
  daa: number;
  price: string;
  amount: string;
  quote: string;
  side: 'buy' | 'sell';
  fills: number;
  confirmations: number | null;
  settled: boolean;
}

/** `GET /v1/trades/{token}?limit=&before=`, newest first. */
export interface TradesView {
  token: Hex;
  price_basis: string;
  decimals?: number | null;
  items: TradeView[];
  next_cursor: string | null;
}

export type CandleInterval = '1m' | '5m' | '1h' | '1d';

/** One candle of `GET /v1/candles/{token}`. */
export interface CandleView {
  /** Bucket start, unix ms (UTC). */
  t: number;
  o: string;
  h: string;
  l: string;
  c: string;
  volume: string;
  quote_volume: string;
  trades: number;
}

/**
 * `GET /v1/quotes/{token}`: the top of the book per bucket with a change, ascending. Each item is
 * `[t, bid, ask]` in sompi per `price_basis`, with null for an empty side.
 */
export interface QuotesView {
  token: Hex;
  interval: CandleInterval;
  price_basis: string;
  decimals?: number | null;
  items: [number, string | null, string | null][];
}

/**
 * `GET /v1/candles/{token}?interval=&from=&to=&limit=`, ascending. Buckets without trades are
 * omitted.
 */
export interface CandlesView {
  token: Hex;
  interval: CandleInterval;
  price_basis: string;
  decimals?: number | null;
  items: CandleView[];
}

/**
 * `GET /v1/stats?since=`: the store's activity totals from `since` to the newest block
 * (docs/api-reference.md 3.21).
 *
 * `since` is unix ms, rounded down to the minute by the server, or null for all time. An indexer
 * of schema 10 answers every figure. Its `executors` are the distinct scripts that funded the
 * range's fills, those it recorded. An earlier one has `traders` and `fees_paid` null beyond its
 * scan caps, and `executors` always null. `kas_locked` (sompi) is the live order UTXOs now,
 * whatever the range.
 *
 * The executors' side of the range is null on an older indexer that lacks these fields.
 * - `executor_income` is what the KOB transactions left to the executors that sent them, before
 *   network fees. It covers crossing spread, auction margins and keeper tips. It sums the
 *   transactions whose income is known (`executor_income_txs`). It leaves out the
 *   `executor_income_unknown_txs` and the `executor_income_own_trade_txs`, in which an executor
 *   traded for itself as an inventory buy or sale or as a taker. It can be negative.
 * - `tips_paid` is the priority-tip sum. `fees_paid` is the same value, deprecated.
 * - `miner_fees` are the network fees of those transactions, over the ones whose fee is known
 *   (`miner_fee_txs`, `miner_fee_unknown_txs`).
 * - `executor_net` is income minus fee over the transactions where both are known
 *   (`executor_net_txs`). It can be negative, and the page does not show it.
 */
export interface ActivityStatsView {
  since: number | null;
  ts: number | null;
  orders: number;
  traders: number | null;
  executors: number | null;
  /** Deprecated. The tip sum, the same as `tips_paid`. */
  fees_paid: string | null;
  tips_paid: string | null;
  /** Sompi, signed decimal. */
  executor_income: string | null;
  executor_income_txs: number | null;
  executor_income_unknown_txs: number | null;
  /**
   * Executor transactions in which the sender traded for itself, not counted in
   * `executor_income`. Null on an older indexer.
   */
  executor_income_own_trade_txs: number | null;
  miner_fees: string | null;
  miner_fee_txs: number | null;
  miner_fee_unknown_txs: number | null;
  /** Sompi, signed decimal. */
  executor_net: string | null;
  executor_net_txs: number | null;
  kas_locked: string;
}

/**
 * `GET /v1/stats/{token}`: rolling 24 h relative to the newest chain block time, plus the book
 * top. Missing values are null.
 */
export interface StatsView {
  token: Hex;
  price_basis: string;
  decimals?: number | null;
  ts: number;
  last: string | null;
  last_ts: number | null;
  last_side: 'buy' | 'sell' | null;
  open_24h: string | null;
  high_24h: string | null;
  low_24h: string | null;
  change_24h_bps: number | null;
  volume_24h: string | null;
  quote_volume_24h: string | null;
  trades_24h: number | null;
  best_bid: string | null;
  best_ask: string | null;
  mid: string | null;
  spread_bps: number | null;
  open_asks: number | null;
  open_bids: number | null;
  /**
   * Sompi worth of the resting asks up to 2 % above the mid and the resting bids down to 2 %
   * below it. With one side only, it is measured around that side's best price. Null for an empty
   * book. Older executors omit it.
   */
  depth_2pct_quote?: string | null;
  /**
   * The two parts of `depth_2pct_quote` in sompi: the bids down to 2 % below the reference price
   * and the asks up to 2 % above it, both bounds inclusive. "0" for a missing side or an empty
   * book. Older executors omit them, and then there is no thin-book marker.
   */
  depth_2pct_bid_sompi?: string | null;
  depth_2pct_ask_sompi?: string | null;
}

/** One level of `GET /v1/depth/{token}`. */
export interface DepthLevelView {
  price: string;
  amount: string;
  orders: number;
  cum_amount: string;
  cum_quote: string;
  /** When set, bid amounts are upper bounds, because a bid's amount is its buying power. */
  estimated: boolean;
}

/**
 * `GET /v1/depth/{token}?levels=`: the listed book merged by per-basis price, best first, with
 * cumulative sums.
 */
export interface DepthView {
  token: Hex;
  price_basis: string;
  decimals?: number | null;
  ts: number;
  daa: number;
  bids: DepthLevelView[];
  asks: DepthLevelView[];
}
