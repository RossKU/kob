// View model of the PRE-SIGN confirmation screen: `SigningSummary` (kob/decode.ts, derived from the built transaction ONLY) -> sections and rows of
// display strings. Nothing here reads planner or form state: the screen shows what the transaction does, and the planner's claims are only
// compared against it by `decodeSigning` (a mismatch is a blocking finding).
//
// Pure and DOM-free. Text comes through an injected translator (`tr`, default the app's `t`) so the model is unit-tested in both languages and
// the component stays a thin renderer. All amounts arrive as bigint and are formatted exactly (units.ts).
import type { SigningKind, SigningSummary, DecodedInput, DecodedOrder, DecodedOutput, DecodedSpend, DecodedTrigger, LockedKas, TokenNet, TokenRef } from '../../kob/decode';
import type { WalletNotice } from '../../kob/decode';
import { daaToUnix, formatJst, formatUtc } from '../../kob/daa';
import { UNLIMITED_REPEAT_COUNT } from '../../kob/orders/cond-common';
import { stopWorstPrice } from '../../kob/orders/cond-legs';
import { baseKind, type OrderDescription } from '../../kob/order-facts';
import type { Clock } from '../../kob/plan-types';
import type { TokenRegistry } from '../../kob/registry';
import type { Hex, PayloadRecord } from '../../kob/types';
import { formatKas, formatPricePerToken, formatTokenAmount, quoteOf } from '../../kob/units';
import { formatDateTime, t, type Params } from '../../i18n';
import type { FeeDisclosure } from './fee-disclosure';

export type Translate = (key: string, params?: Params) => string;
export type Tone = 'normal' | 'good' | 'warn' | 'bad' | 'muted';

export interface Row {
  id: string;
  label: string;
  value: string;
  detail?: string;
  tone: Tone;
}

export interface Card {
  /** covenant id (orders) or `input-<n>` */
  id: string;
  title: string;
  /** short badge: side / verification */
  badge: string;
  rows: Row[];
  /** the exit an if-done entry commits to */
  children: Card[];
  /** the output that carries it did not verify */
  flagged: boolean;
}

export interface Section {
  id: 'spend' | 'create' | 'sweep' | 'close' | 'locked' | 'tokens' | 'back' | 'others' | 'fee' | 'net' | 'issue';
  title: string;
  /** one explaining sentence under the title */
  note?: string;
  rows: Row[];
  cards: Card[];
}

export interface AdvancedInput { index: number; text: string; willSign: boolean }
export interface AdvancedOutput { index: number; text: string; flagged: boolean }
export interface AdvancedModel {
  txid: string;
  signatures: number;
  fee: { paid: string; declared: string; minimum: string; mode: 'relay' | 'priority'; feeMass: number; priorityMass: number; storageMass: number };
  inputs: AdvancedInput[];
  outputs: AdvancedOutput[];
  payload: string[];
}

/** A finding of the decoder (`SigningIssue`) or of the issuance check; `text` (when set) is already translated, else the UI translates by `code`. */
export interface Finding {
  code: string;
  severity: 'blocking' | 'warning' | 'info';
  message: string;
  params?: Params;
  input?: number;
  output?: number;
  text?: string;
}

export interface ConfirmModel {
  kind: SigningKind | 'issue';
  heading: string;
  intro: string;
  sections: Section[];
  blocking: Finding[];
  warnings: Finding[];
  info: Finding[];
  /** the decoder found nothing that must stop the user (the UI still needs the explicit acknowledgement) */
  canSign: boolean;
  notices: WalletNotice[];
  advanced: AdvancedModel;
}

export interface ConfirmContext {
  registry?: TokenRegistry | null;
  /** node clock read when the screen opened: turns DAA scores into "around <time>" */
  clock?: Clock | null;
  tr?: Translate;
  /** format a unix time for the reader (default: locale date-time + UTC) */
  time?: (unix: bigint) => string;
  /** the fee rate and where it came from (`feeDisclosureOf(built)`): rows in the fee section, notes among the warnings / info */
  fee?: FeeDisclosure | null;
}

const GROUP = { group: ',' };
const kas = (v: bigint): string => `${formatKas(v, GROUP)} KAS`;
const short = (h: string): string => (h.length > 12 ? `${h.slice(0, 4)}...${h.slice(-4)}` : h);
const DAA_PER_SECOND = 10;
/** Exit orders committed inside an entry carry a "never" expiry (2^62); anything past two years is not a date. */
const FAR_FUTURE_DAA = 2n * 365n * 86_400n * BigInt(DAA_PER_SECOND);

// ------------------------------------------------------------------------------------------------ token labels

/** The identity line of a token: `TICKER (abcd...1234) [verified]` for registry tokens, `unknown token (abcd...1234)` otherwise. Never the ticker alone. */
export function tokenLabel(ref: TokenRef, tr: Translate = t, registry?: TokenRegistry | null): string {
  const info = registry?.byCovenantId.get(ref.covenantId);
  if (!info) return tr('confirm.token.unknown', { id: short(ref.covenantId) });
  const state = info.status === 'delisted' ? 'delisted' : info.official ? 'official' : info.verified ? 'verified' : 'unverified';
  if (info.openList) return tr('confirm.token.labelOpen', { id: short(info.covenantId), state: tr(`confirm.token.${state}`) });
  return tr('confirm.token.label', { ticker: info.ticker, id: short(info.covenantId), state: tr(`confirm.token.${state}`) });
}

/** A token amount in human units when the decimals are known (registry), else raw base units. */
export function tokenAmountText(ref: TokenRef, amount: bigint, tr: Translate = t): string {
  return ref.decimals === null
    ? tr('confirm.amount.baseUnits', { amount: amount.toString() })
    : `${formatTokenAmount(amount, ref.decimals, GROUP)} ${ref.ticker ?? ''}`.trim();
}

