// JSON types of the kob-wasm boundary. They mirror the serde types in
// crates/kob-protocol/src/json.rs.
//
// Conventions, the same as on the Rust side.
//   * Byte strings (hashes, keys, scripts, state spans) are lower-case hex strings.
//   * 64-bit integers are decimal strings (`I64` / `U64`) so JavaScript never loses precision.
//     Inputs also accept JSON numbers. Outputs are always strings. Arithmetic uses `bigint`
//     (see units.ts).
//   * Small integers (u8/u16/u32, indices, positions) are plain numbers.

/** Lower-case hex string of bytes. */
export type Hex = string;
/** Decimal string of a signed 64-bit integer. */
export type I64 = string;
/** Decimal string of an unsigned 64-bit integer (sompi, DAA scores, unix seconds). */
export type U64 = string;

// ------------------------------------------------------------------------------------ order states

/**
 * Token program lineage of an order or token.
 * `kcc20` has a 112-byte token state and `kron` a 46-byte one. The order layout rules are the
 * same. Only templates, token states and token input authorisation differ.
 */
export type Family = 'kcc20' | 'kron';
/**
 * The pair order kinds. Each trades token A for token B at a price in B per whole A.
 * One template per kind serves both sides and both token families.
 */
export type PairKind = 'KobPair' | 'KobCondPair' | 'KobIfdPair';
/** The KAS-quoted order kinds (a token against KAS), family-neutral. */
export type KasKind = 'KobAsk' | 'KobBid' | 'KobCondAsk' | 'KobCondBid' | 'KobIfdBid' | 'KobIfdAsk';
/** The order kinds, family-neutral (what the UI and the planners reason about). */
export type BaseKind = KasKind | PairKind;
/**
 * Order kinds as kob-wasm tags them.
 * The KRON family has its own templates of the KAS kinds (`KobAskKron`, ...). Each pair kind has
 * one template, because its state names both families.
 */
export type OrderKind = BaseKind | `${KasKind}Kron`;
/**
 * The id_type 0 builds of the KRON kinds (`KobAskKronD0`, ...).
 * kob-wasm instantiates them for a KRON program whose holders are id_type 0.
 * Their states are the KRON kinds' states, which `AnyState` tags `KobAskKron`, ...
 * Only plans and records name the build.
 */
export type KronD0Template = `${KasKind}KronD0`;
/** Any template kob-wasm instantiates: an order kind, a KRON id_type 0 build or a token program. */
export type TemplateName = OrderKind | KronD0Template | TokenProgram;
// The token programs are declared once in crates/kob-protocol/data/programs.json.
// scripts/gen-programs.mjs generates programs.generated from it.
import type { Kcc20Program, KronProgram, TokenProgram } from './programs.generated';
export type { Kcc20Program, KronProgram, TokenProgram };

