// Intents of the plain order kinds (docs/spec/order-types.md): limit (GTC / GTD / day / timed activation), IOC, FOK, market, streaming,
// close, TWAP, DCA, Dutch decay / rising bid. The UI builds these from form state; `planSimple` (orders/simple.ts) turns them into an
// OrderPlan. All amounts are bigint: quantities in token BASE UNITS (any positive amount), prices and tips in SOMPI PER WHOLE TOKEN (the
// state price: sompi per `scale` base units, see TokenMarket.scale; units.ts converts from the KAS-per-token text); wall-clock times are
// UTC unix seconds, protocol times are DAA scores.
import type { Duration } from './daa';

/** When a resting order ends. Default (omitted): GTC. */
export type Lifetime =
  /** good till cancelled: refundable after 90 days idle (renew before day 85) */
  | { kind: 'gtc' }
  /** good till date: refundable from `at` (UTC unix seconds, at most 90 days ahead) */
  | { kind: 'gtd'; at: bigint }
  /** day order: until the next 00:00 UTC (09:00 JST) */
  | { kind: 'day' };

/** Timed activation: no fill before this moment. */
export type Activation = { unixSeconds: bigint } | { daa: bigint };

/** What to do with a limit that already crosses the book at placement (matcher.md 10.5). */
export type CrossingPolicy =
  /** place it as an auction from the touch down/up to the limit, then resting at the limit (default) */
  | 'auction'
  /** place it as a plain limit: it fills at its limit, a matcher keeps the difference (warning) */
  | 'limit'
  /** refuse */
  | 'reject';

interface SimpleBase {
  /** token base units */
  amount: bigint;
  /** optional priority tip, sompi per whole token (default 0): a sell receives limit - tip, a buy pays limit + tip */
  tip?: bigint;
  /**
   * the smallest fill (base units) unless a fill takes everything left (the order's `minFill`). Default: the amount worth 10 KAS at the
   * limit (kob-wasm `defaultMinFill`) for resting orders, 1 for IOC / FOK / market / streaming / close.
   */
  minFill?: bigint;
}

/** Limit GTC / GTD / day, optionally with timed activation. sell -> KobAsk (tokens in custody), buy -> KobBid (KAS budget). */
export interface LimitIntent extends SimpleBase {
  type: 'limit';
  side: 'sell' | 'buy';
  /** sompi per whole token */
  price: bigint;
  lifetime?: Lifetime;
  activeFrom?: Activation;
  crossing?: CrossingPolicy;
  /** buy only: fills the escrow budgets a delivery carrier for (default 3) */
  maxFills?: bigint;
}

/** IOC / FAK (tif 1): fills what crosses now, the rest is returned; default life 300 DAA (max 600). */
export interface IocIntent extends SimpleBase {
  type: 'ioc';
  side: 'sell' | 'buy';
  price: bigint;
  life?: Duration;
  activeFrom?: Activation;
}

/** FOK (tif 2): all or nothing in one transaction; refused when the visible book cannot fill it (matcher.md 10.4). */
export interface FokIntent extends SimpleBase {
  type: 'fok';
  side: 'sell' | 'buy';
  price: bigint;
  life?: Duration;
  activeFrom?: Activation;
}

interface AuctionOptions {
  /** slippage bound in basis points from the reference price (default 300 = 3%) */
  slippageBps?: bigint;
  /** auction length to reach the bound (default 200 DAA = 20 s) */
  auction?: Duration;
  /** DAA between placement and the auction start (default 30) */
  activation?: bigint;
  /** life after activation (default 300 DAA; at most 600) */
  life?: Duration;
  /** FOK instead of IOC: all or nothing */
  allOrNothing?: boolean;
}

/** Market: an IOC auction from the touch (best opposite price) to the slippage bound (matcher.md 10.2). Needs a non-empty opposite book. */
export interface MarketIntent extends SimpleBase, AuctionOptions {
  type: 'market';
  side: 'sell' | 'buy';
}

/** Streaming / quote-and-execute (10.3): as market, but from the price the user saw, with their tolerance. */
export interface StreamingIntent extends SimpleBase, AuctionOptions {
  type: 'streaming';
  side: 'sell' | 'buy';
  /** the displayed price the user accepted, sompi per whole token */
  displayedPrice: bigint;
  /** tolerance in basis points (required: it is what the user agreed to) */
  toleranceBps: bigint;
}

/** Close: a market sell of the held tokens. `amount` omitted = the whole balance. */
export interface CloseIntent extends AuctionOptions {
  type: 'close';
  side?: 'sell';
  amount?: bigint;
  tip?: bigint;
}

interface ScheduleBase extends SimpleBase {
  /** base units per slice (the on-chain `maxFill`) */
  sliceAmount: bigint;
  /** minimum time between fills (the on-chain `interval`) */
  interval: Duration;
  /** price of every slice (start price when `priceEnd` is set), sompi per whole token */
  price: bigint;
  /** optional: each slice is its own auction from `price` to `priceEnd` (sell: lower, buy: higher) */
  priceEnd?: bigint;
  /** length of each slice's auction (default 200 DAA), only with `priceEnd` */
  sliceAuction?: Duration;
  lifetime?: Lifetime;
  activeFrom?: Activation;
}

/** TWAP (sell): at most `sliceAmount` per `interval`. */
export interface TwapIntent extends ScheduleBase {
  type: 'twap';
  side?: 'sell';
}

/** DCA (buy): as TWAP on the bid side; the escrow budgets one delivery carrier per slice unless `maxFills` says more. */
export interface DcaIntent extends ScheduleBase {
  type: 'dca';
  side?: 'buy';
  maxFills?: bigint;
}

/** Dutch / decay (sell) and rising bid (buy): the price moves step by step from `price` to `priceEnd` over `duration`, then rests there. */
export interface DutchIntent extends SimpleBase {
  type: 'dutch';
  side: 'sell' | 'buy';
  /** start price, sompi per whole token */
  price: bigint;
  /** end price: sell = lower than `price` (floor), buy = higher (cap) */
  priceEnd: bigint;
  /** time to move from `price` to `priceEnd` */
  duration: Duration;
  /** DAA per price step (default 1) */
  stepDaa?: bigint;
  lifetime?: Lifetime;
  /** start of the auction (default: placement + 30 DAA) */
  activeFrom?: Activation;
  maxFills?: bigint;
}

export type SimpleIntent = LimitIntent | IocIntent | FokIntent | MarketIntent | StreamingIntent | CloseIntent | TwapIntent | DcaIntent | DutchIntent;

/** Every `type` planSimple handles. */
export const SIMPLE_TYPES: readonly SimpleIntent['type'][] = ['limit', 'ioc', 'fok', 'market', 'streaming', 'close', 'twap', 'dca', 'dutch'];

/** Loose base shape (kept from the stub): what every simple intent has in common. */
export interface SimpleIntentBase { type: string; side?: 'sell' | 'buy'; amount?: bigint }
