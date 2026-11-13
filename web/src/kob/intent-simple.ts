// Intents of the plain order kinds (docs/spec/order-types.md). They are limit (GTC, GTD, day and
// timed activation), IOC, FOK, market, streaming, close, TWAP, DCA, and Dutch decay or rising bid.
// The UI builds them from form state, and `planSimple` (orders/simple.ts) turns them into an
// OrderPlan.
//
// All amounts are bigint. Quantities are token base units, any positive amount. Prices and tips are
// sompi per whole token, which is the state price in sompi per `scale` base units (see
// TokenMarket.scale). units.ts converts from the KAS-per-token text. Wall-clock times are UTC unix
// seconds, and protocol times are DAA scores.
import type { Duration } from './daa';

/** When a resting order ends. An omitted lifetime means GTC. */
export type Lifetime =
  /** Good till cancelled. It is refundable after 90 days idle, so renew before day 85. */
  | { kind: 'gtc' }
  /** Good till date. It is refundable from `at`, in UTC unix seconds and at most 90 days ahead. */
  | { kind: 'gtd'; at: bigint }
  /** A day order, which lasts until the next 00:00 UTC. */
  | { kind: 'day' };

/** Timed activation. No fill happens before this moment. */
export type Activation = { unixSeconds: bigint } | { daa: bigint };

/** What to do with a limit that already crosses the book at placement (matcher.md 10.5). */
export type CrossingPolicy =
  /**
   * Place it as an auction from the touch to the limit, then resting at the limit. This is the
   * default.
   */
  | 'auction'
  /**
   * Place it as a plain limit. It fills at its limit and a matcher keeps the difference, with a
   * warning.
   */
  | 'limit'
  /** Refuse it. */
  | 'reject';

interface SimpleBase {
  /** Token base units. */
  amount: bigint;
  /**
   * Optional priority tip in sompi per whole token, 0 by default. A sell receives limit - tip and a
   * buy pays limit + tip.
   */
  tip?: bigint;
  /**
   * The smallest fill in base units, unless a fill takes everything left. It is the order's
   * `minFill`. The default is the amount worth 10 KAS at the limit (kob-wasm `defaultMinFill`) for
   * resting orders, and 1 for IOC, FOK, market, streaming and close.
   */
  minFill?: bigint;
}

/**
 * Limit GTC, GTD or day, optionally with timed activation. A sell is a KobAsk and a buy is a
 * KobBid.
 */
export interface LimitIntent extends SimpleBase {
  type: 'limit';
  side: 'sell' | 'buy';
  /** Sompi per whole token. */
  price: bigint;
  lifetime?: Lifetime;
  activeFrom?: Activation;
  crossing?: CrossingPolicy;
  /**
   * For a buy only, the number of fills the escrow budgets a delivery carrier for. Defaults to 3.
   */
  maxFills?: bigint;
}

/**
 * IOC or FAK (tif 1). It fills what crosses now and returns the rest. The default life is 300 DAA,
 * at most 600.
 */
export interface IocIntent extends SimpleBase {
  type: 'ioc';
  side: 'sell' | 'buy';
  price: bigint;
  life?: Duration;
  activeFrom?: Activation;
}

/**
 * FOK (tif 2). It is all or nothing in one transaction, and refused when the visible book cannot
 * fill it (matcher.md 10.4).
 */
export interface FokIntent extends SimpleBase {
  type: 'fok';
  side: 'sell' | 'buy';
  price: bigint;
  life?: Duration;
  activeFrom?: Activation;
}

interface AuctionOptions {
  /** The slippage bound in basis points from the reference price. Defaults to 300 (3%). */
  slippageBps?: bigint;
  /** The auction length to reach the bound. Defaults to 200 DAA (20 s). */
  auction?: Duration;
  /** The DAA between placement and the auction start. Defaults to 30. */
  activation?: bigint;
  /** The life after activation. Defaults to 300 DAA, at most 600. */
  life?: Duration;
  /** Makes it FOK instead of IOC, so it is all or nothing. */
  allOrNothing?: boolean;
}

