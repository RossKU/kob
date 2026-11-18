// View model of "My orders". It merges the indexer's orders with the wallet's placement records,
// which are resolved on a node. It classifies every order into a plain-words type and derives
// everything a row shows. That is the status, all-in price, amounts, expiry, refund and renewal
// times and available actions. It is pure, with no DOM and no network. The caller injects the
// clock, the node-resolved records and kob-wasm.
import type { OrderView } from '../../data/indexer-types';
import { GTC_DAYS, MARKET_AUCTION_DAA, MAX_IDLE_DAA, daaToUnix, gtcRenewalUnix, renewalUnixForExpiry } from '../../kob/daa';
import { asOrderState, baseKind, custodiesOf, custodyAmountOf, describeOrder, holdsTokens, isMultiFillMarket, isPairKind, type CurveFacts, type OrderDescription } from '../../kob/order-facts';
import { boughtOf, deliveryModeOf } from '../../kob/bought';
import { pairOfView } from '../../kob/pair-view';
import type { Clock } from '../../kob/plan-types';
import { recordCodec, retiredOf, type PlacementRecord, type RecordKind, type ResolvedRecord } from '../../kob/records';
import type { Hex, OrderState, PairKind, PairTriggerRule } from '../../kob/types';
import type { KobWasm } from '../../kob/wasm';
import { decaySlope } from '../../kob/orders/simple';
import { labelText } from '../ticket/order-label';

export const ORDER_STATUSES = ['open', 'partial', 'filled', 'cancelled', 'refunded', 'killed', 'closed', 'unknown'] as const;
export type OrderStatusUi = (typeof ORDER_STATUSES)[number];

/** Plain-words order types (i18n `orders.type.<key>`). */
export const ORDER_TYPE_KEYS = [
  'limit', 'limit-gtd', 'limit-day', 'timed', 'market', 'ioc', 'fok', 'twap', 'dca', 'dutch',
  'stop', 'trailing', 'take-profit', 'oco', 'exit', 'ifd', 'ifo', 'repeat', 'curve', 'unknown',
] as const;
// Pair orders (KobPair, KobCondPair, KobIfdPair) get the type of the KAS kind they mirror, such as
// limit, stop or IFD. The row's `pair` says they trade a token pair.
export type OrderTypeKey = (typeof ORDER_TYPE_KEYS)[number];

/**
 * The scheduled plain kinds, KAS and pair: TWAP, DCA, Dutch / rising and timed activation. They are
 * replaced from the full ticket form with their schedule.
 */
export const SCHEDULED_TYPES: readonly OrderTypeKey[] = ['twap', 'dca', 'dutch', 'timed'];

export interface OrderEntry {
  id: Hex;
  /**
   * The indexer's view, or null when it does not know the order, as after a fresh placement or an
   * indexer outage.
   */
  view: OrderView | null;
  /** The wallet's placement record, if any. */
  record: PlacementRecord | null;
  /**
   * The record resolved on a node. It is computed for orders the indexer does not know or whose
   * current state it has not proven.
   */
  resolved: ResolvedRecord | null;
}

/**
 * The covenant ids, in lower case, of the wallet's orders. They come from the list's entries and
 * its placement records.
 */
export function ownOrderIds(entries: readonly Pick<OrderEntry, 'id'>[], records: readonly Pick<PlacementRecord, 'covenantId'>[]): Set<string> {
  return new Set([...entries.map((e) => e.id), ...records.map((r) => r.covenantId)].map((id) => id.toLowerCase()));
}

/** A fill notice that concerns the wallet because it names one of its orders. */
export const isOwnFill = (own: ReadonlySet<string>, e: { order?: string }): boolean => !!e.order && own.has(e.order.toLowerCase());

const bi = (v: string | number | bigint | null | undefined): bigint | null => (v === null || v === undefined ? null : BigInt(v));
const big0 = (v: string | number | bigint | null | undefined): bigint => bi(v) ?? 0n;

/**
 * The wallet's records to resolve on the node. These are the records the indexer's list does not
 * contain, and those whose live view has no proven current state (`state_known: false`, such as a
 * continuation the indexer could not splice). The record path may still find and cancel them. An
 * ended order (filled, cancelled, refunded, killed or closed) never has a current state. Its
 * placement state describes it, so there is nothing to resolve. Looking up every finished order on
 * the node would delay the list by about 30 s over a tunnel.
 */
export function recordsToResolve(views: readonly OrderView[], records: readonly PlacementRecord[]): PlacementRecord[] {
  const settled = new Set(views.filter((v) => v.state_known || ENDED.has(v.status)).map((v) => v.covenant_id));
  return records.filter((r) => !settled.has(r.covenantId));
}

