// Order-ticket form state <-> Intent mapping. Pure: no DOM, no network, runs in node tests.
//
// The ticket keeps every input as text (`values`, keyed by the intent path of the field, such as
// `price`, `stop`, `trail.step` or `exit.takeProfit`). What the user typed is never rewritten under
// their fingers. `buildIntent` parses the text into the bigint intents of kob/intent-simple.ts and
// kob/intent-cond.ts and reports per-field problems. `formFromIntent` is the inverse, used for
// prefill, cancel-replace and tests.
//
// Units the user types. All are converted here and nowhere else.
// - Amounts (`amount`, `minFill`, `sliceAmount`, `exit.minFill`) are token units with the token's
//   decimals, converted exactly to base units. More decimals than the token has is an error, never
//   a rounding.
// - Prices and price distances are the quote per whole token. They convert to the state price
//   (quote base units per whole token of `scale` base units, the identity for the wallet's scale
//   10^decimals) and round onto the tick in the direction that never makes the maker's limit
//   worse: a sell rounds up and a buy rounds down.
// - The quote is KAS (8 decimals, sompi) on a token's KAS market. It is token B (its decimals, base
//   units of B) on a token/token pair A/B (`TicketCtx.quoteDecimals`). The pair ticket offers
//   exactly the order types of the KAS ticket.
// - Tips are always KAS per whole token, also on a pair, because tips, keeper tips and carriers are
//   KAS. They are in sompi per whole token and round to nearest, since a tip is not a limit.
// - Slippage and tolerance are percent (two decimals), converted to basis points. Durations are
//   seconds or minutes, converted to `Duration` or DAA.
// - A trigger threshold (`touch` fields) is `min` (the order's minimum fill, the default), a preset
//   percent of the order's own amount (`25%`, `50%`, `100%`, rounded up to whole base units) or a
//   custom token amount.
//
// Protocol rules (tick, 90-day limits, stop above take-profit) are the planner's job (kob/plan.ts).
// This layer only refuses text that cannot become a number. It leaves everything optional
// undefined so the planner applies the wallet defaults of matcher.md section 10.
import type { Intent } from '../../kob/plan';
import type { CondExpiry, EntrySpec, ExitSpec, TrailSpec } from '../../kob/intent-cond';
import type { Lifetime } from '../../kob/intent-simple';
import { SLIPPAGE_BPS, secondsToDaa } from '../../kob/daa';
import { stopWorstPrice } from '../../kob/orders/cond-legs';
import { auctionBound } from '../../kob/orders/simple';
import { DELIVERY_CHOICES, isDeliveryChoice, type DeliveryChoice, type DeliveryRequest } from '../../kob/orders/delivery';
import {
  UnitsError, fitsI64, formatPricePerToken, formatUnits, parseKas, parseUnits, pow10, roundToTick, safeRounding, tokenPriceToStatePrice, type Rounding,
} from '../../kob/units';

/** Most levels of a repeat ladder. Each level costs one transaction and one signature. */
export const MAX_LADDER_LEVELS = 20n;
/** A limit ladder places 2 to 20 limit orders. */
export const MIN_LIMIT_LADDER_LEVELS = 2n;
export const MAX_LIMIT_LADDER_LEVELS = 20n;

/**
 * Trigger-threshold presets of the ticket (matcher.md 10.6). `min` is the order's minimum fill, the
 * wallet default. The others are a percent of the order's own amount. Any other text is a custom
 * token amount.
 */
export const TOUCH_PRESETS = ['min', '25%', '50%', '100%'] as const;
export const TOUCH_MIN = 'min';

/**
 * Threshold in base units of a `touch` field text. `min` gives null, meaning the wallet default,
 * the order's minimum fill. `N%` is that percent of `orderAmount`, rounded up to at least one base
 * unit. Any other text is a token amount with `decimals`.
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
  // No order size yet. The amount field reports its own error, so there is nothing to resolve.
  if (orderAmount === undefined || orderAmount <= 0n) return bad('required');
  const amount = (orderAmount * pct + 99n) / 100n;
  return good(amount < 1n ? 1n : amount);
}

// ------------------------------------------------------------------------------- vocabulary

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

/** Sides an order type can be placed on. Close and TWAP only sell and DCA only buys. */
export function sidesOf(type: OrderTypeId): readonly Side[] {
  return type === 'close' || type === 'twap' ? ['sell'] : type === 'dca' ? ['buy'] : ['buy', 'sell'];
}

export const isIfdType = (t: OrderTypeId): boolean => t === 'ifd' || t === 'ifo' || t === 'repeatIfd' || t === 'repeatIfo';
export const isCondType = (t: OrderTypeId): boolean => ['stopMarket', 'stopLimit', 'trailingStop', 'takeProfit', 'oco'].includes(t) || isIfdType(t);

/** `amount` is a token amount typed in whole-token units, stored as base units. */
export type FieldKind = 'amount' | 'int' | 'price' | 'delta' | 'tip' | 'kas' | 'percent' | 'seconds' | 'minutes' | 'datetime' | 'select' | 'bool' | 'touch';

export interface FieldSpec {
  /** intent path of the field, key of `values` */
  id: string;
  kind: FieldKind;
  /** The `data-testid` of the input: order-amount, order-price, order-tip, else field-<path>. */
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
    F('crossing', 'select', ['take', 'auction', 'limit', 'reject']), F('maxFills', 'int'), F('direct', 'select', ['take', 'order']), F('tipAuto', 'bool'),
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
    F('ladder.levels', 'int'), F('ladder.step', 'delta'), F('ladder.on', 'bool'), F('ladder.pct', 'percent'), F('ladder.end', 'price'),
    F('prefund', 'delta'), F('repeat.count', 'int'),
    // Where a buy's tokens go (orders/delivery.ts). The choices are Each fill, Collected and
    // Top-up with its tip per fill in KAS.
    F('delivery', 'select', DELIVERY_CHOICES), F('topupTip', 'kas'),
  ].map((f) => [f.id, f]),
);

