// Shared vocabulary of the order-planning layer (intent -> OrderPlan). UI-independent and DOM-free,
// so everything here runs in node tests.
//
// `planOrder(env, intent)` returns an `OrderPlan`. It does four things.
//
// - It validates the intent against the protocol rules and the wallet rules of docs/spec/matcher.md
//   section 10.
// - It maps the intent to the exact on-chain order states and a kob-wasm `createOrder` request,
//   with funding and token UTXOs selected from the env.
// - It runs kob-wasm `build` (unsigned tx and signing plan), so protocol-level rejections, fee and
//   mass are known before the user signs.
// - It returns the numbers the UI must disclose, such as all-in prices, tip, carriers and expiry.
//
// The pre-sign checks (kob/decode.ts, ui/confirm/send.ts) re-derive what the tx does from `built`
// alone.
//
// Amounts are `bigint`. KAS is in sompi (1 KAS = 100_000_000 sompi), token quantities are in base
// units, and prices are in sompi per whole token (`scale` base units, see `TokenMarket`).
import type { FeeContext, RatePick } from './fee-policy';
import type { PairBookView } from '../data/indexer-types';
import type { BuiltTx, CreateOrderRequest, Family, FeeMode, Hex, KeyUtxo, OrderState, PairKind, PairTriggerRule, TokenProgram, TokenUtxo } from './types';
import type { KobWasm } from './wasm';

/**
 * Market parameters of one token, from its registry entry (kob/registry.ts) and the embedded token
 * program table.
 */
export interface TokenMarket {
  covenantId: Hex;
  ticker: string;
  decimals: number;
  program: TokenProgram;
  /**
   * Token program lineage. It decides the order kinds (`KobAsk` or `KobAskKron`) and the token
   * state layout.
   */
  family: Family;
  templateHash: Hex;
  /**
   * Template prefix and suffix lengths. Order states carry them as `tplPrefixLen` and
   * `tplSuffixLen`.
   */
  prefixLen: number;
  suffixLen: number;
  /** KCC-20 extension commitment. All zeros for a KRON token, which has none. */
  extensionCommitment: Hex;
  /** Slot limits of the token program: the most token inputs and outputs per transfer. */
  slots: { inputs: number; outputs: number };
  /**
   * The order scale: base units per whole token, `10^decimals` capped at 10^9 (kob-wasm
   * `defaultScale`). Every order of this token carries it, and every price and tip is sompi per
   * `scale` base units, which is per whole token unless decimals exceed 9.
   */
  scale: bigint;
  /** Minimum price increment in sompi per whole token (`tick` of the registry, 1 when unset). */
  tick: bigint;
  /** Default refund tip and keeper tip in sompi for this token program (`kob.keeperTips()`). */
  refundTip: bigint;
  keeperTip: bigint;
  /**
   * True when the registry checked every genesis output of the token (`genesis_verified: true`).
   * For a KCC-20 token this proves no unit of it was minted outside its program. Absent or false
   * (every open-list token) means not verified, so trades of units minted outside the program can
   * arm a stop of this token (stop leg, trailing stop or stop entry). Planning warns with
   * STOP_EVIDENCE_GENESIS_UNVERIFIED.
   */
  genesisVerified?: boolean;
}

/**
 * UTC clock and the node's virtual DAA score, read together (docs/spec/matcher.md section 10, rule
 * 10).
 */
export interface Clock {
  daa: bigint;
  unixSeconds: bigint;
  /**
   * Measured DAA advance in milli-DAA per second. 10_000 is nominal, and kob.dayOrder clamps it to
   * 9_500..10_500.
   */
  rateMilli: number;
}

/**
 * One aggregated price level of the visible book. `price` is sompi per whole token as posted,
 * without tips, and `amount` is in base units.
 */
export interface BookLevel {
  price: bigint;
  amount: bigint;
  orders: number;
  tip?: bigint;
  /**
   * Pair book only. How many of `orders` are KAS orders of route levels, which route through the
   * two KAS books, and how many route levels of the indexer there are. Each route level combines at
   * least one KAS order of each token. Only the traded token's orders use its slots, which is the
   * count the FOK pre-check uses.
   */
  routeOrders?: number;
  routeRows?: number;
}
/**
 * One resting order of the non-aggregated book (indexer orders view), for exact FOK pre-checks.
 * `price` is as posted, `tip` is the priority tip and `minFill` the minimum fill.
 */