/**
 * Merges by covenant id, newest first. The sort key is the genesis DAA of the view, else the
 * record's placement DAA, and ties go by id.
 */
export function buildEntries(views: readonly OrderView[], records: readonly PlacementRecord[], resolved: ReadonlyMap<Hex, ResolvedRecord> = new Map()): OrderEntry[] {
  const recById = new Map(records.map((r) => [r.covenantId, r]));
  const out: OrderEntry[] = views.map((v) => ({ id: v.covenant_id, view: v, record: recById.get(v.covenant_id) ?? null, resolved: (!v.state_known && resolved.get(v.covenant_id)) || null }));
  const seen = new Set(out.map((e) => e.id));
  for (const r of records) {
    if (seen.has(r.covenantId)) continue;
    seen.add(r.covenantId);
    out.push({ id: r.covenantId, view: null, record: r, resolved: resolved.get(r.covenantId) ?? null });
  }
  const age = (e: OrderEntry): bigint => (e.view ? BigInt(e.view.genesis.daa) : big0(e.record?.placedAtDaa));
  return out.sort((a, b) => (age(a) === age(b) ? (a.id < b.id ? -1 : 1) : age(a) > age(b) ? -1 : 1));
}

/**
 * The current state of an order from the best source. That is the proven indexer state, else the
 * node-resolved state, else the record's original state, which may be older. An ended order has no
 * current state. The indexer's placement state then names its type and terms. It is never the basis
 * of a cancel or an amend, because the order is not live.
 */
export function entryState(e: OrderEntry, kob: KobWasm | null): { state: OrderState; source: 'indexer' | 'node' | 'record' } | null {
  const fromView = e.view && e.view.state_known ? asOrderState(e.view.state) : null;
  if (fromView) return { state: fromView, source: 'indexer' };
  const placed = e.view && ENDED.has(e.view.status) ? asOrderState(e.view.placed_state) : null;
  if (placed) return { state: placed, source: 'indexer' };
  if (e.resolved?.status === 'live' && e.resolved.order) return { state: e.resolved.order.state, source: 'node' };
  if (e.record && kob) {
    try {
      const codec = recordCodec(kob, e.record);
      return codec ? { state: codec.decode(e.record.state), source: 'record' } : null;
    } catch {
      return null;
    }
  }
  return null;
}

const ENDED = new Set(['filled', 'cancelled', 'refunded', 'killed', 'closed']);

/**
 * True when the order is under a retired template of the cancel-only table. That holds when the
 * indexer's single view says so (`cancel_only`, or its template hash is one), the node resolution
 * of the wallet's record found it under one, or the record names one.
 */
export function isCancelOnly(e: OrderEntry, kob: KobWasm | null): boolean {
  if (e.resolved?.retired) return true;
  const v = e.view;
  if (v?.cancel_only === true) return true;
  if (!kob) return false;
  if (v && retiredOf(kob, { kind: v.contract as RecordKind, templateHash: v.template_hash })) return true;
  try {
    return !!e.record && !!recordCodec(kob, e.record)?.retired;
  } catch {
    return false;
  }
}

function statusOf(e: OrderEntry): OrderStatusUi {
  if (e.view) {
    const s = e.view.status;
    return s === 'open' || s === 'partial' || s === 'filled' || s === 'cancelled' || s === 'refunded' || s === 'killed' || s === 'closed' ? s : 'unknown';
  }
  if (e.resolved) return e.resolved.status === 'live' ? 'open' : e.resolved.status === 'spent' ? 'closed' : 'unknown';
  return 'unknown';
}

/**
 * Above this an expiry is the "no fixed date" sentinel of an exit, which is 2^62, and not a date.
 */
const NO_DATE_DAA = 1n << 60n;
/**
 * A GTC's expiry is placement + 90 days. Anything within this window of that counts as GTC. The
 * window is about 2.8 hours of DAA.
 */
const GTC_TOLERANCE_DAA = 100_000n;

export interface ClassifyInput {
  /** An order kind. */
  kind: string;
  desc: OrderDescription;
  /** Placement deadline of a day order, in UTC unix seconds. */
  deadline: bigint | null;
  genesisDaa: bigint | null;
  nowDaa: bigint | null;
}

/**
 * A GTC's on-chain expiry. It is the no-date sentinel, or placement + 90 days within the tolerance
 * of the placement's DAA reading. The wallet's GTC of every kind follows `daa.ts`. Any other date
 * is a GTD the user chose.
 */
export function isGtcExpiry(expiryDaa: bigint, genesisDaa: bigint | null): boolean {
  return expiryDaa >= NO_DATE_DAA || (genesisDaa !== null && expiryDaa >= genesisDaa + MAX_IDLE_DAA - GTC_TOLERANCE_DAA && expiryDaa <= genesisDaa + MAX_IDLE_DAA + GTC_TOLERANCE_DAA);
}

