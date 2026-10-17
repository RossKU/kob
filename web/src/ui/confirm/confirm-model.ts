// Decoded-order vocabulary shared by the issuance screen, the silent pre-sign checks (send.ts) and the signer frame: the rows and title of one
// decoded order (`orderRows`, `orderTitle`), token labels and amounts, the open-token identity check, and the row / finding shapes they produce.
// It is not the model of an order confirmation screen: orders are no longer confirmed on a screen of their own.
//
// Pure and DOM-free. Text comes through an injected translator (`tr`, default the app's `t`) so it is unit-tested and the renderers stay thin.
// All amounts arrive as bigint and are formatted exactly (units.ts).
import type { SigningKind, SigningSummary, LockedKas, TokenRef } from '../../kob/decode';
import type { WalletNotice } from '../../kob/decode';
import { daaToUnix, formatJst, formatUtc } from '../../kob/daa';
import { UNLIMITED_REPEAT_COUNT } from '../../kob/orders/cond-common';
import { stopWorstPrice } from '../../kob/orders/cond-legs';
import { baseKind, type OrderDescription } from '../../kob/order-facts';
import type { Clock } from '../../kob/plan-types';
import { longId, type ExtensionReference, type TokenInfo, type TokenRegistry } from '../../kob/registry';
import type { Hex } from '../../kob/types';
import { formatKas, formatPricePerToken, formatTokenAmount, quoteOf, statePriceToTokenPrice } from '../../kob/units';
import { t, type Params } from '../../i18n';

export type Translate = (key: string, params?: Params) => string;
export type Tone = 'normal' | 'good' | 'warn' | 'bad' | 'muted';

export interface Row {
  id: string;
  label: string;
  value: string;
  detail?: string;
  tone: Tone;
}

