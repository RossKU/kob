// Translated sentences for findings (planner issues, pre-sign findings, cancel / issuance / registry codes, wallet and node errors).
//
// Dictionary keys are `issues.<code>` (en/issues.ts) with plain `{name}` placeholders. This module turns the raw params of a finding
// into display strings first (sompi -> "1.5 KAS", token base units -> "12.5 TICK", basis points -> "12.50", direction words -> plain
// words, 64-hex ids -> "abcd...1234") so the dictionaries stay free of number formatting. DOM-free: it runs in node tests.
import { has, t, type Params } from './index';
import { nodeReason } from '../data/node-error';
import { COND_ISSUE_CATALOG } from '../kob/orders/cond-issues';
import { ISSUE_CATALOG } from '../kob/orders/common-issues';
import { formatBps, formatKas, formatTokenAmount } from '../kob/units';
import { withoutRaw } from './build-error';

export interface IssueLike {
  code: string;
  message: string;
  params?: Params;
  /** decoder findings carry the input / output index on the issue itself: merged into the params as `{input}` / `{output}` */
  input?: number;
  output?: number;
  /** form findings carry the field (`holders[2].owner`): the holder number is merged into the params as `{holder}` (1-based) */
  field?: string;
  /** registry findings carry the JSON path: merged as `{path}` */
  path?: string;
}

export interface IssueTextContext {
  /** decimals of the token the finding is about (token amounts are base units) */
  tokenDecimals?: number;
  tokenTicker?: string;
  /**
   * a token/token pair A/B (the pair ticket): the quote token B. Prices of the findings are then B base units per whole A, shown with B's decimals
   * (and B's ticker where the sentence does not name it); amounts of the PAIR_* findings are of the token their `{ticker}` names (A or B).
   */
  quoteDecimals?: number;
  quoteTicker?: string;
}

/**
 * kas: sompi; bps: basis points; token: base units of the finding's token; price: a price per whole token (sompi, or on a pair B base units, shown
 * with B's ticker); pairPrice: a PAIR_* price (B base units per whole A, number only: the sentence names {ticker}); pairToken: a PAIR_* amount of
 * the token `{ticker}` names (number only).
 */
type Fmt = 'kas' | 'bps' | 'token' | 'price' | 'pairPrice' | 'pairToken';

// ------------------------------------------------------------------------------------------------ which params are what

/** `code.param` -> format. The `{x:kas}` / `{x:bps}` placeholders of the two planner catalogues are read from their English messages. */
const FORMATS = new Map<string, Fmt>();

function readCatalog(cat: Record<string, { message: string }>): void {
  for (const [code, entry] of Object.entries(cat)) {
    for (const m of entry.message.matchAll(/\{(\w+):(kas|bps)\}/g)) FORMATS.set(`${code}.${m[1]}`, m[2] as Fmt);
  }
}
readCatalog(ISSUE_CATALOG);
readCatalog(COND_ISSUE_CATALOG);

function declare(fmt: Fmt, entries: Record<string, string[]>): void {
  for (const [code, names] of Object.entries(entries)) for (const n of names) FORMATS.set(`${code}.${n}`, fmt);
}

// prices per whole token: sompi on a KAS market, B base units per whole A on a pair (the planner's shared codes keep their meaning on a pair).
// Declared after the catalogues: a `{x:kas}` price of a catalogue message is a price here.
declare('price', {
  PRICE_NOT_ON_TICK: ['below', 'above'],
  PRICE_AGGRESSIVE_VS_MARKET: ['reference'],
  PRICE_FAR_FROM_MARKET: ['reference'],
  SELF_TRADE: ['ownPrice'],
  COND_ENTRY_STOP_BEYOND_LIMIT: ['stop', 'price'],
  COND_TP_STOP_ORDER: ['takeProfit', 'stop'],
  COND_STOP_LIMIT_BEYOND_STOP: ['limit', 'stop'],
  MARKETABLE_AUCTION: ['touch'],
  MARKETABLE_LIMIT_FILLS_AT_LIMIT: ['touch'],
  MARKETABLE_REJECTED: ['touch'],
  MARKET_REFERENCE_SOURCE: ['reference', 'worst'],
  MARKET_REFERENCE_DIVERGES: ['reference', 'lastFill'],
  MARKET_START_VS_LAST_FILL: ['start', 'reference'],
  MARKET_START_VS_INDEXER: ['start', 'reference'],
  MARKET_START_ACKNOWLEDGED: ['start', 'reference'],
  MARKET_START_UNCHECKED: ['start'],
  COND_TP_CROSSES: ['touch'],
  COND_TP_NOT_PROFITABLE: ['profitPerToken'],
  COND_PREFUND_SHORT: ['needed', 'given'],
});

