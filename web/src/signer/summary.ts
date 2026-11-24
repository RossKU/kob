// The signer's own short summary of a signing request.
//
// It is decoded inside the signer frame with kob-wasm (`decodeSigning`, the decoder of the app's
// confirmation screen). It comes from the transaction itself, never from what the app says about
// it. It holds:
// - the net effect on this key,
// - the fee,
// - the terms of every order the transaction creates or amends: side, type, amount, prices, stops
//   and timing, as the rows of the confirmation screen.
//
// The order terms are there so that a changed price or stop cannot pass behind an unchanged net
// effect. A plain order of a KAS market that the app shows inverted (KAS/TOKEN) is said first as
// the user placed it, for example sell KAS for tokens. Its chain-side terms follow. Every price
// reads "X per Y", never as a pair notation.
import { decodeSigning, type DecodedOrder, type DecodedOutput, type SigningKind, type SigningSummary, type TokenRef } from '../kob/decode';
import { spendsPreparation } from '../kob/issue';
import { curveNumbersOfState, curveProblems, tablePreview } from '../kob/curve';
import { decodeDeclaration } from '../kob/kcc23';
import { declaredLock as lockOfDeclaration } from '../kob/creator-lock';
import { isTokenProgram } from '../kob/programs.generated';
import { formatRowDate } from '../ui/kit/format';
import { baseKind, isMultiFillMarket, type OrderDescription } from '../kob/order-facts';
import { DEFAULT_BID_FILLS } from '../kob/orders/common';
import { longId, tickerUnit, type TokenInfo, type TokenRegistry } from '../kob/registry';
import type { BuiltTx, Hex } from '../kob/types';
import { decimalsOfScale, formatKas, formatTokenAmount } from '../kob/units';
import type { KobWasm } from '../kob/wasm';
import { RAW_TOKEN_DECIMALS, t } from '../i18n';
import { formatRowTime } from '../ui/kit/format';
import { frameShape } from './shapes';
import { issueText } from '../i18n/issue-text';
import { auctionSeconds, orderRows, orderTitle, orderTypeCode, type Fmt, type MarketFillsFacts } from '../ui/confirm/confirm-model';
import type { GenesisClaim, LockHint, TokenHint } from './protocol';
import { div, kasWords, tokensPerKasWords, tokenWords, type Rounding } from './words';

/** One line of the summary, such as a net amount, a lock or a payment to other keys. */
export interface SummaryLine {
  label: string;
  value: string;
  /**
   * The value holds a whole id or key (never shortened in the middle, founder 2026-11-12): drawn in
   * a monospace block that breaks over lines, so nothing of it is hidden.
   */
  mono?: boolean;
  /** `out` leaves this key and `in` comes to it. */
  dir: 'in' | 'out' | null;
}

/**
 * One order the transaction creates or amends, described from its decoded state. An if-done entry
 * carries its committed exit as `exit`.
 */
export interface SummaryOrder {
  title: string;
  /** The token of the order, as its ticker hint and 8 + 8 hex of the covenant id. */
  token: string;
  /**
   * The rows of the order.
   *
   * - `id` is the confirmation row it comes from (confirm-model.ts `orderRows`).
   * - `daa` is the DAA score of a start or an expiry the row names. Where a clock is known, it is
   *   said from it, as in "in 28 s".
   */
  rows: { id?: string; label: string; value: string; daa?: string }[];
  exit: SummaryOrder | null;
}

/** The count and the distinct codes of the findings the frame shows as one problems note. */
const problemsOf = (found: readonly { code: string }[]): { problems: number; problemCodes: string[] } => ({ problems: found.length, problemCodes: [...new Set(found.map((b) => b.code))] });

/** The signer frame's summary of one request, as lines, orders, the fee and the problems found. */
export interface SignerSummary {
  title: string;
  lines: SummaryLine[];
  /**
   * The orders the transaction creates or amends. A sweep in place keeps its order's terms and is
   * not listed.
   */
  orders: SummaryOrder[];
  fee: string;
  /**
   * Set when the transaction spends inputs the wallet does not sign, such as an order's own UTXOs,
   * custodies or strays. Their KAS is the node's word, which the signature does not cover.
   */
  unsignedKas?: boolean;
  /**
   * The count of decoder findings that block signing on the app's confirmation screen. The frame
   * shows them as one line and never hides them.
   */
  problems: number;
  /** The codes of those findings. The frame names them, and Approve stays disabled while any do. */
  problemCodes: string[];
  /**
   * The decoding the summary was made from. A re-timed request is compared with the approved one on
   * it (retime.ts).
   */
  decoded: SigningSummary;
}

const TITLE: Record<SigningKind, string> = {
  create: 'signer.kind.create',
  cancel: 'signer.kind.cancel',
  'cancel-replace': 'signer.kind.cancelReplace',
  'cancel-position': 'signer.kind.cancel',
  refund: 'signer.kind.refund',
  send: 'signer.kind.send',
  sweep: 'signer.kind.sweep',
  claim: 'signer.kind.claim',
  'claim-merge': 'signer.kind.claimMerge',
  'claim-place': 'signer.kind.claimPlace',
  take: 'signer.kind.take',
  unlock: 'signer.kind.unlock',
  'lock-coin': 'signer.kind.lockCoin',
  other: 'signer.kind.other',
};

/**
 * Findings that block the app's confirmation screen unless the user asked for them. The app knows
 * whether the user did and the frame does not, so the frame shows them as "to other keys" lines.
 */
const SHOWN_AS_LINES = new Set<string>(['payment-out', 'transfer-out']);

/**
 * A token's label in the frame. It is the app's ticker hint next to 8 + 8 hex of the covenant id
 * the transaction itself carries. The ticker is only a label that the app chooses. A 4 + 4 fragment
 * would be 32 bits, which a token made to match it can copy.
 */
const tokenName = (covenantId: Hex, hint: TokenHint | undefined, frame = false): string => {
  // In the frame a token is named by its WHOLE covenant id (never shortened in the middle, founder
  // 2026-11-12), with what is known of it and from where.
  if (frame) return frameTokenName(covenantId, hint);
  // Nothing states its decimals. It is named by its covenant id, and the label says the amount is
  // raw.
  if (!hint || hint.source === 'assumed') return t('signer.net.tokenAssumed', { id: longId(covenantId) });
  // Named and marked as app/token-display.ts shows it, by its covenant id and by the ticker its
  // creator gave it where it has one.
  if (hint.source === 'scale') return t('signer.net.tokenScale', { id: longId(covenantId), decimals: hint.decimals });
  if (hint.source === 'program') return t('signer.net.tokenProgram', { id: longId(covenantId), decimals: hint.decimals, source: hint.convention ?? '' });
  if (hint.source === 'issued') return t('signer.net.tokenIssued', { ticker: hint.ticker, id: longId(covenantId) });
  if (hint.source === 'declared') return t('signer.net.tokenDeclared', { ticker: hint.ticker, id: longId(covenantId) });
  // A registry token. In the signer frame (`frame`) its ticker and decimals are the app's word, and
  // the label says so. The app's own confirmation is the app, so it shows them plainly.
  if (!frame) return `${hint.ticker} (${longId(covenantId)})`;
  return t('signer.net.tokenListed', { ticker: hint.ticker, id: longId(covenantId), decimals: hint.decimals });
};