/** State of a plain ask (`KobAsk`): an amount of a token offered for KAS. */
export interface AskState {
  maker: Hex; tokenCovId: Hex; tokenTplHash: Hex; tplPrefixLen: I64; tplSuffixLen: I64;
  /** Base units per whole token (`10^decimals`, at most 10^9): the price denominator. */
  scale: I64;
  /** Smallest fill in base units, unless the fill takes everything left. */
  minFill: I64;
  /** Sompi per whole token (`scale` base units); a decay's start price. */
  price: I64;
  /** Priority tip, sompi per whole token. */
  tip: I64;
  /** 0 GTC/GTD, 1 IOC, 2 FOK. */
  tif: I64;
  activeFrom: I64; expiryDaa: I64; refundTip: I64;
  /** TWAP: most base units per fill (0 = off). */
  interval: I64; maxFill: I64; slope: I64; priceEnd: I64; decayStep: I64;
  /** Base units in custody (the custody holds exactly this). */
  amountLeft: I64;
  /** KCC-20 extension commitment of the custody (zero for KRON). The order takes no other one. */
  extensionCommitment: Hex;
}
/** State of a plain bid (`KobBid`): a KAS escrow that buys a token. */
export interface BidState {
  maker: Hex; tokenCovId: Hex; tokenTplHash: Hex; tplPrefixLen: I64; tplSuffixLen: I64; extensionCommitment: Hex;
  scale: I64;
  /** Smallest fill in base units (> 0), unless under one minimum fill of buying power remains. */
  minFill: I64; price: I64; tip: I64; tif: I64;
  activeFrom: I64; expiryDaa: I64; refundTip: I64; reserve: I64; deliveryCarrier: I64;
  /** DCA: most base units per fill (0 = off). */
  interval: I64; maxFill: I64; slope: I64; priceEnd: I64; decayStep: I64;
}
/** State of a conditional ask (`KobCondAsk`): stop, take-profit or trailing exit of a token. */
export interface CondAskState {
  maker: Hex; tokenCovId: Hex; tokenTplHash: Hex; tplPrefixLen: I64; tplSuffixLen: I64;
  scale: I64; minFill: I64; tip: I64; activeFrom: I64; expiryDaa: I64; refundTip: I64;
  tpPrice: I64; stopPrice: I64; slipBps: I64; trailStep: I64; trailGap: I64; trailWait: I64;
  /**
   * Smallest trigger evidence, in base units of a plain order of the same scale that is filled in
   * the same transaction.
   */
  minTouch: I64; minRestDaa: I64; armed: I64; bandDaa: I64; keeperTip: I64; amountLeft: I64;
  /** Repeat IFD: the entry's budget rate (sompi per whole token) returned to it. */
  parent: Hex; rptPrice: I64; rptUntil: I64;
  /**
   * KCC-20 extension commitment of the custody (zero for KRON).
   * An if-done exit takes its entry's value.
   */
  extensionCommitment: Hex;
}
/** State of a conditional bid (`KobCondBid`): a KAS escrow with a stop, take-profit or trail. */
export interface CondBidState {
  maker: Hex; tokenCovId: Hex; tokenTplHash: Hex; tplPrefixLen: I64; tplSuffixLen: I64; extensionCommitment: Hex;
  scale: I64; minFill: I64; tip: I64; activeFrom: I64; expiryDaa: I64; refundTip: I64; deliveryCarrier: I64;
  tpPrice: I64; stopPrice: I64; slipBps: I64; trailStep: I64; trailGap: I64; trailWait: I64;
  minTouch: I64; minRestDaa: I64; amountLeft: I64; armed: I64; bandDaa: I64; keeperTip: I64;
  /** Repeat IFD: the entry's proceeds rate (`price - tip`) and prefund rate, sompi per token. */
  parent: Hex; rptPrice: I64; rptPre: I64; rptUntil: I64;
}
/** State of an if-done bid entry (`KobIfdBid`): buys a token, then places a conditional ask. */
export interface IfdBidState {
  maker: Hex; tokenCovId: Hex; tokenTplHash: Hex; tplPrefixLen: I64; tplSuffixLen: I64; extensionCommitment: Hex;
  scale: I64; amountLeft: I64; price: I64; tip: I64; activeFrom: I64; expiryDaa: I64; refundTip: I64;
  deliveryCarrier: I64; exitCarrier: I64; minFill: I64; entryStop: I64; bandDaa: I64;
  minTouch: I64; minRestDaa: I64; keeperTip: I64; armed: I64;
  /** Repeat: 0 = off, else 1 + base units of re-arms left. */
  rptAmount: I64;
  /** Hex of the first 279 bytes of the committed exit `KobCondAsk` state. */
  exitState: Hex;
}
/** State of an if-done ask entry (`KobIfdAsk`): sells a token, then places a conditional bid. */
export interface IfdAskState {
  maker: Hex; tokenCovId: Hex; tokenTplHash: Hex; tplPrefixLen: I64; tplSuffixLen: I64;
  scale: I64; price: I64; tip: I64; activeFrom: I64; expiryDaa: I64; refundTip: I64;
  /** Buy-back budget beyond the proceeds, sompi per whole token. */
  prefund: I64; exitCarrier: I64; minFill: I64; entryStop: I64; bandDaa: I64;
  minTouch: I64; minRestDaa: I64; keeperTip: I64; armed: I64; amountLeft: I64; rptAmount: I64;
  /** Hex of the first 321 bytes of the committed exit `KobCondBid` state. */
  exitState: Hex;
}
/**
 * Pair order (`KobPair`, kind 0x08). One template serves both sides and both token families.
 * It trades an amount of the base token A at a price in the quote token B.
 * `price` is B base units per whole A, that is per `scale(A)` base units of A.
 * S is the token the order sells and holds in custody. T is the token it buys.
 * - An ASK (side 1) sells A, so S = A. A fill of n delivers at least `ceil(n * p(t) / scale(A))`
 *   of B to the maker.
 * - A BID (side 2) escrows B, so S = B. A fill pays exactly `floor(n * p(t) / scale(A))` of B and
 *   receives exactly n of A.
 * Tips, keeper tips and carriers are KAS. `tip` is sompi per whole A, prefunded on the order UTXO
 * and released rounded down per fill. Source: contracts/orders/KobPair.sil.
 */
