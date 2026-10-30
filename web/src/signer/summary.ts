// The signer's own short summary of a signing request: decoded INSIDE the signer frame with kob-wasm (`decodeSigning`, the decoder of the
// app's confirmation screen), from the transaction itself, never from what the app says about it: the net effect on this key, the fee, and the
// terms of every order the transaction creates or amends (side, type, amount, prices, stops, timing: the rows of the confirmation screen), so a
// changed price or stop cannot pass behind an unchanged net effect. A plain order of a KAS market the app shows inverted (KAS/TOKEN) is said
// first as the user placed it (sell KAS for tokens), its chain-side terms below; every price reads "X per Y", never a pair notation.
import { decodeSigning, type DecodedOrder, type SigningKind, type SigningSummary, type TokenRef } from '../kob/decode';
import { spendsPreparation } from '../kob/issue';
import { decodeDeclaration } from '../kob/kcc23';
import { baseKind, isMultiFillMarket, type OrderDescription } from '../kob/order-facts';
import { DEFAULT_BID_FILLS } from '../kob/orders/common';
import { longId, type TokenInfo, type TokenRegistry } from '../kob/registry';
import type { BuiltTx, Hex } from '../kob/types';
import { formatKas, formatTokenAmount } from '../kob/units';
import type { KobWasm } from '../kob/wasm';
import { ASSUMED_TOKEN_DECIMALS, formatDateTime, t } from '../i18n';
import { auctionSeconds, orderRows, orderTitle, orderTypeCode, type Fmt, type MarketFillsFacts } from '../ui/confirm/confirm-model';
import type { GenesisClaim, TokenHint } from './protocol';
import { div, kasWords, tokensPerKasWords, tokenWords, type Rounding } from './words';

export interface SummaryLine {
  label: string;
  value: string;
  /** 'out' = leaves this key, 'in' = comes to it */
  dir: 'in' | 'out' | null;
}

/** One order the transaction creates or amends, described from its decoded state (an if-done entry: its committed exit as `exit`). */
export interface SummaryOrder {
  title: string;
  /** the token of the order (ticker hint and the covenant id's 8 + 8 hex) */
  token: string;
  /**
   * `id`: the confirmation row it comes from (confirm-model.ts `orderRows`); `daa`: the DAA score of a start or an expiry the row names (said
   * from the clock as "in 28 s" where one is known)
   */
  rows: { id?: string; label: string; value: string; daa?: string }[];
  exit: SummaryOrder | null;
}

/** The count and the distinct codes of the findings the frame shows as one problems note. */
const problemsOf = (found: readonly { code: string }[]): { problems: number; problemCodes: string[] } => ({ problems: found.length, problemCodes: [...new Set(found.map((b) => b.code))] });

export interface SignerSummary {
  title: string;
  lines: SummaryLine[];
  /** the orders the transaction creates or amends (a sweep in place keeps its order's terms and is not listed) */
  orders: SummaryOrder[];
  fee: string;
  /** the transaction spends inputs the wallet does not sign (an order's own UTXOs, custodies, strays): their KAS is the node's word, which the signature does not cover (audit round 2, S1) */
  unsignedKas?: boolean;
  /** decoder findings that block signing on the app's confirmation screen (shown as one line here, never hidden) */
  problems: number;
  /** the codes of those findings (the frame names them, and Approve stays disabled while there are any) */
  problemCodes: string[];
  /** the decoding it was made from (a re-timed request is compared with the approved one on it: retime.ts) */
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
  other: 'signer.kind.other',
};

/** blocking for the app's confirmation screen unless the user asked for them (it knows, the frame does not): shown here as "to other keys" lines */
const SHOWN_AS_LINES = new Set<string>(['payment-out', 'transfer-out']);

/**
 * A token's label in the frame: the app's ticker hint (a label only: the app chooses it) next to 8 + 8 hex of the covenant id the transaction
 * itself carries (a 4 + 4 fragment is 32 bits, which a token made to match it can copy).
 */
