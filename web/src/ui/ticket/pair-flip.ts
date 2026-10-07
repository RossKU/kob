// The flip of a token/token pair page (A/B -> B/A) carries the order being typed across: the pair page flips by opening the other pair (another
// route, a fresh ticket), and without this the new ticket started empty on its default side, so a flip never turned Buy into Sell.
//
// The same order seen from B/A: buying A with B is selling B for A. So the side turns over, every price p (B per whole A) becomes 1/p (A per
// whole B), and an amount of A becomes the amount of B it is worth at the order's own price: N_B = N_A x p, rounded down to B's smallest unit.
// A tip (KAS per whole base token traded) is re-expressed per whole B: t / p. Fields that have no exact counterpart on the other side (price
// distances: trailing steps and gaps, a ladder step, a prefund; an exit's minimum fill and custom trigger thresholds) are left to their defaults,
// and the ticket says so. Prices are rounded to the decimals the other pair's quote can take; the planner then rounds each limit the way that
// never makes it worse, as for any typed price. Pure, DOM-free; the hand-over between the two pages is the small module state at the bottom.
import { formatRational, parseDecimal, reduce, significantDp, type Rational } from '../../kob/pair';
import { FIELDS, initialForm, layoutOf, primaryPriceField, sidesOf, type OrderTypeId, type Side, type TicketForm } from './form-state';

/** Fields with no exact counterpart on the flipped pair: left to their defaults. */
const DROPPED_KINDS = new Set(['delta', 'touch']);
const DROPPED_IDS = new Set(['exit.minFill']);
/** Amount fields converted at the order's own price (counted in the base of the pair). */
const CONVERTED_AMOUNTS = new Set(['amount', 'minFill', 'sliceAmount']);

/** The order type on the other side: TWAP only sells and DCA only buys (each is the other seen from B/A); close (sell everything) becomes a market buy. */
function flippedType(type: OrderTypeId, side: Side): OrderTypeId {
  if (sidesOf(type).includes(side)) return type;
  if (type === 'twap') return 'dca';
  if (type === 'dca') return 'twap';
  return 'market';
}

const textOf = (r: Rational, dp: number, mode: 'down' | 'nearest'): string => formatRational(r, dp, mode, 0);

export interface FlipDecimals {
  /** decimals of the pair's base token A (the quote of the flipped pair) */
  base: number;
  /** decimals of the pair's quote token B (the base of the flipped pair) */
  quote: number;
}

/**
 * The form of the pair A/B as the same order on B/A (see the header). `dec` names the decimals of A and B. The result is a form of B/A in its
 * own units: amounts of B, prices of A per whole B.
 */
export function flipPairForm(form: TicketForm, dec: FlipDecimals): TicketForm {
  const side: Side = form.side === 'buy' ? 'sell' : 'buy';
  const type = flippedType(form.type, side);
  const next = initialForm(type, side);
  const values = { ...next.values };
  const pField = primaryPriceField(form.type);
  const p = pField ? parseDecimal(form.values[pField] ?? '') : null;
  const price = p && p.num > 0n ? p : null;
  const applicable = new Set([...layoutOf({ type, side, values: form.values }).main, ...layoutOf({ type, side, values: form.values }).advanced]);
  for (const [id, raw] of Object.entries(form.values)) {
    const text = raw.trim();
    const spec = FIELDS[id];
    if (!spec || text === '' || !applicable.has(id) || DROPPED_IDS.has(id) || DROPPED_KINDS.has(spec.kind)) continue;
    if (spec.kind === 'price') {
      const r = parseDecimal(text);
      if (!r || r.num === 0n) continue;
      const inv = reduce({ num: r.den, den: r.num });
      // the flipped pair takes prices with at most A's decimals
      const s = textOf(inv, Math.min(significantDp(inv, 8, 2, 12), dec.base), 'nearest');
      if (parseDecimal(s)?.num) values[id] = s;
      continue;
    }
    if (spec.kind === 'amount') {
      const r = CONVERTED_AMOUNTS.has(id) ? parseDecimal(text) : null;
      if (!r || !price) continue;
      const s = textOf(reduce({ num: r.num * price.num, den: r.den * price.den }), dec.quote, 'down');
      if (parseDecimal(s)?.num) values[id] = s;
      continue;
    }
    if (spec.kind === 'tip') {
      const r = parseDecimal(text);
      if (!r || !price) continue;
      values[id] = textOf(reduce({ num: r.num * price.den, den: r.den * price.num }), 8, 'nearest');
      continue;
    }
    values[id] = raw;
  }
  return { type, side, values };
}

// ------------------------------------------------------------------------------------------------ hand-over between the two pair pages

const lastForms = new Map<string, TicketForm>();
let pending: { from: string; to: string } | null = null;
const keyOf = (base: string, quote: string): string => `${base.toLowerCase()}/${quote.toLowerCase()}`;

/** The ticket of the pair base/quote records what is typed (the latest form), for a flip. */
export function rememberPairForm(base: string, quote: string, form: TicketForm): void {
  lastForms.set(keyOf(base, quote), form);
}

/** The pair page base/quote is about to open quote/base: its ticket takes the order across. */
export function requestPairFlip(base: string, quote: string): void {
  pending = { from: keyOf(base, quote), to: keyOf(quote, base) };
}

/**
 * The ticket of the pair base/quote opening: the order typed on quote/base just before a flip, as a form of this pair, or null (once: the
 * request is consumed).
 */
export function takePairFlip(base: string, quote: string, dec: FlipDecimals): TicketForm | null {
  const p = pending;
  pending = null;
  if (!p || p.to !== keyOf(base, quote)) return null;
  const f = lastForms.get(p.from);
  return f ? flipPairForm(f, dec) : null;
}
