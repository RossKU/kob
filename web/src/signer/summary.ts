// The signer's own short summary of a signing request: decoded INSIDE the signer frame with kob-wasm (`decodeSigning`, the decoder of the
// app's pre-sign checks), from the transaction itself, never from what the app says about it: the net effect on this key, the fee, and the
// terms of every order the transaction creates or amends (side, type, amount, price, worst price, stop), so a changed price or stop cannot pass
// behind an unchanged net effect. The frame shows it as one sentence (`text`) next to Reject / Approve.
import { decodeSigning, type SigningKind, type SigningSummary, type TokenRef } from '../kob/decode';
import type { OrderDescription } from '../kob/order-facts';
import { longId, type TokenInfo, type TokenRegistry } from '../kob/registry';
import type { BuiltTx, Hex } from '../kob/types';
import { formatKas, formatTokenAmount } from '../kob/units';
import type { KobWasm } from '../kob/wasm';
import { formatDateTime, t } from '../i18n';
import { orderRows, orderTitle, type Fmt, type Row } from '../ui/confirm/confirm-model';
import type { TokenHint } from './protocol';

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
export function summaryLines(s: SigningSummary, hints: TokenHint[]): SignerSummary {
  const lines: SummaryLine[] = [];
  const k = s.net.kas;
  const kas = (v: bigint) => `${formatKas(v)} KAS`;
  const kasDelta = signed(k.walletDelta, kas);
  lines.push({ label: t('signer.net.kas'), value: kasDelta, phrase: kasDelta });
  for (const tok of s.net.tokens) {
    const hint = hints.find((h) => h.covenantId === tok.ref.covenantId);
    const label = tokenName(tok.ref.covenantId, hint);
    const amount = (v: bigint) => (hint ? formatTokenAmount(v, hint.decimals) : `${v.toString()} ${t('signer.net.baseUnits')}`);
    if (tok.walletDelta !== 0n) {
      const value = signed(tok.walletDelta, amount);
      lines.push({ label, value, phrase: t('signer.phrase.token', { amount: value, token: label }) });
    }
    if (tok.toOthers > 0n) lines.push({ label: t('signer.net.tokensToOthers', { token: label }), value: amount(tok.toOthers), phrase: t('signer.phrase.tokensToOthers', { amount: amount(tok.toOthers), token: label }) });
  }
  if (k.locked.total > 0n) lines.push({ label: t('signer.net.locked'), value: kas(k.locked.total), phrase: t('signer.phrase.locked', { kas: kas(k.locked.total) }) });
  if (k.released > 0n) lines.push({ label: t('signer.net.released'), value: kas(k.released), phrase: t('signer.phrase.released', { kas: kas(k.released) }) });
  if (k.toOthers > 0n) lines.push({ label: t('signer.net.toOthers'), value: kas(k.toOthers), phrase: t('signer.phrase.toOthers', { kas: kas(k.toOthers) }) });
  const registry = hintRegistry(hints);
  const orders = s.orders.filter((o) => o.sweptFrom === undefined).map((o) => orderSummary(o.description, { value: o.value, locked: o.locked, deadline: o.deadline }, hints, registry));
  const title = t(TITLE[s.kind] ?? TITLE.other);
  const fee = kas(k.fee);
  const head = orders.length ? orders.map((o) => o.line).join('; ') : title;
  const net = lines.map((l) => l.phrase).join(', ');
  return { title, lines, orders, fee, problems: s.blocking.filter((b) => !SHOWN_AS_LINES.has(b.code)).length, text: t('signer.text', { head, net, fee }) };
}

/** Decodes `built` for `maker` (the signer's own key) and summarises it. Throws when the decoder does. */
export function summarize(kob: KobWasm, built: BuiltTx, maker: Hex, hints: TokenHint[]): SignerSummary {
  return summaryLines(decodeSigning({ kob, built, maker, registry: null }), hints);
}
