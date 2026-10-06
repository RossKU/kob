// JSON types of the kob-wasm boundary (mirror of the `kob-protocol` serde types; see crates/kob-protocol/src/json.rs).
//
// Conventions (identical to the Rust side):
//   * byte strings (hashes, keys, scripts, state spans) are lower-case hex strings;
//   * 64-bit integers are DECIMAL STRINGS (`I64` / `U64`) so JavaScript never loses precision. Inputs also accept JSON numbers,
//     but always produce strings and do all arithmetic in `bigint` (see units.ts);
//   * small integers (u8/u16/u32, indices, output/input positions) are plain numbers.

export type Hex = string;
/** Decimal string of a signed 64-bit integer. */
export type I64 = string;
/** Decimal string of an unsigned 64-bit integer (sompi, DAA scores, unix seconds). */
export type U64 = string;

// ------------------------------------------------------------------------------------------------ order states

/** Token program lineage of an order / token: `kcc20` (112-byte state) or `kron` (46-byte state). Every layout rule of the orders is the same; only templates, token states and token input authorisation differ. */
export type Family = 'kcc20' | 'kron';
/** The pair order kinds (token A for token B at a price in B per whole A): ONE template each for both sides and both token families. */
export type PairKind = 'KobPair' | 'KobCondPair' | 'KobIfdPair';
/** The KAS-quoted order kinds (a token against KAS), family-neutral. */
export type KasKind = 'KobAsk' | 'KobBid' | 'KobCondAsk' | 'KobCondBid' | 'KobIfdBid' | 'KobIfdAsk';
/** The order kinds, family-neutral (what the UI and the planners reason about). */
export type BaseKind = KasKind | PairKind;
/** Order kinds as kob-wasm tags them: the KRON family has its own templates of the KAS kinds (`KobAskKron`, ...); the pair kinds have one each (their states name both families). */
export type OrderKind = BaseKind | `${KasKind}Kron`;
/** A base kind of a RETIRED template (`retiredTemplates` / `decodeRetired`): also the retired cross limit `KobCross` (replaced by the pair kinds). */
export type RetiredKind = KasKind | 'KobCross';
export type TemplateName = OrderKind | TokenProgram;
export type Kcc20Program = 'KCC20Ref' | 'KCC20Ref_4x5' | 'KCC20Ref_8x8' | 'KCC20Ref_16x16' | 'KCC20P2' | 'KCC20KaspaCom_0_2_5';
export type KronProgram = 'KronToken2433' | 'KronToken2732';
export type TokenProgram = Kcc20Program | KronProgram;

