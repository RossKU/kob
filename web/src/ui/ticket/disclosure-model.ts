// View model of the ticket's DISCLOSURE panel: `OrderPlan` (+ the conditional summary) -> rows of plain strings and i18n keys.
// Everything shown derives from the planned state and the built transaction (`plan.disclosure`, `plan.built`), never from the form text:
// what the user reads here is what the confirmation screen will then re-derive from the transaction itself.
// Pure and DOM-free (unit-tested); the component only renders rows with `t(labelKey)`.
//
// A token/token pair A/B (`DisclosureCtx.quote`): prices are B per whole A, the all-in total is an amount of B (a sell receives at least, a buy
// pays at most: the covenant's rounding), tips, carriers, reserves and fees stay KAS, and the pair rows (`pair`) add the B escrow and the
// pair trigger rule (two KAS books or a resting pair order). Prices are never derived from pair fills.
import { gtcRenewalUnix, formatJst, formatUtc } from '../../kob/daa';
import type { CondSummary } from '../../kob/orders/cond-common';
import type { CarrierLine, Clock, Disclosure, OrderPlan, PairDisclosure } from '../../kob/plan-types';
import { formatKas, formatPricePerToken, formatTokenAmount, formatUnits, pow10 } from '../../kob/units';

export interface DisclosureCtx {
  ticker: string;
  decimals: number;
  /** the order scale: base units per whole token (the denominator of every price) */
  scale: bigint;
  clock: Clock;
  /** a token/token pair: the quote token B the prices are in (B per whole `ticker`); absent = KAS */
  quote?: { ticker: string; decimals: number };
}

/** The pair facts of a pair plan the panel shows: the planner's `PairDisclosure` (kob/orders/pair*.ts, exact rounded amounts from kob-wasm). */
export type PairFacts = PairDisclosure;

export type Tone = 'normal' | 'primary' | 'muted' | 'warn';

/** A plain string (numbers, KAS amounts) or an i18n key with pre-formatted params. */
export type Txt = string | { key: string; params: Record<string, string> };
const tx = (key: string, params: Record<string, string> = {}): Txt => ({ key, params });

export interface DisclosureRow {
  /** stable id: `data-testid="disc-<id>"` */
  id: string;
  labelKey: string;
  labelParams?: Record<string, string>;
  /** main value, e.g. "2.5 KAS / EXKCC" */
  value: Txt;
  /** second line */
  detail?: Txt;
  tone: Tone;
}

export interface CarrierRow {
  kind: string;
  /** i18n key of the label (`ticket.carrier.<kind>`) */
  labelKey: string;
  count: number;
  each: string;
  total: string;
  /** stays in a UTXO the wallet owns (not locked) */
  kept: boolean;
}

export interface TimeRow {
  id: string;
  /** i18n key: `ticket.time.<kind>` */
  labelKey: string;
  unix: bigint;
  utc: string;
  jst: string;
  /** an extra sentence key with the renewal date etc. */
  extra?: { labelKey: string; unix: bigint; utc: string; jst: string };
}

export interface NoteRow {
  /** the planner's tag */
  tag: string;
  /** i18n key `ticket.note.<tag>` */
  key: string;
  params: Record<string, string>;
}

export interface DisclosureModel {
  side: 'sell' | 'buy';
  /** the amount in token units, e.g. "5.25" */
  tokenAmount: string;
  /** the smallest fill (unless a fill takes everything left) in token units with the ticker, e.g. "0.4 EXKCC" */
  minFill: string;
  /** the trigger threshold of a stop (token units with the ticker), null for kinds without a trigger */
  minTouch: string | null;
  ticker: string;
  price: DisclosureRow[];
  carriers: CarrierRow[];
  /** total KAS leaving the spendable balance into covenants, excluding the network fee */
  kasLocked: string;
  /** tokens moved into custody (sell side), null for buys */
  tokensEscrowed: string | null;
  fee: string | null;
  /** refund / keeper tips reserved inside the order */
  reserves: DisclosureRow[];
  times: TimeRow[];
  /** type-specific figures: trailing, keeper, repeat, exit legs */
  extras: DisclosureRow[];
  notes: NoteRow[];
  /** a token/token pair: the quote token's ticker and the pair rows (B escrow, pair trigger rule); null on a KAS market */
  pair: { quoteTicker: string; rows: DisclosureRow[] } | null;
}

