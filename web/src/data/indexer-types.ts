// Response shapes of the KOB indexer read API (`/v1`, crates/kob-executor, docs/ops/indexer.md on branch m3/indexer).
//
// Conventions of the API: JSON, snake_case keys, ids/hashes/keys lower-case hex, every sompi and token amount a DECIMAL STRING, DAA scores /
// counts / scales plain numbers. Protocol v3 (docs/ops/executor.md 5.0): quantities are token base units, prices and tips sompi per whole token
// (`scale` base units of the order's token). Every chain-derived view carries `confirmations` and `settled` (>= settle_depth_daa). The API is being extended
// on branch m3/indexer (state, custody, strays, auction, repeat, refund_due_daa ...): every field added after the M3 part-1 commit is
// optional here, and the client tolerates its absence. Fields marked PROPOSED are requirements the web app places on the indexer.
import type { AnyState, Family, Hex, TokenState } from '../kob/types';

export interface Page<T> { items: T[]; next_cursor: string | null }

export interface HealthView {
  state: 'starting' | 'following' | 'catching_up' | 'node_unavailable' | 'gap' | string;
  ok: boolean;
  network: string;
  node_version?: string | null;
  cursor_hash?: Hex | null;
  cursor_daa: number;
  /** node virtual DAA score at the last poll */
  node_daa: number | null;
  lag_daa: number | null;
  lag_seconds: number | null;
  settle_depth_daa: number;
  last_error?: string | null;
  alarms?: unknown[];
}

/** How much KOB vouches for a token: `official` = confirmed genuine; `unverified` = tradable token of an audited program template that KOB has not confirmed (tickers collide: identify it by covenant id); `delisted`. */
export type TokenStanding = 'official' | 'unverified' | 'delisted';
/** Capabilities of a token's program template that can act on holders or supply. `freeze` and `seize` put escrowed orders at the issuer's discretion. */
export type TokenPower = 'mint-authority' | 'public-mint' | 'burn' | 'freeze' | 'seize' | 'blacklist';

export interface IndexerTokenView {
  /** the registry ticker; an EMPTY string for a token that is not in the registry (show the short covenant id instead) */
  ticker: string;
  covenant_id: Hex;
  template_hash: Hex | null;
  extension_commitment: Hex | null;
  /** the token's standard scale (`10^decimals`, at most 10^9): an order is listed only when its `scale` equals it; null without decimals */
  scale?: number | null;
  decimals: number | null;
  open_asks: number;
  open_bids: number;
  /** `GET /v1/tokens` of an executor with the registry; absent on older ones (treated as `unverified`) */
  standing?: TokenStanding;
  /** template capabilities (empty for most tokens); unknown values are dropped by the client */
  powers?: string[];
  /** the audited program template of the token, null when unknown */
  template_id?: string | null;
  /** `kcc20` | `kron` */
  family?: 'kcc20' | 'kron';
}

/**
 * An aggregated level: orders with the same state `price` AND `scale` (`price` is sompi per `scale` base units; `amount` base units summed,
 * `amount_estimated` when it holds a bid, whose amount is its buying power).
 */
export interface LevelView { price: string; amount: string; amount_estimated: boolean; orders: number; scale: number }

export interface BookOrderView {
  covenant_id: Hex; contract: string; maker: Hex | null; price: string;
  /** priority tip, sompi per whole token */
  tip: string | null;
  /** smallest fill (base units) */
  min_fill: string | null;
  scale: number | null;
  /** base units left (a bid: its buying power, `amount_estimated`) */
  amount_left: string | null;
  amount_estimated: boolean;
  status: string; cur_value: string | null; expiry_daa: number | null; expired: boolean; genesis_daa: number;
  confirmations: number | null; settled: boolean;
  /** the current price of an auction (decay / rise) at the node's DAA score, else `price`; absent on older executors */
  quote?: string | null;
  /** see `OrderView.possibly_frozen` (the server omits such orders from the book, so this is normally false / absent) */
  possibly_frozen?: boolean;
}