export interface BookOrder { covenantId?: Hex; price: bigint; amount: bigint; tip?: bigint; minFill?: bigint }
/** The visible order book of one market. */
export interface BookView {
  /** ascending by price */
  asks: BookLevel[];
  /** descending by price */
  bids: BookLevel[];
  /**
   * Optional per-order entries in the same sort order as asks and bids. When present the FOK
   * pre-check is exact, otherwise it is conservative.
   */
  askOrders?: BookOrder[];
  bidOrders?: BookOrder[];
}

/**
 * A book order the node did not confirm (app/book-verify.ts). It is left out of `PlanEnv.book`.
 *
 * - `not-on-node`: no unspent UTXO at the script of the state the indexer reported, so the order is
 *   a phantom or spent.
 * - `changed`: the node's order differs from the book row in price, side, scale, value or tokens.
 * - `unproven`: the indexer gave no state to check.
 * - `unchecked`: the order is behind a side with no confirmed order, or listed without orders.
 */
export interface DroppedBookOrder {
  covenantId: Hex;
  /** The side of the order. An ask sells the token. */
  side: 'sell' | 'buy';
  price: bigint;
  reason: 'not-on-node' | 'changed' | 'unproven' | 'unchecked' | 'other-market';
}

/** The best prices of the market as another indexer reports them (`label`: its URL). */
export interface ReferenceTouch {
  label: string;
  bestAsk: bigint | null;
  bestBid: bigint | null;
}

/**
 * An own resting order, for client-side self-trade prevention (docs/spec/matcher.md section 10,
 * rule 12).
 */
export interface OwnOrderRef {
  covenantId: Hex;
  side: 'sell' | 'buy';
  /**
   * Limit price in sompi per whole token. For auctions and stops it is the price the order can fill
   * at, the worst bound for its side.
   */
  price: bigint;
  tip: bigint;
  /** Base units left. */
  amountLeft: bigint;
  /** True for an armed or resting order that can currently trade (open, not expired). */
  active: boolean;
}

/**
 * Everything a planner needs from the outside world, built by the app from the registry, the node,
 * the indexer and the wallet.
 */
export interface PlanEnv {
  kob: KobWasm;
  token: TokenMarket;
  /**
   * The wallet's x-only public key. It is the maker of every order and owns the funding and token
   * inputs.
   */
  maker: Hex;
  /** Where change returns. Defaults to the maker. */
  changeTo?: Hex;
  clock: Clock;
  book: BookView;
  ownOrders: OwnOrderRef[];
  /**
   * Guards whose input could not be read because the indexer failed. `own-orders` means self-trade
   * prevention has nothing to compare with. `book` means the FOK pre-check and the price bands run
   * on an empty book. Planning then reports GUARDS_UNAVAILABLE, an error, and the order waits for
   * the data.
   */
  guardsUnavailable?: readonly ('own-orders' | 'book')[];
  /**
   * Price in sompi per whole token of the newest fill. Indexer trades are a data path independent
   * of the book, so it cross-checks the book reference price.
   */
  lastFillPrice?: bigint | null;
  /**
   * Best prices, in the units of `book`, that other indexers report for this market (config
   * `extraIndexerUrls`). They are references for the start of a market or close auction that do not
   * come from the primary indexer. Absent or empty means none is configured or none answered.
   */
  referenceTouches?: readonly ReferenceTouch[];
  /**
   * Labels of further indexers configured as references that did not answer. Planning says so in a
   * warning and never drops them silently.
   */
  referencesUnavailable?: readonly string[];
  /**
   * How far, in bps, the start of a market or close auction may be on the costly side of another
   * indexer's best price before planning reports MARKET_START_VS_INDEXER, an error. Defaults to
   * `MARKET_START_TOLERANCE_BPS`.
   */
  marketStartToleranceBps?: bigint;
  /**
   * Spendable P2PK KAS UTXOs of the maker, with coinbase maturity already applied by the caller.
   */
  funding: KeyUtxo[];
  /** Spendable P2PK-owned token UTXOs of the maker for `token`. */
  tokenUtxos: TokenUtxo[];
  /**
   * Sompi per mass unit, 100 (the relay minimum) by default. An explicit rate means the fee policy
   * (`fees`) is not asked.
   */
  feeRate?: bigint;
  /**
   * `priority` means a storage-inclusive fee. The default is `relay`, exactly the node's relay
   * floor.
   */
  feeMode?: FeeMode;
  /**
   * Dynamic fee policy and the node's estimate (kob/fee-policy.ts). `planOrder` picks the rate of
   * the order's urgency from it. Absent means the floor.
   */
  fees?: FeeContext;
  /** Set by the planner: the pick behind `feeRate`, for the disclosure. */
  feePick?: RatePick;
  /** Protocol default for carriers. Tests override it. */
  carrier?: bigint;
}

