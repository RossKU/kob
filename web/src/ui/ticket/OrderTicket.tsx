// The order-entry form for one token: EVERY order type of docs/spec/order-types.md.
//
// A thin renderer: the form keeps text (`form-state.ts` turns it into an Intent), the planner (`kob/plan.ts`) validates it against the protocol and
// wallet rules and builds the transaction, `disclosure-model.ts` explains what the order costs and does. The order button is enabled only for a
// built, error-free plan; a click plans the order again from the state at the click, checks the built transaction silently (`ui/confirm/send.ts`:
// decoded and compared with the plan) and asks the wallet for its approval at once. Progress and the result are one line under the button.
import { useEffect, useMemo, useRef, useState } from 'preact/hooks';
import { useServices, useWallet } from '../../app/context';
import { has, t } from '../../i18n';
import { issueText, keyedText } from '../../i18n/issue-text';
import { buildErrorText, rawOf } from '../../i18n/build-error';
import { planOrder, type Intent } from '../../kob/plan';
import { errors, isPairEnv, type Clock, type OrderPlan } from '../../kob/plan-types';
import type { TokenInfo } from '../../kob/registry';
import { formatKas, formatUnits, quoteOf } from '../../kob/units';
import { DEFAULT_CARRIER } from '../../kob/orders/common';
import { Banner, Button, Loading, RawDetails, Segmented, useAsync } from '../kit';
import { bookFromIndexer, readClock } from '../../app/env';
import { readPairReferences, readReferences, type References } from '../../data/indexer-crosscheck';
import type { PlanEnv } from '../../kob/plan-types';
import { auctionSubmitBy } from '../../wallet/sign';
import { SendStatusLine, sendBusy, useSend, type SendOutcome } from '../confirm/SendStatus';
import type { SendJob } from '../confirm/send';
import { oppositeSide } from '../market/orientation';
import { DisclosurePanel } from './DisclosurePanel';
import { useCovenantSignGate } from './CovenantSignGate';
import { TicketFields } from './TicketFields';
import { buildDisclosureModel } from './disclosure-model';
import {
  BASE_SUFFIX, TYPE_GROUPS, amountBaseOf, amountPriceOf, baseToKas, buildIntent, kasTextOf, followQuote, formatPrice, streamingQuoteText, initialForm, layoutOf, parsePrice, reorientAmounts, setAmountBase,
  setSide, setValue, sidesOf, switchType, visibleErrors,
  type OrderTypeId, type Side, type TicketCtx, type TicketForm,
} from './form-state';
import { expectedFromPlan } from './plan-expected';
import { expandLadder, expandLimitLadder, ladderOfForm, ladderTotals, limitLadderOfForm, limitLadderValues, type LadderLevel } from './ladder';
import { maxBuyAmount } from './max-amount';
import { orderLabel } from './order-label';
import { usePairTicketEnv, useTicketEnv, useTicketPlan, type EnvWait } from './use-ticket';
import { CLOCK_TICK_MS, CLOCK_WAIT_MS, REVIEW_CACHE_MS, REVIEW_REUSE_MS, liveClockAt, recheckVerdict, reviewClock } from './review-check';
import { rememberPairForm, takePairFlip } from './pair-flip';
import './ticket.css';

export interface OrderTicketProps {
  /** the token traded (the BASE token A of a pair) */
  token: TokenInfo;
  /**
   * a token/token pair A/B: the QUOTE token B. The ticket then offers exactly the same order types, prices are typed in B per whole A, amounts in A,
   * tips stay KAS; the planner builds pair orders (KobPair / KobCondPair / KobIfdPair) against the pair environment (both tokens, the pair book).
   * Absent = the token's KAS market.
   */
  quote?: TokenInfo;
  /** optional prefill from the book (click on a level) */
  /** `price`: the state price (sompi per whole token); `amount`: base units */
  prefill?: { side?: 'buy' | 'sell'; price?: bigint; amount?: bigint; type?: OrderTypeId };
  /**
   * the market is shown inverted (KAS/TOKEN): the ticket speaks the shown pair, whose base is KAS. The side buttons name the side of KAS (Buy = buy
   * KAS = sell the token), prices are tokens per KAS and amounts are KAS, converted at the order's price (form-state.ts, KAS amounts). The order
   * built is the native one; a flip of a filled form keeps it exactly (orientation.ts).
   */
  inverted?: boolean;
}