export interface AskState {
  maker: Hex; tokenCovId: Hex; tokenTplHash: Hex; tplPrefixLen: I64; tplSuffixLen: I64;
  /** base units per whole token (`10^decimals`, at most 10^9): the price denominator */
  scale: I64;
  /** smallest fill in base units, unless the fill takes everything left */
  minFill: I64;
  /** sompi per whole token (`scale` base units); a decay's start price */
  price: I64;
  /** priority tip, sompi per whole token */
  tip: I64;
  /** 0 GTC/GTD, 1 IOC, 2 FOK */
  tif: I64;
  activeFrom: I64; expiryDaa: I64; refundTip: I64;
  /** TWAP: most base units per fill (0 = off) */
  interval: I64; maxFill: I64; slope: I64; priceEnd: I64; decayStep: I64;
  /** base units in custody (the custody holds exactly this) */
  amountLeft: I64;
}
export interface BidState {
  maker: Hex; tokenCovId: Hex; tokenTplHash: Hex; tplPrefixLen: I64; tplSuffixLen: I64; extensionCommitment: Hex;
  scale: I64;
  /** smallest fill in base units (> 0), unless less than one minimum fill of buying power is left */
  minFill: I64; price: I64; tip: I64; tif: I64;
  activeFrom: I64; expiryDaa: I64; refundTip: I64; reserve: I64; deliveryCarrier: I64;
  /** DCA: most base units per fill (0 = off) */
  interval: I64; maxFill: I64; slope: I64; priceEnd: I64; decayStep: I64;
}
export interface CondAskState {
  maker: Hex; tokenCovId: Hex; tokenTplHash: Hex; tplPrefixLen: I64; tplSuffixLen: I64;
  scale: I64; minFill: I64; tip: I64; activeFrom: I64; expiryDaa: I64; refundTip: I64;
  tpPrice: I64; stopPrice: I64; slipBps: I64; trailStep: I64; trailGap: I64; trailWait: I64;
  /** smallest trigger evidence: base units of a plain order of the same scale filled in the same transaction */
  minTouch: I64; minRestDaa: I64; armed: I64; bandDaa: I64; keeperTip: I64; amountLeft: I64;
  /** repeat IFD: the entry's budget rate (sompi per whole token) returned to it */
  parent: Hex; rptPrice: I64; rptUntil: I64;
}
export interface CondBidState {
  maker: Hex; tokenCovId: Hex; tokenTplHash: Hex; tplPrefixLen: I64; tplSuffixLen: I64; extensionCommitment: Hex;
  scale: I64; minFill: I64; tip: I64; activeFrom: I64; expiryDaa: I64; refundTip: I64; deliveryCarrier: I64;
  tpPrice: I64; stopPrice: I64; slipBps: I64; trailStep: I64; trailGap: I64; trailWait: I64;
  minTouch: I64; minRestDaa: I64; amountLeft: I64; armed: I64; bandDaa: I64; keeperTip: I64;
  /** repeat IFD: the entry's proceeds rate (`price - tip`) and prefund rate, sompi per whole token */
  parent: Hex; rptPrice: I64; rptPre: I64; rptUntil: I64;
}
export interface IfdBidState {
  maker: Hex; tokenCovId: Hex; tokenTplHash: Hex; tplPrefixLen: I64; tplSuffixLen: I64; extensionCommitment: Hex;
  scale: I64; amountLeft: I64; price: I64; tip: I64; activeFrom: I64; expiryDaa: I64; refundTip: I64;
  deliveryCarrier: I64; exitCarrier: I64; minFill: I64; entryStop: I64; bandDaa: I64;
  minTouch: I64; minRestDaa: I64; keeperTip: I64; armed: I64;
  /** repeat: 0 = off, else 1 + base units of re-arms left */
  rptAmount: I64;
  /** hex of the first 279 bytes of the committed exit `KobCondAsk` state */
  exitState: Hex;
}
export interface IfdAskState {
  maker: Hex; tokenCovId: Hex; tokenTplHash: Hex; tplPrefixLen: I64; tplSuffixLen: I64;
  scale: I64; price: I64; tip: I64; activeFrom: I64; expiryDaa: I64; refundTip: I64;
  /** buy-back budget beyond the proceeds, sompi per whole token */
  prefund: I64; exitCarrier: I64; minFill: I64; entryStop: I64; bandDaa: I64;
  minTouch: I64; minRestDaa: I64; keeperTip: I64; armed: I64; amountLeft: I64; rptAmount: I64;
  /** hex of the first 321 bytes of the committed exit `KobCondBid` state */
  exitState: Hex;
}
/**
 * Pair order (`KobPair`, kind 0x08; ONE template for both sides and both token families): an amount of the BASE token A at a price in the QUOTE
 * token B, `price` B base units per WHOLE A (`scale(A)` base units of A). S = the token the order sells and holds in custody (an ASK: A; a BID:
 * B, its escrow), T = the token it buys. ASK (side 1): a fill of n delivers at least `ceil(n * p(t) / scale(A))` of B to the maker; BID (side 2):
 * pays exactly `floor(n * p(t) / scale(A))` of B and receives exactly n of A. Tips, keeper tips and carriers are KAS (`tip`: sompi per whole A,
 * prefunded on the order UTXO, released rounded down per fill). contracts/v2/KobPair.sil.
 */