/** How serious a plan finding is. */
export type IssueSeverity = 'error' | 'warning' | 'info';

/**
 * A finding shown to the user. `code` keys the i18n message, `message` is the English fallback and
 * `params` fill the placeholders.
 */
export interface PlanIssue {
  code: string;
  severity: IssueSeverity;
  message: string;
  params?: Record<string, string | number | bigint>;
  /** The intent field the finding is about, for inline form errors. */
  field?: string;
}

/** One itemised line of the KAS a transaction locks in carriers. */
export interface CarrierLine {
  /**
   * i18n key suffix, such as 'orderCarrier', 'tokenCarrier', 'deliveryCarrier', 'exitCarrier',
   * 'tokenChangeCarrier' or 'reserve'.
   */
  kind: string;
  /** Sompi per item. The line totals `amount * count`. */
  amount: bigint;
  count: number;
  /**
   * True when the KAS sits in a UTXO the wallet itself owns, such as the token-change carrier. It
   * is not counted in `Disclosure.kasLocked`.
   */
  kept?: boolean;
}

/**
 * Numbers the order-entry screen must show. They derive from the planned state, not from form text.
 */
export interface Disclosure {
  side: 'sell' | 'buy';
  /** Token base units traded in total. */
  tokenAmount: bigint;
  /** The order's scale: base units per whole token, the denominator of every price below. */
  scale: bigint;
  /**
   * Smallest fill in base units unless a fill takes everything left. It is the order's `minFill`,
   * and 1 for an IOC, FOK or market order.
   */
  minFill: bigint;
  /**
   * Stops only: the smallest trigger evidence in base units (the state's `minTouch`). Null for
   * kinds without a trigger.
   */
  minTouch: bigint | null;
  /**
   * The order's limit in sompi per whole token. For auctions, stops and an entry it is the worst
   * bound.
   */
  limitPrice: bigint | null;
  /**
   * All-in price per whole token. A buy pays limit plus tip and a sell receives limit minus tip.
   */
  allInPrice: bigint | null;
  /**
   * All-in total of the whole amount at the limit, in KAS moved. It follows the covenant's quote
   * rule. A sell receives at least ceil(amount x allIn / scale) and a buy pays at most floor(amount
   * x allIn / scale) (kob-wasm `askProceedsAt` and `bidSpendAt`, and the conditional and if-done
   * twins).
   */
  allInTotal: bigint | null;
  /** Best price expected now, in sompi per whole token. The touch for market orders. */
  expectedPrice: bigint | null;
  /** Worst price the order can fill at (market or stop bound), in sompi per whole token. */
  worstPrice: bigint | null;
  /** Priority tip in sompi per whole token. */
  tip: bigint;
  /**
   * KAS locked by this transaction beyond fees, itemised. Carrier KAS returns on cancel, refund and
   * fill.
   */
  carriers: CarrierLine[];
  /**
   * Total KAS leaving the wallet's spendable balance into covenants (carriers plus escrow),
   * excluding the network fee.
   */
  kasLocked: bigint;
  /** Tokens moved into 0x04 custody in base units. 0 for bids. */
  tokensEscrowed: bigint;
  /** Network fee of the placement tx in sompi, from the built tx. */
  fee: bigint | null;
  /** Refund tip and keeper tip reserved in the order, in sompi. */
  refundTip: bigint;
  keeperTip: bigint;
  /** Expiry as the on-chain DAA, the approximate wall clock in unix seconds, and the kind. */
  expiry: { kind: 'gtc' | 'gtd' | 'day' | 'ioc' | 'fok' | 'none'; daa: bigint | null; approxUnixSeconds: bigint | null; deadlineUnixSeconds: bigint | null };
  /** Timed activation. The order starts at this DAA and approximate unix seconds. */
  activatesAt: { daa: bigint; approxUnixSeconds: bigint } | null;
  /**
   * Short machine-readable tags for extra disclosures that the UI turns into sentences, such as
   * 'auction', 'triggerExposure' or 'repeatReBuys'.
   */
  notes: string[];
}