function refOf(registry: TokenRegistry | null | undefined, covenantId: Hex): TokenRef {
  const t0 = registry?.byCovenantId.get(covenantId);
  return t0
    ? { covenantId, ticker: t0.ticker, decimals: t0.decimals, display: '', inRegistry: true, tradable: t0.tradable }
    : { covenantId, ticker: null, decimals: null, display: '', inRegistry: false, tradable: false };
}

// ------------------------------------------------------------------------------------------------ order descriptions

/** The order type as the user knows it, re-derived from the state fields (a tx cannot claim one type and carry another). */
export function orderTypeCode(d: OrderDescription, dayDeadline: bigint | null = null): string {
  const a = d.auction;
  // a pair order has the type of the KAS kind it mirrors (its card names the pair)
  switch (baseKind(d.kind)) {
    case 'KobAsk':
    case 'KobBid':
    case 'KobPair': {
      if (d.tif === 'fok') return a ? 'marketFok' : 'fok';
      if (d.tif === 'ioc') return a ? 'market' : 'ioc';
      if (a && a.interval > 0n) return d.side === 'sell' ? 'twap' : 'dca';
      if (a && a.slope > 0n) return d.side === 'sell' ? 'dutch' : 'rising';
      return dayDeadline !== null ? 'limitDay' : 'limit';
    }
    case 'KobCondAsk':
    case 'KobCondBid':
    case 'KobCondPair': {
      const tr0 = d.trigger;
      if (!tr0) return 'limit';
      const stop = tr0.stopPrice > 0n;
      const tp = d.price > 0n;
      if (stop && tp) return tr0.trailStep > 0n ? 'trailingOco' : 'oco';
      if (stop) return tr0.trailStep > 0n ? 'trailingStop' : 'stop';
      return 'takeProfit';
    }
    case 'KobIfdBid':
    case 'KobIfdAsk':
    case 'KobIfdPair': {
      const e = d.entry;
      const both = !!e?.exit && e.exit.trigger !== null && e.exit.trigger.stopPrice > 0n && e.exit.price > 0n;
      const repeat = !!e && e.rptAmount > 0n;
      const stopEntry = !!e && e.entryStop > 0n;
      return `${repeat ? 'repeat' : ''}${repeat ? (both ? 'Ifo' : 'Ifd') : both ? 'ifo' : 'ifd'}${stopEntry ? 'Stop' : ''}`;
    }
    default:
      return 'limit';
  }
}

interface Fmt {
  tr: Translate;
  ref: TokenRef;
  clock: Clock | null;
  time: (unix: bigint) => string;
  /** resolves other tokens (a pair order's quote token B) */
  registry?: TokenRegistry | null;
}

const secondsOf = (daa: bigint): string => (Number(daa) / DAA_PER_SECOND).toString();

const row = (id: string, label: string, value: string, tone: Tone = 'normal', detail?: string): Row => ({ id, label, value, tone, ...(detail ? { detail } : {}) });

/**
 * A state price (sompi per whole token of `d.scale` base units) as KAS per whole token when the token's decimals are known; else as sompi per
 * `scale` base units. The detail names the scale when it is not the token's own `10^decimals` (a token of more than 9 decimals).
 */
/** A price DELTA (sompi per `scale` base units) as the bare KAS amount per whole token (the template prints the unit), e.g. a trailing step. */
function kasPerToken(price: bigint, d: OrderDescription, f: Fmt): string {
  return f.ref.decimals === null || d.scale <= 0n ? formatKas(price, GROUP) : formatPricePerToken(price, f.ref.decimals, d.scale, GROUP);
}

function priceText(price: bigint, d: OrderDescription, f: Fmt): { value: string; detail?: string } {
  if (f.ref.decimals === null || d.scale <= 0n) return { value: `${formatKas(price, GROUP)} KAS`, detail: f.tr('confirm.f.perScale', { scale: d.scale.toString() }) };
  const whole = 10n ** BigInt(f.ref.decimals);
  return {
    value: `${formatPricePerToken(price, f.ref.decimals, d.scale, GROUP)} KAS / ${f.ref.ticker ?? ''}`.trim(),
    ...(d.scale !== whole ? { detail: f.tr('confirm.f.perScale', { scale: d.scale.toString() }) } : {}),
  };
}

const approxTime = (daa: bigint, f: Fmt): string | null => (f.clock ? f.time(daaToUnix(f.clock, daa)) : null);

/** A price of a pair order (B base units per WHOLE A) in B per whole A when B's decimals are known; else raw base units of B per whole A. */
function pairPriceText(price: bigint, d: OrderDescription, f: Fmt): { value: string; detail?: string } {
  const bRef = refOf(f.registry, d.pair!.quote.covId);
  const aName = f.ref.ticker ?? short(f.ref.covenantId);
  return { value: `${tokenAmountText(bRef, price, f.tr)} / ${aName}` };
}

/**
 * The pair rows of a pair order (KobPair, KobCondPair, KobIfdPair), from the state alone: which token it sells for which, what it holds of each token
 * (an ask's A, a bid's B escrow, a sell-first entry's A and B prefund), what the whole amount guarantees at the order's own price (a sell receives at
 * least `ceil(n x price / scale(A))` of B, a buy pays at most `floor(...)`), the KAS tip and carriers, and the trigger rule of a pair stop: it arms
 * when the two KAS books imply a rate beyond the stop (a resting order of each token on the costly side, each rested and filled together), or when a
 * resting pair order at or beyond the stop is filled. A pair order never routes by itself: a matcher may net it, route it or fill it from inventory.
 */