export interface PairState {
  maker: Hex;
  /** 1 ASK (sells base A), 2 BID (buys base A). */
  side: I64;
  /** S: the token sold and held in custody (A for an ask, B for a bid). */
  sCovId: Hex; sTplHash: Hex; sPre: I64; sSuf: I64;
  /** Family code of S: 1 KCC-20, 2 KRON. */
  sFamily: I64;
  /** Base units per whole S. */
  sScale: I64;
  /** T: the token bought (B for an ask, A for a bid). */
  tCovId: Hex; tTplHash: Hex; tPre: I64; tSuf: I64; tFamily: I64;
  /** KCC-20 extension commitment of the T deliveries (zero for a KRON T). */
  tExt: Hex;
  tScale: I64;
  /** Smallest fill (base units of A) unless it takes all that is left. */
  minFill: I64;
  /** Limit: B base units per whole A (a decay's start). */
  price: I64;
  /** Priority tip: KAS, sompi per whole A. */
  tip: I64;
  /** 0 GTC/GTD, 1 IOC, 2 FOK. */
  tif: I64;
  activeFrom: I64; expiryDaa: I64; refundTip: I64;
  /** Sompi on each T delivery (prefunded on the order UTXO). */
  deliveryCarrier: I64;
  /** TWAP / DCA: least DAA between fills and most base units of A per fill (0 = off). */
  interval: I64; maxFill: I64;
  /** Decay (ASK down, BID up): B per whole A per `decayStep` DAA (0 = off), to `priceEnd` */
  slope: I64; priceEnd: I64; decayStep: I64;
  /** Mutable: base units of A still to trade. */
  amountLeft: I64;
  /** Mutable: the exact custody of S (an ask: == amountLeft). */
  custody: I64;
  /** KCC-20 extension commitment of the custody of S (zero for a KRON S). No other is taken. */
  sExt: Hex;
}

/**
 * Conditional pair order (`KobCondPair`, kind 0x09).
 * It covers stop, stop-limit, trailing, take-profit / limit leg, OCO and the exit of every
 * `KobIfdPair` fill. An ASK sells A (S = A) with a stop below the market. A BID holds a B escrow
 * (S = B) and buys A with a stop above it. Prices are B per whole A.
 * It arms from evidence in one of two modes. Mode 0 is two KAS-book fills of A and B, which imply
 * a rate. Mode 1 is a fill of a resting `KobPair` of the pair.
 */
export interface CondPairState {
  maker: Hex; side: I64;
  sCovId: Hex; sTplHash: Hex; sPre: I64; sSuf: I64; sFamily: I64; sScale: I64;
  tCovId: Hex; tTplHash: Hex; tPre: I64; tSuf: I64; tFamily: I64; tExt: Hex; tScale: I64;
  minFill: I64; tip: I64; activeFrom: I64; expiryDaa: I64; refundTip: I64;
  /** Sompi on each maker token output (delivery or profit). */
  deliveryCarrier: I64;
  /** 0 = no take-profit / limit leg; B per whole A. */
  tpPrice: I64;
  slipBps: I64; trailStep: I64; trailGap: I64; trailWait: I64;
  /** Smallest A evidence fill (base units of A) that arms or trails. */
  minTouch: I64;
  minRestDaa: I64; bandDaa: I64; keeperTip: I64;
  /** Mutable (trailing): 0 = no stop leg. */
  stopPrice: I64;
  /** Mutable: 0 not armed, 1 armed, >= 2 the auction origin. */
  armed: I64;
  /** Mutable: base units of A still to trade. */
  amountLeft: I64;
  /** Mutable: the exact custody of S. */
  custody: I64;
  /** Repeat IFD: the entry this exit re-arms (zero = none), the rates it wrote, and rptUntil. */
  parent: Hex; rptPrice: I64; rptPre: I64; rptUntil: I64;
  /** KCC-20 extension commitment of the custody of S (zero for a KRON S). Copied from the entry. */
  sExt: Hex;
}

