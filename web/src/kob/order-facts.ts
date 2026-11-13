// Facts that can be read straight off an order state. They hold no protocol logic, because the
// covenants and kob-wasm decide validity. The pre-sign decoder (decode.ts), the cancel builders
// (cancel.ts), balances and positions share them. DOM-free, bigint arithmetic only.
//
// Pair orders (`KobPair`, `KobCondPair`, `KobIfdPair`, docs/spec/order-types.md) trade an amount of
// the base token A at a price in the quote token B per whole A. Their states name both tokens
// (S / T for KobPair and KobCondPair, A / B for KobIfdPair). The helpers below read them
// oriented A / B. So for the rest of the app the order's token is A, its scale is scale(A), its
// price is B base units per whole A, and its tip is KAS (sompi per whole A).
import type { AnyState, BaseKind, Family, Hex, IfdAskState, IfdBidState, KasKind, OrderKind, OrderState, PairKind, PairTriggerRule, TemplateName } from './types';
import type { KobWasm } from './wasm';
import { quoteOf } from './units';
import { isTokenProgram, CODEC_OF_PROGRAM } from './programs.generated';

/** The KAS-quoted kinds that sell the token (asks). */
export const ASK_KINDS: readonly BaseKind[] = ['KobAsk', 'KobCondAsk', 'KobIfdAsk'];
/** The KAS-quoted kinds that buy the token (bids). */
export const BID_KINDS: readonly BaseKind[] = ['KobBid', 'KobCondBid', 'KobIfdBid'];
/** The KAS-quoted kinds (a token against KAS), each with a KRON twin. */
export const KAS_KINDS: readonly KasKind[] = ['KobAsk', 'KobCondAsk', 'KobIfdAsk', 'KobBid', 'KobCondBid', 'KobIfdBid'];
/**
 * The pair kinds (token A for token B).
 * One template per kind serves both sides and both token families, so there is no KRON twin.
 */
export const PAIR_KINDS: readonly PairKind[] = ['KobPair', 'KobCondPair', 'KobIfdPair'];
/** Every family-neutral order kind. */
export const BASE_KINDS: readonly BaseKind[] = [...ASK_KINDS, ...BID_KINDS, ...PAIR_KINDS];
/**
 * Every order kind as kob-wasm tags it.
 * That is the KAS-quoted kinds with their KRON twins, and the three pair kinds.
 */
export const ORDER_KINDS: readonly OrderKind[] = [...BASE_KINDS, ...[...ASK_KINDS, ...BID_KINDS].map((k) => `${k}Kron` as OrderKind)];

/** A pair kind (`KobPair`, `KobCondPair`, `KobIfdPair`). */
export const isPairKind = (k: string): k is PairKind => (PAIR_KINDS as readonly string[]).includes(k);
/** Strips the `Kron` suffix. Every KAS-quoted kind exists once per token family. */
export const baseKind = (k: OrderKind): BaseKind => (k.endsWith('Kron') ? k.slice(0, -4) : k) as BaseKind;
/**
 * Family of an order kind or template name (KAS-quoted kinds and token programs).
 * A pair kind names no family, because its state names one per token. For an order use
 * {@link familyOfOrder}. A pair order's family is its base token A's, as in kob-protocol
 * `AnyState::family`.
 */
export const familyOfKind = (k: TemplateName | string): Family => (isTokenProgram(k) ? CODEC_OF_PROGRAM[k] : k.endsWith('Kron') ? 'kron' : 'kcc20');
/** The kind tag of a base kind in a family. The pair kinds have one template for every family. */
export const kindFor = (k: BaseKind, family: Family): OrderKind => (family === 'kron' && !isPairKind(k) ? (`${k}Kron` as OrderKind) : k);

/** True if `k` is an order kind as kob-wasm tags it. */
export const isOrderKind = (k: string): k is OrderKind => (ORDER_KINDS as readonly string[]).includes(k);
/**
 * The order kind of a template name.
 * A delivery id_type 0 build (`KobAskKronD0`, a KRON program with id_type 0 holders) maps to its
 * KRON kind.
 */