/**
 * Maps an order's state to the type the user chose (order-types.md). The heuristics use only state
 * fields the builders set.
 */
export function orderTypeKey(i: ClassifyInput): OrderTypeKey {
  const d = i.desc;
  const kind = baseKind(i.kind as OrderState['kind']);
  if (kind === 'KobCurveAsk') return 'curve';
  if (kind === 'KobAsk' || kind === 'KobBid' || kind === 'KobPair') {
    const a = d.auction;
    if (a && a.interval > 0n && a.maxFill > 0n) return d.side === 'sell' ? 'twap' : 'dca';
    if (d.tif === 'fok') return 'fok';
    if (d.tif === 'ioc') return a && a.slope > 0n ? 'market' : 'ioc';
    // A market order of several fills is the market auction resting for its short life
    // (order-facts.ts `isMultiFillMarket`).
    if (isMultiFillMarket(d)) return 'market';
    // A limit that crossed the book is placed as an auction from the touch to the limit over
    // MARKET_AUCTION_DAA, one step per DAA (orders/simple.ts). It is still the user's limit and not
    // a Dutch schedule. A Dutch of exactly that shape reads as a limit too.
    if (a && a.slope > 0n && !(a.decayStep === 1n && a.slope === decaySlope(d.price, a.priceEnd, MARKET_AUCTION_DAA, 1n))) return 'dutch';
    if (i.nowDaa !== null && d.activeFrom > i.nowDaa) return 'timed';
    if (i.deadline !== null) return 'limit-day';
    return isGtcExpiry(d.expiryDaa, i.genesisDaa) || i.genesisDaa === null ? 'limit' : 'limit-gtd';
  }
  if (kind === 'KobCondAsk' || kind === 'KobCondBid' || kind === 'KobCondPair') {
    const t = d.trigger;
    if (d.booked) return 'exit';
    if (!t) return 'unknown';
    const hasStop = t.stopPrice > 0n;
    const hasTp = d.price > 0n;
    if (t.trailStep > 0n) return 'trailing';
    if (hasStop && hasTp) return 'oco';
    if (hasStop) return 'stop';
    return hasTp ? 'take-profit' : 'unknown';
  }
  if (kind === 'KobIfdAsk' || kind === 'KobIfdBid' || kind === 'KobIfdPair') {
    const e = d.entry;
    if (!e) return 'unknown';
    if (e.rptAmount > 0n) return 'repeat';
    const x = e.exit;
    if (x && x.trigger && x.trigger.stopPrice > 0n && x.price > 0n) return 'ifo';
    return 'ifd';
  }
  return 'unknown';
}

export interface ExpiryInfo {
  kind: 'gtc' | 'gtd' | 'day' | 'none';
  daa: bigint | null;
  /** Approximate wall-clock time of the DAA expiry in unix seconds, shown as "expires around". */
  approxUnix: bigint | null;
  /** Day orders: the exact placement deadline. */
  deadlineUnix: bigint | null;
}