function pairRows(d: OrderDescription, f: Fmt): Row[] {
  const { tr } = f;
  const x = d.pair!;
  const bRef = refOf(f.registry, x.quote.covId);
  const aName = f.ref.ticker ?? short(f.ref.covenantId);
  const bName = bRef.ticker ?? short(x.quote.covId);
  const rows: Row[] = [];
  rows.push(row('pairPair', tr('confirm.f.pairPair'), tr(d.side === 'sell' ? 'confirm.f.pairSells' : 'confirm.f.pairBuys', { base: aName, quote: bName }), bRef.inRegistry ? 'normal' : 'warn', bRef.inRegistry ? undefined : tr('confirm.f.pairUnknownQuote', { id: short(x.quote.covId) })));
  if (x.escrowA > 0n) rows.push(row('pairEscrowA', tr('confirm.f.pairEscrowA'), tokenAmountText(f.ref, x.escrowA, tr)));
  if (x.escrowB > 0n) rows.push(row('pairEscrowB', tr(x.kind === 'KobIfdPair' && d.side === 'sell' ? 'confirm.f.pairPrefundB' : 'confirm.f.pairEscrowB'), tokenAmountText(bRef, x.escrowB, tr)));
  // the whole amount at the order's own bound: a resting / auction pair order (KobPair); a conditional or an entry is described by its legs
  if (x.kind === 'KobPair' && d.amountLeft !== null && d.scale > 0n) {
    const bound = x.priceEnd ?? x.price;
    if (bound > 0n) {
      const sell = d.side === 'sell';
      const total = quoteOf(d.amountLeft, bound, d.scale, sell ? 'up' : 'down');
      rows.push(row('pairTotal', tr(sell ? 'confirm.f.pairReceiveMin' : 'confirm.f.pairPayMax'), tokenAmountText(bRef, total, tr), 'good', tr(sell ? 'confirm.f.pairReceiveHint' : 'confirm.f.pairPayHint')));
    }
  }
  const r = x.triggerRule;
  if (r) {
    const stop = pairPriceText(BigInt(r.stop), d, f).value;
    rows.push(
      row('pairTrigger', tr('confirm.f.pairTrigger'), tr(r.direction === 'fallsTo' ? 'ticket.val.pairTriggerSell' : 'ticket.val.pairTriggerBuy', { stop, base: aName, quote: bName, seconds: secondsOf(BigInt(r.minRestDaa)) }), 'warn',
        tr('ticket.val.pairTriggerMin', { a: tokenAmountText(f.ref, BigInt(r.minTouch), tr), b: r.minTouchB !== null ? tokenAmountText(bRef, BigInt(r.minTouchB), tr) : '-' })),
    );
  }
  if (x.deliveryCarrier > 0n) rows.push(row('deliveryCarrier', tr('confirm.f.deliveryCarrier'), kas(x.deliveryCarrier), 'muted', tr('confirm.f.pairCarrierHint', { quote: bName, base: aName })));
  return rows;
}