export const orderKindOfTemplate = (t: string): string => (/^Kob[A-Za-z]+KronD0$/.test(t) ? t.slice(0, -2) : t);
/**
 * True for kinds that may hold tokens in custody.
 * The ask-side KAS kinds hold the maker's tokens in one custody UTXO owned by the order
 * (docs/spec/matcher.md 1.2). Every pair kind may hold custodies of A, of B or both.
 * {@link custodiesOf} says which, from the state.
 */
export const holdsTokens = (k: OrderKind): boolean => (ASK_KINDS as readonly string[]).includes(baseKind(k)) || isPairKind(k);
/**
 * Side of a KAS kind.
 * A pair kind's side is in its state ({@link sideOf}), so 'sell' is returned for it as a
 * placeholder.
 */
export const sideOfKind = (k: OrderKind): 'sell' | 'buy' => ((ASK_KINDS as readonly string[]).includes(baseKind(k)) || isPairKind(k) ? 'sell' : 'buy');
/** Side of an order: KAS kinds by kind, pair kinds by their state (`side` 1 sells A, 2 buys A). */
export function sideOf(o: { kind: string; state: object }): 'sell' | 'buy' {
  if (isPairKind(o.kind)) return bigOr0((o.state as { side?: unknown }).side) === 1n ? 'sell' : 'buy';
  return sideOfKind(o.kind as OrderKind);
}
/** Conditional kinds (stop, take-profit, OCO legs): KobCondAsk, KobCondBid and KobCondPair. */
export const isCondKind = (k: OrderKind): boolean => baseKind(k) === 'KobCondAsk' || baseKind(k) === 'KobCondBid' || k === 'KobCondPair';
/** If-done entries: KobIfdBid, KobIfdAsk and the pair entry KobIfdPair. */
export const isIfdKind = (k: OrderKind): boolean => baseKind(k) === 'KobIfdAsk' || baseKind(k) === 'KobIfdBid' || k === 'KobIfdPair';
/** Narrows by family-neutral kind: `isKind(o, 'KobAsk')` holds for `KobAsk` and `KobAskKron`. */
export function isKind<K extends BaseKind>(o: OrderState, k: K): o is Extract<OrderState, { kind: K | `${K}Kron` }> {
  return baseKind(o.kind) === k;
}

/** Converts a 64-bit decimal string to bigint. It also takes numbers and bigints. */
export const big = (v: string | number | bigint): bigint => BigInt(v);
/**
 * A state field as a bigint, 0 when absent or malformed.
 * A hostile state describes as 0, because describing it must not throw.
 */
function bigOr0(v: unknown): bigint {
  if (typeof v === 'bigint') return v;
  if (typeof v === 'number' && Number.isSafeInteger(v)) return BigInt(v);
  return typeof v === 'string' && /^-?\d+$/.test(v) ? BigInt(v) : 0n;
}

// ------------------------------------------------------------------------------------- pair tokens

/** One token of a pair order, as its state pins it. */
export interface PairTokenFacts {
  covId: Hex;
  tplHash: Hex;
  prefixLen: bigint;
  suffixLen: bigint;
  /**
   * The state's family code as a family. Null for a code other than 1 (KCC-20) or 2 (KRON), which
   * a hostile state can carry.
   */
  family: Family | null;
  familyCode: bigint;
  /** Base units per whole token. */
  scale: bigint;
  /**
   * The KCC-20 extension commitment the state names for new outputs of this token (zero for
   * KRON). Null when the state names none.
   */
  ext: Hex | null;
}

/** The two tokens of a pair order (A = base, B = quote) and its side, as its state pins them. */
export interface PairFacts {
  kind: PairKind;
  side: 'sell' | 'buy';
  a: PairTokenFacts;
  b: PairTokenFacts;
}

