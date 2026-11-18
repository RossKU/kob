// The delivery of a buy-side order, meaning where the tokens it buys go
// (docs/design/bid-delivery-spec.md §1, wallet rules §10).
//
// - Each fill (mode 0): every fill sends a new token UTXO to the maker, with one delivery carrier
//   per fill.
// - Collected (mode 1): the order keeps what it buys in its own custody, and sends one UTXO when it
//   ends, is cancelled or is claimed. There is one delivery carrier for the whole order.
// - Top-up (mode 2): a fill may add the tokens to the maker's receive UTXO (borrow scheme 0x01),
//   and keeps the top-up tip in place of the carrier. A fill that cannot top up arrives as a new
//   UTXO, so the order is funded like mode 0.
//
// The planners call `resolveDelivery` once per delivering order. The rules here are pure. What the
// wallet stores, the default of Settings and the receive threshold per token, comes in through
// `DeliveryRequest`.
import type { DeliveryFields, DeliveryMode, ExitDeliveryFields } from '../types';
import type { OrderPlan, PlanIssue } from '../plan-types';
import { programTopupExtra } from '../programs.generated';
import { ceilDiv } from '../units';
import { issue } from './common-issues';

/** The three deliveries by their names in the app. */
export type DeliveryChoice = 'each' | 'collected' | 'topup';
/** The order of the choices in the ticket's select and in Settings. */
export const DELIVERY_CHOICES: readonly DeliveryChoice[] = ['each', 'collected', 'topup'];
/** Tells whether `v` is a delivery choice. */
export const isDeliveryChoice = (v: unknown): v is DeliveryChoice => v === 'each' || v === 'collected' || v === 'topup';

/** The state's `mode` of each choice. */
export const MODE_OF: Readonly<Record<DeliveryChoice, DeliveryMode>> = { each: '0', collected: '1', topup: '2' };
/** The choice of a state's `mode`. An absent or unknown mode is each fill. */
export const choiceOfMode = (mode: string | undefined): DeliveryChoice => (mode === '1' ? 'collected' : mode === '2' ? 'topup' : 'each');

/**
 * The KAS a receive UTXO's threshold is sized to (bid-delivery-spec §10), which is 5 KAS of the
 * token at the order's price.
 */
export const RECV_THRESHOLD_SOMPI = 500_000_000n;

/**
 * What the default rule needs to know of an order.
 * - `resting`: a GTC order with tif 0. That is a limit, a ladder level, a DCA, a rising bid, a
 *   conditional buy, or the exit of a non-repeating entry.
 * - `immediate`: IOC, FOK and every market order, which is one fill or a short multi-fill auction.
 *   They deliver at each fill.
 * - `repeating`: the exits of a repeating entry, which merge back into the entry. They deliver at
 *   each fill.
 */
export type DeliveryClass = 'resting' | 'immediate' | 'repeating';

/** What the user and the wallet asked for. */
export interface DeliveryRequest {
  /** The choice under Advanced options. Absent: the wallet default. */
  choice?: DeliveryChoice;
  /** The wallet-wide default of Settings. Absent: Each fill. */
  walletDefault?: DeliveryChoice;
  /** Top-up only: the tip per fill in sompi. Absent: the default (`defaultTopupTip`). */
  topupTip?: bigint;
  /**
   * Top-up only: the receive threshold this wallet stored for the token, by wallet key, token
   * covenant id and extension commitment. It is absent at the wallet's first top-up order of the
   * token, and then computed from the order's price (`walletThreshold`).
   */
  threshold?: bigint;
}

/** The order the delivery is for. */
export interface DeliveryOrder {
  /** The codec of the token the order buys. A KRON delivery is always each fill. */
  codec: 'kcc20' | 'kron';
  /** The bought token's program, for the top-up extra (programs.generated.ts `topupExtra`). */
  program: string;
  cls: DeliveryClass;
  /** The order's delivery carrier, sompi. */
  carrier: bigint;
  /** Base units per whole token of the bought token. */
  scale: bigint;
  /** The order's minimum fill in base units. */
  minFill: bigint;
  /**
   * The order's limit price in sompi per whole token, which for a rising bid is its cap. It is null
   * when the order has none.
   */
  price: bigint | null;
}