export interface PairState {
  maker: Hex;
  /** 1 ASK (sells base A), 2 BID (buys base A) */
  side: I64;
  /** S: the token sold and held in custody (A for an ask, B for a bid) */
  sCovId: Hex; sTplHash: Hex; sPre: I64; sSuf: I64;
  /** family code of S: 1 KCC-20, 2 KRON */
  sFamily: I64;
  /** base units per whole S */
  sScale: I64;
  /** T: the token bought (B for an ask, A for a bid) */
  tCovId: Hex; tTplHash: Hex; tPre: I64; tSuf: I64; tFamily: I64;
  /** KCC-20 extension commitment of the T deliveries (zero for a KRON T) */
  tExt: Hex;
  tScale: I64;
  /** smallest fill (base units of A) unless it takes all that is left */
  minFill: I64;
  /** limit: B base units per whole A (a decay's start) */
  price: I64;
  /** priority tip: KAS, sompi per whole A */
  tip: I64;
  /** 0 GTC/GTD, 1 IOC, 2 FOK */
  tif: I64;
  activeFrom: I64; expiryDaa: I64; refundTip: I64;
  /** sompi on each T delivery (prefunded on the order UTXO) */
  deliveryCarrier: I64;
  /** TWAP / DCA: least DAA between fills and most base units of A per fill (0 = off) */
  interval: I64; maxFill: I64;
  /** decay (ASK down, BID up): B per whole A per `decayStep` DAA (0 = off), to `priceEnd` */
  slope: I64; priceEnd: I64; decayStep: I64;
  /** mutable: base units of A still to trade */
  amountLeft: I64;
  /** mutable: the exact custody of S (an ask: == amountLeft) */
  custody: I64;
}

/**
 * Conditional pair order (`KobCondPair`, 0x09): stop, stop-limit, trailing, take-profit / limit leg, OCO, and the exit of every `KobIfdPair`
 * fill. Side ASK sells A (S = A) with a stop BELOW the market; side BID holds a B escrow (S = B) and buys A with a stop ABOVE it. Prices in B
 * per whole A. Arms from evidence in two modes (two KAS-book fills of A and B: the implied rate; or a fill of a resting `KobPair` of the pair).
 */
export interface CondPairState {
  maker: Hex; side: I64;
  sCovId: Hex; sTplHash: Hex; sPre: I64; sSuf: I64; sFamily: I64; sScale: I64;
  tCovId: Hex; tTplHash: Hex; tPre: I64; tSuf: I64; tFamily: I64; tExt: Hex; tScale: I64;
  minFill: I64; tip: I64; activeFrom: I64; expiryDaa: I64; refundTip: I64;
  /** sompi on each maker token output (delivery or profit) */
  deliveryCarrier: I64;
  /** 0 = no take-profit / limit leg; B per whole A */
  tpPrice: I64;
  slipBps: I64; trailStep: I64; trailGap: I64; trailWait: I64;
  /** smallest A evidence fill (base units of A) that arms or trails */
  minTouch: I64;
  minRestDaa: I64; bandDaa: I64; keeperTip: I64;
  /** mutable (trailing): 0 = no stop leg */
  stopPrice: I64;
  /** mutable: 0 not armed, 1 armed, >= 2 the auction origin */
  armed: I64;
  /** mutable: base units of A still to trade */
  amountLeft: I64;
  /** mutable: the exact custody of S */
  custody: I64;
  /** repeat IFD: the entry this exit re-arms (zero = none), the rates written by the entry, rptUntil */
  parent: Hex; rptPrice: I64; rptPre: I64; rptUntil: I64;
}

/**
 * If-done pair entry (`KobIfdPair`, 0x0a): IFD / IFO / bracket / repeat. Side BID (2, buy-first) holds a B escrow (`custody`) and buys A into a
 * fresh `KobCondPair` ASK exit per fill; side ASK (1, sell-first) holds A (exactly `amountLeft`) and a B PREFUND (`custody`) and sells A into a
 * fresh `KobCondPair` BID exit holding the proceeds plus the prefund of the fill.
 */
export interface IfdPairState {
  maker: Hex;
  /** 2 BID (buy-first), 1 ASK (sell-first) */
  side: I64;
  aCovId: Hex; aTplHash: Hex; aPre: I64; aSuf: I64; aFamily: I64; aScale: I64;
  /** KCC-20 extension commitment of NEW A outputs (zero for KRON) */
  aExt: Hex;
  bCovId: Hex; bTplHash: Hex; bPre: I64; bSuf: I64; bFamily: I64; bScale: I64; bExt: Hex;
  /** B base units per whole A (the limit of a stop entry) */
  price: I64;
  /** sell-first: the buy-back budget beyond the proceeds, B per whole A (>= 0) */
  prefund: I64;
  tip: I64; activeFrom: I64; expiryDaa: I64; refundTip: I64;
  /** KAS on each exit custody */
  deliveryCarrier: I64;
  /** KAS on each exit UTXO (and on a custody a merge creates) */
  exitCarrier: I64;
  minFill: I64;
  /** 0 = limit entry; else the stop trigger (B per whole A) */
  entryStop: I64;
  bandDaa: I64; minTouch: I64; minRestDaa: I64; keeperTip: I64;
  /** mutable */
  armed: I64; amountLeft: I64;
  /** mutable: the exact B held (buy-first the escrow, sell-first the prefund; 0 = no custody UTXO) */
  custody: I64;
  /** mutable: 0 = off, else 1 + base units of re-arms left */
  rptAmount: I64;
  /** hex of the committed exit: the `KobCondPair` state up to `armed` (432 bytes; kob-wasm `ifdPairCommitExit`) */
  exitState: Hex;
}

