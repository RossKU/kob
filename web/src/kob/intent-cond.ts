// Intents of the conditional and if-done order types (docs/spec/order-types.md, and matcher.md
// sections 4, 6, 6.1, 10.6 to 10.9 and 10.13). They are stop-market, stop-limit, trailing stop,
// take-profit, OCO, IFD, IFO or bracket with stop entries, and repeat IFD and IFO.
//
// The conventions are those of the rest of the planning layer, see plan-types.ts.
// - Prices are sompi per whole token, which is the state price per `scale` base units. They must
//   lie on the token's tick. A `limit` price is a leg price, what the order trades at. The all-in
//   value with the tip is derived and disclosed.
// - Quantities are token base units in `amount`. All amounts, prices and DAA spans are `bigint`.
//   `slipBps` is a small integer in basis points.
// - `side` is the side of the order that is placed. For `ifd`, `ifo` and `repeat*` it is the entry
//   side. 'buy' buys first and sells the exit later, and 'sell' sells first and buys back later.
//   Every field named `exit` describes the opposite-side exit.
//
// Everything optional has a documented wallet default (matcher.md section 10). Unknown fields are
// ignored.

import type { Activation } from './intent-simple';

/** How long a placed order lives. The default is `gtc`, with no fixed date. */
export type CondExpiry =
  /** Good till cancelled. Anyone can refund it after 90 days idle (matcher.md 5). */
  | { kind: 'gtc' }
  /**
   * Good till date. It is refundable from this UTC time, converted to DAA with the measured rate.
   */
  | { kind: 'gtdUnix'; atUnixSeconds: bigint }
  /** Good till DAA score. */
  | { kind: 'gtdDaa'; daa: bigint }
  /**
   * Until the next 00:00 UTC. Only entries and plain conditionals, since an exit is created later.
   */
  | { kind: 'day' };

/** Settings shared by every conditional intent. */
export interface CondCommon {
  side: 'sell' | 'buy';
  /** Base units. For `ifd`, `ifo` and `repeat*` it is the amount of one cycle. */
  amount: bigint;
  /**
   * Priority tip in sompi per whole token, 0 by default. A limit is all-in, so a sell receives
   * `price - tip` and a buy pays `price + tip`.
   */
  tip?: bigint;
  /**
   * The smallest fill in base units, unless a fill takes everything left. It is the state's
   * `minFill`. The default is the amount worth 10 KAS at the leg price (kob-wasm `defaultMinFill`).
   * For if-done entries it is ceil(amount / 4) (kob-wasm `defaultMinFillIfd`).
   */
  minFill?: bigint;
  /** Defaults to gtc. */
  expiry?: CondExpiry;
  /**
   * Timed activation, with the same type and rules as the plain kinds.
   * The covenant refuses every fill and arm before `tx.daa >= activeFrom`. The default is active at
   * once. For `ifd`, `ifo` and `repeat*` it times the entry only, and the exits stay active at
   * once.
   */
  activeFrom?: Activation;
  /**
   * KAS per covenant UTXO (order, custody, delivery, exit). Defaults to `env.carrier`, else 1 KAS
   * (`DEFAULT_CARRIER`).
   */
  carrier?: bigint;
  /**
   * Separate fills to pre-fund a delivery carrier for, on buy-side conditionals.
   * Those are stop, take-profit, OCO and trailing buy. The default is 2, at most the number of
   * fills the minimum fill allows.
   */
  maxFills?: bigint;
}

/** Trailing parameters of a stop leg (matcher.md 4.4, 10.7). */
export interface TrailSpec {
  /**
   * The stop moves by every whole `step` the market justifies, in sompi per whole token on the
   * tick.
   */
  step: bigint;
  /**
   * The distance the trail keeps between the market and the stop, in sompi per whole token, at
   * least 0.
   */
  gap: bigint;
  /**
   * At most one update per `wait` DAA. The default is 6000 (10 minutes) and the minimum is 600 (60
   * s).
   */
  wait?: bigint;
  /** Updates to pre-fund from the order's carrier or escrow, `keeperTip` each. Defaults to 20. */
  expectedUpdates?: number;
}

/** Settings of a stop leg, the protective or trigger leg of stop, OCO, trailing and exits. */
export interface StopSpec {
  /**
   * The trigger price in sompi per whole token.
   * A sell stop arms on a fill of a resting ask quoting at or below it. A buy stop arms on a
   * resting bid at or above it.
   */
  stop: bigint;
  /**
   * For a stop-limit, the worst leg price the user accepts, at most `stop` for a sell and at least
   * `stop` for a buy. It is converted to the largest `slipBps` within it (matcher.md 10.6).
   */
  limit?: bigint;
  /** The stop-market band in basis points, 300 (3%) by default. Ignored when `limit` is set. */
  slipBps?: number;
  /** Auction length in DAA, 300 (30 s) by default. */
  bandDaa?: bigint;
  /**
   * The fee the keeper may take to arm or trail. Defaults to the token program's tip, 0.019 KAS for
   * the reference programs.
   */
  keeperTip?: bigint;
  /**
   * The trigger threshold, called the touch.
   * It is the smallest fill of a resting order at or beyond the stop that arms it, in base units
   * of a plain order of the same scale. It is the state's `minTouch`. The default is the larger of
   * the order's minimum fill and a quarter of its amount (kob-wasm `defaultMinTouch`). The ticket
   * offers the minimum fill, 25%, 50% and 100% of the amount.
   */
  minTouch?: bigint;
  /**
   * The trigger rest time R in DAA: how long that resting order must have been exposed at its quote
   * before the fill. Defaults to 50 (5 s).
   */
  minRestDaa?: bigint;
  /** Makes the stop trail, which gives a trailing stop. */
  trail?: TrailSpec;
}