/**
 * If-done pair entry (`KobIfdPair`, kind 0x0a): IFD, IFO, bracket and repeat.
 * A BID (side 2, buy-first) holds a B escrow (`custody`) and buys A into a fresh `KobCondPair`
 * ASK exit per fill.
 * An ASK (side 1, sell-first) holds A (exactly `amountLeft`) and a B prefund (`custody`). It sells
 * A into a fresh `KobCondPair` BID exit that holds the proceeds plus the fill's prefund.
 */
export interface IfdPairState {
  maker: Hex;
  /** 2 BID (buy-first), 1 ASK (sell-first). */
  side: I64;
  aCovId: Hex; aTplHash: Hex; aPre: I64; aSuf: I64; aFamily: I64; aScale: I64;
  /** KCC-20 extension commitment of NEW A outputs (zero for KRON). */
  aExt: Hex;
  bCovId: Hex; bTplHash: Hex; bPre: I64; bSuf: I64; bFamily: I64; bScale: I64; bExt: Hex;
  /** B base units per whole A (the limit of a stop entry). */
  price: I64;
  /** Sell-first: the buy-back budget beyond the proceeds, B per whole A (>= 0). */
  prefund: I64;
  tip: I64; activeFrom: I64; expiryDaa: I64; refundTip: I64;
  /** KAS on each exit custody. */
  deliveryCarrier: I64;
  /** KAS on each exit UTXO (and on a custody a merge creates). */
  exitCarrier: I64;
  minFill: I64;
  /** 0 = limit entry; else the stop trigger (B per whole A). */
  entryStop: I64;
  bandDaa: I64; minTouch: I64; minRestDaa: I64; keeperTip: I64;
  /** Mutable. */
  armed: I64; amountLeft: I64;
  /** Mutable: the exact B held (buy-first the escrow, sell-first the prefund; 0 = no custody). */
  custody: I64;
  /** Mutable: 0 = off, else 1 + base units of re-arms left. */
  rptAmount: I64;
  /** Hex of the committed exit, the `KobCondPair` state up to `armed` (432 bytes). */
  exitState: Hex;
}

/** Trigger evidence of a pair conditional (kob-wasm `pairArms` / `condPairTrailK`). */
export type PairEvidence =
  /**
   * Mode 0: a resting KAS-book order of A quoting `a` (sompi per whole A) and one of B quoting
   * `b` (sompi per whole B), filled together.
   */
  | { mode: 'kasBooks'; a: I64; b: I64 }
  /** Mode 1: a resting `KobPair` of the same pair quoting `price` (B per whole A), filled. */
  | { mode: 'pair'; price: I64 };

/** One token of a pair order (kob-wasm `pairTokens`). */
export interface PairTokenInfo { covId: Hex; tplHash: Hex; prefixLen: number; suffixLen: number; family: number; scale: I64; ext: Hex | null }
/** Both tokens of a pair order and its side (kob-wasm `pairTokens`). */
export interface PairTokensInfo { kind: PairKind; side: 'ask' | 'bid'; a: PairTokenInfo; b: PairTokenInfo }

/** Kob-wasm `pairTriggerRule`: which resting order sides arm (and trail) a pair stop. */
export interface PairTriggerRule {
  kind: PairKind;
  stop: I64;
  /** A sell stop arms when the rate falls to the stop, a buy stop when it rises to it. */
  direction: 'fallsTo' | 'risesTo';
  /** Least A evidence (base units of A) and, in mode 0, least B evidence (base units of B). */
  minTouch: I64;
  minTouchB: I64 | null;
  minRestDaa: I64;
  /**
   * Mode 0: the side of the resting KAS-book order of A and of B that counts.
   * Mode 1: the side of the resting pair order.
   */
  arm: { kasBooks: { a: 'ask' | 'bid'; b: 'ask' | 'bid' }; pair: 'ask' | 'bid' };
  trail: { direction: 'up' | 'down'; kasBooks: { a: 'ask' | 'bid'; b: 'ask' | 'bid' }; pair: 'ask' | 'bid'; step: I64; gap: I64 } | null;
}