/** The result of planning one order. */
export interface OrderPlan {
  /** True when there are no errors and the tx was built (`built` is present). */
  ok: boolean;
  issues: PlanIssue[];
  /** The orders that exist after the tx. An if-done entry lists its committed exit last. */
  states: OrderState[];
  request: CreateOrderRequest | null;
  built: BuiltTx | null;
  disclosure: Disclosure | null;
  /**
   * Set for an order that takes liquidity now (market, IOC, a buy that crosses, a rising bid): its
   * fills against the visible book, also on a failed build.
   */
  fills?: PlanFills;
}

/**
 * The fills of an order that takes liquidity, against the visible book (guards.ts `estimateFills`).
 * The ticket says this next to its Fills box and warns about it. One transaction takes at most
 * `perTx` counterparties: asks (its token inputs) for a buy, bids (its token outputs) for a sell.
 */
export interface PlanFills {
  /**
   * The fills the order takes: a buy's budgeted delivery carriers, a sell's allowed transactions (1
   * for an IOC or a one-transaction market).
   */
  budgeted: bigint;
  /**
   * Transactions the visible book needs for the amount (for a resting buy, its part that crosses
   * now). 0 when nothing crosses.
   */
  needed: number;
  perTx: number;
  counterparty: 'asks' | 'bids';
  /**
   * Base units the budgeted fills can take from the visible book now, the whole amount when they
   * suffice.
   */
  fillable: bigint;
  /** True when the visible book holds the whole amount within the order's bound. */
  enough: boolean;
  /**
   * KAS a buy deposits for its delivery carriers (`budgeted` times its carrier). 0 for a sell,
   * which has no carrier per fill.
   */
  carrierDeposit: bigint;
  /**
   * 'amount' counts the whole amount (market, IOC). 'crossing' counts the part of a resting buy
   * that crosses now.
   */
  scope: 'amount' | 'crossing';
  /**
   * A buy only: the most the order's escrow lets the fills deliver (kob-wasm `bidBuyingPower` of
   * the escrow, the covenant's remaining buying power). A bid's quantity is its KAS escrow, not an
   * amount. The delivery carriers budgeted for fills that do not happen buy tokens at the bound
   * price, so a book that lets one fill take everything delivers up to `amount + (budgeted - 1) x
   * carrier / price`. This exceeds `amount` only for a multi-fill market buy, and the spend never
   * exceeds the KAS locked.
   */
  maxReceive?: bigint;
  /**
   * Multi-fill market order: the Fills value the user asked for, when the minimum fill allows fewer
   * (`budgeted` is then the clamp).
   */
  requested?: bigint;
  /**
   * Multi-fill market order: its minimum fill in base units. It is the user's, else the smaller of
   * the 10 KAS default and the smallest crossing counterparty.
   */
  minFill?: bigint;
}

/** The issues of a plan that have severity 'error'. */
export const errors = (p: { issues: PlanIssue[] }): PlanIssue[] => p.issues.filter((i) => i.severity === 'error');

// ------------------------------------------------------------------------- Token/token pairs (A/B)
//
// In a pair A/B, `PlanEnv.token` is A (the base) and `PairEnvPart.quote` is B (the quote). Amounts
// are base units of A. Prices are B base units per whole A (`scale(A)` base units of A). Tips,
// keeper tips and carriers are KAS in sompi, and `tip` is sompi per whole A, never part of a B
// price.

/**
 * The pair part of a token/token environment. `env.token` is A (base), and prices in `env.book`,
 * `ownOrders` and intents are B base units per whole A.
 */
export interface PairEnvPart {
  /** B */
  quote: TokenMarket;
  /** the maker's P2PK-owned B UTXOs (env.tokenUtxos are A's) */
  quoteTokenUtxos: TokenUtxo[];
  /** Raw pair book (sources direct, entry and route). Null when unavailable. */
  view: PairBookView | null;
  /**
   * KAS reference of A in sompi per whole A, used for the default minimum fill (the amount of A
   * worth 10 KAS). Null when unknown.
   */
  kasPerWholeA: bigint | null;
  /** KAS reference of B in sompi per whole B. */
  kasPerWholeB: bigint | null;
}

