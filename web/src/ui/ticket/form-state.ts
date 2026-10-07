// Order-ticket form state <-> Intent mapping (pure: no DOM, no network; runs in node tests).
//
// The ticket keeps EVERY input as text (`values`, keyed by the intent path of the field: `price`, `stop`, `trail.step`, `exit.takeProfit` ...)
// so what the user typed is never rewritten under their fingers; `buildIntent` parses the text into the bigint intents of
// kob/intent-simple.ts and kob/intent-cond.ts and reports per-field problems. `formFromIntent` is the inverse (prefill, cancel-replace, tests).
//
// Units the user types (all converted here, nowhere else):
//   * amounts (`amount`, `minFill`, `sliceAmount`, `exit.minFill`): TOKEN units with the token's decimals, converted exactly to base units (more
//     decimals than the token has is an error, never a rounding);
//   * prices and price distances: the QUOTE per whole token, converted to the state price (quote base units per whole token of `scale` base
//     units: the identity for the wallet's scale 10^decimals) and rounded onto the tick in the direction that never makes the maker's limit worse
//     (a sell rounds up, a buy rounds down). The quote is KAS (8 decimals, sompi) on a token's KAS market and token B (its decimals, base units
//     of B) on a token/token pair A/B (`TicketCtx.quoteDecimals`): the pair ticket offers exactly the order types of the KAS ticket;
//   * tips: ALWAYS KAS per whole token (sompi per whole token, also on a pair: tips, keeper tips and carriers are KAS), rounded to nearest (a tip
//     is not a limit);
//   * slippage / tolerance: percent (two decimals) -> basis points; durations: seconds or minutes -> `Duration` / DAA;
//   * trigger threshold (`touch` fields): `min` (the order's minimum fill, the default), a preset percent of the order's own amount (`25%`,
//     `50%`, `100%`, rounded up to whole base units) or a custom token amount.
// Protocol rules (tick, 90-day limits, stop above take-profit ...) are the PLANNER's job (kob/plan.ts): this layer only refuses text that
// cannot become a number and leaves everything optional undefined so the planner applies the wallet defaults of matcher.md section 10.
import type { Intent } from '../../kob/plan';
import type { CondExpiry, EntrySpec, ExitSpec, TrailSpec } from '../../kob/intent-cond';
import type { Lifetime } from '../../kob/intent-simple';
import { secondsToDaa } from '../../kob/daa';
import {
  UnitsError, fitsI64, formatPricePerToken, formatUnits, parseKas, parseUnits, pow10, roundToTick, safeRounding, tokenPriceToStatePrice, type Rounding,
} from '../../kob/units';

/** most levels of a repeat ladder (one transaction and one signature each) */
export const MAX_LADDER_LEVELS = 20n;

/**
 * Trigger-threshold presets of the ticket (matcher.md 10.6): `min` = the order's minimum fill (the wallet default), or a percent of the order's
 * own amount. Any other text is a custom token amount.
 */
export const TOUCH_PRESETS = ['min', '25%', '50%', '100%'] as const;
export const TOUCH_MIN = 'min';

/**
 * Threshold in base units of a `touch` field text: `min` (null: the wallet default, the order's minimum fill), `N%` of `orderAmount` rounded up
 * (at least one base unit), or a token amount with `decimals`.
 */
export function parseTouch(text: string, orderAmount: bigint | undefined, decimals: number): Parsed<bigint | null> {
  const s = text.trim();
  if (s === '' || s === TOUCH_MIN) return good(null);
  const m = /^(\d{1,3})%$/.exec(s);
  if (!m) {
    const a = parseAmount(s, decimals);
    return a.ok && a.value === 0n ? bad('positive') : a;
  }
  const pct = BigInt(m[1]!);
  if (pct < 1n || pct > 100n) return bad('range');
  // no order size yet (the amount field has its own error): nothing to resolve against
  if (orderAmount === undefined || orderAmount <= 0n) return bad('required');
  const amount = (orderAmount * pct + 99n) / 100n;
  return good(amount < 1n ? 1n : amount);
}

// ------------------------------------------------------------------------------------------------ vocabulary

export const ORDER_TYPES = [
  'limit', 'market', 'ioc', 'fok', 'streaming', 'close',
  'stopMarket', 'stopLimit', 'trailingStop', 'takeProfit',
  'oco', 'ifd', 'ifo', 'repeatIfd', 'repeatIfo',
  'twap', 'dca', 'dutch',
] as const;
export type OrderTypeId = (typeof ORDER_TYPES)[number];

export type TypeGroup = 'basic' | 'trigger' | 'combination' | 'algorithmic';
export const TYPE_GROUPS: readonly { group: TypeGroup; types: readonly OrderTypeId[] }[] = [
  { group: 'basic', types: ['limit', 'market', 'ioc', 'fok', 'streaming', 'close'] },
  { group: 'trigger', types: ['stopMarket', 'stopLimit', 'trailingStop', 'takeProfit'] },
  { group: 'combination', types: ['oco', 'ifd', 'ifo', 'repeatIfd', 'repeatIfo'] },
  { group: 'algorithmic', types: ['twap', 'dca', 'dutch'] },
];

export type Side = 'buy' | 'sell';

/** Sides an order type can be placed on (close and TWAP only sell, DCA only buys: intent-simple.ts). */
export function sidesOf(type: OrderTypeId): readonly Side[] {
  return type === 'close' || type === 'twap' ? ['sell'] : type === 'dca' ? ['buy'] : ['buy', 'sell'];
}

export const isIfdType = (t: OrderTypeId): boolean => t === 'ifd' || t === 'ifo' || t === 'repeatIfd' || t === 'repeatIfo';
export const isCondType = (t: OrderTypeId): boolean => ['stopMarket', 'stopLimit', 'trailingStop', 'takeProfit', 'oco'].includes(t) || isIfdType(t);

/** `amount`: a token amount typed in whole-token units with the token's decimals (stored as base units) */
export type FieldKind = 'amount' | 'int' | 'price' | 'delta' | 'tip' | 'kas' | 'percent' | 'seconds' | 'minutes' | 'datetime' | 'select' | 'bool' | 'touch';

export interface FieldSpec {
  /** intent path of the field, key of `values` */
  id: string;
  kind: FieldKind;
  /** the `data-testid` of the input (UI-COMMON: order-amount / order-price / order-tip, else field-<intent path>) */
  testid: string;
  options?: readonly string[];
}

const F = (id: string, kind: FieldKind, options?: readonly string[]): FieldSpec => ({
  id,
  kind,
  testid: id === 'amount' ? 'order-amount' : id === 'price' ? 'order-price' : id === 'tip' ? 'order-tip' : `field-${id}`,
  ...(options ? { options } : {}),
});