/**
 * Whether the frame can vouch for a ticker: the pinned registry table's row (a hint without a
 * source, trusted-hints.ts) or the token's own declaration on chain. Any other ticker is the app's.
 */
const verifiedTicker = (h: TokenHint | undefined): string | null => (h && h.ticker && (!h.source || h.source === 'declared') ? h.ticker : null);

/**
 * A token in the frame: its whole covenant id, then what is known of it and from where, and a name
 * the app gives it that the frame cannot vouch for, said to be the app's.
 */
function frameTokenName(covenantId: Hex, h: TokenHint | undefined): string {
  const known = !h || h.source === 'assumed'
    ? t('signer.token.assumed')
    : h.source === 'scale'
      ? t('signer.token.scale', { decimals: h.decimals })
      : h.source === 'program'
        ? t('signer.token.program', { decimals: h.decimals, source: h.convention ?? '' })
        : h.source === 'issued'
          ? t('signer.token.issued', { decimals: h.decimals })
          : h.source === 'declared'
            ? t('signer.token.declared', { ticker: h.ticker, decimals: h.decimals })
            : t('signer.token.listed', { ticker: h.ticker, decimals: h.decimals });
  const named = h?.appName ?? (h && h.ticker && !verifiedTicker(h) ? h.ticker : null);
  const appName = named ? ` ${t('signer.token.appCalls', { ticker: named })}` : '';
  return `${covenantId} (${known})${appName}`;
}

/**
 * A TokenRef from the app's hint. Its ticker and decimals are labels. Without a hint, amounts stay
 * in base units. In the frame the unit of every amount and price is a ticker the frame vouches for,
 * else the plain word "token" ("token A" / "token B" on a pair order): never an id fragment and never
 * the app's unverified ticker.
 */
function refOfHint(covenantId: Hex, hints: TokenHint[], frame = false, role?: 'A' | 'B'): TokenRef {
  const h = hints.find((x) => x.covenantId === covenantId);
  const ticker = frame ? (verifiedTicker(h) ?? (role ? t('signer.token.unitRole', { role }) : t('signer.token.unit'))) : h ? (h.ticker || longId(covenantId)) : null;
  return { covenantId, ticker, decimals: h?.decimals ?? null, display: tokenName(covenantId, h, frame), inRegistry: false, tradable: false };
}

/**
 * Adds to the hints the scale the transaction's own orders name, for a token whose decimals nothing
 * else states. Such a token has no declaration, no program default and no table of this frame.
 *
 * The order quotes per that whole token, as the ticket that planned it did. The scale is read from
 * the decoded transaction, in the frame too, so it is not the app's word.
 */
function withOrderScales(s: SigningSummary, hints: readonly TokenHint[]): TokenHint[] {
  const scales = new Map<Hex, bigint>();
  const add = (d: OrderDescription | null | undefined): void => {
    if (!d) return;
    if (d.pair) for (const x of [d.pair.base, d.pair.quote]) scales.set(x.covId, x.scale);
    else if (d.tokenCovId) scales.set(d.tokenCovId, d.scale);
    add(d.entry?.exit);
  };
  for (const o of s.orders) add(o.description);
  for (const sp of s.spends) add(sp.description);
  const out = hints.filter((h) => h.source !== 'assumed' || !scales.has(h.covenantId) || decimalsOfScale(scales.get(h.covenantId)!) === null);
  for (const [covenantId, scale] of scales) {
    const decimals = decimalsOfScale(scale);
    if (decimals === null || out.some((h) => h.covenantId === covenantId)) continue;
    const inverted = hints.find((h) => h.covenantId === covenantId)?.inverted ? { inverted: true } : {};
    out.push({ covenantId, ticker: '', decimals, source: 'scale', ...inverted });
  }
  return out;
}

/**
 * The hints as the confirmation model's registry lookup, which it uses for a pair order's quote
 * token B. Only the ticker and decimals are filled.
 */
function hintRegistry(hints: TokenHint[], frame = false): TokenRegistry {
  // a pair order's quote token B: in the frame its unit is a vouched ticker or "token B"
  const tickerOf = (h: TokenHint) => (frame ? (verifiedTicker(h) ?? t('signer.token.unitRole', { role: 'B' })) : h.ticker || longId(h.covenantId));
  const byCovenantId = new Map(hints.map((h) => [h.covenantId, { covenantId: h.covenantId, ticker: tickerOf(h), decimals: h.decimals, tradable: false, ...(frame ? { openList: true } : {}) } as unknown as TokenInfo]));
  return { network: '', templates: [], templateChecks: [], tokens: [], byCovenantId };
}

/**
 * Rows whose detail carries a signed term that the value does not. They are:
 * - the seconds of an auction, which say how fast the price falls to the bound,
 * - the seconds of the auction after a stop entry's trigger,
 * - the sizes a pair trigger needs to touch.
 *
 * The signer frame (`summarize`) shows no detail line of its own, so it says them in the value. The
 * app's confirmation shows the detail beside the value.
 */
const SIGNED_TERM_DETAIL = new Set(['auction', 'entryStop', 'pairTrigger']);

function orderSummary(d: OrderDescription, extra: Parameters<typeof orderRows>[1], hints: TokenHint[], registry: TokenRegistry, terms = false): SummaryOrder {
  const f: Fmt = { tr: t, ref: refOfHint(d.tokenCovId, hints, terms, d.pair ? 'A' : undefined), clock: null, time: (unix) => formatRowTime(Number(unix) * 1000, 0), registry };
  const exit = d.entry?.exit ? orderSummary(d.entry.exit, { value: null, locked: null, deadline: null }, hints, registry, terms) : null;
  // a pair order names both its tokens in full, in the order its lines use them (token A, token B)
  const pairToken = terms && d.pair ? t('signer.token.pair', { a: frameTokenName(d.pair.base.covId, hints.find((h) => h.covenantId === d.pair!.base.covId)), b: frameTokenName(d.pair.quote.covId, hints.find((h) => h.covenantId === d.pair!.quote.covId)) }) : null;
  return {
    title: orderTitle(d, extra.deadline, f),
    token: pairToken ?? f.ref.display,
    rows: orderRows(d, extra, f).map((r) => ({ id: r.id, label: r.label, value: r.detail && terms && SIGNED_TERM_DETAIL.has(r.id) ? `${r.value} (${r.detail})` : r.value, ...timeOf(r, d) })),
    exit: exit ? { ...exit, title: t('confirm.exitOf', { title: exit.title }) } : null,
  };
}

const signed = (v: bigint, text: (abs: bigint) => string) => (v < 0n ? `−${text(-v)}` : `+${text(v)}`);

/** The DAA score a start or expiry row says as a DAA score (not an if-done exit's open expiry). */
function timeOf(r: { id: string; value: string }, d: OrderDescription): { daa?: string } {
  const daa = r.id === 'activeFrom' ? d.activeFrom : r.id === 'expiry' ? d.expiryDaa : 0n;
  return daa > 0n && r.value === t('confirm.f.daa', { daa: daa.toString() }) ? { daa: daa.toString() } : {};
}