/**
 * A state with its `kind` as kob-wasm tags it: the KCC-20 name or, for the KRON family, the same
 * name with a `Kron` suffix. Pair kinds have no suffix, because their states name the families
 * of A and B.
 */
type Tagged<K extends string, S> = { kind: K | `${K}Kron`; state: S };
type TaggedOne<K extends string, S> = { kind: K; state: S };
/** A decoded order state with its kob-wasm kind tag. */
export type AnyState =
  | Tagged<'KobAsk', AskState>
  | Tagged<'KobBid', BidState>
  | Tagged<'KobCondAsk', CondAskState>
  | Tagged<'KobCondBid', CondBidState>
  | Tagged<'KobIfdBid', IfdBidState>
  | Tagged<'KobIfdAsk', IfdAskState>
  | TaggedOne<'KobPair', PairState>
  | TaggedOne<'KobCondPair', CondPairState>
  | TaggedOne<'KobIfdPair', IfdPairState>;

/** Every state kob-wasm decodes is an order state. */
export type OrderState = AnyState;

/**
 * KCC-20 token state (112-byte layout). The keys are snake_case.
 * `owner_scheme` is 0 for a P2PK owner and 4 for covenant-id custody.
 */
export interface Kcc20State {
  amount: I64;
  owner: Hex;
  owner_scheme: number;
  borrow_scheme: number;
  borrow_guard: Hex;
  extension_commitment: Hex;
}

/**
 * KRON token state (46-byte layout). The keys are snake_case.
 * `id_type` is 0 pubkey, 1 script hash, 2 covenant id (KOB custody) or 3 address presence
 * (wallet balances and deliveries).
 */
export interface KronState {
  amount: I64;
  owner: Hex;
  id_type: number;
  is_minter: number;
}
/** A token state of either family. A state with `id_type` and `is_minter` is KRON. */
export type TokenState = Kcc20State | KronState;

// ------------------------------------------------------------------------------------------- utxos

/** An unspent transaction output. */
export interface Utxo {
  transactionId: Hex;
  index: number;
  /** KAS (sompi) on the output. */
  amount: U64;
  blockDaaScore?: U64;
  covenantId?: Hex | null;
}
/** A UTXO owned by a P2PK key. */
export interface KeyUtxo extends Utxo { pubkey: Hex }
/** A token UTXO with its decoded token state. */
export interface TokenUtxo extends Utxo { state: TokenState }
/** An order UTXO with its decoded state. */
export interface OrderUtxo<S = AnyState> extends Utxo { state: S }

// ---------------------------------------------------------------------------------------- requests

/**
 * How the fee is priced.
 * `relay` (default) pays the node's relay floor, `rate x max(compute, normalized transient)`.
 * Storage mass needs no relay fee. `priority` is storage-inclusive:
 * `rate x max(compute, transient, storage)`.
 */
export type FeeMode = 'relay' | 'priority';
/** Fee rate and pricing mode of a built transaction. */
export interface FeeOptions { feeRate?: U64 | null; feeMode?: FeeMode }

/** The custody token output of a placement: its output index and extension commitment. */
export interface Custody { tokenOutput: number; extensionCommitment: Hex }

/** One record of a KOB payload. */
export type PayloadRecord =
  | { type: 'order'; output: number; family: number; template: TemplateName; templateHash: Hex; state: Hex; custody: Custody | null; deadline?: U64 | null }
  /**
   * In-place amend (payload version 4). The maker's cancel of the order spent at `input` continues
   * its covenant id at `output` with `state`.
   */
  | { type: 'amend'; output: number; input: number; family: number; template: TemplateName; state: Hex; deadline?: U64 | null }
  /**
   * A maker's sweep in place (optional record 0x83). The maker's cancel of the order spent at
   * `input` continues it at `output` under the same script.
   */
  | { type: 'sweep'; output: number; input: number }
  | { type: 'x402'; reference: Hex }
  | { type: 'note'; text: string }
  /**
   * The KOB release of the build that wrote the payload (optional record 0x84, for example
   * `0.1.1`). kob-wasm's builders append it to every placement.
   */
  | { type: 'release'; version: string }
  | { type: 'unknown'; recordType: number; value: Hex };

