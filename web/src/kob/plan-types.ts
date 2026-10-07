// Shared vocabulary of the order-planning layer (intent -> OrderPlan). UI-independent and DOM-free: everything here runs in node tests.
//
//   planOrder(env, intent) -> OrderPlan
//     * validates the intent against the protocol rules AND the wallet rules of docs/spec/matcher.md section 10;
//     * maps it to the exact on-chain order state(s) and a kob-wasm `createOrder` request (funding / token UTXOs selected from env);
//     * runs kob-wasm `build` (unsigned tx + signing plan) so protocol-level rejections and fee/mass are known before the user signs;
//     * returns the numbers the UI must disclose (all-in prices, tip, carriers, expiry, ...).
//   The pre-sign confirmation screen (kob/decode.ts) independently re-derives what the tx does from `built` alone.
//
// Amounts: KAS in sompi, token quantities in base units, prices in sompi per whole token (`scale` base units, see TokenMarket), all as `bigint`.
// 1 KAS = 100_000_000 sompi.
import type { FeeContext, RatePick } from './fee-policy';
import type { PairBookView } from '../data/indexer-types';
import type { BuiltTx, CreateOrderRequest, Family, FeeMode, Hex, KeyUtxo, OrderState, PairKind, PairTriggerRule, TokenProgram, TokenUtxo } from './types';
import type { KobWasm } from './wasm';

/** Market parameters of one token, derived from its registry entry (kob/registry.ts) + the embedded token program table. */
export interface TokenMarket {
  covenantId: Hex;
  ticker: string;
  decimals: number;
  program: TokenProgram;
  /** token program lineage: decides the order kinds (`KobAsk` / `KobAskKron`) and the token state layout */
  family: Family;
  templateHash: Hex;
  /** template prefix / suffix lengths (order states carry them: `tplPrefixLen`, `tplSuffixLen`) */
  prefixLen: number;
  suffixLen: number;
  /** KCC-20 extension commitment; all zeros for a KRON token (it has none) */
  extensionCommitment: Hex;
  /** slot limits of the token program: max token inputs / outputs per transfer */
  slots: { inputs: number; outputs: number };
  /**
   * The order scale: base units per whole token, `10^decimals` capped at 10^9 (kob-wasm `defaultScale`). Every order of this token carries it, and
   * every price / tip is sompi per `scale` base units (= per whole token unless decimals > 9).
   */
  scale: bigint;
  /** minimum price increment, sompi per whole token (`tick` of the registry; 1 when unset) */
  tick: bigint;
  /** default refund tip and keeper tip (sompi) of this token program (kob.keeperTips()) */
  refundTip: bigint;
  keeperTip: bigint;
}

/** UTC clock and the node's virtual DAA score, read together (matcher.md 10.10). */
export interface Clock {
  daa: bigint;
  unixSeconds: bigint;
  /** measured DAA advance in milli-DAA per second; 10_000 = nominal (clamped to 9_500..10_500 by kob.dayOrder) */
  rateMilli: number;
}

/** One aggregated price level of the visible book (`price` in sompi per whole token as posted, i.e. without tips; `amount` base units). */
export interface BookLevel { price: bigint; amount: bigint; orders: number; tip?: bigint }
/** One resting order of the NON-aggregated book (indexer orders view), for exact FOK pre-checks: price as posted, `tip` its priority tip, `minFill` its minimum fill. */
export interface BookOrder { covenantId?: Hex; price: bigint; amount: bigint; tip?: bigint; minFill?: bigint }
export interface BookView {
  /** ascending by price */
  asks: BookLevel[];
  /** descending by price */
  bids: BookLevel[];
  /** optional per-order entries (same sort order as asks / bids): when present the FOK pre-check is exact, else it is conservative */
  askOrders?: BookOrder[];
  bidOrders?: BookOrder[];
}

/** The best prices of the market as another indexer reports them (`label`: its URL). */
export interface ReferenceTouch {
  label: string;
  bestAsk: bigint | null;
  bestBid: bigint | null;
}