const GROUP = { group: ',' };
export const kasText = (sompi: bigint): string => `${formatKas(sompi, GROUP)} KAS`;

/** The quote ticker of the market (KAS, or a pair's token B). */
const quoteOf = (c: DisclosureCtx): string => c.quote?.ticker ?? 'KAS';
/** A state price (quote units per whole token of `scale` base units) in quote per whole token: KAS, or B per whole A (B's decimals, exact). */
const perToken = (price: bigint, c: DisclosureCtx): string => {
  if (!c.quote) return formatPricePerToken(price, c.decimals, c.scale, GROUP);
  const digits = Math.max(c.quote.decimals, 0);
  const den = c.scale * pow10(c.quote.decimals);
  const num = price * pow10(c.decimals) * pow10(digits);
  return formatUnits((num + den / 2n) / den, digits, GROUP);
};
/** A KAS price per whole token (a tip on every market). */
const kasPerToken = (price: bigint, c: DisclosureCtx): string => formatPricePerToken(price, c.decimals, c.scale, GROUP);
/** base units -> "1.25 EXKCC" */
const tokens = (amount: bigint, c: DisclosureCtx): string => `${formatTokenAmount(amount, c.decimals, GROUP)} ${c.ticker}`;
/** base units of the quote token B -> "1.25 EXUSD" */
const quoteTokens = (amount: bigint, q: NonNullable<DisclosureCtx['quote']>): string => `${formatTokenAmount(amount, q.decimals, GROUP)} ${q.ticker}`;
/** "2.5 KAS / EXKCC" or, on a pair, "2.5 EXUSD / EXKCC" (a state price per whole token of `scale` base units). */
const quotePrice = (price: bigint, c: DisclosureCtx): string => `${perToken(price, c)} ${quoteOf(c)}`;

function priceValue(price: bigint, c: DisclosureCtx): { value: Txt } {
  return { value: `${perToken(price, c)} ${quoteOf(c)} / ${c.ticker}` };
}

function priceRow(id: string, labelKey: string, price: bigint, c: DisclosureCtx, tone: Tone = 'normal', labelParams?: Record<string, string>): DisclosureRow {
  return { id, labelKey, ...(labelParams ? { labelParams } : {}), tone, ...priceValue(price, c) };
}
/** A KAS price per whole token (tips: KAS on every market). */
function kasPriceRow(id: string, labelKey: string, price: bigint, c: DisclosureCtx, tone: Tone = 'normal'): DisclosureRow {
  return { id, labelKey, tone, value: `${kasPerToken(price, c)} KAS / ${c.ticker}` };
}

const timeParts = (unix: bigint) => ({ unix, utc: formatUtc(unix), jst: formatJst(unix) });

function timesOf(d: Disclosure, c: DisclosureCtx): TimeRow[] {
  const rows: TimeRow[] = [];
  if (d.activatesAt) rows.push({ id: 'activates', labelKey: 'ticket.time.activates', ...timeParts(d.activatesAt.approxUnixSeconds) });
  const e = d.expiry;
  switch (e.kind) {
    case 'gtc': {
      if (e.approxUnixSeconds !== null) {
        const renew = gtcRenewalUnix(c.clock.unixSeconds);
        rows.push({ id: 'expiry', labelKey: 'ticket.time.gtc', ...timeParts(e.approxUnixSeconds), extra: { labelKey: 'ticket.time.renew', ...timeParts(renew) } });
      }
      break;
    }
    case 'gtd':
      if (e.approxUnixSeconds !== null) rows.push({ id: 'expiry', labelKey: 'ticket.time.gtd', ...timeParts(e.approxUnixSeconds) });
      break;
    case 'day': {
      const at = e.deadlineUnixSeconds ?? e.approxUnixSeconds;
      if (at !== null) rows.push({ id: 'expiry', labelKey: 'ticket.time.day', ...timeParts(at) });
      break;
    }
    case 'ioc':
    case 'fok':
      if (e.approxUnixSeconds !== null) rows.push({ id: 'expiry', labelKey: `ticket.time.${e.kind}`, ...timeParts(e.approxUnixSeconds) });
      break;
    case 'none':
      break;
  }
  return rows;
}