/**
 * Market, an IOC auction from the touch to the slippage bound (matcher.md 10.2).
 * The touch is the best opposite price, so the opposite book must not be empty.
 *
 * With `maxFills` above 1, and not with `allOrNothing`, the same auction is a short-lived resting
 * order (tif 0). It keeps trading within the bound across up to that many transactions. One
 * transaction takes at most the token's slots of counterparties, which is 3 asks on KOB's standard
 * program. What is not filled when it expires shortly after the auction is refunded
 * (orders/simple.ts `planAuction`).
 */
export interface MarketIntent extends SimpleBase, AuctionOptions {
  type: 'market';
  side: 'sell' | 'buy';
  /**
   * The fills, meaning transactions, the order may take. The default 1 is one IOC transaction. A
   * buy budgets one delivery carrier per fill.
   */
  maxFills?: bigint;
}

/**
 * Streaming or quote-and-execute (matcher.md 10.3). It is a market order from the price the user
 * saw, within their tolerance.
 */
export interface StreamingIntent extends SimpleBase, AuctionOptions {
  type: 'streaming';
  side: 'sell' | 'buy';
  /** The displayed price the user accepted, in sompi per whole token. */
  displayedPrice: bigint;
  /** The tolerance in basis points. It is required, because it is what the user agreed to. */
  toleranceBps: bigint;
}

/** Close, a market sell of the held tokens. An omitted `amount` means the whole balance. */
export interface CloseIntent extends AuctionOptions {
  type: 'close';
  side?: 'sell';
  amount?: bigint;
  tip?: bigint;
}

interface ScheduleBase extends SimpleBase {
  /** Base units per slice, which is the on-chain `maxFill`. */
  sliceAmount: bigint;
  /** The minimum time between fills, which is the on-chain `interval`. */
  interval: Duration;
  /** The price of every slice in sompi per whole token. With `priceEnd` it is the start price. */
  price: bigint;
  /**
   * Optional. Each slice is its own auction from `price` to `priceEnd`, lower for a sell and higher
   * for a buy.
   */
  priceEnd?: bigint;
  /** The length of each slice's auction, 200 DAA by default. Only used with `priceEnd`. */
  sliceAuction?: Duration;
  lifetime?: Lifetime;
  activeFrom?: Activation;
}

/** TWAP (sell), at most `sliceAmount` per `interval`. */
export interface TwapIntent extends ScheduleBase {
  type: 'twap';
  side?: 'sell';
}

/**
 * DCA (buy), TWAP on the bid side. The escrow budgets one delivery carrier per slice unless
 * `maxFills` says more.
 */
export interface DcaIntent extends ScheduleBase {
  type: 'dca';
  side?: 'buy';
  maxFills?: bigint;
}

/**
 * Dutch decay (sell) and rising bid (buy). The price moves step by step from `price` to `priceEnd`
 * over `duration`, then rests there.
 */
export interface DutchIntent extends SimpleBase {
  type: 'dutch';
  side: 'sell' | 'buy';
  /** The start price in sompi per whole token. */
  price: bigint;
  /** The end price. For a sell it is lower than `price`, a floor. For a buy it is higher, a cap. */
  priceEnd: bigint;
  /** The time to move from `price` to `priceEnd`. */
  duration: Duration;
  /** DAA per price step. Defaults to 1. */
  stepDaa?: bigint;
  lifetime?: Lifetime;
  /** The start of the auction. Defaults to placement plus 30 DAA. */
  activeFrom?: Activation;
  maxFills?: bigint;
}

/** Every plain intent. */
export type SimpleIntent = LimitIntent | IocIntent | FokIntent | MarketIntent | StreamingIntent | CloseIntent | TwapIntent | DcaIntent | DutchIntent;

/** Every `type` planSimple handles. */
export const SIMPLE_TYPES: readonly SimpleIntent['type'][] = ['limit', 'ioc', 'fok', 'market', 'streaming', 'close', 'twap', 'dca', 'dutch'];

/** The loose base shape that every simple intent has in common. */
export interface SimpleIntentBase { type: string; side?: 'sell' | 'buy'; amount?: bigint }
