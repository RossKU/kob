// The signer's own short summary of a signing request: decoded INSIDE the signer frame with kob-wasm (`decodeSigning`, the decoder of the
// app's pre-sign checks), from the transaction itself, never from what the app says about it: the net effect on this key, the fee, and the
// terms of every order the transaction creates or amends (side, type, amount, price, worst price, stop), so a changed price or stop cannot pass
// behind an unchanged net effect. The frame shows it as one sentence (`text`) next to Reject / Approve, in the words of the ticket: a plain order
// of a KAS market says what the user does in the orientation the app shows that market in (the hint's `inverted`: KAS/TOKEN sells KAS where the
// chain buys the token), numbers at a reader's precision (words.ts), rounded so that a bound never looks better than it is.
import { decodeSigning, type DecodedOrder, type SigningKind, type SigningSummary, type TokenRef } from '../kob/decode';
import { baseKind, type OrderDescription } from '../kob/order-facts';
import { DEFAULT_BID_FILLS } from '../kob/orders/common';
import { longId, type TokenInfo, type TokenRegistry } from '../kob/registry';
import type { BuiltTx, Hex } from '../kob/types';
import { formatKas, formatTokenAmount } from '../kob/units';
import type { KobWasm } from '../kob/wasm';
import { formatDateTime, t } from '../i18n';
import { orderRows, orderTitle, orderTypeCode, type Fmt, type Row } from '../ui/confirm/confirm-model';
import type { TokenHint } from './protocol';
import { div, kasPerTokenWords, kasWords, tokenWords, tokensPerKasWords } from './words';

export interface SummaryLine {
  label: string;
  value: string;
  /** the line as a part of the sentence ("−10 EXKCC (70707070…70707070)", "20 KAS locked in orders") */
  phrase: string;
}

/** One order the transaction creates or amends, described from its decoded state (an if-done entry: its committed exit as `exit`). */
export interface SummaryOrder {
  title: string;
  /** the token of the order (ticker hint and the covenant id's 8 + 8 hex) */
  token: string;
  rows: { label: string; value: string }[];
  exit: SummaryOrder | null;
  /** the order in a few words: title, amount and price, the worst price of an auction, a stop, a pair's bound */
  line: string;
}