// the pair planner's own codes: prices B base units per whole A, amounts of the token their {ticker} names
declare('pairPrice', {
  PAIR_MARKETABLE_AUCTION: ['touch'],
  PAIR_MARKETABLE_LIMIT_FILLS_AT_LIMIT: ['touch'],
  PAIR_MARKETABLE_REJECTED: ['touch'],
  PAIR_PRICE_AGGRESSIVE_VS_MARKET: ['reference'],
  PAIR_PRICE_FAR_FROM_MARKET: ['reference'],
  PAIR_MARKET_REFERENCE_SOURCE: ['reference', 'worst'],
  PAIR_MARKET_REFERENCE_DIVERGES: ['reference', 'lastFill'],
  PAIR_TP_CROSSES: ['touch'],
  PAIR_TP_NOT_PROFITABLE: ['profitPerToken'],
  PAIR_PREFUND_SHORT: ['needed', 'given'],
});
declare('pairToken', {
  PAIR_INSUFFICIENT_TOKENS: ['needed', 'have', 'shortfall'],
  PAIR_KRON_CUSTODY_TOO_LARGE: ['amount', 'max'],
  PAIR_KRON_DELIVERY_TOO_LARGE: ['max'],
});

// sompi params the catalogue messages print plainly (KAS amounts of the other catalogues)
declare('kas', {
  'fee-mismatch': ['derived', 'declared'],
  'fee-excessive': ['fee', 'moved'],
  'fee-rate-excessive': ['rate', 'max'],
  'fee-high': ['fee', 'minimum'],
  'expected-max-fee': ['fee', 'max'],
  'expected-kas-locked': ['expected', 'actual'],
  'kas-released': ['released'],
  'payment-out': ['amount'],
  'funds.insufficient': ['need', 'have'],
  'cancel.insufficient-funds': ['shortfall'],
});

// token amounts in base units (shown with the token's decimals when the caller knows them)
declare('token', {
  INSUFFICIENT_TOKENS: ['needed', 'have', 'shortfall'],
  MARKET_DEPTH_INSUFFICIENT: ['available', 'amount'],
  FOK_INSUFFICIENT_DEPTH: ['amount', 'available'],
  COND_MIN_FILL_INVALID: ['amount'],
  'cancel.insufficient-tokens': ['need', 'have', 'short'],
  'cancel.top-up': ['amount'],
  'cancel.stray-other-extension': ['amount'],
  'cancel.strays-abandoned': ['amount'],
  'holders.sum_mismatch': ['holders', 'supply'],
  'supply.too_large': ['max'],
  'transfer-out': ['amount'],
  'strays-swept': ['amount'],
  'token-unbalanced': ['in', 'out'],
  'expected-tokens-escrowed': ['expected', 'actual'],
});

/** Words the planners pass as params in English; localised through `issues.term.*`. Only the params named here are looked up. */
const TERM_PARAMS = new Set(['direction', 'side', 'counterparty']);
const TERM_KEYS: Record<string, string> = {
  below: 'below', above: 'above', 'at or below': 'atOrBelow', 'at or above': 'atOrAbove', buy: 'buy', sell: 'sell', bids: 'bids', asks: 'asks',
};

// ------------------------------------------------------------------------------------------------ formatting