export const FIELDS: Readonly<Record<string, FieldSpec>> = Object.fromEntries(
  [
    F('amount', 'amount'), F('price', 'price'), F('tip', 'tip'), F('minFill', 'amount'),
    F('lifetime', 'select', ['gtc', 'gtd', 'day']), F('lifetimeAt', 'datetime'), F('activeFrom', 'datetime'),
    F('crossing', 'select', ['auction', 'limit', 'reject']), F('maxFills', 'int'),
    F('life', 'seconds'), F('slippageBps', 'percent'), F('auction', 'seconds'), F('allOrNothing', 'bool'),
    F('displayedPrice', 'price'), F('toleranceBps', 'percent'),
    F('sliceAmount', 'amount'), F('interval', 'minutes'), F('priceEnd', 'price'), F('sliceAuction', 'seconds'), F('duration', 'minutes'), F('stepDaa', 'int'),
    F('stop', 'price'), F('limit', 'price'), F('slipBps', 'percent'), F('bandDaa', 'seconds'), F('keeperTip', 'kas'), F('minTouch', 'touch'),
    F('minRestDaa', 'seconds'), F('takeProfit', 'price'), F('carrier', 'kas'),
    F('trail.step', 'delta'), F('trail.gap', 'delta'), F('trail.wait', 'minutes'), F('trail.expectedUpdates', 'int'),
    F('entry.stop', 'price'), F('entry.bandDaa', 'seconds'), F('entry.minTouch', 'touch'), F('entry.minRestDaa', 'seconds'),
    F('exit.kind', 'select', ['takeProfit', 'stop']), F('exit.takeProfit', 'price'), F('exit.stop', 'price'), F('exit.stopLimit', 'price'),
    F('exit.slipBps', 'percent'), F('exit.minTouch', 'touch'), F('exit.minRestDaa', 'seconds'), F('exit.trail', 'bool'), F('exit.trail.step', 'delta'), F('exit.trail.gap', 'delta'), F('exit.tip', 'tip'),
    F('exit.minFill', 'amount'), F('exit.lifetime', 'select', ['gtc', 'gtd']), F('exit.lifetimeAt', 'datetime'),
    F('ladder.levels', 'int'), F('ladder.step', 'delta'),
    F('prefund', 'delta'), F('repeat.count', 'int'),
  ].map((f) => [f.id, f]),
);

/** Wallet defaults of matcher.md section 10 / defaults.rs, shown as the prefilled value (an empty box always means "the default"). */
export const FIELD_DEFAULTS: Readonly<Record<string, string>> = {
  lifetime: 'gtc',
  crossing: 'auction',
  life: '30',
  slippageBps: '3',
  auction: '20',
  toleranceBps: '3',
  slipBps: '3',
  bandDaa: '30',
  'entry.bandDaa': '30',
  // trigger (touch rule, matcher.md §4, §10.6): threshold = the order's minimum fill (founder 2026-10-03; presets 25% / 50% / 100% of the order,
  // or a custom amount), R = 5 s
  minTouch: TOUCH_MIN,
  minRestDaa: '5',
  'entry.minTouch': TOUCH_MIN,
  'entry.minRestDaa': '5',
  'exit.minTouch': TOUCH_MIN,
  'exit.minRestDaa': '5',
  'trail.wait': '10',
  'trail.expectedUpdates': '20',
  'exit.kind': 'takeProfit',
  'exit.lifetime': 'gtc',
  'ladder.levels': '1',
  'exit.slipBps': '3',
  sliceAuction: '20',
  stepDaa: '1',
};

// ------------------------------------------------------------------------------------------------ state

export interface TicketForm {
  type: OrderTypeId;
  side: Side;
  /** text of every field, by intent path; absent = empty */
  values: Record<string, string>;
  /**
   * The amount boxes count KAS (a token's KAS market shown inverted, KAS/TOKEN: the shown pair's base is KAS). Every amount field (`amount`,
   * `minFill`, `sliceAmount`, `exit.minFill`, a custom trigger threshold) is then typed in KAS and converted to the token's base units at the
   * price of its own leg (`amountPriceOf`); prices and side stay native in the form. Absent / false = token units (the native convention).
   */
  kas?: boolean;
}

export interface TicketCtx {
  /** the (base) token's decimals, order scale (base units per whole token, `10^decimals` capped at 10^9) and tick (quote units per whole token; 1) */
  decimals: number;
  scale: bigint;
  tick: bigint;
  /** decimals of the quote the prices are typed in: 8 (KAS, the default) or a pair's quote token B (prices in B base units per whole A) */
  quoteDecimals?: number;
  /** minutes WEST of UTC as `Date.getTimezoneOffset()` (0 = UTC): `datetime-local` text is wall-clock time of the browser */
  tzOffsetMin?: number;
  /** measured DAA rate in milli-DAA per second (default 10 000 = 10 DAA/s) */
  rateMilli?: number;
  /**
   * the book's best prices (state prices, sompi per `scale` base units): the price a KAS amount of an order WITHOUT its own price (market,
   * close) is converted at (`kas` forms only). `buy` = the best ask (what a buy meets), `sell` = the best bid.
   */
  ref?: { buy?: bigint | null; sell?: bigint | null };
}

export function initialForm(type: OrderTypeId = 'limit', side: Side = 'sell', kas = false): TicketForm {
  const sides = sidesOf(type);
  const values: Record<string, string> = {};
  for (const id of new Set([...layoutOf({ type, side, values: {} }).main, ...layoutOf({ type, side, values: {} }).advanced])) {
    const d = FIELD_DEFAULTS[id];
    if (d !== undefined) values[id] = d;
  }
  return { type, side: sides.includes(side) ? side : sides[0]!, values, ...(kas ? { kas: true } : {}) };
}

/** Switching the type keeps what still applies (amount, prices, tip, lifetime ...), adds the defaults of the new type, fixes an impossible side. */
export function switchType(form: TicketForm, type: OrderTypeId): TicketForm {
  const side = sidesOf(type).includes(form.side) ? form.side : sidesOf(type)[0]!;
  const next = initialForm(type, side, !!form.kas);
  const values = { ...next.values };
  const applicable = new Set([...layoutOf({ type, side, values: {} }).main, ...layoutOf({ type, side, values: {} }).advanced]);
  for (const [k, v] of Object.entries(form.values)) {
    // an exact base amount kept for a KAS amount box travels with its box
    const field = k.endsWith(BASE_SUFFIX) ? k.slice(0, -BASE_SUFFIX.length) : k;
    if (applicable.has(field) && v !== '') values[k] = v;
  }
  return { ...next, type, side, values };
}

export function setSide(form: TicketForm, side: Side): TicketForm {
  return sidesOf(form.type).includes(side) ? { ...form, side } : form;
}

/** What the user typed into a box. An exact base amount kept for that box (`setAmountBase`) is dropped: the typed text is the amount now. */
export function setValue(form: TicketForm, id: string, value: string): TicketForm {
  const values = { ...form.values, [id]: value };
  delete values[id + BASE_SUFFIX];
  return { ...form, values };
}

// ------------------------------------------------------------------------------------------------ layout

export interface Layout {
  /** always shown */
  main: string[];
  /** behind the "advanced" disclosure */
  advanced: string[];
}

const LIFE = ['lifetime', 'lifetimeAt'];
const STOP_KNOBS = ['minTouch', 'minRestDaa', 'bandDaa', 'keeperTip'];
/** trigger knobs of a stop entry / an exit stop (touch rule): shown for every trigger kind */
const ENTRY_TRIGGER = ['entry.minTouch', 'entry.minRestDaa'];
const EXIT_TRIGGER = ['exit.minTouch', 'exit.minRestDaa'];