const carrierRow = (l: CarrierLine): CarrierRow => ({
  kind: l.kind,
  labelKey: `ticket.carrier.${l.kind}`,
  count: l.count,
  each: kasText(l.amount),
  total: kasText(l.amount * BigInt(l.count)),
  kept: l.kept === true,
});

/** how long the exits live: until cancelled (90 days idle from each exit's creation) or until the chosen date */
function exitLifetimeRow(x: NonNullable<CondSummary['exit']>): DisclosureRow {
  if (x.expiry === 'gtd' && x.expiryUnixSeconds !== null) {
    return { id: 'exitLifetime', labelKey: 'ticket.disc.exitLifetime', tone: 'normal', value: tx('ticket.val.exitGtd', { utc: formatUtc(x.expiryUnixSeconds), jst: formatJst(x.expiryUnixSeconds) }) };
  }
  return { id: 'exitLifetime', labelKey: 'ticket.disc.exitLifetime', tone: 'normal', value: tx('ticket.val.exitGtc') };
}

function extrasOf(cond: CondSummary | null, c: DisclosureCtx): DisclosureRow[] {
  const out: DisclosureRow[] = [];
  if (!cond) return out;
  const legs = cond.legs;
  if (legs) {
    if (legs.stop !== null) out.push(priceRow('stop', 'ticket.disc.stop', legs.stop, c));
    if (legs.stop !== null && legs.stopWorst !== null) out.push(priceRow('stopWorst', 'ticket.disc.stopWorst', legs.stopWorst, c, 'warn'));
    if (legs.takeProfit !== null) out.push(priceRow('takeProfit', 'ticket.disc.takeProfit', legs.takeProfit, c));
    if (legs.stop !== null) {
      out.push({
        id: 'trigger', labelKey: 'ticket.disc.trigger', tone: 'muted',
        value: tx('ticket.val.trigger', { amount: tokens(legs.minTouch, c), seconds: String(Number(legs.minRestDaa) / 10) }),
        detail: tx('ticket.val.band', { seconds: String(Number(legs.bandDaa) / 10), percent: String(Number(legs.slipBps) / 100) }),
      });
    }
  }
  if (cond.trail) {
    out.push({
      id: 'trail', labelKey: 'ticket.disc.trail', tone: 'normal',
      value: tx('ticket.val.trail', { step: quotePrice(cond.trail.step, c), gap: quotePrice(cond.trail.gap, c) }),
      detail: tx('ticket.val.trailWait', { minutes: String(cond.trail.waitSeconds / 60n), updates: String(cond.trail.expectedUpdates) }),
    });
  }
  if (cond.keeper) {
    out.push({ id: 'keeper', labelKey: 'ticket.disc.keeper', tone: 'muted', value: tx('ticket.val.keeper', { tip: kasText(cond.keeper.tip), updates: String(cond.keeper.expectedUpdates) }), detail: kasText(cond.keeper.reserve) });
  }
  if (cond.entry) {
    out.push({ id: 'entryFills', labelKey: 'ticket.disc.entryFills', tone: 'muted', value: tx('ticket.val.entryFills', { fills: String(cond.entry.maxFills) }) });
    if (cond.entry.stop !== null) {
      out.push(priceRow('entryStop', 'ticket.disc.entryStop', cond.entry.stop, c));
      out.push({ id: 'entryTrigger', labelKey: 'ticket.disc.entryTrigger', tone: 'muted', value: tx('ticket.val.trigger', { amount: tokens(cond.entry.minTouch, c), seconds: String(Number(cond.entry.minRestDaa) / 10) }) });
    }
  }
  if (cond.exit) {
    const x = cond.exit;
    if (x.legs.takeProfit !== null) out.push(priceRow('exitTakeProfit', 'ticket.disc.exitTakeProfit', x.legs.takeProfit, c));
    if (x.legs.stop !== null) out.push(priceRow('exitStop', 'ticket.disc.exitStop', x.legs.stop, c));
    if (x.legs.stopWorst !== null && x.legs.stop !== null) out.push(priceRow('exitStopWorst', 'ticket.disc.exitStopWorst', x.legs.stopWorst, c, 'warn'));
    if (x.legs.stop !== null) {
      out.push({ id: 'exitTrigger', labelKey: 'ticket.disc.exitTrigger', tone: 'muted', value: tx('ticket.val.trigger', { amount: tokens(x.legs.minTouch, c), seconds: String(Number(x.legs.minRestDaa) / 10) }) });
    }
    out.push({ id: 'exitMinFill', labelKey: 'ticket.disc.exitMinFill', tone: 'muted', value: tokens(x.minFill, c) });
    if (x.takeProfitAllIn !== null) out.push(priceRow('exitTakeProfitAllIn', 'ticket.disc.exitTakeProfitAllIn', x.takeProfitAllIn, c));
    if (x.trail) {
      out.push({ id: 'exitTrail', labelKey: 'ticket.disc.trail', tone: 'normal', value: tx('ticket.val.trail', { step: quotePrice(x.trail.step, c), gap: quotePrice(x.trail.gap, c) }), detail: tx('ticket.val.trailWait', { minutes: String(x.trail.waitSeconds / 60n), updates: String(x.trail.expectedUpdates) }) });
    }
    out.push(exitLifetimeRow(x));
    if (x.prefund !== null) out.push(priceRow('prefund', 'ticket.disc.prefund', x.prefund, c, 'muted'));
  }
  if (cond.repeat) {
    const r = cond.repeat;
    out.push({
      id: 'repeat', labelKey: 'ticket.disc.repeat', tone: 'primary',
      value: r.count === null ? tx('ticket.val.repeatUnlimited') : tx('ticket.val.repeatCount', { count: String(r.count) }),
      detail: tx('ticket.val.repeatCycle', { amount: tokens(r.cycleAmount, c) }),
    });
    out.push(priceRow('profitPerToken', 'ticket.disc.profitPerToken', r.profitPerToken, c, r.profitPerToken > 0n ? 'normal' : 'warn'));
  }
  return out;
}