/** Rows that describe one order state: quantity, prices (all-in), timing, auction, trigger, entry, repeat. */
export function orderRows(d: OrderDescription, extra: { value: bigint | null; locked: LockedKas | null; deadline: bigint | null }, f: Fmt): Row[] {
  const { tr } = f;
  // a pair order: its prices are B per whole A, its tip KAS per whole A (never part of its B price)
  const pair = !!d.pair;
  const rows: Row[] = pair ? pairRows(d, f) : [];
  const sell = d.side === 'sell';
  const p = (price: bigint) => (pair ? pairPriceText(price, d, f) : priceText(price, d, f));
  const tip = pair ? 0n : d.tip;

  // quantity
  if (d.amountLeft !== null) {
    rows.push(row('amount', tr('confirm.f.amount'), tokenAmountText(f.ref, d.tokenAmount ?? d.amountLeft, tr)));
  } else if (extra.locked) {
    // a plain bid is a KAS budget: how much that buys depends on the price
    const budget = extra.locked.escrow;
    rows.push(row('budget', tr('confirm.f.budget'), kas(budget), 'normal', tr('confirm.f.budgetAmount')));
  }
  // the smallest fill unless a fill takes everything left (the order's anti-dust rule)
  rows.push(row('minFill', tr('confirm.f.minFill'), tokenAmountText(f.ref, d.minFill, tr), 'muted', tr('confirm.f.minFillHint')));

  // prices
  const isCond = d.trigger !== null;
  if (d.price > 0n) {
    const x = p(d.price);
    const label = d.entry ? tr('confirm.f.entryLimit') : isCond ? tr('confirm.f.takeProfit') : d.auction && d.auction.slope > 0n ? tr('confirm.f.startPrice') : tr('confirm.f.limit');
    rows.push(row('price', label, x.value, 'normal', x.detail));
    // an auction (market, marketable limit, Dutch) only guarantees its END price: the covenant fills at max(priceEnd, price - slope * t) for a
    // sell, min(...) for a buy, so the all-in guarantee is derived from `priceEnd` and labelled the worst price (matcher.md 10.2); the start is
    // only what the order is expected to get at the touch
    const decays = !!d.auction && d.auction.slope > 0n; // priceEnd 0 decays to 0: the worst price IS 0
    const bound = decays ? d.auction!.priceEnd : d.price;
    const allIn = sell ? (bound > tip ? bound - tip : 0n) : bound + tip;
    if (tip > 0n || d.entry || isCond || d.tif !== null || decays) {
      const a = p(allIn);
      const label = decays ? (sell ? tr('confirm.f.worstAllInReceive') : tr('confirm.f.worstAllInPay')) : sell ? tr('confirm.f.allInReceive') : tr('confirm.f.allInPay');
      rows.push(row('allIn', label, a.value, decays ? 'warn' : 'normal', a.detail));
    }
  }
  if (tip > 0n) {
    const x = p(tip);
    rows.push(row('tip', tr('confirm.f.tip'), x.value, 'muted', x.detail));
  }
  if (pair && d.tip > 0n) {
    // KAS per whole A, prefunded on the order UTXO and released to the filler per fill (rounded down): never taken from the B
    const x = priceText(d.tip, d, f);
    rows.push(row('tip', tr('confirm.f.tip'), x.value, 'muted', tr('confirm.f.pairTipHint')));
  }

  // auctions and schedules
  const a = d.auction;
  if (a) {
    if (a.interval > 0n) {
      rows.push(row('schedule', tr('confirm.f.schedule'), tr('confirm.f.scheduleValue', { amount: tokenAmountText(f.ref, a.maxFill, tr), seconds: secondsOf(a.interval) })));
    }
    if (a.slope > 0n && a.priceEnd > 0n) {
      const steps = a.slope > 0n ? (Number(d.price > a.priceEnd ? d.price - a.priceEnd : a.priceEnd - d.price) / Number(a.slope)) * Number(a.decayStep || 1n) : 0;
      const to = p(a.priceEnd);
      rows.push(row('auction', tr('confirm.f.auction'), to.value, 'warn', tr('confirm.f.auctionValue', { seconds: (Math.ceil(steps) / DAA_PER_SECOND).toString() })));
    }
  }
  if (d.tif === 'ioc') rows.push(row('tif', tr('confirm.f.tif'), tr('confirm.f.tifIoc')));
  if (d.tif === 'fok') rows.push(row('tif', tr('confirm.f.tif'), tr('confirm.f.tifFok')));

  // trigger legs
  const trg = d.trigger;
  if (trg && trg.stopPrice > 0n) {
    const s = p(trg.stopPrice);
    rows.push(row('stop', tr('confirm.f.stop'), s.value, 'warn', s.detail));
    // the worst fill after the trigger: the band's end (sell: stop - band, buy: stop + band; multiply first, as the covenant). A stop is NOT a
    // guaranteed price: it fills inside the band or not at all (matcher.md 10.6), as the ticket's stopWorst row says
    const worst = p(stopWorstPrice(sell ? 'sell' : 'buy', trg.stopPrice, trg.slipBps));
    rows.push(row('stopWorst', tr('confirm.f.stopWorst'), worst.value, 'warn', tr(sell ? 'confirm.f.stopWorstSell' : 'confirm.f.stopWorstBuy')));
    rows.push(row('band', tr('confirm.f.band'), tr('confirm.f.bandValue', { percent: (Number(trg.slipBps) / 100).toString(), seconds: secondsOf(trg.bandDaa) })));
    // the direction of the trigger: a sell stop arms on a resting SELL quoting at or below it, a buy stop on a resting BUY at or above it (a pair
    // stop: the pair trigger rule row above)
    if (!pair) rows.push(row('exposure', tr('confirm.f.exposure'), tr(sell ? 'confirm.f.exposureValueSell' : 'confirm.f.exposureValueBuy', { amount: tokenAmountText(f.ref, trg.minTouch, tr), seconds: secondsOf(trg.minRestDaa) })));
    if (trg.keeperTip > 0n) rows.push(row('keeperTip', tr('confirm.f.keeperTip'), kas(trg.keeperTip), 'muted'));
    if (trg.trailStep > 0n) {
      const step = pair ? p(trg.trailStep).value : kasPerToken(trg.trailStep, d, f);
      const gap = pair ? p(trg.trailGap).value : kasPerToken(trg.trailGap, d, f);
      rows.push(row('trail', tr(pair ? 'confirm.f.pairTrail' : 'confirm.f.trail'), tr(pair ? 'confirm.f.pairTrailValue' : 'confirm.f.trailValue', { step, gap, minutes: (Number(trg.trailWait) / DAA_PER_SECOND / 60).toString() })));
    }
    rows.push(row('armed', tr('confirm.f.armed'), trg.armed > 0n ? tr('confirm.f.armedYes') : tr('confirm.f.armedNo'), 'muted'));
  }

  // if-done entry
  const e = d.entry;
  if (e) {
    if (e.entryStop > 0n) {
      const s = p(e.entryStop);
      rows.push(row('entryStop', tr('confirm.f.entryStop'), s.value, 'warn', tr('confirm.f.entryStopDetail', { seconds: secondsOf(e.bandDaa) })));
      if (!pair) rows.push(row('entryExposure', tr('confirm.f.exposure'), tr(sell ? 'confirm.f.exposureValueSell' : 'confirm.f.exposureValueBuy', { amount: tokenAmountText(f.ref, e.minTouch, tr), seconds: secondsOf(e.minRestDaa) })));
    }
    if (e.exitCarrier > 0n) rows.push(row('exitCarrier', tr('confirm.f.exitCarrier'), kas(e.exitCarrier), 'muted'));
    if (e.deliveryCarrier > 0n && !pair) rows.push(row('deliveryCarrier', tr('confirm.f.deliveryCarrier'), kas(e.deliveryCarrier), 'muted'));
    if (e.prefund > 0n) rows.push(row('prefund', tr('confirm.f.prefund'), p(e.prefund).value, 'muted'));
    if (e.rptAmount > 0n) {
      const cycle = d.amountLeft ?? 0n;
      const repeats = cycle > 0n ? (e.rptAmount - 1n) / cycle : 0n;
      const cycleText = tokenAmountText(f.ref, cycle, tr);
      // the wallet's "unlimited" is a count so large that only the 90-day bound ends it: say so instead of printing the sentinel
      const shown = repeats >= UNLIMITED_REPEAT_COUNT ? `${tr('ticket.val.repeatUnlimited')} (${tr('ticket.val.repeatCycle', { amount: cycleText })})` : tr('confirm.f.repeatValue', { count: repeats.toString(), amount: cycleText });
      rows.push(row('repeat', tr('confirm.f.repeat'), repeats > 0n ? shown : tr('confirm.f.repeatNone'), 'warn'));
    }
  }
  if (d.booked) {
    rows.push(row('booked', tr('confirm.f.booked'), tr('confirm.f.bookedValue', { parent: short(d.booked.parent), price: p(d.booked.rptPrice).value }), 'muted'));
  }
  rows.push(...timingRows(d, extra, f));
  return rows;
}