const famOf = (code: bigint): Family | null => (code === 1n ? 'kcc20' : code === 2n ? 'kron' : null);
function tokenFacts(s: Record<string, unknown>, p: 's' | 't' | 'a' | 'b'): PairTokenFacts {
  const g = (k: string): unknown => s[`${p}${k}`];
  const code = bigOr0(g('Family'));
  const ext = g('Ext');
  const str = (v: unknown): string => (typeof v === 'string' ? v : '');
  return {
    covId: str(g('CovId')), tplHash: str(g('TplHash')), prefixLen: bigOr0(g('Pre')), suffixLen: bigOr0(g('Suf')), family: famOf(code), familyCode: code,
    scale: bigOr0(g('Scale')), ext: typeof ext === 'string' ? ext : null,
  };
}

/** A, B and the side of a pair order (mirrors kob-wasm `pairTokens`), null for other kinds. */
export function pairFactsOf(o: { kind: string; state: object }): PairFacts | null {
  if (!isPairKind(o.kind)) return null;
  const s = o.state as Record<string, unknown>;
  const side: 'sell' | 'buy' = bigOr0(s.side) === 1n ? 'sell' : 'buy';
  if (o.kind === 'KobIfdPair') return { kind: o.kind, side, a: tokenFacts(s, 'a'), b: tokenFacts(s, 'b') };
  const sTok = tokenFacts(s, 's');
  const tTok = tokenFacts(s, 't');
  return side === 'sell' ? { kind: o.kind, side, a: sTok, b: tTok } : { kind: o.kind, side, a: tTok, b: sTok };
}

/** The tokens an order's own transfers move: its token and, for a pair order, both A and B. */
export function ownTokenIdsOf(o: { kind: string; state: object }): Hex[] {
  const pf = pairFactsOf(o);
  if (pf) return pf.a.covId === pf.b.covId ? [pf.a.covId] : [pf.a.covId, pf.b.covId];
  const s = o.state as { tokenCovId?: Hex };
  return [s.tokenCovId ?? ''].filter(Boolean);
}

/**
 * One exact custody an order holds: the token, its amount and its role.
 * `base` is the order's token A and `quote` is a pair order's B.
 */
export interface CustodyFact { token: Hex; amount: bigint; role: 'base' | 'quote' }

/**
 * The exact custodies an order holds, in record order (kob-protocol `AnyState::custodies`).
 * - A KAS ask holds its token (`amountLeft`).
 * - A `KobPair` or `KobCondPair` holds its S in `custody` (an ask: A, a bid: B).
 * - A buy-first `KobIfdPair` holds its B escrow, when non-zero.
 * - A sell-first one holds its A (when `amountLeft` > 0), then its B prefund (when `custody` > 0).
 * With `kob`, kob-wasm's own answer is used.
 */
export function custodiesOf(o: OrderState, kob?: KobWasm): CustodyFact[] {
  const pf = pairFactsOf(o);
  if (kob) {
    try {
      return kob.custodies(o).map((c) => ({ token: c.token, amount: c.amount, role: pf && c.token === pf.b.covId && pf.a.covId !== pf.b.covId ? 'quote' : 'base' }));
    } catch {
      /* a state kob-wasm refuses: read it below */
    }
  }
  if (pf) {
    const s = o.state as unknown as Record<string, unknown>;
    const custody = bigOr0(s.custody);
    if (o.kind !== 'KobIfdPair') return [{ token: pf.side === 'sell' ? pf.a.covId : pf.b.covId, amount: custody, role: pf.side === 'sell' ? 'base' : 'quote' }];
    const out: CustodyFact[] = [];
    const left = bigOr0(s.amountLeft);
    if (pf.side === 'sell' && left > 0n) out.push({ token: pf.a.covId, amount: left, role: 'base' });
    if (custody > 0n) out.push({ token: pf.b.covId, amount: custody, role: 'quote' });
    return out;
  }
  if (!holdsTokens(o.kind)) return [];
  const amount = custodyAmountOf(o);
  return amount === null ? [] : [{ token: tokenCovIdOf(o), amount, role: 'base' }];
}