/** The chain-side price rows of a plain order. The flipped view says them in its own terms. */
const CHAIN_PRICE_ROWS = new Set(['price', 'allIn', 'auction']);

/**
 * The view word of each plain order type, for the title of an order on the flipped view, as in
 * "Sell KAS · market (auction)".
 */
const FLIP_TYPE: Record<string, string> = {
  limit: 'signer.flip.type.limit', limitDay: 'signer.flip.type.limitDay', market: 'signer.flip.type.market', marketFok: 'signer.flip.type.marketFok',
  ioc: 'signer.flip.type.ioc', fok: 'signer.flip.type.fok', twap: 'signer.flip.type.twap', dca: 'signer.flip.type.dca', dutch: 'signer.flip.type.dutch',
  rising: 'signer.flip.type.rising', marketFills: 'signer.flip.type.marketFills',
};

/**
 * Says a plain order of a KAS market (KobAsk or KobBid) as the user placed it, on a view the app
 * shows inverted.
 *
 * The view is KAS/TOKEN, the hint's `inverted`. The summary still comes from the decoded order.
 * Where the chain buys tokens, the user sells KAS. The summary gives:
 * - a title such as "Sell KAS · market (auction)",
 * - what is sold and received,
 * - the prices in tokens per KAS, rounded against the user.
 *
 * The chain-side terms, such as budget, escrow and minimum fill, follow as plainly labelled rows.
 * It returns null for every other order and for a native view.
 */
function flippedSummary(o: DecodedOrder, hint: TokenHint | undefined, kob: KobWasm | undefined, chain: SummaryOrder, terms = false): SummaryOrder | null {
  const d = o.description;
  const kind = baseKind(d.kind);
  if (!hint?.inverted || d.pair || d.entry || (kind !== 'KobAsk' && kind !== 'KobBid') || d.scale <= 0n || d.price <= 0n) return null;
  const T = hint.ticker ? tickerUnit(hint.ticker, d.tokenCovId) : longId(d.tokenCovId);
  const dec = hint.decimals;
  const scale = d.scale;
  // A price that decays to 0 is still a decay. Its bound is 0 KAS, and the summary says so.
  const decays = !!d.auction && d.auction.slope > 0n;
  const start = d.price;
  const end = decays ? d.auction!.priceEnd : start;
  const typeName = t(FLIP_TYPE[orderTypeCode(d, o.deadline)] ?? 'signer.flip.type.limit');
  const kasT = (v: bigint, mode: Rounding) => `${kasWords(v, mode)} KAS`;
  const tokT = (n: bigint, mode: Rounding) => `${tokenWords(n, dec, mode)} ${T}`;
  const per = (price: bigint, mode: Rounding) => t('signer.flip.perKas', { price: tokensPerKasWords(price, scale, dec, mode), ticker: T });
  const rows: { label: string; value: string }[] = [];
  let side: 'buy' | 'sell';
  if (kind === 'KobBid') {
    // The chain buys a fixed amount of tokens for at most its budget, so the user sells up to that
    // much KAS for those tokens. A higher KAS price per token is fewer tokens per KAS, which is
    // worse for the user, so it rounds down.
    side = 'sell';
    const st = o.state.state as unknown as { reserve?: string; deliveryCarrier?: string };
    const { amount: n, fills } = bidAmountOf(o, kob, scale, end + d.tip);
    const budget = o.value - (fills > 0n ? fills - 1n : 0n) - fills * BigInt(st.deliveryCarrier ?? '0') - BigInt(st.reserve ?? '0');
    rows.push({ label: t('signer.flip.youSell'), value: t('signer.flip.upTo', { amount: kasT(budget > 0n ? budget : 0n, 'up') }) });
    // A multi-fill market order can deliver more than the amount its budget was sized for, because
    // the carrier of a fill that does not happen buys tokens.
    const power = kob && isMultiFillMarket(d) ? maxReceiveOf(o, kob) : undefined;
    rows.push({ label: t('signer.flip.youReceive'), value: power !== undefined && power > n ? t('signer.flip.upTo', { amount: tokT(power, 'down') }) : tokT(n, 'down') });
    rows.push({ label: t(decays ? 'signer.flip.expected' : 'signer.flip.price'), value: per(start, 'down') });
    if (decays || d.tip > 0n) rows.push({ label: t('signer.flip.worst'), value: per(end + d.tip, 'down') });
  } else {
    // The chain sells a fixed amount of tokens for at least its price, so the user pays those
    // tokens and buys the KAS they fetch. A lower KAS price per token is more tokens per KAS, which
    // is worse for the user, so it rounds up.
    side = 'buy';
    const n = d.tokenAmount ?? d.amountLeft ?? 0n;
    const kasAt = (price: bigint) => div(n * (price > d.tip ? price - d.tip : 0n), scale, 'down');
    rows.push({ label: t('signer.flip.youPay'), value: tokT(n, 'nearest') });
    rows.push({ label: t('signer.flip.youReceive'), value: t('signer.flip.atLeast', { amount: kasT(kasAt(end), 'down') }) });
    rows.push({ label: t(decays ? 'signer.flip.expected' : 'signer.flip.price'), value: per(start, 'up') });
    if (decays || d.tip > 0n) rows.push({ label: t('signer.flip.worst'), value: per(end > d.tip ? end - d.tip : 1n, 'up') });
  }
  // How fast the price falls is a signed term that the flipped prices do not say.
  const seconds = auctionSeconds(d);
  if (terms && seconds !== null) rows.push({ label: t('confirm.f.auction'), value: t('confirm.f.auctionValue', { seconds }) });
  return {
    title: t('signer.flip.title', { side: t(side === 'sell' ? 'common.side.sell' : 'common.side.buy'), type: typeName }),
    // The token, and the order as the chain holds it. Its terms follow.
    token: t('signer.flip.onChain', { token: chain.token, order: chain.title }),
    // The chain's prices are said above in the view's terms. Its other terms follow.
    rows: [...rows, ...chain.rows.filter((r) => !CHAIN_PRICE_ROWS.has(r.id ?? ''))],
    exit: null,
  };
}

/**
 * Finds the amount a plain bid's escrow was made for and the deliveries it funds.
 *
 * The amount is the largest `n` whose escrow the order UTXO holds. The escrow is the covenant's
 * `bidEscrow` of `n` in `f` fills. First `f` is taken as the planner budgets it:
 * - one for IOC, FOK and market,
 * - one per slice of a DCA,
 * - one per minimum fill of a resting bid, up to DEFAULT_BID_FILLS.
 *
 * When that escrow is not exact, it tries each count up to MAX_FILLS_SEARCHED that makes it exact,
 * which is a count the user chose. Without kob-wasm it gives the value over the worst rate, with
 * carriers and reserve not counted.
 */
