// The signer's own short summary of a signing request: decoded INSIDE the signer frame with kob-wasm (`decodeSigning`, the decoder of the
// app's confirmation screen), from the transaction itself, never from what the app says about it. Net effect on this key and the fee only.
import { decodeSigning, type SigningKind, type SigningSummary } from '../kob/decode';
import type { BuiltTx, Hex } from '../kob/types';
import { formatKas, formatTokenAmount } from '../kob/units';
import type { KobWasm } from '../kob/wasm';
import { t } from '../i18n';
import type { TokenHint } from './protocol';

export interface SummaryLine {
  label: string;
  value: string;
  /** 'out' = leaves this key, 'in' = comes to it */
  dir: 'in' | 'out' | null;
}

export interface SignerSummary {
  title: string;
  lines: SummaryLine[];
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

const short = (h: string) => `${h.slice(0, 4)}…${h.slice(-4)}`;
const signed = (v: bigint, text: (abs: bigint) => string) => (v < 0n ? `−${text(-v)}` : `+${text(v)}`);

/** Compact lines of a decoded request: KAS and every token, net for this key; KAS locked into orders and paid to other keys; the fee. */
export function summaryLines(s: SigningSummary, hints: TokenHint[]): SignerSummary {
  const lines: SummaryLine[] = [];
  const k = s.net.kas;
  const kas = (v: bigint) => `${formatKas(v)} KAS`;
  lines.push({ label: t('signer.net.kas'), value: signed(k.walletDelta, kas), dir: k.walletDelta < 0n ? 'out' : k.walletDelta > 0n ? 'in' : null });
  for (const tok of s.net.tokens) {
    const hint = hints.find((h) => h.covenantId === tok.ref.covenantId);
    const label = hint ? `${hint.ticker} (${short(tok.ref.covenantId)})` : t('signer.net.token', { id: short(tok.ref.covenantId) });
    const amount = (v: bigint) => (hint ? formatTokenAmount(v, hint.decimals) : `${v.toString()} ${t('signer.net.baseUnits')}`);
    if (tok.walletDelta !== 0n) lines.push({ label, value: signed(tok.walletDelta, amount), dir: tok.walletDelta < 0n ? 'out' : 'in' });
    if (tok.toOthers > 0n) lines.push({ label: t('signer.net.tokensToOthers', { token: label }), value: amount(tok.toOthers), dir: 'out' });
  }
  if (k.locked.total > 0n) lines.push({ label: t('signer.net.locked'), value: kas(k.locked.total), dir: null });
  if (k.released > 0n) lines.push({ label: t('signer.net.released'), value: kas(k.released), dir: null });
  if (k.toOthers > 0n) lines.push({ label: t('signer.net.toOthers'), value: kas(k.toOthers), dir: 'out' });
  return { title: t(TITLE[s.kind] ?? TITLE.other), lines, fee: kas(k.fee), problems: s.blocking.filter((b) => !SHOWN_AS_LINES.has(b.code)).length };
}

/** Decodes `built` for `maker` (the signer's own key) and summarises it. Throws when the decoder does. */
export function summarize(kob: KobWasm, built: BuiltTx, maker: Hex, hints: TokenHint[]): SignerSummary {
  return summaryLines(decodeSigning({ kob, built, maker, registry: null }), hints);
}