const toBig = (v: string | number | bigint): bigint | null => {
  if (typeof v === 'bigint') return v;
  if (typeof v === 'number') return Number.isSafeInteger(v) ? BigInt(v) : null;
  return /^-?\d+$/.test(v) ? BigInt(v) : null;
};

const HEX64 = /^[0-9a-f]{64}$/i;
const OUTPOINT = /^([0-9a-f]{64}):(\d+)$/i;
const shortHex = (s: string): string => `${s.slice(0, 4)}…${s.slice(-4)}`;

function formatToken(b: bigint, ctx: IssueTextContext): string {
  if (ctx.tokenDecimals === undefined) return t('issues.fmt.baseUnits', { n: b.toString() });
  const amount = formatTokenAmount(b, ctx.tokenDecimals);
  return ctx.tokenTicker ? `${amount} ${ctx.tokenTicker}` : amount;
}

/** DAA runs at ~10 per second (matcher.md); "about 3 h 20 min". */
function formatWait(seconds: bigint): string {
  if (seconds < 60n) return t('issues.dur.lt');
  const d = seconds / 86_400n;
  const h = (seconds % 86_400n) / 3_600n;
  const m = (seconds % 3_600n) / 60n;
  if (d > 0n) return t('issues.dur.dh', { d: d.toString(), h: h.toString() });
  if (h > 0n) return t('issues.dur.hm', { h: h.toString(), m: m.toString() });
  return t('issues.dur.m', { m: m.toString() });
}

/** A price per whole token: KAS on a KAS market, the quote token B on a pair (B base units per whole A). */
function formatPrice(b: bigint, ctx: IssueTextContext): string {
  if (ctx.quoteDecimals === undefined) return `${formatKas(b)} KAS`;
  const amount = formatTokenAmount(b, ctx.quoteDecimals);
  return ctx.quoteTicker ? `${amount} ${ctx.quoteTicker}` : amount;
}

/** An amount of the pair token `ticker` names (A or B), number only; base units when its decimals are unknown. */
function formatPairToken(b: bigint, ticker: unknown, ctx: IssueTextContext): string {
  const decimals = ticker !== undefined && ticker === ctx.quoteTicker ? ctx.quoteDecimals : ticker !== undefined && ticker === ctx.tokenTicker ? ctx.tokenDecimals : undefined;
  return decimals === undefined ? t('issues.fmt.baseUnits', { n: b.toString() }) : formatTokenAmount(b, decimals);
}

function formatParam(code: string, name: string, v: string | number | bigint, ctx: IssueTextContext, params: Params = {}): string {
  if (typeof v === 'string' && TERM_PARAMS.has(name) && v in TERM_KEYS) return t(`issues.term.${TERM_KEYS[v]}`);
  const fmt = FORMATS.get(`${code}.${name}`);
  if (fmt) {
    const b = toBig(v);
    if (b !== null) {
      if (fmt === 'kas') return `${formatKas(b)} KAS`;
      if (fmt === 'bps') return formatBps(b);
      if (fmt === 'price') return formatPrice(b, ctx);
      if (fmt === 'pairPrice') return ctx.quoteDecimals === undefined ? t('issues.fmt.baseUnits', { n: b.toString() }) : formatTokenAmount(b, ctx.quoteDecimals);
      if (fmt === 'pairToken') return formatPairToken(b, params.ticker, ctx);
      return formatToken(b, ctx);
    }
  }
  if (typeof v === 'string') {
    if (HEX64.test(v)) return shortHex(v);
    const o = OUTPOINT.exec(v);
    if (o) return `${shortHex(o[1])}:${o[2]}`;
  }
  return String(v);
}