/**
 * Tells whether the order delivers at each fill whatever is asked. That holds for KRON, for IOC,
 * FOK and market orders, and for a repeating entry's exits.
 */
export const deliveryFixed = (o: Pick<DeliveryOrder, 'codec' | 'cls'>): boolean => o.codec !== 'kcc20' || o.cls !== 'resting';

/**
 * The largest top-up tip (kob-protocol `state::TOPUP_TIP_MAX`). It is just below the 0.08 KAS a
 * token output carries at least (`COVENANT_OUTPUT_MIN`).
 *
 * A filler may top up a receive UTXO it made itself with 0 units, and its KAS then goes to the
 * maker. Above this tip, a fresh receive UTXO per fill would pay the filler. The indexer does not
 * list an order over it.
 */
export const TOPUP_TIP_MAX = 7_999_999n;

/**
 * The most tip an order with `carrier` may set, which is the carrier and at most `TOPUP_TIP_MAX`.
 */
export const topupTipMax = (carrier: bigint): bigint => (carrier < TOPUP_TIP_MAX ? carrier : TOPUP_TIP_MAX);

/**
 * The default top-up tip per fill (bid-delivery-spec §10), which is
 * `min(deliveryCarrier, ceil(1.25 × topup_extra))` and at most `TOPUP_TIP_MAX`. So a filler that
 * tops up is paid its extra relay fee with a margin. It is null when the program has no top-up
 * extra.
 */
export function defaultTopupTip(carrier: bigint, extra: bigint | null): bigint | null {
  if (extra === null) return null;
  const tip = ceilDiv(extra * 5n, 4n);
  const max = topupTipMax(carrier);
  return tip < max ? tip : max;
}

/** `defaultTopupTip` of a program by name (programs.generated.ts). */
export const programTopupTip = (program: string, carrier: bigint): bigint | null => defaultTopupTip(carrier, programTopupExtra(program));

/**
 * The wallet's receive threshold of a token (bid-delivery-spec §10), set at its first top-up
 * order. It is the token amount worth RECV_THRESHOLD_SOMPI at the order's price,
 * `max(1, ceil(RECV_THRESHOLD_SOMPI × scale / price))`. It is null without a price.
 */
export function walletThreshold(price: bigint | null, scale: bigint): bigint | null {
  if (price === null || price <= 0n || scale <= 0n) return null;
  const t = ceilDiv(RECV_THRESHOLD_SOMPI * scale, price);
  return t < 1n ? 1n : t;
}

/**
 * An order's `recvThreshold`, `min(T, minFill - 1)` and at least 0. At 0 the order cannot top up,
 * because no fill would grow the UTXO past it.
 */
export function orderRecvThreshold(threshold: bigint, minFill: bigint): bigint {
  const cap = minFill - 1n;
  const r = threshold < cap ? threshold : cap;
  return r > 0n ? r : 0n;
}

/** Why top-up cannot be offered for an order. */
export type TopupUnavailable = 'fixed' | 'program' | 'price' | 'minFill';

/**
 * The top-up for an order, as its threshold, `recvThreshold` and default tip, or why it is not
 * offered.
 */
export function topupOffer(o: DeliveryOrder, stored?: bigint): { ok: true; threshold: bigint; recvThreshold: bigint; defaultTip: bigint } | { ok: false; reason: TopupUnavailable } {
  if (deliveryFixed(o)) return { ok: false, reason: 'fixed' };
  const defaultTip = programTopupTip(o.program, o.carrier);
  if (defaultTip === null) return { ok: false, reason: 'program' };
  const threshold = stored !== undefined && stored > 0n ? stored : walletThreshold(o.price, o.scale);
  if (threshold === null) return { ok: false, reason: 'price' };
  const recvThreshold = orderRecvThreshold(threshold, o.minFill);
  if (recvThreshold === 0n) return { ok: false, reason: 'minFill' };
  return { ok: true, threshold, recvThreshold, defaultTip };
}