const tokenName = (covenantId: Hex, hint: TokenHint | undefined, frame = false): string => {
  // no label from the app: named by its covenant id, its amounts on the assumed 8 decimals (never "base units")
  if (!hint) return t('signer.net.tokenAssumed', { id: longId(covenantId), decimals: ASSUMED_TOKEN_DECIMALS });
  // a token the registry does not list (app/token-display.ts): its issued ticker, else its covenant id; marked
  if (hint.source === 'assumed') return t('signer.net.tokenAssumed', { id: longId(covenantId), decimals: hint.decimals });
  if (hint.source === 'program') return t('signer.net.tokenProgram', { id: longId(covenantId), decimals: hint.decimals, source: hint.convention ?? '' });
  if (hint.source === 'issued') return t('signer.net.tokenIssued', { ticker: hint.ticker, id: longId(covenantId) });
  if (hint.source === 'declared') return t('signer.net.tokenDeclared', { ticker: hint.ticker, id: longId(covenantId) });
  // a registry token: in the signer frame (`frame`) the ticker and decimals are the app's word, which the frame says; the app's own confirmation
  // is the app
  if (!frame) return `${hint.ticker} (${longId(covenantId)})`;
  return t('signer.net.tokenListed', { ticker: hint.ticker, id: longId(covenantId), decimals: hint.decimals });
};

/** A TokenRef from the app's hint (ticker and decimals are labels; without a hint amounts stay in base units). */
function refOfHint(covenantId: Hex, hints: TokenHint[], frame = false): TokenRef {
  const h = hints.find((x) => x.covenantId === covenantId);
  // an assumed token's amounts carry its covenant id as their unit
  const ticker = h ? (h.ticker || longId(covenantId)) : null;
  return { covenantId, ticker, decimals: h?.decimals ?? null, display: tokenName(covenantId, h, frame), inRegistry: false, tradable: false };
}

/** The hints as the confirmation model's registry lookup (a pair order's quote token B): ticker and decimals only. */
function hintRegistry(hints: TokenHint[]): TokenRegistry {
  const byCovenantId = new Map(hints.map((h) => [h.covenantId, { covenantId: h.covenantId, ticker: h.ticker, decimals: h.decimals, tradable: false } as unknown as TokenInfo]));
  return { network: '', templates: [], templateChecks: [], tokens: [], byCovenantId };
}

/**
 * Rows whose detail carries a signed term the value does not: the seconds of an auction (how fast the price falls to the bound), of the auction
 * after a stop entry's trigger, and the sizes a pair trigger needs to touch. The signer frame (`summarize`) shows no detail line of its own, so it
 * says them in the value (audit 2026-10-30 G2); the app's confirmation shows the detail beside the value as before.
 */
const SIGNED_TERM_DETAIL = new Set(['auction', 'entryStop', 'pairTrigger']);

function orderSummary(d: OrderDescription, extra: Parameters<typeof orderRows>[1], hints: TokenHint[], registry: TokenRegistry, terms = false): SummaryOrder {
  const f: Fmt = { tr: t, ref: refOfHint(d.tokenCovId, hints, terms), clock: null, time: (unix) => formatDateTime(unix), registry };
  const exit = d.entry?.exit ? orderSummary(d.entry.exit, { value: null, locked: null, deadline: null }, hints, registry, terms) : null;
  return {
    title: orderTitle(d, extra.deadline, f),
    token: f.ref.display,
    // a price of a token whose decimals the frame does not know (no hint: an unlisted token's first order) is per `scale` base units:
    // the frame keeps that detail, as amounts there are base units too (TN10 2026-10-15 B-13: "1000 base units" at "0.5 KAS")
    rows: orderRows(d, extra, f).map((r) => ({ id: r.id, label: r.label, value: r.detail && ((f.ref.decimals === null && /^decimals assumed/.test(r.detail)) || (terms && SIGNED_TERM_DETAIL.has(r.id))) ? `${r.value} (${r.detail})` : r.value, ...timeOf(r, d) })),
    exit: exit ? { ...exit, title: t('confirm.exitOf', { title: exit.title }) } : null,
  };
}

const signed = (v: bigint, text: (abs: bigint) => string) => (v < 0n ? `−${text(-v)}` : `+${text(v)}`);

/** The DAA score a start or expiry row says as a DAA score (not an if-done exit's open expiry). */
function timeOf(r: { id: string; value: string }, d: OrderDescription): { daa?: string } {
  const daa = r.id === 'activeFrom' ? d.activeFrom : r.id === 'expiry' ? d.expiryDaa : 0n;
  return daa > 0n && r.value === t('confirm.f.daa', { daa: daa.toString() }) ? { daa: daa.toString() } : {};
}