/** Trigger evidence of a pair conditional (kob-wasm `pairArms` / `condPairTrailK`). */
export type PairEvidence =
  /** mode 0: a resting KAS-book order of A quoting `a` (sompi per whole A) and one of B quoting `b` (sompi per whole B), filled together */
  | { mode: 'kasBooks'; a: I64; b: I64 }
  /** mode 1: a resting `KobPair` of the same pair quoting `price` (B per whole A), filled */
  | { mode: 'pair'; price: I64 };

/** One token of a pair order (kob-wasm `pairTokens`). */
export interface PairTokenInfo { covId: Hex; tplHash: Hex; prefixLen: number; suffixLen: number; family: number; scale: I64; ext: Hex | null }
export interface PairTokensInfo { kind: PairKind; side: 'ask' | 'bid'; a: PairTokenInfo; b: PairTokenInfo }

/** kob-wasm `pairTriggerRule`: which resting order sides arm (and trail) a pair stop. */
export interface PairTriggerRule {
  kind: PairKind;
  stop: I64;
  /** a sell stop arms when the rate falls to the stop, a buy stop when it rises to it */
  direction: 'fallsTo' | 'risesTo';
  /** least A evidence (base units of A) and, in mode 0, least B evidence (base units of B) */
  minTouch: I64;
  minTouchB: I64 | null;
  minRestDaa: I64;
  /** mode 0: the side of the resting KAS-book order of A and of B that counts; mode 1: the side of the resting pair order */
  arm: { kasBooks: { a: 'ask' | 'bid'; b: 'ask' | 'bid' }; pair: 'ask' | 'bid' };
  trail: { direction: 'up' | 'down'; kasBooks: { a: 'ask' | 'bid'; b: 'ask' | 'bid' }; pair: 'ask' | 'bid'; step: I64; gap: I64 } | null;
}

/** `kind` of a state as kob-wasm tags it: the KCC-20 name or, for the KRON family, the same name with a `Kron` suffix (the pair kinds have no suffix: their states name the families of A and B). */
type Tagged<K extends string, S> = { kind: K | `${K}Kron`; state: S };
type TaggedOne<K extends string, S> = { kind: K; state: S };
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

/** Every state kob-wasm decodes is an order state (protocol v2.6 retired the v2.4 trade receipt). */
export type OrderState = AnyState;

/** KCC-20 token state (112-byte layout). `owner_scheme` 0 = P2PK owner, 4 = covenant-id custody. Note the snake_case keys. */
export interface Kcc20State {
  amount: I64;
  owner: Hex;
  owner_scheme: number;
  borrow_scheme: number;
  borrow_guard: Hex;
  extension_commitment: Hex;
}

/** KRON token state (46-byte layout). `id_type`: 0 pubkey, 1 script hash, 2 covenant id (KOB custody), 3 address presence (wallet balances, deliveries). */
export interface KronState {
  amount: I64;
  owner: Hex;
  id_type: number;
  is_minter: number;
}
/** A token state of either family (a state with `id_type` / `is_minter` is KRON). See token-state.ts for the helpers. */
export type TokenState = Kcc20State | KronState;

// ------------------------------------------------------------------------------------------------ utxos

export interface Utxo {
  transactionId: Hex;
  index: number;
  /** KAS (sompi) on the output */
  amount: U64;
  blockDaaScore?: U64;
  covenantId?: Hex | null;
}
export interface KeyUtxo extends Utxo { pubkey: Hex }
export interface TokenUtxo extends Utxo { state: TokenState }
export interface OrderUtxo<S = AnyState> extends Utxo { state: S }

// ------------------------------------------------------------------------------------------------ requests