/** Which fields the form shows for its type, side and current selections (conditional fields appear when their switch is on). */
export function layoutOf(form: TicketForm): Layout {
  const v = form.values;
  const buy = form.side === 'buy';
  const life = (): string[] => (v.lifetime === 'gtd' ? LIFE : ['lifetime']);
  const fills = buy ? ['maxFills'] : [];
  /** how long the exits of an if-done order live (GTC = 90 days idle from each exit's creation, or until a date) */
  const exitLife = (): string[] => (v['exit.lifetime'] === 'gtd' ? ['exit.lifetime', 'exit.lifetimeAt'] : ['exit.lifetime']);
  /** ladder: the step is asked once there is more than one level */
  const ladder = (): string[] => (Number((v['ladder.levels'] ?? '').trim()) > 1 ? ['ladder.levels', 'ladder.step'] : ['ladder.levels']);
  // the minimum fill is an advanced option of every type (its default is shown in the disclosure; IOC / FOK / market orders: 1 base unit)
  switch (form.type) {
    case 'limit': return { main: ['amount', 'price', 'tip', ...life()], advanced: ['minFill', 'activeFrom', 'crossing', ...fills] };
    case 'ioc':
    case 'fok': return { main: ['amount', 'price', 'tip'], advanced: ['minFill', 'life', 'activeFrom'] };
    case 'market': return { main: ['amount', 'slippageBps', 'tip'], advanced: ['minFill', 'auction', 'life', 'allOrNothing'] };
    case 'streaming': return { main: ['amount', 'displayedPrice', 'toleranceBps', 'tip'], advanced: ['minFill', 'allOrNothing', 'auction', 'life'] };
    case 'close': return { main: ['amount', 'tip'], advanced: ['slippageBps', 'auction', 'life', 'allOrNothing'] };
    case 'twap':
    case 'dca': return { main: ['amount', 'sliceAmount', 'interval', 'price', 'tip'], advanced: ['minFill', 'priceEnd', 'sliceAuction', ...life(), 'activeFrom', ...(form.type === 'dca' ? ['maxFills'] : [])] };
    case 'dutch': return { main: ['amount', 'price', 'priceEnd', 'duration', 'tip'], advanced: ['minFill', 'stepDaa', ...life(), 'activeFrom', ...fills] };
    case 'stopMarket': return { main: ['amount', 'stop', 'tip'], advanced: ['minFill', 'slipBps', ...STOP_KNOBS, ...life(), 'activeFrom', 'carrier', ...fills] };
    case 'stopLimit': return { main: ['amount', 'stop', 'limit', 'tip'], advanced: ['minFill', ...STOP_KNOBS, ...life(), 'activeFrom', 'carrier', ...fills] };
    case 'trailingStop':
      return { main: ['amount', 'stop', 'trail.step', 'trail.gap', 'tip'], advanced: ['minFill', 'takeProfit', 'trail.wait', 'trail.expectedUpdates', 'slipBps', ...STOP_KNOBS, ...life(), 'activeFrom', 'carrier', ...fills] };
    case 'takeProfit': return { main: ['amount', 'price', 'tip', ...life()], advanced: ['minFill', 'activeFrom', 'carrier', ...fills] };
    case 'oco': return { main: ['amount', 'takeProfit', 'stop', 'tip'], advanced: ['minFill', 'limit', 'slipBps', ...STOP_KNOBS, ...life(), 'activeFrom', 'carrier', ...fills] };
    case 'ifd':
    case 'repeatIfd': {
      const rep = form.type === 'repeatIfd';
      const exitKind = rep ? 'takeProfit' : (v['exit.kind'] ?? 'takeProfit');
      const main = ['amount', 'price', ...(rep ? [] : ['exit.kind']), exitKind === 'stop' ? 'exit.stop' : 'exit.takeProfit', ...(rep ? ['repeat.count'] : []), 'tip'];
      const trailing = exitKind === 'stop' ? ['exit.trail', ...(v['exit.trail'] === 'true' ? ['exit.trail.step', 'exit.trail.gap'] : [])] : [];
      const adv = [
        'entry.stop', 'entry.bandDaa', ...ENTRY_TRIGGER, ...(exitKind === 'stop' ? ['exit.stopLimit', 'exit.slipBps', ...EXIT_TRIGGER] : []), ...trailing,
        'minFill', 'exit.minFill', ...(buy ? [] : ['prefund']), ...life(), 'activeFrom', ...exitLife(), 'exit.tip', ...(rep ? ladder() : []), 'carrier',
      ];
      return { main, advanced: adv };
    }
    case 'ifo':
    case 'repeatIfo': {
      const rep = form.type === 'repeatIfo';
      return {
        main: ['amount', 'price', 'exit.takeProfit', 'exit.stop', ...(rep ? ['repeat.count'] : []), 'tip'],
        advanced: [
          'entry.stop', 'entry.bandDaa', ...ENTRY_TRIGGER, 'exit.stopLimit', 'exit.slipBps', ...EXIT_TRIGGER, 'exit.trail', ...(v['exit.trail'] === 'true' ? ['exit.trail.step', 'exit.trail.gap'] : []),
          'minFill', 'exit.minFill', ...(buy ? [] : ['prefund']), ...life(), 'activeFrom', ...exitLife(), 'exit.tip', ...(rep ? ladder() : []), 'carrier',
        ],
      };
    }
  }
}

/** Every field id an order type can show (any selection): to drop stale values when the type changes. */
export const specsOf = (ids: readonly string[]): FieldSpec[] => ids.map((i) => FIELDS[i]!).filter(Boolean);

// ------------------------------------------------------------------------------------------------ parsing

export type FieldErrorCode = 'required' | 'format' | 'precision' | 'positive' | 'range' | 'negative' | 'noRef';
export interface FieldError { field: string; code: FieldErrorCode; params?: Record<string, string | number> }
export interface BuildResult {
  /** the intent for `planOrder`, null while any field is missing or malformed */
  intent: Intent | null;
  errors: FieldError[];
}

const I64_TEXT_MAX = 1n << 63n;

const unitsCode = (e: unknown): FieldErrorCode => {
  if (e instanceof UnitsError) return e.code === 'empty' ? 'required' : e.code === 'too_many_decimals' ? 'precision' : e.code === 'negative' ? 'negative' : 'format';
  throw e;
};

type Parsed<T> = { ok: true; value: T } | { ok: false; code: FieldErrorCode; params?: Record<string, string | number> };
const good = <T,>(value: T): Parsed<T> => ({ ok: true, value });
const bad = (code: FieldErrorCode, params?: Record<string, string | number>): Parsed<never> => ({ ok: false, code, ...(params ? { params } : {}) });

/** A whole number >= 0 typed as digits only. */
export function parseWhole(text: string): Parsed<bigint> {
  const s = text.trim();
  if (s === '') return bad('required');
  if (!/^\d+$/.test(s)) return bad(s.startsWith('-') ? 'negative' : 'format');
  const n = BigInt(s);
  return n >= I64_TEXT_MAX ? bad('range') : good(n);
}

/** Decimal text -> value scaled by 10^scale, at most `scale` decimals. */
function parseScaled(text: string, scale: number): Parsed<bigint> {
  try {
    const n = parseUnits(text, scale);
    return n >= I64_TEXT_MAX ? bad('range') : good(n);
  } catch (e) {
    return bad(unitsCode(e), scale >= 0 ? { decimals: scale } : undefined);
  }
}

/** A token amount typed in whole-token units (at most `decimals` decimals) -> base units, exactly. */
export const parseAmount = (text: string, decimals: number): Parsed<bigint> => parseScaled(text, decimals);