/** An own resting order, for client-side self-trade prevention (matcher.md 10.12). */
export interface OwnOrderRef {
  covenantId: Hex;
  side: 'sell' | 'buy';
  /** limit price, sompi per whole token; for auctions / stops the price the order can fill at (worst bound for its side) */
  price: bigint;
  tip: bigint;
  /** base units left */
  amountLeft: bigint;
  /** an armed or resting order that can currently trade (open, not expired) */
  active: boolean;
}

/** Everything a planner needs from the outside world. Built by the app from the registry, the node, the indexer and the wallet. */
export interface PlanEnv {
  kob: KobWasm;
  token: TokenMarket;
  /** the wallet's x-only public key: maker of every order, owner of funding and token inputs */
  maker: Hex;
  /** where change returns (default: maker) */
  changeTo?: Hex;
  clock: Clock;
  book: BookView;
  ownOrders: OwnOrderRef[];
  /**
   * Guards whose input could not be read (the indexer failed): `own-orders` = self-trade prevention has nothing to compare with, `book` = FOK
   * pre-check and price bands run on an empty book. Planning then reports GUARDS_UNAVAILABLE (an error) until `guardsAcknowledged` is set.
   */
  guardsUnavailable?: readonly ('own-orders' | 'book')[];
  guardsAcknowledged?: boolean;
  /** price (sompi per whole token) of the newest fill (indexer trades: a data path independent of the book), to cross-check the book reference price */
  lastFillPrice?: bigint | null;
  /**
   * Best prices (same units as `book`) other indexers report for this market (config `extraIndexerUrls`): references for the start of a market /
   * close auction that do not come from the primary indexer. Absent / empty = none configured or none answered.
   */
  referenceTouches?: readonly ReferenceTouch[];
  /**
   * How far (bps) the start of a market / close auction may be on the costly side of a reference (the last fill, another indexer's best price)
   * before planning reports MARKET_START_VS_* (an error until `marketStartAcknowledged`). Default `MARKET_START_TOLERANCE_BPS`.
   */
  marketStartToleranceBps?: bigint;
  marketStartAcknowledged?: boolean;
  /** spendable P2PK KAS UTXOs of the maker (coinbase maturity already applied by the caller) */
  funding: KeyUtxo[];
  /** spendable P2PK-owned token UTXOs of the maker for `token` */
  tokenUtxos: TokenUtxo[];
  /** sompi per mass unit (default 100 = the relay minimum). An explicit rate: the fee policy (`fees`) is then not asked. */
  feeRate?: bigint;
  /** `priority` = storage-inclusive fee (default `relay`: exactly the node's relay floor) */
  feeMode?: FeeMode;
  /** dynamic fee policy + the node's estimate (kob/fee-policy.ts): `planOrder` picks the rate of the order's urgency from it; absent = the floor */
  fees?: FeeContext;
  /** set by the planner: the pick behind `feeRate` (disclosure) */
  feePick?: RatePick;
  /** protocol defaults (carriers etc.); override in tests */
  carrier?: bigint;
}

export type IssueSeverity = 'error' | 'warning' | 'info';

/** A finding shown to the user. `code` keys the i18n message; `message` is the English fallback; `params` fill the placeholders. */
export interface PlanIssue {
  code: string;
  severity: IssueSeverity;
  message: string;
  params?: Record<string, string | number | bigint>;
  /** intent field the finding is about (for inline form errors) */
  field?: string;
}

export interface CarrierLine {
  /** i18n key suffix: 'orderCarrier' | 'tokenCarrier' | 'deliveryCarrier' | 'exitCarrier' | 'tokenChangeCarrier' | 'reserve' | ... */
  kind: string;
  /** sompi (per item; the line totals amount * count) */
  amount: bigint;
  count: number;
  /** true: the KAS sits in a UTXO the wallet itself owns (e.g. the token-change carrier) and is NOT counted in Disclosure.kasLocked */
  kept?: boolean;
}

