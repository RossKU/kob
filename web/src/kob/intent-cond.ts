// Intents of the conditional and if-done order types (docs/spec/order-types.md, matcher.md sections 4, 6, 6.1 and 10.6-10.9, 10.13):
// stop-market, stop-limit, trailing stop, take-profit, OCO, IFD, IFO / bracket (incl. stop entries) and repeat IFD / IFO.
//
// Conventions (same as the rest of the planning layer, see plan-types.ts):
//   * prices are SOMPI PER WHOLE TOKEN (the state price: per `scale` base units) and must lie on the token's tick; `limit`-style prices are
//     LEG prices (what the order trades at), the all-in value (tip included) is derived and disclosed;
//   * quantities are token BASE UNITS (`amount`), all amounts / prices / DAA spans are `bigint`; `slipBps` is a small integer in basis points;
//   * `side` is the side of the order that is PLACED: for `ifd` / `ifo` / `repeat*` it is the ENTRY side ('buy' = buy first and sell
//     the exit later, 'sell' = sell first and buy back later). Every field named `exit` describes the opposite-side exit.
//
// Everything optional has a documented wallet default (matcher.md section 10). Unknown fields are ignored.

import type { Activation } from './intent-simple';

/** How long a placed order lives. Default `gtc`: refundable after 90 days without activity (no fixed date). */
export type CondExpiry =
  /** good till cancelled: refundable by anyone after 90 days idle (matcher.md 5) */
  | { kind: 'gtc' }
  /** good till date: refundable from this UTC time (converted to DAA with the measured rate, section 10.10) */
  | { kind: 'gtdUnix'; atUnixSeconds: bigint }
  /** good till DAA score */
  | { kind: 'gtdDaa'; daa: bigint }
  /** until the next 00:00 UTC (entries and plain conditionals only; an exit is created later, a day order cannot be committed) */
  | { kind: 'day' };

/** Settings shared by every conditional intent. */
export interface CondCommon {
  side: 'sell' | 'buy';
  /** base units; for `ifd` / `ifo` / `repeat*` the amount of ONE cycle (N) */
  amount: bigint;
  /** priority tip in sompi per whole token (default 0). A limit is all-in: a sell receives `price - tip`, a buy pays `price + tip`. */
  tip?: bigint;
  /**
   * the smallest fill (base units) unless a fill takes everything left (the state's `minFill`). Default: the amount worth 10 KAS at the leg
   * price (kob-wasm `defaultMinFill`); if-done entries: ceil(amount / 4) (kob-wasm `defaultMinFillIfd`).
   */
  minFill?: bigint;
  /** default: gtc */
  expiry?: CondExpiry;
  /**
   * timed activation (same type and rules as the plain kinds): the covenant refuses every fill / arm before `tx.daa >= activeFrom`.
   * Default: active at once. For `ifd` / `ifo` / `repeat*` it times the ENTRY only; the exits it creates stay immediately active.
   */
  activeFrom?: Activation;
  /** KAS per covenant UTXO (order, custody, delivery, exit). Default: `env.carrier`, else 2 KAS (`DEFAULT_CARRIER`). */
  carrier?: bigint;
  /** buy-side conditionals (stop / take-profit / OCO / trailing buy): separate fills to pre-fund a delivery carrier for (default 2, at most the fills the minimum fill allows) */
  maxFills?: bigint;
}

/** Trailing parameters of a stop leg (matcher.md 4.4, 10.7). */
export interface TrailSpec {
  /** the stop moves by every whole `step` the market justifies (sompi per whole token, on the tick) */
  step: bigint;
  /** distance between the market and the stop that the trail keeps (sompi per whole token, >= 0) */
  gap: bigint;
  /** at most one update per `wait` DAA (default 6000 = 10 minutes; at least 600 = 60 s) */
  wait?: bigint;
  /** updates to pre-fund from the order's carrier / escrow (`keeperTip` each; default 20) */
  expectedUpdates?: number;
}

/** Settings of a stop leg (the protective / trigger leg of stop, OCO, trailing and exits). */
export interface StopSpec {
  /** trigger price (sompi per whole token): a sell stop arms on a fill of a resting ask quoting at or below it, a buy stop on a resting bid at or above it */
  stop: bigint;
  /** stop-limit: the worst leg price the user accepts (sell: <= stop, buy: >= stop); converted to the largest `slipBps` within it (10.6) */
  limit?: bigint;
  /** stop-market band in basis points (default 300 = 3%); ignored when `limit` is set */
  slipBps?: number;
  /** auction length in DAA (default 300 = 30 s) */
  bandDaa?: bigint;
  /** fee the keeper may take to arm / trail (default: the token program's tip, 0.019 KAS for the reference programs) */
  keeperTip?: bigint;
  /**
   * trigger threshold (touch): the smallest fill, in base units of a plain order of the same scale, of a resting order at or beyond the stop
   * that arms it (the state's `minTouch`). Default: the larger of the order's minimum fill and a quarter of its amount (kob-wasm `defaultMinTouch`); the
   * ticket offers min fill / 25% / 50% / 100% of the amount
   */
  minTouch?: bigint;
  /** trigger rest time R in DAA: how long that resting order must have been exposed at its quote before the fill (default 50 = 5 s) */
  minRestDaa?: bigint;
  /** make the stop trail (trailing stop) */
  trail?: TrailSpec;
}