/** Stop-market, a stop leg with the default 3% band. */
export interface StopMarketIntent extends CondCommon, Omit<StopSpec, 'limit' | 'trail'> {
  type: 'stopMarket';
}

/** Stop-limit, a stop leg whose band ends at `limit`. */
export interface StopLimitIntent extends CondCommon, Omit<StopSpec, 'limit' | 'trail' | 'slipBps'> {
  type: 'stopLimit';
  /** Required. A sell needs `limit <= stop` and a buy needs `limit >= stop`. */
  limit: bigint;
}

/** Trailing stop. `takeProfit` optionally adds the limit leg, which makes a trailing OCO. */
export interface TrailingStopIntent extends CondCommon, Omit<StopSpec, 'limit' | 'trail'> {
  type: 'trailingStop';
  /** The initial stop. It moves toward the market only. */
  trail: TrailSpec;
  takeProfit?: bigint;
}

/** Take-profit, a conditional limit leg that fills at `price` or better. It composes with OCO. */
export interface TakeProfitIntent extends CondCommon {
  type: 'takeProfit';
  price: bigint;
}

/**
 * OCO, one covenant with both legs. A fill of either keeps the other for the rest, and a partial
 * fill keeps both.
 */
export interface OcoIntent extends CondCommon, Omit<StopSpec, 'trail'> {
  type: 'oco';
  takeProfit: bigint;
}

/** The entry of an if-done order. */
export interface EntrySpec {
  /**
   * The limit price in sompi per whole token, before the tip. A buy entry pays at most it and a
   * sell entry receives at least it.
   */
  price: bigint;
  /**
   * The trigger of a stop entry. A buy needs `stop <= price` and a sell needs `stop >= price`.
   * The fill runs as an auction from the stop to `price`.
   */
  stop?: bigint;
  /** Auction length of a stop entry in DAA. Defaults to 300. */
  bandDaa?: bigint;
  keeperTip?: bigint;
  /** The trigger threshold of a stop entry in base units. Defaults to the entry's minimum fill. */
  minTouch?: bigint;
  minRestDaa?: bigint;
}

/**
 * The exits created by every entry fill. `ifd` needs exactly one of `takeProfit` and `stop`, and
 * `ifo` needs both.
 */
export interface ExitSpec {
  takeProfit?: bigint;
  stop?: bigint;
  /** The stop-limit of the exit's stop leg. */
  stopLimit?: bigint;
  slipBps?: number;
  bandDaa?: bigint;
  keeperTip?: bigint;
  /**
   * The trigger threshold of the exit's stop in base units. Defaults to the exit's minimum fill.
   */
  minTouch?: bigint;
  minRestDaa?: bigint;
  trail?: TrailSpec;
  /**
   * The priority tip of the exit in sompi per whole token. Defaults to 0, and a repeat adds the
   * merge cost.
   */
  tip?: bigint;
  /**
   * The exit's minimum fill in base units. Defaults to the amount worth 10 KAS at the exit's leg
   * price, at most the cycle amount.
   */
  minFill?: bigint;
  /** Defaults to gtc, which is 90 days idle from each exit's creation. There is no `day`. */
  expiry?: Exclude<CondExpiry, { kind: 'day' }>;
}

/** Repeat settings (matcher.md 6.1 and 10.13). */
export interface RepeatSpec {
  /** How many times each base unit re-arms. Omitted means unlimited within the 90-day bound. */
  count?: bigint;
}

interface IfdBase extends CondCommon {
  /** The entry. */
  entry: EntrySpec;
  exit: ExitSpec;
  /**
   * For sell-first, the KAS pre-funded per whole token beyond the entry proceeds for the exit's
   * worst buy-back. The default is the smallest sufficient amount.
   */
  prefund?: bigint;
}

/** IFD (if-done), an entry followed by one exit leg. */
export interface IfdIntent extends IfdBase { type: 'ifd' }
/** IFO or bracket, an entry followed by an OCO exit of take-profit and stop. */
export interface IfoIntent extends IfdBase { type: 'ifo' }
/** Repeat IFD. The entry re-arms after every take-profit. */
export interface RepeatIfdIntent extends IfdBase { type: 'repeatIfd'; repeat?: RepeatSpec }
/** Repeat IFO, like repeat IFD with an OCO exit. A stop-loss ends the repeat for that amount. */
export interface RepeatIfoIntent extends IfdBase { type: 'repeatIfo'; repeat?: RepeatSpec }

/** Every conditional or if-done intent. */
export type CondIntent =
  | StopMarketIntent
  | StopLimitIntent
  | TrailingStopIntent
  | TakeProfitIntent
  | OcoIntent
  | IfdIntent
  | IfoIntent
  | RepeatIfdIntent
  | RepeatIfoIntent;

/** Every `type` string handled by orders/cond.ts. plan.ts dispatches on it. */
export const COND_TYPES: readonly string[] = [
  'stopMarket', 'stopLimit', 'trailingStop', 'takeProfit', 'oco', 'ifd', 'ifo', 'repeatIfd', 'repeatIfo',
];

/** The intents that have an entry and an exit. */
export type IfdLikeIntent = IfdIntent | IfoIntent | RepeatIfdIntent | RepeatIfoIntent;
/** True for an intent with an entry and an exit. */
export const isIfdLike = (i: CondIntent): i is IfdLikeIntent => i.type === 'ifd' || i.type === 'ifo' || i.type === 'repeatIfd' || i.type === 'repeatIfo';
/** True for a repeat intent. */
export const isRepeat = (i: CondIntent): i is RepeatIfdIntent | RepeatIfoIntent => i.type === 'repeatIfd' || i.type === 'repeatIfo';