/** The chain-side price rows of a plain order, said in the view's terms instead on the flipped view. */
const CHAIN_PRICE_ROWS = new Set(['price', 'allIn', 'auction']);

/** The view word of each plain order type, for the title of an order on the flipped view ("Sell KAS · market (auction)"). */
const FLIP_TYPE: Record<string, string> = {
  limit: 'signer.flip.type.limit', limitDay: 'signer.flip.type.limitDay', market: 'signer.flip.type.market', marketFok: 'signer.flip.type.marketFok',
  ioc: 'signer.flip.type.ioc', fok: 'signer.flip.type.fok', twap: 'signer.flip.type.twap', dca: 'signer.flip.type.dca', dutch: 'signer.flip.type.dutch',
  rising: 'signer.flip.type.rising', marketFills: 'signer.flip.type.marketFills',
};

/**
 * A plain order of a KAS market (KobAsk / KobBid) on the view the app shows inverted (KAS/TOKEN, the hint's `inverted`): said as the user placed
 * it, still from the decoded order. The chain buys tokens where the user sells KAS: "Sell KAS · market (auction)", what is sold and received,
 * the prices in tokens per KAS (rounded against the user); the chain-side terms (budget, escrow, minimum fill) follow as plainly labelled rows.
 * Null for every other order, a native view, or a token the app gave no ticker for.
 */
function flippedSummary(o: DecodedOrder, hint: TokenHint | undefined, kob: KobWasm | undefined, chain: SummaryOrder, terms = false): SummaryOrder | null {
  const d = o.description;
  const kind = baseKind(d.kind);
  if (!hint?.inverted || d.pair || d.entry || (kind !== 'KobAsk' && kind !== 'KobBid') || d.scale <= 0n || d.price <= 0n) return null;
  const T = hint.ticker || longId(d.tokenCovId);
  const dec = hint.decimals;
  const scale = d.scale;
  // a price that decays to 0 is a decay (the bound is 0 KAS, and it is said so: audit 2026-10-30 G6)
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
    // the chain buys a fixed amount of tokens for at most its budget: the user sells up to that much KAS for those tokens; a higher KAS
    // price per token is fewer tokens per KAS, worse for the user (rounded down)
    side = 'sell';
    const st = o.state.state as unknown as { reserve?: string; deliveryCarrier?: string };
    const { amount: n, fills } = bidAmountOf(o, kob, scale, end + d.tip);
    const budget = o.value - (fills > 0n ? fills - 1n : 0n) - fills * BigInt(st.deliveryCarrier ?? '0') - BigInt(st.reserve ?? '0');
    rows.push({ label: t('signer.flip.youSell'), value: t('signer.flip.upTo', { amount: kasT(budget > 0n ? budget : 0n, 'up') }) });
    // a multi-fill market order can deliver more than the amount its budget was sized for (the carrier of a fill that does not happen buys tokens)
    const power = kob && isMultiFillMarket(d) ? maxReceiveOf(o, kob) : undefined;
    rows.push({ label: t('signer.flip.youReceive'), value: power !== undefined && power > n ? t('signer.flip.upTo', { amount: tokT(power, 'down') }) : tokT(n, 'down') });
    rows.push({ label: t(decays ? 'signer.flip.expected' : 'signer.flip.price'), value: per(start, 'down') });
    if (decays || d.tip > 0n) rows.push({ label: t('signer.flip.worst'), value: per(end + d.tip, 'down') });
  } else {
    // the chain sells a fixed amount of tokens for at least its price: the user pays those tokens and buys the KAS they fetch; a lower KAS
    // price per token is more tokens per KAS, worse for the user (rounded up)
    side = 'buy';
    const n = d.tokenAmount ?? d.amountLeft ?? 0n;
    const kasAt = (price: bigint) => div(n * (price > d.tip ? price - d.tip : 0n), scale, 'down');
    rows.push({ label: t('signer.flip.youPay'), value: tokT(n, 'nearest') });
    rows.push({ label: t('signer.flip.youReceive'), value: t('signer.flip.atLeast', { amount: kasT(kasAt(end), 'down') }) });
    rows.push({ label: t(decays ? 'signer.flip.expected' : 'signer.flip.price'), value: per(start, 'up') });
    if (decays || d.tip > 0n) rows.push({ label: t('signer.flip.worst'), value: per(end > d.tip ? end - d.tip : 1n, 'up') });
  }
  // how fast the price falls is a signed term the flipped prices do not say (audit 2026-10-30 G2)
  const seconds = auctionSeconds(d);
  if (terms && seconds !== null) rows.push({ label: t('confirm.f.auction'), value: t('confirm.f.auctionValue', { seconds }) });
  return {
    title: t('signer.flip.title', { side: t(side === 'sell' ? 'common.side.sell' : 'common.side.buy'), type: typeName }),
    // the token, and the order as the chain holds it: its terms follow
    token: t('signer.flip.onChain', { token: chain.token, order: chain.title }),
    // the chain's prices are said above in the view's terms: its other terms follow
    rows: [...rows, ...chain.rows.filter((r) => !CHAIN_PRICE_ROWS.has(r.id ?? ''))],
    exit: null,
  };
}

