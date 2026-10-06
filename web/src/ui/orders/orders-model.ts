// View model of "My orders": merges the indexer's orders with the wallet's placement records (resolved on a node), classifies every order into a
// plain-words type, and derives everything a row shows (status, all-in price, amounts, expiry, refund and renewal times, available actions). Pure:
// no DOM, no network; the caller injects the clock, the node-resolved records and kob-wasm.
import type { OrderView } from '../../data/indexer-types';
import { GTC_DAYS, MAX_IDLE_DAA, daaToUnix, gtcRenewalUnix, renewalUnixForExpiry } from '../../kob/daa';
import { asOrderState, baseKind, custodiesOf, custodyAmountOf, describeLegacy, describeOrder, holdsTokens, isLegacyState, isPairKind, type OrderDescription } from '../../kob/order-facts';
import { pairOfView } from '../../kob/pair-view';
import type { Clock } from '../../kob/plan-types';
import { recordCodec, type PlacementRecord, type ResolvedRecord } from '../../kob/records';
import type { Hex, LegacyState, OrderState, PairKind, PairTriggerRule } from '../../kob/types';
import type { KobWasm } from '../../kob/wasm';

export const ORDER_STATUSES = ['open', 'partial', 'filled', 'cancelled', 'refunded', 'killed', 'closed', 'unknown'] as const;
export type OrderStatusUi = (typeof ORDER_STATUSES)[number];

/** Plain-words order types (i18n `orders.type.<key>`). */
export const ORDER_TYPE_KEYS = [
  'limit', 'limit-gtd', 'limit-day', 'timed', 'market', 'ioc', 'fok', 'twap', 'dca', 'dutch',
  'stop', 'trailing', 'take-profit', 'oco', 'exit', 'ifd', 'ifo', 'repeat', 'cross', 'unknown',
] as const;
// 'cross': an order of the RETIRED cross limit (KobCross, an older contract version: cancel only). Pair orders (KobPair, KobCondPair, KobIfdPair)
// get the type of the KAS kind they mirror (limit, stop, IFD, ...); the row's `pair` says they trade a token pair.
export type OrderTypeKey = (typeof ORDER_TYPE_KEYS)[number];

export interface OrderEntry {
  id: Hex;
  /** the indexer's view (null when it does not know the order: fresh placement, indexer outage) */
  view: OrderView | null;
  /** the wallet's placement record, if any */
  record: PlacementRecord | null;
  /** the record resolved on a node (computed for orders the indexer does not know, or whose current state it has not proven) */
  resolved: ResolvedRecord | null;
}

const bi = (v: string | number | bigint | null | undefined): bigint | null => (v === null || v === undefined ? null : BigInt(v));
const big0 = (v: string | number | bigint | null | undefined): bigint => bi(v) ?? 0n;

/**
 * The wallet's records to resolve on the node: those the indexer's list does not contain, and those whose view has no PROVEN current state
 * (`state_known: false`, e.g. a continuation the indexer could not splice): the record path may still find (and cancel) them.
 */
export function recordsToResolve(views: readonly OrderView[], records: readonly PlacementRecord[]): PlacementRecord[] {
  const proven = new Set(views.filter((v) => v.state_known).map((v) => v.covenant_id));
  return records.filter((r) => !proven.has(r.covenantId));
}

/** Merge by covenant id, newest first (genesis DAA of the view, else the record's placement DAA; ties by id). */
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

/** The current state of an order from the best source: proven indexer state, node-resolved state, else the record's ORIGINAL state (may be older). */
export function entryState(e: OrderEntry, kob: KobWasm | null): { state: OrderState; source: 'indexer' | 'node' | 'record' } | null {
  const fromView = e.view && e.view.state_known ? asOrderState(e.view.state) : null;
  if (fromView) return { state: fromView, source: 'indexer' };
  if (e.resolved?.status === 'live' && e.resolved.order) return { state: e.resolved.order.state, source: 'node' };
  if (e.record && kob) {
    try {
      // a record of a retired template decodes through that template: its LEGACY lot state (describeLegacy reads it)
      const codec = recordCodec(kob, e.record);
      return codec ? { state: codec.decode(e.record.state), source: 'record' } : null;
    } catch {
      return null;
    }
  }
  return null;
}

function statusOf(e: OrderEntry): OrderStatusUi {
  if (e.view) {
    const s = e.view.status;
    return s === 'open' || s === 'partial' || s === 'filled' || s === 'cancelled' || s === 'refunded' || s === 'killed' || s === 'closed' ? s : 'unknown';
  }
  if (e.resolved) return e.resolved.status === 'live' ? 'open' : e.resolved.status === 'spent' ? 'closed' : 'unknown';
  return 'unknown';
}