function bidAmountOf(o: DecodedOrder, kob: KobWasm | undefined, scale: bigint, rate: bigint): { amount: bigint; fills: bigint } {
  const d = o.description;
  const upper = rate > 0n ? (o.value * scale) / rate : 0n;
  if (!kob) return { amount: upper, fills: 1n };
  const escrow = (n: bigint, f: bigint): bigint | null => {
    try {
      return kob.bidEscrow(o.state, n, f);
    } catch {
      return null;
    }
  };
  const largest = (fillsFor: (n: bigint) => bigint): bigint => {
    let lo = 0n;
    let hi = upper;
    while (lo < hi) {
      const mid = (lo + hi + 1n) / 2n;
      const e = escrow(mid, fillsFor(mid));
      if (e !== null && e <= o.value) lo = mid;
      else hi = mid - 1n;
    }
    return lo;
  };
  const planned = (n: bigint): bigint => {
    if (d.tif === 'ioc' || d.tif === 'fok') return 1n;
    const slice = d.auction && d.auction.interval > 0n ? d.auction.maxFill : 0n;
    if (slice > 0n) return (n + slice - 1n) / slice;
    const possible = d.minFill > 0n ? (n + d.minFill - 1n) / d.minFill : 1n;
    return possible < DEFAULT_BID_FILLS ? (possible > 0n ? possible : 1n) : DEFAULT_BID_FILLS;
  };
  const n = largest(planned);
  if (escrow(n, planned(n)) === o.value) return { amount: n, fills: planned(n) };
  for (let f = 1n; f <= MAX_FILLS_SEARCHED; f++) {
    const m = largest(() => f);
    if (escrow(m, f) === o.value) return { amount: m, fills: f };
  }
  return { amount: n, fills: planned(n) };
}

/**
 * The fills a multi-fill market buy budgets, and the delivery carrier each pays.
 *
 * Its escrow is `bidEscrow(order, amount, fills)`, and `bidAmountOf` finds the count that makes it
 * exact. It returns null for every other order, and without kob-wasm.
 */
function marketFillsOf(o: DecodedOrder, kob: KobWasm | undefined): MarketFillsFacts | null {
  const d = o.description;
  if (!kob || baseKind(d.kind) !== 'KobBid' || !isMultiFillMarket(d) || d.scale <= 0n) return null;
  const end = d.auction && d.auction.slope > 0n && d.auction.priceEnd > 0n ? d.auction.priceEnd : d.price;
  const { amount, fills } = bidAmountOf(o, kob, d.scale, end + d.tip);
  const carrier = BigInt((o.state.state as unknown as { deliveryCarrier?: string }).deliveryCarrier ?? '0');
  return { count: fills, carrier, amount, maxReceive: maxReceiveOf(o, kob), minFillLimit: d.minFill > 0n && amount > 0n && fills === (amount + d.minFill - 1n) / d.minFill };
}

/**
 * The most a bid's escrow lets its fills deliver.
 *
 * It is kob-wasm `bidBuyingPower` of the order state and the KAS on its UTXO, the covenant's
 * remaining buying power. A bid's quantity is its KAS. So this is the amount the escrow was sized
 * for plus what the carriers of fills that do not happen buy.
 */
function maxReceiveOf(o: DecodedOrder, kob: KobWasm): bigint | undefined {
  try {
    return kob.bidBuyingPower(o.state, o.value);
  } catch {
    return undefined;
  }
}

/**
 * The KAS that a pure self-transfer of token UTXOs moves off the wallet's token UTXOs into
 * spendable KAS.
 *
 * Such a transfer is a Merge. It creates no order, and every token output is the wallet's own. The
 * amount is the KAS on its token inputs less the KAS on its token outputs. The KAS line already
 * counts KAS on the wallet's token UTXOs as the wallet's, so there a merge reads as the fee alone.
 * This line says what becomes spendable. It is 0 for anything else, because in a placement part of
 * that KAS moves into the order's custody instead.
 */
export function freedFromTokens(s: Pick<SigningSummary, 'maker' | 'inputs' | 'outputs' | 'orders'>): bigint {
  const outs = s.outputs.filter((o) => o.token !== null);
  if (s.orders.length > 0 || outs.length === 0 || !outs.every((o) => o.token!.owner === s.maker && o.token!.escrowOf === null)) return 0n;
  const kasIn = s.inputs.filter((i) => i.type === 'token' && i.ownedBy === 'maker').reduce((sum, i) => sum + i.amount, 0n);
  const kasOut = outs.reduce((sum, o) => sum + o.value, 0n);
  return kasIn > kasOut ? kasIn - kasOut : 0n;
}

/**
 * One token a pure merge folds. It gives how many of the wallet's UTXOs go in, how many come out,
 * and the amount, which is the same on both sides.
 */
export interface MergedToken { ref: TokenRef; inCount: number; outCount: number; amount: bigint }

/**
 * The tokens a request merges, or null when it is not a pure merge.
 *
 * A pure merge is a self-transfer of the wallet's own token UTXOs. It creates or spends no order,
 * and every token input and output is the wallet's own plain UTXO. It puts each token's amount into
 * fewer UTXOs and keeps the amount unchanged. A merge moves no token to anyone and changes no
 * balance, so the net effect lists no token line. This says what is signed: N UTXOs into M, with
 * the same amount.
 */
export function mergedTokens(s: Pick<SigningSummary, 'kind' | 'maker' | 'inputs' | 'outputs' | 'orders' | 'spends'>): MergedToken[] | null {
  if (s.kind !== 'send' || s.orders.length > 0 || s.spends.length > 0) return null;
  const ins = s.inputs.filter((i) => i.token !== null);
  const outs = s.outputs.filter((o) => o.token !== null);
  if (ins.length === 0 || outs.length === 0 || ins.some((i) => i.ownedBy !== 'maker' || i.token!.escrowOf !== null || i.token!.owner !== s.maker)) return null;
  if (outs.some((o) => o.token!.owner !== s.maker || o.token!.escrowOf !== null)) return null;
  const byToken = new Map<string, MergedToken>();
  for (const i of ins) {
    const m = byToken.get(i.token!.ref.covenantId) ?? { ref: i.token!.ref, inCount: 0, outCount: 0, amount: 0n };
    m.inCount += 1;
    m.amount += i.token!.amount;
    byToken.set(i.token!.ref.covenantId, m);
  }
  const outAmount = new Map<string, bigint>();
  for (const o of outs) {
    const m = byToken.get(o.token!.ref.covenantId);
    if (!m) return null;
    m.outCount += 1;
    outAmount.set(o.token!.ref.covenantId, (outAmount.get(o.token!.ref.covenantId) ?? 0n) + o.token!.amount);
  }
  const merged = [...byToken.values()];
  return merged.every((m) => m.outCount >= 1 && m.outCount < m.inCount && outAmount.get(m.ref.covenantId) === m.amount) ? merged : null;
}

/** Most "closes order" lines of one request (a position cancel names each of its orders). */
const MAX_CLOSED_LINES = 8;

/** How many delivery counts `bidAmountOf` tries for a bid whose fills the user chose. */
const MAX_FILLS_SEARCHED = 64n;