/**
 * Where a level of a pair book comes from (docs/ops/executor.md 5.4): `direct` = resting `KobPair` orders, `entry` = `KobIfdPair` entries resting at
 * their limit (a stop entry once armed), `route` = quotes implied through the two KAS books (indicative: no minimum fills, rounding or fees).
 */
export type PairLevelSource = 'direct' | 'entry' | 'route';
/**
 * One level of `GET /v1/pairs/{base}/{quote}/book`: price = price_num / price_den QUOTE base units per BASE base unit (exact rational),
 * `amount` in BASE base units (decimal strings).
 */
export interface PairLevelView { source: PairLevelSource; price_num: string; price_den: string; amount: string; orders: number }
/** `GET /v1/pairs/{base}/{quote}/book`: asks = buy BASE with QUOTE (ascending), bids = sell BASE for QUOTE (descending). */
export interface PairBookView { base: Hex; quote: Hex; daa_score: number; asks: PairLevelView[]; bids: PairLevelView[] }
/**
 * One entry of `GET /v1/pairs?token=` (an oriented pair with at least one listed live pair order): live `KobPair` asks / bids, sell-first / buy-first
 * `KobIfdPair` entries, live `KobCondPair` orders (stops, take-profits, OCO, exits).
 */
export interface PairSummaryView { base: Hex; quote: Hex; direct_asks: number; direct_bids: number; entry_asks: number; entry_bids: number; conditionals: number }

/** An exact rate of a pair candle: `value` = floor(num / den) B base units per `price_basis` base units of A. */
export interface PairRateView { value: string; num: string; den: string }
/** One pair candle derived from the two KAS series (never from pair fills): o = open(A)/open(B), c = close(A)/close(B), h = high(A)/low(B), l = low(A)/high(B). */
export interface PairCandleView {
  /** bucket start, unix ms (UTC) */
  t: number;
  o: PairRateView; h: PairRateView; l: PairRateView; c: PairRateView;
  /** whether A / B traded in KAS in the bucket (false: that side carries its last close forward) */
  a_traded: boolean; b_traded: boolean;
  /** pair volume of the bucket (pair-order fills): base units of A and of B, and their count */
  pair_volume_a: string; pair_volume_b: string; pair_fills: number;
}
/** `GET /v1/pairs/{base}/{quote}/candles`: rates B base units per `price_basis` base units of A (A's KAS price basis). */
export interface PairCandlesView {
  base: Hex; quote: Hex; interval: CandleInterval;
  price_basis: string; quote_price_basis: string;
  decimals: number | null; quote_decimals: number | null;
  /** always `kas_books` */
  price_source: string;
  items: PairCandleView[];
}
/** How a pair fill was filled: `route` (the tx also filled KAS-book orders of A or B), `netting` (an opposite pair order), `inventory` (the filler's tokens). */
export type PairCounterparty = 'route' | 'netting' | 'inventory';
/** One pair-order fill (volume only: `price_source` is always `none`, a pair fill never sets a price). */
export interface PairFillView {
  id: number; txid: Hex; ts: number; daa: number;
  /** the filled pair order and its kind */
  order: Hex; contract: string;
  /** `ask` (the order sold A) or `bid` (it bought A) */
  side: 'ask' | 'bid';
  /** base units of A filled; base units of B the maker received (ask) or paid (bid) */
  amount_a: string; amount_b: string | null;
  /** the order's quote at the fill (B per whole A, `a_scale` base units) and per A base unit */
  price: string | null; price_num: string | null; price_den: string | null; a_scale: number;
  tip_kas: string | null;
  counterparty: PairCounterparty | string;
  price_source: string;
  confirmations: number | null; settled: boolean;
}
/** `GET /v1/pairs/{base}/{quote}/fills`: newest first, the 24 h pair volume. */
export interface PairFillsView {
  base: Hex; quote: Hex;
  volume_24h: { amount_a: string; amount_b: string; fills: number };
  items: PairFillView[];
  next_cursor: string | null;
}