/**
 * Custody token amount the order holds in its first custody, or null.
 * A KAS ask holds exactly `amountLeft` base units (docs/spec/matcher.md 1.2). A pair order holds
 * the first custody of kob-protocol `custodies`, because a sell-first entry's B prefund is the
 * second ({@link custodiesOf}). KAS bid kinds and pair orders without custody give null.
 */
export function custodyAmountOf(o: OrderState): bigint | null {
  if (isPairKind(o.kind)) return custodiesOf(o)[0]?.amount ?? null;
  if (!holdsTokens(o.kind)) return null;
  return big((o.state as { amountLeft: string }).amountLeft);
}

/**
 * Base units (of A for a pair order) still open, or null for plain KAS bids.
 * The quantity of a plain KAS bid is its KAS budget.
 */
export function amountLeftOf(o: OrderState): bigint | null {
  if (baseKind(o.kind) === 'KobBid') return null;
  return big((o.state as { amountLeft: string }).amountLeft);
}

/** The order's scale: base units per whole token (a pair order: per whole A). */
export const scaleOf = (o: OrderState): bigint => {
  const pf = pairFactsOf(o);
  return pf ? pf.a.scale : big((o.state as { scale: string }).scale);
};
/** The order's minimum fill (base units), unless a fill takes everything left. */
export const minFillOf = (o: OrderState): bigint => big((o.state as { minFill: string }).minFill);

/** The order's maker. */
export const makerOf = (o: OrderState): Hex => o.state.maker;
/** The order's token: a KAS-quoted order's token, a pair order's base token A. */
export const tokenCovIdOf = (o: { kind: string; state: object }): Hex => pairFactsOf(o)?.a.covId ?? (o.state as { tokenCovId: Hex }).tokenCovId;
/** The order's token template hash (a pair order: A's). */
export const tokenTplHashOf = (o: { kind: string; state: object }): Hex => pairFactsOf(o)?.a.tplHash ?? (o.state as { tokenTplHash: Hex }).tokenTplHash;
/** A pair order's quote token B (null for every other kind). */
export const quoteCovIdOf = (o: { kind: string; state: object }): Hex | null => pairFactsOf(o)?.b.covId ?? null;
/** The family of an order: a KAS-quoted kind's, or a pair order's base token A's. */
export const familyOfOrder = (o: { kind: string; state: object }): Family => {
  const pf = pairFactsOf(o);
  return pf ? (pf.a.family ?? 'kcc20') : familyOfKind(o.kind);
};

/**
 * Extension commitment of the token of the order's first custody, when the state carries it.
 * A KAS bid-side kind carries its token's. A sell-first `KobIfdPair` holding A carries its `aExt`
 * and any other `KobIfdPair` its `bExt`. KAS asks, `KobPair` and `KobCondPair` carry the
 * commitment of their custody token only in the custody itself, so they give null.
 */
export function extensionOf(o: OrderState): Hex | null {
  if (o.kind === 'KobIfdPair') {
    const s = o.state;
    return String(s.side) === '1' && bigOr0(s.amountLeft) > 0n ? s.aExt : s.bExt;
  }
  if (isPairKind(o.kind)) return null;
  // A KCC-20 sell-first entry names its custody's commitment in its committed exit (KobCondBid
  // state [118..150)). A KRON one has none.
  if (o.kind === 'KobIfdAsk') return o.state.exitState.length === 642 ? o.state.exitState.slice(236, 300) : null;
  return 'extensionCommitment' in o.state ? (o.state as { extensionCommitment: Hex }).extensionCommitment : null;
}

/**
 * The extension commitment a pair order's state names for new outputs of `token` (A or B), or null.
 * A `KobIfdPair` names both (`aExt`, `bExt`). A `KobPair` or `KobCondPair` names only its bought
 * token's (`tExt`). Every other kind or token gives null.
 */
export function pairExtFor(o: OrderState, token: Hex): Hex | null {
  const pf = pairFactsOf(o);
  if (!pf) return null;
  if (token === pf.a.covId && pf.a.ext !== null) return pf.a.ext;
  if (token === pf.b.covId && pf.b.ext !== null) return pf.b.ext;
  return null;
}

