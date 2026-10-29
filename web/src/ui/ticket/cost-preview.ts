// The cost preview under the ticket's amount box: what the typed amount costs (a buy) or yields (a sell) in the other asset (KAS, or a pair's quote token),
// estimated from the node-confirmed book the planner uses (app/book-verify.ts). Pure; amounts are base units, prices sompi (or quote base units) per
// whole token, `scale` the token's base units per whole token.
//
//   * a limit order (and IOC / FOK): amount x its limit price, exact, one value;
//   * the order's own tip is part of every value (a buy pays price + tip per token, a sell receives price - tip), and the network fee when the planner knows it
//     (a buy pays it on top, a sell receives it less): the preview is the total the confirmation shows;
//   * a market order (also streaming, close): the walk of the book from the best level, level by level, as the auction would start; the second value is the
//     bound the order allows (amount x its worst price, the slippage allowed), a buy's "max" plus the network fee, a sell's "min" less it, when the
//     planner knows them;
//   * a book too thin for the amount says what it covers instead.
import type { Intent } from '../../kob/plan';
import type { BookLevel, BookView } from '../../kob/plan-types';
import { quoteOf } from '../../kob/units';

export type CostPreview =
  | { kind: 'empty' }
  /** `estimate` and `bound` in the quote's base units; `bound` null = the estimate is exact (a limit order) */
  | { kind: 'cost'; side: 'buy' | 'sell'; estimate: bigint; bound: bigint | null }
  /** the visible book covers only `covers` base units of the amount */
  | { kind: 'thin'; covers: bigint };

/**
 * The one line under the amount box: "110.55 KAS" (exact: a limit order, or the estimate is the bound), "≈ 110.55 KAS · max 113.87 KAS" (a buy), "≈ 110.55 KAS · min 107.23 KAS"
 * (a sell), "book covers only 40 TUSD", or "—". The two values are shown only when they differ at the precision shown. `fmt` prints a quote amount with its unit,
 * `fmtBase` a token amount with its ticker. The wording comes in as `tr` so this stays pure.
 */
export function costLine(
  p: CostPreview,
  fmt: (quoteBase: bigint) => string,
  fmtBase: (base: bigint) => string,
  tr: (key: 'dash' | 'exact' | 'buy' | 'sell' | 'thin', params: Record<string, string>) => string,
): string {
  if (p.kind === 'empty') return tr('dash', {});
  if (p.kind === 'thin') return tr('thin', { amount: fmtBase(p.covers) });
  const est = fmt(p.estimate);
  if (p.bound === null) return tr('exact', { value: est });
  const bound = fmt(p.bound);
  if (bound === est) return tr('exact', { value: est });
  return tr(p.side === 'buy' ? 'buy' : 'sell', { estimate: est, bound });
}

export interface CostInput {
  intent: Intent | null;
  book: BookView;
  scale: bigint;
  /** the worst price the plan allows (its disclosure), sompi (or quote base units) per whole token; null = not known (yet) */
  worstPrice: bigint | null;
  /** the network fee of the placement (KAS) when the quote is KAS and the planner knows it: part of every value */
  fee: bigint | null;
}

/** A price with the order's own tip: a buy pays it on top, a sell receives it less. */
const withTip = (price: bigint, tip: bigint, side: 'buy' | 'sell'): bigint => (side === 'buy' ? price + tip : price > tip ? price - tip : 0n);
/** A total with the network fee: a buy pays it on top, a sell receives it less. */
const withFee = (total: bigint, fee: bigint | null, side: 'buy' | 'sell'): bigint => (fee === null ? total : side === 'buy' ? total + fee : total > fee ? total - fee : 0n);

const allIn = (l: BookLevel, side: 'buy' | 'sell'): bigint => {
  const tip = l.tip ?? 0n;
  return side === 'buy' ? l.price + tip : l.price > tip ? l.price - tip : 0n;
};

/** The preview of `i`: empty without a positive amount or for an order that is no plain trade. */
export function costPreview(i: CostInput): CostPreview {
  const it = i.intent;
  if (!it || i.scale <= 0n) return { kind: 'empty' };
  const side = (it as { side?: 'buy' | 'sell' }).side ?? (it.type === 'close' ? 'sell' : undefined);
  const amount = (it as { amount?: bigint }).amount;
  if (side === undefined || amount === undefined || amount <= 0n) return { kind: 'empty' };
  const round = side === 'sell' ? 'up' : 'down';
  const tip = (it as { tip?: bigint }).tip ?? 0n;
  switch (it.type) {
    case 'limit':
    case 'ioc':
    case 'fok': {
      const price = (it as { price: bigint }).price;
      if (price <= 0n) return { kind: 'empty' };
      return { kind: 'cost', side, estimate: withFee(quoteOf(amount, withTip(price, tip, side), i.scale, round), i.fee, side), bound: null };
    }
    case 'market':
    case 'streaming':
    case 'close': {
      const levels = side === 'buy' ? i.book.asks : i.book.bids;
      let left = amount;
      let estimate = 0n;
      for (const l of levels) {
        if (left <= 0n) break;
        if (l.amount <= 0n) continue;
        const take = left < l.amount ? left : l.amount;
        estimate += quoteOf(take, withTip(allIn(l, side), tip, side), i.scale, round);
        left -= take;
      }
      if (left > 0n) return { kind: 'thin', covers: amount - left };
      let bound: bigint | null = null;
      if (i.worstPrice !== null && i.worstPrice > 0n) bound = withFee(quoteOf(amount, withTip(i.worstPrice, tip, side), i.scale, round), i.fee, side);
      return { kind: 'cost', side, estimate: withFee(estimate, i.fee, side), bound };
    }
    default:
      return { kind: 'empty' };
  }
}