/**
 * The orders whose price scale does not fit the decimals the app gave their token: those it creates,
 * spends or takes. Any such order blocks Approve.
 *
 * Take a plain order of a token whose decimals the app says it knows, meaning any hint but an
 * `assumed` one. It quotes its price per the standard scale of `10^min(decimals, 9)` base units,
 * which is what the app plans. The frame cannot check the decimals themselves. But an order whose
 * scale disagrees with them reads differently under a different hint, so the same order could read
 * as 0.5, 500,000,000 or 0.05 tokens.
 */
function scaleMismatches(s: SigningSummary, hints: TokenHint[]): { code: string }[] {
  const out: { code: string }[] = [];
  const check = (token: Hex, scale: bigint): void => {
    const h = hints.find((x) => x.covenantId === token);
    if (!h || h.source === 'assumed' || scale <= 0n) return;
    if (scale !== 10n ** BigInt(Math.min(h.decimals, 9))) out.push({ code: 'token-scale' });
  };
  // The orders it creates, the orders it spends (a cancel's token line uses the same decimals) and
  // the orders a take fills (its token line), so app decimals cannot misstate any of them by a power
  // of ten (R8F2b-3, R8F2-I2).
  for (const d of [...s.orders.map((o) => o.description), ...s.spends.map((x) => x.description)]) {
    if (d && !d.pair) check(d.tokenCovId, d.scale);
    // a pair order names the scale of both its tokens (R9FB2-8)
    if (d?.pair) {
      check(d.pair.base.covId, d.pair.base.scale);
      check(d.pair.quote.covId, d.pair.quote.scale);
    }
  }
  if (s.take) check(s.take.ref.covenantId, s.take.scale);
  return out;
}

/**
 * Adds the delivery the order is signed with, a signed term. It names the mode, and for a top-up
 * its tip and threshold. An if-done entry's delivery goes on its exit.
 */
function withDelivery(sum: SummaryOrder, o: DecodedOrder, hints: TokenHint[]): SummaryOrder {
  const dl = o.delivery;
  if (!dl) return sum;
  const d = o.description;
  const token = d.pair ? (d.pair.side === 'buy' ? d.pair.base.covId : d.pair.quote.covId) : d.tokenCovId;
  const hint = hints.find((h) => h.covenantId === token);
  const mode = dl.mode === 1n ? 'collected' : dl.mode === 2n ? 'topup' : 'each';
  const rows: SummaryOrder['rows'] = [{ id: 'delivery', label: t('signer.delivery.label'), value: t(`signer.delivery.${mode}`) }];
  if (dl.mode === 2n) {
    rows.push({ id: 'topupTip', label: t('signer.delivery.tip'), value: `${formatKas(dl.topupTip)} KAS` });
    rows.push({ id: 'recvThreshold', label: t('signer.delivery.threshold'), value: formatTokenAmount(dl.recvThreshold, hint ? hint.decimals : RAW_TOKEN_DECIMALS) });
  }
  if (dl.exit && sum.exit) return { ...sum, exit: { ...sum.exit, rows: [...sum.exit.rows, ...rows] } };
  return { ...sum, rows: [...sum.rows, ...rows] };
}

/**
 * Builds the compact lines of a decoded request. They are:
 * - KAS and every token, net for this key,
 * - KAS locked into orders and paid to other keys,
 * - the fee.
 */