/** Builds the panel model. Null when the plan has no disclosure (it failed before a transaction was built). */
export function buildDisclosureModel(plan: OrderPlan, ctx: DisclosureCtx): DisclosureModel | null {
  const d = plan.disclosure;
  if (!d) return null;
  const cond = (plan as OrderPlan & { cond?: CondSummary | null }).cond ?? null;
  const price: DisclosureRow[] = [];
  const buy = d.side === 'buy';
  // a market / auction order's limit IS its worst price: show "expected" and "worst" instead of the same number twice
  const boundOnly = d.expectedPrice !== null && d.worstPrice !== null && d.worstPrice === d.limitPrice;
  if (d.limitPrice !== null && !boundOnly) price.push(priceRow('limit', 'ticket.disc.limit', d.limitPrice, ctx));
  if (d.tip > 0n) price.push(kasPriceRow('tip', 'ticket.disc.tip', d.tip, ctx, 'muted'));
  if (d.allInPrice !== null) price.push(priceRow('allInPrice', buy ? 'ticket.disc.allInPay' : 'ticket.disc.allInReceive', d.allInPrice, ctx, 'primary'));
  // the total by the covenant's quote rule: a sell receives at least (rounded up), a buy pays at most (rounded down); on a pair an amount of B
  if (d.allInTotal !== null) {
    price.push({ id: 'allInTotal', labelKey: buy ? 'ticket.disc.totalPay' : 'ticket.disc.totalReceive', tone: 'primary', value: ctx.quote ? quoteTokens(d.allInTotal, ctx.quote) : kasText(d.allInTotal) });
  }
  if (d.expectedPrice !== null) price.push(priceRow('expected', 'ticket.disc.expected', d.expectedPrice, ctx));
  if (d.worstPrice !== null && (boundOnly || d.worstPrice !== d.limitPrice)) price.push(priceRow('worst', 'ticket.disc.worst', d.worstPrice, ctx, 'warn'));

  const reserves: DisclosureRow[] = [];
  // R-13: paid only to whoever refunds the order after its expiry; it returns to you with a cancel or the last fill
  if (d.refundTip > 0n) reserves.push({ id: 'refundTip', labelKey: 'ticket.disc.refundTip', tone: 'muted', value: kasText(d.refundTip), detail: tx('ticket.disc.refundTipHint') });
  if (d.keeperTip > 0n) reserves.push({ id: 'keeperTip', labelKey: 'ticket.disc.keeperTip', tone: 'muted', value: kasText(d.keeperTip) });

  const pairFacts = ctx.quote ? ((plan as OrderPlan & { pair?: PairFacts | null }).pair ?? null) : null;
  const seen = new Set<string>();
  const notes: NoteRow[] = [];
  for (const tag of [...d.notes, ...(pairFacts?.notes ?? [])]) {
    if (seen.has(tag)) continue;
    seen.add(tag);
    const params = noteParams(tag, d, cond, ctx, pairFacts);
    // a sentence that quotes a number the plan does not have is left out rather than shown with a hole
    if ((NOTE_REQUIRES[tag] ?? []).some((k) => params[k] === undefined || params[k] === '')) continue;
    notes.push({ tag, key: `ticket.note.${tag}`, params });
  }

  return {
    side: d.side,
    tokenAmount: formatTokenAmount(d.tokenAmount, ctx.decimals, GROUP),
    minFill: tokens(d.minFill, ctx),
    minTouch: d.minTouch === null ? null : tokens(d.minTouch, ctx),
    ticker: ctx.ticker,
    price,
    carriers: d.carriers.map(carrierRow),
    kasLocked: kasText(d.kasLocked),
    tokensEscrowed: d.tokensEscrowed > 0n ? `${formatTokenAmount(d.tokensEscrowed, ctx.decimals, GROUP)} ${ctx.ticker}` : null,
    fee: d.fee === null ? null : kasText(d.fee),
    reserves,
    times: timesOf(d, ctx),
    extras: extrasOf(cond, ctx),
    notes,
    pair: ctx.quote ? { quoteTicker: ctx.quote.ticker, rows: pairRows(pairFacts, ctx) } : null,
  };
}