export interface Section {
  id: 'issue' | 'spend' | 'locked' | 'back' | 'fee' | 'net';
  title: string;
  /** one explaining sentence under the title */
  note?: string;
  rows: Row[];
  /**
   * the gross wallet flows (`spend`: the wallet's inputs, `back`: its change): the screen shows them folded under "Wallet inputs and change",
   * after the net effect and the orders
   */
  fold?: boolean;
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

const GROUP = { group: ',' };
const kas = (v: bigint): string => `${formatKas(v, GROUP)} KAS`;
const short = (h: string): string => (h.length > 12 ? `${h.slice(0, 4)}...${h.slice(-4)}` : h);
const DAA_PER_SECOND = 10;
/** Exit orders committed inside an entry carry a "never" expiry (2^62); anything past two years is not a date. */
const FAR_FUTURE_DAA = 2n * 365n * 86_400n * BigInt(DAA_PER_SECOND);

// ------------------------------------------------------------------------------------------------ open-list token identity (A1-2)

/** Covenant ids of every token the transaction names: order tokens (a pair: both), spent orders, token movements. */
export function tokensOf(summary: SigningSummary): Set<Hex> {
  const ids = new Set<Hex>();
  const add = (d: OrderDescription | null | undefined): void => {
    if (!d) return;
    if (d.tokenCovId) ids.add(d.tokenCovId);
    if (d.pair) {
      ids.add(d.pair.base.covId);
      ids.add(d.pair.quote.covId);
    }
    add(d.entry?.exit);
  };
  for (const o of summary.orders) add(o.description);
  for (const sp of summary.spends) add(sp.description);
  for (const n of summary.net.tokens) ids.add(n.ref.covenantId);
  return ids;
}

type CheckedOpenToken = TokenInfo & { extension: NonNullable<TokenInfo['extension']> };

/**
 * Audit A1-2: an open-list KCC-20 token's extension commitment is the indexer's pick, and the decoder only checks the transaction against it. The
 * screen shows the commitment and where it came from, so the user can check it (a token's identity is its covenant id AND this commitment); a
 * commitment known without the indexer (the registry's genesis-verified entry, the wallet's node-verified holdings) that differs is blocking.
 */
export function openTokenIdentity(summary: SigningSummary, registry: TokenRegistry | null, tr: Translate = t): { rows: Row[]; blocking: Finding[] } {
  const rows: Row[] = [];
  const blocking: Finding[] = [];
  const tokens = [...tokensOf(summary)].map((id) => registry?.byCovenantId.get(id));
  // a first order: the scale is the one the user entered (no order of the token tells one yet), shown with its decimals
  for (const info of tokens) {
    if (!info || info.openList !== true || info.openScale !== 'entered') continue;
    const token = longId(info.covenantId);
    rows.push(row('scale-' + info.covenantId, tr('confirm.openScale.label', { token }), tr('confirm.openScale.value', { decimals: info.decimals }), 'warn', tr('confirm.openScale.detail')));
  }
  const open = tokens.filter((i): i is CheckedOpenToken => !!i && i.openList === true && i.family === 'kcc20' && !!i.extension);
  for (const info of open) {
    const x = info.extension;
    const token = longId(info.covenantId);
    const refName = (src: ExtensionReference['source']): string => tr(src === 'holdings' ? 'confirm.ext.ref.holdings' : 'confirm.ext.ref.registryGenesis');
    const source = tr(x.source === 'indexer' ? 'confirm.ext.fromIndexer' : x.source === 'holdings' ? 'confirm.ext.fromHoldings' : 'confirm.ext.fromOrders');
    const check =
      x.mismatch.length > 0
        ? tr('confirm.ext.checkMismatch')
        : x.references.length > 0
          ? tr('confirm.ext.checkMatch', { sources: [...new Set(x.references.map((r) => refName(r.source)))].join(', ') })
          : tr('confirm.ext.checkNone');
    const tone: Tone = x.mismatch.length ? 'bad' : x.references.length ? 'good' : 'warn';
    rows.push(row('ext-' + info.covenantId, tr('confirm.ext.label', { token }), x.commitment, tone, source + ' ' + check));
    for (const m of x.mismatch) {
      const params = { token, shown: x.commitment, known: m.commitment, source: refName(m.source) };
      blocking.push({
        code: 'open-token-extension-mismatch',
        severity: 'blocking',
        message: 'extension commitment ' + x.commitment + ' differs from ' + m.commitment + ' (' + m.source + ')',
        params,
        text: tr('confirm.ext.mismatch', params),
      });
    }
  }
  return { rows, blocking };
}

// ------------------------------------------------------------------------------------------------ token labels

/** The identity line of a token: `TICKER (abcd...1234)` for registry tokens, `unknown token (abcd...1234)` otherwise. Never the ticker alone. */
export function tokenLabel(ref: TokenRef, tr: Translate = t, registry?: TokenRegistry | null): string {
  const info = registry?.byCovenantId.get(ref.covenantId);
  // a token outside the registry is identified by its id alone: 8 + 8 hex, not the 4 + 4 fragment
  if (!info) return tr('confirm.token.unknown', { id: longId(ref.covenantId) });
  if (info.openList) return tr('confirm.token.labelOpen', { id: longId(info.covenantId) });
  return tr('confirm.token.label', { ticker: info.ticker, id: short(info.covenantId) });
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

export interface Fmt {
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
    // "X KAS per TOKEN", never the pair notation "X TOKEN / KAS" (it reads as tokens per KAS)
    value: f.tr('confirm.f.perUnit', { price: formatPricePerToken(price, f.ref.decimals, d.scale, GROUP), unit: 'KAS', per: f.ref.ticker ?? longId(f.ref.covenantId) }),
    ...(d.scale !== whole ? { detail: f.tr('confirm.f.perScale', { scale: d.scale.toString() }) } : {}),
  };
}

const approxTime = (daa: bigint, f: Fmt): string | null => (f.clock ? f.time(daaToUnix(f.clock, daa)) : null);

/**
 * A price of a pair order. The state price is B base units per `d.scale` base units of A (scale = 10^min(decimals(A), 9)); with A's decimals known
 * it is shown per WHOLE A (`price x 10^decimals(A) / scale`, the ticket's own conversion), in B when B's decimals are known, else raw base units of B.
 * Without A's decimals it stays per `scale` base units of A and the detail says so.
 */
/** The bare amount of a pair price (the unit follows as "B per A"): whole B when its decimals are known, else raw base units. */
const pairNumber = (ref: TokenRef, amount: bigint): string => (ref.decimals === null ? amount.toString() : formatTokenAmount(amount, ref.decimals, GROUP));
const bUnit = (ref: TokenRef, tr: Translate): string => ref.ticker ?? (ref.decimals === null ? tr('common.baseUnits') : '?');

function pairPriceText(price: bigint, d: OrderDescription, f: Fmt): { value: string; detail?: string } {
  const bRef = refOf(f.registry, d.pair!.quote.covId);
  const aName = f.ref.ticker ?? longId(f.ref.covenantId);
  if (f.ref.decimals === null || d.scale <= 0n) return { value: f.tr('confirm.f.perUnit', { price: pairNumber(bRef, price), unit: bUnit(bRef, f.tr), per: aName }), detail: f.tr('confirm.f.perScale', { scale: d.scale.toString() }) };
  const perWholeA = statePriceToTokenPrice(price, f.ref.decimals, d.scale, 'nearest');
  return { value: f.tr('confirm.f.perUnit', { price: pairNumber(bRef, perWholeA), unit: bUnit(bRef, f.tr), per: aName }) };
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
  const aName = f.ref.ticker ?? longId(f.ref.covenantId);
  const bName = bRef.ticker ?? longId(x.quote.covId);
  const rows: Row[] = [];
  rows.push(row('pairPair', tr('confirm.f.pairPair'), tr(d.side === 'sell' ? 'confirm.f.pairSells' : 'confirm.f.pairBuys', { base: aName, quote: bName }), bRef.inRegistry ? 'normal' : 'warn', bRef.inRegistry ? undefined : tr('confirm.f.pairUnknownQuote', { id: longId(x.quote.covId) })));
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
    // an if-done exit's GTC expiry (`GTC_EXIT_EXPIRY_DAA`, 2^62: 90 days idle from the exit's creation) or one years ahead of the clock. Without
    // a clock (the signer frame, the app before its clock loads) only the sentinel range is told apart: today's DAA scores are above
    // FAR_FUTURE_DAA as numbers, and every plain GTC order read as an exit's expiry. The DAA score is shown then.
    if (d.expiryDaa >= 1n << 60n || (f.clock !== null && d.expiryDaa >= f.clock.daa + FAR_FUTURE_DAA)) {
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

export function orderTitle(d: OrderDescription, deadline: bigint | null, f: Fmt): string {
  const type = orderTypeCode(d, deadline);
  const kind = f.tr(`confirm.order.${type}`);
  return f.tr('confirm.order.title', { side: f.tr(d.side === 'sell' ? 'common.side.sell' : 'common.side.buy'), type: kind });
}