/** Activation, expiry / day deadline, refund tip and the order value. */
function timingRows(d: OrderDescription, extra: { value: bigint | null; deadline: bigint | null }, f: Fmt): Row[] {
  const { tr } = f;
  const rows: Row[] = [];
  if (d.activeFrom > 0n) {
    const at = approxTime(d.activeFrom, f);
    rows.push(row('activeFrom', tr('confirm.f.activeFrom'), at ?? tr('confirm.f.daa', { daa: d.activeFrom.toString() })));
  }
  if (extra.deadline !== null) {
    const at = extra.deadline;
    rows.push(row('deadline', tr('confirm.f.dayDeadline'), `${formatUtc(at)} / ${formatJst(at)}`, 'warn'));
  } else if (d.expiryDaa > 0n) {
    if (d.expiryDaa >= (f.clock?.daa ?? 0n) + FAR_FUTURE_DAA || (d.expiryDaa >= 1n << 60n)) {
      rows.push(row('expiry', tr('confirm.f.expiry'), tr('confirm.f.expiryExit'), 'muted'));
    } else {
      const at = approxTime(d.expiryDaa, f);
      rows.push(row('expiry', tr('confirm.f.expiry'), at ? tr('confirm.f.expiryAround', { time: at }) : tr('confirm.f.daa', { daa: d.expiryDaa.toString() }), 'muted', tr('confirm.f.expiryHint')));
    }
  }
  // R-13: the refund tip is paid only to whoever refunds the order after its expiry; a cancel or the last fill returns it to the maker
  if (d.refundTip > 0n) rows.push(row('refundTip', tr('confirm.f.refundTip'), kas(d.refundTip), 'muted', tr('confirm.f.refundTipHint')));
  if (extra.value !== null) rows.push(row('value', tr('confirm.f.orderValue'), kas(extra.value), 'muted'));
  return rows;
}

function orderTitle(d: OrderDescription, deadline: bigint | null, f: Fmt): string {
  const type = orderTypeCode(d, deadline);
  const kind = f.tr(`confirm.order.${type}`);
  return f.tr('confirm.order.title', { side: f.tr(d.side === 'sell' ? 'common.side.sell' : 'common.side.buy'), type: kind });
}

function orderCard(id: string, d: OrderDescription, extra: { value: bigint | null; locked: LockedKas | null; deadline: bigint | null; verified: boolean }, f: Fmt): Card {
  const children: Card[] = [];
  if (d.entry?.exit) {
    const x = d.entry.exit;
    children.push({
      id: `${id}:exit`, title: f.tr('confirm.exitOf', { title: orderTitle(x, null, f) }), badge: f.tr(x.side === 'sell' ? 'common.side.sell' : 'common.side.buy'),
      rows: orderRows(x, { value: null, locked: null, deadline: null }, { ...f }), children: [], flagged: false,
    });
  }
  return {
    id, title: orderTitle(d, extra.deadline, f), badge: extra.verified ? f.tr('confirm.verified') : f.tr('confirm.unverified'),
    rows: orderRows(d, extra, f), children, flagged: !extra.verified,
  };
}

// ------------------------------------------------------------------------------------------------ the model

const KAS_KINDS: Record<string, string> = { carriers: 'confirm.locked.carriers', escrow: 'confirm.locked.escrow', refundTips: 'confirm.locked.refundTips', keeperTips: 'confirm.locked.keeperTips', reserves: 'confirm.locked.reserves' };

/** `a` minus `b`, field by field (never below zero). */
function subLocked(a: LockedKas, b: LockedKas): LockedKas {
  const d = (x: bigint, y: bigint): bigint => (x > y ? x - y : 0n);
  return { total: d(a.total, b.total), carriers: d(a.carriers, b.carriers), escrow: d(a.escrow, b.escrow), refundTips: d(a.refundTips, b.refundTips), keeperTips: d(a.keeperTips, b.keeperTips), reserves: d(a.reserves, b.reserves) };
}

function lockedRows(l: LockedKas, tr: Translate): Row[] {
  const rows: Row[] = [];
  for (const k of ['escrow', 'carriers', 'refundTips', 'keeperTips', 'reserves'] as const) {
    if (l[k] > 0n) rows.push(row(`locked-${k}`, tr(KAS_KINDS[k]!), kas(l[k]), 'normal', k === 'carriers' ? tr('confirm.locked.carriersHint') : k === 'refundTips' ? tr('confirm.f.refundTipHint') : undefined));
  }
  return rows;
}