/**
 * The environment of a pair order.
 *
 * `book` is the pair book as a `BookView` in B per whole A. Asks are rounded up, bids down, amounts
 * are base units of A and there are no tips, because the KAS tip never changes a B price. The
 * shared guards (crossing touch, reference price, FOK pre-check, price bands) therefore work
 * unchanged. `ownOrders` are the wallet's live pair orders of this pair, oriented A/B, with prices
 * in B per whole A and `tip` 0. `lastFillPrice`, when set, is the rate the newest KAS trades of A
 * and B imply, in B per whole A, because pair fills never make a price.
 */
export interface PairPlanEnv extends PlanEnv { pair: PairEnvPart }

/** True when the env is a pair environment. */
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
 * The pair-specific numbers of a planned pair order (`PairOrderPlan.pair`).
 *
 * Every amount is the covenant's exact rounded amount from kob-wasm, in B base units, A base units
 * or KAS sompi. Prices are B base units per whole A.
 */
export interface PairDisclosure {
  kind: PairKind;
  /**
   * `sell` sells A for B (an ASK, or a sell-first entry). `buy` buys A with B (a BID, or a
   * buy-first entry).
   */
  side: 'sell' | 'buy';
  base: PairSideFacts;
  quote: PairSideFacts;
  /** A base units traded. For an if-done entry, one cycle. */
  amount: bigint;
  /** A moved into custody: the amount for an ask or a sell-first entry, else 0. */
  escrowA: bigint;
  /**
   * B moved into custody: a bid's or conditional bid's escrow, a buy-first entry's escrow or a
   * sell-first entry's prefund, else 0.
   */
  escrowB: bigint;
  /**
   * Sell only: the B received at least for the whole amount at the worst price (kob-wasm, rounded
   * up). Null for a buy.
   */
  receiveMinB: bigint | null;
  /**
   * Buy only: the B paid at most for the whole amount at the worst price (kob-wasm, rounded down).
   * Null for a sell.
   */
  payMaxB: bigint | null;
  /**
   * B of the whole amount at the expected price (an auction's start, a market order's touch),
   * rounded like the side. Null when none.
   */
  expectedB: bigint | null;
  /** B of one minimum fill at the limit, rounded like the side. */
  minFillB: bigint | null;
  /** KAS tip prefunded on the order UTXO for the whole amount, `floor(amount * tip / scale(A))`. */
  tipKasTotal: bigint;
  /**
   * Fills the order UTXO budgets a delivery carrier for. For an if-done entry, its possible fills.
   */
  deliveries: bigint;
  /** KAS on each token delivery. For an entry, on each exit's custody. */
  deliveryCarrier: bigint;
  /** If-done entry only: KAS on each exit UTXO. Null otherwise. */
  exitCarrier: bigint | null;
  /**
   * KAS on the order UTXO (carriers, tip prefund, keeper reserve), at least kob-wasm
   * `minOrderValue`.
   */
  orderValue: bigint;
  /**
   * kob-wasm `pairTriggerRule` of the conditional or stop entry, which says what resting sides arm
   * and trail it. Null without a stop.
   */
  trigger: PairTriggerRule | null;
  /**
   * An if-done entry's exit: its legs (B per whole A) and what each exit holds. `custodyPerFill` is
   * `baseAmount` for a buy-first entry (the n of A the fill bought) and `proceedsPlusPrefund` for a
   * sell-first entry (the B proceeds of the fill plus its prefund).
   */
  exit: { kind: 'takeProfit' | 'stop' | 'oco'; takeProfit: bigint | null; stop: bigint | null; custodyPerFill: string } | null;
  /**
   * Repeat entries only. `count` is K re-arms per base unit, capped when unlimited. `levels` is the
   * ladder levels this plan covers, always 1 because the ticket expands a ladder into one plan per
   * level. `rptAmount` is the state field (1 + K * amount). `unlimited` is true when no K was
   * given.
   */
  repeat: { count: bigint; levels: bigint; rptAmount: bigint; unlimited: boolean } | null;
  /**
   * Tags for extra sentences, such as 'pairAuction', 'pairNetting', 'pairRoute', 'pairInventory',
   * 'pairPricesFromKasBooks' or 'pairTipKas'.
   */
  notes: string[];
}

/**
 * The plan of a pair order: the generic disclosure in the pair meaning (see `planPair`) plus the
 * pair numbers.
 */
export interface PairOrderPlan extends OrderPlan {
  pair: PairDisclosure | null;
}