/**
 * The amount a plain bid's escrow was made for and the deliveries it funds: the largest `n` whose escrow (the covenant's `bidEscrow` of `n` in
 * `f` fills) the order UTXO holds, `f` as the planner budgets it (one for IOC / FOK / market, one per slice of a DCA, one per minimum fill of a
 * resting bid up to DEFAULT_BID_FILLS), else the count of up to MAX_FILLS_SEARCHED that makes the escrow exact (a count the user chose).
 * Without kob-wasm: the value over the worst rate, carriers and reserve not counted.
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
 * The fills a multi-fill market BUY budgets (its escrow is `bidEscrow(order, amount, fills)`: `bidAmountOf` finds the count that makes it
 * exact) and the delivery carrier each pays; null for every other order, or without kob-wasm.
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
 * The most a bid's escrow lets its fills deliver: kob-wasm `bidBuyingPower` of the order state and the KAS on its UTXO (the covenant's remaining
 * buying power). A bid's quantity is its KAS, so it is the amount the escrow was sized for plus what the carriers of fills that do not happen buy.
 */
function maxReceiveOf(o: DecodedOrder, kob: KobWasm): bigint | undefined {
  try {
    return kob.bidBuyingPower(o.state, o.value);
  } catch {
    return undefined;
  }
}

/**
 * KAS a pure self-transfer of token UTXOs (a Merge: no order created, every token output the wallet's own) moves off the wallet's token UTXOs into
 * spendable KAS: the KAS on its token inputs less the KAS on its token outputs. The KAS line counts KAS on the wallet's token UTXOs as the wallet's
 * already, so a merge reads as the fee alone there; this line says what becomes spendable. 0 for anything else: in a placement, part of that KAS
 * moves into the order's custody instead.
 */
export function freedFromTokens(s: Pick<SigningSummary, 'maker' | 'inputs' | 'outputs' | 'orders'>): bigint {
  const outs = s.outputs.filter((o) => o.token !== null);
  if (s.orders.length > 0 || outs.length === 0 || !outs.every((o) => o.token!.owner === s.maker && o.token!.escrowOf === null)) return 0n;
  const kasIn = s.inputs.filter((i) => i.type === 'token' && i.ownedBy === 'maker').reduce((sum, i) => sum + i.amount, 0n);
  const kasOut = outs.reduce((sum, o) => sum + o.value, 0n);
  return kasIn > kasOut ? kasIn - kasOut : 0n;
}

/** One token a pure merge folds: how many of the wallet's UTXOs go in, how many come out, and the amount (the same on both sides). */
export interface MergedToken { ref: TokenRef; inCount: number; outCount: number; amount: bigint }

/**
 * The tokens a request merges, or null when it is not a pure merge: a self-transfer of the wallet's own token UTXOs (no order created or spent,
 * every token input and output the wallet's own plain UTXO) that puts each token's amount into FEWER UTXOs and keeps it unchanged. A merge moves
 * no token to anyone and changes no balance, so the net effect lists no token line: this says what is signed (N UTXOs into M, the same amount).
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
 * The orders whose price scale does not fit the decimals the app gave their token (audit 2026-10-30 G7). A plain order of a token whose decimals the
 * app says it knows (a registry, issued, declared or program-default token: not an `assumed` one) quotes its price per the standard scale
 * `10^min(decimals, 9)` base units, which is what the app plans; the frame cannot check the decimals themselves, but an order whose scale
 * disagrees with them reads differently under a different hint (the same order read as 0.5, 500,000,000 or 0.05 tokens). Blocks Approve.
 */