/**
 * The pair rows (exact rounded amounts of the planner's `PairDisclosure`): A and B moved into custody (an ask's A, a bid's B escrow, a sell-first
 * entry's A and B prefund), the B the whole amount is guaranteed at its worst price (a sell receives at least, a buy pays at most) and at the
 * expected price, the B of one minimum fill, the KAS tip prefunded, what each exit of an if-done entry holds, and the trigger rule of a pair stop:
 * it arms when the two KAS books imply a rate beyond the stop (a resting order of each token on the costly side, each rested `minRestDaa` and
 * filled together) or when a resting pair order at or beyond the stop is filled.
 */
function pairRows(p: PairFacts | null, c: DisclosureCtx): DisclosureRow[] {
  const q = c.quote;
  if (!p || !q) return [];
  const out: DisclosureRow[] = [];
  const buy = p.side === 'buy';
  if (p.escrowA > 0n) out.push({ id: 'pairEscrowA', labelKey: 'ticket.disc.pairEscrowA', tone: 'normal', value: tokens(p.escrowA, c) });
  if (p.escrowB > 0n) {
    out.push({
      id: 'pairEscrowB', labelKey: p.kind === 'KobIfdPair' && !buy ? 'ticket.disc.pairPrefundB' : 'ticket.disc.pairEscrowB', tone: 'normal', value: quoteTokens(p.escrowB, q),
    });
  }
  if (p.receiveMinB != null) out.push({ id: 'pairReceiveMinB', labelKey: 'ticket.disc.pairReceiveMinB', tone: 'primary', value: quoteTokens(p.receiveMinB, q), detail: tx('ticket.disc.pairRoundUp') });
  if (p.payMaxB != null) out.push({ id: 'pairPayMaxB', labelKey: 'ticket.disc.pairPayMaxB', tone: 'primary', value: quoteTokens(p.payMaxB, q), detail: tx('ticket.disc.pairRoundDown') });
  if (p.expectedB != null && p.expectedB !== (buy ? p.payMaxB : p.receiveMinB)) {
    out.push({ id: 'pairExpectedB', labelKey: buy ? 'ticket.disc.pairExpectedPayB' : 'ticket.disc.pairExpectedReceiveB', tone: 'normal', value: quoteTokens(p.expectedB, q) });
  }
  if (p.minFillB != null && p.minFillB > 0n) out.push({ id: 'pairMinFillB', labelKey: 'ticket.disc.pairMinFillB', tone: 'muted', value: quoteTokens(p.minFillB, q) });
  if (p.tipKasTotal > 0n) out.push({ id: 'pairTipKas', labelKey: 'ticket.disc.pairTipKas', tone: 'muted', value: kasText(p.tipKasTotal) });
  if (p.exit) {
    const custody = p.exit.custodyPerFill === 'proceedsPlusPrefund' ? tx('ticket.val.pairExitHoldsB', { quote: q.ticker }) : tx('ticket.val.pairExitHoldsA', { base: c.ticker });
    out.push({ id: 'pairExitCustody', labelKey: 'ticket.disc.pairExitCustody', tone: 'normal', value: custody });
  }
  const r = p.trigger;
  if (r) {
    const stop = `${perToken(BigInt(r.stop), c)} ${q.ticker} / ${c.ticker}`;
    const seconds = String(Number(r.minRestDaa) / 10);
    out.push({
      id: 'pairTrigger', labelKey: 'ticket.disc.pairTrigger', tone: 'normal',
      value: tx(r.direction === 'fallsTo' ? 'ticket.val.pairTriggerSell' : 'ticket.val.pairTriggerBuy', {
        stop, base: c.ticker, quote: q.ticker, seconds,
        a: r.arm.kasBooks.a, b: r.arm.kasBooks.b, pair: r.arm.pair,
      }),
      detail: tx('ticket.val.pairTriggerMin', {
        a: tokens(BigInt(r.minTouch), c),
        b: r.minTouchB !== null ? quoteTokens(BigInt(r.minTouchB), q) : '-',
      }),
    });
    if (r.trail) {
      out.push({
        id: 'pairTrail', labelKey: 'ticket.disc.pairTrail', tone: 'muted',
        value: tx(r.trail.direction === 'up' ? 'ticket.val.pairTrailUp' : 'ticket.val.pairTrailDown', { base: c.ticker, quote: q.ticker }),
      });
    }
  }
  return out;
}