/** A decoded KOB payload: its version and records. */
export interface Payload { version: number; records: PayloadRecord[]; legacy?: boolean }

/** Places one order. */
export interface CreateOrderRequest {
  action: 'createOrder';
  order: OrderState;
  /** KAS on the order UTXO: the carrier (token kinds) or the whole escrow (bid kinds). */
  value: U64;
  /** Token kinds: the maker's P2PK-owned token UTXOs to draw the custody (`amountLeft`) from. */
  tokens?: TokenUtxo[];
  /** KAS on the custody token UTXO and on token change. */
  tokenCarrier?: U64;
  /** P2PK funding; the first input authorises the genesis. */
  funding: KeyUtxo[];
  change?: Hex | null;
  lockTime?: U64;
  /** Day orders: wall-clock deadline (UTC unix seconds) for the placement record. */
  deadline?: U64 | null;
  records?: PayloadRecord[];
  fee?: FeeOptions;
  /**
   * Wallet keys (x-only). kob-wasm refuses an order maker or change key outside this list.
   * `guardRequest` sets it.
   */
  ownKeys?: Hex[];
}

/** The order that replaces a cancelled one, with its KAS value and optional token carrier. */
export interface Replacement { order: OrderState; value: U64; tokenCarrier?: U64 | null; deadline?: U64 | null }

/** Cancels one order, returns its custody and strays and optionally places a replacement. */
export interface CancelOrderRequest {
  action: 'cancelOrder';
  order: OrderUtxo<OrderState>;
  /**
   * Set when the order is under a retired template (kob-wasm `retiredTemplates`, cancel only).
   * Holds that template's hash. Only a plain cancel is built.
   */
  retired?: Hex | null;
  /** The custody (a pair order: the first of kob-wasm `custodies`). */
  custody?: TokenUtxo | null;
  /** A sell-first `KobIfdPair`'s B prefund custody (the second of `custodies`). */
  prefund?: TokenUtxo | null;
  strays?: TokenUtxo[];
  /** Extra maker tokens for a replacement that needs a larger custody. */
  tokens?: TokenUtxo[];
  funding?: KeyUtxo[];
  change?: Hex | null;
  replace?: Replacement | null;
  /**
   * Foreign strays: tokens of other covenant ids owned by the order id. They return to the maker,
   * one output per token.
   */
  foreign?: ForeignStrays[];
  lockTime?: U64;
  records?: PayloadRecord[];
  fee?: FeeOptions;
  ownKeys?: Hex[];
}

/**
 * Stray UTXOs of one token other than the order's own (a foreign stray, see matcher.md).
 * The order's covenant id owns them. `token` names the token and its program.
 */
export interface ForeignStrays { token: { covenantId: Hex; program: TokenProgram }; utxos: TokenUtxo[] }

/**
 * The maker's sweep of an order's strays in place (kob-protocol `SweepOrder`).
 * The maker's cancel continues the order's covenant id with the same script at output 0, with the
 * same state and the custody untouched. It returns the strays (the order's own token, a cross
 * limit's token B and foreign ones) to the maker, one token output per token.
 * Without `funding` only a plain ask's carrier pays the fee. Every other kind needs a funding UTXO.
 */
export interface SweepOrderRequest {
  action: 'sweepOrder';
  order: OrderUtxo<OrderState>;
  strays?: TokenUtxo[];
  foreign?: ForeignStrays[];
  funding?: KeyUtxo[];
  change?: Hex | null;
  tokenCarrier?: U64 | null;
  lockTime?: U64;
  records?: PayloadRecord[];
  fee?: FeeOptions;
  ownKeys?: Hex[];
}

/**
 * In-place amend of a plain ask (kob-protocol `AmendOrder`).
 * The maker's cancel continues the order's covenant id with `amended`, and the custody stays where
 * it is. Only price, tip, time-in-force, timing and decay can change. The maker, token, scale,
 * minFill and amountLeft cannot. Without `funding` the order's carrier pays the fee.
 */