/** A custody of a pair order (`OrderView.pair.custodies`, record order: a sell-first entry's A, then its B prefund). */
export interface PairCustodyView {
  token: Hex;
  /** `base` (A) or `quote` (B) */
  role: 'base' | 'quote' | string;
  expected_amount: string;
  /** single-order lookups and the live orders of a `maker=` list */
  utxo?: TokenUtxoView | null;
  ok?: boolean;
}
/** The `pair` object of a pair order's `GET /v1/orders/{id}` view (prices B base units per whole A). */
export interface PairOrderView {
  base: Hex; quote: Hex;
  base_family: Family | null; quote_family: Family | null;
  base_template_hash: Hex; quote_template_hash: Hex;
  base_scale: number; quote_scale: number;
  /** `ask` (sells A; a sell-first entry) or `bid` (buys A; a buy-first entry) */
  side: 'ask' | 'bid';
  /** KobPair: its price (a decay's start); KobIfdPair: the entry limit; KobCondPair: the take-profit / limit leg (null without one) */
  price: string | null; price_num: string | null; price_den: string | null;
  /** KobCondPair: the stop; KobIfdPair: the entry stop (absent otherwise) */
  stop_price?: string | null;
  /** the auction price now, else `price` */
  quote_now: string | null;
  /** base units of A still to trade (null once closed) */
  amount_left: string | null;
  /** sell-first entry: the B prefund per whole A */
  prefund?: string | null;
  /** KAS on each token delivery */
  delivery_carrier: string;
  custodies: PairCustodyView[];
}

/** `detail.pair` of a pair fill event (no price is ever recorded from it: `price_source` none). */
export interface PairEventDetail {
  side: 'ask' | 'bid'; base: Hex; quote: Hex; a_scale: number;
  amount_a: string; amount_b: string | null;
  price: string | null; price_num: string | null; price_den: string | null;
  tip_kas: string | null; counterparty: PairCounterparty | string; price_source: string;
}
/** `detail.evidence` of a pair conditional's arm / trail / triggered fill: mode 0 two KAS-book fills (quotes `a`, `b`), mode 1 a resting KobPair (`price`). */
export interface PairEvidenceDetail { mode: 0 | 1 | number; inputs?: number[]; orders?: (Hex | null)[]; a?: string; b?: string; price?: string }

export interface BookView {
  token: Hex;
  aggregated: boolean;
  node_daa: number | null;
  /** ascending by price */
  asks: (LevelView | BookOrderView)[];
  /** descending by price */
  bids: (LevelView | BookOrderView)[];
}

export type OrderStatus = 'open' | 'partial' | 'filled' | 'cancelled' | 'refunded' | 'killed' | 'closed' | 'unknown';

export interface OutpointView { txid: Hex; index: number; value: string | null }

export interface TokenUtxoView {
  txid: Hex; index: number; token: Hex; owner: Hex;
  /** token base units */
  amount: string;
  /** KAS carrier (sompi) */
  value: string;
  /** `owned` (held by a user key: KCC-20 scheme 0, KRON address presence) | `custody` (an order's exact custody) | `stray` (sent to an order id from outside) */
  role: string;
  created_daa: number; spent: boolean; spent_txid: Hex | null;
  /** the decoded token state of either family (needed to spend the UTXO); the app can also rebuild custody states from owner + amount (+ extension commitment) */
  state?: TokenState;
  /** token program family of the UTXO's token */
  family?: Family;
  /**
   * kob-wasm token program name (`KCC20Ref_8x8`, `KronToken2433`, ...): the program the indexer proved `state` under (also on stray views, where it
   * is what moving a FOREIGN stray needs); `template_hash`: `GET /v1/token-utxos` only
   */
  program?: string;
  template_hash?: Hex;
  /** a stray of a token other than the order's own (a pair order: neither of its two): a FOREIGN stray (matcher.md 1.2) */
  foreign?: boolean;
  /** `owner_scheme` (KCC-20) or `id_type` (KRON) of the state */
  owner_kind?: number;
  /** the state span, hex: `GET /v1/token-utxos` only */
  state_hex?: Hex;
  confirmations: number | null; settled: boolean;
}