/** `relay` (default): the node's relay floor `rate x max(compute, normalized transient)`; storage mass needs no relay fee. `priority`: storage-inclusive, `rate x max(compute, transient, storage)`. */
export type FeeMode = 'relay' | 'priority';
export interface FeeOptions { feeRate?: U64 | null; feeMode?: FeeMode }

export interface Custody { tokenOutput: number; extensionCommitment: Hex }

export type PayloadRecord =
  | { type: 'order'; output: number; family: number; template: TemplateName; templateHash: Hex; state: Hex; custody: Custody | null; deadline?: U64 | null }
  /** in-place amend (payload version 4): the maker's cancel of the order spent at `input` continues its covenant id at `output` with `state` */
  | { type: 'amend'; output: number; input: number; family: number; template: TemplateName; state: Hex; deadline?: U64 | null }
  /** a maker's sweep in place (optional record 0x83): the maker's cancel of the order spent at `input` continues it at `output` under the SAME script */
  /** placement record of an order of a RETIRED lot template (payload versions 2 and 3; decoded, never written): `state` is in that template's layout */
  | { type: 'retiredOrder'; output: number; family: number; template: string; templateHash: Hex; state: Hex; custody: Custody | null; deadline?: U64 | null }
  /** in-place amend of a RETIRED lot template (payload version 3; decoded, never written) */
  | { type: 'retiredAmend'; output: number; input: number; family: number; template: string; templateHash: Hex; state: Hex; deadline?: U64 | null }
  | { type: 'sweep'; output: number; input: number }
  | { type: 'x402'; reference: Hex }
  | { type: 'note'; text: string }
  | { type: 'unknown'; recordType: number; value: Hex };

export interface Payload { version: number; records: PayloadRecord[]; legacy?: boolean }

export interface CreateOrderRequest {
  action: 'createOrder';
  order: OrderState;
  /** KAS on the order UTXO: the carrier (token kinds) or the whole escrow (bid kinds) */
  value: U64;
  /** token kinds: the maker's P2PK-owned token UTXOs to draw the custody (`amountLeft` base units) from */
  tokens?: TokenUtxo[];
  /** KAS on the custody token UTXO and on token change */
  tokenCarrier?: U64;
  /** P2PK funding; the first input authorises the genesis */
  funding: KeyUtxo[];
  change?: Hex | null;
  lockTime?: U64;
  /** day orders: wall-clock deadline (UTC unix seconds) for the placement record */
  deadline?: U64 | null;
  records?: PayloadRecord[];
  fee?: FeeOptions;
  /** wallet keys (x-only): kob-wasm refuses an order maker / change key outside this list; set by `guardRequest` */
  ownKeys?: Hex[];
}

export interface Replacement { order: OrderState; value: U64; tokenCarrier?: U64 | null; deadline?: U64 | null }

export interface CancelOrderRequest {
  action: 'cancelOrder';
  order: OrderUtxo<OrderState>;
  /** the custody (a pair order: the first of kob-wasm `custodies`) */
  custody?: TokenUtxo | null;
  /** a sell-first `KobIfdPair`'s B prefund custody (the second of `custodies`) */
  prefund?: TokenUtxo | null;
  strays?: TokenUtxo[];
  /** extra maker tokens for a replacement that needs a larger custody */
  tokens?: TokenUtxo[];
  funding?: KeyUtxo[];
  change?: Hex | null;
  replace?: Replacement | null;
  /** foreign strays (tokens of other covenant ids owned by the order id): returned to the maker, one output per token */
  foreign?: ForeignStrays[];
  lockTime?: U64;
  records?: PayloadRecord[];
  fee?: FeeOptions;
  ownKeys?: Hex[];
}

/** Stray UTXOs of ONE token other than the order's own (a foreign stray, matcher.md 1.2), owned by the order's covenant id, with that token's program. */
export interface ForeignStrays { token: { covenantId: Hex; program: TokenProgram }; utxos: TokenUtxo[] }

/**
 * The maker's SWEEP of an order's strays IN PLACE (kob-protocol `SweepOrder`): the maker's cancel continues the order's covenant id with the SAME
 * script at output 0 (same state, custody untouched) and returns the strays (its own token, a cross limit's token B, foreign ones) to the maker,
 * one token output per token. Without `funding` only a plain ask's carrier pays the fee; every other kind needs a funding UTXO.
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
 * In-place amend of a plain ask (kob-protocol `AmendOrder`): the maker's cancel continues the order's covenant id with `amended` and the custody
 * stays where it is. Only price / tip / time-in-force / timing / decay change (not the maker, token, scale, minFill or amountLeft). Without `funding` the
 * order's carrier pays the fee.
 */