export interface SignerSummary {
  title: string;
  lines: SummaryLine[];
  /** the orders the transaction creates or amends (a sweep in place keeps its order's terms and is not listed) */
  orders: SummaryOrder[];
  fee: string;
  /** decoder findings that stop the app's own pre-sign checks (shown as one line here, never hidden) */
  problems: number;
  /** the whole request in one sentence: the orders (or the kind of request), the net effect on this key, the fee */
  text: string;
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

/** blocking for the app's pre-sign checks unless the user asked for them (it knows, the frame does not): shown here as "to other keys" lines */
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

/** One order in a few words, from its rows: amount at price, the worst price of an auction, a stop, what a pair order receives or pays at most. */
function orderLine(title: string, d: OrderDescription, rows: Row[], value: (r: Row) => string): string {
  const of = (id: string): string | null => {
    const r = rows.find((x) => x.id === id);
    return r ? value(r) : null;
  };
  const terms: string[] = [];
  const amount = of('amount') ?? of('budget');
  const price = of('price');
  if (amount && price) terms.push(t('signer.line.at', { amount, price }));
  else if (amount ?? price) terms.push((amount ?? price)!);
  // an auction only guarantees its end: the worst price is what the order may get
  const worst = rows.find((r) => r.id === 'allIn' && r.tone === 'warn');
  if (worst) terms.push(t('signer.line.worst', { price: value(worst) }));
  for (const id of ['stop', 'entryStop']) {
    const stop = of(id);
    if (stop) terms.push(t('signer.line.stop', { price: stop }));
  }
  const bound = of('pairTotal');
  if (bound) terms.push(t(d.side === 'sell' ? 'signer.line.receiveMin' : 'signer.line.payMax', { amount: bound }));
  const repeat = of('repeat');
  if (repeat) terms.push(repeat);
  return terms.length ? `${title}: ${terms.join(', ')}` : title;
}

/** Order types a plain KAS order names in its sentence (a limit says nothing); the rest are said by `orderLine`. */
const TYPE_WORD: Record<string, string> = {
  market: 'signer.type.market', marketFok: 'signer.type.marketFok', ioc: 'signer.type.ioc', fok: 'signer.type.fok', twap: 'signer.type.twap', dca: 'signer.type.dca',
  dutch: 'signer.type.auction', rising: 'signer.type.auction', limitDay: 'signer.type.day',
};

/**
 * A plain order of a KAS market (KobAsk / KobBid: limit, IOC / FOK, market, TWAP / DCA, Dutch) in the words of the ticket, null for every other
 * order (a conditional, an if-done, a pair: `orderLine`) and for a token the app gave no ticker for. A sell holds a fixed amount of tokens: an
 * auction (a price that moves) receives about their value at its start and at least at its end. A buy is a fixed amount of tokens too (the amount
 * its escrow was made for): an auction pays about their value at its start, the tip included, and at most its escrow; what is not filled returns.
 */
function plainLine(o: DecodedOrder, hint: TokenHint | undefined, kob: KobWasm | undefined, deadline: bigint | null): string | null {
  const d = o.description;
  const kind = baseKind(d.kind);
  if (!hint || d.pair || (kind !== 'KobAsk' && kind !== 'KobBid')) return null;
  const T = hint.ticker;
  const dec = hint.decimals;
  const scale = d.scale;
  if (scale <= 0n || d.price <= 0n) return null;
  const decays = !!d.auction && d.auction.slope > 0n && d.auction.priceEnd > 0n;
  const start = d.price;
  const worst = decays ? d.auction!.priceEnd : start;
  const code = orderTypeCode(d, deadline);
  const type = TYPE_WORD[code] ? t(TYPE_WORD[code]!) : null;
  const typed = (line: string): string => (type && !decays ? `${line} (${type})` : line);
  const tokens = (n: bigint, mode: 'down' | 'up' | 'nearest' = 'nearest') => `${tokenWords(n, dec, mode)} ${T}`;
  const inverted = hint.inverted === true;
  if (kind === 'KobAsk') {
    const n = d.tokenAmount ?? d.amountLeft ?? 0n;
    // what the sale receives at a price: the price less the tip, per whole token
    const kasAt = (price: bigint, mode: 'down' | 'nearest') => kasWords(div(n * (price > d.tip ? price - d.tip : 0n), scale, mode), mode);
    if (!decays) {
      return typed(inverted
        ? t('signer.say.buyKasAt', { kas: kasAt(start, 'down'), price: tokensPerKasWords(start, scale, dec), ticker: T })
        : t('signer.say.sellAt', { amount: tokens(n), price: kasPerTokenWords(start, scale, dec) }));
    }
    return t(inverted ? 'signer.say.buyKasFor' : 'signer.say.sellFor', { amount: tokens(n), kas: kasAt(start, 'nearest'), least: kasAt(worst, 'down'), type: type ?? '' });
  }
  // a buy: the amount its escrow was made for (at its worst price), that budget (the most it pays) and what the amount costs at its start
  const st = o.state.state as unknown as { reserve?: string; deliveryCarrier?: string };
  const reserve = BigInt(st.reserve ?? '0');
  const carrier = BigInt(st.deliveryCarrier ?? '0');
  const { amount: least, fills: f } = bidAmountOf(o, kob, scale, worst + d.tip);
  const budget = o.value - (f > 0n ? f - 1n : 0n) - f * carrier - reserve;
  const about = div(least * (start + d.tip), scale, 'nearest');
  if (!decays) {
    return typed(inverted
      ? t('signer.say.sellKasAt', { kas: kasWords(budget, 'up'), price: tokensPerKasWords(start, scale, dec), ticker: T })
      : t('signer.say.buyAt', { amount: tokens(least, 'down'), price: kasPerTokenWords(start, scale, dec) }));
  }
  return t(inverted ? 'signer.say.sellKasFor' : 'signer.say.buyFor', { amount: tokens(least), about: kasWords(about), kas: kasWords(budget, 'up'), type: type ?? '' });
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

function orderSummary(d: OrderDescription, extra: Parameters<typeof orderRows>[1], hints: TokenHint[], registry: TokenRegistry): SummaryOrder {
  const f: Fmt = { tr: t, ref: refOfHint(d.tokenCovId, hints), clock: null, time: (unix) => formatDateTime(unix), registry };
  const exit = d.entry?.exit ? orderSummary(d.entry.exit, { value: null, locked: null, deadline: null }, hints, registry) : null;
  const perScale = t('confirm.f.perScale', { scale: d.scale.toString() });
  // a price of a token whose decimals the frame does not know (no hint: an unlisted token's first order) is per `scale` base units:
  // the frame keeps that detail, as amounts there are base units too (TN10 2026-10-15 B-13: "1000 base units" at "0.5 KAS")
  const value = (r: Row): string => (r.detail === perScale ? `${r.value} ${r.detail}` : r.value);
  const rows = orderRows(d, extra, f);
  const title = orderTitle(d, extra.deadline, f);
  const line = orderLine(title, d, rows, value);
  return {
    title,
    token: f.ref.display,
    rows: rows.map((r) => ({ label: r.label, value: value(r) })),
    exit: exit ? { ...exit, title: t('confirm.exitOf', { title: exit.title }) } : null,
    line: exit ? `${line}; ${t('signer.line.then', { line: exit.line })}` : line,
  };
}

const signed = (v: bigint, text: (abs: bigint) => string) => (v < 0n ? `−${text(-v)}` : `+${text(v)}`);

/** Compact lines of a decoded request: KAS and every token, net for this key; KAS locked into orders and paid to other keys; the fee. */
export function summaryLines(s: SigningSummary, hints: TokenHint[], kob?: KobWasm): SignerSummary {
  const lines: SummaryLine[] = [];
  const k = s.net.kas;
  const kas = (v: bigint) => `${formatKas(v)} KAS`;
  // the sentence's numbers: two decimals of KAS, a few significant digits of a token (the lines keep the exact values)
  const kasSay = (v: bigint) => `${kasWords(v)} KAS`;
  const kasDelta = signed(k.walletDelta, kas);
  lines.push({ label: t('signer.net.kas'), value: kasDelta, phrase: signed(k.walletDelta, kasSay) });
  for (const tok of s.net.tokens) {
    const hint = hints.find((h) => h.covenantId === tok.ref.covenantId);
    const label = tokenName(tok.ref.covenantId, hint);
    const amount = (v: bigint) => (hint ? formatTokenAmount(v, hint.decimals) : `${v.toString()} ${t('signer.net.baseUnits')}`);
    const say = (v: bigint) => (hint ? tokenWords(v, hint.decimals) : `${v.toString()} ${t('signer.net.baseUnits')}`);
    if (tok.walletDelta !== 0n) lines.push({ label, value: signed(tok.walletDelta, amount), phrase: t('signer.phrase.token', { amount: signed(tok.walletDelta, say), token: label }) });
    if (tok.toOthers > 0n) lines.push({ label: t('signer.net.tokensToOthers', { token: label }), value: amount(tok.toOthers), phrase: t('signer.phrase.tokensToOthers', { amount: say(tok.toOthers), token: label }) });
  }
  if (k.locked.total > 0n) lines.push({ label: t('signer.net.locked'), value: kas(k.locked.total), phrase: t('signer.phrase.locked', { kas: kasSay(k.locked.total) }) });
  if (k.released > 0n) lines.push({ label: t('signer.net.released'), value: kas(k.released), phrase: t('signer.phrase.released', { kas: kasSay(k.released) }) });
  if (k.toOthers > 0n) lines.push({ label: t('signer.net.toOthers'), value: kas(k.toOthers), phrase: t('signer.phrase.toOthers', { kas: kasSay(k.toOthers) }) });
  const registry = hintRegistry(hints);
  const created = s.orders.filter((o) => o.sweptFrom === undefined);
  const orders = created.map((o) => {
    const sum = orderSummary(o.description, { value: o.value, locked: o.locked, deadline: o.deadline }, hints, registry);
    const plain = plainLine(o, hints.find((h) => h.covenantId === o.description.tokenCovId), kob, o.deadline);
    return plain ? { ...sum, line: plain } : sum;
  });
  const title = t(TITLE[s.kind] ?? TITLE.other);
  const fee = kas(k.fee);
  const head = orders.length ? orders.map((o) => o.line).join('; ') : title;
  // what a new order does to the wallet is what it locks (the tokens and KAS it takes leave for that order); anything to other keys is always said
  let body: string;
  if (orders.length && k.toOthers === 0n && s.net.tokens.every((x) => x.toOthers === 0n)) {
    const tokensLocked = s.net.tokens.filter((x) => x.escrowed > 0n).map((x) => {
      const hint = hints.find((h) => h.covenantId === x.ref.covenantId);
      return hint ? `${tokenWords(x.escrowed, hint.decimals)} ${hint.ticker}` : `${x.escrowed.toString()} ${t('signer.net.baseUnits')}`;
    });
    const what = [...(k.locked.total > 0n ? [kasSay(k.locked.total)] : []), ...tokensLocked].join(t('signer.and'));
    body = what ? t('signer.locks', { what }) : lines.map((l) => l.phrase).join(', ');
  } else body = lines.map((l) => l.phrase).join(', ');
  // the identity of every token named: the ticker is the app's label, the covenant id the transaction's
  const named = hints.filter((h) => s.net.tokens.some((x) => x.ref.covenantId === h.covenantId) || created.some((o) => o.description.tokenCovId === h.covenantId));
  const ids = named.map((h) => t('signer.id', { ticker: h.ticker, id: longId(h.covenantId) })).join(', ');
  const text = t('signer.text', { head, body, fee: kasWords(k.fee, 'up') }) + (ids ? ` ${t('signer.ids', { ids })}` : '');
  return { title, lines, orders, fee, problems: s.blocking.filter((b) => !SHOWN_AS_LINES.has(b.code)).length, text };
}

/** Decodes `built` for `maker` (the signer's own key) and summarises it. Throws when the decoder does. */
export function summarize(kob: KobWasm, built: BuiltTx, maker: Hex, hints: TokenHint[]): SignerSummary {
  return summaryLines(decodeSigning({ kob, built, maker, registry: null }), hints, kob);
}