/** Base units -> the token amount text the amount fields take (exact, trailing zeros trimmed). */
export const formatAmount = (base: bigint, decimals: number): string => formatUnits(base, decimals);

export interface PriceParse {
  /** the state price (sompi per whole token of `scale` base units) after the tick rounding */
  price: bigint;
  /** the typed price is not on the tick / not a whole number of sompi per `scale` base units: `price` is the rounded one */
  rounded: boolean;
}

/** What a price-like field is denominated in: the market's quote (KAS, or a pair's token B) or KAS (tips, always). */
export type PriceUnit = 'quote' | 'kas';
/** Decimals of the typed number of a price-like field. */
export const unitDecimals = (ctx: Pick<TicketCtx, 'quoteDecimals'>, unit: PriceUnit = 'quote'): number => (unit === 'kas' ? 8 : ctx.quoteDecimals ?? 8);

/**
 * Quote per token (text: KAS, or a pair's token B per whole A) -> the state price on the tick. `mode` 'safe' rounds in the direction that never
 * makes a limit worse for `side` (sell up, buy down); 'nearest' is for tips and distances. `unit` 'kas' reads KAS whatever the quote (tips).
 * Never returns a price that overflows the 64-bit state fields.
 */
export function parsePrice(
  text: string, side: Side, ctx: Pick<TicketCtx, 'decimals' | 'scale' | 'tick' | 'quoteDecimals'>, mode: 'safe' | 'nearest' | 'exact' = 'safe', allowZero = false, unit: PriceUnit = 'quote',
): Parsed<PriceParse> {
  let perToken: bigint;
  const qd = unitDecimals(ctx, unit);
  try {
    perToken = qd === 8 ? parseKas(text) : parseUnits(text, qd);
  } catch (e) {
    return bad(unitsCode(e), { decimals: qd });
  }
  const rounding: Rounding = mode === 'safe' ? safeRounding(side) : 'nearest';
  // sompi per `scale` base units: the identity when scale = 10^decimals (the wallet's scale); a token of more than 9 decimals quotes per 10^9
  const exact = perToken * ctx.scale;
  const whole = pow10(ctx.decimals);
  const floor = exact / whole;
  const price = tokenPriceToStatePrice(perToken, ctx.decimals, ctx.scale, rounding);
  // limits and trail steps live on the tick; tips and gaps are plain quote units (the planner does not tick-check them); a tip is never on a tick
  const tick = unit === 'kas' ? 1n : ctx.tick;
  const onTick = mode === 'exact' ? price : roundToTick(price, tick, rounding);
  const rounded = exact % whole !== 0n || (mode !== 'exact' && tick > 0n && floor % tick !== 0n) || (mode !== 'exact' && onTick !== floor);
  if (!allowZero && onTick <= 0n) return bad('positive');
  if (!fitsI64(onTick)) return bad('range');
  return good({ price: onTick, rounded });
}

/** A state price (quote units per whole token of `scale` base units) -> quote per token text (exact, trailing zeros trimmed; KAS or token B). */
export const formatPrice = (price: bigint, ctx: Pick<TicketCtx, 'decimals' | 'scale' | 'quoteDecimals'>, unit: PriceUnit = 'quote'): string => {
  const qd = unitDecimals(ctx, unit);
  if (qd === 8) return formatPricePerToken(price, ctx.decimals, ctx.scale);
  // quote per whole token = price * 10^decimals / scale / 10^qd, shown with up to qd (at least 8) fraction digits, rounded half up like the KAS form
  const digits = Math.max(qd, 8);
  const num = price * pow10(ctx.decimals) * pow10(digits);
  const den = ctx.scale * pow10(qd);
  return formatUnits((num + den / 2n) / den, digits);
};

/** Basis points -> the percent text of the percent fields. */
export const formatPercent = (bps: bigint): string => formatUnits(bps, 2);

/** `datetime-local` text (`2026-10-01T09:30`, wall clock of the browser) -> UTC unix seconds. `tzOffsetMin` = `Date.getTimezoneOffset()`. */
export function parseLocalDateTime(text: string, tzOffsetMin = 0): Parsed<bigint> {
  const s = text.trim();
  if (s === '') return bad('required');
  const m = /^(\d{4})-(\d{2})-(\d{2})[T ](\d{2}):(\d{2})(?::(\d{2}))?$/.exec(s);
  if (!m) return bad('format');
  const [y, mo, d, h, mi, se] = [m[1]!, m[2]!, m[3]!, m[4]!, m[5]!, m[6] ?? '0'].map(Number) as [number, number, number, number, number, number];
  const ms = Date.UTC(y, mo - 1, d, h, mi, se);
  const back = new Date(ms);
  // Date.UTC normalises 31 February to March: refuse impossible dates
  if (back.getUTCFullYear() !== y || back.getUTCMonth() !== mo - 1 || back.getUTCDate() !== d || h > 23 || mi > 59) return bad('format');
  return good(BigInt(Math.floor(ms / 1000)) + BigInt(tzOffsetMin) * 60n);
}

/** UTC unix seconds -> `datetime-local` text of the browser's wall clock. */
export function formatLocalDateTime(unixSeconds: bigint, tzOffsetMin = 0): string {
  const d = new Date(Number(unixSeconds - BigInt(tzOffsetMin) * 60n) * 1000);
  const p = (n: number) => String(n).padStart(2, '0');
  return `${d.getUTCFullYear()}-${p(d.getUTCMonth() + 1)}-${p(d.getUTCDate())}T${p(d.getUTCHours())}:${p(d.getUTCMinutes())}`;
}

// ------------------------------------------------------------------------------------------------ intent builder

class Reader {
  readonly errors: FieldError[] = [];
  constructor(readonly form: TicketForm, readonly ctx: TicketCtx) {}