export interface AuctionView {
  /** `pair_*`: a pair order's auction (decaying ask, rising bid, triggered stop, stop entry); its prices are B base units per whole A, not KAS prices */
  kind: 'decay' | 'rise' | 'stop' | 'entry' | 'pair_decay' | 'pair_rise' | 'pair_stop' | 'pair_entry';
  origin_daa: number; start_price: string; end_price: string; current_price: string; elapsed_daa: number; complete: boolean;
}

export interface RepeatView {
  role: 'entry' | 'exit';
  /** entry: `rptAmount` (1 + base units of re-arms left; 0 = no repeat) and the base units it can still re-arm */
  rpt_amount?: string | null; rearm_amount?: string | null;
  parent?: Hex | null; rpt_until?: string | null;
  /** exit: the entry's rate returned on a merge, sompi per whole token */
  rpt_price?: string | null;
}

export interface CustodyView { expected_amount: string | null; utxo: TokenUtxoView | null; ok: boolean }

export interface OrderView {
  covenant_id: Hex;
  /** KobAsk | KobBid | KobCondAsk | KobCondBid | KobIfdBid | KobIfdAsk (KRON: ...Kron) | KobPair | KobCondPair | KobIfdPair */
  contract: string;
  template_hash: Hex;
  family: number;
  /** 1 sells the token, 2 buys the token */
  side: 1 | 2;
  maker: Hex | null;
  token: Hex | null;
  token_template_hash: Hex | null;
  extension_commitment?: Hex | null;
  /** base units per whole token (the price denominator) */
  scale: number | null;
  /** smallest fill, base units */
  min_fill: string | null;
  /** sompi per whole token */
  price: string | null; tip: string | null; tif: number | null;
  expiry_daa: number | null; active_from: number | null;
  /** bids: the budget rate `pMax + tip` (sompi per whole token) at which a fill consumes the escrow */
  in_book: boolean; budget_rate: string | null; reserve: string | null;
  /** base units at placement (a bid: its buying power then) */
  initial_amount: string | null;
  listed: boolean; unlisted_reason: string | null;
  origin: string;
  parent: Hex | null;
  genesis: { txid: Hex | null; out: number | null; block_seq: number; daa: number; confirmations: number | null; settled: boolean };
  status: OrderStatus;
  /** base units filled so far */
  filled_amount: string;
  /** base units left (a bid: its buying power, `amount_estimated`); null once closed */
  amount_left: string | null;
  amount_estimated: boolean;
  /** current outpoint of the covenant UTXO (what a cancel spends) */
  current: OutpointView | null;
  state_known: boolean;
  /** decoded state of the CURRENT utxo (`{kind, state}`, 64-bit fields as strings) when proven */
  state?: AnyState | null;
  current_daa?: number | null;
  deadline?: number | null;
  deadline_passed?: boolean;
  refund_due_daa?: number | null;
  kill_daa?: number | null;
  auction?: AuctionView | null;
  quote?: string | null;
  repeat?: RepeatView | null;
  custody?: CustodyView;
  strays?: TokenUtxoView[];
  last_block: number; last_daa: number; confirmations: number | null; settled: boolean;
  expired: boolean;
  children?: Hex[];
  /**
   * Pair orders (`KobPair`, `KobCondPair`, `KobIfdPair`) only: the pair, side, prices (B base units per whole A) and custodies (their `price` / `quote`
   * are null; the tip is KAS). `orders?token=` lists a pair order under both its tokens.
   */
  pair?: PairOrderView | null;
  /** the executor's pre-simulation of the order's next fill failed (the token program may have frozen / blacklisted it or its owner): the order is NOT in the book depth; absent on older executors */
  possibly_frozen?: boolean;
}