export interface OrderRowModel {
  id: Hex;
  entry: OrderEntry;
  token: Hex | null;
  side: 'sell' | 'buy';
  contract: string;
  typeKey: OrderTypeKey;
  status: OrderStatusUi;
  /**
   * Where the facts come from: the indexer, a node-resolved record, or only the record, which may
   * be older.
   */
  source: 'indexer' | 'node' | 'record' | 'none';
  live: boolean;
  /** Open or partial past its expiry, so a refund is pending. */
  expired: boolean;
  /**
   * Limit, take-profit and entry limit: the state price in sompi per whole token of `scale` base
   * units. It is null for none, such as a pure stop or a budget market order. For an auction it is
   * the END price, its bound.
   */
  price: bigint | null;
  /**
   * Auctions (market, marketable limit, Dutch): the start price, which is expected at the touch.
   * `price` is then the worst price and not a limit.
   */
  auctionStart: bigint | null;
  stopPrice: bigint | null;
  /** Priority tip, in sompi per whole token. */
  tip: bigint;
  /** A buy pays price + tip. A sell receives price - tip, never below 0. */
  allInPrice: bigint | null;
  /** Base units at placement, null when unknown. */
  amountTotal: bigint | null;
  /**
   * Base units left, null when unknown. For a bid it is its buying power, see `amountEstimated`.
   */
  amountLeft: bigint | null;
  /** The amount left is an estimate, because a bid holds a KAS budget. */
  amountEstimated: boolean;
  /**
   * A plain KAS bid's quantity is its KAS budget in sompi. That is the order UTXO's value minus the
   * refund tip, the reserve and the delivery carrier. `placed` comes from the wallet's placement
   * record. It is null on a browser that never placed the order, because the indexer's view carries
   * no placement value. `left` comes from the order's current UTXO. It is 0 once the order is
   * filled and null for an order that ended otherwise. The whole field is null for every other
   * kind.
   */
  budget: { placed: bigint | null; left: bigint | null } | null;
  /**
   * The order's scale in base units per whole token, which is the denominator of its prices. It is
   * 1 when unknown.
   */
  scale: bigint;
  /** Smallest fill in base units, unless a fill takes everything left. Null when unknown. */
  minFill: bigint | null;
  /** Current auction price and state for market and Dutch orders, from the indexer. */
  auction: { kind: string; currentPrice: bigint; complete: boolean } | null;
  /** Stop and entry orders: the trigger has fired. */
  armed: boolean | null;
  startsAtDaa: bigint | null;
  expiry: ExpiryInfo;
  /** GTC only: when to renew, on day 85 of 90, and whether that time has come. */
  renewalUnix: bigint | null;
  renewalDue: boolean;
  refundDueDaa: bigint | null;
  /** The order can be refunded by its maker right now. */
  refundable: boolean;
  /**
   * Ask kinds: the indexer proved exactly one custody UTXO with the expected amount. Null when not
   * known.
   */
  custodyOk: boolean | null;
  /**
   * A delivering buy (bid kinds and pair orders): its mode, where '1' is Collected. Null for the
   * other kinds.
   */
  delivery: '0' | '1' | '2' | null;
  /**
   * A mode-1 order holding what it bought: the token, the base units and whether the indexer proved
   * its UTXO. Null when not known.
   */
  bought: { token: Hex; amount: bigint; ok: boolean | null } | null;
  /** Claim, Claim and merge, and Claim and sell. The order stays open. */
  canClaim: boolean;
  placementTx: Hex | null;
  parent: Hex | null;
  label: string | null;
  canCancel: boolean;
  canRefund: boolean;
  /** An amend form exists for this order. */
  canAmend: boolean;
  /**
   * An if-done entry: the exit it commits, decoded from its `exitState` with kob-wasm and booked
   * for what the entry fills. It holds the take-profit and stop state prices, null when the exit
   * has no such leg, and the amount still to enter in base units. It is absent for other kinds or
   * without kob-wasm.
   */
  plannedExit?: { takeProfit: bigint | null; stop: bigint | null; amount: bigint | null } | null;
  /**
   * Why a live order cannot be cancelled although it was found:
   * - 'no-extension': its custody token cannot be rebuilt because the token's extension commitment
   *   is unknown.
   * - 'custody-missing': there is no custody UTXO at its address.
   * - 'old-template': it was placed with an older contract version.
   */
  cancelBlocked: 'no-extension' | 'custody-missing' | 'old-template' | null;
  /**
   * The wallet's record of this order names an order template that this build neither pins nor
   * keeps in its cancel-only table, an older contract version. Nothing can be built for it here, so
   * there is no Cancel button and one line says why.
   */
  oldTemplate: boolean;
  /**
   * The order is under a retired template of the cancel-only table, an older contract version. Its
   * maker's cancel is the only action this app builds for it, with no amend, sweep or refund. Its
   * terms decode with today's kind of the same name.
   */
  cancelOnly: boolean;
  strayCount: number;
  /**
   * A live bid whose escrow no longer funds one base unit, so the indexer counts amount 0. It is
   * hidden from the book. It can be refunded by a cancel or topped up.
   */
  unfundable: boolean;
  /**
   * The executor's pre-simulation of the order's next fill failed. The token program may have
   * frozen or blacklisted it. It is not in the book depth.
   */
  possiblyFrozen: boolean;
  /**
   * A live order the indexer does not list (`listed: false`). It is indexed, with status and
   * cancel, but in no book, so it never fills. `reason` is the indexer's `unlisted_reason` code,
   * which ui/orders/unlisted.ts words. It is null when an older executor names none. The whole
   * field is null for a listed order, an ended order (nothing left to list) or an order the indexer
   * does not know.
   */
  unlisted: { reason: string | null } | null;
  /**
   * A pair order (KobPair, KobCondPair, KobIfdPair), or null for KAS-quoted orders. It carries:
   * - base A (`token`) and quote B,
   * - its prices in B base units per whole A, where `scale` is scale(A) base units, so `price`,
   *   `stopPrice` and `auctionStart` of the row are B prices too and `allInPrice` is null because
   *   the tip is KAS,
   * - what it holds of each token: an ask's A, a bid's B escrow, and a sell-first entry's A and B
   *   prefund,
   * - the indexer's current quote of an auction,
   * - the trigger rule of a stop.
   */
  pair: PairRowFacts | null;
  /**
   * A stepped sell curve (`KobCurveAsk`) with its table and current level. Absent for other kinds.
   */
  curve?: CurveFacts | null;
}

