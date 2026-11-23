// The order-entry form for one token. It covers every order type of docs/spec/order-types.md.
//
// A thin renderer. The form keeps text (`form-state.ts` turns it into an Intent). The planner
// (`kob/plan.ts`) validates it against the protocol and wallet rules and builds the transaction.
// The ticket is the inputs and one button. The order's details are shown only in the final
// confirmation (signer/summary.ts `receipt`).
//
// The button is enabled only for a built, error-free plan. A click plans the order again from the
// state at the click, checks the built transaction silently (`ui/confirm/send.ts`, decoded and
// compared with the plan) and opens the final confirmation, whose Confirm asks the wallet.
// Progress is the label of the button itself ("Confirm in KasWare...", "Sending..."). The result is
// a card of the notifications at the top right.
import type { ComponentChildren } from 'preact';
import { useEffect, useMemo, useRef, useState } from 'preact/hooks';
import { useServices, useWallet } from '../../app/context';
import { kasTitleOf, sompiToUsd, usdTextFit } from '../../app/fiat';
import { useFiat } from '../../app/use-fiat';
import { has, t } from '../../i18n';

import { issueText, keyedText } from '../../i18n/issue-text';
import { buildErrorText, rawOf } from '../../i18n/build-error';
import { useSystemStatus } from '../shell/StatusProvider';
import { useTradingStatus } from '../shell/StatusBar';
import { planOrder, type Intent } from '../../kob/plan';
import { errors, isPairEnv, type Clock, type OrderPlan, type TakeFacts } from '../../kob/plan-types';
import { tickerUnit, type TokenInfo } from '../../kob/registry';
import { formatKas, formatUnits, quoteOf } from '../../kob/units';
import { costLine, costPreview } from './cost-preview';
import { DEFAULT_CARRIER, heldAmount, unusableTokenHeld } from '../../kob/orders/common';
import { lowPairTipFields } from '../../kob/orders/pair-common';
import { planDelivery, programTopupTip, topupTipTotal, withThreshold } from '../../kob/orders/delivery';
import { CODEC_OF_PROGRAM, isTokenProgram } from '../../kob/programs.generated';
import { readThreshold, rememberThreshold, useDeliveryPref } from '../settings/delivery-pref';
import { Banner, Button, Dropdown, Loading, RawDetails, Segmented, Stepper, kasApprox, useAsync, type StepperItem } from '../kit';
import type { CurveIntent } from '../../kob/intent-simple';
import { curveOfLadder, ladderStepItems } from './ladder-curve';
import { executorTip } from '../../kob/fill-tip';
import { runTakes } from './take-run';
import { bookFromIndexer, readClock } from '../../app/env';
import { readPairReferences, readReferences, type References } from '../../data/indexer-crosscheck';
import type { PlanEnv } from '../../kob/plan-types';
import { auctionSubmitBy } from '../../wallet/sign';
import { useFailoverState } from '../shell/failover';
import { useCongestion } from './use-congestion';
import { SendStatusInline, SendStatusToast, sendBusy, useSend, useSendFailureToast, type SendOutcome } from '../confirm/SendStatus';
import type { GateVerdict, SendJob } from '../confirm/send';
import { RETIME_LEAD_DAA, firstAttemptLead, laterClock, rememberedSigningMs, retimeLead } from '../confirm/signing-time';
import { oppositeSide } from '../market/orientation';
import { useCovenantSignGate } from './CovenantSignGate';
import { TicketFields, labelKey } from './TicketFields';
import { DeliveryHelp } from './DeliveryHelp';
import {
  BASE_SUFFIX, TYPE_GROUPS, amountBaseOf, buildIntent, deliveryChoicesOf, deliveryOf, kasTextOf, followQuote, formatPrice, streamingQuoteText, initialForm, layoutOf, parsePrice, reorientAmounts, setAmountBase,
  isIncomplete, setSide, setValue, sidesOf, switchType, visibleErrors,
  type OrderTypeId, type Side, type TicketCtx, type TicketForm,
} from './form-state';
import { expectedFromPlan } from './plan-expected';
import { confirmNotes, confirmSignerNotes, minOrderValueOf, minValueNote } from './ticket-notes';
import { expandLadder, expandLimitLadder, ladderOfForm, ladderTotals, limitLadderOfForm, limitLadderValues, type LadderLevel } from './ladder';
import { maxBuyAmount } from './max-amount';
import { MERGE_ANSWERS, useMergeBeforeSale } from './merge-before-sale';
import { orderLabel } from './order-label';
import { usePairTicketEnv, useTicketEnv, useTicketPlan, type EnvWait } from './use-ticket';
import { CLOCK_TICK_MS, CLOCK_WAIT_MS, REVIEW_CACHE_MS, REVIEW_REUSE_MS, keepTermsPlan, liveClockAt, reviewClock, sameTransaction } from './review-check';
import { rememberPairForm, takePairFlip } from './pair-flip';
import './ticket.css';

export interface OrderTicketProps {
  /** The token traded (the BASE token A of a pair). */
  token: TokenInfo;
  /**
   * A token/token pair A/B: the QUOTE token B. The ticket then offers exactly the same order types.
   * Prices are typed in B per whole A, amounts in A, and tips stay KAS. The planner builds pair
   * orders (KobPair, KobCondPair, KobIfdPair) against the pair environment (both tokens, the pair
   * book). Absent means the token's KAS market.
   */
  quote?: TokenInfo;
  /** Optional prefill from the book (a click on a level). */
  /** `price` is the state price (sompi per whole token) and `amount` is in base units. */
  prefill?: { side?: 'buy' | 'sell'; price?: bigint; amount?: bigint; type?: OrderTypeId; /** a click counter: a new value re-applies an unchanged prefill (a re-click of the same price after the field was edited) */ seq?: number };
  /**
   * The market is shown inverted (KAS/TOKEN), so the ticket speaks the shown pair, whose base is
   * KAS. The side buttons name the side of KAS (Buy is buy KAS, which is sell the token). Prices
   * are tokens per KAS, and amounts are KAS converted at the order's price (form-state.ts, KAS
   * amounts). The order built is the native one. A flip of a filled form keeps it exactly
   * (orientation.ts).
   */
  inverted?: boolean;
  /**
   * Facts about the token(s) traded, shown above the notes next to the button. They are the
   * indexer's warnings and the program's powers (market/TokenBadges.tsx `TokenFacts`).
   */
  facts?: ComponentChildren;
}

/**
 * A level that could not be planned at the click. It holds the sentence, the raw builder text
 * behind "Details", and the code of the status line.
 */
interface PlanStop {
  text: string;
  raw?: string;
  code?: string;
}

/**
 * One reason the order cannot be placed now (the slot above the button). `kind` is how the tests
 * and the styles tell them apart, and `testid` overrides the default one.
 */
interface Reason {
  code: string;
  text: string;
  /**
   * `warn`: the order can be placed, but not as it is meant (below the minimum order value, it is
   * never listed). It is shown in the slot even while the button is on.
   */
  kind: 'need' | 'field' | 'issue' | 'ladder' | 'warn';
  action?: 'retry';
  raw?: string;
  testid?: string;
}

/** Token-holding shortfalls. While the holdings are still being read they mean "not read yet". */
const TOKEN_SHORT = new Set(['INSUFFICIENT_TOKENS', 'TOKEN_UTXOS_FRAGMENTED']);

const sum = (xs: bigint[]): bigint => xs.reduce((a, b) => a + b, 0n);

/**
 * How long the sale after a merge waits for an environment that holds the merged UTXOs, before the
 * ticket gives the button back.
 */
const SELL_AFTER_MERGE_MS = 30_000;

/**
 * A plan that takes longer than this shows its "Calculating..." indicator. A background re-plan on
 * a tick is over before and shows nothing.
 */
const PENDING_SHOW_MS = 500;
const HELP_KEY = 'kob.ticket.help';

/** `on` once it has been on for `ms`, false again the moment it goes off. */
function useDelayedFlag(on: boolean, ms: number): boolean {
  const [shown, setShown] = useState(false);
  useEffect(() => {
    if (!on) {
      setShown(false);
      return;
    }
    const h = setTimeout(() => setShown(true), ms);
    return () => clearTimeout(h);
  }, [on, ms]);
  return on && shown;
}

/**
 * "Enter an amount", "Enter a price" or "Enter a stop price": the instruction a disabled submit
 * button carries for the first empty required field.
 */
function enterTextOf(field: string, type: string, inverted: boolean): string {
  if (has(`ticket.enter.${field}`)) return t(`ticket.enter.${field}`);
  const label = t(inverted && has(`ticket.label.${field}.inv`) ? `ticket.label.${field}.inv` : labelKey(type, field));
  return t('ticket.enter.other', { field: /^[A-Z][a-z]/.test(label) ? label.charAt(0).toLowerCase() + label.slice(1) : label });
}