/** Numbers some type-specific sentences quote (`{stop}`, `{count}`, ...). Unknown tags get no params. */
function noteParams(tag: string, d: Disclosure, cond: CondSummary | null, c: DisclosureCtx, pair: PairFacts | null = null): Record<string, string> {
  const p: Record<string, string> = { ticker: c.ticker };
  if (pair && c.quote) {
    if (PAIR_NOTE_TAGS.includes(tag as (typeof PAIR_NOTE_TAGS)[number])) p.quote = c.quote.ticker;
    if (tag === 'pairTipKas') p.tip = kasText(pair.tipKasTotal);
  }
  const legs = cond?.legs ?? cond?.exit?.legs ?? null;
  switch (tag) {
    case 'stopEntryAuction':
      if (cond?.entry) p.auction = String(Number(cond.entry.bandDaa) / 10);
      break;
    case 'stopEntryTrigger':
      if (cond?.entry) {
        p.exposure = String(Number(cond.entry.minRestDaa) / 10);
        p.volume = tokens(cond.entry.minTouch, c);
      }
      break;
    case 'stopTrigger':
    case 'stopAuction':
    case 'stopLimitMayNotFill':
      if (legs) {
        p.exposure = String(Number(legs.minRestDaa) / 10);
        p.volume = tokens(legs.minTouch, c);
        p.auction = String(Number(legs.bandDaa) / 10);
        p.slippage = String(Number(legs.slipBps) / 100);
        if (legs.limit !== null) p.limit = quotePrice(legs.limit, c);
      }
      break;
    case 'trailing': {
      const t = cond?.trail ?? cond?.exit?.trail ?? null;
      if (t) {
        p.step = quotePrice(t.step, c);
        p.gap = quotePrice(t.gap, c);
        p.wait = String(t.waitSeconds / 60n);
      }
      break;
    }
    case 'keeperReserve': {
      const k = cond?.keeper ?? cond?.exit?.keeper ?? null;
      if (k) {
        p.tip = kasText(k.tip);
        p.updates = String(k.expectedUpdates);
        p.reserve = kasText(k.reserve);
      }
      break;
    }
    case 'repeatCounted':
    case 'repeatReBuys':
    case 'repeatReSells':
    case 'mergeTip':
      if (cond?.repeat) {
        p.count = cond.repeat.count === null ? '' : String(cond.repeat.count);
        p.mergeTip = `${kasPerToken(cond.repeat.mergeTip, c)} KAS`;
      }
      if (cond?.entry) p.price = quotePrice(cond.entry.price, c);
      break;
    case 'minFill':
      if (cond?.entry) {
        p.minFill = tokens(cond.entry.minFill, c);
        p.fills = String(cond.entry.maxFills);
      }
      break;
    case 'prefund':
      if (cond?.exit?.prefund != null) p.prefund = quotePrice(cond.exit.prefund, c);
      break;
    case 'carrierReturned':
      p.carriers = kasText(d.carriers.filter((x) => !x.kept).reduce((s, x) => s + x.amount * BigInt(x.count), 0n));
      break;
    default:
      break;
  }
  return p;
}