/** Time in force: good-till-cancelled, immediate-or-cancel or fill-or-kill. */
export type TifName = 'gtc' | 'ioc' | 'fok';
/**
 * Stop leg terms.
 * A trigger (touch) arms only in a tx that fills at least `minTouch` base units of a resting
 * order exposed for at least `minRestDaa` DAA.
 */
export interface TriggerTerms {
  stopPrice: bigint; slipBps: bigint; armed: bigint; bandDaa: bigint; keeperTip: bigint; minRestDaa: bigint; minTouch: bigint;
  trailStep: bigint; trailGap: bigint; trailWait: bigint;
}

/**
 * The pair side of a pair order (`KobPair`, `KobCondPair`, `KobIfdPair`).
 * It has both tokens, prices in B base units per whole A, custodies and the trigger rule.
 * Amounts of A are in base units of A and amounts of B in base units of B.
 */
export interface PairTerms {
  kind: PairKind;
  /**
   * `sell` sells A for B (an ASK, a sell-first entry).
   * `buy` buys A with B (a BID, a buy-first entry).
   */
  side: 'sell' | 'buy';
  base: PairTokenFacts;
  quote: PairTokenFacts;
  /**
   * B base units per whole A: a `KobPair`'s limit (a decay's start), a `KobIfdPair`'s limit or a
   * `KobCondPair`'s take-profit / limit leg (0 = none).
   */
  price: bigint;
  /** The end price of a decaying ask or rising bid (`KobPair` with a slope), else null. */
  priceEnd: bigint | null;
  /** A `KobCondPair`'s stop (`stopPrice`) or a stop entry's `entryStop`. 0 means none. */
  stop: bigint;
  /** The state's mutable `custody`: the exact S held (a `KobIfdPair`: its B). */
  custody: bigint;
  /** The exact custodies in record order (kob-protocol `custodies`). */
  custodies: CustodyFact[];
  /**
   * A held in custody, and B held in custody. B is a bid's escrow, a buy-first entry's escrow or
   * a sell-first entry's prefund.
   */
  escrowA: bigint;
  escrowB: bigint;
  /** A sell-first entry's buy-back prefund, B base units per whole A (0 otherwise). */
  prefund: bigint;
  /**
   * KAS on each delivery (a `KobIfdPair`: on each exit custody) and on each exit UTXO (if-done
   * entries), in sompi.
   */
  deliveryCarrier: bigint;
  exitCarrier: bigint;
  /**
   * kob-wasm `pairTriggerRule` of a stop (conditional or stop entry), when `describeOrder` had
   * kob-wasm. Null otherwise.
   */
  triggerRule: PairTriggerRule | null;
}

/** Terms of an if-done entry. */
export interface EntryTerms {
  /**
   * `rptAmount`: 0 = no repeat, else 1 + base units of re-arms left.
   * `prefund`: sompi per whole token (sell-first KAS entries), B base units per whole A (a
   * sell-first `KobIfdPair`).
   */
  entryStop: bigint; rptAmount: bigint; exitCarrier: bigint; deliveryCarrier: bigint; prefund: bigint; keeperTip: bigint;
  armed: bigint; bandDaa: bigint;
  /** Trigger rule of a stop entry (touch), as for a stop leg. */
  minTouch: bigint; minRestDaa: bigint;
}

/**
 * Plain-data description of one order state for the pre-sign checks and the signer frame.
 * All amounts are bigint base units. Prices are sompi per whole token, or B base units per whole A
 * for a pair order. A pair order's tip stays KAS (sompi per whole A).
 */