export function OrderTicket(props: OrderTicketProps) {
  const { token, quote } = props;
  // A pair is never shown inverted here. The pair page flips by swapping base and quote, which is
  // another pair and another route.
  const inverted = !quote && !!props.inverted;
  // A ticker that reads as KAS carries 8 + 8 hex of the id (`tickerUnit`), so no amount here reads as
  // native KAS.
  const name = tickerUnit(token.ticker, token.covenantId);
  /** The ticker the prices are in: KAS, or the pair's quote token B. */
  const quoteTicker = quote ? tickerUnit(quote.ticker, quote.covenantId) : 'KAS';
  const services = useServices();
  const contended = useCongestion();
  const wallet = useWallet();
  const { indexer: indexerStatus } = useSystemStatus();
  // The app is taking the market from the node because the indexer just went away. A guard that
  // could not be read says that, for as long as it lasts.
  const switching = useTradingStatus().code === 'switching';
  const tradable = token.tradable && token.program !== null && (!quote || (quote.tradable && quote.program !== null));
  // The order scale of this token (10^decimals, at most 10^9, kob-wasm defaultScale). Every price
  // is per that many base units.
  const scale = 10n ** BigInt(Math.min(token.decimals, 9));
  const walletReady = !!wallet.info && !!wallet.adapter && !wallet.networkMismatch;
  const enabled = tradable && walletReady;

  // A pair page just flipped (A/B to B/A). The order typed there becomes the same order on this
  // pair (pair-flip.ts).
  const [carried] = useState<TicketForm | null>(() => (quote ? takePairFlip(token.covenantId, quote.covenantId, { base: quote.decimals, quote: token.decimals }) : null));
  const [form, setForm] = useState<TicketForm>(() => carried ?? initialForm('limit', props.prefill?.side ?? 'sell', inverted));
  useEffect(() => {
    if (quote) rememberPairForm(token.covenantId, quote.covenantId, form);
  }, [form]);
  // The send flow of the order button: checking, the wallet's approval, sending and the result. It
  // is one line under the button.
  const sender = useSend();
  // From the click until the last transaction of it is sent or stopped. A ladder sends every level,
  // each its own transaction signed in turn.
  const [placing, setPlacing] = useState(false);
  // A take (placeTake): the steps of its stepper, and the facts of the plan the click was for. They
  // are shown while it runs.
  const [takeRun, setTakeRun] = useState<StepperItem[] | null>(null);
  const [takeAtClick, setTakeAtClick] = useState<TakeFacts | null>(null);
  // A ladder: the level being sent ("Level 2 of 3"), and how the whole ladder ended.
  const [ladderLead, setLadderLead] = useState<string | null>(null);
  const [ladderNote, setLadderNote] = useState<{ done: boolean; text: string } | null>(null);
  // A sell ladder placed as one curve order or as separate orders. Null is the default, which is
  // one curve above 2 levels.
  const [placeAs, setPlaceAs] = useState<'curve' | 'separate' | null>(null);
  // The level of a separate ladder being placed, for its stepper.
  const [ladderAt, setLadderAt] = useState<{ at: number; levels: number; failed?: boolean } | null>(null);
  // What Max just did, in a sentence. The next edit clears it.
  const [maxNote, setMaxNote] = useState<string | null>(null);

  // One of the two environments is live: the token's KAS market, or the pair A/B (both tokens'
  // UTXOs, the pair book, own pair orders).
  // The book is verified on the node as deep as an order that walks it needs. That is a market,
  // close, streaming, IOC or FOK order, and its amount sets the depth. `walkDepth` is set from the
  // order built below, so the first read is the shallow one and a bigger order reads deeper once.
  const [walkDepth, setWalkDepth] = useState(0n);
  const kasEnv = useTicketEnv(token, enabled && !quote, walkDepth);
  const pairEnv = usePairTicketEnv(token, quote ?? token, enabled && !!quote, walkDepth);
  const { env: rawEnv, error: envError, errorSource: envSource, loading: envLoading, refresh, since, snapshot, stale, waiting } = quote ? pairEnv : kasEnv;
  // The node clock, read every CLOCK_TICK_MS while an order can be placed (a visible page). A click
  // plans with it projected to the click, with no round trip before the wallet is asked
  // (review-check.ts rule 1).
  const liveClock = useRef<{ clock: Clock; at: number } | null>(null);
  useEffect(() => {
    if (!enabled) return;
    let live = true;
    const read = () => {
      if (typeof document !== 'undefined' && document.visibilityState === 'hidden') return;
      readClock(services).then((clock) => live && (liveClock.current = { clock, at: Date.now() }), () => undefined);
    };
    read();
    const h = setInterval(read, CLOCK_TICK_MS);
    return () => {
      live = false;
      clearInterval(h);
    };
  }, [enabled, services]);
  // A market or close order cannot be placed when its start (the indexer's best price) is this far
  // beyond another indexer's best price.
  const startTolerance = BigInt(services.config.marketStartToleranceBps);
  // The best prices further indexers report for this market, empty when none is configured. For a
  // KAS market these are its own books, and for a pair they come from the two KAS books.
  useFailoverState(services.failover); // on a backup indexer the references are the other indexers: this ticket re-reads them after a switch
  const verifiers = services.verifiers;
  const marketOf = rawEnv ? rawEnv.token : null;
  const quoteMarketOf = rawEnv && isPairEnv(rawEnv) ? rawEnv.pair.quote : null;
  const touchesOf = (signal?: AbortSignal): Promise<References> => {
    if (!marketOf || !verifiers?.length) return Promise.resolve({ touches: [], unavailable: [] });
    if (!quote) return readReferences(verifiers, token.covenantId, (v) => bookFromIndexer(marketOf, v), signal);
    if (!quoteMarketOf) return Promise.resolve({ touches: [], unavailable: [] });
    return readPairReferences(
      verifiers,
      { covenantId: marketOf.covenantId, toBook: (v) => bookFromIndexer(marketOf, v) },
      { covenantId: quoteMarketOf.covenantId, toBook: (v) => bookFromIndexer(quoteMarketOf, v), scale: quoteMarketOf.scale },
      signal,
    );
  };
  const touches = useAsync((signal) => touchesOf(signal), [marketOf, quoteMarketOf, verifiers, token.covenantId]);
  const withRefs = <E extends PlanEnv>(e: E, refs: References): E => ({
    ...e, referenceTouches: refs.touches, referencesUnavailable: refs.unavailable, marketStartToleranceBps: startTolerance,
  });
  const env = useMemo(() => (rawEnv ? withRefs(rawEnv, touches.data ?? { touches: [], unavailable: [] }) : null), [rawEnv, touches.data, startTolerance]);
  const rateMilli = env?.clock.rateMilli;
  // The book's best prices. A KAS amount of a market or close order converts at them (an inverted
  // market).
  const bestAsk = env?.book.asks[0]?.price ?? null;
  const bestBid = env?.book.bids[0]?.price ?? null;
  // The delivery of a buy (kob/orders/delivery.ts): the wallet default of Settings and the receive
  // threshold this wallet stored for the token.
  const [walletDelivery] = useDeliveryPref();
  const family = token.program && isTokenProgram(token.program) ? CODEC_OF_PROGRAM[token.program] : undefined;
  const quoteFamily = quote?.program && isTokenProgram(quote.program) ? CODEC_OF_PROGRAM[quote.program] : undefined;
  const pubkey = wallet.info?.pubkey ?? null;
  const ext = env?.token.extensionCommitment ?? null;
  const [thresholdTick, setThresholdTick] = useState(0);
  const storedThreshold = useMemo(
    () => (pubkey && ext ? readThreshold({ maker: pubkey, tokenCovId: token.covenantId, extensionCommitment: ext }) : undefined),
    [pubkey, ext, token.covenantId, thresholdTick],
  );
  const ctx: TicketCtx = useMemo(
    () => ({
      decimals: token.decimals, scale, tick: token.tick ?? 1n, tzOffsetMin: new Date().getTimezoneOffset(), ...(rateMilli ? { rateMilli } : {}),
      ...(quote ? { quoteDecimals: quote.decimals } : { ref: { buy: bestAsk, sell: bestBid } }),
      ...(family ? { family } : {}),
      ...(quoteFamily ? { quoteFamily } : {}),
      delivery: { walletDefault: walletDelivery, ...(storedThreshold !== undefined ? { threshold: storedThreshold } : {}) },
    }),
    [token.decimals, scale, token.tick, rateMilli, quote?.decimals, bestAsk, bestBid, family, quoteFamily, walletDelivery, storedThreshold],
  );
  // A streaming order's "price you saw" is the quote its side meets (buy: the best ask, sell: the
  // best bid). It is filled in and kept on the quote until the user types a price of their own.
  const streamQuote = form.type === 'streaming' ? streamingQuoteText(form.side, { ask: bestAsk, bid: bestBid }, ctx) : null;
  const autoQuote = useRef<string | null>(null);
  useEffect(() => {
    const r = followQuote(form, streamQuote, autoQuote.current);
    autoQuote.current = r.auto;
    if (r.form !== form) setForm(r.form);
  }, [form, streamQuote]);
  // A flip of the market recounts the amount boxes in the shown base (KAS or the token) and keeps
  // the order they build exactly.
  useEffect(() => {
    setForm((f) => reorientAmounts(f, inverted, ctx));
  }, [inverted]);
  const built = useMemo(() => buildIntent(form, ctx), [form, ctx]);
  const ladderSet = useMemo(() => ladderOfForm(form, ctx), [form, ctx]);
  const limitSet = useMemo(() => limitLadderOfForm(form, ctx), [form, ctx]);
  // A ladder places each level as one transaction. A crossing level is the auction, because a take
  // would leave its rest unplaced.
  const planIntent = useMemo((): Intent | null => {
    const i = built.intent;
    return i && (ladderSet || limitSet) && i.type === 'limit' && i.crossing === 'take' ? { ...i, crossing: 'auction' } : i;
  }, [built.intent, ladderSet, limitSet]);
  const { plan, current, pending, editPending, crash } = useTicketPlan(enabled ? env : null, planIntent);
  // After an edit of the form it shows at once. A re-plan on a tick (a new environment) shows it
  // only if it takes longer than PENDING_SHOW_MS.
  const slowPending = useDelayedFlag(pending, PENDING_SHOW_MS);
  const pendingShown = editPending || slowPending;

  // Prefill from the book. A price implies a limit-style form, and a side the matching order type.
  // A click on the book or the depth chart fills the price only and keeps the ticket's side.
  const prefillKey = props.prefill ? `${props.prefill.seq ?? ''}|${props.prefill.type ?? ''}|${props.prefill.side ?? ''}|${props.prefill.price ?? ''}|${props.prefill.amount ?? ''}` : '';
  useEffect(() => {
    const p = props.prefill;
    if (!p) return;
    setForm((f0) => {
      let f = p.type ? switchType(f0, p.type) : f0;
      if (p.side && !sidesOf(f.type).includes(p.side)) f = switchType(f, 'limit');
      if (p.side) f = setSide(f, p.side);
      if (p.price !== undefined) {
        if (!layoutOf(f, ctx).main.includes('price')) f = switchType(f, 'limit');
        f = setValue(f, 'price', formatPrice(p.price, ctx));
      }
      if (p.amount !== undefined) f = setAmountBase(f, 'amount', p.amount, ctx);
      return f;
    });
  }, [prefillKey]);

  const ladder = useMemo(() => {
    if (!planIntent) return null;
    if (ladderSet) return expandLadder(planIntent, ladderSet.levels, ladderSet.step, ctx.tick);
    if (limitSet) return expandLimitLadder(planIntent, limitSet, ctx.tick);
    return null;
  }, [ladderSet, limitSet, planIntent, ctx.tick]);

  // A sell ladder of a KAS market that can be one curve order (KCC-20, prices that never fall), and
  // whether it is placed so.
  const curveIntent: CurveIntent | null = useMemo(
    () => (limitSet && ladder?.ok && form.side === 'sell' && !quote && token.family === 'kcc20' ? curveOfLadder(ladder, built.intent) : null),
    [limitSet, ladder, form.side, quote, token.family, built.intent],
  );
  const asCurve = !!curveIntent && (placeAs ?? (ladder!.levels.length > 2 ? 'curve' : 'separate')) === 'curve';

  const layout = layoutOf(form, ctx);
  const errs = visibleErrors(built.errors).filter((e) => (form.values[e.field] ?? '') !== '');
  const ticker = name;

  // Balances the wallet can spend. The planner does the exact funding check.
  const tokenBalance = env ? heldAmount(env) : 0n;
  // Held units of this token the planner cannot spend: another owner type, another extension, or a
  // KRON minter UTXO. A key-owned holding with a borrow setting counts.
  const unusable = env ? unusableTokenHeld(env) : null;
  // Another market starts shallow again. It is declared before the effect that sets it, so a change
  // of both in one pass ends with the set one.
  useEffect(() => setWalkDepth(0n), [token.covenantId, quote?.covenantId]);
  const intentType = built.intent?.type;
  const intentAmount = (built.intent as { amount?: bigint } | null)?.amount;
  const li = built.intent?.type === 'limit' ? built.intent : null;
  const limitCrosses = !!li && li.crossing === 'take' && (li.side === 'buy' ? bestAsk !== null && li.price >= bestAsk : bestBid !== null && li.price <= bestBid);
  useEffect(() => {
    // A crossing limit that fills from the book walks it too, so as many of its orders are verified
    // as it may take.
    const takes = intentType === 'limit' && limitCrosses;
    const walks = intentType === 'market' || intentType === 'streaming' || intentType === 'close' || intentType === 'fok' || intentType === 'ioc' || takes;
    const need = walks ? (intentAmount ?? (intentType === 'close' ? tokenBalance : 0n)) : 0n;
    // Only ever deeper for this market. A smaller amount keeps the book already read.
    setWalkDepth((d) => (need > d ? need : d));
  }, [intentType, intentAmount, limitCrosses, intentType === 'close' ? tokenBalance : 0n]);
  const kasBalance = env ? sum(env.funding.map((u) => BigInt(u.amount))) : 0n;
  const pairPart = env && isPairEnv(env) ? env.pair : null;
  /** A pair: the tip fields typed below what a netting fill needs (the order's own, an exit's). */
  const lowTips = useMemo(() => (env && isPairEnv(env) ? lowPairTipFields(env, built.intent as Parameters<typeof lowPairTipFields>[1]) : []), [env, built.intent]);
  /** A pair: the quote token B the wallet can spend (a buy pays in B). */
  const quoteBalance = pairPart ? sum(pairPart.quoteTokenUtxos.map((u) => BigInt(u.state.amount))) : 0n;
  const refPrice = (() => {
    const own = parsePrice(form.values.price ?? form.values.displayedPrice ?? '', form.side, ctx);
    if (own.ok) return own.value.price;
    return env?.book.asks[0]?.price ?? env?.book.bids[0]?.price ?? 0n;
  })();
  // What the inverted fields need to speak KAS: the price of the form (else the book's best), per
  // whole token of `scale` base units.
  const ownPrice = parsePrice(form.values.price ?? form.values.displayedPrice ?? '', form.side, ctx).ok;
  const flip = { name, price: refPrice > 0n ? refPrice : null, ownPrice, scale, decimals: token.decimals };
  /**
   * The largest buy the wallet can place at the form's price, or at the book's best when it is
   * empty. It is a binary search on the amount, each step planned by the real planner, so carriers,
   * the network fee and the escrow rounding are exact. A small reserve stays in the wallet. A
   * ladder searches the amount of one order such that all its orders together are affordable.
   * Fields the planner cannot plan without (a missing stop) fall back to a conservative rule of
   * thumb.
   */
  const maxBuy = (tip: bigint): bigint => {
    if (!env || refPrice <= 0n) return 0n;
    const withAmount = (amount: bigint): TicketForm => {
      const f = setAmountBase(form, 'amount', amount, ctx);
      return (form.values.price ?? '') === '' && layoutOf(f, ctx).main.includes('price') ? setValue(f, 'price', formatPrice(refPrice, ctx)) : f;
    };
    // The prices of a ladder do not depend on the amount, so they are read off a probe.
    let prices: bigint[] = [refPrice];
    if (limitSet) {
      const probe = buildIntent(withAmount(1n), ctx).intent;
      const lad = probe ? expandLimitLadder(probe, limitSet, ctx.tick) : null;
      if (lad?.ok) prices = lad.levels.map((l) => l.entryPrice);
    }
    const levels = BigInt(prices.length);
    const sumPrice = prices.reduce((a, p) => a + p + (quote ? 0n : tip), 0n);
    const balance = quote ? quoteBalance : kasBalance;
    const keep = quote ? 4n * levels : (DEFAULT_CARRIER * 4n + 20_000_000n) * levels;
    const conservative = balance > keep ? ((balance - keep) * scale) / sumPrice : 0n;
    const upper = (balance * scale) / sumPrice + 1n;
    return maxBuyAmount({
      upper, conservative, kasBalance, ...(quote ? { quoteBalance } : {}), ladderPrices: prices.length > 1 ? prices : null,
      planAt: (amount) => {
        const intent = buildIntent(withAmount(amount), ctx).intent;
        return intent ? planOrder(env, intent) : null;
      },
    });
  };
  /**
   * What Max gives for the form as it is. A sell gives the whole token balance, split over the
   * orders of a ladder. A buy is searched against the planner.
   */
  const computeMax = (): bigint => {
    if (form.side === 'sell') return tokenBalance / BigInt(limitSet?.levels ?? 1);
    // The form's own tip, read off the order at one base unit. The amount may still be empty.
    const probe = built.intent ?? buildIntent(setAmountBase(form, 'amount', 1n, ctx), ctx).intent;
    const tip = probe && 'tip' in probe && probe.tip ? probe.tip : 0n;
    return maxBuy(tip);
  };
  const onMax = () => {
    const n = computeMax();
    setForm((f) => (n > 0n ? setAmountBase(f, 'amount', n, ctx) : setValue(f, 'amount', '')));
    setMaxNote(form.side === 'buy' && n <= 0n ? t('ticket.max.none') : null);
  };
  // The amount slider is 0..100 % of Max. It is recomputed when anything but the amount changes
  // (type, side, price, tip, balances, the book). A buy's search runs the planner, so it waits for
  // typing to pause.
  // The Max of the slider is kept with the form it was computed for. A stale one (another side,
  // type or price) is never used.
  const [sliderMax, setSliderMax] = useState<{ key: string; max: bigint | null } | null>(null);
  const { amount: _amount, [`amount${BASE_SUFFIX}`]: _amountBase, ...otherValues } = form.values;
  void _amount;
  void _amountBase;
  const maxKey = enabled && env ? JSON.stringify([form.type, form.side, !!form.kas, otherValues, String(tokenBalance), String(kasBalance), String(quoteBalance), String(refPrice)]) : '';
  useEffect(() => {
    if (!maxKey) {
      setSliderMax(null);
      return;
    }
    const key = maxKey;
    const h = setTimeout(() => {
      let max: bigint | null = null;
      try {
        max = computeMax();
      } catch {
        max = null;
      }
      setSliderMax({ key, max });
    }, form.side === 'sell' ? 0 : 250);
    return () => clearTimeout(h);
  }, [maxKey, env]);
  const baseOf = (id: string): bigint | null => {
    if ((form.values[id] ?? '') === '' && form.values[id + BASE_SUFFIX] === undefined) return null;
    const b = amountBaseOf(form, ctx, id);
    return b.ok && b.value > 0n ? b.value : null;
  };
  const slider = {
    max: sliderMax && sliderMax.key === maxKey ? sliderMax.max : null,
    amount: baseOf('amount'),
    minFill: baseOf('minFill'),
    describe: (b: bigint) => (form.kas ? `${kasTextOf(form, ctx, 'amount', b)} KAS` : `${formatUnits(b, token.decimals, { group: ',' })} ${ticker}`),
    onPick: (b: bigint | null) => {
      edited();
      setForm((f) => (b === null ? setValue(f, 'amount', '') : setAmountBase(f, 'amount', b, ctx)));
    },
    side: (inverted ? oppositeSide(form.side) : form.side) as Side,
    disabled: !enabled || !env,
  };

  const onChange = (id: string, value: string) => {
    edited();
    // A tip typed by hand is the user's own, so the executor tip option goes off.
    setForm((f) => {
      const g = setValue(f, id, value);
      return id === 'tip' && f.values.tipAuto === 'true' ? setValue(g, 'tipAuto', '') : g;
    });
  };
  const onType = (type: OrderTypeId) => {
    edited();
    setForm((f) => switchType(f, type));
  };
  const onSide = (side: Side) => {
    edited();
    setForm((f) => setSide(f, side));
  };

  const shownPlan: OrderPlan | null = enabled && current ? plan : null;
  // The executor tip option (kob/fill-tip.ts) is the tip of a match of this order, measured on its
  // placement. A take places none, so the same order as a resting one is measured. It is written
  // into the tip box while the option is on.
  const tipAutoOn = form.values.tipAuto === 'true' && !quote;
  const autoTip = useMemo(() => {
    if (!tipAutoOn || !env || !shownPlan || !planIntent) return null;
    const probe = shownPlan.take ? planOrder(env, { ...planIntent, ...(planIntent.type === 'limit' ? { crossing: 'limit' } : { take: false }) } as Intent) : shownPlan;
    try {
      return probe.built ? executorTip(env, probe) : null;
    } catch {
      return null;
    }
  }, [tipAutoOn, env, shownPlan, planIntent]);
  useEffect(() => {
    if (!autoTip) return;
    const text = formatPrice(autoTip.tip, ctx, 'kas');
    if ((form.values.tip ?? '') !== text) setForm((f) => (f.values.tipAuto === 'true' ? setValue(f, 'tip', text) : f));
  }, [autoTip, form.values.tip]);
  // What a crossing order fills from the book. While a click runs it is the one the click was for.
  // Otherwise it is the plan's, planned again for a new amount, price, side or book.
  const takeFacts: TakeFacts | null = (placing && takeRun ? takeAtClick : null) ?? (!ladder && shownPlan?.take && shownPlan.ok ? shownPlan.take : null);
  const takeLines = takeFacts
    ? {
        fills: t('ticket.take.fills', {
          amount: formatUnits(takeFacts.total.amount, token.decimals, { group: ',' }), ticker,
          price: takeFacts.total.amount > 0n ? formatKas((takeFacts.total.kas * takeFacts.scale) / takeFacts.total.amount, { group: ',' }) : '0',
        }),
        // About two significant digits. The final confirmation has the exact fee. On a narrow
        // ticket it is cut first.
        fee: t('ticket.take.fee', { fee: kasApprox(takeFacts.fee * BigInt(takeFacts.steps.length)) }),
        rest: takeFacts.rest > 0n
          ? t(takeFacts.restKind === 'limit' ? 'ticket.take.rest' : 'ticket.take.restNone', { amount: formatUnits(takeFacts.rest, token.decimals, { group: ',' }), ticker, price: formatKas(takeFacts.limit, { group: ',' }) })
          : null,
      }
    : null;
  const tipAutoNote = tipAutoOn ? (autoTip ? t('ticket.tipAuto.total', { kas: formatKas(autoTip.total, { group: ',' }) }) : t('ticket.tipAuto.unknown')) : undefined;
  const planErrors = shownPlan ? errors(shownPlan) : [];
  // The delivery rows of Advanced: the choice in force, the program's default Top-up tip, and what
  // Top-up costs at most (the shown plan's fills).
  const delivery = deliveryOf(form, ctx);
  const planned = shownPlan ? planDelivery(shownPlan) : null;
  const carrier = env?.carrier ?? DEFAULT_CARRIER;
  const defaultTip = token.program ? programTopupTip(token.program, carrier) : null;
  const topupThreshold = planned?.delivery.choice === 'topup' ? planned.delivery.threshold : null;
  const [deliveryHelp, setDeliveryHelp] = useState(false);
  const deliverySubs: Record<string, { text: string; testid: string }> = {};
  // One line under the select that follows the choice. For Added to one UTXO, its tip and the tip
  // total follow under the tip box.
  // A pair sell receives token B, so the line names it.
  if (delivery) deliverySubs.delivery = { text: quote && form.side === 'sell' ? t(`ticket.delivery.hint.${delivery}.pairSell`, { quote: quoteTicker }) : t(`ticket.delivery.hint.${delivery}`), testid: 'order-delivery-note' };
  if (delivery === 'topup' && planned?.delivery.choice === 'topup') {
    const tip = BigInt(planned.delivery.fields.topupTip);
    const kas = formatKas(topupTipTotal(tip, planned.fills), { group: ',' });
    deliverySubs.topupTip = { text: planned.fills === 1n ? t('ticket.topup.total.one', { kas }) : t('ticket.topup.total.other', { kas, count: planned.fills.toString() }), testid: 'order-topup-total' };
  }
  // Under the amount box: what this amount costs (a buy) or yields (a sell) in the quote, from the
  // book the planner sees. It is a fixed line and shows "—" when empty.
  // The USD display (Settings) is a muted read-only "≈ $x" beside the estimate of a KAS-quoted
  // order. The ticket's own figures and everything signed stay KAS.
  const fiat = useFiat();
  const costText = useMemo(() => {
    const pv = env
      ? costPreview({ intent: built.intent, book: env.book, scale: env.token.scale, worstPrice: plan?.disclosure?.worstPrice ?? null, fee: quote ? null : plan?.disclosure?.fee ?? null, take: plan?.take ?? null })
      : ({ kind: 'empty' } as const);
    // The figures without their unit. The unit stands once, in the units column under the amount's
    // (TicketFields.tsx).
    const text = costLine(
      pv,
      (v) => (quote ? formatUnits(v, quote.decimals, { group: ',' }) : formatKas(v, { group: ',' })),
      (v) => formatUnits(v, token.decimals, { group: ',' }),
      (key, params) => t(`ticket.cost.${key}`, params),
    );
    // The dollars of the estimate. Only a KAS quote has a price in dollars. The slot stays while
    // USD is chosen, so nothing moves when a figure comes or goes.
    const usd = fiat && !quote ? { text: pv.kind === 'cost' ? `≈ ${usdTextFit(sompiToUsd(pv.estimate, fiat.rate), 11)}` : '', title: pv.kind === 'cost' ? kasTitleOf(formatKas(pv.estimate, { group: ',' }), fiat) : '' } : null;
    return { text, unit: pv.kind === 'empty' ? '' : pv.kind === 'thin' ? ticker : quote ? quoteTicker : 'KAS', usd };
  }, [env, built.intent, plan, quote, token.decimals, ticker, fiat]);
  /** An amount of the market's quote (KAS, or a pair's token B) in words. */
  const quoteText = (v: bigint): string => (quote ? `${formatUnits(v, quote.decimals, { group: ',' })} ${quoteTicker}` : `${formatKas(v, { group: ',' })} KAS`);
  // The last order of a limit ladder is the one furthest from the typed price. It is planned too,
  // so a level that could never be placed is refused before the first is signed.
  // One curve order: its own plan. Its errors stop the button, and its carriers and fee are what
  // placing it costs.
  const curvePlan = useMemo(() => {
    if (!enabled || !env || !asCurve || !curveIntent) return null;
    try {
      return planOrder(env, curveIntent);
    } catch {
      return null;
    }
  }, [enabled, env, asCurve, curveIntent]);
  // A curve order: the KAS it locks plus the network fee. It locks the order's and its custody's
  // carriers, returned when it sells out or is cancelled.
  const curveCost = asCurve ? (curvePlan?.disclosure ? curvePlan.disclosure.kasLocked + (curvePlan.disclosure.fee ?? 0n) : null) : undefined;
  const footText = curveCost === undefined ? costText : curveCost === null ? { text: t('ticket.cost.dash'), unit: '', usd: null } : { text: formatKas(curveCost, { group: ',' }), unit: 'KAS', usd: null };
  const lastOrderErrors = useMemo(() => {
    if (!enabled || !env || !limitSet || !ladder?.ok) return [];
    try {
      if (asCurve) return curvePlan ? errors(curvePlan) : [];
      return errors(planOrder(env, ladder.levels[ladder.levels.length - 1]!.intent));
    } catch {
      return [];
    }
  }, [enabled, env, limitSet, ladder, asCurve, curvePlan]);
  const ladderTot = useMemo(() => (shownPlan && ladder ? ladderTotals(shownPlan, ladder) : null), [shownPlan, ladder]);
  // A ladder the wallet cannot pay for as a whole (each level alone may plan). The shortfall is an
  // error and the button stays disabled.
  const ladderFunds: { code: string; text: string }[] = [];
  if (env && ladder?.ok) {
    if (ladderTot && form.side === 'buy' && !quote && ladderTot.kasLocked > kasBalance) {
      ladderFunds.push({ code: 'kas', text: t('ticket.ladder.fundsKas', { kas: `${formatKas(ladderTot.kasLocked, { group: ',' })} KAS`, have: `${formatKas(kasBalance, { group: ',' })} KAS` }) });
    }
    if (ladderTot && (!quote || form.side === 'sell') && ladderTot.tokens > tokenBalance) {
      ladderFunds.push({ code: 'tokens', text: t('ticket.ladder.fundsTokens', { need: formatUnits(ladderTot.tokens, token.decimals, { group: ',' }), have: formatUnits(tokenBalance, token.decimals, { group: ',' }), ticker }) });
    }
    if (limitSet && quote && form.side === 'buy' && limitLadderValues(ladder, 'buy', scale).total > quoteBalance) {
      ladderFunds.push({ code: 'quote', text: t('ticket.ladder.fundsQuote', { need: quoteText(limitLadderValues(ladder, 'buy', scale).total), have: quoteText(quoteBalance) }) });
    }
  }
  // A wallet not known to sign covenant inputs (every cancel needs one) passes a one-time test
  // signature before its first order.
  const gate = useCovenantSignGate(enabled ? env : null);
  // Too many UTXOs of the token for one sale. The ticket offers to merge just enough first, then
  // places the sale (merge-before-sale.tsx). This is for a single order of a KAS market only,
  // because a ladder's levels and a pair's quote token are planned apart.
  const [sellAfterMerge, setSellAfterMerge] = useState<{ leaves: ReadonlySet<string>; at: number } | null>(null);
  const merge = useMergeBeforeSale({
    token, ticker: name, pubkey: wallet.info?.pubkey ?? null, env,
    issue: enabled && !quote && !ladder ? planErrors.find((i) => MERGE_ANSWERS.has(i.code)) : undefined,
    refresh,
    onMerged: (leaves) => setSellAfterMerge({ leaves, at: Date.now() }),
    onStop: (text, raw) => sender.stop(text, raw, 'merge'),
  });
  // A click that ended without its order (blocked, declined, refused by the node, or a direct fill
  // that lost its race every time) is also said at the top right in red, once the click is over.
  // That is the only place it is said. There is no red box in the ticket.
  useSendFailureToast(
    sender.status,
    placing,
    t(sender.status?.code === 'merge' ? 'send.failed.merge' : takeRun ? 'send.failed.take' : 'send.failed.order'),
  );
  const canPlace =
    enabled && gate.ready && !!shownPlan && shownPlan.ok && planErrors.length === 0 && !!shownPlan.built && !pending && !placing && !sendBusy(sender.status) && !crash
    && (!ladder || ladder.ok) && lastOrderErrors.length === 0 && ladderFunds.length === 0 && !merge.running;
  // The only thing in the way is that the sale needs more token UTXOs than one transaction takes,
  // and a merge can help. The order button itself merges first, then places the sale. Its label and
  // the note above it say so.
  const mergeFirst =
    !!merge.offer && enabled && gate.ready && !!shownPlan && !pending && !placing && !sendBusy(sender.status) && !crash && !merge.running
    && planErrors.length > 0 && planErrors.every((i) => i.code === planErrors[0]!.code && MERGE_ANSWERS.has(i.code));
  // An edit makes a reason the last click stopped for stale. A submitted order stays on the line
  // until the next click.
  const edited = () => {
    setMaxNote(null);
    setLadderNote(null);
    setLadderAt(null);
    if (!placing) setTakeRun(null);
    if (!placing && sender.status && sender.status.phase !== 'submitted') sender.clear();
  };

  // An inverted market words the type where the native name or help speaks of buying or selling the
  // token.
  const typeKey = (id: OrderTypeId, part: 'name' | 'help'): string => (inverted && has(`ticket.type.${id}.${part}Inv`) ? `ticket.type.${id}.${part}Inv` : `ticket.type.${id}.${part}`);
  const typeName = t(typeKey(form.type, 'name'), { name });
  const typeHelp = t(typeKey(form.type, 'help'), { name });
  // The side as the shown pair names it. Buying KAS in KAS/TOKEN is selling the token.
  const shownSide: Side = inverted ? oppositeSide(form.side) : form.side;
  const sideName = t(shownSide === 'buy' ? 'common.side.buy' : 'common.side.sell');

  /**
   * One order of the ladder in words: the level and its prices. A limit ladder adds side, amount,
   * price and value.
   */
  const ladderRow = (l: LadderLevel): string => {
    if (!limitSet) return ladderRepeatRow(l);
    const amount = (l.intent as { amount: bigint }).amount;
    return t(asCurve ? 'ticket.ladder.rowCurve' : 'ticket.ladder.rowLimit', {
      level: l.level, side: t(form.side === 'buy' ? 'common.side.buy' : 'common.side.sell'), amount: `${formatUnits(amount, token.decimals, { group: ',' })} ${ticker}`,
      price: `${formatPrice(l.entryPrice, ctx)} ${quoteTicker}`, value: quoteText(quoteOf(amount, l.entryPrice, scale, form.side === 'sell' ? 'up' : 'down')),
    });
  };
  const ladderRepeatRow = (l: LadderLevel): string =>
    t(l.exitStop !== null ? 'ticket.ladder.rowStop' : 'ticket.ladder.row', {
      level: l.level,
      entry: `${formatPrice(l.entryPrice, ctx)} ${quoteTicker}`,
      exit: l.exitTakeProfit !== null ? `${formatPrice(l.exitTakeProfit, ctx)} ${quoteTicker}` : '-',
      stop: l.exitStop !== null ? `${formatPrice(l.exitStop, ctx)} ${quoteTicker}` : '-',
    });

  const noRefs = (): References => ({ touches: [], unavailable: verifiers.map((v) => v.label) });

  /**
   * The plan made at the click within the terms the ticket showed (`shown`, review-check.ts
   * `withinTerms`). It is the order to sign, whatever its start price, coins or fee. A market or
   * close order whose auction would end beyond the worst price shown is planned again with the
   * slippage narrowed to end there. A touch already beyond it, or another order kind past its
   * shown total, stops the order.
   */
  const keepTerms = (p: OrderPlan, intent: Intent, plan: (i: Intent) => OrderPlan, shown: OrderPlan | null): { p: OrderPlan; intent: Intent } | PlanStop =>
    keepTermsPlan(p, intent, plan, shown) ?? { text: t('ticket.beyondTerms'), code: 'terms' };

  /**
   * Plans one order of the click at once. `index` is its place among `levels`, since a ladder
   * places each level as its own transaction. It plans from the environment on hand when that is
   * recent enough, with the node clock read now (an auction starts from it), within the terms the
   * ticket showed (`shown`: the plan of a single order the button was enabled for). The signature
   * then waits for the re-check against the state at the click (review-check.ts, the freshness
   * rule). The re-check runs alongside the send's own checks and may put its own plan of the order
   * in place. A level that cannot be planned stops the click.
   */
  const prepareLevel = async (intent0: Intent, index: number, levels: number, shown: OrderPlan | null, opts: { fresh?: boolean } = {}): Promise<(SendJob & { plan?: OrderPlan }) | PlanStop> => {
    let intent = intent0;
    const clickAt = Date.now();
    try {
      // The environment. The first level takes the one on hand when its oldest part is at most
      // REVIEW_CACHE_MS old, else a read started at most REVIEW_REUSE_MS before the click. A later
      // level of a ladder reads everything after the click, because the level before has just spent
      // coins. A take (`fresh`) reads the book again right before it is built, because the orders
      // it spends must still be there.
      const snap = index === 0 && !opts.fresh ? snapshot() : null;
      const cached = snap && clickAt - snap.at <= REVIEW_CACHE_MS ? snap : null;
      let base = cached ? cached.env : await (index === 0 ? (opts.fresh ? since(clickAt, ['market']) : since(clickAt - REVIEW_REUSE_MS)) : refresh());
      if (!base) return { text: t('ticket.reviewNoEnv') };
      let fromCache = !!cached;
      // The node clock now. DAA times (an auction's start, a timed start, an IOC life, a day order)
      // count from it, never from the clock of the environment on hand. That is the background
      // clock projected to the click when it is fresh, else a read. A node slower than
      // CLOCK_WAIT_MS gives the clock on hand projected to now (review-check.ts `projectClock`).
      let clock = liveClockAt(liveClock.current, clickAt)
        ?? (await reviewClock(() => readClock(services), base.clock, cached ? cached.clockAt : (snapshot()?.clockAt ?? clickAt), { waitMs: CLOCK_WAIT_MS })).clock;
      let refsNow = touches.data ?? noRefs();
      const planWith = (e: PlanEnv, refs: References, i: Intent = intent): OrderPlan => planOrder(withRefs({ ...e, clock }, refs), i);
      let p = planWith(base, refsNow);
      if ((!p.ok || !p.built || errors(p).length > 0) && fromCache) {
        // The parts on hand cannot plan it (the holdings of a sell still being read, or the book
        // moved), so plan from the state at the click instead.
        const fresh = await since(clickAt - REVIEW_REUSE_MS);
        if (!fresh) return { text: t('ticket.reviewNoEnv') };
        base = fresh;
        fromCache = false;
        refsNow = verifiers.length ? await touchesOf().catch(noRefs) : (touches.data ?? noRefs());
        p = planWith(base, refsNow);
      }
      const firstError = errors(p)[0];
      if (!p.ok || !p.built || firstError) return { text: firstError ? issueText(firstError, issueCtx) : t('ticket.reviewChanged') };
      const env0 = base;
      // An auction signed in a wallet whose last auction signature took longer than its window
      // (confirm/signing-time.ts) is planned with the clock later by the shortfall. That is the
      // move a re-timing makes, which only makes its times later. A plan that cannot be made so
      // keeps the clock of the click.
      let shift = 0n;
      const extra = wallet.adapter ? firstAttemptLead(rememberedSigningMs(wallet.adapter.id), clock.rateMilli) : 0n;
      if (extra > 0n && auctionSubmitBy(p.states, clock.daa) !== null) {
        const later = laterClock(clock, extra);
        const q = planOrder(withRefs({ ...env0, clock: later }, refsNow), intent);
        if (q.ok && q.built && errors(q).length === 0) {
          clock = later;
          p = q;
          shift = extra;
        }
      }
      const kept = keepTerms(p, intent, (i) => planWith(env0, refsNow, i), shown);
      if (!('p' in kept)) return kept;
      p = kept.p;
      intent = kept.intent;
      const planned = p;
      // The re-check reads again only the parts that may no longer be the state at the click
      // (review-check.ts rule 2). It reads none when the environment was read after the click. The
      // references of further indexers are always read again.
      const toRead = fromCache ? stale(clickAt) : [];
      const settled = toRead.length === 0;
      const label = intent0.type === 'curve' ? t('ticket.ladder.curveLabel', { name }) : orderLabel(form.type, form.side, levels > 1 ? { level: index + 1, levels } : undefined, name);
      const openTokens = (quote ? [token, quote] : [token]).filter((x) => x.openList);
      // The lines the final confirmation adds (an order below the minimum value, a price far
      // through the market, contended blocks). They are worked out here, once, from this plan.
      const notes = confirmNotes(planned, {
        plain: !quote && levels === 1, min: minOrderValueOf(indexerStatus.health), contended, issue: (i) => issueText(i, issueCtx),
        low: (n, source) => t(`ticket.minValue.${source}`, { value: formatKas(n.value, { group: ',' }), min: formatKas(n.min, { group: ',' }) }),
        busy: () => t('ticket.congestion'),
      });
      // The same notes as codes and numbers for the private-key signer's confirmation, which words
      // them itself.
      const signerNotes = confirmSignerNotes(planned, { plain: !quote && levels === 1, min: minOrderValueOf(indexerStatus.health), contended });
      // A take was planned on a book read just now and has no auction, so it has no re-check, no
      // re-timing and no following of the market. A lost race is the node's answer, and the take is
      // planned again (placeTake).
      if (planned.take) return { built: planned.built!, expected: expectedFromPlan(planned), label, openTokens, notes, signerNotes, submitByDaa: null, plan: planned };
      return {
        plan: planned,
        built: planned.built!, expected: expectedFromPlan(planned), label, openTokens, notes, signerNotes,
        gate: !settled || verifiers.length > 0 ? recheck(planned, intent, clock, base.fees, settled ? base : null, clickAt, toRead, shown, label, openTokens, shift) : undefined,
        submitByDaa: auctionSubmitBy(planned.states, clock.daa),
        retime: retimer(env0, refsNow, intent, null, label, openTokens, shift + RETIME_LEAD_DAA),
        // The open final confirmation follows the market and the clock: the same order planned
        // again, as at a click now.
        replan: async () => {
          const again = await prepareLevel(intent0, index, levels, shown);
          return 'built' in again ? again : { stop: { code: again.code ?? 'plan', text: again.text } };
        },
      };
    } catch (e) {
      const raw = e instanceof Error ? e.message : String(e);
      return { text: buildErrorText(raw), raw };
    }
  };

  /**
   * The re-check of a planned order (review-check.ts rule 2). It plans the same intent against the
   * environment at the click and the references read now, with the clock and the fees of
   * `planned`. The environment is `env` when the one on hand already is the one at the click, else
   * `parts` read again from REVIEW_REUSE_MS before the click on. The same transaction is kept.
   * Another one without an error and within the terms the ticket showed (`shown`) replaces the plan
   * (a moved book, coins replaced after a submission). An error or a broken term stops the order.
   * `shift` is how much later than the click `clock` is (a slow wallet's auction, prepareLevel),
   * where its re-timings start from.
   */
  const recheck = async (
    planned: OrderPlan, intent: Intent, clock: Clock, fees: PlanEnv['fees'], env: PlanEnv | null, clickAt: number, parts: EnvWait[], shown: OrderPlan | null,
    label: string, openTokens: SendJob['openTokens'], shift: bigint,
  ): Promise<GateVerdict> => {
    const stop = (text: string, code = 'changed'): GateVerdict => ({ stop: { code, text } });
    try {
      const [fresh, refs] = await Promise.all([env ? Promise.resolve(env) : since(clickAt - REVIEW_REUSE_MS, parts), touchesOf().catch(noRefs)]);
      if (!fresh) return stop(t('ticket.reviewNoEnv'));
      const plan = (i: Intent): OrderPlan => planOrder(withRefs({ ...fresh, clock, fees }, refs), i);
      const p = plan(intent);
      const firstError = errors(p)[0];
      if (!p.ok || !p.built || firstError) return stop(firstError ? issueText(firstError, issueCtx) : t('ticket.reviewChanged'));
      if (planned.built && sameTransaction(planned.built, p.built)) return null;
      const kept = keepTerms(p, intent, plan, shown);
      if (!('p' in kept)) return stop(kept.text, kept.code);
      return { replace: { built: kept.p.built!, expected: expectedFromPlan(kept.p), submitByDaa: auctionSubmitBy(kept.p.states, clock.daa), retime: retimer(fresh, refs, kept.intent, fees, label, openTokens, shift + RETIME_LEAD_DAA) } };
    } catch (e) {
      return stop(buildErrorText(e instanceof Error ? e.message : String(e)));
    }
  };

  /**
   * The re-timing of a planned order (`SendJob.retime`: a long read of the final confirmation, or
   * an auction start that passed while the wallet signed). It plans the same intent on the same
   * environment, references and fees, so with the same terms, inputs and fee. For its times it
   * uses the node clock now plus `lead`. `lead` grows by RETIME_LEAD_DAA at each re-timing, and is
   * at least the signing time the failed attempt measured plus RETIME_LEAD_DAA
   * (confirm/signing-time.ts `retimeLead`). Only its times move, later, and the signer and the app
   * accept it without asking again (signer/retime.ts). The first `lead` is the shift the plan of
   * the click had (`shift`, a slow wallet's) plus RETIME_LEAD_DAA.
   */
  const retimer = (env: PlanEnv, refs: References, intent: Intent, fees: PlanEnv['fees'] | null, label: string, openTokens: SendJob['openTokens'], lead = RETIME_LEAD_DAA): SendJob['retime'] =>
    async (o) => {
      try {
        const now = liveClockAt(liveClock.current, Date.now()) ?? (await readClock(services));
        const used = retimeLead(lead, o?.signingMs, now.rateMilli);
        const clock = laterClock(now, used);
        const p = planOrder(withRefs({ ...env, clock, ...(fees ? { fees } : {}) }, refs), intent);
        const firstError = errors(p)[0];
        if (!p.ok || !p.built || firstError) return { stop: { code: 'changed', text: firstError ? issueText(firstError, issueCtx) : t('ticket.reviewChanged') } };
        return { built: p.built, expected: expectedFromPlan(p), label, openTokens, submitByDaa: auctionSubmitBy(p.states, clock.daa), retime: retimer(env, refs, intent, fees, label, openTokens, used + RETIME_LEAD_DAA) };
      } catch (e) {
        return { stop: { code: 'plan', text: buildErrorText(e instanceof Error ? e.message : String(e)) } };
      }
    };

  /**
   * One level: planned, checked and sent. An auction whose start passed while the wallet signed is
   * re-timed by the send (SendStatus.tsx).
   */
  const placeLevel = async (intent: Intent, index: number, levels: number, shown: OrderPlan | null): Promise<SendOutcome | null> => {
    const job = await prepareLevel(intent, index, levels, shown);
    if (!('built' in job)) {
      sender.stop(job.text, job.raw, job.code);
      return null;
    }
    return sender.send(job);
  };

      // After the merges, the sale is placed as by its button once the ticket has planned it on an
      // environment that holds the merged UTXOs, and its usual final confirmation follows. A plan
      // that cannot be placed then shows its reason instead, and so does one that never sees them.
  const sellReady = !!sellAfterMerge && !!env && current && env.tokenUtxos.some((u) => sellAfterMerge.leaves.has(`${u.transactionId}:${u.index}`));
  useEffect(() => {
    if (!sellAfterMerge) return;
    if (sellReady) {
      setSellAfterMerge(null);
      if (canPlace) void place();
      else sender.stop(t('ticket.merge.saleNotPlaced'), undefined, 'merge');
      return;
    }
    // The merged coins never showed up. The sale is not placed, and the ticket says so.
    const h = setTimeout(() => {
      setSellAfterMerge(null);
      sender.stop(t('ticket.merge.notVisible'), undefined, 'merge');
    }, Math.max(0, SELL_AFTER_MERGE_MS - (Date.now() - sellAfterMerge.at)));
    return () => clearTimeout(h);
  }, [sellAfterMerge, sellReady]);

  /**
   * A crossing order filled from the book (kob/take.ts): its takes, each its own transaction, and
   * then the rest (take-run.ts `runTakes`). The stepper says where it is.
   */
  const placeTake = (intent0: Intent, shown: OrderPlan) =>
    runTakes(intent0, shown, {
      prepare: (intent, index, terms, fresh) => prepareLevel(intent, index, 2, terms, fresh ? { fresh: true } : {}),
      send: (job) => sender.send(job),
      stop: (s) => sender.stop(s.text, s.raw, s.code),
      show: (items) => setTakeRun(items),
      words: {
        step: (k, n) => (n === 1 ? t('ticket.take.stepOne') : t('ticket.take.step', { n: k + 1, of: n })),
        rest: t('ticket.take.stepRest'),
        retry: t('ticket.take.retry'),
        nothingNow: t('ticket.take.nothingNow'),
        stopTerms: (filled) => t('ticket.take.stopTerms', { amount: formatUnits(filled, token.decimals, { group: ',' }), ticker, price: formatKas(shown.take!.limit, { group: ',' }) }),
        stopGone: (filled) => t('ticket.take.stopGone', { amount: formatUnits(filled, token.decimals, { group: ',' }), ticker }),
      },
    });

  /**
   * The order button: every level of the order in turn, each planned at its turn, checked and
   * approved in the wallet. A level that stops ends it.
   */
  const place = async () => {
    if (!canPlace || !built.intent) return;
    // A crossing order fills from the book: its own flow of takes and the rest.
    if (!ladder && shownPlan?.take) {
      setPlacing(true);
      setMaxNote(null);
      setLadderNote(null);
      setTakeRun(null);
      setTakeAtClick(shownPlan.take);
      sender.clear();
      try {
        const r = await placeTake(planIntent ?? built.intent, shownPlan);
        if (r.done) setForm((f) => setValue(f, 'amount', ''));
      } finally {
        setPlacing(false);
      }
      void refresh().catch(() => null);
      return;
    }
    // Top-up: every order of the click uses one receive threshold, the stored one or the one the
    // shown plan set (the wallet's first top-up order of the token). It is stored once an order
    // with it is submitted.
    const firstTopup = topupThreshold === null ? null : storedThreshold === undefined ? topupThreshold : null;
    const intents = (asCurve && curveIntent ? [curveIntent as Intent] : ladder?.ok ? ladder.levels.map((l) => l.intent) : [built.intent]).map((i) => (firstTopup !== null ? withThreshold(i, firstTopup) : i));
    const levels = intents.length;
    // The terms the user saw: the plan the button was enabled for. A ladder's levels are planned
    // each at its turn, each within the planner's rules.
    const shown = levels === 1 ? shownPlan : null;
    setPlacing(true);
    setMaxNote(null);
    setLadderNote(null);
    setLadderAt(null);
    // The line of an earlier click goes. The button shows the click is running until the first
    // check reports.
    sender.clear();
    let placed = 0;
    try {
      for (let i = 0; i < levels; i++) {
        setLadderLead(levels > 1 ? t('ticket.ladder.reviewing', { level: i + 1, levels }) : null);
        if (levels > 1) setLadderAt({ at: i, levels });
        const out = await placeLevel(intents[i]!, i, levels, shown);
        if (out?.status !== 'submitted') {
          if (levels > 1) setLadderAt({ at: i, levels, failed: true });
          break;
        }
        placed += 1;
        if (firstTopup !== null && pubkey && ext && rememberThreshold({ maker: pubkey, tokenCovId: token.covenantId, extensionCommitment: ext, threshold: firstTopup, ticker, decimals: token.decimals })) {
          setThresholdTick((n) => n + 1);
        }
      }
    } finally {
      setPlacing(false);
    }
    if (placed === levels) {
      setLadderLead(null);
      setLadderAt(levels > 1 ? { at: levels, levels } : null);
      if (asCurve) setLadderNote({ done: true, text: t('ticket.ladder.curveDone') });
      if (levels > 1) setLadderNote({ done: true, text: t('ticket.ladder.done', { levels }) });
      // The order is placed, so the next one starts empty. The price, type and side stay, because
      // the next click is usually a variation.
      setForm((f) => setValue(f, 'amount', ''));
    } else if (placed > 0) {
      setLadderNote({ done: false, text: t('ticket.ladder.stopped', { placed, levels }) });
    }
    // A failed read is already the environment's error, which the ticket shows. It is caught, so it
    // is not also an unhandled rejection.
    void refresh().catch(() => null);
  };

  const issueCtx = { tokenDecimals: token.decimals, tokenTicker: ticker, ...(quote ? { quoteDecimals: quote.decimals, quoteTicker: quoteTicker } : {}) };
  // ---- The reasons the order cannot be placed, and nothing else: one fixed-height slot above the
  // button. Warnings that never stop the order (a price far through the market, a low order value,
  // the fills of a market order) are not shown on the ticket at all. So nothing appears, changes or
  // disappears there on a tick and the button never moves. The final confirmation shows the
  // order's terms.
  const reasons: Reason[] = [];
  if (!wallet.info) reasons.push({ code: 'wallet', text: t('ticket.need.wallet'), kind: 'need' });
  else if (wallet.networkMismatch) reasons.push({ code: 'network', text: t('ticket.need.network', { wallet: wallet.info.network, app: services.config.network }), kind: 'need' });
  if (enabled && envError) reasons.push({ code: 'env', text: t(`ticket.need.env.${envSource ?? 'other'}`), kind: 'need', action: 'retry', raw: envError });
  if (crash) reasons.push({ code: 'crash', text: t('ticket.need.crash'), kind: 'need', raw: crash });
  // What the ticket is still reading, named. The order cannot be placed before, and it is the
  // moment the answers are in.
  const holdingsShort = waiting.includes('holdings') && planErrors.some((i) => TOKEN_SHORT.has(i.code));
  const stillReading: EnvWait[] = enabled && !envError ? (rawEnv ? (holdingsShort ? ['holdings'] : []) : waiting) : [];
  if (stillReading.length) reasons.push({ code: 'loading', text: t('ticket.need.loading', { what: stillReading.map((w) => t(`ticket.wait.${w}`)).join(', ') }), kind: 'need' });
  if (enabled && !gate.ready) reasons.push({ code: 'covenant', text: t('ticket.need.covenant'), kind: 'need' });
  if (enabled && maxNote) reasons.push({ code: 'max', text: maxNote, kind: 'need' });
  const issues = (shownPlan?.issues ?? []).filter((i) => !(holdingsShort && TOKEN_SHORT.has(i.code)));
  // What is wrong in the form: a typed value that does not parse first, then what is still empty.
  // The typed errors are also under their boxes.
  const typedErrors = visibleErrors(built.errors).filter((e) => (form.values[e.field] ?? '') !== '');
  for (const e of typedErrors) reasons.push({ code: `field-${e.field}`, text: `${t(labelKey(form.type, e.field))}: ${t(`ticket.err.${e.code}`, (e.params ?? {}) as Record<string, string | number>)}`, kind: 'field' });
  // A required field that is simply empty (untouched, or just reset after an order) is not an
  // error, so the slot shows no text. The instruction is on the disabled button (`enterText`
  // below). The slot's red text is for blocking errors only.
  for (const i of issues.filter((x) => x.severity === 'error')) {
    // Too many UTXOs for one sale. When the order button merges first, a note says what that takes
    // instead of the dead end.
    if (MERGE_ANSWERS.has(i.code) && mergeFirst) {
      reasons.push({ code: i.code, text: merge.offer!, kind: 'need', testid: 'order-note-merge' });
      continue;
    }
    const text = i.code === 'GUARDS_UNAVAILABLE' && switching ? t('ticket.switching') : issueText(i, issueCtx);
    reasons.push({ code: i.code, text, kind: 'issue', ...(rawOf(i) ? { raw: rawOf(i)! } : {}) });
  }
  if (ladder && !ladder.ok) reasons.push({ code: 'ladder', text: ladder.errors.map((e) => t(`ticket.ladder.err.${e.code}`, { level: e.level ?? 1, field: e.field ?? '', max: 20 })).join(' '), kind: 'ladder', testid: 'order-ladder-errors' });
  for (const f of ladderFunds) reasons.push({ code: f.code, text: f.text, kind: 'ladder', testid: 'order-ladder-funds' });
  if (lastOrderErrors.length > 0) reasons.push({ code: 'ladder-last', text: lastOrderErrors.map((i) => t('ticket.ladder.lastLevel', { text: issueText(i, issueCtx) })).join(' '), kind: 'ladder', testid: 'order-ladder-last-errors' });
  // The button is off for a reason nothing above names (the plan is still being made, a click is
  // running). That is said too, so a disabled button always has its sentence.
  const busy = placing || sendBusy(sender.status) || merge.running || !!sellAfterMerge;
  // What the click is doing, as the label of the button itself (the eye is on it): checking, the
  // app's confirmation, the wallet's approval, sending. A ladder says its level first. The button
  // keeps its size and only its text changes.
  const phase = sender.status?.phase;
  const busyText = busy && !placing && !sendBusy(sender.status)
    ? merge.busy
    : busy
    ? (ladderLead ? `${ladderLead} · ` : '')
      + t(sender.status?.waiting ? 'send.waitWallet' : phase === 'signing' ? 'send.signing' : phase === 'sending' ? 'send.sending' : phase === 'confirming' ? 'send.confirming' : 'send.checking', { wallet: wallet.info?.label || t('send.yourWallet') })
    : null;
  // Nothing but empty required fields is in the way. The disabled button itself says what to enter
  // first ("Enter an amount", the first missing field).
  const firstEmpty = isIncomplete(built.errors) ? built.errors.find((e) => e.code === 'required') : undefined;
  const enterText = !canPlace && !busy && reasons.length === 0 && firstEmpty ? enterTextOf(firstEmpty.field, form.type, inverted) : null;
  if (!canPlace && !busy && reasons.length === 0 && !enterText) reasons.push({ code: 'checking', text: t(pendingShown || pending ? 'ticket.pending' : 'ticket.need.checking'), kind: 'need' });
  // An order worth less than the minimum order value is never listed, so it never fills and locks
  // its funds until cancelled. The order can still be placed, so this is a warning, said here
  // before the click. The final confirmation says it again as a callout. It applies only to a plain
  // order of a KAS market, because the rule is not measured for a ladder, a conditional or a pair
  // order (ticket-notes.ts `minValueNote`).
  const lowValue = shownPlan && shownPlan.ok && planErrors.length === 0 && !quote && !ladder ? minValueNote(shownPlan, minOrderValueOf(indexerStatus.health).sompi) : null;
  if (lowValue) reasons.push({ code: 'min-value', text: t('ticket.warn.belowMin', { min: formatKas(lowValue.min, { group: ',' }) }), kind: 'warn', testid: 'order-warn-min-value' });
  // A re-plan on a tick keeps the sentence of the plan it replaces for the moment it takes, so no
  // sentence flickers in and out of the slot. An order that can be placed shows its warnings only.
  const lastReasons = useRef<Reason[]>([]);
  const backgroundPlan = pending && !editPending;
  const ownReasons = busy ? [] : canPlace ? reasons.filter((r) => r.kind === 'warn') : reasons;
  const shownReasons = busy ? [] : backgroundPlan && lastReasons.current.length > 0 ? lastReasons.current : ownReasons;
  if (!backgroundPlan) lastReasons.current = ownReasons;

  // The balance this order spends, shown at the right end of the amount label row (it is the Max
  // button). A buy pays in the quote (KAS, or a pair's token B), and a sell gives the token. The
  // form's side is the native one, so an inverted view needs no case of its own.
  const spendBalance =
    form.side === 'buy'
      ? quote
        ? { amount: formatUnits(quoteBalance, quote.decimals, { group: ',' }), unit: quoteTicker }
        : { amount: formatKas(kasBalance, { group: ',' }), unit: 'KAS' }
      : { amount: formatUnits(tokenBalance, token.decimals, { group: ',' }), unit: ticker };
  const available = enabled && env ? spendBalance : undefined;

  // The height of the whole Limit form, for the market page. It is measured folded, with no ladder
  // and the field help off (the default), with panel and footer included. On two and three columns
  // its row is at least that tall (a phone: the panel is that tall, market.css --tk-fit), so the
  // form does not scroll. It is measured once per width (again once the fonts and the data are
  // first in), never on what comes and goes in the form later (another type, a ladder, an expiry,
  // a reason, the help), because that never moves the page.
  // The field help under the inputs (the title row's "?" shows or hides it, kept in this browser)
  // is off at first on every width. So the form is its labels and boxes and fits the panel without
  // scrolling (the --tk-fit measure below is taken on this default). The long text of a field is
  // its tooltip meanwhile.
  const [helpOn, setHelpOn] = useState(() => {
    try {
      const kept = localStorage.getItem(HELP_KEY);
      if (kept === '1' || kept === '0') return kept === '1';
    } catch {
      /* nothing kept */
    }
    return false;
  });
  const toggleHelp = () => {
    const on = !helpOn;
    setHelpOn(on);
    try {
      localStorage.setItem(HELP_KEY, on ? '1' : '0');
    } catch {
      /* not kept */
    }
  };

  const tkRef = useRef<HTMLElement>(null);
  const fit = useRef({ w: -1, fonts: false, data: false });
  const fitShape = form.type === 'limit' && !ladder;
  const dataIn = useRef(false);
  if (env && !envLoading) dataIn.current = true;
  const dataWasIn = dataIn.current;
  useEffect(() => {
    const tk = tkRef.current;
    const grid = tk?.closest<HTMLElement>('.mkt-grid');
    if (!tk || !grid || !fitShape || typeof ResizeObserver === 'undefined') return;
    const measure = () => {
      const scroll = tk.querySelector<HTMLElement>('.tk-scroll');
      const formEl = tk.querySelector<HTMLElement>('.tk-form');
      const side = tk.closest<HTMLElement>('.mkt-side');
      const holder = side && getComputedStyle(side).display !== 'contents' ? side : tk.closest<HTMLElement>('.mkt-area-ticket');
      if (!scroll || !formEl || !holder || tk.querySelector<HTMLDetailsElement>('.tk-advanced')?.open) return;
      const w = tk.clientWidth;
      if (w === fit.current.w) return;
      // Always the form with the field help off, the default. A reader who turned the help on gets
      // a form that scrolls in the same panel, so the panel (and the row of the page) is the same
      // whether the help is on or not. The class is put back before anything is painted.
      const helpWasOn = !tk.classList.contains('tk-help-off');
      if (helpWasOn) tk.classList.add('tk-help-off');
      const top = scroll.getBoundingClientRect().top - scroll.scrollTop;
      const content = formEl.getBoundingClientRect().bottom - top + parseFloat(getComputedStyle(scroll).paddingBottom || '0');
      const chrome = holder.offsetHeight - scroll.clientHeight;
      if (helpWasOn) tk.classList.remove('tk-help-off');
      fit.current.w = w;
      grid.style.setProperty('--tk-fit', `${Math.ceil(chrome + content)}px`);
    };
    if (dataWasIn && !fit.current.data) {
      fit.current.data = true;
      fit.current.w = -1;
    }
    measure();
    if (!fit.current.fonts) {
      void document.fonts?.ready.then(() => {
        if (fit.current.fonts) return;
        fit.current.fonts = true;
        fit.current.w = -1;
        measure();
      });
    }
    const ro = new ResizeObserver(measure);
    ro.observe(tk);
    return () => ro.disconnect();
  }, [fitShape, dataWasIn]);

  if (!tradable) {
    const reason = token.untradableReason;
    return (
      <section class="tk tk-plain" data-testid="order-ticket" data-state="untradable">
        <h2>{t('ticket.title')}</h2>
        <Banner tone="warn" title={t('ticket.untradable.title', { ticker })} data-testid="order-untradable">
          <p>{reason ? keyedText('untradable', reason) : t('ticket.untradable.generic')}</p>
          <p class="muted">{t('ticket.untradable.hint')}</p>
        </Banner>
      </section>
    );
  }

  return (
    <section ref={tkRef} class={`tk${helpOn ? '' : ' tk-help-off'}`} data-testid="order-ticket" data-type={form.type} data-side={form.side} data-shown-side={shownSide} data-inverted={inverted ? '1' : '0'} aria-busy={pending || envLoading ? 'true' : 'false'}>
      {/* the panel has the height of its row of the page (market.css) and never changes it: the form scrolls inside .tk-scroll, the reason slot and the button are the
          footer, pinned to the bottom of the panel, so nothing that comes or goes in the form moves the button or anything beside or under the ticket */}
      <div class="tk-scroll" data-testid="order-scroll">
      {/* no visible title row (founder, 2026-11-11: the rows are for the form): the panel's name is a heading for screen readers, the field help's "?" is at the right end of the Buy | Sell row,
          and the "Calculating..." indicator is an overlay at the right end of the Order type label row, so neither adds a row. The indicator shows only when a plan takes longer than
          PENDING_SHOW_MS (a background re-plan on a tick is over before that and shows nothing) */}
      <h2 class="sr-only">{t('ticket.title')}</h2>

      <div class="tk-form">
        <div class="tk-siderow">
          <Segmented<Side>
            block
            aria-label={t('ticket.side')}
            value={shownSide}
            onChange={(s) => onSide(inverted ? oppositeSide(s) : s)}
            options={(['buy', 'sell'] as Side[]).map((s) => ({
              value: s,
              label: t(s === 'buy' ? 'common.buy' : 'common.sell'),
              tone: s,
              disabled: !sidesOf(form.type).includes(inverted ? oppositeSide(s) : s),
              'data-testid': s === 'buy' ? 'order-side-buy' : 'order-side-sell',
            }))}
          />
          <button
            type="button"
            class="tk-help-toggle"
            aria-pressed={helpOn ? 'true' : 'false'}
            title={t(helpOn ? 'ticket.helpToggle.hide' : 'ticket.helpToggle.show')}
            aria-label={t('ticket.helpToggle.label')}
            data-testid="order-help-toggle"
            onClick={toggleHelp}
          >
            ?
          </button>
        </div>

        <div class="field tk-typefield" title={typeHelp}>
          <label id="order-type-label" for="order-type">{t('ticket.type.label')}</label>
          {pendingShown ? (
            <span class="tk-pending" data-testid="order-pending">
              <Loading text={t('ticket.pending')} />
            </span>
          ) : null}
          <Dropdown
            id="order-type"
            labelledBy="order-type-label"
            value={form.type}
            options={TYPE_GROUPS.map((g) => ({ group: t(`ticket.group.${g.group}`), options: g.types.map((id) => ({ value: id, label: t(typeKey(id, 'name'), { name }) })) }))}
            onChange={(v) => onType(v as OrderTypeId)}
            data-testid="order-type"
          />
          <div class="field-hint tk-help" title={typeHelp} data-testid="order-type-help">
            {typeHelp}
          </div>
        </div>

        <TicketFields
          form={form} ctx={ctx} ticker={ticker} ids={asCurve ? layout.main.filter((id) => id !== 'lifetime' && id !== 'lifetimeAt') : layout.main} errors={errs} onChange={onChange} {...(form.type === 'market' ? { marketPrice: true } : {})} onMax={enabled && env ? onMax : undefined} {...(available ? { available } : {})} {...(available && form.side === 'sell' && unusable ? { unusable: { text: t('ticket.unusable', { amount: formatUnits(unusable.amount, token.decimals, { group: ',' }), ticker }), reason: t(`ticket.unusable.${unusable.reason}`) } } : {})} slider={slider}
          {...(inverted ? { flip } : {})} {...(quote ? { quoteTicker, lowTips } : {})}
        />

        <details class="tk-advanced" data-testid="order-advanced">
          <summary>{t('ticket.advanced')}</summary>
          <div class="tk-advanced-body">
            <TicketFields
              form={form} ctx={ctx} ticker={ticker} ids={layout.advanced} errors={errs} onChange={onChange} {...(inverted ? { flip } : {})} {...(quote ? { quoteTicker, lowTips } : {})}
              defaults={{ delivery: delivery ?? walletDelivery }} options={{ delivery: deliveryChoicesOf(ctx) }} placeholders={defaultTip !== null ? { topupTip: formatKas(defaultTip) } : {}} subs={deliverySubs} {...(tipAutoNote ? { tipAutoNote } : {})}
              asides={{ delivery: <button type="button" class="tk-whats" onClick={() => setDeliveryHelp(true)} data-testid="order-delivery-help">{t('ticket.dhelp.link')}</button> }}
            />
          </div>
        </details>
      </div>

      {ladder ? (
        <div class="tk-ladder" data-testid="order-ladder">
          <h3>{t('ticket.ladder.title', { levels: ladder.levels.length })}</h3>
          {ladder.ok ? (
            <>
              <ol data-testid="order-ladder-levels">
                {ladder.levels.map((l) => (
                  <li key={l.level} data-testid={`order-ladder-level-${l.level}`}>
                    {ladderRow(l)}
                  </li>
                ))}
              </ol>
              {/* the total is a line of its own height whether or not a plan is on hand: a re-plan never moves the lines below */}
              <p class="tk-ladder-total" data-testid="order-ladder-total">
                {limitSet
                  ? t(asCurve ? 'ticket.ladder.totalCurve' : 'ticket.ladder.totalLimit', {
                      amount: `${formatUnits(ladder.totalAmount, token.decimals, { group: ',' })} ${ticker}`, levels: ladder.levels.length,
                      value: quoteText(limitLadderValues(ladder, form.side, scale).total),
                    })
                  : ladderTot
                    ? t('ticket.ladder.total', { amount: `${formatUnits(ladderTot.amount, token.decimals, { group: ',' })} ${ticker}`, levels: ladder.levels.length, kas: `${formatKas(ladderTot.kasLocked, { group: ',' })} KAS` })
                    : ' '}
              </p>
              {curveIntent ? (
                <div class="tk-place-as" data-testid="order-ladder-place-as">
                  <span class="small muted">{t('ticket.ladder.placeAs')}</span>
                  <Segmented<'curve' | 'separate'>
                    block
                    aria-label={t('ticket.ladder.placeAs')}
                    value={asCurve ? 'curve' : 'separate'}
                    onChange={(v) => { setPlaceAs(v); edited(); }}
                    options={[
                      { value: 'curve', label: t('ticket.ladder.asCurve'), 'data-testid': 'order-ladder-as-curve' },
                      { value: 'separate', label: t('ticket.ladder.asSeparate'), 'data-testid': 'order-ladder-as-separate' },
                    ]}
                  />
                </div>
              ) : null}
              {/* why a curve is cheaper is field help (the title row's "?"); that separate orders ask for one approval each is not */}
              <p class={asCurve ? 'muted tk-help' : 'muted'} data-testid="order-ladder-how">{asCurve ? t('ticket.ladder.curveNote') : t('ticket.ladder.separate', { levels: ladder.levels.length })}</p>
              {delivery === 'collected' ? <p class="muted tk-help" data-testid="order-ladder-custodies">{t('ticket.ladder.custodies', { levels: ladder.levels.length })}</p> : null}
            </>
          ) : null}
        </div>
      ) : null}

      {takeLines || (takeRun && takeRun.length > 0) ? (
        <div class="tk-take" data-testid="order-take">
          {/* two short lines, one row each (the final confirmation has the details: the orders, the fee exactly); a finished click keeps its steps only */}
          {takeLines ? (
            <p class="tk-take-fills" data-testid="order-take-fills" title={takeLines.fills + takeLines.fee}>
              <span class="tk-take-main">{takeLines.fills}</span>
              <span class="tk-take-fee">{takeLines.fee}</span>
            </p>
          ) : null}
          {takeLines?.rest ? <p class="muted" data-testid="order-take-rest" title={takeLines.rest}>{takeLines.rest}</p> : null}
          {takeRun && takeRun.length > 0 ? <Stepper steps={takeRun} label={t('ticket.take.steps')} data-testid="order-take-stepper" /> : null}
        </div>
      ) : null}
      {enabled ? gate.element : null}
      {/* what the token's records say (facts, the genesis check) and the note of a ladder: the end of the scrolling form, so a late answer never moves the button */}
      {props.facts}
      {/* a ladder of separate orders: one step per level while it is placed, and after (the level list goes with the reset amount) */}
      {ladderAt ? <Stepper data-testid="order-ladder-stepper" label={t('stepper.label')} steps={ladderStepItems(ladderAt, placing)} /> : null}
      {ladderNote ? (
        <p class="muted" data-testid={ladderNote.done ? 'order-result-ladder' : 'order-ladder-stopped'}>
          <span key={ladderNote.text}>{ladderNote.text}</span>
        </p>
      ) : null}
      </div>
      <div class="tk-foot">
      {/* the one place that says why the order cannot be placed (and what to know before it is: a warning), above the total and the button: no room when there is
          nothing to say, two lines at most (more scroll in it); the footer grows upwards from the pinned button, and the total is under the slot, so a sentence coming and
          going moves neither of them */}
      {/* what a click is doing, in the footer itself (never over the form); why it stopped is the red toast, a submitted order a notification (below) */}
      <div class="tk-status">
        <SendStatusInline sender={sender} lead={ladderLead} data-testid="order-status" />
      </div>
      <div class="tk-reason" data-testid="order-issues" data-count={shownReasons.length} aria-live="polite">
        {shownReasons.length > 0 ? (
          <ul>
            {shownReasons.map((r, n) => (
              <li
                key={`${r.code}-${n}`}
                class={`tk-reason-item tk-reason-${r.kind}`}
                data-code={r.code}
                data-severity={r.kind === 'issue' ? 'error' : undefined}
                data-testid={r.testid ?? (r.kind === 'issue' ? `order-issue-${r.code}` : r.kind === 'need' ? `order-need-${r.code}` : `order-reason-${r.code}`)}
              >
                <span key={r.text}>{r.text}</span>
                {r.action === 'retry' ? <Button small onClick={() => void refresh().catch(() => null)} data-testid="order-retry">{t('common.retry')}</Button> : null}
                <RawDetails text={r.raw} />
              </li>
            ))}
          </ul>
        ) : null}
      </div>
      {/* what the amount costs (a buy) or yields (a sell), the figure a trader checks before the click, labelled and directly over the button, under the reason slot: one line of a fixed height,
          "—" until there is an amount; the dollars of the estimate (USD display) stand in a column of their own at its left */}
      <div class="tk-total" data-testid="order-total">
        <span class="tk-total-label" {...(curveCost !== undefined ? { title: t('ticket.toPlaceHint') } : {})}>{t(curveCost !== undefined ? 'ticket.toPlace' : 'ticket.total')}</span>
        <div class="tk-cost" {...(footText.unit ? {} : { 'data-empty': '1' })} aria-label={t(curveCost !== undefined ? 'ticket.toPlaceHint' : 'ticket.cost.aria')} data-testid="order-cost-preview" title={footText.text === t('ticket.cost.dash') ? undefined : `${footText.text} ${footText.unit}`.trim()}>
          {footText.usd ? <span class="tk-line-usd" title={footText.usd.title || undefined} data-testid="order-cost-usd">{footText.usd.text}</span> : null}
          <span class="tk-line-text" key={footText.text}>{footText.text}</span>{footText.unit ? ' ' : null}
          <span class="tk-line-unit">{footText.unit}</span>
        </div>
      </div>
      {/* the submitted order: a card of the notifications (its id, Copy, then whether a block took it), not a line of the footer; its progress is the
          label of the button and the line above */}
      <SendStatusToast sender={sender} lead={ladderLead} only="submitted" data-testid="order-status" />
      {deliveryHelp && delivery ? <DeliveryHelp selected={delivery} onClose={() => setDeliveryHelp(false)} /> : null}
      {merge.card}
      <Button
        variant="primary" block class="tk-place" data-side={form.side} onClick={() => (mergeFirst ? merge.start() : void place())} disabled={!(canPlace || mergeFirst) || !!sellAfterMerge}
        loading={placing || merge.running || !!sellAfterMerge} data-testid="order-place" data-state={placing ? 'placing' : mergeFirst ? 'merge' : undefined} {...(mergeFirst ? { title: merge.hint } : {})}
      >
        <span key={busyText ?? enterText ?? (mergeFirst ? 'merge' : `${sideName}|${typeName}`)} data-state={busyText ? 'busy' : enterText ? 'enter' : undefined}>
          {busyText ?? enterText ?? (mergeFirst ? merge.action : t('ticket.place', { side: sideName, type: typeName }))}
        </span>
      </Button>
      </div>
    </section>
  );
}