function tokenSectionRows(tokens: TokenNet[], f: Fmt, registry: TokenRegistry | null | undefined): { spend: Row[]; locked: Row[]; back: Row[]; others: Row[]; net: Row[] } {
  const { tr } = f;
  const out = { spend: [] as Row[], locked: [] as Row[], back: [] as Row[], others: [] as Row[], net: [] as Row[] };
  for (const n of tokens) {
    const label = tokenLabel(n.ref, tr, registry);
    const amt = (v: bigint) => tokenAmountText(n.ref, v, tr);
    if (n.fromWallet > 0n) out.spend.push(row(`tok-spend-${n.ref.covenantId}`, label, amt(n.fromWallet)));
    if (n.released > 0n) out.spend.push(row(`tok-released-${n.ref.covenantId}`, tr('confirm.tokens.released', { token: label }), amt(n.released), 'good', tr('confirm.tokens.releasedHint')));
    if (n.escrowed > 0n) out.locked.push(row(`tok-escrow-${n.ref.covenantId}`, tr('confirm.tokens.escrowed', { token: label }), amt(n.escrowed), 'warn', tr('confirm.tokens.escrowedHint')));
    if (n.toMaker > 0n) out.back.push(row(`tok-back-${n.ref.covenantId}`, label, amt(n.toMaker), 'good'));
    if (n.toOthers > 0n) out.others.push(row(`tok-others-${n.ref.covenantId}`, label, amt(n.toOthers), 'bad'));
    if (n.walletDelta !== 0n) {
      out.net.push(row(`tok-net-${n.ref.covenantId}`, label, `${n.walletDelta > 0n ? '+' : ''}${tokenAmountText(n.ref, n.walletDelta, tr)}`, n.walletDelta > 0n ? 'good' : 'normal', tr('confirm.net.tokenHint')));
    }
  }
  return out;
}

const HEADING: Record<SigningKind, string> = {
  create: 'confirm.kind.create', cancel: 'confirm.kind.cancel', 'cancel-replace': 'confirm.kind.cancelReplace', 'cancel-position': 'confirm.kind.cancelPosition',
  refund: 'confirm.kind.refund', send: 'confirm.kind.send', sweep: 'confirm.kind.sweep', other: 'confirm.kind.other',
};

const actionKey = (a: DecodedSpend['action']): string => `confirm.action.${a}`;

/** A stray of another token: its label, and for a token outside the registry the "unknown token" warning (it may be worthless or hostile). */
function otherStrayRow(x: DecodedSpend['otherStrays'][number], f: Fmt): Row {
  const label = f.tr(x.foreign ? 'confirm.spend.foreignStraysOf' : 'confirm.spend.straysOf', { token: tokenLabel(x.ref, f.tr, f.registry) });
  const known = !!f.registry?.byCovenantId.get(x.ref.covenantId);
  return row(`strays-${x.ref.covenantId}`, label, tokenAmountText(x.ref, x.amount, f.tr), known ? 'warn' : 'bad', f.tr(known ? 'confirm.spend.straysHint' : 'confirm.spend.unknownTokenHint'));
}

function spendCard(s: DecodedSpend, f: Fmt): Card {
  const d = s.description;
  const id = s.covenantId ?? `input-${s.input}`;
  if (!d) return { id, title: f.tr('confirm.spend.unknown'), badge: f.tr(actionKey(s.action)), rows: [], children: [], flagged: true };
  const rows = orderRows(d, { value: null, locked: null, deadline: null }, f);
  // a sweep releases no custody: every token it moves is a stray
  if (s.tokensReleased > 0n && !s.sweptTo) rows.push(row('tokensReleased', f.tr('confirm.spend.tokensReleased'), tokenAmountText(f.ref, s.tokensReleased, f.tr), 'good'));
  if (s.strays > 0n) rows.push(row('strays', f.tr('confirm.spend.strays'), tokenAmountText(f.ref, s.strays, f.tr), 'warn', f.tr('confirm.spend.straysHint')));
  for (const x of s.otherStrays ?? []) rows.push(otherStrayRow(x, f));
  if (s.sweptTo) {
    rows.push(row('strayKas', f.tr('confirm.sweep.kas'), kas(s.sweptTo.kas), 'good', f.tr('confirm.sweep.kasHint')));
    rows.push(row('continues', f.tr('confirm.sweep.continues'), f.tr('confirm.sweep.continuesValue', { output: s.sweptTo.output }), 'good', f.tr('confirm.sweep.continuesHint')));
    rows.push(row('idle', f.tr('confirm.sweep.idle'), f.tr('confirm.sweep.idleValue'), 'muted', f.tr('confirm.sweep.idleHint')));
  }
  if (s.trigger) rows.push(...triggerRows(s.trigger, d, f));
  return {
    id, title: `${f.tr(actionKey(s.action))}: ${orderTitle(d, null, f)}`, badge: s.makerIsWallet ? f.tr('confirm.spend.yours') : f.tr('confirm.spend.notYours'),
    rows, children: [], flagged: !s.makerIsWallet && (s.action === 'cancel' || s.action === 'sweep'),
  };
}

/**
 * The intro of a sweep, stated from the transaction: N stray token UTXOs (amounts per token) and the KAS they carry return to the maker; the order
 * continues unchanged (same price, amount, custody) and, as a new UTXO, its 90-day idle window restarts.
 */
function sweepIntro(spends: DecodedSpend[], f: Fmt, refOf: (cov: Hex) => TokenRef): string {
  const sw = spends.filter((s) => s.sweptTo);
  const utxos = sw.reduce((a, s) => a + s.sweptTo!.utxos, 0);
  const kasSum = sw.reduce((a, s) => a + s.sweptTo!.kas, 0n);
  const parts: string[] = [];
  for (const s of sw) {
    if (s.strays > 0n && s.description) parts.push(tokenAmountText(refOf(s.description.tokenCovId), s.strays, f.tr));
    for (const x of s.otherStrays) {
      const known = !!f.registry?.byCovenantId.get(x.ref.covenantId);
      parts.push(known ? tokenAmountText(x.ref, x.amount, f.tr) : f.tr('confirm.sweep.unknownAmount', { amount: x.amount.toString(), token: tokenLabel(x.ref, f.tr, f.registry) }));
    }
  }
  return f.tr('confirm.kind.sweepIntro', { count: utxos, amounts: parts.join(', '), kas: formatKas(kasSum, GROUP) });
}

