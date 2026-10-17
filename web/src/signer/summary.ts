// The signer's own short summary of a signing request: decoded INSIDE the signer frame with kob-wasm (`decodeSigning`, the decoder of the
// app's confirmation screen), from the transaction itself, never from what the app says about it: the net effect on this key, the fee, and the
// terms of every order the transaction creates or amends (side, type, amount, prices, stops, timing: the rows of the confirmation screen), so a
// changed price or stop cannot pass behind an unchanged net effect. A plain order of a KAS market the app shows inverted (KAS/TOKEN) is said
// first as the user placed it (sell KAS for tokens), its chain-side terms below; every price reads "X per Y", never a pair notation.
import { decodeSigning, type DecodedOrder, type SigningKind, type SigningSummary, type TokenRef } from '../kob/decode';
import { baseKind, type OrderDescription } from '../kob/order-facts';
import { DEFAULT_BID_FILLS } from '../kob/orders/common';
import { longId, type TokenInfo, type TokenRegistry } from '../kob/registry';
import type { BuiltTx, Hex } from '../kob/types';
import { formatKas, formatTokenAmount } from '../kob/units';
import type { KobWasm } from '../kob/wasm';
import { formatDateTime, t } from '../i18n';
import { orderRows, orderTitle, orderTypeCode, type Fmt } from '../ui/confirm/confirm-model';
import type { TokenHint } from './protocol';
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