export interface PairRowFacts {
  kind: PairKind;
  base: Hex;
  quote: Hex;
  /** A held in custody and B held in custody, exact, from the state. */
  escrowA: bigint;
  escrowB: bigint;
  /** A sell-first entry's buy-back prefund in B base units per whole A. It is 0 otherwise. */
  prefund: bigint;
  /**
   * The auction or decay price now, in B per whole A (indexer `pair.quote_now` or a `pair_*`
   * auction). It is null when not running.
   */
  quoteNow: bigint | null;
  /**
   * kob-wasm `pairTriggerRule` of a stop (conditional or stop entry), when kob-wasm described the
   * order.
   */
  trigger: PairTriggerRule | null;
  /** KAS tip prefunded per whole A in sompi. It is never part of the B price. */
  tipKas: bigint;
  /**
   * Base units per whole B as the state names them, when the state is known. A token without a
   * registry entry reads on it.
   */
  quoteScale?: bigint;
}

export interface DescribeContext {
  kob: KobWasm | null;
  clock: Clock | null;
}

/**
 * Which amend form an order has:
 * - 'limit': plain limits, in place where the covenant allows, which is an ask keeping its amount
 *   and a bid.
 * - 'cond': the quick form of non-trailing stop, take-profit and OCO orders.
 * - 'replace': the full ticket form re-planned as a cancel-replace. It covers trailing stops and
 *   IFD / IFO entries that do not repeat. It is also reachable from the quick 'cond' form for band,
 *   trigger and expiry.
 * Pair orders have no in-place amend in the protocol. Their resting limits, stops, take-profits,
 * OCO and IFD / IFO entries are replaced from the full pair ticket form, as a cancel-replace.
 * Repeat entries and booked exits have none. The position is cancelled as a whole, and its exits
 * re-arm the entry.
 */
export function amendKind(state: OrderState, typeKey: OrderTypeKey): 'limit' | 'cond' | 'replace' | 'curve' | null {
  const k = baseKind(state.kind);
  // A curve has its levels left, level size, tip and minimum fill amended in place.
  if (k === 'KobCurveAsk') return 'curve';
  // TWAP, DCA, Dutch / rising and timed orders, KAS and pair, are replaced from the full ticket
  // form with their schedule (replace-intent.ts).
  if ((k === 'KobPair' || k === 'KobAsk' || k === 'KobBid') && SCHEDULED_TYPES.includes(typeKey)) return 'replace';
  if (k === 'KobPair') return typeKey === 'limit' || typeKey === 'limit-gtd' || typeKey === 'limit-day' ? 'replace' : null;
  if (k === 'KobCondPair') return typeKey === 'stop' || typeKey === 'take-profit' || typeKey === 'oco' || typeKey === 'trailing' ? 'replace' : null;
  if (k === 'KobIfdPair') return typeKey === 'ifd' || typeKey === 'ifo' ? 'replace' : null;
  if (typeKey === 'limit' || typeKey === 'limit-gtd' || typeKey === 'limit-day') return k === 'KobAsk' || k === 'KobBid' ? 'limit' : null;
  if (typeKey === 'stop' || typeKey === 'take-profit' || typeKey === 'oco') return k === 'KobCondAsk' || k === 'KobCondBid' ? 'cond' : null;
  if (typeKey === 'trailing') return k === 'KobCondAsk' || k === 'KobCondBid' ? 'replace' : null;
  if (typeKey === 'ifd' || typeKey === 'ifo') return k === 'KobIfdAsk' || k === 'KobIfdBid' ? 'replace' : null;
  return null;
}

/**
 * The exit an if-done entry commits, for what the entry has still to fill. `describeOrder` decodes
 * it from the entry's `exitState` with kob-wasm.
 */
function plannedExitOf(desc: OrderDescription | null, amountLeft: bigint | null): OrderRowModel['plannedExit'] {
  const x = desc?.entry?.exit;
  if (!x) return null;
  const takeProfit = x.price > 0n ? x.price : null;
  const stop = x.trigger && x.trigger.stopPrice > 0n ? x.trigger.stopPrice : null;
  if (takeProfit === null && stop === null) return null;
  return { takeProfit, stop, amount: amountLeft ?? desc?.amountLeft ?? null };
}

/**
 * The KAS budget of a plain KAS bid (see `OrderRowModel.budget`). What is not budget is the refund
 * tip, the reserve and the delivery carrier. It is the same in every state of the order, so the
 * placement record's value and the current UTXO's value give the placed and the remaining budget
 * alike.
 */