/**
 * The trigger evidence of an order this tx arms or trails (touch, protocol v2.6): how it is armed, which input is the evidence (and its custody),
 * its side, quote and size, how long it rested, and whether it satisfies the order's rule. From the decoder's derivation only.
 */
export function triggerRows(t: DecodedTrigger, d: OrderDescription, f: Fmt): Row[] {
  const { tr } = f;
  const e = t.evidence;
  const rows: Row[] = [];
  const how = t.effect === 'trail' && t.trail
    ? tr('confirm.trigger.trailed', { stop: priceText(t.trail.newStop, d, f).value })
    : tr(t.path === 'fill' ? 'confirm.trigger.armedInFill' : 'confirm.trigger.armedByUpdate');
  rows.push(row('trigger', tr('confirm.trigger.title'), how, t.ok ? 'warn' : 'bad'));
  const side = e.side ? tr(e.side === 'ask' ? 'confirm.trigger.sideAsk' : 'confirm.trigger.sideBid') : tr('confirm.trigger.sideUnknown', { kind: e.kind ?? '?' });
  rows.push(row('triggerEvidence', tr('confirm.trigger.evidence'), tr('confirm.trigger.evidenceValue', { input: e.input, side }), t.checks.plain && t.checks.sameToken && t.checks.filled && t.checks.custody ? 'normal' : 'bad',
    e.custodyInput !== null ? tr('confirm.trigger.custody', { input: e.custodyInput }) : undefined));
  if (e.price !== null) {
    const p = priceText(e.price, d, f);
    rows.push(row('triggerQuote', tr('confirm.trigger.quote'), p.value, t.checks.side && t.checks.price ? 'normal' : 'bad', tr('confirm.trigger.quoteRule', { stop: priceText(t.rule.stop, d, f).value })));
  }
  if (e.amount !== null) {
    rows.push(row('triggerAmount', tr('confirm.trigger.amount'), tr('confirm.trigger.amountValue', { amount: tokenAmountText(f.ref, e.amount, tr), min: tokenAmountText(f.ref, t.rule.minTouch, tr) }), t.checks.amount ? 'normal' : 'bad'));
  }
  if (e.exposedSince !== null) {
    rows.push(row('triggerRest', tr('confirm.trigger.rest'), tr('confirm.trigger.restValue', { daa: e.exposedSince.toString(), seconds: secondsOf(t.rule.minRestDaa) }), t.checks.rest && t.checks.notDecaying ? 'normal' : 'bad'));
  }
  rows.push(row('triggerVerdict', tr('confirm.trigger.verdict'), tr(t.ok ? 'confirm.trigger.ok' : 'confirm.trigger.notOk'), t.ok ? 'good' : 'bad'));
  return rows;
}

function inputText(i: DecodedInput, ctx: { tr: Translate; registry?: TokenRegistry | null }): string {
  const { tr } = ctx;
  const base = tr('confirm.adv.input', { index: i.index, role: i.role, kas: formatKas(i.amount, GROUP) });
  if (i.token) return `${base}; ${tokenLabel(i.token.ref, tr, ctx.registry)} ${tokenAmountText(i.token.ref, i.token.amount, tr)}`;
  if (i.order) return `${base}; ${tr(actionKey(i.order.action))} ${i.order.kind}`;
  return base;
}

function outputText(o: DecodedOutput, ctx: { tr: Translate; registry?: TokenRegistry | null }): string {
  const { tr } = ctx;
  const base = tr('confirm.adv.output', { index: o.index, kind: tr(`confirm.outkind.${o.kind}`), kas: formatKas(o.value, GROUP) });
  if (o.token) return `${base}; ${tokenLabel(o.token.ref, tr, ctx.registry)} ${tokenAmountText(o.token.ref, o.token.amount, tr)}`;
  if (o.recipient) return `${base}; ${tr('confirm.adv.recipient', { key: short(o.recipient) })}`;
  return base;
}

function payloadText(r: PayloadRecord, tr: Translate): string {
  switch (r.type) {
    case 'order': return tr('confirm.adv.recordOrder', { output: r.output, template: r.template });
    case 'amend': return tr('confirm.adv.recordAmend', { output: r.output, input: r.input, template: r.template });
    case 'sweep': return tr('confirm.adv.recordSweep', { output: r.output, input: r.input });
    case 'x402': return tr('confirm.adv.recordX402', { ref: short(r.reference) });
    case 'note': return tr('confirm.adv.recordNote', { text: r.text });
    default: return tr('confirm.adv.recordUnknown', { type: r.recordType });
  }
}