/** Above this an expiry is the "no fixed date" sentinel of an exit (2^62), not a date. */
const NO_DATE_DAA = 1n << 60n;
/** A GTC's expiry is placement + 90 days; anything within this window of that counts as GTC (about 2.8 hours of DAA). */
const GTC_TOLERANCE_DAA = 100_000n;

export interface ClassifyInput {
  /** an order kind, or a retired kind (`KobCross`) of an order of a retired template */
  kind: string;
  desc: OrderDescription;
  /** placement deadline of a day order (UTC unix seconds) */
  deadline: bigint | null;
  genesisDaa: bigint | null;
  nowDaa: bigint | null;
}

/** Maps an order's state to the type the user chose (order-types.md). Heuristics use only state fields the builders set. */
export function orderTypeKey(i: ClassifyInput): OrderTypeKey {
  const d = i.desc;
  if (i.kind === 'KobCross') return 'cross';
  const kind = baseKind(i.kind as OrderState['kind']);
  if (kind === 'KobAsk' || kind === 'KobBid' || kind === 'KobPair') {
    const a = d.auction;
    if (a && a.interval > 0n && a.maxFill > 0n) return d.side === 'sell' ? 'twap' : 'dca';
    if (d.tif === 'fok') return 'fok';
    if (d.tif === 'ioc') return a && a.slope > 0n ? 'market' : 'ioc';
    if (a && a.slope > 0n) return 'dutch';
    if (i.nowDaa !== null && d.activeFrom > i.nowDaa) return 'timed';
    if (i.deadline !== null) return 'limit-day';
    const gtc = d.expiryDaa >= NO_DATE_DAA || (i.genesisDaa !== null && d.expiryDaa >= i.genesisDaa + MAX_IDLE_DAA - GTC_TOLERANCE_DAA && d.expiryDaa <= i.genesisDaa + MAX_IDLE_DAA + GTC_TOLERANCE_DAA);
    return gtc || i.genesisDaa === null ? 'limit' : 'limit-gtd';
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
  /** approximate wall-clock time of the DAA expiry (unix seconds): "expires around" */
  approxUnix: bigint | null;
  /** day orders: the exact placement deadline */
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
  /** where the facts come from: the indexer, a node-resolved record, or only the (possibly older) record */
  source: 'indexer' | 'node' | 'record' | 'none';
  live: boolean;
  /** open / partial past its expiry: a refund is pending */
  expired: boolean;
  /**
   * limit / take-profit / entry limit, the state price: sompi per whole token of `scale` base units (null: none, e.g. a pure stop or a budget
   * market order); an auction's END price (its bound)
   */
  price: bigint | null;
  /** auctions (market, marketable limit, Dutch): the start price (expected at the touch); `price` is then the worst price, not a limit */
  auctionStart: bigint | null;
  stopPrice: bigint | null;
  /** priority tip, sompi per whole token */
  tip: bigint;
  /** buy pays price + tip, sell receives price - tip (never below 0) */
  allInPrice: bigint | null;
  /** base units at placement, null when unknown */
  amountTotal: bigint | null;
  /** base units left (a bid: its buying power, `amountEstimated`), null when unknown */
  amountLeft: bigint | null;
  /** the amount left is an estimate (a bid holds a KAS budget) */
  amountEstimated: boolean;
  /** the order's scale: base units per whole token (the denominator of its prices); 1 when unknown */
  scale: bigint;
  /** smallest fill (base units) unless a fill takes everything left; null when unknown */
  minFill: bigint | null;
  /** current auction price etc. for market / dutch orders (indexer) */
  auction: { kind: string; currentPrice: bigint; complete: boolean } | null;
  /** stop / entry orders: the trigger has fired */
  armed: boolean | null;
  startsAtDaa: bigint | null;
  expiry: ExpiryInfo;
  /** GTC only: when to renew (day 85 of 90) and whether that time has come */
  renewalUnix: bigint | null;
  renewalDue: boolean;
  refundDueDaa: bigint | null;
  /** the order can be refunded by its maker right now */
  refundable: boolean;
  /** ask kinds: the indexer proved exactly one custody UTXO with the expected amount (null: not known) */
  custodyOk: boolean | null;
  placementTx: Hex | null;
  parent: Hex | null;
  label: string | null;
  canCancel: boolean;
  canRefund: boolean;
  /** an amend form exists for this order */
  canAmend: boolean;
  /**
   * why a live order cannot be cancelled although it was found: 'no-extension' (its custody token cannot be rebuilt: the token's extension
   * commitment is unknown), 'custody-missing' (no custody UTXO at its address), 'old-template' (placed with an older contract version)
   */
  cancelBlocked: 'no-extension' | 'custody-missing' | 'old-template' | null;
  /** the wallet's record of this order names an order template this build does not pin (an older contract version, C5-02) */
  oldTemplate: boolean;
  /** placed with a RETIRED template this build can still spend: only the maker's cancel is offered (no amend, no refund) */
  retiredTemplate: boolean;
  strayCount: number;
  /** a live bid whose escrow no longer funds one base unit (the indexer counts amount 0): hidden from the book, refund (cancel) or top it up */
  unfundable: boolean;
  /** the executor's pre-simulation of the order's next fill failed: the token program may have frozen / blacklisted it (not in the book depth) */
  possiblyFrozen: boolean;
  /**
   * a pair order (KobPair, KobCondPair, KobIfdPair): base A (`token`) and quote B, its prices in B BASE UNITS PER WHOLE A (`scale` = scale(A)
   * base units; `price`, `stopPrice`, `auctionStart` of the row are then B prices too, `allInPrice` is null: the tip is KAS), what it holds
   * of each token (an ask's A, a bid's B escrow, a sell-first entry's A and B prefund), the indexer's current quote of an auction, the trigger rule
   * of a stop; null for KAS-quoted orders
   */
  pair: PairRowFacts | null;
}

export interface PairRowFacts {
  kind: PairKind;
  base: Hex;
  quote: Hex;
  /** A held in custody, B held in custody (exact, from the state) */
  escrowA: bigint;
  escrowB: bigint;
  /** a sell-first entry's buy-back prefund, B base units per whole A (0 otherwise) */
  prefund: bigint;
  /** the auction / decay price now, B per whole A (indexer `pair.quote_now` or a `pair_*` auction), null when not running */
  quoteNow: bigint | null;
  /** kob-wasm `pairTriggerRule` of a stop (conditional or stop entry), when kob-wasm described the order */
  trigger: PairTriggerRule | null;
  /** KAS tip prefunded per whole A (sompi): never part of the B price */
  tipKas: bigint;
}

export interface DescribeContext {
  kob: KobWasm | null;
  clock: Clock | null;
}

/**
 * Which amend form an order has: 'limit' plain limits (in place where the covenant allows: an ask keeping its amount, a bid), 'cond' the quick
 * form of non-trailing stop / take-profit / OCO orders, 'replace' the full ticket form re-planned as a cancel-replace (trailing stops, IFD / IFO
 * entries that do not repeat; also reachable from the quick 'cond' form for band / trigger / expiry). Pair orders have no in-place amend in the
 * protocol: their resting limits, stops, take-profits, OCO and IFD / IFO entries are replaced from the full pair ticket form (cancel-replace).
 * Repeat entries and booked exits have none: the position is cancelled as a whole (their exits re-arm the entry). The retired cross limit has
 * none either (cancel only).
 */
export function amendKind(state: OrderState, typeKey: OrderTypeKey): 'limit' | 'cond' | 'replace' | null {
  const k = baseKind(state.kind);
  if (k === 'KobPair') return typeKey === 'limit' || typeKey === 'limit-gtd' || typeKey === 'limit-day' ? 'replace' : null;
  if (k === 'KobCondPair') return typeKey === 'stop' || typeKey === 'take-profit' || typeKey === 'oco' || typeKey === 'trailing' ? 'replace' : null;
  if (k === 'KobIfdPair') return typeKey === 'ifd' || typeKey === 'ifo' ? 'replace' : null;
  if (typeKey === 'limit' || typeKey === 'limit-gtd' || typeKey === 'limit-day') return k === 'KobAsk' || k === 'KobBid' ? 'limit' : null;
  if (typeKey === 'stop' || typeKey === 'take-profit' || typeKey === 'oco') return k === 'KobCondAsk' || k === 'KobCondBid' ? 'cond' : null;
  if (typeKey === 'trailing') return k === 'KobCondAsk' || k === 'KobCondBid' ? 'replace' : null;
  if (typeKey === 'ifd' || typeKey === 'ifo') return k === 'KobIfdAsk' || k === 'KobIfdBid' ? 'replace' : null;
  return null;
}

export function describeEntry(e: OrderEntry, ctx: DescribeContext): OrderRowModel {
  const es = entryState(e, ctx.kob);
  const status = statusOf(e);
  const live = status === 'open' || status === 'partial';
  const v = e.view;
  const nowDaa = ctx.clock ? ctx.clock.daa : v?.current_daa != null ? BigInt(v.current_daa) : null;
  // an order of a RETIRED template carries its legacy lot state (an older contract version: cancel only)
  const legacy = !!es && isLegacyState(es.state);
  const desc = es ? (legacy ? describeLegacy(es.state as unknown as LegacyState) : describeOrder(es.state, ctx.kob ?? undefined)) : null;
  const scale = desc ? desc.scale : v?.scale ? BigInt(v.scale) : 1n;
  const genesisDaa = v ? BigInt(v.genesis.daa) : e.record ? bi(e.record.placedAtDaa) : null;
  const deadline = bi(v?.deadline ?? null) ?? bi(e.record?.deadline ?? null);
  const kind = es ? es.state.kind : (v?.contract ?? e.record?.kind ?? 'unknown');
  const typeKey: OrderTypeKey = es && desc ? orderTypeKey({ kind: es.state.kind, desc, deadline, genesisDaa: genesisDaa && genesisDaa > 0n ? genesisDaa : null, nowDaa }) : 'unknown';

  // an auction (market, marketable limit, Dutch) is bounded by its END price: that is the order's limit; the start is only what it expects
  const decays = !!desc?.auction && desc.auction.slope > 0n;
  const rawPrice = desc ? (decays ? desc.auction!.priceEnd : desc.price) : bi(v?.price) !== null ? big0(v?.price) : null;
  const auctionStart = decays && desc && desc.price > 0n ? desc.price : null;
  const price = rawPrice !== null && rawPrice > 0n ? rawPrice : null;
  const stopRaw = desc?.trigger ? desc.trigger.stopPrice : desc?.entry ? desc.entry.entryStop : 0n;
  const stopPrice = stopRaw > 0n ? stopRaw : null;
  const tip = desc ? desc.tip : big0(v?.tip);
  const side: 'sell' | 'buy' = desc ? desc.side : v ? (v.side === 1 ? 'sell' : 'buy') : e.record && holdsTokens(e.record.kind as OrderState['kind']) ? 'sell' : 'buy';
  // a pair order's tip is KAS (never part of its B price): its price IS its all-in price, shown once
  const pairOrder = !!desc?.pair || (!!v && isPairKind(v.contract));
  const allInPrice = price === null || pairOrder ? null : side === 'buy' ? price + tip : price > tip ? price - tip : 0n;

  // expiry
  const expDaa = desc ? desc.expiryDaa : bi(v?.expiry_daa ?? null);
  let expiry: ExpiryInfo = { kind: 'none', daa: null, approxUnix: null, deadlineUnix: deadline };
  if (expDaa !== null && expDaa > 0n) {
    const noDate = expDaa >= NO_DATE_DAA;
    const approx = !noDate && ctx.clock ? daaToUnix(ctx.clock, expDaa) : null;
    expiry = { kind: deadline !== null ? 'day' : typeKey === 'limit-gtd' ? 'gtd' : noDate || typeKey === 'limit' ? 'gtc' : 'gtd', daa: noDate ? null : expDaa, approxUnix: approx, deadlineUnix: deadline };
  }

  // GTC renewal reminder (day 85 of 90), from the ON-CHAIN expiry: the refund is due at min(expiryDaa, utxoDaa + 90 days) and a GTC's expiryDaa is
  // placement + 90 days, so fills do not extend its life (counting from the last fill came up to 85 days too late). An exit's "no date" sentinel
  // has no reminder (the idle bound applies); without a dated expiry the placement time is the fallback.
  let renewalUnix: bigint | null = null;
  let renewalDue = false;
  if (live && expiry.kind === 'gtc' && ctx.clock) {
    const placedUnix = e.record && big0(e.record.placedAtUnix) > 0n ? big0(e.record.placedAtUnix) : null;
    renewalUnix = expiry.daa !== null ? renewalUnixForExpiry(ctx.clock, expiry.daa) : placedUnix !== null ? gtcRenewalUnix(placedUnix) : null;
    renewalDue = renewalUnix !== null && ctx.clock.unixSeconds >= renewalUnix;
  }

  // refund
  const refundDue = bi(v?.refund_due_daa ?? null) ?? (expiry.daa !== null && live ? expiry.daa : null);
  const refundable = live && nowDaa !== null && refundDue !== null && nowDaa >= refundDue;
  // C5 W-15: a day order has ended at its deadline (00:00 UTC: conforming matchers stop filling it) although its on-chain expiry, the refund time,
  // follows a little later: it reads "ended, refund pending" from the deadline on, not live
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
  // the indexer's proven state can be cancelled from the indexer's data; otherwise only a node-resolved record, and an ask only with its custody
  const viaView = !!v && v.state_known && v.current !== null;
  const r = e.resolved;
  const oldTemplate = r?.status === 'old-template';
  const retiredTemplate = legacy || !!r?.retired || (!!e.record && !!ctx.kob && !oldTemplate && !viaView && recordCodec(ctx.kob, e.record)?.retired != null);
  const needsCustody = !!es && holdsTokens(es.state.kind) && (custodyAmountOf(es.state) ?? 0n) > 0n;
  const cancelBlocked: OrderRowModel['cancelBlocked'] = viaView
    ? null
    : oldTemplate
      ? 'old-template'
      : r?.status === 'live' && needsCustody && !r.custody
        ? r.custodyIssue === 'no-extension' ? 'no-extension' : 'custody-missing'
        : null;
  const viaRecord = r?.status === 'live' && cancelBlocked === null;

  return {
    id: e.id, entry: e, token: desc ? desc.tokenCovId : (v?.token ?? null), side, contract: v?.contract ?? kind, typeKey, status, source, live, expired, price, auctionStart, stopPrice, tip, allInPrice,
    amountTotal, amountLeft, amountEstimated: v ? v.amount_estimated : e.resolved !== null && es?.source === 'node' && baseKind(es.state.kind) === 'KobBid',
    scale, minFill: desc ? desc.minFill : bi(v?.min_fill ?? null),
    auction, armed: armedRaw === null ? null : armedRaw !== 0n, startsAtDaa: activeFrom !== null && nowDaa !== null && activeFrom > nowDaa ? activeFrom : null,
    expiry, renewalUnix, renewalDue, refundDueDaa: refundDue, refundable, custodyOk, placementTx: v?.genesis.txid ?? e.record?.txid ?? null, parent: v?.parent ?? null,
    label: e.record?.label ?? null,
    // a cancel needs a live order whose current UTXO can be found: proven indexer state, or a node-resolved record
    canCancel: live && (viaView || viaRecord),
    canRefund: refundable && !retiredTemplate,
    canAmend: live && !!es && amendKind(es.state, typeKey) !== null && es.source !== 'record' && !retiredTemplate,
    cancelBlocked: live || oldTemplate ? cancelBlocked : null,
    oldTemplate,
    retiredTemplate,
    strayCount: v?.strays ? v.strays.filter((s) => !s.spent).length : 0,
    // a sold-out repeat ENTRY waiting for its exits to re-arm it also has amount 0 left: that is a healthy position, not an unfundable bid
    unfundable: live && side === 'buy' && v !== null && bi(v.amount_left) === 0n && v.repeat?.role !== 'entry',
    possiblyFrozen: live && v?.possibly_frozen === true,
    pair,
  };
}

const DEC = /^\d+$/;

/**
 * The pair facts of a row: from the described state (exact custodies, trigger rule) or, when the state is unknown, from the indexer view's `pair`
 * object (docs/ops/executor.md 5.4). The current quote of an auction is the indexer's (`quote_now` or a `pair_*` auction's `current_price`).
 */
function pairRowOf(
  state: OrderState | null, desc: OrderDescription | null, v: OrderView | null, auction: OrderRowModel['auction'], kob: KobWasm | null,
): PairRowFacts | null {
  const pv = pairOfView(v);
  const pairAuction = auction && auction.kind.startsWith('pair_') && !auction.complete ? auction.currentPrice : null;
  const viewNow = pv?.quote_now != null && DEC.test(pv.quote_now) ? BigInt(pv.quote_now) : null;
  const priceOfView = pv?.price != null && DEC.test(pv.price) ? BigInt(pv.price) : null;
  // a running auction: its price now (a resting limit's quote_now equals its price: not shown twice)
  const quoteNow = pairAuction ?? (viewNow !== null && viewNow !== priceOfView ? viewNow : null);
  const p = desc?.pair ?? null;
  if (p && p.kind !== 'KobCross') {
    const held = state ? custodiesOf(state, kob ?? undefined) : p.custodies;
    const sum = (role: 'base' | 'quote') => held.filter((c) => c.role === role).reduce((a, c) => a + c.amount, 0n);
    return { kind: p.kind, base: p.base.covId, quote: p.quote.covId, escrowA: sum('base'), escrowB: sum('quote'), prefund: p.prefund, quoteNow, trigger: p.triggerRule, tipKas: desc!.tip };
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