function scaleMismatches(s: SigningSummary, hints: TokenHint[]): { code: string }[] {
  const out: { code: string }[] = [];
  for (const o of s.orders) {
    const d = o.description;
    const h = hints.find((x) => x.covenantId === d.tokenCovId);
    if (!h || h.source === 'assumed' || d.pair || d.scale <= 0n) continue;
    if (d.scale !== 10n ** BigInt(Math.min(h.decimals, 9))) out.push({ code: 'token-scale' });
  }
  return out;
}

/** Compact lines of a decoded request: KAS and every token, net for this key; KAS locked into orders and paid to other keys; the fee. */
export function summaryLines(s: SigningSummary, hints: TokenHint[], kob?: KobWasm, terms = false): SignerSummary {
  const lines: SummaryLine[] = [];
  const k = s.net.kas;
  const kas = (v: bigint) => `${formatKas(v)} KAS`;
  lines.push({ label: t('signer.net.kas'), value: signed(k.walletDelta, kas), dir: k.walletDelta < 0n ? 'out' : k.walletDelta > 0n ? 'in' : null });
  for (const tok of s.net.tokens) {
    const hint = hints.find((h) => h.covenantId === tok.ref.covenantId);
    const label = tokenName(tok.ref.covenantId, hint, terms);
    const amount = (v: bigint) => formatTokenAmount(v, hint ? hint.decimals : ASSUMED_TOKEN_DECIMALS);
    if (tok.walletDelta !== 0n) lines.push({ label, value: signed(tok.walletDelta, amount), dir: tok.walletDelta < 0n ? 'out' : 'in' });
    if (tok.toOthers > 0n) lines.push({ label: t('signer.net.tokensToOthers', { token: label }), value: amount(tok.toOthers), dir: 'out' });
  }
  const merged = mergedTokens(s);
  for (const m of merged ?? []) {
    const hint = hints.find((h) => h.covenantId === m.ref.covenantId);
    const amount = formatTokenAmount(m.amount, hint ? hint.decimals : ASSUMED_TOKEN_DECIMALS);
    lines.push({ label: t('signer.net.merged', { token: tokenName(m.ref.covenantId, hint, terms) }), value: t('signer.net.mergedValue', { inCount: m.inCount, outCount: m.outCount, amount }), dir: null });
  }
  if (k.locked.total > 0n) lines.push({ label: t('signer.net.locked'), value: kas(k.locked.total), dir: null });
  if (k.released > 0n) lines.push({ label: t('signer.net.released'), value: kas(k.released), dir: null });
  // WHICH of the wallet's orders a cancel closes (8 + 8 hex of its covenant id, as the token is named): a hostile indexer can only swap one of the user's own orders for another, and this makes the swap visible (S7)
  for (const sp of s.spends.filter((x) => x.makerIsWallet && x.covenantId && (x.action === 'cancel' || x.action === 'refund' || x.action === 'sweep')).slice(0, MAX_CLOSED_LINES)) {
    lines.push({ label: t(sp.action === 'sweep' ? 'signer.net.sweeps' : 'signer.net.closes'), value: longId(sp.covenantId!), dir: null });
  }
  if (k.toOthers > 0n) lines.push({ label: t('signer.net.toOthers'), value: kas(k.toOthers), dir: 'out' });
  const fromTokens = freedFromTokens(s);
  if (fromTokens > 0n) lines.push({ label: t('signer.net.fromTokens'), value: kas(fromTokens), dir: null });
  const registry = hintRegistry(hints);
  const orders = s.orders.filter((o) => o.sweptFrom === undefined).map((o) => {
    const chain = orderSummary(o.description, { value: o.value, locked: o.locked, deadline: o.deadline, fills: marketFillsOf(o, kob) }, hints, registry, terms);
    return flippedSummary(o, hints.find((h) => h.covenantId === o.description.tokenCovId), kob, chain, terms) ?? chain;
  });
  const found = [...s.blocking.filter((b) => !SHOWN_AS_LINES.has(b.code)), ...(terms ? scaleMismatches(s, hints) : [])];
  return { title: t(merged ? 'signer.kind.merge' : (TITLE[s.kind] ?? TITLE.other)), lines, orders, fee: kas(k.fee), ...(s.inputs.length > s.signatures ? { unsignedKas: true } : {}), ...problemsOf(found), decoded: s };
}