export interface EventView {
  id: number; covenant_id: Hex; block_seq: number; daa: number; ts: number; txid: Hex; tx_pos: number;
  /** fill | cancel | refund | genesis | arm | trail | ... */
  kind: string;
  /** `amount`: base units filled (a fill event) */
  token: Hex | null; side: number | null; amount: string | null; price: string | null; payout: string | null; closes: boolean;
  detail: unknown; confirmations: number | null; settled: boolean;
}

export interface StrayView extends TokenUtxoView { order_status: string | null; maker: Hex | null; lost: boolean }

/** WebSocket frames: `{channel, type, data}` feed messages and `{type, data}` control replies. */
export type WsChannel = 'health' | 'reorg' | 'fills' | `fills:${string}` | `book:${string}` | `order:${string}`;
export interface WsFrame { channel?: string; type: string; data?: unknown }

// ------------------------------------------------------------------------------------------------ additions (data layer)

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

/** `fill` frame data of the WebSocket feed (`FillNotice` of the indexer). Amounts are JSON numbers here (i64 on the wire). */
export interface FillNoticeView {
  order: Hex;
  token: Hex | null;
  /** 1: an ask (sell) was filled, 2: a bid (buy) was filled */
  side: number;
  price: number | null;
  /** base units filled */
  amount: number;
  payout: number | null;
  txid: Hex;
  block: Hex;
  daa: number;
}

/** `book:<token>` / `order:<id>` notice frames: clients refetch the REST resource. */
export interface BookNotice { token: Hex; cursor_daa: number }
export interface OrderNotice { covenant_id: Hex; cursor_daa: number }
/** `health` channel: `cursor` frames per committed batch, `health` frames on the server's keepalive tick. */
export interface HealthCursorNotice { cursor_hash: Hex | null; cursor_daa: number; added_blocks: number; reverted_blocks: number }
export interface HealthTickNotice { state: string; cursor_daa: number; node_daa: number | null; lag_daa: number | null }
export interface ReorgNotice { reverted_blocks: number; added_blocks: number; cursor_daa: number }

// ------------------------------------------------------------------------------------------------ market data (M5: trades, candles, stats, depth)
//
// Every price below is sompi per `price_basis` token base units (`price_basis` = the token's standard scale `10^decimals`, i.e. sompi per whole
// token, when the registry knows its decimals; else the scale of the token's first order). KAS per whole token = price / 1e8 * 10^decimals /
// price_basis. Amounts are token base units, quotes sompi, all as decimal strings; timestamps unix ms. Every view also carries `decimals` (null
// without a registry entry).

/** One trade = every fill event of one transaction on one token; priced at the resting side's VWAP, `side` is the AGGRESSOR's. */
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

/** `GET /v1/trades/{token}?limit=&before=` (newest first). */
export interface TradesView {
  token: Hex;
  price_basis: string;
  decimals?: number | null;
  items: TradeView[];
  next_cursor: string | null;
}

export type CandleInterval = '1m' | '5m' | '1h' | '1d';

export interface CandleView {
  /** bucket start, unix ms (UTC) */
  t: number;
  o: string;
  h: string;
  l: string;
  c: string;
  volume: string;
  quote_volume: string;
  trades: number;
}

/** `GET /v1/candles/{token}?interval=&from=&to=&limit=` (ascending; buckets without trades are omitted). */
export interface CandlesView {
  token: Hex;
  interval: CandleInterval;
  price_basis: string;
  decimals?: number | null;
  items: CandleView[];
}

/** `GET /v1/stats/{token}`: rolling 24 h relative to the newest chain block time, plus the book top. Missing values are null. */
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
}

export interface DepthLevelView {
  price: string;
  amount: string;
  orders: number;
  cum_amount: string;
  cum_quote: string;
  /** bid amounts are upper bounds when set (a bid's amount is its buying power) */
  estimated: boolean;
}

/** `GET /v1/depth/{token}?levels=`: the listed book merged by per-basis price, best first, with cumulative sums. */
export interface DepthView {
  token: Hex;
  price_basis: string;
  decimals?: number | null;
  ts: number;
  daa: number;
  bids: DepthLevelView[];
  asks: DepthLevelView[];
}