/** Wallet defaults of matcher.md section 10 and defaults.rs, shown as the prefilled value. */
export const FIELD_DEFAULTS: Readonly<Record<string, string>> = {
  lifetime: 'gtc',
  // A crossing limit fills from the book from the wallet (kob/take.ts). A pair order has no such
  // fill and uses its auction. The `direct` default of an IOC, FOK or market order depends on its
  // type, see `fieldDefault`.
  crossing: 'take',
  life: '30',
  slippageBps: '3',
  auction: '20',
  toleranceBps: '3',
  slipBps: '3',
  bandDaa: '30',
  'entry.bandDaa': '30',
  // Trigger (touch rule, matcher.md section 4 and 10.6). The threshold is the order's minimum fill,
  // with presets of 25%, 50% and 100% of the order or a custom amount. R is 5 s.
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

// --------------------------------------------------------------------------------------- state

export interface TicketForm {
  type: OrderTypeId;
  side: Side;
  /** Text of every field, by intent path. Absent means empty. */
  values: Record<string, string>;
  /**
   * The amount boxes count KAS. That is a token's KAS market shown inverted as KAS/TOKEN, where the
   * shown pair's base is KAS. Every amount field (`amount`, `minFill`, `sliceAmount`,
   * `exit.minFill`, a custom trigger threshold) is then typed in KAS and converted to the token's
   * base units at the price of its own leg (`amountPriceOf`). Prices and side stay native in the
   * form. Absent or false means token units, the native convention.
   */
  kas?: boolean;
}

export interface TicketCtx {
  /**
   * The (base) token's decimals, order scale and tick. The scale is base units per whole token,
   * `10^decimals` capped at 10^9. The tick is in quote units per whole token, and is 1.
   */
  decimals: number;
  scale: bigint;
  tick: bigint;
  /**
   * Decimals of the quote the prices are typed in. It is 8 for KAS (the default) or a pair's quote
   * token B (prices in B base units per whole A). Set only on a token/token pair ticket, where its
   * presence makes the ticket a pair ticket (`isPairCtx`).
   */
  quoteDecimals?: number;
  /** Minutes west of UTC as `Date.getTimezoneOffset()`, 0 for UTC. Text is wall clock. */
  tzOffsetMin?: number;
  /** Measured DAA rate in milli-DAA per second. The default is 10 000, which is 10 DAA/s. */
  rateMilli?: number;
  /**
   * The book's best prices (state prices, sompi per `scale` base units). A KAS amount of an order
   * without its own price (market, close) is converted at this price, in `kas` forms only. `buy`
   * is the best ask, which is what a buy meets. `sell` is the best bid.
   */
  ref?: { buy?: bigint | null; sell?: bigint | null };
  /**
   * The family of the token's program. A KRON token's buys deliver at each fill, so its ticket has
   * no Delivery choice. Absent means KCC-20.
   */
  family?: 'kcc20' | 'kron';
  /**
   * A token pair: the family of the quote token B, which a pair sell receives. A KRON B delivers at
   * each fill. Absent means KCC-20.
   */
  quoteFamily?: 'kcc20' | 'kron';
  /**
   * The wallet's delivery settings (orders/delivery.ts). `walletDefault` is the default of
   * Settings, absent meaning Each fill. `threshold` is the receive threshold this wallet stored for
   * the token, for Top-up. When it is absent the planner sets it from the price at the first top-up
   * order.
   */
  delivery?: { walletDefault?: DeliveryChoice; threshold?: bigint };
}

/**
 * The order types whose buys rest and deliver (KobBid, KobCondBid) and so choose their delivery.
 * The sell side of an if-done buys back.
 */
const DELIVERY_BUY_TYPES: readonly OrderTypeId[] = ['limit', 'dca', 'dutch', 'stopMarket', 'stopLimit', 'trailingStop', 'takeProfit', 'oco'];

/** The order types of a token pair whose sells rest and deliver token B (the pair asks). */
const DELIVERY_PAIR_SELL_TYPES: readonly OrderTypeId[] = ['limit', 'twap', 'dutch', 'stopMarket', 'stopLimit', 'trailingStop', 'takeProfit', 'oco'];

/**
 * Whether the form's order chooses its delivery (bid-delivery-spec section 10). These choose:
 * - a resting buy of a KCC-20 token, also on a token pair, where a pair bid buys A;
 * - a resting sell of a token pair whose B is KCC-20, which receives B;
 * - the buy-back exits of a sell-first IFD or IFO of a KAS market.
 * IOC, FOK, market orders, KRON tokens, repeating entries and the exits of pair if-done orders
 * deliver at each fill, so they have no choice.
 */
export function hasDeliveryChoice(form: Pick<TicketForm, 'type' | 'side'>, ctx?: Pick<TicketCtx, 'quoteDecimals' | 'family' | 'quoteFamily'>): boolean {
  if (isPairCtx(ctx) && form.side === 'sell') return ctx?.quoteFamily !== 'kron' && DELIVERY_PAIR_SELL_TYPES.includes(form.type);
  if (ctx?.family === 'kron') return false;
  if (form.type === 'ifd' || form.type === 'ifo') return !isPairCtx(ctx) && form.side === 'sell';
  return form.side === 'buy' && DELIVERY_BUY_TYPES.includes(form.type);
}

/**
 * The choices the ticket offers. A token pair has no "Added to one UTXO", because the spec defines
 * no threshold price for a pair order.
 */
export const deliveryChoicesOf = (ctx?: Pick<TicketCtx, 'quoteDecimals'>): readonly DeliveryChoice[] => (isPairCtx(ctx) ? DELIVERY_CHOICES.filter((c) => c !== 'topup') : DELIVERY_CHOICES);

/**
 * The delivery the form's order gets. It is the choice under Advanced, else the wallet default,
 * else Each fill. A choice the ticket does not offer (Added to one UTXO on a pair) becomes Each
 * fill. It is null when the order has no choice.
 */
export function deliveryOf(form: Pick<TicketForm, 'type' | 'side' | 'values'>, ctx?: Pick<TicketCtx, 'quoteDecimals' | 'family' | 'quoteFamily' | 'delivery'>): DeliveryChoice | null {
  if (!hasDeliveryChoice(form, ctx)) return null;
  const v = form.values.delivery;
  const c = isDeliveryChoice(v) ? v : ctx?.delivery?.walletDefault ?? 'each';
  return deliveryChoicesOf(ctx).includes(c) ? c : 'each';
}

/**
 * A token/token pair ticket, where the quote is a token B. A pair sell delivers B per fill, so it
 * budgets its fills (`maxFills`) like a buy.
 */
export const isPairCtx = (ctx: Pick<TicketCtx, 'quoteDecimals'> | undefined): boolean => ctx?.quoteDecimals !== undefined;

/** Whether an order of `side` budgets its delivery carriers (`maxFills`): a buy or a pair sell. */
const budgetsFills = (side: Side, ctx: Pick<TicketCtx, 'quoteDecimals'> | undefined): boolean => side === 'buy' || isPairCtx(ctx);

/**
 * Whether a market order has the Fills box (`maxFills`). Its default of 1 is one IOC transaction,
 * and more is a short-lived multi-fill auction (orders/simple.ts `planAuction`). Both sides of a
 * KAS market have it. It is absent with "all or nothing", because a FOK is one transaction, and on
 * a token pair, whose market orders stay one transaction.
 */
const marketFills = (form: Pick<TicketForm, 'values'>, ctx: Pick<TicketCtx, 'quoteDecimals'> | undefined): boolean => !isPairCtx(ctx) && form.values.allOrNothing !== 'true';

/**
 * The default of a field for an order type. An IOC or market order goes to the executors and a FOK
 * fills from the book.
 */
export function fieldDefault(id: string, type: OrderTypeId): string | undefined {
  if (id === 'direct') return type === 'fok' ? 'take' : 'order';
  return FIELD_DEFAULTS[id];
}

export function initialForm(type: OrderTypeId = 'limit', side: Side = 'sell', kas = false): TicketForm {
  const sides = sidesOf(type);
  const values: Record<string, string> = {};
  for (const id of new Set([...layoutOf({ type, side, values: {} }).main, ...layoutOf({ type, side, values: {} }).advanced])) {
    const d = fieldDefault(id, type);
    if (d !== undefined) values[id] = d;
  }
  return { type, side: sides.includes(side) ? side : sides[0]!, values, ...(kas ? { kas: true } : {}) };
}

/**
 * The fields a type or side change keeps are the amount (with an exact base amount kept for its
 * box) and the order's own price, when the new form has one. Everything else starts again from the
 * new form's defaults. That covers stops, exits, tip, lifetime, ladder and advanced options. A
 * value typed for one type or side means something else, or nothing, on another and would be
 * carried out of sight.
 */
const CARRIED = ['amount', 'price'];

function carryShared(from: TicketForm, next: TicketForm): TicketForm {
  const shown = layoutOf(next);
  const has = new Set([...shown.main, ...shown.advanced]);
  const values = { ...next.values };
  for (const id of CARRIED) {
    if (!has.has(id)) continue;
    for (const k of [id, id + BASE_SUFFIX]) {
      const v = from.values[k];
      if (v !== undefined && v !== '') values[k] = v;
    }
  }
  return { ...next, values };
}

/**
 * Switching the type keeps the amount and the price when the new type has one. Every other field
 * starts from its default, and an impossible side is fixed.
 */
export function switchType(form: TicketForm, type: OrderTypeId): TicketForm {
  if (type === form.type) return form;
  const side = sidesOf(type).includes(form.side) ? form.side : sidesOf(type)[0]!;
  return carryShared(form, initialForm(type, side, !!form.kas));
}

/** Switching the side keeps the amount and the price. Every other field starts from its default. */
export function setSide(form: TicketForm, side: Side): TicketForm {
  if (side === form.side || !sidesOf(form.type).includes(side)) return form;
  return carryShared(form, initialForm(form.type, side, !!form.kas));
}

/**
 * What the user typed into a box. An exact base amount kept for that box (`setAmountBase`) is
 * dropped, because the typed text is the amount now.
 */
export function setValue(form: TicketForm, id: string, value: string): TicketForm {
  const values = { ...form.values, [id]: value };
  delete values[id + BASE_SUFFIX];
  return { ...form, values };
}

// ----------------------------------------------------------------------------------- layout

export interface Layout {
  /** Always shown. */
  main: string[];
  /** Behind the "advanced" disclosure. */
  advanced: string[];
}

const LIFE = ['lifetime', 'lifetimeAt'];
/** The fields of a limit ladder, shown while the Ladder switch is on. */
export const LIMIT_LADDER = ['ladder.levels', 'ladder.pct', 'ladder.end'];
const STOP_KNOBS = ['minTouch', 'minRestDaa', 'bandDaa', 'keeperTip'];
/** Trigger knobs of a stop entry or an exit stop (touch rule), shown for every trigger kind. */
const ENTRY_TRIGGER = ['entry.minTouch', 'entry.minRestDaa'];
const EXIT_TRIGGER = ['exit.minTouch', 'exit.minRestDaa'];

/**
 * Which fields the form shows for its type, side and current selections. Conditional fields appear
 * when their switch is on. `ctx` tells a pair ticket (`isPairCtx`), whose sells budget their fills
 * too.
 */
export function layoutOf(form: TicketForm, ctx?: Pick<TicketCtx, 'quoteDecimals' | 'family' | 'quoteFamily' | 'delivery'>): Layout {
  const out = layoutOfType(form, ctx);
  const delivery = deliveryOf(form, ctx);
  if (delivery === null) return out;
  // Delivery is the row after the priority tip, and Top-up adds its tip. A Collected order is
  // funded with one carrier, so it has no Fills box.
  const adv = out.advanced.filter((id) => !(delivery === 'collected' && id === 'maxFills'));
  const at = adv.indexOf('tip') + 1;
  adv.splice(at, 0, 'delivery', ...(delivery === 'topup' ? ['topupTip'] : []));
  return { main: out.main, advanced: adv };
}

function layoutOfType(form: TicketForm, ctx?: Pick<TicketCtx, 'quoteDecimals'>): Layout {
  const v = form.values;
  const buy = form.side === 'buy';
  const life = (): string[] => (v.lifetime === 'gtd' ? LIFE : ['lifetime']);
  const fills = budgetsFills(form.side, ctx) ? ['maxFills'] : [];
  /** How long the exits of an if-done order live. GTC is 90 days idle from each exit's creation. */
  const exitLife = (): string[] => (v['exit.lifetime'] === 'gtd' ? ['exit.lifetime', 'exit.lifetimeAt'] : ['exit.lifetime']);
  /** The step of a ladder is asked once there is more than one level. */
  const ladder = (): string[] => (Number((v['ladder.levels'] ?? '').trim()) > 1 ? ['ladder.levels', 'ladder.step'] : ['ladder.levels']);
  // The priority tip is the first advanced option of every type, and its default is fine for most
  // orders. The minimum fill is an advanced option of every type. Its default is shown in the
  // disclosure, and is 1 base unit for IOC, FOK and market orders.
  // On a KAS market the tip can be the executors' (kob/fill-tip.ts), and an immediate order fills
  // from the book directly (kob/take.ts).
  const kas = !isPairCtx(ctx);
  const tip = kas ? ['tip', 'tipAuto'] : ['tip'];
  const direct = kas ? ['direct'] : [];
  switch (form.type) {
    case 'limit': return { main: ['amount', 'price', 'ladder.on', ...(v['ladder.on'] === 'true' ? LIMIT_LADDER : []), ...life()], advanced: [...tip, 'minFill', 'activeFrom', 'crossing', ...fills] };
    case 'ioc':
    case 'fok': return { main: ['amount', 'price'], advanced: [...tip, 'minFill', 'life', 'activeFrom', ...direct] };
    case 'market': return { main: ['amount', 'slippageBps'], advanced: [...tip, 'minFill', ...(marketFills(form, ctx) ? ['maxFills'] : []), 'auction', 'life', 'allOrNothing', ...direct] };
    case 'streaming': return { main: ['amount', 'displayedPrice', 'toleranceBps'], advanced: ['tip', 'minFill', 'allOrNothing', 'auction', 'life'] };
    case 'close': return { main: ['amount'], advanced: ['tip', 'slippageBps', 'auction', 'life', 'allOrNothing'] };
    case 'twap':
    case 'dca': return { main: ['amount', 'sliceAmount', 'interval', 'price'], advanced: [...tip, 'minFill', 'priceEnd', 'sliceAuction', ...life(), 'activeFrom', ...(form.type === 'dca' ? ['maxFills'] : [])] };
    case 'dutch': return { main: ['amount', 'price', 'priceEnd', 'duration'], advanced: [...tip, 'minFill', 'stepDaa', ...life(), 'activeFrom', ...fills] };
    case 'stopMarket': return { main: ['amount', 'stop'], advanced: ['tip', 'minFill', 'slipBps', ...STOP_KNOBS, ...life(), 'activeFrom', 'carrier', ...fills] };
    case 'stopLimit': return { main: ['amount', 'stop', 'limit'], advanced: ['tip', 'minFill', ...STOP_KNOBS, ...life(), 'activeFrom', 'carrier', ...fills] };
    case 'trailingStop':
      return { main: ['amount', 'stop', 'trail.step', 'trail.gap'], advanced: ['tip', 'minFill', 'takeProfit', 'trail.wait', 'trail.expectedUpdates', 'slipBps', ...STOP_KNOBS, ...life(), 'activeFrom', 'carrier', ...fills] };
    case 'takeProfit': return { main: ['amount', 'price', ...life()], advanced: ['tip', 'minFill', 'activeFrom', 'carrier', ...fills] };
    case 'oco': return { main: ['amount', 'takeProfit', 'stop'], advanced: ['tip', 'minFill', 'limit', 'slipBps', ...STOP_KNOBS, ...life(), 'activeFrom', 'carrier', ...fills] };
    case 'ifd':
    case 'repeatIfd': {
      const rep = form.type === 'repeatIfd';
      const exitKind = rep ? 'takeProfit' : (v['exit.kind'] ?? 'takeProfit');
      const main = ['amount', 'price', ...(rep ? [] : ['exit.kind']), exitKind === 'stop' ? 'exit.stop' : 'exit.takeProfit', ...(rep ? ['repeat.count'] : [])];
      const trailing = exitKind === 'stop' ? ['exit.trail', ...(v['exit.trail'] === 'true' ? ['exit.trail.step', 'exit.trail.gap'] : [])] : [];
      const adv = [
        'tip',
        'entry.stop', 'entry.bandDaa', ...ENTRY_TRIGGER, ...(exitKind === 'stop' ? ['exit.stopLimit', 'exit.slipBps', ...EXIT_TRIGGER] : []), ...trailing,
        'minFill', 'exit.minFill', ...(buy ? [] : ['prefund']), ...life(), 'activeFrom', ...exitLife(), 'exit.tip', ...(rep ? ladder() : []), 'carrier',
      ];
      return { main, advanced: adv };
    }
    case 'ifo':
    case 'repeatIfo': {
      const rep = form.type === 'repeatIfo';
      return {
        main: ['amount', 'price', 'exit.takeProfit', 'exit.stop', ...(rep ? ['repeat.count'] : [])],
        advanced: [
          'tip',
          'entry.stop', 'entry.bandDaa', ...ENTRY_TRIGGER, 'exit.stopLimit', 'exit.slipBps', ...EXIT_TRIGGER, 'exit.trail', ...(v['exit.trail'] === 'true' ? ['exit.trail.step', 'exit.trail.gap'] : []),
          'minFill', 'exit.minFill', ...(buy ? [] : ['prefund']), ...life(), 'activeFrom', ...exitLife(), 'exit.tip', ...(rep ? ladder() : []), 'carrier',
        ],
      };
    }
  }
}

/** Every field id an order type can show, for any selection. Used to drop stale values. */
export const specsOf = (ids: readonly string[]): FieldSpec[] => ids.map((i) => FIELDS[i]!).filter(Boolean);

// ------------------------------------------------------------------------------------- parsing

export type FieldErrorCode = 'required' | 'format' | 'precision' | 'positive' | 'range' | 'negative' | 'noRef';
export interface FieldError { field: string; code: FieldErrorCode; params?: Record<string, string | number> }
export interface BuildResult {
  /** The intent for `planOrder`, null while any field is missing or malformed. */
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

/** Percent text (up to two decimals, 0.01 to 99.99) -> basis points, else null. */
export function parseBps(text: string): bigint | null {
  const m = /^(\d{1,2})(?:[.,](\d{1,2}))?$/.exec(text.trim());
  if (!m) return null;
  const bps = BigInt(m[1]!) * 100n + BigInt((m[2] ?? '').padEnd(2, '0') || '0');
  return bps >= 1n && bps <= 9_999n ? bps : null;
}

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

/** A token amount typed in whole-token units (at most `decimals` decimals) -> exact base units. */
export const parseAmount = (text: string, decimals: number): Parsed<bigint> => parseScaled(text, decimals);

/** Base units -> the token amount text the amount fields take (exact, trailing zeros trimmed). */
export const formatAmount = (base: bigint, decimals: number): string => formatUnits(base, decimals);

export interface PriceParse {
  /** The state price (sompi per whole token of `scale` base units) after the tick rounding. */
  price: bigint;
  /**
   * The typed price is not on the tick, or not a whole number of sompi per `scale` base units.
   * `price` is then the rounded one.
   */
  rounded: boolean;
}

/** What a price-like field is denominated in: the market's quote or KAS (tips). */
export type PriceUnit = 'quote' | 'kas';
/** Decimals of the typed number of a price-like field. */
export const unitDecimals = (ctx: Pick<TicketCtx, 'quoteDecimals'>, unit: PriceUnit = 'quote'): number => (unit === 'kas' ? 8 : ctx.quoteDecimals ?? 8);

/**
 * Quote per token (text: KAS, or a pair's token B per whole A) -> the state price on the tick.
 * `mode` 'safe' rounds in the direction that never makes a limit worse for `side` (sell up, buy
 * down). 'nearest' is for tips and distances. `unit` 'kas' reads KAS whatever the quote (tips).
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
  // Sompi per `scale` base units. This is the identity when scale = 10^decimals (the wallet's
  // scale). A token of more than 9 decimals quotes per 10^9.
  const exact = perToken * ctx.scale;
  const whole = pow10(ctx.decimals);
  const floor = exact / whole;
  const price = tokenPriceToStatePrice(perToken, ctx.decimals, ctx.scale, rounding);
  // Limits and trail steps live on the tick. Tips and gaps are plain quote units that the planner
  // does not tick-check, and a tip is never on a tick.
  const tick = unit === 'kas' ? 1n : ctx.tick;
  const onTick = mode === 'exact' ? price : roundToTick(price, tick, rounding);
  const rounded = exact % whole !== 0n || (mode !== 'exact' && tick > 0n && floor % tick !== 0n) || (mode !== 'exact' && onTick !== floor);
  if (!allowZero && onTick <= 0n) return bad('positive');
  if (!fitsI64(onTick)) return bad('range');
  return good({ price: onTick, rounded });
}

/**
 * A state price (quote units per whole token of `scale` base units) -> quote per token text, exact
 * with trailing zeros trimmed (KAS or token B).
 */
export const formatPrice = (price: bigint, ctx: Pick<TicketCtx, 'decimals' | 'scale' | 'quoteDecimals'>, unit: PriceUnit = 'quote'): string => {
  const qd = unitDecimals(ctx, unit);
  if (qd === 8) return formatPricePerToken(price, ctx.decimals, ctx.scale);
  // Quote per whole token = price * 10^decimals / scale / 10^qd. It is shown with up to qd (at
  // least 8) fraction digits, rounded half up like the KAS form.
  const digits = Math.max(qd, 8);
  const num = price * pow10(ctx.decimals) * pow10(digits);
  const den = ctx.scale * pow10(qd);
  return formatUnits((num + den / 2n) / den, digits);
};

/** Basis points -> the percent text of the percent fields. */
export const formatPercent = (bps: bigint): string => formatUnits(bps, 2);

/**
 * `datetime-local` text (`YYYY-MM-DDThh:mm`, the browser's wall clock) -> UTC unix seconds.
 * `tzOffsetMin` is `Date.getTimezoneOffset()`.
 */
export function parseLocalDateTime(text: string, tzOffsetMin = 0): Parsed<bigint> {
  const s = text.trim();
  if (s === '') return bad('required');
  const m = /^(\d{4})-(\d{2})-(\d{2})[T ](\d{2}):(\d{2})(?::(\d{2}))?$/.exec(s);
  if (!m) return bad('format');
  const [y, mo, d, h, mi, se] = [m[1]!, m[2]!, m[3]!, m[4]!, m[5]!, m[6] ?? '0'].map(Number) as [number, number, number, number, number, number];
  const ms = Date.UTC(y, mo - 1, d, h, mi, se);
  const back = new Date(ms);
  // Date.UTC normalises 31 February to March, so impossible dates are refused.
  if (back.getUTCFullYear() !== y || back.getUTCMonth() !== mo - 1 || back.getUTCDate() !== d || h > 23 || mi > 59) return bad('format');
  return good(BigInt(Math.floor(ms / 1000)) + BigInt(tzOffsetMin) * 60n);
}

/** UTC unix seconds -> `datetime-local` text of the browser's wall clock. */
export function formatLocalDateTime(unixSeconds: bigint, tzOffsetMin = 0): string {
  const d = new Date(Number(unixSeconds - BigInt(tzOffsetMin) * 60n) * 1000);
  const p = (n: number) => String(n).padStart(2, '0');
  return `${d.getUTCFullYear()}-${p(d.getUTCMonth() + 1)}-${p(d.getUTCDate())}T${p(d.getUTCHours())}:${p(d.getUTCMinutes())}`;
}

// ------------------------------------------------------------------------------ intent builder

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
  /** A whole number >= 1. */
  positive(id: string, required = true): bigint | undefined {
    const n = this.whole(id, required);
    if (n === 0n) {
      this.errors.push({ field: id, code: 'positive' });
      return undefined;
    }
    return n;
  }
  /**
   * A token amount in base units, at least one base unit. It is typed in whole-token units with
   * the token's decimals. In a `kas` form it is KAS converted at its leg's price.
   */
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
  /**
   * A price distance (quote per token -> the state price, nearest). With `onTick` it is also on
   * the tick, as a trail step must be. Zero is allowed unless `allowZero` is false.
   */
  delta(id: string, required = false, allowZero = true, onTick = false): bigint | undefined {
    if (!this.has(id) && !required) return undefined;
    return this.take(id, parsePrice(this.raw(id), this.form.side, this.ctx, onTick ? 'nearest' : 'exact', allowZero), required)?.price;
  }
  /** A tip: KAS per token, also on a pair -> sompi per whole token of `scale` base units, exact.
   * Zero is allowed. */
  tipKas(id: string): bigint | undefined {
    if (!this.has(id)) return undefined;
    return this.take(id, parsePrice(this.raw(id), this.form.side, this.ctx, 'exact', true, 'kas'), false)?.price;
  }
  kas(id: string, required = false): bigint | undefined {
    if (!this.has(id) && !required) return undefined;
    return this.take(id, parseScaled(this.raw(id), 8), required);
  }
  /** Percent with two decimals -> basis points. */
  bps(id: string): bigint | undefined {
    if (!this.has(id)) return undefined;
    return this.take(id, parseScaled(this.raw(id), 2), false);
  }
  /** Seconds as a whole number. */
  seconds(id: string): bigint | undefined {
    if (!this.has(id)) return undefined;
    return this.take(id, parseWhole(this.raw(id)), false);
  }
  /** Minutes, up to two decimals -> seconds, rounded up. */
  minutes(id: string, required = false): bigint | undefined {
    if (!this.has(id) && !required) return undefined;
    const p = parseScaled(this.raw(id), 2);
    const secs = this.take(id, p, required);
    return secs === undefined ? undefined : (secs * 60n + 99n) / 100n;
  }
  /** DAA of a duration typed in seconds. */
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
  /**
   * Trigger threshold in base units: `min`, a preset percent of `orderAmount`, or a token amount.
   * Empty or `min` is the wallet default.
   */
  touch(id: string, orderAmount: bigint | undefined): bigint | undefined {
    if (!this.has(id)) return undefined;
    const s = this.raw(id);
    // A custom threshold of a `kas` form is a KAS amount. The presets are the order's own minimum
    // fill or a share of it.
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

/** Parses the form into the intent of its order type. `intent` is null while `errors` is set. */
export function buildIntent(form: TicketForm, ctx: TicketCtx): BuildResult {
  const r = new Reader(form, ctx);
  const side = form.side;
  const opp: Side = side === 'buy' ? 'sell' : 'buy';
  const type = form.type;

  const amount = type === 'close' ? r.amount('amount', false) : r.amount('amount');
  const tip = r.tipKas('tip');
  // Empty means the wallet default (kob-wasm defaultMinFill and defaultMinFillIfd, 1 for immediate
  // orders), shown in the disclosure.
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
  // Shared knobs of a stop leg. Empty means the wallet default of matcher.md 10.6.
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
  // The delivery of a resting buy and of a sell-first if-done's exits. It carries the choice, the
  // wallet's default and stored threshold, and a Top-up tip.
  const delivery = deliveryOf(form, ctx);
  const deliveryReq = (): { delivery?: DeliveryRequest } => {
    if (delivery === null) return {};
    const offered = deliveryChoicesOf(ctx);
    const choice = r.pick('delivery', offered);
    const wallet = ctx.delivery?.walletDefault;
    const req: DeliveryRequest = dropUndefined({
      choice, walletDefault: wallet !== undefined && offered.includes(wallet) ? wallet : wallet !== undefined ? 'each' : undefined, topupTip: delivery === 'topup' ? r.kas('topupTip') : undefined, threshold: delivery === 'topup' ? ctx.delivery?.threshold : undefined,
    });
    return Object.keys(req).length ? { delivery: req } : {};
  };
  /** A Collected order is funded with one delivery carrier, so it has no Fills box. */
  const fillsOf = (budgets: boolean): { maxFills?: bigint | undefined } => (budgets && delivery !== 'collected' ? { maxFills: r.positive('maxFills', false) } : {});
  const condCommon = () => ({
    amount: amount as bigint,
    tip,
    minFill,
    expiry: lifetimeCond(),
    activeFrom: activeFrom(),
    carrier: r.kas('carrier'),
    ...fillsOf(budgetsFills(side, ctx)),
    ...deliveryReq(),
  });
  // Lifetime of the exits. There is no day order, because the exit is created later. Empty means
  // good till cancelled.
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

  // A crossing limit fills from the book by default. A pair order has no such fill and uses its
  // default, the auction.
  const crossingOf = () => {
    const c = r.pick('crossing', ['take', 'auction', 'limit', 'reject'] as const) ?? 'take';
    return isPairCtx(ctx) && c === 'take' ? undefined : c;
  };
  // An IOC, FOK or market order fills from the book when chosen (a FOK by default). Otherwise it
  // is the order for executors, on a KAS market only.
  const takeOf = (): { take?: boolean } => (isPairCtx(ctx) ? {} : { take: (r.pick('direct', ['take', 'order'] as const) ?? fieldDefault('direct', type)) === 'take' });

  let intent: Record<string, unknown> | null = null;
  switch (type) {
    case 'limit':
      intent = { type, side, amount, minFill, price: r.price('price'), tip, lifetime: lifetimeSimple(), activeFrom: activeFrom(), crossing: crossingOf(), ...fillsOf(budgetsFills(side, ctx)), ...deliveryReq() };
      break;
    case 'ioc':
    case 'fok':
      intent = { type, side, amount, minFill, price: r.price('price'), tip, life: sec('life'), activeFrom: activeFrom(), ...takeOf() };
      break;
    case 'market':
      intent = { type, side, amount, minFill, tip, ...auctionOpts(), ...(marketFills(form, ctx) ? { maxFills: r.positive('maxFills', false) } : {}), ...takeOf() };
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
        lifetime: lifetimeSimple(), activeFrom: activeFrom(), ...fillsOf(type === 'dca'), ...(type === 'dca' ? deliveryReq() : {}),
      };
      break;
    }
    case 'dutch': {
      const duration = r.minutes('duration', true);
      intent = {
        type, side, amount, minFill, tip, price: r.price('price'), priceEnd: r.price('priceEnd'), duration: duration === undefined ? undefined : { seconds: duration },
        stepDaa: r.positive('stepDaa', false), lifetime: lifetimeSimple(), activeFrom: activeFrom(), ...fillsOf(budgetsFills(side, ctx)), ...deliveryReq(),
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
        // The trigger rule of a stop entry. A limit entry has no trigger.
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

  // A limit ladder has 2 to 20 levels and either a step in percent or an end price. The end price
  // wins when both are typed.
  if (type === 'limit' && r.raw('ladder.on') === 'true') {
    const levels = r.whole('ladder.levels', true);
    if (levels !== undefined && (levels < MIN_LIMIT_LADDER_LEVELS || levels > MAX_LIMIT_LADDER_LEVELS)) r.err('ladder.levels', 'range');
    if (r.has('ladder.end')) {
      const end = r.price('ladder.end');
      if (end !== undefined && intent !== null && end === (intent as { price?: bigint }).price) r.err('ladder.end', 'range');
    } else if (!r.has('ladder.pct')) r.err('ladder.pct', 'required');
    else if (parseBps(r.raw('ladder.pct')) === null) r.err('ladder.pct', 'range');
  }

  if (r.errors.length > 0 || intent === null) return { intent: null, errors: r.errors };
  return { intent: dropUndefined(intent) as unknown as Intent, errors: [] };
}

// ------------------------------------------------------------------------------ intent -> form

/**
 * Inverse of `buildIntent`: the form that produces `intent`. Amounts render as exact decimals and
 * durations in the unit of their field.
 */
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
  put('delivery', i.delivery?.choice);
  if (i.delivery?.topupTip !== undefined) v.topupTip = formatUnits(i.delivery.topupTip, 8);
  switch (intent.type) {
    case 'limit': price('price', i.price); life(i.lifetime); from(i.activeFrom); put('crossing', i.crossing); put('maxFills', i.maxFills); break;
    case 'ioc': case 'fok': price('price', i.price); secs('life', i.life); from(i.activeFrom); v.direct = i.take ? 'take' : 'order'; break;
    case 'market': case 'close':
      if (intent.type === 'market') v.direct = i.take ? 'take' : 'order';
      bps('slippageBps', i.slippageBps); secs('auction', i.auction); secs('life', i.life); if (i.allOrNothing) v.allOrNothing = 'true'; put('maxFills', i.maxFills); break;
    case 'streaming':
      price('displayedPrice', i.displayedPrice); bps('toleranceBps', i.toleranceBps); secs('auction', i.auction); secs('life', i.life); if (i.allOrNothing) v.allOrNothing = 'true'; break;
    case 'twap': case 'dca':
      amount('sliceAmount', i.sliceAmount); price('price', i.price); price('priceEnd', i.priceEnd); secs('sliceAuction', i.sliceAuction);
      if (i.interval) mins('interval', BigInt('seconds' in i.interval ? i.interval.seconds : (i.interval.daa * 1000n) / rate)); life(i.lifetime); from(i.activeFrom); put('maxFills', i.maxFills); break;
    case 'dutch':
      price('price', i.price); price('priceEnd', i.priceEnd); put('stepDaa', i.stepDaa); life(i.lifetime); from(i.activeFrom); put('maxFills', i.maxFills);
      if (i.duration) mins('duration', BigInt('seconds' in i.duration ? i.duration.seconds : (i.duration.daa * 1000n) / rate)); break;
    default: {
      // The conditional family.
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

// -------------------------------------------------------------- streaming: the price you saw

/**
 * The quote a streaming order shows its user, as the text of the "price you saw" box. It is the
 * best price its side meets: the best ask for a buy and the best bid for a sell. It is null when
 * that side of the book is empty.
 */
export function streamingQuoteText(side: Side, best: { ask: bigint | null; bid: bigint | null }, ctx: Pick<TicketCtx, 'decimals' | 'scale' | 'quoteDecimals'>): string | null {
  const p = side === 'buy' ? best.ask : best.bid;
  return p !== null && p > 0n ? formatPrice(p, ctx) : null;
}

/**
 * Keeps the streaming "price you saw" on the quote. While the box is empty or still holds the
 * quote filled last (`auto`), it follows `quote`. A price the user typed is left alone, and
 * emptying the box hands it back to the quote. Returns the form (the same object when nothing
 * changes) and the text auto-filled now (null when the box is the user's).
 */
export function followQuote(form: TicketForm, quote: string | null, auto: string | null): { form: TicketForm; auto: string | null } {
  if (form.type !== 'streaming') return { form, auto: null };
  const cur = form.values.displayedPrice ?? '';
  const mine = cur.trim() !== '' && cur !== auto;
  if (mine) return { form, auto: null };
  if (quote === null || quote === cur) return { form, auto: cur === '' ? null : cur };
  return { form: { ...form, values: { ...form.values, displayedPrice: quote } }, auto: quote };
}

// ------------------------------------------------------------- helpers for the component

/**
 * Per-field summary for display under an input: the exact state price after rounding, and whether
 * rounding happened.
 */
export function priceInfo(text: string, side: Side, ctx: Pick<TicketCtx, 'decimals' | 'scale' | 'tick' | 'quoteDecimals'>): { price: bigint; perToken: string; rounded: boolean } | null {
  const p = parsePrice(text, side, ctx);
  return p.ok ? { price: p.value.price, perToken: formatPrice(p.value.price, ctx), rounded: p.value.rounded } : null;
}

export interface MaxAmountInput {
  side: Side;
  /** Free (P2PK-owned) tokens, base units. */
  tokenBalance: bigint;
  /** Spendable KAS, sompi. */
  kasBalance: bigint;
  /** The order scale: base units per whole token. */
  scale: bigint;
  /** Limit price including tip, sompi per whole token (all-in of a buy). 0 or absent is unknown. */
  allInPrice?: bigint;
  /** Carrier KAS per covenant UTXO. */
  carrier: bigint;
}

/**
 * The most base units the balance can plausibly place. The planner does the exact funding check.
 * Sell: the free token balance. Buy: the KAS balance minus a reserve for carriers (delivery
 * carriers and the network fee), at the all-in price, `floor(kas * scale / allIn)`.
 */
export function maxAmount(i: MaxAmountInput): bigint {
  if (i.side === 'sell') return i.tokenBalance;
  if (!i.allInPrice || i.allInPrice <= 0n) return 0n;
  const reserve = i.carrier * 4n + 20_000_000n;
  return i.kasBalance > reserve ? ((i.kasBalance - reserve) * i.scale) / i.allInPrice : 0n;
}

/**
 * Errors worth showing under a field right away (the user typed something wrong), versus fields
 * that are merely still empty.
 */
export const visibleErrors = (errors: readonly FieldError[]): FieldError[] => errors.filter((e) => e.code !== 'required');
export const isIncomplete = (errors: readonly FieldError[]): boolean => errors.some((e) => e.code === 'required');

// ---------------------------------------------------------- KAS amounts (a KAS market inverted)
//
// KAS/TOKEN is the token's own TOKEN/KAS book seen the other way round, so its base, the left
// asset, is KAS. A `kas` form counts every amount in KAS, as any BASE/QUOTE market counts the base.
// The order is still the native one, because buying KAS is selling the token. X KAS is
// floor(X x scale / P) base units of the token, at the price P of the amount's own leg:
// - the limit price;
// - the stop of a stop leg;
// - the exit price of an exit;
// - the book's best price for an order without a price of its own, market and close.
// Rounding down means the KAS value of the order never exceeds what was typed. A shown Sell spends
// at most the KAS typed (plus the tip), and a shown Buy receives at most that (minus the tip).

/**
 * Key suffix of an exact base amount kept for an amount box of a `kas` form. The box then shows
 * that amount's KAS value.
 */
export const BASE_SUFFIX = '@base';

/**
 * The amount-like fields, typed in KAS in a `kas` form. A `touch` field is typed in KAS only for a
 * custom threshold.
 */
export const AMOUNT_FIELD_IDS: readonly string[] = ['amount', 'minFill', 'sliceAmount', 'exit.minFill', 'minTouch', 'entry.minTouch', 'exit.minTouch'];

/**
 * Where the conversion price of an amount comes from. It is the order's own price, a stop trigger,
 * an exit price or the book. For a buy, which spends the KAS typed, it is `allIn` (its own price
 * plus the tip) or `worst` (the most a price that moves can reach, plus the tip).
 */
export type AmountBasis = 'own' | 'stop' | 'exit' | 'book' | 'allIn' | 'worst';

/**
 * The price field of an order type that sizes it (its limit, else its trigger). Null means no price
 * of its own (market, close), so the book's price is used.
 */
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
 * The state price (sompi per `scale` base units) a KAS amount of field `id` converts at, with where
 * it comes from. It is null while that price is missing or malformed (the price box reports its
 * own problem) or, for an order without a price, while the book has none.
 *
 * A buy spends the KAS typed. Its own amount converts at the most it can pay per token, which is
 * the worst price its auction or stop can reach, plus the tip. That worst price is:
 * - market or close order: the book's best price moved by the slippage;
 * - streaming: the displayed price moved by the tolerance;
 * - a rising Dutch or a DCA: its end price;
 * - a stop: the stop moved by its band;
 * - an OCO: the dearer of its take-profit and its stop's band.
 * So the escrow it is built with never spends more KAS than typed. A fill at a better price
 * spends less and the rest returns. A sell receives KAS, so its own price (the book's for a
 * market) is what it receives about.
 */
export function amountPriceOf(form: TicketForm, ctx: TicketCtx, id: string): { price: bigint; basis: AmountBasis } | null {
  const leg = legPriceOf(form, ctx, id);
  if (!leg || form.side !== 'buy' || !OWN_AMOUNTS.includes(id)) return leg;
  const v = form.values;
  const bps = (field: string, dflt: bigint | null): bigint | null => {
    const raw = (v[field] ?? '').trim();
    if (raw === '') return dflt;
    const p = parseScaled(raw, 2);
    return p.ok ? p.value : null;
  };
  const price = (field: string): bigint | null => {
    const p = parsePrice(v[field] ?? '', 'buy', ctx);
    return p.ok ? p.value.price : null;
  };
  let worst = leg.price;
  switch (form.type) {
    case 'market':
    case 'streaming': {
      const b = bps(form.type === 'market' ? 'slippageBps' : 'toleranceBps', form.type === 'market' ? SLIPPAGE_BPS : null);
      if (b === null) return null;
      worst = auctionBound('buy', leg.price, b, ctx.tick);
      break;
    }
    case 'dutch':
    case 'dca': {
      const end = price('priceEnd');
      if (end !== null && end > worst) worst = end;
      break;
    }
    case 'stopMarket':
    case 'trailingStop': {
      const b = bps('slipBps', SLIPPAGE_BPS);
      if (b === null) return null;
      worst = stopWorstPrice('buy', leg.price, b);
      break;
    }
    case 'oco': {
      const stop = price('stop');
      const b = bps('slipBps', SLIPPAGE_BPS);
      if (stop !== null && b !== null) {
        const w = stopWorstPrice('buy', stop, b);
        if (w > worst) worst = w;
      }
      break;
    }
    default:
  }
  const tipText = (v.tip ?? '').trim();
  const tip = tipText === '' ? 0n : (() => {
    const p = parsePrice(tipText, 'buy', ctx, 'exact', true, 'kas');
    return p.ok ? p.value.price : null;
  })();
  if (tip === null) return null;
  if (worst === leg.price && tip === 0n) return leg;
  return { price: worst + tip, basis: worst !== leg.price ? 'worst' : 'allIn' };
}

/**
 * The amounts of the order's own leg. An exit's amounts and the trigger thresholds convert at
 * their own leg's price.
 */
const OWN_AMOUNTS: readonly string[] = ['amount', 'minFill', 'sliceAmount'];

/** The price of the leg an amount belongs to, before a buy's worst price and tip. */
function legPriceOf(form: TicketForm, ctx: TicketCtx, id: string): { price: bigint; basis: AmountBasis } | null {
  const opp: Side = form.side === 'buy' ? 'sell' : 'buy';
  const v = form.values;
  const read = (field: string, side: Side, basis: AmountBasis) => {
    const p = parsePrice(v[field] ?? '', side, ctx);
    return p.ok ? { price: p.value.price, basis } : null;
  };
  if (id === 'exit.minFill' || id === 'exit.minTouch') {
    // The exit trades the other way, at its own price: the take-profit, else the stop. An exit
    // stop's threshold uses the stop.
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
 * The base units of amount field `id`. In a native form it is the token amount typed. In a `kas`
 * form it is the exact amount kept for the box, else the KAS typed converted at the field's price.
 * `noRef` when an order without a price has no book price to convert at.
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

/**
 * The KAS text of `base` units of the token at the field's price, rounded down to the sompi. It is
 * '' when there is no price to value it at.
 */
export function kasTextOf(form: TicketForm, ctx: TicketCtx, id: string, base: bigint): string {
  const p = amountPriceOf(form, ctx, id);
  return p ? formatUnits(baseToKas(base, p.price, ctx.scale), 8) : '';
}

/**
 * Sets amount field `id` to exactly `base` units of the token (Max, a prefilled amount, a flip of
 * a filled form). A token form shows them. A `kas` form keeps them exactly and shows their KAS
 * value, so the order does not move by a KAS rounding.
 */
export function setAmountBase(form: TicketForm, id: string, base: bigint, ctx: TicketCtx): TicketForm {
  if (!form.kas) return setValue(form, id, formatAmount(base, ctx.decimals));
  return { ...form, values: { ...form.values, [id]: kasTextOf(form, ctx, id, base), [id + BASE_SUFFIX]: base.toString() } };
}

/**
 * The same order with its amount boxes in the other unit (`kas` true is KAS, false is token
 * units). Every amount that parses keeps its exact base units. A box that does not parse is
 * emptied and a preset threshold is kept, so a flip never changes the order that is built.
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