  raw(id: string): string {
    return (this.form.values[id] ?? '').trim();
  }
  has(id: string): boolean {
    return this.raw(id) !== '';
  }
  private take<T>(id: string, p: Parsed<T>, required: boolean): T | undefined {
    if (p.ok) return p.value;
    if (p.code === 'required' && !required) return undefined;
    this.errors.push({ field: id, code: p.code, ...(p.params && p.code !== 'required' ? { params: p.params } : {}) });
    return undefined;
  }
  whole(id: string, required = true): bigint | undefined {
    if (!this.has(id) && !required) return undefined;
    return this.take(id, parseWhole(this.raw(id)), required);
  }
  /** whole number >= 1 */
  positive(id: string, required = true): bigint | undefined {
    const n = this.whole(id, required);
    if (n === 0n) {
      this.errors.push({ field: id, code: 'positive' });
      return undefined;
    }
    return n;
  }
  /** a token amount (whole-token units with the token's decimals; KAS converted at its leg's price in a `kas` form) in base units, at least one base unit */
  amount(id: string, required = true): bigint | undefined {
    if (!this.has(id) && this.form.values[id + BASE_SUFFIX] === undefined && !required) return undefined;
    const n = this.take(id, amountBaseOf(this.form, this.ctx, id), required);
    if (n === 0n) {
      this.errors.push({ field: id, code: 'positive' });
      return undefined;
    }
    return n;
  }
  price(id: string, side: Side = this.form.side, required = true): bigint | undefined {
    if (!this.has(id) && !required) return undefined;
    return this.take(id, parsePrice(this.raw(id), side, this.ctx), required)?.price;
  }
  /** a price distance (quote per token -> the state price, nearest; `onTick`: also on the tick, as a trail step must be); zero allowed unless `allowZero` is false */
  delta(id: string, required = false, allowZero = true, onTick = false): bigint | undefined {
    if (!this.has(id) && !required) return undefined;
    return this.take(id, parsePrice(this.raw(id), this.form.side, this.ctx, onTick ? 'nearest' : 'exact', allowZero), required)?.price;
  }
  /** a tip: KAS per token (also on a pair) -> sompi per whole token of `scale` base units, exact; zero allowed */
  tipKas(id: string): bigint | undefined {
    if (!this.has(id)) return undefined;
    return this.take(id, parsePrice(this.raw(id), this.form.side, this.ctx, 'exact', true, 'kas'), false)?.price;
  }
  kas(id: string, required = false): bigint | undefined {
    if (!this.has(id) && !required) return undefined;
    return this.take(id, parseScaled(this.raw(id), 8), required);
  }
  /** percent with two decimals -> basis points */
  bps(id: string): bigint | undefined {
    if (!this.has(id)) return undefined;
    return this.take(id, parseScaled(this.raw(id), 2), false);
  }
  /** seconds as a whole number */
  seconds(id: string): bigint | undefined {
    if (!this.has(id)) return undefined;
    return this.take(id, parseWhole(this.raw(id)), false);
  }
  /** minutes, up to two decimals -> seconds (rounded up) */
  minutes(id: string, required = false): bigint | undefined {
    if (!this.has(id) && !required) return undefined;
    const p = parseScaled(this.raw(id), 2);
    const secs = this.take(id, p, required);
    return secs === undefined ? undefined : (secs * 60n + 99n) / 100n;
  }
  /** DAA of a duration typed in seconds */
  daa(seconds: bigint | undefined): bigint | undefined {
    return seconds === undefined ? undefined : secondsToDaa({ daa: 0n, unixSeconds: 0n, rateMilli: this.ctx.rateMilli ?? 10_000 }, seconds);
  }
  date(id: string, required = false): bigint | undefined {
    if (!this.has(id) && !required) return undefined;
    return this.take(id, parseLocalDateTime(this.raw(id), this.ctx.tzOffsetMin ?? 0), required);
  }
  bool(id: string): boolean {
    return this.raw(id) === 'true';
  }
  /** trigger threshold in base units (`min`, a preset percent of `orderAmount`, or a token amount); empty or `min` = the wallet default */
  touch(id: string, orderAmount: bigint | undefined): bigint | undefined {
    if (!this.has(id)) return undefined;
    const s = this.raw(id);
    // a custom threshold of a `kas` form is a KAS amount (presets are the order's own minimum fill or a share of it)
    if (this.form.kas && s !== TOUCH_MIN && !/%$/.test(s)) {
      const n = this.take(id, amountBaseOf(this.form, this.ctx, id), false);
      if (n === 0n) {
        this.errors.push({ field: id, code: 'positive' });
        return undefined;
      }
      return n;
    }
    return this.take(id, parseTouch(s, orderAmount, this.ctx.decimals), false) ?? undefined;
  }
  pick<T extends string>(id: string, allowed: readonly T[]): T | undefined {
    const s = this.raw(id);
    return (allowed as readonly string[]).includes(s) ? (s as T) : undefined;
  }
  err(id: string, code: FieldErrorCode): void {
    this.errors.push({ field: id, code });
  }
}

const dropUndefined = <T extends object>(o: T): T => Object.fromEntries(Object.entries(o).filter(([, v]) => v !== undefined)) as T;

