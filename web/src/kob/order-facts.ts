// Facts that can be read straight off an order state (no protocol logic: the covenants and kob-wasm decide validity). Shared by the
// pre-sign decoder (decode.ts), the cancel builders (cancel.ts), balances and positions. DOM-free, bigint arithmetic only.
//
// Pair orders (`KobPair`, `KobCondPair`, `KobIfdPair`; protocol v3, docs/spec/order-types.md): an amount of the BASE token A at a price in the
// QUOTE token B per WHOLE A. Their states name both tokens (S / T for KobPair and KobCondPair, A / B for KobIfdPair); the helpers below read
// them oriented A / B, so for the rest of the app "the order's token" is A, its scale scale(A), its price B base units per whole A, and its tip
// KAS (sompi per whole A). The retired cross limit (`KobCross`) exists only as an order of a RETIRED template (`describeLegacy`).
import type { AnyState, BaseKind, Family, Hex, IfdAskState, IfdBidState, KasKind, LegacyState, OrderKind, OrderState, PairKind, PairTriggerRule, RetiredKind, TemplateName } from './types';
import type { KobWasm } from './wasm';
import { quoteOf } from './units';

export const ASK_KINDS: readonly BaseKind[] = ['KobAsk', 'KobCondAsk', 'KobIfdAsk'];
export const BID_KINDS: readonly BaseKind[] = ['KobBid', 'KobCondBid', 'KobIfdBid'];
/** The KAS-quoted kinds (a token against KAS), each with a KRON twin. */
export const KAS_KINDS: readonly KasKind[] = ['KobAsk', 'KobCondAsk', 'KobIfdAsk', 'KobBid', 'KobCondBid', 'KobIfdBid'];
/** The pair kinds (token A for token B): one template each for both sides and both token families (no KRON twin). */
export const PAIR_KINDS: readonly PairKind[] = ['KobPair', 'KobCondPair', 'KobIfdPair'];
export const BASE_KINDS: readonly BaseKind[] = [...ASK_KINDS, ...BID_KINDS, ...PAIR_KINDS];
/** Every order kind as kob-wasm tags it: the KAS-quoted kinds with their KRON twins and the three pair kinds. */
export const ORDER_KINDS: readonly OrderKind[] = [...BASE_KINDS, ...[...ASK_KINDS, ...BID_KINDS].map((k) => `${k}Kron` as OrderKind)];

/** A pair kind (`KobPair`, `KobCondPair`, `KobIfdPair`). */
export const isPairKind = (k: string): k is PairKind => (PAIR_KINDS as readonly string[]).includes(k);
/** Every KAS-quoted kind exists once per token family: the KRON kinds are the KCC-20 names with a `Kron` suffix. */
export const baseKind = (k: OrderKind): BaseKind => (k.endsWith('Kron') ? k.slice(0, -4) : k) as BaseKind;
/**
 * Family of an order kind or template name (KAS-quoted kinds and token programs). A pair kind names no family: its state names one per token
 * (use {@link familyOfOrder} for an order: a pair order's family is its base token A's, as kob-protocol `AnyState::family`).
 */
export const familyOfKind = (k: TemplateName | string): Family => (k.endsWith('Kron') || k.startsWith('Kron') ? 'kron' : 'kcc20');
/** The kind tag of a base kind in a family (the pair kinds have one template for every family). */
export const kindFor = (k: BaseKind, family: Family): OrderKind => (family === 'kron' && !isPairKind(k) ? (`${k}Kron` as OrderKind) : k);

export const isOrderKind = (k: string): k is OrderKind => (ORDER_KINDS as readonly string[]).includes(k);
/**
 * Ask-side KAS kinds hold the maker's tokens in one custody UTXO owned by the order (matcher.md 1.2); every pair kind may hold custodies (of A,
 * of B or both: {@link custodiesOf} says which, from the state).
 */