export interface AmendOrderRequest {
  action: 'amendOrder';
  order: OrderUtxo<OrderState>;
  amended: OrderState;
  /** KAS on the continuation when funded (default: the order's value) */
  value?: U64 | null;
  funding?: KeyUtxo[];
  change?: Hex | null;
  lockTime?: U64;
  deadline?: U64 | null;
  records?: PayloadRecord[];
  fee?: FeeOptions;
  ownKeys?: Hex[];
}

export interface CancelItem { order: OrderUtxo<OrderState>; custody?: TokenUtxo | null; prefund?: TokenUtxo | null; strays?: TokenUtxo[] }
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

export type ActionRequest = CreateOrderRequest | CancelOrderRequest | AmendOrderRequest | CancelPositionRequest | SendTokensRequest | SweepOrderRequest;

/**
 * kob-wasm `buildCancelRetired`: the maker's cancel of an order placed under a RETIRED template (`docs/spec/template-retirement.md`; spend-only).
 * `state` is the order's state span under the retired template; `order` the plain order UTXO (no decoded state).
 */
export interface CancelRetiredRequest {
  templateHash: Hex;
  state: Hex;
  order: Utxo;
  custody?: TokenUtxo | null;
  strays?: TokenUtxo[];
  foreign?: ForeignStrays[];
  funding?: KeyUtxo[];
  change?: Hex | null;
  fee?: FeeOptions;
  ownKeys?: Hex[];
}

/** kob-wasm `retiredTemplates()`: an older order template this build can still spend (the maker's cancel only). */
export interface RetiredTemplateInfo {
  /** the base kind (`KobAsk`, ..., `KobCross`) */
  kind: RetiredKind;
  /** its name in the family as it was pinned (`KobAskKron`, `KobCrossKron`, ...) */
  kindName: string;
  /** family code of the escrowed token: 1 KCC-20, 2 KRON */
  family: number;
  hash: Hex; stateLen: number; note: string;
}

/**
 * kob-wasm `decodeRetired`: the state of an order of a RETIRED template in its LEGACY lot layout (protocol v2.3 to v2.6: `lotsLeft` lots of
 * `lotUnits x unit` base units, prices per `unit`), tagged by its base kind. Spend-only: the app shows it as an older contract version and
 * builds the maker's cancel from it; the custody of a token-holding order is `lotsLeft x lotUnits x unit` base units.
 */
export interface LegacyState {
  kind: RetiredKind;
  state: { maker: Hex; tokenCovId: Hex; tokenTplHash: Hex; tplPrefixLen: I64; tplSuffixLen: I64; [field: string]: unknown };
}

// ------------------------------------------------------------------------------------------------ built / signed transactions

export interface TxUtxoJson {
  address: string | null; amount: U64; scriptPublicKey: string; blockDaaScore: U64; isCoinbase: boolean; covenantId: Hex | null;
}
export interface TxInputJson {
  transactionId: Hex; index: number; sequence: U64; sigOpCount: number; computeBudget: number; signatureScript: Hex; utxo: TxUtxoJson;
}
export interface TxOutputJson {
  value: U64; scriptPublicKey: string; covenant: { authorizingInput: number; covenantId: Hex } | null;
}
/** kaspa-wasm "safe JSON" transaction (`Transaction.deserializeFromSafeJSON`). */
export interface TxJson {
  id: Hex; version: number; inputs: TxInputJson[]; outputs: TxOutputJson[];
  subnetworkId: Hex; lockTime: U64; gas: U64; storageMass: U64; payload: Hex;
}