/** Parses the form into the intent of its order type. `intent` is null whenever `errors` is non-empty. */
export function buildIntent(form: TicketForm, ctx: TicketCtx): BuildResult {
  const r = new Reader(form, ctx);
  const side = form.side;
  const opp: Side = side === 'buy' ? 'sell' : 'buy';
  const type = form.type;

  const amount = type === 'close' ? r.amount('amount', false) : r.amount('amount');
  const tip = r.tipKas('tip');
  // empty = the wallet default (kob-wasm defaultMinFill / defaultMinFillIfd, 1 for immediate orders), shown in the disclosure
  const minFill = r.amount('minFill', false);

  const lifetimeSimple = (): Lifetime | undefined => {
    const k = r.pick('lifetime', ['gtc', 'gtd', 'day'] as const) ?? 'gtc';
    if (k === 'day') return { kind: 'day' };
    if (k === 'gtd') {
      const at = r.date('lifetimeAt', true);
      return at === undefined ? undefined : { kind: 'gtd', at };
    }
    return undefined;
  };
  const lifetimeCond = (): CondExpiry | undefined => {
    const k = r.pick('lifetime', ['gtc', 'gtd', 'day'] as const) ?? 'gtc';
    if (k === 'day') return { kind: 'day' };
    if (k === 'gtd') {
      const at = r.date('lifetimeAt', true);
      return at === undefined ? undefined : { kind: 'gtdUnix', atUnixSeconds: at };
    }
    return undefined;
  };
  const activeFrom = (): { unixSeconds: bigint } | undefined => {
    const t = r.date('activeFrom');
    return t === undefined ? undefined : { unixSeconds: t };
  };
  const sec = (id: string): { seconds: bigint } | undefined => {
    const s = r.seconds(id);
    return s === undefined ? undefined : { seconds: s };
  };
  const auctionOpts = () => ({
    slippageBps: r.bps('slippageBps'),
    auction: sec('auction'),
    life: sec('life'),
    ...(r.bool('allOrNothing') ? { allOrNothing: true } : {}),
  });
  // shared knobs of a stop leg; empty = the wallet default of matcher.md 10.6
  const stopKnobs = (p = '') => ({
    slipBps: undefined as number | undefined,
    bandDaa: r.daa(r.seconds(`${p}bandDaa`)),
    keeperTip: r.kas(`${p}keeperTip`),
    minTouch: r.touch(`${p}minTouch`, amount),
    minRestDaa: r.daa(r.seconds(`${p}minRestDaa`)),
  });
  const slip = (id: string): number | undefined => {
    const b = r.bps(id);
    if (b === undefined) return undefined;
    if (b > 10_000n) {
      r.err(id, 'range');
      return undefined;
    }
    return Number(b);
  };
  const condCommon = () => ({
    amount: amount as bigint,
    tip,
    minFill,
    expiry: lifetimeCond(),
    activeFrom: activeFrom(),
    carrier: r.kas('carrier'),
    ...(side === 'buy' ? { maxFills: r.positive('maxFills', false) } : {}),
  });
  // lifetime of the exits (no day order: the exit is created later); empty = good till cancelled
  const exitExpiry = (): Exclude<CondExpiry, { kind: 'day' }> | undefined => {
    if (r.pick('exit.lifetime', ['gtc', 'gtd'] as const) !== 'gtd') return undefined;
    const at = r.date('exit.lifetimeAt', true);
    return at === undefined ? undefined : { kind: 'gtdUnix', atUnixSeconds: at };
  };
  const trail = (p: string, waitId?: string, updatesId?: string): TrailSpec | undefined => {
    const step = r.delta(`${p}.step`, true, false, true);
    const gap = r.delta(`${p}.gap`, true, true);
    const wait = waitId ? r.daa(r.minutes(waitId)) : undefined;
    const expectedUpdates = updatesId ? r.whole(updatesId, false) : undefined;
    if (step === undefined || gap === undefined) return undefined;
    return dropUndefined({ step, gap, wait, expectedUpdates: expectedUpdates === undefined ? undefined : Number(expectedUpdates) });
  };

  let intent: Record<string, unknown> | null = null;
  switch (type) {
    case 'limit':
      intent = { type, side, amount, minFill, price: r.price('price'), tip, lifetime: lifetimeSimple(), activeFrom: activeFrom(), crossing: r.pick('crossing', ['auction', 'limit', 'reject'] as const), ...(side === 'buy' ? { maxFills: r.positive('maxFills', false) } : {}) };
      break;
    case 'ioc':
    case 'fok':
      intent = { type, side, amount, minFill, price: r.price('price'), tip, life: sec('life'), activeFrom: activeFrom() };
      break;
    case 'market':
      intent = { type, side, amount, minFill, tip, ...auctionOpts() };
      break;
    case 'streaming':
      intent = { type, side, amount, minFill, tip, displayedPrice: r.price('displayedPrice'), toleranceBps: r.bps('toleranceBps') ?? (r.err('toleranceBps', 'required'), undefined), ...auctionOpts(), slippageBps: undefined };
      break;
    case 'close':
      intent = { type, amount, tip, ...auctionOpts() };
      break;
    case 'twap':
    case 'dca': {
      const interval = r.minutes('interval', true);
      intent = {
        type, side, amount, minFill, tip, sliceAmount: r.amount('sliceAmount'), interval: interval === undefined ? undefined : { seconds: interval }, price: r.price('price'),
        priceEnd: r.price('priceEnd', side, false), sliceAuction: r.has('priceEnd') ? sec('sliceAuction') : undefined,
        lifetime: lifetimeSimple(), activeFrom: activeFrom(), ...(type === 'dca' ? { maxFills: r.positive('maxFills', false) } : {}),
      };
      break;
    }
    case 'dutch': {
      const duration = r.minutes('duration', true);
      intent = {
        type, side, amount, minFill, tip, price: r.price('price'), priceEnd: r.price('priceEnd'), duration: duration === undefined ? undefined : { seconds: duration },
        stepDaa: r.positive('stepDaa', false), lifetime: lifetimeSimple(), activeFrom: activeFrom(), ...(side === 'buy' ? { maxFills: r.positive('maxFills', false) } : {}),
      };
      break;
    }
    case 'stopMarket':
      intent = { type, side, ...condCommon(), stop: r.price('stop'), ...stopKnobs(), slipBps: slip('slipBps') };
      break;
    case 'stopLimit':
      intent = { type, side, ...condCommon(), stop: r.price('stop'), limit: r.price('limit'), ...stopKnobs(), slipBps: undefined };
      break;
    case 'trailingStop':
      intent = { type, side, ...condCommon(), stop: r.price('stop'), trail: trail('trail', 'trail.wait', 'trail.expectedUpdates'), takeProfit: r.price('takeProfit', side, false), ...stopKnobs(), slipBps: slip('slipBps') };
      break;
    case 'takeProfit':
      intent = { type, side, ...condCommon(), price: r.price('price') };
      break;
    case 'oco':
      intent = { type, side, ...condCommon(), takeProfit: r.price('takeProfit'), stop: r.price('stop'), limit: r.price('limit', side, false), ...stopKnobs(), slipBps: slip('slipBps') };
      break;
    case 'ifd':
    case 'ifo':
    case 'repeatIfd':
    case 'repeatIfo': {
      const withStop = type === 'ifo' || type === 'repeatIfo' || (type === 'ifd' && r.raw('exit.kind') === 'stop');
      const withTp = type !== 'ifd' || !withStop;
      const entry: EntrySpec = dropUndefined({
        price: r.price('price') as bigint,
        stop: r.price('entry.stop', side, false),
        bandDaa: r.daa(r.seconds('entry.bandDaa')),
        // the trigger rule of a stop entry (a limit entry has no trigger)
        ...(r.has('entry.stop') ? { minTouch: r.touch('entry.minTouch', amount), minRestDaa: r.daa(r.seconds('entry.minRestDaa')) } : {}),
      });
      const exit: ExitSpec = dropUndefined({
        takeProfit: withTp ? r.price('exit.takeProfit', opp) : undefined,
        stop: withStop ? r.price('exit.stop', opp) : undefined,
        stopLimit: withStop ? r.price('exit.stopLimit', opp, false) : undefined,
        slipBps: withStop && !r.has('exit.stopLimit') ? slip('exit.slipBps') : undefined,
        trail: withStop && r.bool('exit.trail') ? trail('exit.trail') : undefined,
        minTouch: withStop ? r.touch('exit.minTouch', amount) : undefined,
        minRestDaa: withStop ? r.daa(r.seconds('exit.minRestDaa')) : undefined,
        tip: r.tipKas('exit.tip'),
        minFill: r.amount('exit.minFill', false),
        expiry: exitExpiry(),
      });
      const cond = { ...condCommon() };
      intent = {
        type, side, ...cond, entry, exit, ...(side === 'sell' ? { prefund: r.delta('prefund') } : {}),
        ...(type === 'repeatIfd' || type === 'repeatIfo' ? { repeat: dropUndefined({ count: r.positive('repeat.count', false) }) } : {}),
      };
      break;
    }
  }

  if (type === 'repeatIfd' || type === 'repeatIfo') {
    const levels = r.whole('ladder.levels', false);
    if (levels !== undefined && (levels < 1n || levels > MAX_LADDER_LEVELS)) r.err('ladder.levels', 'range');
    else if (levels !== undefined && levels > 1n) r.delta('ladder.step', true, false, true);
  }

  if (r.errors.length > 0 || intent === null) return { intent: null, errors: r.errors };
  return { intent: dropUndefined(intent) as unknown as Intent, errors: [] };
}

// ------------------------------------------------------------------------------------------------ intent -> form