export interface SignerSummary {
  title: string;
  lines: SummaryLine[];
  /** the orders the transaction creates or amends (a sweep in place keeps its order's terms and is not listed) */
  orders: SummaryOrder[];
  fee: string;
  /** decoder findings that block signing on the app's confirmation screen (shown as one line here, never hidden) */
  problems: number;
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
const tokenName = (covenantId: Hex, hint: TokenHint | undefined): string => (hint ? `${hint.ticker} (${longId(covenantId)})` : t('signer.net.token', { id: longId(covenantId) }));

/** A TokenRef from the app's hint (ticker and decimals are labels; without a hint amounts stay in base units). */
function refOfHint(covenantId: Hex, hints: TokenHint[]): TokenRef {
  const h = hints.find((x) => x.covenantId === covenantId);
  return { covenantId, ticker: h?.ticker ?? null, decimals: h?.decimals ?? null, display: tokenName(covenantId, h), inRegistry: false, tradable: false };
}

/** The hints as the confirmation model's registry lookup (a pair order's quote token B): ticker and decimals only. */
function hintRegistry(hints: TokenHint[]): TokenRegistry {
  const byCovenantId = new Map(hints.map((h) => [h.covenantId, { covenantId: h.covenantId, ticker: h.ticker, decimals: h.decimals, tradable: false } as unknown as TokenInfo]));
  return { network: '', templates: [], templateChecks: [], tokens: [], byCovenantId };
}

function orderSummary(d: OrderDescription, extra: Parameters<typeof orderRows>[1], hints: TokenHint[], registry: TokenRegistry): SummaryOrder {
  const f: Fmt = { tr: t, ref: refOfHint(d.tokenCovId, hints), clock: null, time: (unix) => formatDateTime(unix), registry };
  const exit = d.entry?.exit ? orderSummary(d.entry.exit, { value: null, locked: null, deadline: null }, hints, registry) : null;
  const perScale = t('confirm.f.perScale', { scale: d.scale.toString() });
  return {
    title: orderTitle(d, extra.deadline, f),
    token: f.ref.display,
    // a price of a token whose decimals the frame does not know (no hint: an unlisted token's first order) is per `scale` base units:
    // the frame keeps that detail, as amounts there are base units too (TN10 2026-10-15 B-13: "1000 base units" at "0.5 KAS")
    rows: orderRows(d, extra, f).map((r) => ({ id: r.id, label: r.label, value: r.detail === perScale && f.ref.decimals === null ? `${r.value} ${r.detail}` : r.value, ...timeOf(r, d) })),
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
  rising: 'signer.flip.type.rising',
};

/**
 * A plain order of a KAS market (KobAsk / KobBid) on the view the app shows inverted (KAS/TOKEN, the hint's `inverted`): said as the user placed
 * it, still from the decoded order. The chain buys tokens where the user sells KAS: "Sell KAS · market (auction)", what is sold and received,
 * the prices in tokens per KAS (rounded against the user); the chain-side terms (budget, escrow, minimum fill) follow as plainly labelled rows.
 * Null for every other order, a native view, or a token the app gave no ticker for.
 */
function flippedSummary(o: DecodedOrder, hint: TokenHint | undefined, kob: KobWasm | undefined, chain: SummaryOrder): SummaryOrder | null {
  const d = o.description;
  const kind = baseKind(d.kind);
  if (!hint?.inverted || d.pair || d.entry || (kind !== 'KobAsk' && kind !== 'KobBid') || d.scale <= 0n || d.price <= 0n) return null;
  const T = hint.ticker;
  const dec = hint.decimals;
  const scale = d.scale;
  const decays = !!d.auction && d.auction.slope > 0n && d.auction.priceEnd > 0n;
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
    rows.push({ label: t('signer.flip.youReceive'), value: tokT(n, 'down') });
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

/** How many delivery counts `bidAmountOf` tries for a bid whose fills the user chose. */
const MAX_FILLS_SEARCHED = 64n;

/** Compact lines of a decoded request: KAS and every token, net for this key; KAS locked into orders and paid to other keys; the fee. */
export function summaryLines(s: SigningSummary, hints: TokenHint[], kob?: KobWasm): SignerSummary {
  const lines: SummaryLine[] = [];
  const k = s.net.kas;
  const kas = (v: bigint) => `${formatKas(v)} KAS`;
  lines.push({ label: t('signer.net.kas'), value: signed(k.walletDelta, kas), dir: k.walletDelta < 0n ? 'out' : k.walletDelta > 0n ? 'in' : null });
  for (const tok of s.net.tokens) {
    const hint = hints.find((h) => h.covenantId === tok.ref.covenantId);
    const label = tokenName(tok.ref.covenantId, hint);
    const amount = (v: bigint) => (hint ? formatTokenAmount(v, hint.decimals) : `${v.toString()} ${t('signer.net.baseUnits')}`);
    if (tok.walletDelta !== 0n) lines.push({ label, value: signed(tok.walletDelta, amount), dir: tok.walletDelta < 0n ? 'out' : 'in' });
    if (tok.toOthers > 0n) lines.push({ label: t('signer.net.tokensToOthers', { token: label }), value: amount(tok.toOthers), dir: 'out' });
  }
  if (k.locked.total > 0n) lines.push({ label: t('signer.net.locked'), value: kas(k.locked.total), dir: null });
  if (k.released > 0n) lines.push({ label: t('signer.net.released'), value: kas(k.released), dir: null });
  if (k.toOthers > 0n) lines.push({ label: t('signer.net.toOthers'), value: kas(k.toOthers), dir: 'out' });
  const registry = hintRegistry(hints);
  const orders = s.orders.filter((o) => o.sweptFrom === undefined).map((o) => {
    const chain = orderSummary(o.description, { value: o.value, locked: o.locked, deadline: o.deadline }, hints, registry);
    return flippedSummary(o, hints.find((h) => h.covenantId === o.description.tokenCovId), kob, chain) ?? chain;
  });
  return { title: t(TITLE[s.kind] ?? TITLE.other), lines, orders, fee: kas(k.fee), problems: s.blocking.filter((b) => !SHOWN_AS_LINES.has(b.code)).length, decoded: s };
}

/** Decodes `built` for `maker` (the signer's own key) and summarises it. Throws when the decoder does. */
export function summarize(kob: KobWasm, built: BuiltTx, maker: Hex, hints: TokenHint[]): SignerSummary {
  return summaryLines(decodeSigning({ kob, built, maker, registry: null }), hints, kob);
}