export function summaryLines(s: SigningSummary, appHints: TokenHint[], kob?: KobWasm, terms = false, built?: BuiltTx): SignerSummary {
  const hints = withOrderScales(s, appHints);
  const lines: SummaryLine[] = [];
  const k = s.net.kas;
  const kas = (v: bigint) => `${formatKas(v)} KAS`;
  lines.push({ label: t('signer.net.kas'), value: signed(k.walletDelta, kas), dir: k.walletDelta < 0n ? 'out' : k.walletDelta > 0n ? 'in' : null });
  // Every token figure carries its unit, as in "−2 EXKCC". A token nothing names has none to
  // carry. That is an assumed one, whose amount is raw and whose label is its covenant id.
  const unitOf = (hint: TokenHint | undefined): string => (hint && hint.source !== 'assumed' && hint.ticker ? ` ${tickerUnit(hint.ticker, hint.covenantId)}` : '');
  // Whom a payment or transfer to other keys goes to: each WHOLE key (never shortened in the
  // middle, founder 2026-11-12: a chosen head and tail is cheap to grind), and an output to a
  // script the decoder could not read as such (R8F2-I1).
  const to = (outs: readonly DecodedOutput[], amount: string): string => {
    const keys = [...new Set(outs.flatMap((o) => (o.recipient ? [t('signer.shape.key', { key: o.recipient })] : [])))];
    if (outs.some((o) => !o.recipient)) keys.push(t('signer.net.unknownScript'));
    return keys.length ? t('signer.net.toKeysValue', { amount, keys: keys.join(', ') }) : amount;
  };
  for (const tok of s.net.tokens) {
    const hint = hints.find((h) => h.covenantId === tok.ref.covenantId);
    const label = tokenName(tok.ref.covenantId, hint, terms);
    const amount = (v: bigint) => `${formatTokenAmount(v, hint ? hint.decimals : RAW_TOKEN_DECIMALS)}${unitOf(hint)}`;
    if (tok.walletDelta !== 0n) lines.push({ label, value: signed(tok.walletDelta, amount), dir: tok.walletDelta < 0n ? 'out' : 'in' });
    if (tok.toOthers > 0n) lines.push({ label: t('signer.net.tokensToOthers', { token: label }), value: to(s.outputs.filter((o) => o.kind === 'token-out' && o.token?.ref.covenantId === tok.ref.covenantId), amount(tok.toOthers)), dir: 'out', mono: true });
    // An unlock says what the wallet's creator lock held. The line above says it comes to this key.
    if (tok.unlocked > 0n) lines.push({ label: t('signer.net.unlocked', { token: label }), value: amount(tok.unlocked), dir: null });
  }
  const merged = mergedTokens(s);
  for (const m of merged ?? []) {
    const hint = hints.find((h) => h.covenantId === m.ref.covenantId);
    const amount = `${formatTokenAmount(m.amount, hint ? hint.decimals : RAW_TOKEN_DECIMALS)}${unitOf(hint)}`;
    lines.push({ label: t('signer.net.merged', { token: tokenName(m.ref.covenantId, hint, terms) }), value: t('signer.net.mergedValue', { inCount: m.inCount, outCount: m.outCount, amount }), dir: null });
  }
  if (k.locked.total > 0n) lines.push({ label: t('signer.net.locked'), value: kas(k.locked.total), dir: null });
  if (k.released > 0n) lines.push({ label: t('signer.net.released'), value: kas(k.released), dir: null });
  // Which of the wallet's orders a cancel closes, by 8 + 8 hex of its covenant id as the token is
  // named. A hostile indexer can only swap one of the user's own orders for another, and this
  // makes the swap visible.
  for (const sp of s.spends.filter((x) => x.makerIsWallet && x.covenantId && (x.action === 'cancel' || x.action === 'refund' || x.action === 'sweep')).slice(0, MAX_CLOSED_LINES)) {
    // the id is the app's word (no signature covers an input's covenant id); the terms are the
    // order's bound state (R9FB2-4)
    const d = sp.description;
    // the whole covenant id (never shortened in the middle)
    const value = terms && d ? t('signer.net.orderTerms', { id: sp.covenantId!, terms: `${d.kind}, ${d.side}` }) : sp.covenantId!;
    lines.push({ label: t(sp.action === 'sweep' ? 'signer.net.sweeps' : sp.retired ? 'signer.net.closesRetired' : 'signer.net.closes'), value, dir: null, mono: true });
  }
  // A claim (bid-delivery-spec §7) says what the order bought, where it goes and the carrier added
  // to the order.
  for (const sp of s.spends.filter((x) => x.makerIsWallet && x.covenantId && x.bought && (x.action === 'claim' || x.bought.input !== null))) {
    const b = sp.bought!;
    const token = sp.description?.pair ? (sp.description.pair.side === 'buy' ? sp.description.pair.base.covId : sp.description.pair.quote.covId) : sp.description?.tokenCovId;
    if (!token) continue;
    const hint = hints.find((h) => h.covenantId === token);
    const amount = (v: bigint) => `${formatTokenAmount(v, hint ? hint.decimals : RAW_TOKEN_DECIMALS)}${unitOf(hint)}`;
    if (sp.action === 'claim') lines.push({ label: t('signer.net.claims'), value: longId(sp.covenantId!), dir: null });
    lines.push({ label: t('signer.net.claimed', { token: tokenName(token, hint, terms) }), value: amount(b.amount), dir: null });
    if (b.toYou !== undefined && b.toOrder !== undefined) {
      const to = b.toOrder === 0n ? t('signer.net.claimToYou') : b.toYou === 0n ? t('signer.net.claimToOrder') : t('signer.net.claimSplit', { order: amount(b.toOrder), you: amount(b.toYou) });
      lines.push({ label: t('signer.net.claimTo'), value: to, dir: null });
    }
    const cont = s.orders.find((o) => o.claimedFrom === sp.input);
    const was = s.inputs[sp.input]?.amount;
    if (cont && was !== undefined && cont.value > was) lines.push({ label: t('signer.net.claimCarrier'), value: kas(cont.value - was), dir: null });
  }
  if (k.toOthers > 0n) lines.push({ label: t('signer.net.toOthers'), value: to(s.outputs.filter((o) => ['payment-out', 'token-out', 'unknown-covenant', 'unknown'].includes(o.kind)), kas(k.toOthers)), dir: 'out', mono: true });
  // A take says the resting orders it fills at their own prices, and the average price.
  if (s.take) {
    const tk = s.take;
    const hint = hints.find((h) => h.covenantId === tk.ref.covenantId);
    const unit = hint && hint.source !== 'assumed' && hint.ticker ? tickerUnit(hint.ticker, hint.covenantId) : longId(tk.ref.covenantId);
    lines.push({ label: t('signer.take.fills'), value: t('signer.take.fillsValue', { orders: tk.fills.length, amount: `${formatTokenAmount(tk.amount, hint ? hint.decimals : RAW_TOKEN_DECIMALS)} ${unit}` }), dir: null });
    if (tk.avgPrice !== null) lines.push({ label: t('signer.take.price'), value: t('signer.take.priceValue', { price: formatKas(tk.avgPrice), unit }), dir: null });
  }
  const fromTokens = freedFromTokens(s);
  if (fromTokens > 0n) lines.push({ label: t('signer.net.fromTokens'), value: kas(fromTokens), dir: null });
  const registry = hintRegistry(hints, terms);
  const orders = s.orders.filter((o) => o.sweptFrom === undefined && o.claimedFrom === undefined).map((o) => {
    const chain = orderSummary(o.description, { value: o.value, locked: o.locked, deadline: o.deadline, fills: marketFillsOf(o, kob) }, hints, registry, terms);
    return withDelivery(flippedSummary(o, hints.find((h) => h.covenantId === o.description.tokenCovId), kob, chain, terms) ?? chain, o, hints);
  });
  const found: { code: string }[] = [...s.blocking.filter((b) => !SHOWN_AS_LINES.has(b.code)), ...(terms ? scaleMismatches(s, hints) : [])];
  // The decoder's info findings are said in the frame too (a swept order's KAS, strays swept), as
  // the app's confirmation shows them (R9FB2-9).
  if (terms) for (const f of s.info) lines.push({ label: t('signer.info'), value: issueText(f), dir: null });
  // the frame signs only its closed list of shapes, judged on bound data (signer/shapes.ts)
  if (terms && kob && built) {
    // token amounts as the token lines above name them: scaled with their stated source, else in
    // smallest units of the whole id
    const fmtToken = (cov: Hex, v: bigint): string => {
      const hint = hints.find((h) => h.covenantId === cov);
      // with no ticker the amount carries the token's own label, as the token line above names it
      if (hint && hint.source !== 'assumed') return hint.ticker ? `${formatTokenAmount(v, hint.decimals)}${unitOf(hint)}` : `${formatTokenAmount(v, hint.decimals)} (${tokenName(cov, hint, true)})`;
      return t('signer.shape.rawUnits', { amount: String(v), token: cov });
    };
    const v = frameShape(s, built, s.maker, kob, fmtToken);
    if (!v.ok) {
      found.push({ code: 'frame-shape' });
      lines.push({ label: t('signer.shape.refused'), value: v.reason, dir: null });
    } else {
      // a take's claimed fills and price give way to the bound lines of its shape
      if (v.shape === 'take') for (let i = lines.length - 1; i >= 0; i--) if (lines[i]!.label === t('signer.take.fills') || lines[i]!.label === t('signer.take.price')) lines.splice(i, 1);
      lines.push(...v.lines);
    }
  }
  // The maker's cancel of an order under an older contract version, from the cancel-only table, is
  // titled as such, with what returns to the wallet.
  const retiredCancel = (s.kind === 'cancel' || s.kind === 'cancel-position') && s.spends.some((x) => x.retired);
  return { title: t(merged ? 'signer.kind.merge' : retiredCancel ? 'signer.kind.cancelRetired' : (TITLE[s.kind] ?? TITLE.other)), lines, orders, fee: kas(k.fee), ...(s.inputs.length > s.signatures ? { unsignedKas: true } : {}), ...problemsOf(found), decoded: s };
}

/**
 * Decodes `built` for `maker`, the signer's own key, and summarises it. It throws when the decoder
 * does.
 *
 * `locks` is the app's hint of this key's creator locks, so that a lock coin to one is recognised.
 * The decoder takes only a lock of this key that is open by `now`, the frame's clock.
 */
export function summarize(kob: KobWasm, built: BuiltTx, maker: Hex, hints: TokenHint[], locks?: readonly LockHint[], now?: number): SignerSummary {
  return summaryLines(decodeSigning({ kob, built, maker, registry: null, ...(locks?.length ? { locks } : {}), ...(now !== undefined ? { now } : {}) }), hints, kob, true, built);
}