/** Parameters a note sentence quotes: the note is dropped when the plan cannot supply one of them. */
export const NOTE_REQUIRES: Readonly<Record<string, readonly string[]>> = {
  stopTrigger: ['exposure', 'volume'],
  stopAuction: ['auction', 'slippage'],
  stopLimitMayNotFill: ['limit'],
  trailing: ['step', 'gap', 'wait'],
  keeperReserve: ['tip', 'updates', 'reserve'],
  stopEntryAuction: ['auction'],
  stopEntryTrigger: ['exposure', 'volume'],
  repeatCounted: ['count'],
  repeatReBuys: ['price'],
  repeatReSells: ['price'],
  mergeTip: ['mergeTip'],
  minFill: ['minFill', 'fills'],
  prefund: ['prefund'],
  carrierReturned: ['carriers'],
  pairPricesFromKasBooks: ['quote'],
  pairTipKas: ['quote', 'tip'],
  pairRoute: ['quote'],
  pairNetting: ['quote'],
  pairInventory: ['quote'],
  pairAuction: ['quote'],
  pairTrigger: ['quote'],
  pairExitCustody: ['quote'],
};

/** The note tags of the pair planner (`PairDisclosure.notes`): their sentences name the quote token B (`{quote}`). */
export const PAIR_NOTE_TAGS = ['pairPricesFromKasBooks', 'pairTipKas', 'pairRoute', 'pairNetting', 'pairInventory', 'pairAuction', 'pairTrigger', 'pairExitCustody'] as const;

/** Every note tag the planners can emit: the i18n key `ticket.note.<tag>` must exist for each (checked by the coverage test). */
export const NOTE_TAGS = [
  'gtc', 'dayOrder', 'gtd', 'auction', 'marketable', 'fokAllOrNothing', 'iocRemainderReturned', 'market', 'streaming', 'close', 'twap', 'dca', 'dutch', 'rising',
  'carrierReturned', 'stopTrigger', 'stopAuction', 'triggerExposure', 'stopLimitMayNotFill', 'trailing', 'takeProfitLeg', 'oco', 'partialFillsKeepLegs', 'keeperReserve',
  'ifd', 'ifo', 'position', 'buyFirst', 'sellFirst', 'minFill', 'exitGtc', 'exitGtd', 'stopEntry', 'stopEntryTrigger', 'stopEntryAuction', 'exitStop', 'repeat', 'repeatUnlimited',
  'repeatCounted', 'repeatReBuys', 'repeatReSells', 'repeatStopLossEnds', 'mergeTip', 'cancelPosition', 'prefund', ...PAIR_NOTE_TAGS,
] as const;