/** Numbers the order-entry screen and the confirmation screen must show. Everything derives from the planned state, not from form text. */
export interface Disclosure {
  side: 'sell' | 'buy';
  /** token base units traded in total */
  tokenAmount: bigint;
  /** the order's scale: base units per whole token (the denominator of every price below) */
  scale: bigint;
  /** smallest fill (base units) unless a fill takes everything left: the order's `minFill` (an IOC / FOK / market order: 1) */
  minFill: bigint;
  /** stops: the smallest trigger evidence (base units, the state's `minTouch`); null for kinds without a trigger */
  minTouch: bigint | null;
  /** the order's limit (or worst bound for auctions / stops / the entry's limit), sompi per whole token */
  limitPrice: bigint | null;
  /** all-in price per whole token: buy pays limit + tip, sell receives limit - tip */
  allInPrice: bigint | null;
  /**
   * all-in total of the whole amount at the limit (KAS moved), from the covenant's quote rule: a sell RECEIVES at least ceil(amount x allIn /
   * scale), a buy PAYS at most floor(amount x allIn / scale) (kob-wasm askProceedsAt / bidSpendAt and the conditional / if-done twins)
   */
  allInTotal: bigint | null;
  /** best price expected now (the touch for market orders), sompi per whole token */
  expectedPrice: bigint | null;
  /** worst price the order can fill at (market/stop bound), sompi per whole token */
  worstPrice: bigint | null;
  /** priority tip, sompi per whole token */
  tip: bigint;
  /** KAS locked by this transaction beyond fees, itemised. Carrier KAS is returned on cancel/refund/fill (i18n explains). */
  carriers: CarrierLine[];
  /** total KAS leaving the wallet's spendable balance into covenants (sum of carriers + escrow), excluding the network fee */
  kasLocked: bigint;
  /** tokens moved into 0x04 custody (base units); 0 for bids */
  tokensEscrowed: bigint;
  /** network fee of the placement tx, sompi (from the built tx) */
  fee: bigint | null;
  /** refund tip and keeper tip reserved in the order, sompi */
  refundTip: bigint;
  keeperTip: bigint;
  /** expiry: on-chain DAA, approximate wall clock (unix seconds), kind */
  expiry: { kind: 'gtc' | 'gtd' | 'day' | 'ioc' | 'fok' | 'none'; daa: bigint | null; approxUnixSeconds: bigint | null; deadlineUnixSeconds: bigint | null };
  /** timed activation: order starts at this DAA / approx unix seconds */
  activatesAt: { daa: bigint; approxUnixSeconds: bigint } | null;
  /** short machine-readable tags for extra disclosures the UI turns into sentences (e.g. 'auction', 'triggerExposure', 'repeatReBuys') */
  notes: string[];
}

export interface OrderPlan {
  /** true when there are no errors and the tx was built (`built` present) */
  ok: boolean;
  issues: PlanIssue[];
  /** the orders that will exist after the tx (an IFD entry lists its committed exit last) */
  states: OrderState[];
  request: CreateOrderRequest | null;
  built: BuiltTx | null;
  disclosure: Disclosure | null;
}

export const errors = (p: { issues: PlanIssue[] }): PlanIssue[] => p.issues.filter((i) => i.severity === 'error');

// ------------------------------------------------------------------------------------------------ token/token pairs (A/B)
//
// A pair A/B: `PlanEnv.token` is A (the BASE), `PairEnvPart.quote` is B (the QUOTE). Amounts are base units of A; prices are B BASE UNITS PER
// WHOLE A (`scale(A)` base units of A); tips, keeper tips and carriers are KAS (sompi; `tip` = sompi per whole A, never part of a B price).

/** A token/token pair A/B: env.token is A (base); prices in env.book / ownOrders / intents are B base units per WHOLE A. */
export interface PairEnvPart {
  /** B */
  quote: TokenMarket;
  /** the maker's P2PK-owned B UTXOs (env.tokenUtxos are A's) */
  quoteTokenUtxos: TokenUtxo[];
  /** raw pair book (sources direct / entry / route), null = unavailable */
  view: PairBookView | null;
  /** KAS reference of A (sompi per whole A): the default minimum fill (the amount of A worth 10 KAS); null = unknown */
  kasPerWholeA: bigint | null;
  /** KAS reference of B (sompi per whole B) */
  kasPerWholeB: bigint | null;
}