export interface OrderDescription {
  kind: OrderKind;
  side: 'sell' | 'buy';
  maker: Hex;
  /** The order's token (a pair order: its base token A). */
  tokenCovId: Hex;
  /** Base units per whole token (the price denominator; a pair order: scale(A)). */
  scale: bigint;
  /** Smallest fill (base units) unless a fill takes everything left. */
  minFill: bigint;
  /** Base units left; null for KobBid (budget order). */
  amountLeft: bigint | null;
  /** Custody amount of the order's token (KAS asks; a pair order: A held). */
  tokenAmount: bigint | null;
  /**
   * Limit price per whole token: ask / bid limit, conditional take-profit or if-done entry limit.
   * For a pair order, B per whole A.
   */
  price: bigint;
  /** Priority tip, sompi per whole token (KAS for every kind). */
  tip: bigint;
  /** Null for kinds without a tif field. */
  tif: TifName | null;
  activeFrom: bigint;
  expiryDaa: bigint;
  refundTip: bigint;
  /**
   * KobAsk / KobBid / KobPair rate-limit and decay terms (TWAP, DCA, Dutch, market auctions).
   * Null when none is set.
   */
  auction: { interval: bigint; maxFill: bigint; slope: bigint; priceEnd: bigint; decayStep: bigint } | null;
  /** Conditional (stop / trailing / take-profit) terms. */
  trigger: TriggerTerms | null;
  /** If-done entry terms. `exit` is the committed exit order when it could be decoded. */
  entry: (EntryTerms & { exit: OrderDescription | null }) | null;
  /** A pair order's tokens, prices, custodies and trigger rule. Null for every KAS-quoted kind. */
  pair: PairTerms | null;
  /**
   * Repeat fields of a booked exit: parent entry id, the entry's rate per whole token, cut-off and,
   * for a pair exit, `rptPre`. Null for a plain order.
   */
  booked: { parent: Hex; rptPrice: bigint; rptUntil: bigint; rptPre?: bigint } | null;
  /**
   * KAS reserved inside the order value that is not trading value: bid `reserve` and
   * `deliveryCarrier`, entry carriers, prefund, and a pair order's tip prefund.
   */
  reservedKas: bigint;
}

const TIF: Record<string, TifName> = { '0': 'gtc', '1': 'ioc', '2': 'fok' };

/**
 * The whole life (activeFrom to expiry, in DAA) up to which a resting decaying auction of a plain
 * KAS order reads as a multi-fill market order. It is 36,000 DAA, one hour.
 * A multi-fill market order is the market auction of orders/simple.ts `planAuction` with fills > 1.
 * It is tif 0 and expires shortly after it reaches its bound. A Dutch or rising order or a
 * crossing limit auction rests for days, so none of them is that short in practice.
 */
export const MARKET_FILLS_MAX_LIFE_DAA = 36_000n;

/**
 * True for a plain KAS order that is a multi-fill market order.
 * It rests (tif 0), is one decaying auction (no schedule) and lives at most an hour.
 */
export function isMultiFillMarket(d: Pick<OrderDescription, 'kind' | 'pair' | 'tif' | 'auction' | 'activeFrom' | 'expiryDaa'>): boolean {
  const k = baseKind(d.kind);
  const a = d.auction;
  if (d.pair || (k !== 'KobAsk' && k !== 'KobBid') || d.tif !== 'gtc' || !a || a.interval > 0n || a.slope <= 0n || d.activeFrom <= 0n) return false;
  return d.expiryDaa > d.activeFrom && d.expiryDaa - d.activeFrom <= MARKET_FILLS_MAX_LIFE_DAA;
}
const ZERO32 = '00'.repeat(32);

/**
 * Decodes the exit a KAS if-done entry commits, for display only.
 * The entry stores the exit as a truncated state span (`exitState`), without the repeat fields
 * the entry writes itself (docs/spec/matcher.md 1.1). To show it, this pads the missing zero
 * repeat fields (fixed-width pushes) and, for a KCC-20 `KobCondAsk` exit, the entry's own
 * `extensionCommitment`, which the buy-first entry writes behind them. kob-wasm then decodes it.
 * The padded bytes are never signed or hashed. A pair entry's exit comes from kob-wasm
 * `ifdPairExit`.
 */