/**
 * Summarises a token creation (KCC-23) as one request. It is the preparation `prep` and the genesis
 * `built` that spends its output 0.
 *
 * These are checked here, from the transactions alone:
 * - The genesis spends exactly the preparation's output 0.
 * - Every input of the preparation is a P2PK coin of this key and is signed.
 * - Every output of the preparation pays this key by P2PK with no covenant. These are the genesis
 *   funding and the change.
 * - The preparation's payload is a KCC-23 declaration.
 *
 * With the app's `claim` of the genesis token outputs, each claimed output must carry exactly the
 * script of its claimed state in the claimed program, and be bound to the one new covenant. The
 * frame then names the supply and what this key receives, and those outputs are no longer
 * "unknown".
 *
 * The summary is the genesis's decoded effect, with the declared name, ticker and decimals first.
 * The KAS line and the fee cover both transactions. It throws when a check fails, so the request is
 * refused and never shown.
 */
export function creationSummary(kob: KobWasm, prep: BuiltTx, built: BuiltTx, maker: Hex, hints: TokenHint[], claim?: GenesisClaim): SignerSummary {
  const own = `000020${maker}ac`;
  if (prep.tx.version !== 1 || prep.covenants.length > 0 || !spendsPreparation(prep, built)) throw new Error('not a creation');
  prep.tx.inputs.forEach((inp, i) => {
    const plan = prep.plans[i];
    if (!plan || plan.kind !== 'p2pk' || plan.pubkey !== maker || inp.utxo.scriptPublicKey !== own || !prep.sign.some((r) => r.inputIndex === i)) throw new Error('foreign input');
  });
  if (prep.tx.outputs.some((o) => o.covenant || o.scriptPublicKey !== own)) throw new Error('pays another key');
  const decl = decodeDeclaration(kob, prep.tx.payload).token;
  // A creator lock of this key that the declaration states, its terms parsed by kob-wasm. The
  // outputs its hash owns are this key's share, locked. An automatic lock's script is derived from
  // the terms and the program the claimed token outputs are checked under (below), and its genesis
  // coin, one plain output of AUTO_COIN to its address, is this key's KAS, locked until the release.
  const program = claim && isTokenProgram(claim.program) ? claim.program : null;
  const declaredLock = lockOfDeclaration(kob, decl, program);
  const lock = declaredLock && declaredLock.key === maker ? declaredLock : null;
  const prepIn = prep.tx.inputs.reduce((a, i) => a + BigInt(i.utxo.amount), 0n);
  const prepOut = prep.tx.outputs.reduce((a, o) => a + BigInt(o.value), 0n);
  const prepFee = prepIn - prepOut;
  if (prepFee < 0n || prepFee !== BigInt(prep.fee.fee)) throw new Error('fee');
  const g = summaryLines(
    decodeSigning({ kob, built, maker, registry: null, ...(lock?.kind === 'auto' && program ? { autoLocks: [{ until: lock.until, key: lock.key, program }] } : {}) }),
    hints, kob, true,
  );
  // The claimed token outputs, each re-derived from its state. An output that does not match
  // refuses the request.
  const verified = new Set<number>();
  let supply = 0n;
  let toMaker = 0n;
  let lockedToMaker = 0n;
  // A claimed curve is the genesis's sell order of this key. Its output must:
  // - be exactly the script of its unbound state, alone in its own covenant group,
  // - be made by this key, of the claimed program and commitment,
  // - have as custody the claimed token output it owns, with exactly its amount.
  let curveLines: SummaryLine[] = [];
  if (claim?.curve) {
    const c = claim.curve;
    const st = c.state as { kind?: unknown; state?: Record<string, unknown> };
    const s = st.state ?? {};
    const out = built.tx.outputs[c.index];
    const group = built.covenants.find((g) => g.covenantId === c.covenantId);
    const custody = claim.outputs.find((o) => o.owner === c.covenantId && o.ownerScheme === 4);
    const tpl = kob.templates().find((x) => x.name === claim.program);
    if (
      st.kind !== 'KobCurveAsk' || !out || !group || group.outputs.length !== 1 || group.outputs[0] !== c.index || out.covenant?.covenantId !== c.covenantId ||
      kob.scriptPublicKey(c.state as never) !== out.scriptPublicKey || s.maker !== maker || s.tokenCovId !== '0'.repeat(64) || !tpl || s.tokenTplHash !== tpl.hash ||
      s.extensionCommitment !== claim.extensionCommitment || !custody || custody.borrowScheme !== 0 || String(s.amountLeft) !== custody.amount || String(s.total) !== custody.amount
    ) {
      throw new Error('claim');
    }
    verified.add(c.index);
    const table = (s.table as string[]).map((p) => BigInt(p));
    // Stricter than the protocol. It needs:
    // - a table that never falls,
    // - a tip below every level,
    // - levels that hold the amount,
    // - the scale of the declared decimals.
    if (decl.decimals === null || curveProblems(s as never, decl.decimals).length > 0) throw new Error('claim');
    try {
      kob.checkNumbers(c.state as never);
    } catch {
      throw new Error('claim');
    }
    const n = curveNumbersOfState(s as never);
    const tip = BigInt(String(s.tip));
    const units = (v: bigint) => `${formatTokenAmount(v, decl.decimals!)} ${decl.symbol ?? ''}`.trim();
    curveLines = [
      { label: t('signer.declare.curve'), value: t('signer.declare.curveValue', { levels: table.length, first: formatKas(table[0] ?? 0n), last: formatKas(table[table.length - 1] ?? 0n) }), dir: null },
      { label: t('signer.declare.curveLevelSize'), value: units(BigInt(String(s.levelSize))), dir: null },
      { label: t('signer.declare.curveAverage'), value: `${formatKas(n.average)} KAS`, dir: null },
      { label: t('signer.declare.curveTotal'), value: `${formatKas(n.total)} KAS`, dir: null },
      { label: t('signer.declare.curveTip'), value: `${formatKas(tip)} KAS`, dir: null },
      { label: t('signer.declare.curveMinFill'), value: units(BigInt(String(s.minFill))), dir: null },
      { label: t('signer.declare.curveTable'), value: tablePreview(table, (p) => formatKas(p)), dir: null },
    ];
  }
  if (claim) {
    const tokenGroups = built.covenants.filter((g) => !claim.curve || g.covenantId !== claim.curve.covenantId);
    const cov = tokenGroups.length === 1 && built.covenants.length === (claim.curve ? 2 : 1) ? tokenGroups[0]! : null;
    if (!cov || cov.outputs.length !== claim.outputs.length || claim.outputs.some((o) => !cov.outputs.includes(o.index))) throw new Error('claim');
    for (const o of claim.outputs) {
      const out = built.tx.outputs[o.index];
      const state = { amount: o.amount, owner: o.owner, owner_scheme: o.ownerScheme, borrow_scheme: o.borrowScheme, borrow_guard: o.borrowGuard, extension_commitment: claim.extensionCommitment };
      if (!out || out.covenant?.covenantId !== cov.covenantId || kob.tokenScriptPublicKey(claim.program as never, state as never) !== out.scriptPublicKey) throw new Error('claim');
      verified.add(o.index);
      supply += BigInt(o.amount);
      if (o.owner === maker && o.ownerScheme === 0) toMaker += BigInt(o.amount);
      else if (lock && o.ownerScheme === 3 && o.owner === lock.authority && o.borrowScheme === 0) lockedToMaker += BigInt(o.amount);
    }
  }
  // The custody of this key's own curve is not "to others".
  const onCurve = claim?.curve ? claim.outputs.filter((o) => o.owner === claim.curve!.covenantId && o.ownerScheme === 4).reduce((a, o) => a + BigInt(o.amount), 0n) : 0n;
  // The genesis spends this key's preparation output. So the net of the KAS line is the
  // creation's, less the preparation's fee.
  const k = g.decoded.net.kas;
  const delta = k.walletDelta - prepFee;
  const lockCoin = g.decoded.outputs.filter((o) => o.kind === 'lock-coin').reduce((a, o) => a + o.value, 0n);
  const kas = (v: bigint) => `${formatKas(v)} KAS`;
  const lines: SummaryLine[] = [
    { label: t('signer.declare.name'), value: decl.name ?? '', dir: null },
    { label: t('signer.declare.symbol'), value: decl.symbol ?? '', dir: null },
    { label: t('signer.declare.decimals'), value: decl.decimals === null ? '' : String(decl.decimals), dir: null },
    { label: t('signer.declare.steps'), value: t('signer.declare.stepsValue'), dir: null },
    // The program and the extension commitment of the token output scripts, as the outputs were
    // checked against them. Without them, two creations that differ only there would look the same.
    ...(claim
      ? [
          { label: t('signer.declare.program'), value: claim.program, dir: null },
          { label: t('signer.declare.extension'), value: longId(claim.extensionCommitment), dir: null },
        ]
      : []),
    ...(claim && decl.decimals !== null
      ? [
          { label: t('signer.declare.supply'), value: `${formatTokenAmount(supply, decl.decimals)} ${decl.symbol ?? ''}`.trim(), dir: null },
          { label: t('signer.declare.toYou'), value: `+${formatTokenAmount(toMaker, decl.decimals)} ${decl.symbol ?? ''}`.trim(), dir: 'in' as const },
          ...(onCurve > 0n ? [{ label: t('signer.declare.onCurve'), value: `${formatTokenAmount(onCurve, decl.decimals)} ${decl.symbol ?? ''}`.trim(), dir: null }] : []),
          ...(lockedToMaker > 0n ? [{ label: t(lock!.kind === 'auto' ? 'signer.declare.lockedAuto' : 'signer.declare.locked', { date: formatRowDate(Number(lock!.until)) }), value: `${formatTokenAmount(lockedToMaker, decl.decimals)} ${decl.symbol ?? ''}`.trim(), dir: null }] : []),
          ...(supply > toMaker + onCurve + lockedToMaker ? [{ label: t('signer.declare.toOthers'), value: `${formatTokenAmount(supply - toMaker - onCurve - lockedToMaker, decl.decimals)} ${decl.symbol ?? ''}`.trim(), dir: 'out' as const }] : []),
          ...curveLines,
        ]
      : []),
    { label: t('signer.net.kas'), value: signed(delta, kas), dir: delta < 0n ? 'out' : delta > 0n ? 'in' : null },
    // an automatic lock's coin: this key's KAS, locked until the release (not an order's)
    ...(lockCoin > 0n ? [{ label: t('signer.declare.lockCoin'), value: kas(lockCoin), dir: null }] : []),
    ...g.lines.slice(1).filter((l) => !(lockCoin > 0n && l.label === t('signer.net.locked'))),
  ];
  // The token outputs checked against the claim are not "unknown scripts". Every other finding
  // still counts.
  const problems = g.decoded.blocking.filter((b) => !SHOWN_AS_LINES.has(b.code) && !(b.code === 'output-unknown' && b.output !== undefined && verified.has(b.output)));
  return { ...g, title: t('signer.kind.createToken'), lines, fee: kas(k.fee + prepFee), ...problemsOf(problems) };
}