/** Builds the whole screen model from the decoded summary. */
export function buildConfirmModel(summary: SigningSummary, ctx: ConfirmContext = {}, notices: WalletNotice[] = []): ConfirmModel {
  const tr = ctx.tr ?? t;
  const registry = ctx.registry ?? null;
  const clock = ctx.clock ?? null;
  const time = ctx.time ?? ((unix: bigint) => `${formatDateTime(unix)} (${formatUtc(unix)})`);
  const mk = (tokenCov: Hex): Fmt => ({ tr, ref: refOf(registry, tokenCov), clock, time, registry });
  const kasNet = summary.net.kas;
  const sections: Section[] = [];
  // an order swept in place stays where it was: its input and its continuation are not KAS released / newly locked (the difference is the fee)
  const swept = summary.spends.filter((s) => s.sweptTo);
  const sweptIn = swept.reduce((a, s) => a + (summary.inputs[s.input]?.amount ?? 0n), 0n);
  const continued = summary.orders.filter((o) => o.sweptFrom !== undefined);
  const lockedNow = continued.reduce((a, o) => subLocked(a, o.locked), kasNet.locked);
  const released = kasNet.released - sweptIn;

  // --- what you spend
  const tokenRows = tokenSectionRows(summary.net.tokens, mk(''), registry);
  const spend: Row[] = [];
  if (kasNet.fromWallet > 0n) spend.push(row('kas-spend', tr('confirm.spend.kas'), kas(kasNet.fromWallet)));
  if (released > 0n) spend.push(row('kas-released', tr('confirm.spend.kasReleased'), kas(released), 'good', tr(swept.length ? 'confirm.sweep.kasHint' : 'confirm.spend.kasReleasedHint')));
  spend.push(...tokenRows.spend);
  sections.push({ id: 'spend', title: tr('confirm.section.spend'), rows: spend, cards: [] });

  // --- orders created (a swept order is not: it continues)
  const createdOrders = summary.orders.filter((o) => o.sweptFrom === undefined);
  if (createdOrders.length > 0) {
    const cards = createdOrders.map((o: DecodedOrder) => orderCard(o.covenantId, o.description, { value: o.value, locked: o.locked, deadline: o.deadline, verified: o.verified }, mk(o.description.tokenCovId)));
    sections.push({ id: 'create', title: tr('confirm.section.create'), note: tr('confirm.section.createNote'), rows: [], cards });
  }

  // --- orders swept in place: they continue unchanged, only their strays move
  if (swept.length > 0) {
    const cards = swept.map((s) => spendCard(s, mk(s.description?.tokenCovId ?? '')));
    sections.push({ id: 'sweep', title: tr('confirm.section.sweep'), note: tr('confirm.section.sweepNote'), rows: [], cards });
  }

  // --- orders closed
  const closing = summary.spends.filter((s) => !s.sweptTo);
  if (closing.length > 0) {
    const cards = closing.map((s) => spendCard(s, mk(s.description?.tokenCovId ?? '')));
    sections.push({ id: 'close', title: tr('confirm.section.close'), rows: [], cards });
  }

  // --- locked
  const lockRows = [...lockedRows(lockedNow, tr), ...tokenRows.locked];
  if (lockedNow.total > 0n) lockRows.unshift(row('locked-total', tr('confirm.locked.total'), kas(lockedNow.total), 'warn'));
  if (lockRows.length > 0) sections.push({ id: 'locked', title: tr('confirm.section.locked'), note: tr('confirm.section.lockedNote'), rows: lockRows, cards: [] });

  // --- back to you
  const back: Row[] = [];
  if (kasNet.toWallet > 0n) back.push(row('kas-back', tr('confirm.back.kas'), kas(kasNet.toWallet), 'good', tr('confirm.back.kasHint')));
  back.push(...tokenRows.back);
  if (back.length > 0) sections.push({ id: 'back', title: tr('confirm.section.back'), rows: back, cards: [] });

  // --- to other keys (blocking unless the user asked for it: the decoder decides)
  const others: Row[] = [];
  if (kasNet.toOthers > 0n) others.push(row('kas-others', tr('confirm.others.kas'), kas(kasNet.toOthers), 'bad'));
  others.push(...tokenRows.others);
  if (others.length > 0) sections.push({ id: 'others', title: tr('confirm.section.others'), rows: others, cards: [] });

  // --- fee
  const feeDisc = ctx.fee ?? null;
  sections.push({ id: 'fee', title: tr('confirm.section.fee'), rows: [row('fee', tr('confirm.fee.network'), kas(summary.fee.sompi), 'normal', tr('confirm.fee.hint')), ...(feeDisc?.rows ?? [])], cards: [] });

  // --- net effect on the wallet
  const net: Row[] = [];
  const kasDelta = kasNet.walletDelta;
  net.push(row('net-kas', tr('confirm.net.kas'), `${kasDelta > 0n ? '+' : kasDelta < 0n ? '-' : ''}${formatKas(kasDelta < 0n ? -kasDelta : kasDelta, GROUP)} KAS`, kasDelta > 0n ? 'good' : 'normal', tr('confirm.net.kasHint')));
  net.push(...tokenRows.net);
  sections.push({ id: 'net', title: tr('confirm.section.net'), rows: net, cards: [] });

  const advanced: AdvancedModel = {
    txid: summary.txid,
    signatures: summary.signatures,
    fee: {
      paid: kas(summary.fee.sompi), declared: kas(summary.fee.declared), minimum: kas(summary.fee.minimum), mode: summary.fee.mode,
      feeMass: summary.fee.mass.fee, priorityMass: summary.fee.mass.priority, storageMass: summary.fee.mass.storage,
    },
    inputs: summary.inputs.map((i) => ({ index: i.index, text: inputText(i, { tr, registry }), willSign: i.willSign })),
    outputs: summary.outputs.map((o) => ({ index: o.index, text: outputText(o, { tr, registry }), flagged: o.flagged })),
    payload: summary.payload ? summary.payload.records.map((r) => payloadText(r, tr)) : [],
  };

  return {
    kind: summary.kind,
    heading: tr(HEADING[summary.kind]),
    intro: summary.kind === 'sweep' ? sweepIntro(summary.spends, mk(''), (cov) => refOf(registry, cov)) : tr(`${HEADING[summary.kind]}Intro`),
    sections,
    blocking: summary.blocking,
    warnings: [...summary.warnings, ...(feeDisc?.warnings ?? [])],
    info: [...summary.info, ...(feeDisc?.info ?? [])],
    canSign: summary.ok,
    notices,
    advanced,
  };
}