function decodeCommittedExit(kob: KobWasm, o: IfdBidState | IfdAskState, exitKind: 'KobCondAsk' | 'KobCondBid', family: Family): OrderState | null {
  const push32 = '20' + ZERO32;
  const push8 = '08' + '00'.repeat(8);
  const ext = family === 'kron' || !('extensionCommitment' in o) ? '' : '20' + o.extensionCommitment;
  const pad = exitKind === 'KobCondAsk' ? push32 + push8 + push8 + ext : push32 + push8 + push8 + push8;
  try {
    return kob.decodeState(kindFor(exitKind, family), o.exitState + pad) as OrderState;
  } catch {
    return null;
  }
}

/**
 * The pair terms of a pair order, or null for every other kind.
 * With `kob`, the custodies and the trigger rule come from kob-wasm.
 */
export function pairTermsOf(o: OrderState, kob?: KobWasm): PairTerms | null {
  const pf = pairFactsOf(o);
  if (!pf) return null;
  const s = o.state as unknown as Record<string, unknown>;
  const custodies = custodiesOf(o, kob);
  const held = (role: 'base' | 'quote') => custodies.filter((c) => c.role === role).reduce((a, c) => a + c.amount, 0n);
  let triggerRule: PairTriggerRule | null = null;
  if (kob && (o.kind === 'KobCondPair' || (o.kind === 'KobIfdPair' && bigOr0(s.entryStop) > 0n))) {
    try {
      triggerRule = kob.pairTriggerRule(o);
    } catch {
      triggerRule = null;
    }
  }
  const decaying = o.kind === 'KobPair' && bigOr0(s.slope) !== 0n;
  return {
    kind: pf.kind, side: pf.side, base: pf.a, quote: pf.b,
    price: bigOr0(o.kind === 'KobCondPair' ? s.tpPrice : s.price),
    priceEnd: decaying ? bigOr0(s.priceEnd) : null,
    stop: bigOr0(o.kind === 'KobCondPair' ? s.stopPrice : o.kind === 'KobIfdPair' ? s.entryStop : 0),
    custody: bigOr0(s.custody), custodies, escrowA: held('base'), escrowB: held('quote'),
    prefund: o.kind === 'KobIfdPair' ? bigOr0(s.prefund) : 0n,
    deliveryCarrier: bigOr0(s.deliveryCarrier), exitCarrier: bigOr0(s.exitCarrier), triggerRule,
  };
}

/**
 * Describes an order state.
 * With `kob`, an if-done entry's committed exit is decoded as well, and a pair order's custodies
 * and trigger rule.
 */