export interface AmendOrderRequest {
  action: 'amendOrder';
  order: OrderUtxo<OrderState>;
  amended: OrderState;
  /** KAS on the continuation when funded (default: the order's value). */
  value?: U64 | null;
  funding?: KeyUtxo[];
  change?: Hex | null;
  lockTime?: U64;
  deadline?: U64 | null;
  records?: PayloadRecord[];
  fee?: FeeOptions;
  ownKeys?: Hex[];
}

/** One order to cancel in a position cancel, with its custody and strays. */
export interface CancelItem { order: OrderUtxo<OrderState>; custody?: TokenUtxo | null; prefund?: TokenUtxo | null; strays?: TokenUtxo[]; retired?: Hex | null }
/** Cancels several orders in one transaction. */
export interface CancelPositionRequest {
  action: 'cancelPosition';
  orders: CancelItem[];
  funding?: KeyUtxo[];
  change?: Hex | null;
  lockTime?: U64;
  tokenCarrier?: U64 | null;
  records?: PayloadRecord[];
  fee?: FeeOptions;
  ownKeys?: Hex[];
}

/** Sends tokens of one program to P2PK recipients. */
export interface SendTokensRequest {
  action: 'sendTokens';
  token: { covenantId: Hex; program: TokenProgram };
  tokens: TokenUtxo[];
  recipients: { pubkey: Hex; amount: I64; carrier: U64 }[];
  tokenChange?: Hex | null;
  tokenChangeCarrier?: U64;
  funding?: KeyUtxo[];
  change?: Hex | null;
  fee?: FeeOptions;
}

/** Every request kob-wasm builds a transaction for. */
export type ActionRequest = CreateOrderRequest | CancelOrderRequest | AmendOrderRequest | CancelPositionRequest | SendTokensRequest | SweepOrderRequest;

// --------------------------------------------------------------------- built / signed transactions

/** A previous output as the signing input needs it. */
export interface TxUtxoJson {
  address: string | null; amount: U64; scriptPublicKey: string; blockDaaScore: U64; isCoinbase: boolean; covenantId: Hex | null;
}
/** A transaction input with its signature script and previous output. */
export interface TxInputJson {
  transactionId: Hex; index: number; sequence: U64; sigOpCount: number; computeBudget: number; signatureScript: Hex; utxo: TxUtxoJson;
}
/** A transaction output with its covenant binding, if any. */
export interface TxOutputJson {
  value: U64; scriptPublicKey: string; covenant: { authorizingInput: number; covenantId: Hex } | null;
}
/** Kaspa-wasm "safe JSON" transaction (`Transaction.deserializeFromSafeJSON`). */
export interface TxJson {
  id: Hex; version: number; inputs: TxInputJson[]; outputs: TxOutputJson[];
  subnetworkId: Hex; lockTime: U64; gas: U64; storageMass: U64; payload: Hex;
}

/** One argument of a covenant entry call: an int, bytes or a signature. */
export type SigArg = { kind: 'int' | 'bytes'; value: string } | { kind: 'sig'; value: Hex };
/** The owner witness of a token input: a covenant id or a P2PK key. */
export type SigWitness = { kind: 'covenantId' } | { kind: 'p2pk'; value: Hex };
/** How one input is signed. */
export type SigPlan =
  | { kind: 'p2pk'; pubkey: Hex }
  | { kind: 'entry'; template: TemplateName; state: Hex; entry: string; args: SigArg[] }
  | { kind: 'tokenLeader'; template: TokenProgram; state: Kcc20State; nextStates: Kcc20State[]; witness: SigWitness }
  | { kind: 'tokenDelegator'; template: TokenProgram; state: Kcc20State; witness: SigWitness }
  /**
   * KRON token input. It has no leader and no signature of its own, because address presence is a
   * P2PK input of the owner elsewhere. `witnesses` is hex, one input index per token input.
   */
  | { kind: 'kronToken'; template: TokenProgram; state: KronState; nextStates: KronState[]; witnesses: Hex; signer?: Hex }
  /**
   * The maker's `cancel(sig)` of an order under a retired template. `retired` is that template's
   * hash and `template` is the current kind of the same state layout. There is no other entry.
   */
  | { kind: 'retiredCancel'; template: TemplateName; retired: Hex; state: Hex; maker: Hex };