export type SigArg = { kind: 'int' | 'bytes'; value: string } | { kind: 'sig'; value: Hex };
export type SigWitness = { kind: 'covenantId' } | { kind: 'p2pk'; value: Hex };
export type SigPlan =
  | { kind: 'p2pk'; pubkey: Hex }
  | { kind: 'entry'; template: TemplateName; state: Hex; entry: string; args: SigArg[] }
  | { kind: 'tokenLeader'; template: TokenProgram; state: Kcc20State; nextStates: Kcc20State[]; witness: SigWitness }
  | { kind: 'tokenDelegator'; template: TokenProgram; state: Kcc20State; witness: SigWitness }
  /** KRON token input: no leader and no signature of its own (address presence = a P2PK input of the owner elsewhere); `witnesses` = hex, one input index per token input */
  | { kind: 'kronToken'; template: TokenProgram; state: KronState; nextStates: KronState[]; witnesses: Hex }
  /** entry call of a RETIRED order template (spend-only: only `cancel`), by template hash and its state span under that template */
  | { kind: 'retired'; templateHash: Hex; state: Hex; entry: string; args: SigArg[] };

export interface SignRequest {
  inputIndex: number;
  pubkey: Hex;
  /** wallet-facing SIGHASH_ALL = 1 */
  sighashType: number;
  /** Schnorr SIGHASH_ALL digest the signature must be over */
  sighash: Hex;
  /** redeem script of a P2SH input (Kastle `scripts`, Kaspire per-input scripts); null for P2PK */
  redeemScript: Hex | null;
}

export interface MassReport { size: number; compute: number; transient: number; transientNormalized: number; storage: number; feeMass: number; priorityMass: number }
/** `minFee` is the fee the builder targeted at the requested rate and mode (the node's relay floor in `relay` mode). */
export interface FeeReport { fee: U64; minFee: U64; feeRate: U64; feeMode: FeeMode; mass: MassReport; changeOutput: number | null }
export interface NewCovenant { outputs: number[]; authorizingInput: number; covenantId: Hex; template: TemplateName | null }

export interface BuiltTx {
  tx: TxJson;
  plans: SigPlan[];
  roles: string[];
  sign: SignRequest[];
  fee: FeeReport;
  covenants: NewCovenant[];
}
export interface SignedTx { tx: TxJson; fee: FeeReport }
/** A wallet signature: 64 B, 65 B (+ sighash byte) or the 66 B `push(sig65)` script, hex. */
export interface InputSignature { inputIndex: number; signature: Hex }
export interface FinalizeOptions { tightenBudgets?: boolean }

// ------------------------------------------------------------------------------------------------ misc kob-wasm results

export interface TemplateInfo {
  name: TemplateName; hash: Hex; prefixLen: number; stateLen: number; suffixLen: number;
  entries: Record<string, string>; kindCode: number | null; tokenSlots: [number, number] | null;
  /** token programs: the least KAS (sompi) a token output may carry (KaspaCom 0.2.5: 0.5 KAS; the others 1) */
  minTokenOutput?: U64;
}
export interface Tips { refundFee: U64; refundTip: U64; updateFee: U64; keeperTip: U64 }
/**
 * Trigger evidence a batch leg provides (`touchOf`): a plain KobAsk / KobBid filled in the same transaction. `side` 1 = a resting ask sold
 * at its quote, 2 = a resting bid bought at its quote; `price` is the quote (sompi per whole token of `scale` base units), `amount` the
 * base units filled, `exposedSince` = max(order UTXO DAA + interval, custody DAA (asks), activeFrom). All integers are decimal strings.
 */
export interface Touch { side: number; tokenCovId: Hex; scale: I64; price: I64; amount: I64; exposedSince: I64 }
export interface DayOrder { expiryDaa: U64; deadline: U64 }

/** An in-place amend re-derived by kob-wasm `recoverAmends` (the order's covenant id continues at `output`; the custody did not move). */
export interface RecoveredAmend {
  transactionId: Hex; output: number; input: number; value: U64; covenantId: Hex; order: AnyState; previous: AnyState; deadline?: U64 | null;
}

export interface RecoveredOrder {
  transactionId: Hex; output: number; value: U64; covenantId: Hex; order: AnyState;
  custody: { output: number; value: U64; state: TokenState } | null;
  /** a sell-first `KobIfdPair`'s B prefund custody (when it holds one) */
  prefund?: { output: number; value: U64; state: TokenState } | null;
  deadline?: U64 | null;
}

/** An order of a RETIRED lot template re-derived by kob-wasm `recoverRetiredOrders` from a placement record of payload version 2 or 3. */
export interface RecoveredRetiredOrder {
  transactionId: Hex; output: number; value: U64; covenantId: Hex; templateHash: Hex; family: Family; order: LegacyState;
  custody: { output: number; value: U64; state: TokenState } | null;
  deadline?: U64 | null;
}