/**
 * The environment of a pair order. `book` is the pair book as a BookView in B per whole A (asks rounded UP, bids DOWN, amounts base units of A,
 * no tips: the KAS tip never changes a B price), so the shared guards (crossing touch, reference price, FOK pre-check, price bands) work
 * unchanged; `ownOrders` are the wallet's live pair orders of this pair oriented A/B (prices B per whole A, `tip` 0). `lastFillPrice` (when set)
 * is the rate the newest KAS trades of A and B imply (B per whole A): pair fills never make a price.
 */
export interface PairPlanEnv extends PlanEnv { pair: PairEnvPart }

export const isPairEnv = (e: PlanEnv): e is PairPlanEnv => 'pair' in e && !!(e as PairPlanEnv).pair;

/** One token of a pair as the disclosures name it. */
export interface PairSideFacts {
  covenantId: Hex;
  ticker: string;
  decimals: number;
  /** base units per whole token */
  scale: bigint;
  family: Family;
  program: TokenProgram;
}

/**
 * The pair-specific numbers of a planned pair order (`PairOrderPlan.pair`). Every amount is the covenant's exact rounded amount from kob-wasm:
 * B base units, A base units, KAS sompi. Prices are B base units per whole A.
 */
export interface PairDisclosure {
  kind: PairKind;
  /** sell = sell A for B (ASK; a sell-first entry), buy = buy A with B (BID; a buy-first entry) */
  side: 'sell' | 'buy';
  base: PairSideFacts;
  quote: PairSideFacts;
  /** A base units traded (an if-done entry: one cycle) */
  amount: bigint;
  /** A moved into custody (an ask, a sell-first entry: the amount; else 0) */
  escrowA: bigint;
  /** B moved into custody (a bid's or conditional bid's escrow, a buy-first entry's escrow, a sell-first entry's prefund; else 0) */
  escrowB: bigint;
  /** sell: B received at least for the whole amount at the worst price (kob-wasm, rounded UP); null for a buy */
  receiveMinB: bigint | null;
  /** buy: B paid at most for the whole amount at the worst price (kob-wasm, rounded DOWN); null for a sell */
  payMaxB: bigint | null;
  /** B of the whole amount at the expected price (an auction's start, a market order's touch), rounded like the side; null when none */
  expectedB: bigint | null;
  /** B of one minimum fill at the limit (rounded like the side) */
  minFillB: bigint | null;
  /** KAS tip prefunded on the order UTXO for the whole amount, floor(amount * tip / scale(A)) */
  tipKasTotal: bigint;
  /** fills the order UTXO budgets a delivery carrier for (an if-done entry: its possible fills) */
  deliveries: bigint;
  /** KAS on each token delivery (an entry: on each exit's custody) */
  deliveryCarrier: bigint;
  /** an if-done entry: KAS on each exit UTXO; null otherwise */
  exitCarrier: bigint | null;
  /** KAS on the order UTXO (carriers, tip prefund, keeper reserve): at least kob-wasm `minOrderValue` */
  orderValue: bigint;
  /** kob-wasm `pairTriggerRule` of the conditional / stop entry (which resting sides arm and trail it); null without a stop */
  trigger: PairTriggerRule | null;
  /**
   * an if-done entry's exit: its legs (B per whole A) and what each exit holds: `baseAmount` (buy-first: the n of A the fill bought) or
   * `proceedsPlusPrefund` (sell-first: the B proceeds of the fill plus its prefund)
   */
  exit: { kind: 'takeProfit' | 'stop' | 'oco'; takeProfit: bigint | null; stop: bigint | null; custodyPerFill: string } | null;
  /**
   * repeat entries: `count` = K re-arms per base unit (the cap when unlimited), `levels` = ladder levels this plan covers (always 1: the ticket
   * expands a ladder into one plan per level), `rptAmount` the state field (1 + K * amount), `unlimited` when no K was given
   */
  repeat: { count: bigint; levels: bigint; rptAmount: bigint; unlimited: boolean } | null;
  /** tags for extra sentences: 'pairAuction', 'pairNetting', 'pairRoute', 'pairInventory', 'pairPricesFromKasBooks', 'pairTipKas', ... */
  notes: string[];
}

/** The plan of a pair order: the generic disclosure in the pair meaning (see `planPair`) plus the pair numbers. */
export interface PairOrderPlan extends OrderPlan {
  pair: PairDisclosure | null;
}