/** Display params of a finding: formatted raw params + the merged index / field context + derived values. */
function displayParams(code: string, issue: Pick<IssueLike, 'message' | 'params' | 'input' | 'output' | 'field' | 'path'>, ctx: IssueTextContext): Params {
  const raw = issue.params ?? {};
  const out: Params = {};
  for (const [k, v] of Object.entries(raw)) out[k] = formatParam(code, k, v, ctx, raw);
  // defaults that a sentence may use when the finding did not carry them: the English detail of the finding itself
  if (!('message' in out)) out.message = issue.message;
  if (!('reason' in out)) out.reason = issue.message;
  if (issue.input !== undefined) out.input = String(issue.input);
  if (issue.output !== undefined) out.output = String(issue.output);
  out.where = issue.input !== undefined ? t('issues.where.input', { n: issue.input }) : issue.output !== undefined ? t('issues.where.output', { n: issue.output }) : '';
  if (issue.path !== undefined && !('path' in out)) out.path = issue.path;
  const holder = issue.field ? /^holders\[(\d+)\]\./.exec(issue.field) : null;
  if (holder && !('holder' in out)) out.holder = String(Number(holder[1]) + 1);
  else if (!('holder' in out)) out.holder = '?';

  // derived values
  if (code === 'cancel.insufficient-tokens') {
    // with `have`: need = total the replacement needs; without: need is already the shortfall
    const need = toBig(raw.need ?? 0);
    const have = raw.have !== undefined ? toBig(raw.have) : null;
    if (need !== null) out.short = formatToken(have !== null && need > have ? need - have : need, ctx);
  }
  if (code === 'refund.not-yet') {
    const due = toBig(raw.dueDaa ?? 0);
    const now = toBig(raw.nowDaa ?? 0);
    if (due !== null && now !== null) out.wait = formatWait(due > now ? (due - now) / 10n : 0n);
  }
  return out;
}

// ------------------------------------------------------------------------------------------------ API

/**
 * Translated sentence for a finding. Falls back to the English `issue.message` when no `issues.<code>` key exists.
 * Amount params are formatted by (code, param): KAS sompi -> "1.5 KAS", token base units -> `ctx.tokenDecimals` (+ `ctx.tokenTicker`) or
 * "N base units"; `input` / `output` / `field` / `path` of the finding are merged into the params.
 */
export function issueText(issue: IssueLike, ctx: IssueTextContext = {}): string {
  const key = `issues.${issue.code}`;
  if (!has(key)) return issue.message;
  // a raw builder / node text is replaced by a plain sentence (the UI shows the raw text behind "Details", kit RawDetails)
  return t(key, displayParams(issue.code, withoutRaw(issue), ctx));
}

/** Same for a keyed message without an issue object, e.g. `keyedText('wallet', 'rejected')`, `keyedText('untradable', reason)`, `keyedText('lookalike', level, { ... })`. */
export function keyedText(prefix: string, code: string, params?: Params, fallback?: string): string {
  const key = `issues.${prefix}.${code}`;
  if (!has(key)) return fallback ?? key;
  return t(key, displayParams(`${prefix}.${code}`, { message: fallback ?? '', ...(params ? { params } : {}) }, {}));
}

export interface SignFlowErrorLike {
  stage: string;
  code: string;
  message: string;
  cause?: unknown;
}

/**
 * Friendly sentence for a `SignFlowError` (wallet/sign.ts): wallet stages use `issues.signflow.<code>`, the broadcast stage (`submitting`)
 * uses the node error codes `issues.node.<code>`. `{reason}` of node errors is the node's own reason, extracted from the raw text.
 */
export function signFlowText(e: SignFlowErrorLike): string {
  if (e.stage === 'submitting') {
    const raw = (e.cause as { raw?: unknown } | null)?.raw;
    const reason = typeof raw === 'string' ? nodeReason(raw) : e.message;
    return keyedText('node', e.code, { reason }, e.message);
  }
  return keyedText('signflow', e.code, undefined, e.message);
}

export interface WalletNoticeLike { code: string; message: string; params?: Record<string, string | number> }

/** Translated blind-signing notice of `describeInputsForWallet`. */
export function walletNoticeText(n: WalletNoticeLike): string {
  return keyedText('walletnotice', n.code, n.params, n.message);
}