export const holdsTokens = (k: OrderKind): boolean => (ASK_KINDS as readonly string[]).includes(baseKind(k)) || isPairKind(k);
/** The retired cross limit (`KobCross`): only an order of a RETIRED template (decodeRetired) carries this kind. */
export const isCrossKind = (k: OrderKind | RetiredKind | string): boolean => k === 'KobCross';
/** Side of a KAS kind. A pair kind's side is in its state ({@link sideOf}); 'sell' is returned for it as a placeholder. */
export const sideOfKind = (k: OrderKind): 'sell' | 'buy' => ((ASK_KINDS as readonly string[]).includes(baseKind(k)) || isPairKind(k) ? 'sell' : 'buy');
/** Side of an order: KAS kinds by kind, pair kinds by their state (`side` 1 sells A, 2 buys A; a buy-first entry buys). */
export function sideOf(o: { kind: string; state: object }): 'sell' | 'buy' {
  if (isPairKind(o.kind)) return bigOr0((o.state as { side?: unknown }).side) === 1n ? 'sell' : 'buy';
  return sideOfKind(o.kind as OrderKind);
}
/** Conditional kinds (stop / take-profit / OCO legs): KobCondAsk, KobCondBid and the pair conditional KobCondPair. */
export const isCondKind = (k: OrderKind): boolean => baseKind(k) === 'KobCondAsk' || baseKind(k) === 'KobCondBid' || k === 'KobCondPair';
/** If-done entries: KobIfdBid, KobIfdAsk and the pair entry KobIfdPair. */
export const isIfdKind = (k: OrderKind): boolean => baseKind(k) === 'KobIfdAsk' || baseKind(k) === 'KobIfdBid' || k === 'KobIfdPair';
/** Narrowing by family-neutral kind: `isKind(o, 'KobAsk')` is true for `KobAsk` and `KobAskKron`. */
export function isKind<K extends BaseKind>(o: OrderState, k: K): o is Extract<OrderState, { kind: K | `${K}Kron` }> {
  return baseKind(o.kind) === k;
}

/** 64-bit decimal string -> bigint (tolerates numbers and bigints; never Number arithmetic on amounts). */
export const big = (v: string | number | bigint): bigint => BigInt(v);
/** A state field as a bigint, 0 when absent or malformed (a hostile state describes as 0: describing it must not throw). */
function bigOr0(v: unknown): bigint {
  if (typeof v === 'bigint') return v;
  if (typeof v === 'number' && Number.isSafeInteger(v)) return BigInt(v);
  return typeof v === 'string' && /^-?\d+$/.test(v) ? BigInt(v) : 0n;
}

// ------------------------------------------------------------------------------------------------ pair tokens

/** One token of a pair order, as its state pins it. */
export interface PairTokenFacts {
  covId: Hex;
  tplHash: Hex;
  prefixLen: bigint;
  suffixLen: bigint;
  /** the state's family code as a family (null for a code other than 1 KCC-20 / 2 KRON: a hostile state) */
  family: Family | null;
  familyCode: bigint;
  /** base units per whole token */
  scale: bigint;
  /** the KCC-20 extension commitment the state names for NEW outputs of this token (zero for KRON); null when the state names none for it */
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

/** A / B and the side of a pair order (mirrors kob-wasm `pairTokens`), null for every other kind. */
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
  const s = o.state as { tokenCovId?: Hex; bCovId?: Hex };
  // a retired cross limit (legacy layout) also owns token B
  return [s.tokenCovId ?? '', ...(isCrossKind(o.kind) && s.bCovId ? [s.bCovId] : [])].filter(Boolean);
}

/** One exact custody an order holds: the token, its amount and its role (`base` = the order's token A, `quote` = a pair order's B). */
export interface CustodyFact { token: Hex; amount: bigint; role: 'base' | 'quote' }

/**
 * The exact custodies an order holds, in record order (kob-protocol `AnyState::custodies`): a KAS ask its token (`amountLeft`); a `KobPair` /
 * `KobCondPair` its S (an ask: A, a bid: B; `custody`); a buy-first `KobIfdPair` its B escrow (when non-zero); a sell-first one its A (when
 * `amountLeft` > 0) then its B prefund (when `custody` > 0). With `kob`, kob-wasm's own answer is used.
 */