/** Stop-market: a stop leg with the default 3% band. */
export interface StopMarketIntent extends CondCommon, Omit<StopSpec, 'limit' | 'trail'> {
  type: 'stopMarket';
}

/** Stop-limit: a stop leg whose band ends at `limit`. */
export interface StopLimitIntent extends CondCommon, Omit<StopSpec, 'limit' | 'trail' | 'slipBps'> {
  type: 'stopLimit';
  /** required: sell `limit <= stop`, buy `limit >= stop` */
  limit: bigint;
}

/** Trailing stop; `takeProfit` optionally adds the limit leg (a trailing OCO). */
export interface TrailingStopIntent extends CondCommon, Omit<StopSpec, 'limit' | 'trail'> {
  type: 'trailingStop';
  /** initial stop; it moves toward the market only */
  trail: TrailSpec;
  takeProfit?: bigint;
}

/** Take-profit: a conditional limit leg (composes with OCO; fills at `price` or better). */
export interface TakeProfitIntent extends CondCommon {
  type: 'takeProfit';
  price: bigint;
}

/** OCO: ONE covenant with both legs; a fill of either keeps the other for the rest (partial fills keep both legs). */
export interface OcoIntent extends CondCommon, Omit<StopSpec, 'trail'> {
  type: 'oco';
  takeProfit: bigint;
}

/** The entry of an if-done order. */
export interface EntrySpec {
  /** limit price (sompi per whole token): the most a buy entry pays / the least a sell entry receives (before the tip) */
  price: bigint;
  /** stop entry: the trigger. buy: `stop <= price`, sell: `stop >= price`; the fill runs as an auction from the stop to `price` */
  stop?: bigint;
  /** stop-entry auction length in DAA (default 300) */
  bandDaa?: bigint;
  keeperTip?: bigint;
  /** trigger threshold of a stop entry (base units; default: the entry's minimum fill) */
  minTouch?: bigint;
  minRestDaa?: bigint;
}

/** The exit(s) created by every entry fill. `ifd` needs exactly one of `takeProfit` / `stop`; `ifo` needs both. */
export interface ExitSpec {
  takeProfit?: bigint;
  stop?: bigint;
  /** stop-limit of the exit's stop leg */
  stopLimit?: bigint;
  slipBps?: number;
  bandDaa?: bigint;
  keeperTip?: bigint;
  /** trigger threshold of the exit's stop (base units; default: the exit's minimum fill) */
  minTouch?: bigint;
  minRestDaa?: bigint;
  trail?: TrailSpec;
  /** priority tip of the exit in sompi per whole token (default 0; a repeat adds the merge cost) */
  tip?: bigint;
  /** the exit's minimum fill (base units; default: the amount worth 10 KAS at the exit's leg price, at most the cycle amount) */
  minFill?: bigint;
  /** default gtc (90 days idle from each exit's creation); no `day` */
  expiry?: Exclude<CondExpiry, { kind: 'day' }>;
}

/** Repeat settings (matcher.md 6.1 and 10.13). */
export interface RepeatSpec {
  /** K: how many times each base unit re-arms. Omitted = unlimited within the 90-day bound. */
  count?: bigint;
}

interface IfdBase extends CondCommon {
  /** the entry */
  entry: EntrySpec;
  exit: ExitSpec;
  /** sell-first: KAS pre-funded per whole token beyond the entry proceeds for the exit's worst buy-back (default: the smallest sufficient amount) */
  prefund?: bigint;
}

/** IFD (if-done): entry -> one exit leg. */
export interface IfdIntent extends IfdBase { type: 'ifd' }
/** IFO / bracket: entry -> exit = OCO (take-profit + stop). */
export interface IfoIntent extends IfdBase { type: 'ifo' }
/** Repeat IFD: the entry re-arms after every take-profit. */
export interface RepeatIfdIntent extends IfdBase { type: 'repeatIfd'; repeat?: RepeatSpec }
/** Repeat IFO: as repeat IFD with an OCO exit (a stop-loss ends the repeat for that amount). */
export interface RepeatIfoIntent extends IfdBase { type: 'repeatIfo'; repeat?: RepeatSpec }

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

/** every `type` string handled by orders/cond.ts (plan.ts dispatches on this) */
export const COND_TYPES: readonly string[] = [
  'stopMarket', 'stopLimit', 'trailingStop', 'takeProfit', 'oco', 'ifd', 'ifo', 'repeatIfd', 'repeatIfo',
];

export type IfdLikeIntent = IfdIntent | IfoIntent | RepeatIfdIntent | RepeatIfoIntent;
export const isIfdLike = (i: CondIntent): i is IfdLikeIntent => i.type === 'ifd' || i.type === 'ifo' || i.type === 'repeatIfd' || i.type === 'repeatIfo';
export const isRepeat = (i: CondIntent): i is RepeatIfdIntent | RepeatIfoIntent => i.type === 'repeatIfd' || i.type === 'repeatIfo';