/** Decodes `built` for `maker` (the signer's own key) and summarises it. Throws when the decoder does. */
export function summarize(kob: KobWasm, built: BuiltTx, maker: Hex, hints: TokenHint[]): SignerSummary {
  return summaryLines(decodeSigning({ kob, built, maker, registry: null }), hints, kob, true);
}

/**
 * A token creation (KCC-23) as ONE request: the preparation `prep` and the genesis `built` spending its output 0. Checked here, from the
 * transactions alone: the genesis spends exactly the preparation's output 0; every input of the preparation is a P2PK coin of this key and is
 * signed; every output pays this key (P2PK, no covenant): the genesis funding and change; its payload is a KCC-23 declaration. With the app's
 * `claim` of the genesis token outputs, each claimed output must carry exactly the script of its claimed state in the claimed program and be
 * bound to the one new covenant: the frame then names the supply and what this key receives, and those outputs are no longer "unknown". Summarised
 * as the genesis (its decoded effect) with the declared name, ticker and decimals first, the KAS line and the fee covering both. Throws when a
 * check fails (the request is refused, never shown).
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
  const prepIn = prep.tx.inputs.reduce((a, i) => a + BigInt(i.utxo.amount), 0n);
  const prepOut = prep.tx.outputs.reduce((a, o) => a + BigInt(o.value), 0n);
  const prepFee = prepIn - prepOut;
  if (prepFee < 0n || prepFee !== BigInt(prep.fee.fee)) throw new Error('fee');
  const g = summarize(kob, built, maker, hints);
  // the claimed token outputs, each re-derived from its state: an output that does not match refuses the request
  const verified = new Set<number>();
  let supply = 0n;
  let toMaker = 0n;
  if (claim) {
    const cov = built.covenants.length === 1 ? built.covenants[0]! : null;
    if (!cov || cov.outputs.length !== claim.outputs.length || claim.outputs.some((o) => !cov.outputs.includes(o.index))) throw new Error('claim');
    for (const o of claim.outputs) {
      const out = built.tx.outputs[o.index];
      const state = { amount: o.amount, owner: o.owner, owner_scheme: o.ownerScheme, borrow_scheme: o.borrowScheme, borrow_guard: o.borrowGuard, extension_commitment: claim.extensionCommitment };
      if (!out || out.covenant?.covenantId !== cov.covenantId || kob.tokenScriptPublicKey(claim.program as never, state as never) !== out.scriptPublicKey) throw new Error('claim');
      verified.add(o.index);
      supply += BigInt(o.amount);
      if (o.owner === maker && o.ownerScheme === 0) toMaker += BigInt(o.amount);
    }
  }
  // the KAS line: the genesis spends this key's preparation output, so its net is the creation's, less the preparation's fee
  const k = g.decoded.net.kas;
  const delta = k.walletDelta - prepFee;
  const kas = (v: bigint) => `${formatKas(v)} KAS`;
  const lines: SummaryLine[] = [
    { label: t('signer.declare.name'), value: decl.name ?? '', dir: null },
    { label: t('signer.declare.symbol'), value: decl.symbol ?? '', dir: null },
    { label: t('signer.declare.decimals'), value: decl.decimals === null ? '' : String(decl.decimals), dir: null },
    { label: t('signer.declare.steps'), value: t('signer.declare.stepsValue'), dir: null },
    // the program and the extension commitment of the token output scripts, as the outputs were checked against them (audit 2026-10-30 G4: two
    // creations that differ only there looked the same)
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
          ...(supply > toMaker ? [{ label: t('signer.declare.toOthers'), value: `${formatTokenAmount(supply - toMaker, decl.decimals)} ${decl.symbol ?? ''}`.trim(), dir: 'out' as const }] : []),
        ]
      : []),
    { label: t('signer.net.kas'), value: signed(delta, kas), dir: delta < 0n ? 'out' : delta > 0n ? 'in' : null },
    ...g.lines.slice(1),
  ];
  // the token outputs checked against the claim are not "unknown scripts": every other finding still counts
  const problems = g.decoded.blocking.filter((b) => !SHOWN_AS_LINES.has(b.code) && !(b.code === 'output-unknown' && b.output !== undefined && verified.has(b.output)));
  return { ...g, title: t('signer.kind.createToken'), lines, fee: kas(k.fee + prepFee), ...problemsOf(problems) };
}