/** A level that could not be planned at the click: the sentence, and the raw builder text behind "Details". */
interface PlanStop {
  text: string;
  raw?: string;
}

/**
 * How often an auction whose start passed while the wallet waited for approval (`clock-moved`, wallet/sign.ts) is planned again with the current
 * clock and the wallet asked again; after that the reason is shown.
 */
const CLOCK_RETRIES = 1;

/** Token-holding shortfalls: while the holdings are still being read they mean "not read yet", not "not enough". */
const TOKEN_SHORT = new Set(['INSUFFICIENT_TOKENS', 'TOKEN_UTXOS_FRAGMENTED']);

const sum = (xs: bigint[]): bigint => xs.reduce((a, b) => a + b, 0n);

export function OrderTicket(props: OrderTicketProps) {
  const { token, quote } = props;
  // a pair is never shown inverted here: the pair page flips by swapping base and quote (another pair, another route)
  const inverted = !quote && !!props.inverted;
  const name = token.ticker;
  /** the ticker the prices are in: KAS, or the pair's quote token B */
  const quoteTicker = quote ? quote.ticker : 'KAS';
  const services = useServices();
  const wallet = useWallet();
  const tradable = token.tradable && token.program !== null && (!quote || (quote.tradable && quote.program !== null));
  // the order scale of this token (10^decimals, at most 10^9; kob-wasm defaultScale): every price is per that many base units
  const scale = 10n ** BigInt(Math.min(token.decimals, 9));
  const walletReady = !!wallet.info && !!wallet.adapter && !wallet.networkMismatch;
  const enabled = tradable && walletReady;

  // a pair page just flipped (A/B -> B/A): the order typed there, as the same order on this pair (pair-flip.ts)
  const [carried] = useState<TicketForm | null>(() => (quote ? takePairFlip(token.covenantId, quote.covenantId, { base: quote.decimals, quote: token.decimals }) : null));
  const [form, setForm] = useState<TicketForm>(() => carried ?? initialForm('limit', props.prefill?.side ?? 'sell', inverted));
  useEffect(() => {
    if (quote) rememberPairForm(token.covenantId, quote.covenantId, form);
  }, [form]);
  // the send flow of the order button: checking, the wallet's approval, sending, the result (one line under the button)
  const sender = useSend();
  // from the click until the last transaction of it (a ladder: every level, each its own transaction signed in turn) is sent or stopped
  const [placing, setPlacing] = useState(false);
  // a ladder: the level being sent ("Level 2 of 3"), and how the whole ladder ended
  const [ladderLead, setLadderLead] = useState<string | null>(null);
  const [ladderNote, setLadderNote] = useState<{ done: boolean; text: string } | null>(null);
  // what Max just did, in a sentence (cleared by the next edit)
  const [maxNote, setMaxNote] = useState<string | null>(null);

  // one of the two environments is live: the token's KAS market, or the pair A/B (both tokens' UTXOs, the pair book, own pair orders)
  const kasEnv = useTicketEnv(token, enabled && !quote);
  const pairEnv = usePairTicketEnv(token, quote ?? token, enabled && !!quote);
  const { env: rawEnv, error: envError, loading: envLoading, refresh, since, snapshot, stale, waiting } = quote ? pairEnv : kasEnv;
  // the node clock, read every CLOCK_TICK_MS while an order can be placed (a visible page): a click plans with it projected to the click, with no
  // round trip before the wallet is asked (review-check.ts rule 1)
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
  // a market / close order whose start (the indexer's best price) is this far beyond another indexer's best price cannot be placed
  const startTolerance = BigInt(services.config.marketStartToleranceBps);
  // the best prices further indexers report for this market (none configured: empty): a KAS market's own books, a pair's from the two KAS books
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
  // the book's best prices: what a KAS amount of a market / close order converts at (an inverted market)
  const bestAsk = env?.book.asks[0]?.price ?? null;
  const bestBid = env?.book.bids[0]?.price ?? null;
  const ctx: TicketCtx = useMemo(
    () => ({
      decimals: token.decimals, scale, tick: token.tick ?? 1n, tzOffsetMin: new Date().getTimezoneOffset(), ...(rateMilli ? { rateMilli } : {}),
      ...(quote ? { quoteDecimals: quote.decimals } : { ref: { buy: bestAsk, sell: bestBid } }),
    }),
    [token.decimals, scale, token.tick, rateMilli, quote?.decimals, bestAsk, bestBid],
  );
  // a streaming order's "price you saw" is the quote its side meets (buy: the best ask, sell: the best bid): filled in and kept on the quote until
  // the user types a price of their own
  const streamQuote = form.type === 'streaming' ? streamingQuoteText(form.side, { ask: bestAsk, bid: bestBid }, ctx) : null;
  const autoQuote = useRef<string | null>(null);
  useEffect(() => {
    const r = followQuote(form, streamQuote, autoQuote.current);
    autoQuote.current = r.auto;
    if (r.form !== form) setForm(r.form);
  }, [form, streamQuote]);
  // a flip of the market recounts the amount boxes in the shown base (KAS or the token) and keeps the order they build exactly
  useEffect(() => {
    setForm((f) => reorientAmounts(f, inverted, ctx));
  }, [inverted]);
  const built = useMemo(() => buildIntent(form, ctx), [form, ctx]);
  const { plan, current, pending, crash } = useTicketPlan(enabled ? env : null, built.intent);

  // prefill from the book: a price implies a limit-style form, a side the matching order type
  const prefillKey = props.prefill ? `${props.prefill.type ?? ''}|${props.prefill.side ?? ''}|${props.prefill.price ?? ''}|${props.prefill.amount ?? ''}` : '';
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

  const ladderSet = useMemo(() => ladderOfForm(form, ctx), [form, ctx]);
  const limitSet = useMemo(() => limitLadderOfForm(form, ctx), [form, ctx]);
  const ladder = useMemo(() => {
    if (!built.intent) return null;
    if (ladderSet) return expandLadder(built.intent, ladderSet.levels, ladderSet.step, ctx.tick);
    if (limitSet) return expandLimitLadder(built.intent, limitSet, ctx.tick);
    return null;
  }, [ladderSet, limitSet, built.intent, ctx.tick]);

  const layout = layoutOf(form, ctx);
  const errs = visibleErrors(built.errors).filter((e) => (form.values[e.field] ?? '') !== '');
  const ticker = token.ticker;

  // balances the wallet can spend (the planner does the exact funding check)
  const tokenBalance = env ? sum(env.tokenUtxos.map((u) => BigInt(u.state.amount))) : 0n;
  const kasBalance = env ? sum(env.funding.map((u) => BigInt(u.amount))) : 0n;
  const pairPart = env && isPairEnv(env) ? env.pair : null;
  /** a pair: the quote token B the wallet can spend (a buy pays in B) */
  const quoteBalance = pairPart ? sum(pairPart.quoteTokenUtxos.map((u) => BigInt(u.state.amount))) : 0n;
  const refPrice = (() => {
    const own = parsePrice(form.values.price ?? form.values.displayedPrice ?? '', form.side, ctx);
    if (own.ok) return own.value.price;
    return env?.book.asks[0]?.price ?? env?.book.bids[0]?.price ?? 0n;
  })();
  // what the inverted fields need to speak KAS: the price of the form (else the book's best), per whole token of `scale` base units
  const ownPrice = parsePrice(form.values.price ?? form.values.displayedPrice ?? '', form.side, ctx).ok;
  const flip = { name, price: refPrice > 0n ? refPrice : null, ownPrice, scale, decimals: token.decimals };
  /**
   * The largest buy the wallet can place at the form's price (or the book's best when it is empty): a binary search on the amount, each step planned
   * by the real planner, so carriers, the network fee and the escrow rounding are exact; a small reserve stays in the wallet. A ladder searches the
   * amount of ONE order such that all its orders together are affordable. Fields the planner cannot plan without (a missing stop ...) fall back to a
   * conservative rule of thumb.
   */
  const maxBuy = (tip: bigint): bigint => {
    if (!env || refPrice <= 0n) return 0n;
    const withAmount = (amount: bigint): TicketForm => {
      const f = setAmountBase(form, 'amount', amount, ctx);
      return (form.values.price ?? '') === '' && layoutOf(f, ctx).main.includes('price') ? setValue(f, 'price', formatPrice(refPrice, ctx)) : f;
    };
    // the prices of a ladder do not depend on the amount: read them off a probe
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
  /** What Max gives for the form as it is: a sell the whole token balance (split over the orders of a ladder); a buy is searched against the planner. */
  const computeMax = (): bigint => {
    if (form.side === 'sell') return tokenBalance / BigInt(limitSet?.levels ?? 1);
    // the form's own tip (read off the order at one base unit: the amount may still be empty)
    const probe = built.intent ?? buildIntent(setAmountBase(form, 'amount', 1n, ctx), ctx).intent;
    const tip = probe && 'tip' in probe && probe.tip ? probe.tip : 0n;
    return maxBuy(tip);
  };
  const onMax = () => {
    const n = computeMax();
    setForm((f) => (n > 0n ? setAmountBase(f, 'amount', n, ctx) : setValue(f, 'amount', '')));
    setMaxNote(form.side === 'buy' ? t(n > 0n ? 'ticket.max.done' : 'ticket.max.none') : null);
  };
  // the amount slider: 0..100 % of Max, recomputed when anything but the amount changes (type, side, price, tip, balances, the book); a buy's
  // search runs the planner, so it waits for typing to pause
  // the Max of the slider with the form it was computed for: a stale one (another side, type or price) is never used
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
    disabled: !enabled || !env,
  };

  const onChange = (id: string, value: string) => {
    edited();
    setForm((f) => setValue(f, id, value));
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
  const planErrors = shownPlan ? errors(shownPlan) : [];
  const model = useMemo(
    () =>
      shownPlan?.disclosure && env
        ? buildDisclosureModel(shownPlan, {
            ticker, decimals: token.decimals, scale: shownPlan.disclosure.scale, clock: env.clock, ...(quote ? { quote: { ticker: quote.ticker, decimals: quote.decimals } } : {}),
          })
        : null,
    [shownPlan, env, ticker, token.decimals, quote?.ticker, quote?.decimals],
  );
  /** an amount of the market's quote (KAS, or a pair's token B) in words */
  const quoteText = (v: bigint): string => (quote ? `${formatUnits(v, quote.decimals, { group: ',' })} ${quote.ticker}` : `${formatKas(v, { group: ',' })} KAS`);
  // the LAST order of a limit ladder is the one furthest from the typed price: planned too, so a level that could never be placed is refused before the first is signed
  const lastOrderErrors = useMemo(() => {
    if (!enabled || !env || !limitSet || !ladder?.ok) return [];
    try {
      return errors(planOrder(env, ladder.levels[ladder.levels.length - 1]!.intent));
    } catch {
      return [];
    }
  }, [enabled, env, limitSet, ladder]);
  const ladderTot = useMemo(() => (shownPlan && ladder ? ladderTotals(shownPlan, ladder) : null), [shownPlan, ladder]);
  // a ladder the wallet cannot pay for as a whole (each level alone may plan): the shortfall is an error and the button stays disabled
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
  // C5-10: a wallet not known to sign covenant inputs (every cancel needs one) passes a one-time test signature before its first order
  const gate = useCovenantSignGate(enabled ? env : null);
  const canPlace =
    enabled && gate.ready && !!shownPlan && shownPlan.ok && planErrors.length === 0 && !!shownPlan.built && !pending && !placing && !sendBusy(sender.status) && !crash
    && (!ladder || ladder.ok) && lastOrderErrors.length === 0 && ladderFunds.length === 0;
  // an edit makes a reason the last click stopped for stale (a submitted order stays on the line until the next click)
  const edited = () => {
    setMaxNote(null);
    setLadderNote(null);
    if (!placing && sender.status && sender.status.phase !== 'submitted') sender.clear();
  };

  // an inverted market words the type where the native name / help speaks of buying or selling the token
  const typeKey = (id: OrderTypeId, part: 'name' | 'help'): string => (inverted && has(`ticket.type.${id}.${part}Inv`) ? `ticket.type.${id}.${part}Inv` : `ticket.type.${id}.${part}`);
  const typeName = t(typeKey(form.type, 'name'), { name });
  // the side as the shown pair names it: buying KAS in KAS/TOKEN is selling the token
  const shownSide: Side = inverted ? oppositeSide(form.side) : form.side;
  const sideName = t(shownSide === 'buy' ? 'common.side.buy' : 'common.side.sell');

  /**
   * An inverted market: what an intent gives and receives in the shown pair's words (KAS is the base), for the disclosure summary. The KAS is the
   * amount's value at the price its KAS amount converts at (form-state.ts `amountPriceOf`: a limit's own price; a buy's worst price plus the
   * tip, the most it spends; "about" at the book's price or a price that moves).
   */
  const shownTerms = (intent: Intent | null): { tokens: string; kas: string; about: boolean; upTo: boolean } | null => {
    if (!inverted || !intent || !('amount' in intent) || typeof intent.amount !== 'bigint') return null;
    const at = amountPriceOf(form, ctx, 'amount');
    const price = at?.price ?? null;
    if (price === null || price <= 0n) return null;
    return {
      tokens: formatUnits(intent.amount, token.decimals, { group: ',' }),
      kas: formatKas(baseToKas(intent.amount, price, scale), { group: ',' }),
      about: at!.basis === 'book',
      // a buy sized at the most it can pay: the KAS is the most it spends for the tokens it buys
      upTo: at!.basis === 'worst',
    };
  };
  const discSummary = (() => {
    const s = shownTerms(built.intent);
    const key = s?.upTo ? 'ticket.disc.summaryKasUpTo' : s?.about ? 'ticket.disc.summaryKasAbout' : 'ticket.disc.summaryKas';
    return s ? t(key, { side: sideName, kas: s.kas, amount: s.tokens, ticker }) : undefined;
  })();

  /** one order of the ladder in words: the level, its prices (a limit ladder: side, amount, price and value) */
  const ladderRow = (l: LadderLevel): string => {
    if (!limitSet) return ladderRepeatRow(l);
    const amount = (l.intent as { amount: bigint }).amount;
    return t('ticket.ladder.rowLimit', {
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
   * Plans one order of the click (`index` of `levels`: a ladder places each level as its own transaction) at once, from the environment on hand
   * when it is recent enough, with the node clock read now (an auction starts from it); the signature then waits for the re-check against the state
   * at the click (review-check.ts: the freshness rule), which runs alongside the send's own checks. A level that cannot be planned stops the click.
   */
  const prepareLevel = async (intent: Intent, index: number, levels: number): Promise<SendJob | PlanStop> => {
    const clickAt = Date.now();
    try {
      // the environment: the first level takes the one on hand when its oldest part is at most REVIEW_CACHE_MS old, else a read started at most
      // REVIEW_REUSE_MS before the click; a later level of a ladder reads everything after the click (the level before has just spent coins)
      const snap = index === 0 ? snapshot() : null;
      const cached = snap && clickAt - snap.at <= REVIEW_CACHE_MS ? snap : null;
      let base = cached ? cached.env : await (index === 0 ? since(clickAt - REVIEW_REUSE_MS) : refresh());
      if (!base) return { text: t('ticket.reviewNoEnv') };
      let fromCache = !!cached;
      // the node clock now: DAA times (an auction's start, a timed start, an IOC life, a day order) count from it, never from the clock of the
      // environment on hand: the background clock projected to the click when it is fresh, else a read (a node slower than CLOCK_WAIT_MS: the
      // clock on hand projected to now, review-check.ts `projectClock`)
      const clock = liveClockAt(liveClock.current, clickAt)
        ?? (await reviewClock(() => readClock(services), base.clock, cached ? cached.clockAt : (snapshot()?.clockAt ?? clickAt), { waitMs: CLOCK_WAIT_MS })).clock;
      const planWith = (e: PlanEnv, refs: References): OrderPlan => planOrder(withRefs({ ...e, clock }, refs), intent);
      let p = planWith(base, touches.data ?? noRefs());
      if ((!p.ok || !p.built || errors(p).length > 0) && fromCache) {
        // the parts on hand cannot plan it (the holdings of a sell still being read, the book moved): plan from the state at the click instead
        const fresh = await since(clickAt - REVIEW_REUSE_MS);
        if (!fresh) return { text: t('ticket.reviewNoEnv') };
        base = fresh;
        fromCache = false;
        p = planWith(base, verifiers.length ? await touchesOf().catch(noRefs) : (touches.data ?? noRefs()));
      }
      const firstError = errors(p)[0];
      if (!p.ok || !p.built || firstError) return { text: firstError ? issueText(firstError, issueCtx) : t('ticket.reviewChanged') };
      // the re-check reads again only the parts that may no longer be the state at the click (review-check.ts rule 2; none when the environment was
      // read after the click); the references of further indexers are always read again
      const toRead = fromCache ? stale(clickAt) : [];
      const settled = toRead.length === 0;
      return {
        built: p.built, expected: expectedFromPlan(p), label: orderLabel(form.type, form.side, levels > 1 ? { level: index + 1, levels } : undefined, name),
        openToken: token.openList ? token : undefined,
        gate: !settled || verifiers.length > 0 ? recheck(p, intent, clock, base.fees, settled ? base : null, clickAt, toRead) : undefined,
        submitByDaa: auctionSubmitBy(p.states, clock.daa),
      };
    } catch (e) {
      const raw = e instanceof Error ? e.message : String(e);
      return { text: buildErrorText(raw), raw };
    }
  };

  /**
   * The re-check of a planned order (review-check.ts rule 2): the same intent planned against the environment at the click (`env`: the one on hand
   * already is; else `parts` read again from REVIEW_REUSE_MS before the click on) and the references read now, with the clock and the fees of the
   * plan; the wallet is asked only when it gives the same transaction without an error. Resolves to why the order no longer stands, or null.
   */
  const recheck = async (shown: OrderPlan, intent: Intent, clock: Clock, fees: PlanEnv['fees'], env: PlanEnv | null, clickAt: number, parts: EnvWait[]): Promise<string | null> => {
    try {
      const [fresh, refs] = await Promise.all([env ? Promise.resolve(env) : since(clickAt - REVIEW_REUSE_MS, parts), touchesOf().catch(noRefs)]);
      if (!fresh) return t('ticket.reviewNoEnv');
      const v = recheckVerdict(shown, planOrder(withRefs({ ...fresh, clock, fees }, refs), intent));
      if (v.ok) return null;
      return v.issues[0] ? issueText(v.issues[0], issueCtx) : t('ticket.reviewChanged');
    } catch (e) {
      return buildErrorText(e instanceof Error ? e.message : String(e));
    }
  };

  /** One level: planned, checked and sent; an auction whose start passed while the wallet waited is planned again and sent again (CLOCK_RETRIES). */
  const placeLevel = async (intent: Intent, index: number, levels: number): Promise<SendOutcome | null> => {
    for (let attempt = 0; ; attempt++) {
      const job = await prepareLevel(intent, index, levels);
      if (!('built' in job)) {
        sender.stop(job.text, job.raw);
        return null;
      }
      const out = await sender.send(job);
      if (out.status === 'submitted' || out.send.code !== 'clock-moved' || attempt >= CLOCK_RETRIES) return out;
    }
  };

  /** The order button: every level of the order in turn, each planned at its turn, checked and approved in the wallet; a level that stops ends it. */
  const place = async () => {
    if (!canPlace || !built.intent) return;
    const intents = ladder?.ok ? ladder.levels.map((l) => l.intent) : [built.intent];
    const levels = intents.length;
    setPlacing(true);
    setMaxNote(null);
    setLadderNote(null);
    // the line of an earlier click goes: the button shows the click is running until the first check reports
    sender.clear();
    let placed = 0;
    try {
      for (let i = 0; i < levels; i++) {
        setLadderLead(levels > 1 ? t('ticket.ladder.reviewing', { level: i + 1, levels }) : null);
        const out = await placeLevel(intents[i]!, i, levels);
        if (out?.status !== 'submitted') break;
        placed += 1;
      }
    } finally {
      setPlacing(false);
    }
    if (placed === levels) {
      setLadderLead(null);
      if (levels > 1) setLadderNote({ done: true, text: t('ticket.ladder.done', { levels }) });
      // the order is placed: the next one starts empty (the price, type and side stay: the next click is usually a variation)
      setForm((f) => setValue(f, 'amount', ''));
    } else if (placed > 0) {
      setLadderNote({ done: false, text: t('ticket.ladder.stopped', { placed, levels }) });
    }
    void refresh();
  };

  // ---- status lines: what is needed and what is missing
  const needs: { code: string; text: string; tone: 'info' | 'warn' | 'error'; action?: 'retry'; raw?: string }[] = [];
  if (!wallet.info) needs.push({ code: 'wallet', text: t('ticket.need.wallet'), tone: 'info' });
  else if (wallet.networkMismatch) needs.push({ code: 'network', text: t('ticket.need.network', { wallet: wallet.info.network, app: services.config.network }), tone: 'error' });
  if (enabled && envError) needs.push({ code: 'env', text: t('ticket.need.env'), tone: 'error', action: 'retry', raw: envError });
  if (crash) needs.push({ code: 'crash', text: t('ticket.need.crash'), tone: 'error', raw: crash });
  if (enabled && !envError && built.intent === null && built.errors.some((e) => e.code === 'required')) needs.push({ code: 'incomplete', text: t('ticket.need.incomplete'), tone: 'info' });
  // what the ticket is still reading, named (Review cannot be enabled before; it is the moment the answers are in)
  const holdingsShort = waiting.includes('holdings') && planErrors.some((i) => TOKEN_SHORT.has(i.code));
  const stillReading: EnvWait[] = enabled && !envError ? (rawEnv ? (holdingsShort ? ['holdings'] : []) : waiting) : [];
  if (stillReading.length) needs.push({ code: 'loading', text: t('ticket.need.loading', { what: stillReading.map((w) => t(`ticket.wait.${w}`)).join(', ') }), tone: 'info' });

  const issues = (shownPlan?.issues ?? []).filter((i) => !(holdingsShort && TOKEN_SHORT.has(i.code)));
  const planIssues = issues.filter((i) => i.severity === 'error');
  const issueCtx = { tokenDecimals: token.decimals, tokenTicker: ticker, ...(quote ? { quoteDecimals: quote.decimals, quoteTicker: quote.ticker } : {}) };

  if (!tradable) {
    const reason = token.untradableReason;
    return (
      <section class="tk" data-testid="order-ticket" data-state="untradable">
        <h2>{t('ticket.title')}</h2>
        <Banner tone="warn" title={t('ticket.untradable.title', { ticker })} data-testid="order-untradable">
          <p>{reason ? keyedText('untradable', reason) : t('ticket.untradable.generic')}</p>
          <p class="muted">{t('ticket.untradable.hint')}</p>
        </Banner>
      </section>
    );
  }

  return (
    <section class="tk" data-testid="order-ticket" data-type={form.type} data-side={form.side} data-shown-side={shownSide} data-inverted={inverted ? '1' : '0'} aria-busy={pending || envLoading ? 'true' : 'false'}>
      <div class="row-between">
        <h2>{t('ticket.title')}</h2>
        {pending ? (
          <span class="tk-pending" data-testid="order-pending">
            <Loading text={t('ticket.pending')} />
          </span>
        ) : null}
      </div>

      <div class="tk-form">
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

        <div class="field">
          <label for="order-type">{t('ticket.type.label')}</label>
          <select id="order-type" class="select" value={form.type} data-testid="order-type" onChange={(e) => onType((e.currentTarget as HTMLSelectElement).value as OrderTypeId)}>
            {TYPE_GROUPS.map((g) => (
              <optgroup key={g.group} label={t(`ticket.group.${g.group}`)}>
                {g.types.map((id) => (
                  <option key={id} value={id}>
                    {t(typeKey(id, 'name'), { name })}
                  </option>
                ))}
              </optgroup>
            ))}
          </select>
          <div class="field-hint" data-testid="order-type-help">
            {t(typeKey(form.type, 'help'), { name })}
          </div>
        </div>

        {enabled ? (
          <p class="tk-balances" data-testid="order-balances">
            {env
              ? quote
                ? t('ticket.balancePair', {
                    tokens: `${formatUnits(tokenBalance, token.decimals, { group: ',' })} ${ticker}`,
                    quote: `${formatUnits(quoteBalance, quote.decimals, { group: ',' })} ${quote.ticker}`,
                    kas: `${formatKas(kasBalance, { group: ',' })} KAS`,
                  })
                : t('ticket.balance', { tokens: `${formatUnits(tokenBalance, token.decimals, { group: ',' })} ${ticker}`, kas: `${formatKas(kasBalance, { group: ',' })} KAS` })
              : t('ticket.balanceLoading')}
          </p>
        ) : null}

        <TicketFields
          form={form} ctx={ctx} ticker={ticker} ids={layout.main} errors={errs} onChange={onChange} onMax={enabled && env ? onMax : undefined} slider={slider}
          {...(inverted ? { flip } : {})} {...(quote ? { quoteTicker } : {})}
        />

        {maxNote ? (
          <p class="muted" data-testid="order-max-note">
            {maxNote}
          </p>
        ) : null}

        <details class="tk-advanced" data-testid="order-advanced">
          <summary>{t('ticket.advanced')}</summary>
          <div class="tk-advanced-body">
            <TicketFields form={form} ctx={ctx} ticker={ticker} ids={layout.advanced} errors={errs} onChange={onChange} {...(inverted ? { flip } : {})} {...(quote ? { quoteTicker } : {})} />
          </div>
        </details>
      </div>

      <div class="tk-needs" data-testid="order-needs">
        {needs.map((n) => (
          <Banner
            key={n.code}
            tone={n.tone}
            data-testid={`order-need-${n.code}`}
            actions={n.action === 'retry' ? <Button small onClick={() => void refresh()} data-testid="order-retry">{t('common.retry')}</Button> : undefined}
          >
            {n.text}
            <RawDetails text={n.raw} />
          </Banner>
        ))}
      </div>

      {/* only what stops the order: the planner's warnings and notes are not shown (the figures below say what the order does) */}
      <div class="tk-issues" data-testid="order-issues" aria-live="polite">
        {planIssues.length > 0 ? (
          <ul>
            {planIssues.map((i, n) => (
              <li key={`${i.code}-${n}`} class="tk-issue tk-issue-error" data-code={i.code} data-severity={i.severity} data-testid={`order-issue-${i.code}`}>
                {issueText(i, issueCtx)}
                <RawDetails text={rawOf(i)} />
              </li>
            ))}
          </ul>
        ) : null}
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
              {limitSet ? (
                <p data-testid="order-ladder-total">
                  {t('ticket.ladder.totalLimit', {
                    amount: `${formatUnits(ladder.totalAmount, token.decimals, { group: ',' })} ${ticker}`, levels: ladder.levels.length,
                    value: quoteText(limitLadderValues(ladder, form.side, scale).total),
                  })}
                </p>
              ) : ladderTot ? (
                <p data-testid="order-ladder-total">
                  {t('ticket.ladder.total', { amount: `${formatUnits(ladderTot.amount, token.decimals, { group: ',' })} ${ticker}`, levels: ladder.levels.length, kas: `${formatKas(ladderTot.kasLocked, { group: ',' })} KAS` })}
                </p>
              ) : null}
              {ladderFunds.map((f) => (
                <p key={f.code} class="tk-issue tk-issue-error" data-testid="order-ladder-funds" data-code={f.code}>
                  {f.text}
                </p>
              ))}
              {lastOrderErrors.length > 0 ? (
                <ul data-testid="order-ladder-last-errors">
                  {lastOrderErrors.map((i, n) => (
                    <li key={n} class="tk-issue tk-issue-error">
                      {t('ticket.ladder.lastLevel', { text: issueText(i, issueCtx) })}
                    </li>
                  ))}
                </ul>
              ) : null}
              <p class="muted">{t('ticket.ladder.separate', { levels: ladder.levels.length })}</p>
            </>
          ) : (
            <ul data-testid="order-ladder-errors">
              {ladder.errors.map((e, n) => (
                <li key={n} class="tk-issue tk-issue-error">
                  {t(`ticket.ladder.err.${e.code}`, { level: e.level ?? 1, field: e.field ?? '', max: 20 })}
                </li>
              ))}
            </ul>
          )}
        </div>
      ) : null}

      {model ? <DisclosurePanel model={model} built={shownPlan?.built ?? null} summary={discSummary} /> : null}

      {enabled ? gate.element : null}
      <Button variant="primary" block onClick={() => void place()} disabled={!canPlace} loading={placing} data-testid="order-place" data-state={placing ? 'placing' : undefined}>
        {t('ticket.place', { side: sideName, type: typeName })}
      </Button>
      <SendStatusLine status={sender.status} lead={ladderLead} data-testid="order-status" />
      {ladderNote ? (
        <p class="muted" data-testid={ladderNote.done ? 'order-result-ladder' : 'order-ladder-stopped'}>
          {ladderNote.text}
        </p>
      ) : null}
    </section>
  );
}
