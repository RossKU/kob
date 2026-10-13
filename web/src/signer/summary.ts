// The signer's own short summary of a signing request: decoded INSIDE the signer frame with kob-wasm (`decodeSigning`, the decoder of the
// app's confirmation screen), from the transaction itself, never from what the app says about it: the net effect on this key, the fee, and the
// terms of every order the transaction creates or amends (side, type, amount, prices, stops, timing: the rows of the confirmation screen), so a
// changed price or stop cannot pass behind an unchanged net effect.
import { decodeSigning, type SigningKind, type SigningSummary, type TokenRef } from '../kob/decode';
import type { OrderDescription } from '../kob/order-facts';
import { longId, type TokenInfo, type TokenRegistry } from '../kob/registry';
import type { BuiltTx, Hex } from '../kob/types';
import { formatKas, formatTokenAmount } from '../kob/units';
import type { KobWasm } from '../kob/wasm';
import { formatDateTime, t } from '../i18n';
import { orderRows, orderTitle, type Fmt } from '../ui/confirm/confirm-model';
import type { TokenHint } from './protocol';

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
  rows: { label: string; value: string }[];
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
  return {
    title: orderTitle(d, extra.deadline, f),
    token: f.ref.display,
    rows: orderRows(d, extra, f).map((r) => ({ label: r.label, value: r.value })),
    exit: exit ? { ...exit, title: t('confirm.exitOf', { title: exit.title }) } : null,
  };
}

const signed = (v: bigint, text: (abs: bigint) => string) => (v < 0n ? `−${text(-v)}` : `+${text(v)}`);

/** Compact lines of a decoded request: KAS and every token, net for this key; KAS locked into orders and paid to other keys; the fee. */
export function summaryLines(s: SigningSummary, hints: TokenHint[]): SignerSummary {
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
  const orders = s.orders.filter((o) => o.sweptFrom === undefined).map((o) => orderSummary(o.description, { value: o.value, locked: o.locked, deadline: o.deadline }, hints, registry));
  return { title: t(TITLE[s.kind] ?? TITLE.other), lines, orders, fee: kas(k.fee), problems: s.blocking.filter((b) => !SHOWN_AS_LINES.has(b.code)).length };
}

/** Decodes `built` for `maker` (the signer's own key) and summarises it. Throws when the decoder does. */
export function summarize(kob: KobWasm, built: BuiltTx, maker: Hex, hints: TokenHint[]): SignerSummary {
  return summaryLines(decodeSigning({ kob, built, maker, registry: null }), hints);
}