/** One template of the cancel-only table (kob-wasm `retiredTemplates`). Only its maker cancels. */
export interface RetiredTemplateInfo { kind: TemplateName; hash: Hex; label: string; retiredBy: string; lastRelease: string; entries: string[] }

/** One signature the wallet is asked for. */
export interface SignRequest {
  inputIndex: number;
  pubkey: Hex;
  /** Wallet-facing SIGHASH_ALL = 1. */
  sighashType: number;
  /** Schnorr SIGHASH_ALL digest the signature must be over. */
  sighash: Hex;
  /** Redeem script of a P2SH input (Kastle `scripts`, Kaspire per-input scripts); null for P2PK. */
  redeemScript: Hex | null;
}

/** Transaction mass by kind, and the masses the fee is priced on. */
export interface MassReport { size: number; compute: number; transient: number; transientNormalized: number; storage: number; feeMass: number; priorityMass: number }
/**
 * Fee of a built transaction. `minFee` is the fee targeted at the requested rate and mode, which
 * is the node's relay floor in `relay` mode.
 */
export interface FeeReport { fee: U64; minFee: U64; feeRate: U64; feeMode: FeeMode; mass: MassReport; changeOutput: number | null }
/** A covenant a transaction creates: its outputs, authorizing input and id. */
export interface NewCovenant { outputs: number[]; authorizingInput: number; covenantId: Hex; template: TemplateName | null }

/** A built, unsigned transaction with its signing plans and fee. */
export interface BuiltTx {
  tx: TxJson;
  plans: SigPlan[];
  roles: string[];
  sign: SignRequest[];
  fee: FeeReport;
  covenants: NewCovenant[];
}
/** A signed transaction with its fee. */
export interface SignedTx { tx: TxJson; fee: FeeReport }
/** A wallet signature: 64 B, 65 B (+ sighash byte) or the 66 B `push(sig65)` script, hex. */
export interface InputSignature { inputIndex: number; signature: Hex }
/** Options of the finalize step. */
export interface FinalizeOptions { tightenBudgets?: boolean }

// --------------------------------------------------------------------------- misc kob-wasm results

/** Layout, entries and hash of one template. */
export interface TemplateInfo {
  name: TemplateName; hash: Hex; prefixLen: number; stateLen: number; suffixLen: number;
  entries: Record<string, string>; kindCode: number | null; tokenSlots: [number, number] | null;
  /** Token programs only: the least KAS (sompi) a token output may carry. */
  minTokenOutput?: U64;
}
/** Refund and update fees and tips, and the keeper tip, in sompi. */
export interface Tips { refundFee: U64; refundTip: U64; updateFee: U64; keeperTip: U64 }
/**
 * Trigger evidence a batch leg provides (`touchOf`): a plain KobAsk or KobBid filled in the same
 * transaction.
 * `side` 1 is a resting ask sold at its quote and 2 a resting bid bought at its quote.
 * `price` is the quote in sompi per whole token of `scale` base units. `amount` is the base units
 * filled. `exposedSince` is max(order UTXO DAA + interval, custody DAA (asks), activeFrom).
 * All integers are decimal strings.
 */
export interface Touch { side: number; tokenCovId: Hex; scale: I64; price: I64; amount: I64; exposedSince: I64 }
/** Expiry of a day order as a DAA score and as a wall-clock deadline. */
export interface DayOrder { expiryDaa: U64; deadline: U64 }

/**
 * An in-place amend re-derived by kob-wasm `recoverAmends`.
 * The order's covenant id continues at `output`, and the custody has not moved.
 */
export interface RecoveredAmend {
  transactionId: Hex; output: number; input: number; value: U64; covenantId: Hex; order: AnyState; previous: AnyState; deadline?: U64 | null;
}

/** An order re-derived from chain data by kob-wasm, with its custody. */
export interface RecoveredOrder {
  transactionId: Hex; output: number; value: U64; covenantId: Hex; order: AnyState;
  custody: { output: number; value: U64; state: TokenState } | null;
  /** A sell-first `KobIfdPair`'s B prefund custody (when it holds one). */
  prefund?: { output: number; value: U64; state: TokenState } | null;
  deadline?: U64 | null;
}