export function custodiesOf(o: OrderState, kob?: KobWasm): CustodyFact[] {
  const pf = pairFactsOf(o);
  if (kob && !isLegacyState(o)) {
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
  if (!holdsTokens(o.kind) && !isCrossKind(o.kind)) return [];
  const amount = custodyAmountOf(o);
  return amount === null ? [] : [{ token: tokenCovIdOf(o), amount, role: 'base' }];
}

/**
 * Custody token amount the order holds in its (first) custody: a KAS ask exactly `amountLeft` base units (matcher.md 1.2); a pair order its FIRST
 * custody of kob-protocol `custodies` (a sell-first entry's B prefund is the second: {@link custodiesOf}); null for KAS bid kinds and a pair order
 * without custody. An order of a RETIRED template carries its legacy lot state (`LegacyState`): its custody is `lotsLeft x lotUnits x unit`.
 */
export function custodyAmountOf(o: OrderState): bigint | null {
  if (isLegacyState(o)) return holdsTokens(o.kind) || isCrossKind(o.kind) ? retiredAmountLeft(o.state) : null;
  if (isPairKind(o.kind)) return custodiesOf(o)[0]?.amount ?? null;
  if (!holdsTokens(o.kind)) return null;
  return big((o.state as { amountLeft: string }).amountLeft);
}

/** Base units (of A for a pair order) still open (asks / entries / conditionals / pair orders); null for plain KAS bids, whose quantity is their KAS budget. */
export function amountLeftOf(o: OrderState): bigint | null {
  if (baseKind(o.kind) === 'KobBid') return null;
  if (isLegacyState(o)) return retiredAmountLeft(o.state);
  return big((o.state as { amountLeft: string }).amountLeft);
}

// ------------------------------------------------------------------------------------------------ retired (lot) layouts

/**
 * True for the legacy LOT state of an order of a RETIRED template (kob-wasm `decodeRetired`, protocol v2.3 to v2.6: `lotsLeft` lots of
 * `lotUnits x unit` base units, prices in sompi per `unit`; docs/spec/template-retirement.md). Such an order can only be cancelled by its maker.
 */
export const isLegacyState = (o: { kind?: string; state: object }): boolean =>
  ('lotUnits' in o.state && 'unit' in o.state) || (o.kind === 'KobCross' && 'amountLeft' in o.state && 'aFamily' in o.state);
/** The retired protocol v3 cross limit (no lots: `amountLeft` base units of A, `price` B base units per whole A of `scale` base units). */
const isNoLotCross = (s: object): boolean => !('lotUnits' in s) && 'amountLeft' in s && 'aFamily' in s;

/** RETIRED (lot) layouts only: base units left, `lotsLeft x lotUnits x unit` (null when the state does not carry them). */
export function retiredAmountLeft(s: object): bigint | null {
  if (isNoLotCross(s)) return big((s as { amountLeft: string }).amountLeft);
  const r = s as { lotsLeft?: string; lotUnits?: string; unit?: string };
  if (r.lotsLeft === undefined || r.lotUnits === undefined || r.unit === undefined) return null;
  return big(r.lotsLeft) * big(r.lotUnits) * big(r.unit);
}

/**
 * Describes the legacy LOT state of an order of a RETIRED template for display (an "older contract version": cancel only). Its price is sompi
 * per `unit` base units, so `unit` plays the part of the scale; its lot (`lotUnits x unit`) was its minimum fill; per-lot tips and rates are
 * shown per `unit` (rounded down). Conditional legs, if-done terms and repeats are not described. A retired cross limit (`KobCross`, token A for
 * token B) describes as a pair sell order: `pair` names A and B and its rate (B base units per `unit` base units of A).
 */
export function describeLegacy(o: LegacyState): OrderDescription {
  const s = o.state as unknown as Record<string, string | undefined>;
  const n = (k: string): bigint => (s[k] !== undefined && /^-?\d+$/.test(s[k]!) ? BigInt(s[k]!) : 0n);
  // the retired v3 cross limit has no lots: its scale is the price denominator, its minFill the minimum fill, its prices per whole A
  const noLot = isNoLotCross(o.state);
  const unit = noLot ? (n('scale') > 0n ? n('scale') : 1n) : n('unit') > 0n ? n('unit') : 1n;
  const lotUnits = noLot ? 1n : n('lotUnits') > 0n ? n('lotUnits') : 1n;
  const cross = isCrossKind(o.kind);
  const kind = o.kind;
  const left = o.kind === 'KobBid' ? null : retiredAmountLeft(o.state);
  const perUnit = (perLot: bigint): bigint => perLot / lotUnits;
  const sells = cross || (ASK_KINDS as readonly string[]).includes(o.kind);
  const aCode = n('aFamily') !== 0n ? n('aFamily') : 1n;
  const bCode = n('bFamily');
  const pair: PairTerms | null = cross
    ? {
        kind: 'KobCross', side: 'sell',
        base: { covId: o.state.tokenCovId, tplHash: o.state.tokenTplHash, prefixLen: n('tplPrefixLen'), suffixLen: n('tplSuffixLen'), family: famOf(aCode), familyCode: aCode, scale: unit, ext: null },
        quote: { covId: s.bCovId ?? '', tplHash: s.bTplHash ?? '', prefixLen: n('bPrefixLen'), suffixLen: n('bSuffixLen'), family: famOf(bCode), familyCode: bCode, scale: 0n, ext: s.bExt ?? null },
        price: noLot ? n('price') : perUnit(n('bLot')), priceEnd: n('auctionDaa') > 0n ? (noLot ? n('priceEnd') : perUnit(n('bLotEnd'))) : null, stop: 0n, custody: left ?? 0n,
        custodies: left !== null && left > 0n ? [{ token: o.state.tokenCovId, amount: left, role: 'base' }] : [], escrowA: left ?? 0n, escrowB: 0n, prefund: 0n,
        deliveryCarrier: n('deliveryCarrier'), exitCarrier: 0n, triggerRule: null,
      }
    : null;
  return {
    kind, side: sells ? 'sell' : 'buy', maker: o.state.maker, tokenCovId: o.state.tokenCovId, scale: unit, minFill: noLot ? n('minFill') : lotUnits * unit,
    amountLeft: left, tokenAmount: sells ? left : null,
    price: cross ? (noLot ? n('price') : perUnit(n('bLot'))) : n(o.kind === 'KobCondAsk' || o.kind === 'KobCondBid' ? 'tpPrice' : 'price'), tip: noLot ? n('tip') : perUnit(n('tipLot')),
    tif: s.tif !== undefined ? (TIF[s.tif] ?? null) : null, activeFrom: n('activeFrom'), expiryDaa: n('expiryDaa'), refundTip: n('refundTip'),
    auction: null, trigger: null, entry: null, pair, booked: null, reservedKas: 0n,
  };
}

/** The order's scale: base units per whole token (a pair order: per whole A), the denominator of its prices. */
export const scaleOf = (o: OrderState): bigint => {
  const pf = pairFactsOf(o);
  return pf ? pf.a.scale : big((o.state as { scale: string }).scale);
};
/** The order's minimum fill (base units), unless a fill takes everything left. */
export const minFillOf = (o: OrderState): bigint => big((o.state as { minFill: string }).minFill);

export const makerOf = (o: OrderState): Hex => o.state.maker;
/** The order's token: a KAS-quoted order's token, a pair order's base token A. */
export const tokenCovIdOf = (o: { kind: string; state: object }): Hex => pairFactsOf(o)?.a.covId ?? (o.state as { tokenCovId: Hex }).tokenCovId;
/** The order's token template hash (a pair order: A's). */
export const tokenTplHashOf = (o: { kind: string; state: object }): Hex => pairFactsOf(o)?.a.tplHash ?? (o.state as { tokenTplHash: Hex }).tokenTplHash;
/** A pair order's quote token B (null for every other kind). */
export const quoteCovIdOf = (o: { kind: string; state: object }): Hex | null => pairFactsOf(o)?.b.covId ?? null;
/** The family of an order: a KAS-quoted kind's, a pair order's base token A's (kob-protocol `AnyState::family`). */
export const familyOfOrder = (o: { kind: string; state: object }): Family => {
  const pf = pairFactsOf(o);
  return pf ? (pf.a.family ?? 'kcc20') : familyOfKind(o.kind);
};

/**
 * Extension commitment of the token of the order's FIRST custody when the state carries it: a KAS bid-side kind its token's; a sell-first
 * `KobIfdPair` holding A its `aExt`, any other `KobIfdPair` its `bExt`. KAS asks, `KobPair` and `KobCondPair` carry the ext of their custody
 * token only in the custody itself (null).
 */
export function extensionOf(o: OrderState): Hex | null {
  if (o.kind === 'KobIfdPair') {
    const s = o.state;
    return String(s.side) === '1' && bigOr0(s.amountLeft) > 0n ? s.aExt : s.bExt;
  }
  if (isPairKind(o.kind)) return null;
  return 'extensionCommitment' in o.state ? (o.state as { extensionCommitment: Hex }).extensionCommitment : null;
}

/**
 * The extension commitment a pair order's state names for NEW outputs of `token` (A or B), or null: a `KobIfdPair` names both (`aExt`, `bExt`),
 * a `KobPair` / `KobCondPair` only its bought token's (`tExt`). Null for every other kind or token.
 */
export function pairExtFor(o: OrderState, token: Hex): Hex | null {
  const pf = pairFactsOf(o);
  if (!pf) return null;
  if (token === pf.a.covId && pf.a.ext !== null) return pf.a.ext;
  if (token === pf.b.covId && pf.b.ext !== null) return pf.b.ext;
  return null;
}

export type TifName = 'gtc' | 'ioc' | 'fok';
/** Stop leg terms. Trigger (touch): arms only in a tx that fills >= `minTouch` base units of a resting order exposed >= `minRestDaa` DAA. */
export interface TriggerTerms {
  stopPrice: bigint; slipBps: bigint; armed: bigint; bandDaa: bigint; keeperTip: bigint; minRestDaa: bigint; minTouch: bigint;
  trailStep: bigint; trailGap: bigint; trailWait: bigint;
}

/**
 * The pair side of a pair order (`KobPair`, `KobCondPair`, `KobIfdPair`; or a retired cross limit, `KobCross`): both tokens, its prices in B base
 * units per WHOLE A, its custodies and its trigger rule. Amounts: A in base units of A, B in base units of B.
 */
export interface PairTerms {
  kind: PairKind | 'KobCross';
  /** sell = sells A for B (an ASK, a sell-first entry); buy = buys A with B (a BID, a buy-first entry) */
  side: 'sell' | 'buy';
  base: PairTokenFacts;
  quote: PairTokenFacts;
  /** B base units per whole A: a `KobPair`'s limit (a decay's start), a `KobIfdPair`'s limit, a `KobCondPair`'s take-profit / limit leg (0 = none) */
  price: bigint;
  /** a decaying ask's / rising bid's end price (`KobPair` with a slope; a retired auction's worst rate), else null */
  priceEnd: bigint | null;
  /** a `KobCondPair`'s stop (`stopPrice`) or a stop entry's `entryStop`; 0 = none */
  stop: bigint;
  /** the state's mutable `custody` (the exact S held; a `KobIfdPair`: its B) */
  custody: bigint;
  /** the exact custodies in record order (kob-protocol `custodies`) */
  custodies: CustodyFact[];
  /** A held in custody, B held in custody (a bid's escrow, a buy-first entry's escrow, a sell-first entry's prefund) */
  escrowA: bigint;
  escrowB: bigint;
  /** a sell-first entry's buy-back prefund, B base units per whole A (0 otherwise) */
  prefund: bigint;
  /** KAS on each delivery (a `KobIfdPair`: on each exit custody) and on each exit UTXO (if-done entries), sompi */
  deliveryCarrier: bigint;
  exitCarrier: bigint;
  /** kob-wasm `pairTriggerRule` of a stop (conditional or stop entry), when `describeOrder` had kob-wasm; null otherwise */
  triggerRule: PairTriggerRule | null;
}

export interface EntryTerms {
  /**
   * `rptAmount`: 0 = no repeat, else 1 + base units of re-arms left; `prefund`: sompi per whole token (sell-first KAS entries), B base units per
   * whole A (a sell-first `KobIfdPair`)
   */
  entryStop: bigint; rptAmount: bigint; exitCarrier: bigint; deliveryCarrier: bigint; prefund: bigint; keeperTip: bigint;
  armed: bigint; bandDaa: bigint;
  /** trigger rule of a stop entry (touch), as for a stop leg */
  minTouch: bigint; minRestDaa: bigint;
}

/**
 * Plain-data description of one order state for confirmation screens. All amounts bigint (base units); prices are sompi per whole token, a pair
 * order's B base units per whole A (its tip stays KAS, sompi per whole A).
 */
export interface OrderDescription {
  /** an order kind, or a retired kind (`KobCross`) of an order of a retired template */
  kind: OrderKind | RetiredKind;
  side: 'sell' | 'buy';
  maker: Hex;
  /** the order's token (a pair order: its base token A) */
  tokenCovId: Hex;
  /** base units per whole token (the price denominator; a pair order: scale(A)) */
  scale: bigint;
  /** smallest fill (base units) unless a fill takes everything left */
  minFill: bigint;
  /** base units left; null for KobBid (budget order) */
  amountLeft: bigint | null;
  /** custody amount of the order's token (KAS asks; a pair order: A held) */
  tokenAmount: bigint | null;
  /** limit price per whole token: ask/bid limit, conditional take-profit, if-done ENTRY limit; a pair order's B per whole A */
  price: bigint;
  /** priority tip, sompi per whole token (KAS for every kind) */
  tip: bigint;
  /** null for kinds without a tif field */
  tif: TifName | null;
  activeFrom: bigint;
  expiryDaa: bigint;
  refundTip: bigint;
  /** KobAsk / KobBid / KobPair rate-limit and decay terms (TWAP, DCA, Dutch, market auctions); null when none is set */
  auction: { interval: bigint; maxFill: bigint; slope: bigint; priceEnd: bigint; decayStep: bigint } | null;
  /** conditional (stop / trailing / take-profit) terms */
  trigger: TriggerTerms | null;
  /** if-done entry terms; `exit` is the committed exit order when it could be decoded */
  entry: (EntryTerms & { exit: OrderDescription | null }) | null;
  /** a pair order: its two tokens, prices in B, custodies and trigger rule; null for every KAS-quoted kind */
  pair: PairTerms | null;
  /** repeat fields of a booked exit (parent entry id, the entry's rate per whole token, cut-off; a pair exit's `rptPre`); null for a plain order */
  booked: { parent: Hex; rptPrice: bigint; rptUntil: bigint; rptPre?: bigint } | null;
  /** KAS reserved inside the order value that is not trading value (bid `reserve` / `deliveryCarrier`, entry carriers, prefund; a pair order's tip prefund) */
  reservedKas: bigint;
}

const TIF: Record<string, TifName> = { '0': 'gtc', '1': 'ioc', '2': 'fok' };
const ZERO32 = '00'.repeat(32);

/**
 * The exit a KAS if-done entry commits is stored as a truncated state span (`exitState`: the exit state without its repeat fields, which the entry
 * writes itself: matcher.md 1.1). To DISPLAY it we pad the missing zero repeat fields (fixed-width pushes) and let kob-wasm decode it;
 * the padded bytes are used for display only, never signed or hashed. A pair entry's exit comes from kob-wasm `ifdPairExit`.
 */
function decodeCommittedExit(kob: KobWasm, o: IfdBidState | IfdAskState, exitKind: 'KobCondAsk' | 'KobCondBid', family: Family): OrderState | null {
  const push32 = '20' + ZERO32;
  const push8 = '08' + '00'.repeat(8);
  const pad = exitKind === 'KobCondAsk' ? push32 + push8 + push8 : push32 + push8 + push8 + push8;
  try {
    return kob.decodeState(kindFor(exitKind, family), o.exitState + pad) as OrderState;
  } catch {
    return null;
  }
}

/** The pair terms of a pair order (null for every other kind). With `kob`, the custodies and the trigger rule come from kob-wasm. */
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

/** Describes an order state. With `kob`, an if-done entry's committed exit is decoded as well (and a pair order's custodies / trigger rule). */
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
  // a hostile state (negative amounts or rates, a zero scale) describes as 0 here: the decoder flags such a state, describing it must not throw
  const q = (n: bigint, r: bigint, round: 'up' | 'down'): bigint => (n >= 0n && r >= 0n && scale > 0n ? quoteOf(n, r, scale, round) : 0n);
  // a pair order's KAS is carriers (deliveries, exits) and its prefunded priority tip, floor(amountLeft x tip / scale(A)) (released to the filler
  // per fill, rounded down, the rest returned): the tip is the reserved part. A sell-first KAS entry prefunds ceil(amountLeft x prefund / scale).
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

// ------------------------------------------------------------------------------------------------ number formatting

/** `12345000` sompi -> "0.12345 KAS" style decimal string (trailing zeros trimmed, at most 8 decimals). */
export function formatKas(sompi: bigint): string {
  return formatUnits(sompi, 8);
}

/** Base units -> decimal string with `decimals` places (bigint exact, trailing zeros trimmed). */
export function formatUnits(amount: bigint, decimals: number): string {
  const neg = amount < 0n;
  const a = neg ? -amount : amount;
  if (decimals <= 0) return (neg ? '-' : '') + a.toString();
  const base = 10n ** BigInt(decimals);
  const int = a / base;
  const frac = (a % base).toString().padStart(decimals, '0').replace(/0+$/, '');
  return (neg ? '-' : '') + int.toString() + (frac ? '.' + frac : '');
}

/** Decoded state -> `AnyState` narrowing helper for callers holding `{kind,state}` from wasm or the indexer. */
export function asOrderState(s: AnyState | null | undefined): OrderState | null {
  return s && isOrderKind(s.kind) ? (s as OrderState) : null;
}