/** The delivery an order is placed with. */
export interface ResolvedDelivery {
  choice: DeliveryChoice;
  /** The state's delivery block. `bought` is 0 at placement. */
  fields: Required<DeliveryFields>;
  /**
   * For a top-up, the wallet threshold the order used, which the wallet stores after its first
   * top-up order of the token. Otherwise null.
   */
  threshold: bigint | null;
}

/**
 * The choice an order gets: each fill when fixed, else the user's, the wallet default or Each fill.
 */
export const effectiveChoice = (o: Pick<DeliveryOrder, 'codec' | 'cls'>, r: Pick<DeliveryRequest, 'choice' | 'walletDefault'> | undefined): DeliveryChoice =>
  deliveryFixed(o) ? 'each' : r?.choice ?? r?.walletDefault ?? 'each';

const EACH: Required<DeliveryFields> = { mode: '0', recvThreshold: '0', topupTip: '0', bought: '0' };

/**
 * The delivery of an order from what the user and the wallet asked for (bid-delivery-spec §10).
 * Problems are added to `pre`:
 * - A top-up the user chose that the order cannot have is an error (`DELIVERY_TOPUP_UNAVAILABLE`).
 *   A top-up that is only the wallet default falls back to Each fill with a note
 *   (`DELIVERY_TOPUP_FALLBACK`).
 * - A top-up tip below 0 or above `topupTipMax`, the carrier and below 0.08 KAS, is an error
 *   (`TOPUP_TIP_INVALID`). One below the default warns (`TOPUP_TIP_LOW`), because a filler may not
 *   top up for it, and the fill then arrives as a new UTXO.
 */
export function resolveDelivery(o: DeliveryOrder, r: DeliveryRequest | undefined, pre: PlanIssue[]): ResolvedDelivery {
  if (deliveryFixed(o)) return { choice: 'each', fields: { ...EACH }, threshold: null };
  let choice = effectiveChoice(o, r);
  if (choice === 'topup') {
    const offer = topupOffer(o, r?.threshold);
    if (!offer.ok) {
      if (r?.choice === 'topup') pre.push(issue('DELIVERY_TOPUP_UNAVAILABLE', { reason: offer.reason }, 'delivery'));
      else pre.push(issue('DELIVERY_TOPUP_FALLBACK', { reason: offer.reason }));
      choice = 'each';
    } else {
      const tip = r?.topupTip ?? offer.defaultTip;
      const max = topupTipMax(o.carrier);
      if (tip < 0n || tip > max) pre.push(issue('TOPUP_TIP_INVALID', { max }, 'topupTip'));
      else if (tip < offer.defaultTip) pre.push(issue('TOPUP_TIP_LOW', { tip: offer.defaultTip }, 'topupTip'));
      return { choice, fields: { mode: '2', recvThreshold: offer.recvThreshold.toString(), topupTip: tip.toString(), bought: '0' }, threshold: offer.threshold };
    }
  }
  return { choice, fields: choice === 'collected' ? { ...EACH, mode: '1' } : { ...EACH }, threshold: null };
}

/**
 * The exit block an if-done entry commits for its exits (`KobIfdAsk`, `KobIfdPair`), which is the
 * exit's delivery as `exit*` fields.
 */
export const exitFieldsOf = (d: Pick<ResolvedDelivery, 'fields'>): Required<ExitDeliveryFields> => ({
  exitMode: d.fields.mode, exitRecvThreshold: d.fields.recvThreshold, exitTopupTip: d.fields.topupTip,
});

/**
 * The delivery carriers a delivering order is funded with (bid-delivery-spec §10).
 *
 * Collected takes one, because the first fill creates the custody and every later fill rides on
 * it. Any other takes one per possible fill. A top-up order is funded the same, because a fill
 * that cannot top up falls back to a new delivery.
 */
export const fundedFills = (choice: DeliveryChoice, fills: bigint): bigint => (choice === 'collected' ? 1n : fills);

/** The most a top-up order pays in tips, one per funded fill. */
export const topupTipTotal = (tip: bigint, fills: bigint): bigint => tip * fills;

/**
 * What a buy's plan says of its delivery, next to the plan like `cond` and `pair`. The ticket shows
 * it and stores the threshold.
 */
export interface PlanDelivery {
  delivery: ResolvedDelivery;
  /** The delivery carriers the escrow is funded with. A Collected order has one. */
  fills: bigint;
}