function budgetOf(e: OrderEntry, state: OrderState | null, desc: OrderDescription | null, status: OrderStatusUi): OrderRowModel['budget'] {
  if (!state || !desc || desc.pair || baseKind(state.kind) !== 'KobBid') return null;
  const nonBudget = desc.refundTip + desc.reservedKas;
  const net = (value: bigint): bigint => (value > nonBudget ? value - nonBudget : 0n);
  const current = bi(e.view?.current?.value ?? null) ?? (e.resolved?.status === 'live' && e.resolved.order ? bi(e.resolved.order.amount) : null);
  const left = current !== null ? net(current) : status === 'filled' ? 0n : null;
  let placed = e.record ? net(big0(e.record.value)) : null;
  // A top-up replaces the order with a new record. A remaining budget above the record's therefore
  // means the record is older than the order.
  if (placed !== null && left !== null && left > placed) placed = left;
  return { placed, left };
}

export function describeEntry(e: OrderEntry, ctx: DescribeContext): OrderRowModel {
  const es = entryState(e, ctx.kob);
  const status = statusOf(e);
  const live = status === 'open' || status === 'partial';
  const v = e.view;
  const nowDaa = ctx.clock ? ctx.clock.daa : v?.current_daa != null ? BigInt(v.current_daa) : null;
  const desc = es ? describeOrder(es.state, ctx.kob ?? undefined) : null;
  const scale = desc ? desc.scale : v?.scale ? BigInt(v.scale) : 1n;
  const genesisDaa = v ? BigInt(v.genesis.daa) : e.record ? bi(e.record.placedAtDaa) : null;
  const deadline = bi(v?.deadline ?? null) ?? bi(e.record?.deadline ?? null);
  const kind = es ? es.state.kind : (v?.contract ?? e.record?.kind ?? 'unknown');
  const typeKey: OrderTypeKey = es && desc ? orderTypeKey({ kind: es.state.kind, desc, deadline, genesisDaa: genesisDaa && genesisDaa > 0n ? genesisDaa : null, nowDaa }) : 'unknown';

  // An auction (market, marketable limit, Dutch) is bounded by its END price, which is the order's
  // limit. The start is only what it expects.
  const decays = !!desc?.auction && desc.auction.slope > 0n;
  const rawPrice = desc ? (decays ? desc.auction!.priceEnd : desc.price) : bi(v?.price) !== null ? big0(v?.price) : null;
  const auctionStart = decays && desc && desc.price > 0n ? desc.price : null;
  const price = rawPrice !== null && rawPrice > 0n ? rawPrice : null;
  const stopRaw = desc?.trigger ? desc.trigger.stopPrice : desc?.entry ? desc.entry.entryStop : 0n;
  const stopPrice = stopRaw > 0n ? stopRaw : null;
  const tip = desc ? desc.tip : big0(v?.tip);
  const side: 'sell' | 'buy' = desc ? desc.side : v ? (v.side === 1 ? 'sell' : 'buy') : e.record && holdsTokens(e.record.kind as OrderState['kind']) ? 'sell' : 'buy';
  // A pair order's tip is KAS and never part of its B price, so its price is its all-in price,
  // shown once.
  const pairOrder = !!desc?.pair || (!!v && isPairKind(v.contract));
  const allInPrice = price === null || pairOrder ? null : side === 'buy' ? price + tip : price > tip ? price - tip : 0n;

  // Expiry.
  const expDaa = desc ? desc.expiryDaa : bi(v?.expiry_daa ?? null);
  let expiry: ExpiryInfo = { kind: 'none', daa: null, approxUnix: null, deadlineUnix: deadline };
  if (expDaa !== null && expDaa > 0n) {
    const noDate = expDaa >= NO_DATE_DAA;
    const approx = !noDate && ctx.clock ? daaToUnix(ctx.clock, expDaa) : null;
    // A GTC of any kind is placement + 90 days on chain. Kinds are stop, OCO, trailing, if-done
    // entry, TWAP / DCA, Dutch and timed limit. It is a GTC, with a renewal reminder and renew, and
    // not a dated GTD.
    const gtc = noDate || typeKey === 'limit' || (typeKey !== 'limit-gtd' && isGtcExpiry(expDaa, genesisDaa && genesisDaa > 0n ? genesisDaa : null));
    expiry = { kind: deadline !== null ? 'day' : gtc ? 'gtc' : 'gtd', daa: noDate ? null : expDaa, approxUnix: approx, deadlineUnix: deadline };
  }

  // GTC renewal reminder, on day 85 of 90, from the on-chain expiry. The refund is due at
  // min(expiryDaa, utxoDaa + 90 days), and a GTC's expiryDaa is placement + 90 days, so fills do
  // not extend its life. An exit's "no date" sentinel has no reminder, because the idle bound
  // applies. Without a dated expiry the placement time is the fallback.
  let renewalUnix: bigint | null = null;
  let renewalDue = false;
  if (live && expiry.kind === 'gtc' && ctx.clock) {
    const placedUnix = e.record && big0(e.record.placedAtUnix) > 0n ? big0(e.record.placedAtUnix) : null;
    renewalUnix = expiry.daa !== null ? renewalUnixForExpiry(ctx.clock, expiry.daa) : placedUnix !== null ? gtcRenewalUnix(placedUnix) : null;
    renewalDue = renewalUnix !== null && ctx.clock.unixSeconds >= renewalUnix;
  }

  // Refund.
  const refundDue = bi(v?.refund_due_daa ?? null) ?? (expiry.daa !== null && live ? expiry.daa : null);
  const refundable = live && nowDaa !== null && refundDue !== null && nowDaa >= refundDue;
  // A day order has ended at its deadline, 00:00 UTC, when conforming matchers stop filling it. Its
  // on-chain expiry, the refund time, follows a little later. From the deadline on it reads "ended,
  // refund pending" and not live.
  const deadlinePassed = live && (v?.deadline_passed === true || (deadline !== null && ctx.clock !== null && ctx.clock.unixSeconds >= deadline));
  const expired = live && (deadlinePassed || (v ? v.expired : expDaa !== null && expDaa < NO_DATE_DAA && nowDaa !== null && nowDaa >= expDaa));

  const amountTotal = v ? bi(v.initial_amount) : bi(e.record?.amount ?? null);
  const amountLeft = v ? bi(v.amount_left) : desc ? desc.amountLeft : null;
  const auction = v?.auction ? { kind: v.auction.kind, currentPrice: big0(v.auction.current_price), complete: v.auction.complete } : null;
  const pair = pairRowOf(es?.state ?? null, desc, v, auction, ctx.kob);
  const armedRaw = desc?.trigger ? desc.trigger.armed : desc?.entry && desc.entry.entryStop > 0n ? desc.entry.armed : null;
  const custodyOk = v?.custody ? v.custody.ok : null;
  const activeFrom = desc && desc.activeFrom > 0n ? desc.activeFrom : null;
  const source: OrderRowModel['source'] = es ? es.source : 'none';
  // The indexer's proven state can be cancelled from the indexer's data. Otherwise only a
  // node-resolved record can, and an ask only with its custody.
  const viaView = !!v && v.state_known && v.current !== null;
  const r = e.resolved;
  const oldTemplate = r?.status === 'old-template';
  const cancelOnly = isCancelOnly(e, ctx.kob);
  const bf = es ? boughtOf(es.state) : null;
  const bought = bf ? { token: bf.token, amount: bf.amount, ok: v?.bought_custody ? v.bought_custody.ok : null } : null;
  const needsCustody = !!es && ((holdsTokens(es.state.kind) && (custodyAmountOf(es.state) ?? 0n) > 0n) || !!bf);
  const cancelBlocked: OrderRowModel['cancelBlocked'] = viaView
    ? null
    : oldTemplate
      ? 'old-template'
      : r?.status === 'live' && needsCustody && (bf ? true : !r.custody)
        ? r.custodyIssue === 'no-extension' ? 'no-extension' : 'custody-missing'
        : null;
  const viaRecord = r?.status === 'live' && cancelBlocked === null;

  return {
    // An unbound curve, created in its token's genesis, names no token in its state. The indexer's
    // `token` is its custody's.
    id: e.id, entry: e, token: desc && !desc.curve?.unbound ? desc.tokenCovId : (v?.token ?? desc?.tokenCovId ?? null), side, contract: v?.contract ?? kind, typeKey, status, source, live, expired, price, auctionStart, stopPrice, tip, allInPrice,
    budget: budgetOf(e, es ? es.state : null, desc, status),
    amountTotal, amountLeft, amountEstimated: v ? v.amount_estimated : e.resolved !== null && es?.source === 'node' && baseKind(es.state.kind) === 'KobBid',
    scale, minFill: desc ? desc.minFill : bi(v?.min_fill ?? null),
    auction, armed: armedRaw === null ? null : armedRaw !== 0n, startsAtDaa: activeFrom !== null && nowDaa !== null && activeFrom > nowDaa ? activeFrom : null,
    expiry, renewalUnix, renewalDue, refundDueDaa: refundDue, refundable, custodyOk,
    delivery: es && ('mode' in es.state.state || v?.delivery_mode != null) ? deliveryModeOf(es.state) : null,
    bought,
    // A claim spends the bought custody the indexer proved.
    canClaim: live && viaView && !cancelOnly && !!bought && bought.ok !== false && !!v?.bought_custody?.utxo,
    placementTx: v?.genesis.txid ?? e.record?.txid ?? null, parent: v?.parent ?? null,
    // Records of older versions store the form's ids ("ifd buy"). They are shown in the ticket's
    // words.
    label: labelText(e.record?.label),
    // A cancel needs a live order whose current UTXO can be found. That is a proven indexer state
    // or a node-resolved record.
    canCancel: live && (viaView || viaRecord),
    canRefund: refundable && !cancelOnly,
    canAmend: live && !cancelOnly && !!es && amendKind(es.state, typeKey) !== null && es.source !== 'record',
    plannedExit: plannedExitOf(desc, amountLeft),
    cancelBlocked: live || oldTemplate ? cancelBlocked : null,
    oldTemplate,
    cancelOnly,
    strayCount: v?.strays ? v.strays.filter((s) => !s.spent).length : 0,
    // A sold-out repeat entry waiting for its exits to re-arm it also has amount 0 left. That is a
    // healthy position and not an unfundable bid.
    unfundable: live && side === 'buy' && v !== null && bi(v.amount_left) === 0n && v.repeat?.role !== 'entry',
    possiblyFrozen: live && v?.possibly_frozen === true,
    unlisted: live && v?.listed === false ? { reason: v.unlisted_reason ?? null } : null,
    pair,
    ...(desc?.curve ? { curve: desc.curve } : {}),
  };
}