/** Inverse of `buildIntent`: the form that produces `intent`. Amounts render as exact decimals; durations in the unit of their field. */
export function formFromIntent(intent: Intent, ctx: TicketCtx): TicketForm {
  const tz = ctx.tzOffsetMin ?? 0;
  const rate = BigInt(ctx.rateMilli ?? 10_000);
  const side: Side = ('side' in intent && intent.side ? intent.side : 'sell') as Side;
  const form = initialForm(intent.type as OrderTypeId, side);
  const v: Record<string, string> = { ...form.values };
  const price = (id: string, x: bigint | undefined) => {
    if (x !== undefined) v[id] = formatPrice(x, ctx);
  };
  const kasTip = (id: string, x: bigint | undefined) => {
    if (x !== undefined) v[id] = formatPrice(x, ctx, 'kas');
  };
  const put = (id: string, x: string | bigint | undefined) => {
    if (x !== undefined) v[id] = String(x);
  };
  const secs = (id: string, d: { daa: bigint } | { seconds: bigint } | undefined) => {
    if (!d) return;
    v[id] = 'seconds' in d ? String(d.seconds) : String((d.daa * 1000n + rate - 1n) / rate);
  };
  const daaSecs = (id: string, daa: bigint | undefined) => {
    if (daa !== undefined) v[id] = String((daa * 1000n + rate - 1n) / rate);
  };
  const mins = (id: string, seconds: bigint | undefined) => {
    if (seconds !== undefined) v[id] = formatUnits((seconds * 100n) / 60n, 2);
  };
  const life = (l: Lifetime | undefined) => {
    if (!l) return;
    put('lifetime', l.kind);
    if (l.kind === 'gtd') v.lifetimeAt = formatLocalDateTime(l.at, tz);
  };
  const expiry = (e: CondExpiry | undefined) => {
    if (!e) return;
    if (e.kind === 'gtdUnix') {
      v.lifetime = 'gtd';
      v.lifetimeAt = formatLocalDateTime(e.atUnixSeconds, tz);
    } else if (e.kind === 'gtc' || e.kind === 'day') v.lifetime = e.kind;
  };
  const from = (a: { unixSeconds: bigint } | { daa: bigint } | undefined) => {
    if (a && 'unixSeconds' in a) v.activeFrom = formatLocalDateTime(a.unixSeconds, tz);
  };
  const bps = (id: string, x: bigint | number | undefined) => {
    if (x !== undefined) v[id] = formatPercent(BigInt(x));
  };
  const amount = (id: string, x: bigint | undefined) => {
    if (x !== undefined) v[id] = formatAmount(x, ctx.decimals);
  };
  const touch = (id: string, x: bigint | undefined) => {
    if (x !== undefined) v[id] = formatAmount(x, ctx.decimals);
  };
  const i = intent as unknown as Record<string, any>;
  amount('amount', i.amount);
  amount('minFill', i.minFill);
  if (i.tip !== undefined) kasTip('tip', i.tip);
  switch (intent.type) {
    case 'limit': price('price', i.price); life(i.lifetime); from(i.activeFrom); put('crossing', i.crossing); put('maxFills', i.maxFills); break;
    case 'ioc': case 'fok': price('price', i.price); secs('life', i.life); from(i.activeFrom); break;
    case 'market': case 'close':
      bps('slippageBps', i.slippageBps); secs('auction', i.auction); secs('life', i.life); if (i.allOrNothing) v.allOrNothing = 'true'; break;
    case 'streaming':
      price('displayedPrice', i.displayedPrice); bps('toleranceBps', i.toleranceBps); secs('auction', i.auction); secs('life', i.life); if (i.allOrNothing) v.allOrNothing = 'true'; break;
    case 'twap': case 'dca':
      amount('sliceAmount', i.sliceAmount); price('price', i.price); price('priceEnd', i.priceEnd); secs('sliceAuction', i.sliceAuction);
      if (i.interval) mins('interval', BigInt('seconds' in i.interval ? i.interval.seconds : (i.interval.daa * 1000n) / rate)); life(i.lifetime); from(i.activeFrom); put('maxFills', i.maxFills); break;
    case 'dutch':
      price('price', i.price); price('priceEnd', i.priceEnd); put('stepDaa', i.stepDaa); life(i.lifetime); from(i.activeFrom); put('maxFills', i.maxFills);
      if (i.duration) mins('duration', BigInt('seconds' in i.duration ? i.duration.seconds : (i.duration.daa * 1000n) / rate)); break;
    default: {
      // conditional family
      expiry(i.expiry); from(i.activeFrom); if (i.carrier !== undefined) v.carrier = formatUnits(i.carrier, 8); put('maxFills', i.maxFills);
      if (i.stop !== undefined) price('stop', i.stop);
      if (i.limit !== undefined) price('limit', i.limit);
      if (intent.type === 'takeProfit') price('price', i.price);
      if (i.takeProfit !== undefined) price('takeProfit', i.takeProfit);
      bps('slipBps', i.slipBps); daaSecs('bandDaa', i.bandDaa); daaSecs('minRestDaa', i.minRestDaa); touch('minTouch', i.minTouch);
      if (i.keeperTip !== undefined) v.keeperTip = formatUnits(i.keeperTip, 8);
      if (i.trail) {
        price('trail.step', i.trail.step); price('trail.gap', i.trail.gap);
        if (i.trail.wait !== undefined) mins('trail.wait', (i.trail.wait * 1000n) / rate);
        put('trail.expectedUpdates', i.trail.expectedUpdates);
      }
      if (i.entry) {
        price('price', i.entry.price); price('entry.stop', i.entry.stop); daaSecs('entry.bandDaa', i.entry.bandDaa);
        daaSecs('entry.minRestDaa', i.entry.minRestDaa); touch('entry.minTouch', i.entry.minTouch);
        const x = i.exit as ExitSpec;
        price('exit.takeProfit', x.takeProfit); price('exit.stop', x.stop); price('exit.stopLimit', x.stopLimit); bps('exit.slipBps', x.slipBps); kasTip('exit.tip', x.tip);
        daaSecs('exit.minRestDaa', x.minRestDaa); touch('exit.minTouch', x.minTouch); amount('exit.minFill', x.minFill);
        if (x.expiry?.kind === 'gtdUnix') {
          v['exit.lifetime'] = 'gtd';
          v['exit.lifetimeAt'] = formatLocalDateTime(x.expiry.atUnixSeconds, tz);
        }
        if (intent.type === 'ifd') v['exit.kind'] = x.stop !== undefined ? 'stop' : 'takeProfit';
        if (x.trail) {
          v['exit.trail'] = 'true';
          price('exit.trail.step', x.trail.step);
          price('exit.trail.gap', x.trail.gap);
        }
        price('prefund', i.prefund);
        if (i.repeat?.count !== undefined) put('repeat.count', i.repeat.count);
      }
    }
  }
  return { type: form.type, side, values: v };
}

// ------------------------------------------------------------------------------------------------ helpers for the component

/** Per-field summary for display under an input: the exact state price after rounding and whether rounding happened. */
export function priceInfo(text: string, side: Side, ctx: Pick<TicketCtx, 'decimals' | 'scale' | 'tick' | 'quoteDecimals'>): { price: bigint; perToken: string; rounded: boolean } | null {
  const p = parsePrice(text, side, ctx);
  return p.ok ? { price: p.value.price, perToken: formatPrice(p.value.price, ctx), rounded: p.value.rounded } : null;
}

export interface MaxAmountInput {
  side: Side;
  /** free (P2PK-owned) tokens, base units */
  tokenBalance: bigint;
  /** spendable KAS, sompi */
  kasBalance: bigint;
  /** the order scale: base units per whole token */
  scale: bigint;
  /** limit price incl. tip, sompi per whole token (all-in of a buy); 0 or absent = unknown */
  allInPrice?: bigint;
  /** carrier KAS per covenant UTXO */
  carrier: bigint;
}

/**
 * The most base units the balance can plausibly place (the planner does the exact funding check). Sell: the free token balance. Buy: the KAS
 * balance minus a reserve for carriers (delivery carriers and the network fee) at the all-in price, `floor(kas * scale / allIn)`.
 */
export function maxAmount(i: MaxAmountInput): bigint {
  if (i.side === 'sell') return i.tokenBalance;
  if (!i.allInPrice || i.allInPrice <= 0n) return 0n;
  const reserve = i.carrier * 4n + 20_000_000n;
  return i.kasBalance > reserve ? ((i.kasBalance - reserve) * i.scale) / i.allInPrice : 0n;
}

/** Errors worth showing under a field right away (the user typed something wrong) versus fields that are merely still empty. */
export const visibleErrors = (errors: readonly FieldError[]): FieldError[] => errors.filter((e) => e.code !== 'required');
export const isIncomplete = (errors: readonly FieldError[]): boolean => errors.some((e) => e.code === 'required');