/** A plan with the delivery of its delivering order. */
export const withDelivery = <T extends OrderPlan>(p: T, delivery: PlanDelivery): T => ({ ...p, delivery });

/**
 * The delivery a plan carries. It is null for a sell, a kind without a delivery, or a plan that
 * failed before its order was made.
 */
export const planDelivery = (p: OrderPlan): PlanDelivery | null => (p as OrderPlan & { delivery?: PlanDelivery }).delivery ?? null;

/**
 * An intent with its Top-up threshold set, since every order of a ladder uses the wallet's one
 * threshold. An intent without a delivery is returned as it is.
 */
export function withThreshold<I extends object>(intent: I, threshold: bigint): I {
  const d = (intent as { delivery?: DeliveryRequest }).delivery;
  return d ? { ...intent, delivery: { ...d, threshold } } : intent;
}

// --- Pair orders ---
//
// A resting pair bid (KobPair, KobCondPair) whose bought token A is KCC-20 delivers at each fill by
// default. When the user picks Collected, it is funded with one delivery carrier. "Added to one
// UTXO" is not offered on pairs, because the spec defines no threshold price for them. A KRON A
// delivers at each fill. A Collected fill after the first also spends the bought-custody, which
// costs its filler more relay fee, and the default tip pays it.

/**
 * The extra relay fee in sompi of a Collected pair fill after the first, by the program of the
 * bought token. Such a fill carries the bought-custody input.
 *
 * The values are references measured on KCC20Ref_8x8, a later fill at 5,782,600 against a first
 * fill at 4,500,000 sompi. They stand until pair_keeper_tips.json carries a measured value per
 * program pair. A program without a value uses the largest one of the table.
 */
export const PAIR_COLLECTED_EXTRA_REFERENCE: Readonly<Record<string, bigint>> = { KCC20Ref_8x8: 5_782_600n - 4_500_000n };

/** The collected extra of a bought program, else the largest value of the reference table. */
export function pairCollectedExtra(program: string): bigint {
  const v = PAIR_COLLECTED_EXTRA_REFERENCE[program];
  return v ?? Object.values(PAIR_COLLECTED_EXTRA_REFERENCE).reduce((a, b) => (a > b ? a : b), 0n);
}

/**
 * The most later fills a Collected pair tip pays for. They are the 64 deliveries a pair order
 * funds by default, less the first.
 */
export const MAX_COLLECTED_LATER_FILLS = 63n;

/**
 * What a Collected pair order adds to its default tip, in sompi per whole A, the unit of every pair
 * tip:
 *
 *   ceil(ceil(1.25 x extra) x L x scale / amount)
 *   L = min(ceil(amount / minFill) - 1, MAX_COLLECTED_LATER_FILLS)
 *
 * That is 1.25 times the custody input's extra fee for each later fill, since the first fill has
 * no custody yet. It is spread over the whole amount like the default pair tip, so a later fill of
 * the average size pays its filler that much on top. An order of one possible fill adds 0.
 */
export function collectedPairTipExtra(extra: bigint, scale: bigint, amount: bigint, minFill: bigint): bigint {
  if (extra <= 0n || minFill <= 0n || amount <= 0n) return 0n;
  const later = ceilDiv(amount, minFill) - 1n;
  const l = later < MAX_COLLECTED_LATER_FILLS ? later : MAX_COLLECTED_LATER_FILLS;
  if (l <= 0n) return 0n;
  return ceilDiv(ceilDiv(extra * 5n, 4n) * l * scale, amount);
}

/**
 * The delivery of a pair bid, which is `resolveDelivery` without "Added to one UTXO". A GTC order
 * is resting, and any other delivers at each fill. A Top-up the user chose is an error. One that is
 * only the wallet default falls back to Each fill.
 */
export function resolvePairDelivery(o: Omit<DeliveryOrder, 'program' | 'price'>, r: DeliveryRequest | undefined, pre: PlanIssue[]): ResolvedDelivery {
  // With no program there is no top-up extra, so `topupOffer` refuses it.
  return resolveDelivery({ ...o, program: '', price: null }, r, pre);
}