export function describeOrder(o: OrderState, kob?: KobWasm): OrderDescription {
  const kind = o.kind;
  const s = o.state as unknown as Record<string, string>;
  const pair = pairTermsOf(o, kob);
  const auctionRaw = 'slope' in s ? { interval: bigOr0(s.interval), maxFill: bigOr0(s.maxFill), slope: bigOr0(s.slope), priceEnd: bigOr0(s.priceEnd), decayStep: bigOr0(s.decayStep) } : null;
  const auction = auctionRaw && (auctionRaw.interval !== 0n || auctionRaw.maxFill !== 0n || auctionRaw.slope !== 0n || auctionRaw.priceEnd !== 0n) ? auctionRaw : null;
  const cond = isCondKind(kind);
  const trigger: TriggerTerms | null = cond
    ? {
        stopPrice: bigOr0(s.stopPrice), slipBps: bigOr0(s.slipBps), armed: bigOr0(s.armed), bandDaa: bigOr0(s.bandDaa), keeperTip: bigOr0(s.keeperTip),
        minRestDaa: bigOr0(s.minRestDaa), minTouch: bigOr0(s.minTouch), trailStep: bigOr0(s.trailStep), trailGap: bigOr0(s.trailGap), trailWait: bigOr0(s.trailWait),
      }
    : null;
  let entry: OrderDescription['entry'] = null;
  if (isIfdKind(kind)) {
    let exit: OrderDescription | null = null;
    if (kob) {
      let d: OrderState | null = null;
      if (kind === 'KobIfdPair') {
        try {
          d = kob.ifdPairExit(o);
        } catch {
          d = null;
        }
      } else d = decodeCommittedExit(kob, o.state as IfdBidState | IfdAskState, baseKind(kind) === 'KobIfdBid' ? 'KobCondAsk' : 'KobCondBid', familyOfKind(kind));
      if (d) exit = describeOrder(d);
    }
    entry = {
      entryStop: bigOr0(s.entryStop), rptAmount: bigOr0(s.rptAmount), exitCarrier: bigOr0(s.exitCarrier),
      deliveryCarrier: bigOr0(s.deliveryCarrier), prefund: bigOr0(s.prefund), keeperTip: bigOr0(s.keeperTip), armed: bigOr0(s.armed),
      bandDaa: bigOr0(s.bandDaa), minTouch: bigOr0(s.minTouch), minRestDaa: bigOr0(s.minRestDaa), exit,
    };
  }
  const parent = s.parent;
  const booked = parent && parent !== ZERO32
    ? { parent, rptPrice: bigOr0(s.rptPrice), rptUntil: bigOr0(s.rptUntil), ...(kind === 'KobCondPair' ? { rptPre: bigOr0(s.rptPre) } : {}) }
    : null;
  const left = amountLeftOf(o);
  const scale = pair ? pair.base.scale : bigOr0(s.scale);
  // A hostile state (negative amounts or rates, a zero scale) describes as 0 here. The decoder
  // flags such a state, and describing it must not throw.
  const q = (n: bigint, r: bigint, round: 'up' | 'down'): bigint => (n >= 0n && r >= 0n && scale > 0n ? quoteOf(n, r, scale, round) : 0n);
  // A pair order's KAS is carriers (deliveries, exits) and its prefunded priority tip,
  // floor(amountLeft x tip / scale(A)). The tip is released to the filler per fill, rounded down,
  // and the rest is returned, so the tip is the reserved part.
  // A sell-first KAS entry prefunds ceil(amountLeft x prefund / scale).
  const reservedKas = pair
    ? q(left ?? 0n, bigOr0(s.tip), 'down')
    : bigOr0(s.reserve) + bigOr0(s.deliveryCarrier) + bigOr0(s.exitCarrier) + q(left ?? 0n, bigOr0(s.prefund), 'up');
  return {
    kind, side: pair ? pair.side : sideOfKind(kind), maker: s.maker, tokenCovId: pair ? pair.base.covId : s.tokenCovId, scale, minFill: bigOr0(s.minFill),
    amountLeft: left, tokenAmount: pair ? (pair.escrowA > 0n || pair.side === 'sell' ? pair.escrowA : null) : custodyAmountOf(o),
    price: pair ? pair.price : bigOr0(cond ? s.tpPrice : s.price), tip: bigOr0(s.tip), tif: s.tif !== undefined ? (TIF[s.tif] ?? null) : null,
    activeFrom: bigOr0(s.activeFrom), expiryDaa: bigOr0(s.expiryDaa), refundTip: bigOr0(s.refundTip),
    auction, trigger, entry, pair, booked, reservedKas,
  };
}

// ------------------------------------------------------------------------------- number formatting

/** Formats sompi as a decimal KAS string without a unit, for example `12345000` as "0.12345". */
export function formatKas(sompi: bigint): string {
  return formatUnits(sompi, 8);
}

/** Formats base units as a decimal string with at most `decimals` places. Exact, zeros trimmed. */
export function formatUnits(amount: bigint, decimals: number): string {
  const neg = amount < 0n;
  const a = neg ? -amount : amount;
  if (decimals <= 0) return (neg ? '-' : '') + a.toString();
  const base = 10n ** BigInt(decimals);
  const int = a / base;
  const frac = (a % base).toString().padStart(decimals, '0').replace(/0+$/, '');
  return (neg ? '-' : '') + int.toString() + (frac ? '.' + frac : '');
}

/** Narrows a decoded state to `OrderState` for callers holding `{kind,state}`. */
export function asOrderState(s: AnyState | null | undefined): OrderState | null {
  return s && isOrderKind(s.kind) ? (s as OrderState) : null;
}