// ------------------------------------------------------------------------------------------------ KAS amounts (a KAS market shown inverted)
//
// KAS/TOKEN is the token's own TOKEN/KAS book seen the other way round: its base, the left asset, is KAS. A `kas` form counts every amount in
// KAS like any BASE/QUOTE market counts the base, and the order is still the native one: buying KAS is selling the token, and X KAS is
// floor(X x scale / P) base units of the token at the price P of the amount's own leg (the limit price; the stop of a stop leg; the exit price of
// an exit; the book's best price for an order without a price of its own, market and close). Rounding down means the KAS value of the order
// never exceeds what was typed: a shown Sell spends at most the KAS typed (plus the tip), a shown Buy receives at most that (minus the tip).

/** Key suffix of an exact base amount kept for an amount box of a `kas` form (the box then shows that amount's KAS value). */
export const BASE_SUFFIX = '@base';

/** The amount-like fields: typed in KAS in a `kas` form (a `touch` field only for a custom threshold). */
export const AMOUNT_FIELD_IDS: readonly string[] = ['amount', 'minFill', 'sliceAmount', 'exit.minFill', 'minTouch', 'entry.minTouch', 'exit.minTouch'];

/** Where the conversion price of an amount comes from: the order's own price, a stop trigger, an exit price, or the book. */
export type AmountBasis = 'own' | 'stop' | 'exit' | 'book';

/** The price field of an order type that sizes it (its limit, else its trigger); null = no price of its own (market, close: the book's price). */
export function primaryPriceField(type: OrderTypeId): string | null {
  switch (type) {
    case 'market':
    case 'close': return null;
    case 'streaming': return 'displayedPrice';
    case 'stopLimit': return 'limit';
    case 'stopMarket':
    case 'trailingStop': return 'stop';
    case 'oco': return 'takeProfit';
    default: return 'price';
  }
}

/**
 * The state price (sompi per `scale` base units) a KAS amount of field `id` converts at, with where it comes from; null while that price is
 * missing or malformed (the price box reports its own problem) or, for an order without a price, while the book has none.
 */
export function amountPriceOf(form: TicketForm, ctx: TicketCtx, id: string): { price: bigint; basis: AmountBasis } | null {
  const opp: Side = form.side === 'buy' ? 'sell' : 'buy';
  const v = form.values;
  const read = (field: string, side: Side, basis: AmountBasis) => {
    const p = parsePrice(v[field] ?? '', side, ctx);
    return p.ok ? { price: p.value.price, basis } : null;
  };
  if (id === 'exit.minFill' || id === 'exit.minTouch') {
    // the exit trades the other way, at its own price: the take-profit, else the stop (an exit stop's threshold: the stop)
    if (id === 'exit.minFill' && (v['exit.takeProfit'] ?? '').trim() !== '') return read('exit.takeProfit', opp, 'exit');
    return read('exit.stop', opp, 'exit');
  }
  if (id === 'minTouch') return read('stop', form.side, 'stop');
  if (id === 'entry.minTouch') return read('entry.stop', form.side, 'stop');
  const field = primaryPriceField(form.type);
  if (field === null) {
    const p = form.side === 'buy' ? ctx.ref?.buy : ctx.ref?.sell;
    return p && p > 0n ? { price: p, basis: 'book' } : null;
  }
  return read(field, form.side, field === 'stop' ? 'stop' : 'own');
}

/** KAS (sompi) -> base units of the token at a state price, rounded down. */
export const kasToBase = (sompi: bigint, price: bigint, scale: bigint): bigint => (price > 0n ? (sompi * scale) / price : 0n);
/** Base units -> their KAS value (sompi) at a state price, rounded down. */
export const baseToKas = (base: bigint, price: bigint, scale: bigint): bigint => (scale > 0n ? (base * price) / scale : 0n);

/**
 * The base units of amount field `id`: the token amount typed (native form); in a `kas` form the exact amount kept for the box, else the KAS
 * typed converted at the field's price. `noRef` when an order without a price has no book price to convert at.
 */
export function amountBaseOf(form: TicketForm, ctx: TicketCtx, id: string): Parsed<bigint> {
  if (!form.kas) return parseAmount(form.values[id] ?? '', ctx.decimals);
  const kept = form.values[id + BASE_SUFFIX];
  if (kept !== undefined && /^\d+$/.test(kept)) return good(BigInt(kept));
  const sompi = parseScaled(form.values[id] ?? '', 8);
  if (!sompi.ok) return sompi;
  const p = amountPriceOf(form, ctx, id);
  if (!p) return primaryPriceField(form.type) === null && !id.includes('.') && id !== 'minTouch' ? bad('noRef') : bad('required');
  return good(kasToBase(sompi.value, p.price, ctx.scale));
}

/** The KAS text of `base` units of the token at the field's price (rounded down to the sompi); '' when there is no price to value it at. */
export function kasTextOf(form: TicketForm, ctx: TicketCtx, id: string, base: bigint): string {
  const p = amountPriceOf(form, ctx, id);
  return p ? formatUnits(baseToKas(base, p.price, ctx.scale), 8) : '';
}

/**
 * Sets amount field `id` to exactly `base` units of the token (Max, a prefilled amount, a flip of a filled form): a token form shows them, a
 * `kas` form keeps them exactly and shows their KAS value, so the order does not move by a KAS rounding.
 */
export function setAmountBase(form: TicketForm, id: string, base: bigint, ctx: TicketCtx): TicketForm {
  if (!form.kas) return setValue(form, id, formatAmount(base, ctx.decimals));
  return { ...form, values: { ...form.values, [id]: kasTextOf(form, ctx, id, base), [id + BASE_SUFFIX]: base.toString() } };
}

/**
 * The same order with its amount boxes in the other unit (`kas` true: KAS; false: token units). Every amount that parses keeps its exact base
 * units (a box that does not parse is emptied, a preset threshold kept), so a flip never changes the order that is built.
 */
export function reorientAmounts(form: TicketForm, kas: boolean, ctx: TicketCtx): TicketForm {
  if (!!form.kas === kas) return form;
  let next: TicketForm = { ...form, values: { ...form.values } };
  if (kas) next.kas = true;
  else delete next.kas;
  for (const id of AMOUNT_FIELD_IDS) {
    const text = (form.values[id] ?? '').trim();
    const kept = form.values[id + BASE_SUFFIX];
    if (text === '' && kept === undefined) continue;
    if (FIELDS[id]?.kind === 'touch' && (text === TOUCH_MIN || text.endsWith('%'))) continue;
    const base = amountBaseOf(form, ctx, id);
    delete next.values[id + BASE_SUFFIX];
    if (!base.ok) {
      next.values[id] = '';
      continue;
    }
    next = setAmountBase(next, id, base.value, ctx);
  }
  return next;
}

/** The state price an intent is sized at (the counterpart of `primaryPriceField`); null for an order without a price (market, close). */
export function intentSizingPrice(intent: Intent): bigint | null {
  const i = intent as unknown as Record<string, any>;
  const p = i.entry?.price ?? (intent.type === 'streaming' ? i.displayedPrice : intent.type === 'stopLimit' ? i.limit : intent.type === 'stopMarket' || intent.type === 'trailingStop' ? i.stop : intent.type === 'oco' ? i.takeProfit : i.price);
  return typeof p === 'bigint' ? p : null;
}