/** A session's validity as the frame says it, such as "4 min 30 s". It never rounds up. */
function durationWords(ms: number): string {
  const s = Math.floor(ms / 1000);
  const min = Math.floor(s / 60);
  const sec = s % 60;
  return sec === 0 ? t('signer.session.minutes', { min }) : min === 0 ? t('signer.session.seconds', { sec }) : t('signer.session.minSec', { min, sec });
}

/**
 * Builds the screen of a one-approval merge session (session.ts).
 *
 * It shows what the frame derived from the plan it decoded, never what the app says:
 * - the token, by 8 + 8 hex of its covenant id,
 * - the UTXOs merged and left, and the token amount, which is unchanged,
 * - the most transactions,
 * - the most network fee in all, and the planned fee,
 * - how long the approval lasts.
 *
 * `decoded` is the first link's decoding, because the summary type carries one.
 */
export function sessionSummary(terms: Pick<SessionTermsView, 'covenantId' | 'links' | 'utxos' | 'outputs' | 'amount' | 'feeMax' | 'plannedFee' | 'ttlMs'>, hints: TokenHint[], decoded: SigningSummary): SignerSummary {
  const hint = hints.find((h) => h.covenantId === terms.covenantId);
  const unit = hint && hint.source !== 'assumed' && hint.ticker ? ` ${tickerUnit(hint.ticker, hint.covenantId)}` : '';
  // Nothing this frame can check states the decimals. No pinned table lists the token, and no
  // declaration or convention was hinted. So the amount is in base units and says so, never as a
  // bare integer.
  const raw = !hint || hint.source === 'assumed';
  const amount = raw ? t('signer.session.rawAmount', { amount: formatTokenAmount(terms.amount, RAW_TOKEN_DECIMALS) }) : `${formatTokenAmount(terms.amount, hint.decimals)}${unit}`;
  const lines: SummaryLine[] = [
    { label: t('signer.session.token'), value: tokenName(terms.covenantId, hint, true), dir: null },
    { label: t('signer.session.what'), value: t('signer.session.whatValue', { utxos: terms.utxos, outputs: terms.outputs, amount }), dir: null },
    { label: t('signer.session.txs'), value: t('signer.session.txsValue', { count: terms.links.length }), dir: null },
    { label: t('signer.session.valid'), value: t('signer.session.validValue', { time: durationWords(terms.ttlMs) }), dir: null },
  ];
  const title = hint && hint.source !== 'assumed' && hint.ticker ? t('signer.session.title', { ticker: tickerUnit(hint.ticker, hint.covenantId) }) : t('signer.session.titleToken');
  return {
    title, lines, orders: [],
    fee: t('signer.session.feeValue', { max: formatKas(terms.feeMax), planned: formatKas(terms.plannedFee) }),
    problems: 0, problemCodes: [], decoded,
  };
}

/** The terms the session screen shows (session.ts `SessionTerms`). */
interface SessionTermsView {
  covenantId: Hex;
  links: readonly unknown[];
  utxos: number;
  outputs: number;
  amount: bigint;
  feeMax: bigint;
  plannedFee: bigint;
  ttlMs: number;
}