const DEC = /^\d+$/;

/**
 * The pair facts of a row. They come from the described state, with exact custodies and trigger
 * rule, or, when the state is unknown, from the indexer view's `pair` object (docs/ops/executor.md
 * 5.4). The current quote of an auction is the indexer's, which is `quote_now` or a `pair_*`
 * auction's `current_price`.
 */
function pairRowOf(
  state: OrderState | null, desc: OrderDescription | null, v: OrderView | null, auction: OrderRowModel['auction'], kob: KobWasm | null,
): PairRowFacts | null {
  const pv = pairOfView(v);
  const pairAuction = auction && auction.kind.startsWith('pair_') && !auction.complete ? auction.currentPrice : null;
  const viewNow = pv?.quote_now != null && DEC.test(pv.quote_now) ? BigInt(pv.quote_now) : null;
  const priceOfView = pv?.price != null && DEC.test(pv.price) ? BigInt(pv.price) : null;
  // A running auction shows its price now. A resting limit's `quote_now` equals its price, so it is
  // not shown twice.
  const quoteNow = pairAuction ?? (viewNow !== null && viewNow !== priceOfView ? viewNow : null);
  const p = desc?.pair ?? null;
  if (p) {
    const held = state ? custodiesOf(state, kob ?? undefined) : p.custodies;
    const sum = (role: 'base' | 'quote') => held.filter((c) => c.role === role).reduce((a, c) => a + c.amount, 0n);
    return { kind: p.kind, base: p.base.covId, quote: p.quote.covId, escrowA: sum('base'), escrowB: sum('quote'), prefund: p.prefund, quoteNow, trigger: p.triggerRule, tipKas: desc!.tip, quoteScale: p.quote.scale };
  }
  if (!pv || !v || !isPairKind(v.contract)) return null;
  const custody = (role: string) => pv.custodies.filter((c) => c.role === role).reduce((a, c) => a + (DEC.test(c.expected_amount) ? BigInt(c.expected_amount) : 0n), 0n);
  return {
    kind: v.contract as PairKind, base: pv.base, quote: pv.quote, escrowA: custody('base'), escrowB: custody('quote'),
    prefund: pv.prefund != null && DEC.test(pv.prefund) ? BigInt(pv.prefund) : 0n, quoteNow, trigger: null, tipKas: big0(v.tip),
  };
}

/** Rows for every entry, in entry order. */
export const describeEntries = (entries: readonly OrderEntry[], ctx: DescribeContext): OrderRowModel[] => entries.map((e) => describeEntry(e, ctx));

/** Day count of the GTC rule, for the reminder text ("renew before day 85 of 90"). */
export const GTC_RULE = { renewDay: 85, days: Number(GTC_DAYS) } as const;

/** Counts by status for the list header. */
export function countByStatus(rows: readonly OrderRowModel[]): { live: number; done: number; unknown: number } {
  let live = 0;
  let unknown = 0;
  for (const r of rows) {
    if (r.live) live++;
    else if (r.status === 'unknown') unknown++;
  }
  return { live, done: rows.length - live - unknown, unknown };
}
