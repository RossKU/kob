// Typed facade over kob-wasm (crates/kob-wasm). Every export takes/returns JSON strings; this file parses and stringifies so the rest
// of the app deals in typed objects. All protocol logic (transaction builders, signing split, fees, budgets, KOB1 codec, state codec)
// lives in Rust: TypeScript never re-implements it.
//
// Two ways to obtain an instance:
//   * browser: `loadKob()` from `./wasm.browser` (bundled ES-module bindings in web/wasm/web, built by `npm run build:wasm`)
//   * node (vitest, mock server, e2e harness): `loadKobNode()` from `./wasm.node` (web/wasm/node)
import type {
  ActionRequest, AnyState, BuiltTx, DayOrder, FinalizeOptions, Hex, InputSignature, MassReport, Payload, PayloadRecord,
  RecoveredAmend, RecoveredOrder, SignedTx, SigPlan, TemplateInfo, TemplateName, Tips, TokenProgram, TokenState,
  Touch, TxJson, PairEvidence, PairTokensInfo, PairTriggerRule, OrderState,
} from './types';

/** Raw module shape: what wasm-bindgen generates for crates/kob-wasm/src/lib.rs (all string in / string out). */
export interface RawKobWasm {
  version(): string;
  selfCheck(): void;
  templates(): string;
  touchOf(leg: string): string;
  build(request: string): string;
  finalize(built: string, signatures: string, options: string): string;
  validate(signed: string): string;
  masses(tx: string): string;
  encodeState(state: string): string;
  decodeState(kind: string, hex: string): string;
  redeemScript(state: string): string;
  scriptPublicKey(state: string): string;
  encodeTokenState(state: string): string;
  decodeTokenState(hex: string): string;
  tokenScriptPublicKey(program: string, state: string): string;
  encodePayload(records: string): string;
  decodePayload(hex: string): string;
  recoverOrders(tx: string): string;
  recoverAmends(tx: string, plans: string): string;
  budgetTable(): string;
  budgetFor(role: string): number;
  keeperTips(): string;
  dayOrder(d0: string, t0: string, rateMilli: string): string;
  mutableWindows(kind: string): string;
  // numbers and defaults (protocol v3): 64-bit integers as decimal strings, `undefined` where the covenant arithmetic fails
  quote(n: string, rate: string, scale: string, round: string): string | undefined;
  quoteExact(n: string, rate: string, scale: string, round: string): string | undefined;
  checkScale(scale: string): void;
  checkQuote(amount: string, rate: string, scale: string, what: string): void;
  checkNumbers(state: string): void;
  minFillOk(n: string, left: string, minFill: string): boolean;
  defaultScale(decimals: number): string;
  defaultMinFill(amount: string, price: string, scale: string): string;
  defaultMinFillIfd(amount: string): string;
  defaultMinFillPair(amount: string, kasPerWholeA: string, scale: string): string;
  defaultMinTouch(minFill: string): string;
  defaultMinFillSompi(): string;
  defaultConstants(): string;
  // per-kind helpers: the state JSON of the kind (either family) and base-unit amounts
  orderPriceAt(state: string, t: string, utxoDaa: string): string | undefined;
  askProceeds(state: string, n: string, t: string, utxoDaa: string): string | undefined;
  askProceedsAt(state: string, n: string, p: string): string | undefined;
  bidUsed(state: string, n: string): string | undefined;
  bidSpend(state: string, n: string, t: string, utxoDaa: string): string | undefined;
  bidSpendAt(state: string, n: string, p: string): string | undefined;
  bidBudgetRate(state: string): string | undefined;
  bidBuyingPower(state: string, value: string): string;
  bidCanContinue(state: string, left: string): boolean;
  bidEscrow(state: string, amount: string, fills: string): string | undefined;
  fillOk(state: string, n: string, value: string): boolean;
  condAskProceeds(state: string, n: string, legPrice: string): string | undefined;
  condAskRptBudget(state: string, n: string): string | undefined;
  condBidSpend(state: string, n: string, legPrice: string): string | undefined;
  condBidRptProceeds(state: string, n: string): string | undefined;
  condBidRptPrefund(state: string, n: string): string | undefined;
  condBidEscrow(state: string, fills: string): string | undefined;
  ifdBidSpend(state: string, n: string, p: string): string | undefined;
  ifdBidMergeBudget(state: string, m: string): string | undefined;
  ifdBidEscrow(state: string): string | undefined;
  ifdAskProceeds(state: string, n: string, p: string): string | undefined;
  ifdAskPrefund(state: string, n: string): string | undefined;
  ifdAskMergeSelloutBack(state: string, m: string, exitValue: string): string | undefined;
  ifdAskEscrow(state: string, carrier: string): string | undefined;
  // pair orders (KobPair, KobCondPair, KobIfdPair): prices B base units per whole A, tips / carriers KAS
  pairKeeperTips(): string;
  pairTips(programA: string, programB: string): string;
  tipsFor(state: string): string;
  pairTokens(state: string): string;
  pairPrograms(state: string): string;
  custodies(state: string): string;
  checkNewOrder(state: string): void;
  minOrderValue(state: string): string;
  pairSOut(state: string, n: string, p: string): string | undefined;
  pairTOutMin(state: string, n: string, p: string): string | undefined;
  pairTipKas(state: string, n: string): string | undefined;
  pairFill(state: string, n: string, p: string, value: string): string;
  pairMaxTakeable(state: string, p: string, value: string): string;
  pairPriceMax(state: string): string;
  pairBidEscrow(state: string, amount: string, fills: string): string | undefined;
  pairKasValue(state: string, fills: string): string | undefined;
  pairMaxFills(state: string): string;
  pairFundedFills(state: string): string;
  ifdPairExitCarrierNeeded(state: string): string | undefined;
  condPairLegPrice(state: string, leg: string, trigger: string, t: string, utxoDaa: string): string | undefined;
  condPairBounds(state: string): string;
  condPairRptProceeds(state: string, n: string): string | undefined;
  condPairRptBack(state: string, n: string): string | undefined;
  condPairBidEscrow(state: string, fills: string): string | undefined;
  pairArms(state: string, evidence: string): boolean | undefined;
  condPairTrailK(state: string, evidence: string): string | undefined;
  condPairTrailCheck(state: string, evidence: string, k: string): boolean;
  pairMinTouchB(state: string): string | undefined;
  ifdPairExit(state: string): string;
  ifdPairExitFor(state: string, n: string, custody: string, parent: string, until: string): string;
  ifdPairCommitExit(exit: string): string;
  ifdPairPriceAt(state: string, trigger: string, t: string, utxoDaa: string): string | undefined;
  ifdPairAmounts(state: string, n: string, p: string): string;
  ifdPairBCustodyNeeded(state: string): string | undefined;
  impliedLe(a: string, b: string, x: string, bScale: string): boolean | undefined;
  impliedGe(a: string, b: string, x: string, bScale: string): boolean | undefined;
  minTouchB(minTouch: string, stop: string, aScale: string): string | undefined;
  pairEvidenceModes(): string;
  pairTriggerRule(state: string): string;
  // Issuance functions are added to the bindings by the issuance work (see kob/issue.ts); absent in older bindings.
  [extra: string]: unknown;
}

export class KobError extends Error {
  readonly fn: string;
  constructor(message: string, fn: string) {
    super(`kob-wasm ${fn}: ${message}`);
    this.name = 'KobError';
    this.fn = fn;
  }
}

export interface KobWasm {
  readonly raw: RawKobWasm;
  version(): string;
  selfCheck(): void;
  templates(): TemplateInfo[];
  /**
   * The trigger evidence a batch leg (`{"kind": "ask" | "bid", ...}` as kob-protocol's `Leg`) provides: a plain KobAsk / KobBid fill that
   * does not decay. Throws for any other leg (cross limit, conditional and if-done fills are never evidence).
   */
  touchOf(leg: object): Touch;
  /** Builds any action into an unsigned transaction + signing plan. Throws KobError with the protocol's reason on invalid requests. */
  build(request: ActionRequest): BuiltTx;
  /** Verifies wallet signatures against the digests and assembles the signed transaction. */
  finalize(built: BuiltTx, signatures: InputSignature[], options?: FinalizeOptions): SignedTx;
  /** Runs the signed tx through the in-wasm script engine with consensus rules (throws if any input fails). */
  validate(signed: SignedTx): unknown;
  masses(tx: TxJson): MassReport;
  encodeState(state: AnyState): Hex;
  decodeState(kind: TemplateName, hex: Hex): AnyState;
  redeemScript(state: AnyState): Hex;
  /** P2SH script public key in kaspa string form (`version(u16 BE hex) + script hex`). */
  scriptPublicKey(state: AnyState): string;
  /** either family's layout (KRON: `id_type` / `is_minter`) */
  encodeTokenState(state: TokenState): Hex;
  /** 46 bytes decode as KRON, 112 bytes as KCC-20 */
  decodeTokenState(hex: Hex): TokenState;
  tokenScriptPublicKey(program: TokenProgram, state: TokenState): string;
  encodePayload(records: PayloadRecord[]): Hex;
  /** null when the payload is neither KOB1 nor legacy x402 */
  decodePayload(hex: Hex): Payload | null;
  /** Re-derives the orders a genesis tx created from its KOB1 payload, trusting nothing (throws if a record does not verify). */
  recoverOrders(tx: TxJson): RecoveredOrder[];
  /**
   * Re-derives the in-place amends (AMEND records) of a transaction, trusting nothing but the previous state: of a SIGNED tx from its order
   * inputs' signature scripts (`plans` omitted), of a built one from its signing plans (`built.plans`). Throws if a record does not verify.
   */
  recoverAmends(tx: TxJson, plans?: SigPlan[]): RecoveredAmend[];
  budgetTable(): Record<string, number>;
  budgetFor(role: string): number;
  /** default refund / keeper tips of the KAS kinds per token program */
  keeperTips(): Record<TokenProgram, Tips>;
  /** default tips of the pair kinds per program pair (`<program of A>+<program of B>`) */
  pairKeeperTips(): Record<string, Tips>;
  /** default tips of a pair order whose A runs `programA` and B `programB` */
  pairTips(programA: TokenProgram, programB: TokenProgram): Tips;
  /** default tips of an order by its kind (a pair order: the pair table; any other kind: its token program's) */
  tipsFor(state: AnyState): Tips;
  /** Day order until the next 00:00 UTC. d0 = node virtual DAA, t0 = UTC unix seconds read together, rateMilli = measured milli-DAA/s (null = 10 000). */
  dayOrder(d0: bigint | string, t0: bigint | string, rateMilli?: bigint | string | null): DayOrder;
  mutableWindows(kind: TemplateName): [string, number, number][];

  // ------------------------------------------------------------------ amounts and prices (protocol v3, docs/spec/order-types.md)
  // Every quantity is token base units, every price / tip quote units per WHOLE token (`scale` base units). Helpers return null exactly where
  // the covenant's own arithmetic fails (an overflow, a rate below the tip, ...).

  /** `n * rate / scale` rounded `up` (what a maker RECEIVES) or `down` (what a maker PAYS), the covenants' exact split multiplication. */
  quote(n: Num, rate: Num, scale: Num, round: Round): bigint | null;
  /** the same in 128-bit arithmetic (not limited to i64) */
  quoteExact(n: Num, rate: Num, scale: Num, round: Round): bigint | null;
  /** throws unless `scale` is a power of ten in 1..=10^9 */
  checkScale(scale: Num): void;
  /** throws unless the full fill of `amount` at `rate` is worth less than 2^62 quote units (`what` names the rate) */
  checkQuote(amount: Num, rate: Num, scale: Num, what: string): void;
  /** the numeric gate of a placed order (scale and the 2^62 bound of every rate it carries); throws with the reason */
  checkNumbers(state: AnyState): void;
  /** `0 < n <= left` and `n >= minFill` unless n takes everything left */
  minFillOk(n: Num, left: Num, minFill: Num): boolean;
  /** `10^min(decimals, 9)` */
  defaultScale(decimals: number): bigint;
  /** wallet default minFill: the amount worth 10 KAS at `price`, clamped to 1..amount */
  defaultMinFill(amount: Num, price: Num, scale: Num): bigint;
  /** if-done entries: ceil(amount / 4) */
  defaultMinFillIfd(amount: Num): bigint;
  /** pair orders: the amount of A worth 10 KAS on A's KAS book (`kasPerWholeA` sompi per whole A; null: ceil(amount / 4)) */
  defaultMinFillPair(amount: Num, kasPerWholeA: Num | null, scale: Num): bigint;
  /** the default stop touch: the order's own minFill (at least 1) */
  defaultMinTouch(minFill: Num): bigint;
  defaultMinFillSompi(): bigint;
  defaultConstants(): DefaultConstants;
  /** price of an ask / bid at auction time `t` (decay / rise); a `KobPair`'s quote (B per whole A) */
  orderPriceAt(state: AnyState, t: Num, utxoDaa: Num): bigint | null;
  /** ask: `ceil(n * (price(t) - tip) / scale)` paid to the maker */
  askProceeds(state: AnyState, n: Num, t: Num, utxoDaa: Num): bigint | null;
  askProceedsAt(state: AnyState, n: Num, p: Num): bigint | null;
  /** bid: budget a fill of n consumes from the escrow, `ceil(n * (pMax + tip) / scale)` */
  bidUsed(state: AnyState, n: Num): bigint | null;
  /** bid: most the maker pays for n at time t, `floor(n * (price(t) + tip) / scale)` */
  bidSpend(state: AnyState, n: Num, t: Num, utxoDaa: Num): bigint | null;
  bidSpendAt(state: AnyState, n: Num, p: Num): bigint | null;
  /** bid: `pMax + tip`, sompi per whole token */
  bidBudgetRate(state: AnyState): bigint | null;
  /** bid: remaining buying power (base units) of an escrow worth `value` */
  bidBuyingPower(state: AnyState, value: Num): bigint;
  bidCanContinue(state: AnyState, left: Num): boolean;
  /** bid: escrow for `amount` base units in at most `fills` fills */
  bidEscrow(state: AnyState, amount: Num, fills: Num): bigint | null;
  /** the covenant's quantity rules for a fill of n (a plain bid needs its escrow `value`) */
  fillOk(state: AnyState, n: Num, value?: Num): boolean;
  condAskProceeds(state: AnyState, n: Num, legPrice: Num): bigint | null;
  condAskRptBudget(state: AnyState, n: Num): bigint | null;
  condBidSpend(state: AnyState, n: Num, legPrice: Num): bigint | null;
  condBidRptProceeds(state: AnyState, n: Num): bigint | null;
  condBidRptPrefund(state: AnyState, n: Num): bigint | null;
  /** conditional bid: escrow of all `amountLeft` at the worst leg in at most `fills` fills */
  condBidEscrow(state: AnyState, fills: Num): bigint | null;
  ifdBidSpend(state: AnyState, n: Num, p: Num): bigint | null;
  ifdBidMergeBudget(state: AnyState, m: Num): bigint | null;
  /** if-done buy entry: its escrow (limit spend of the whole amount, carriers per possible fill) */
  ifdBidEscrow(state: AnyState): bigint | null;
  ifdAskProceeds(state: AnyState, n: Num, p: Num): bigint | null;
  ifdAskPrefund(state: AnyState, n: Num): bigint | null;
  ifdAskMergeSelloutBack(state: AnyState, m: Num, exitValue: Num): bigint | null;
  /** if-done sell entry: its value (carrier plus the prefund of the whole amount and the exit carriers) */
  ifdAskEscrow(state: AnyState, carrier: Num): bigint | null;

  // ------------------------------------------------------------------ pair orders (crates/kob-protocol/src/state/pair.rs)
  // KobPair / KobCondPair / KobIfdPair: n = base units of A, prices B base units per WHOLE A, tips and carriers KAS (sompi).

  /** the two tokens of a pair order and its side (a buy-first entry is a `bid`) */
  pairTokens(state: AnyState): PairTokensInfo;
  /** the token programs of A and B */
  pairPrograms(state: AnyState): { a: { covId: Hex; program: TokenProgram }; b: { covId: Hex; program: TokenProgram } };
  /** the exact custodies an order holds, in record order (a sell-first pair entry: its A custody, then its B prefund) */
  custodies(state: AnyState): { token: Hex; amount: bigint }[];
  /** the builders' rules for a NEW order (throws with the first rule broken) */
  checkNewOrder(state: AnyState): void;
  /** the least KAS a new order UTXO must hold */
  minOrderValue(state: AnyState): bigint;
  /** S released by a fill of n at quote p (`KobPair`, `KobCondPair`): an ask n, a bid `floor(n * p / scale(A))` of B */
  pairSOut(state: AnyState, n: Num, p: Num): bigint | null;
  /** least T the maker receives: an ask `ceil(n * p / scale(A))` of B, a bid n of A */
  pairTOutMin(state: AnyState, n: Num, p: Num): bigint | null;
  /** KAS tip a fill of n releases, `floor(n * tip / scale(A))` (every pair kind) */
  pairTipKas(state: AnyState, n: Num): bigint | null;
  /** the covenant's complete rule of a `KobPair` fill (throws with the reason when refused) */
  pairFill(state: AnyState, n: Num, p: Num, value: Num): PairFill;
  /** the largest n anyone can take from a `KobPair` now at quote p (order UTXO `value` sompi; 0 = none) */
  pairMaxTakeable(state: AnyState, p: Num, value: Num): bigint;
  /** a `KobPair`'s highest quote (a rising bid's `priceEnd`, else `price`) */
  pairPriceMax(state: AnyState): bigint;
  /** a `KobPair` BID's B escrow for `amount` of A: `floor(amount * pMax / scale(A)) + 1` (exact floors are subadditive; `fills` is ignored) */
  pairBidEscrow(state: AnyState, amount: Num, fills: Num): bigint | null;
  /** KAS a pair order UTXO needs (`KobPair` / `KobCondPair`: `fills` delivery carriers + the tip of the amount; `KobIfdPair`: its carriers, ignores `fills`) */
  pairKasValue(state: AnyState, fills: Num): bigint | null;
  /** `ceil(amountLeft / minFill)` */
  pairMaxFills(state: AnyState): bigint;
  /** the deliveries a NEW `KobPair` / `KobCondPair` funds itself (1 when it never rests after a fill, else 2; TWAP / DCA one per slice); its least value is `pairKasValue(state, pairFundedFills(state))` */
  pairFundedFills(state: AnyState): bigint;
  /** `KobIfdPair`: the KAS every exit needs (the entry's `exitCarrier` at least) */
  ifdPairExitCarrierNeeded(state: AnyState): bigint | null;
  /** `KobCondPair` leg price (0 take-profit / limit, 1 stop) at time t; `trigger`: armed by this transaction's evidence */
  condPairLegPrice(state: AnyState, leg: 0 | 1, trigger: boolean, t: Num, utxoDaa: Num): bigint | null;
  /** `KobCondPair`: the band's worst stop, a BID's worst price, an ASK's lowest price */
  condPairBounds(state: AnyState): { stopWorst: bigint; worst: bigint; lowest: bigint };
  condPairRptProceeds(state: AnyState, n: Num): bigint | null;
  condPairRptBack(state: AnyState, n: Num): bigint | null;
  /** `KobCondPair` BID: B escrow for all of `amountLeft` at the worst leg in at most `fills` fills */
  condPairBidEscrow(state: AnyState, fills: Num): bigint | null;
  /** whether the evidence arms a pair stop (`KobCondPair`, a `KobIfdPair` stop entry); null where the covenant's arithmetic fails */
  pairArms(state: AnyState, evidence: PairEvidence): boolean | null;
  /** the valid and maximal trailing ratchet k (null: none) */
  condPairTrailK(state: AnyState, evidence: PairEvidence): bigint | null;
  condPairTrailCheck(state: AnyState, evidence: PairEvidence, k: Num): boolean;
  /** least B evidence (base units of B) of a two-KAS-book trigger at the current stop */
  pairMinTouchB(state: AnyState): bigint | null;
  /** `KobIfdPair`: its committed exit (a `KobCondPair` state, amountLeft / custody / repeat fields 0) */
  ifdPairExit(state: AnyState): OrderState;
  /** `KobIfdPair`: the exit a fill of n creates holding `custody`, booked with `parent` / `until` when given */
  ifdPairExitFor(state: AnyState, n: Num, custody: Num, booking?: { parent: Hex; until: Num } | null): OrderState;
  /** the `exitState` (hex, 432 bytes) an entry commits for the exit `exit` (a `KobCondPair` state) */
  ifdPairCommitExit(exit: AnyState): Hex;
  ifdPairPriceAt(state: AnyState, trigger: boolean, t: Num, utxoDaa: Num): bigint | null;
  /** `KobIfdPair` amounts of a fill of n at quote p (null where the arithmetic fails) */
  ifdPairAmounts(state: AnyState, n: Num, p: Num): { spend: bigint | null; proceeds: bigint | null; pre: bigint | null; mergeBudget: bigint | null };
  /** `KobIfdPair`: the B custody a new entry needs */
  ifdPairBCustodyNeeded(state: AnyState): bigint | null;
  /** two-KAS-book evidence: the implied rate `a * scale(B) / b` is at most / at least `x` (B per whole A) */
  impliedLe(a: Num, b: Num, x: Num, bScale: Num): boolean | null;
  impliedGe(a: Num, b: Num, x: Num, bScale: Num): boolean | null;
  /** `ceil(minTouch * stop / scale(A))` */
  minTouchB(minTouch: Num, stop: Num, aScale: Num): bigint | null;
  pairEvidenceModes(): { mode: number; name: 'kasBooks' | 'pair'; description: string }[];
  /** which resting sides arm (and trail) a pair stop; null without a stop */
  pairTriggerRule(state: AnyState): PairTriggerRule | null;
}

/** kob-wasm `pairFill`: the covenant amounts of one `KobPair` fill. */
export interface PairFill { sOut: bigint; tOut: bigint; tipKas: bigint; rest: boolean; outAmount: bigint }

/** A 64-bit integer argument: bigint, decimal string or a safe JS integer. */
export type Num = bigint | string | number;
/** `up` = what a maker receives, `down` = what a maker pays */
export type Round = 'up' | 'down';
/** kob-wasm `defaultConstants` (sompi / DAA / bps) */
export interface DefaultConstants {
  defaultOrderCarrier: bigint; defaultMinFillSompi: bigint; defaultMinFillImmediate: bigint; maxScale: bigint; quoteLimit: bigint; marketAuctionDaa: bigint; slippageBps: bigint;
  marketActivationDaa: bigint; iocLifeDaa: bigint; stopBandDaa: bigint; minRestDaa: bigint; daaRateMilli: bigint;
}

const j = <T>(s: string): T => JSON.parse(s) as T;

function wrap<T>(fn: string, f: () => T): T {
  try {
    return f();
  } catch (e) {
    if (e instanceof KobError) throw e;
    throw new KobError(e instanceof Error ? e.message : String(e), fn);
  }
}

/** Wraps a raw wasm-bindgen module. */
export function createKob(raw: RawKobWasm): KobWasm {
  const s = (v: bigint | string | number) => v.toString();
  const J = (st: AnyState) => JSON.stringify(st);
  const ob = (v: string | undefined | null): bigint | null => (v == null ? null : BigInt(v));
  return {
    raw,
    version: () => raw.version(),
    selfCheck: () => wrap('selfCheck', () => raw.selfCheck()),
    templates: () => wrap('templates', () => j(raw.templates())),
    touchOf: (l) => wrap('touchOf', () => j(raw.touchOf(JSON.stringify(l)))),
    build: (r) => wrap('build', () => j(raw.build(JSON.stringify(r)))),
    finalize: (b, sigs, o) => wrap('finalize', () => j(raw.finalize(JSON.stringify(b), JSON.stringify(sigs), o ? JSON.stringify(o) : ''))),
    validate: (t) => wrap('validate', () => j(raw.validate(JSON.stringify(t)))),
    masses: (t) => wrap('masses', () => j(raw.masses(JSON.stringify(t)))),
    encodeState: (st) => wrap('encodeState', () => raw.encodeState(JSON.stringify(st))),
    decodeState: (k, h) => wrap('decodeState', () => j(raw.decodeState(k, h))),
    redeemScript: (st) => wrap('redeemScript', () => raw.redeemScript(JSON.stringify(st))),
    scriptPublicKey: (st) => wrap('scriptPublicKey', () => raw.scriptPublicKey(JSON.stringify(st))),
    encodeTokenState: (st) => wrap('encodeTokenState', () => raw.encodeTokenState(JSON.stringify(st))),
    decodeTokenState: (h) => wrap('decodeTokenState', () => j(raw.decodeTokenState(h))),
    tokenScriptPublicKey: (p, st) => wrap('tokenScriptPublicKey', () => raw.tokenScriptPublicKey(p, JSON.stringify(st))),
    encodePayload: (r) => wrap('encodePayload', () => raw.encodePayload(JSON.stringify(r))),
    decodePayload: (h) => wrap('decodePayload', () => j(raw.decodePayload(h))),
    recoverOrders: (t) => wrap('recoverOrders', () => j(raw.recoverOrders(JSON.stringify(t)))),
    recoverAmends: (t, p) => wrap('recoverAmends', () => j(raw.recoverAmends(JSON.stringify(t), p ? JSON.stringify(p) : ''))),
    budgetTable: () => wrap('budgetTable', () => j(raw.budgetTable())),
    budgetFor: (r) => wrap('budgetFor', () => raw.budgetFor(r)),
    keeperTips: () => wrap('keeperTips', () => j(raw.keeperTips())),
    pairKeeperTips: () => wrap('pairKeeperTips', () => j(raw.pairKeeperTips())),
    pairTips: (a, b) => wrap('pairTips', () => j(raw.pairTips(a, b))),
    tipsFor: (st) => wrap('tipsFor', () => j(raw.tipsFor(J(st)))),
    dayOrder: (d0, t0, rate) => wrap('dayOrder', () => j(raw.dayOrder(s(d0), s(t0), rate == null ? '' : s(rate)))),
    mutableWindows: (k) => wrap('mutableWindows', () => j(raw.mutableWindows(k))),
    quote: (n, r, sc, rd) => wrap('quote', () => ob(raw.quote(s(n), s(r), s(sc), rd))),
    quoteExact: (n, r, sc, rd) => wrap('quoteExact', () => ob(raw.quoteExact(s(n), s(r), s(sc), rd))),
    checkScale: (sc) => wrap('checkScale', () => raw.checkScale(s(sc))),
    checkQuote: (a, r, sc, w) => wrap('checkQuote', () => raw.checkQuote(s(a), s(r), s(sc), w)),
    checkNumbers: (st) => wrap('checkNumbers', () => raw.checkNumbers(J(st))),
    minFillOk: (n, l, m) => wrap('minFillOk', () => raw.minFillOk(s(n), s(l), s(m))),
    defaultScale: (d) => wrap('defaultScale', () => BigInt(raw.defaultScale(d))),
    defaultMinFill: (a, p, sc) => wrap('defaultMinFill', () => BigInt(raw.defaultMinFill(s(a), s(p), s(sc)))),
    defaultMinFillIfd: (a) => wrap('defaultMinFillIfd', () => BigInt(raw.defaultMinFillIfd(s(a)))),
    defaultMinFillPair: (a, p, sc) => wrap('defaultMinFillPair', () => BigInt(raw.defaultMinFillPair(s(a), p == null ? '' : s(p), s(sc)))),
    defaultMinTouch: (m) => wrap('defaultMinTouch', () => BigInt(raw.defaultMinTouch(s(m)))),
    defaultMinFillSompi: () => wrap('defaultMinFillSompi', () => BigInt(raw.defaultMinFillSompi())),
    defaultConstants: () =>
      wrap('defaultConstants', () => {
        const o = j<Record<string, string>>(raw.defaultConstants());
        return Object.fromEntries(Object.entries(o).map(([k, v]) => [k, BigInt(v)])) as unknown as DefaultConstants;
      }),
    orderPriceAt: (st, t, d) => wrap('orderPriceAt', () => ob(raw.orderPriceAt(J(st), s(t), s(d)))),
    askProceeds: (st, n, t, d) => wrap('askProceeds', () => ob(raw.askProceeds(J(st), s(n), s(t), s(d)))),
    askProceedsAt: (st, n, p) => wrap('askProceedsAt', () => ob(raw.askProceedsAt(J(st), s(n), s(p)))),
    bidUsed: (st, n) => wrap('bidUsed', () => ob(raw.bidUsed(J(st), s(n)))),
    bidSpend: (st, n, t, d) => wrap('bidSpend', () => ob(raw.bidSpend(J(st), s(n), s(t), s(d)))),
    bidSpendAt: (st, n, p) => wrap('bidSpendAt', () => ob(raw.bidSpendAt(J(st), s(n), s(p)))),
    bidBudgetRate: (st) => wrap('bidBudgetRate', () => ob(raw.bidBudgetRate(J(st)))),
    bidBuyingPower: (st, v) => wrap('bidBuyingPower', () => BigInt(raw.bidBuyingPower(J(st), s(v)))),
    bidCanContinue: (st, l) => wrap('bidCanContinue', () => raw.bidCanContinue(J(st), s(l))),
    bidEscrow: (st, a, f) => wrap('bidEscrow', () => ob(raw.bidEscrow(J(st), s(a), s(f)))),
    fillOk: (st, n, v) => wrap('fillOk', () => raw.fillOk(J(st), s(n), s(v ?? 0))),
    condAskProceeds: (st, n, p) => wrap('condAskProceeds', () => ob(raw.condAskProceeds(J(st), s(n), s(p)))),
    condAskRptBudget: (st, n) => wrap('condAskRptBudget', () => ob(raw.condAskRptBudget(J(st), s(n)))),
    condBidSpend: (st, n, p) => wrap('condBidSpend', () => ob(raw.condBidSpend(J(st), s(n), s(p)))),
    condBidRptProceeds: (st, n) => wrap('condBidRptProceeds', () => ob(raw.condBidRptProceeds(J(st), s(n)))),
    condBidRptPrefund: (st, n) => wrap('condBidRptPrefund', () => ob(raw.condBidRptPrefund(J(st), s(n)))),
    condBidEscrow: (st, f) => wrap('condBidEscrow', () => ob(raw.condBidEscrow(J(st), s(f)))),
    ifdBidSpend: (st, n, p) => wrap('ifdBidSpend', () => ob(raw.ifdBidSpend(J(st), s(n), s(p)))),
    ifdBidMergeBudget: (st, m) => wrap('ifdBidMergeBudget', () => ob(raw.ifdBidMergeBudget(J(st), s(m)))),
    ifdBidEscrow: (st) => wrap('ifdBidEscrow', () => ob(raw.ifdBidEscrow(J(st)))),
    ifdAskProceeds: (st, n, p) => wrap('ifdAskProceeds', () => ob(raw.ifdAskProceeds(J(st), s(n), s(p)))),
    ifdAskPrefund: (st, n) => wrap('ifdAskPrefund', () => ob(raw.ifdAskPrefund(J(st), s(n)))),
    ifdAskMergeSelloutBack: (st, m, v) => wrap('ifdAskMergeSelloutBack', () => ob(raw.ifdAskMergeSelloutBack(J(st), s(m), s(v)))),
    ifdAskEscrow: (st, c) => wrap('ifdAskEscrow', () => ob(raw.ifdAskEscrow(J(st), s(c)))),
    pairTokens: (st) => wrap('pairTokens', () => j(raw.pairTokens(J(st)))),
    pairPrograms: (st) => wrap('pairPrograms', () => j(raw.pairPrograms(J(st)))),
    custodies: (st) => wrap('custodies', () => j<{ token: Hex; amount: string }[]>(raw.custodies(J(st))).map((c) => ({ token: c.token, amount: BigInt(c.amount) }))),
    checkNewOrder: (st) => wrap('checkNewOrder', () => raw.checkNewOrder(J(st))),
    minOrderValue: (st) => wrap('minOrderValue', () => BigInt(raw.minOrderValue(J(st)))),
    pairSOut: (st, n, p) => wrap('pairSOut', () => ob(raw.pairSOut(J(st), s(n), s(p)))),
    pairTOutMin: (st, n, p) => wrap('pairTOutMin', () => ob(raw.pairTOutMin(J(st), s(n), s(p)))),
    pairTipKas: (st, n) => wrap('pairTipKas', () => ob(raw.pairTipKas(J(st), s(n)))),
    pairFill: (st, n, p, v) =>
      wrap('pairFill', () => {
        const f = j<{ sOut: string; tOut: string; tipKas: string; rest: boolean; outAmount: string }>(raw.pairFill(J(st), s(n), s(p), s(v)));
        return { sOut: BigInt(f.sOut), tOut: BigInt(f.tOut), tipKas: BigInt(f.tipKas), rest: f.rest, outAmount: BigInt(f.outAmount) };
      }),
    pairMaxTakeable: (st, p, v) => wrap('pairMaxTakeable', () => BigInt(raw.pairMaxTakeable(J(st), s(p), s(v)))),
    pairPriceMax: (st) => wrap('pairPriceMax', () => BigInt(raw.pairPriceMax(J(st)))),
    pairBidEscrow: (st, a, f) => wrap('pairBidEscrow', () => ob(raw.pairBidEscrow(J(st), s(a), s(f)))),
    pairKasValue: (st, f) => wrap('pairKasValue', () => ob(raw.pairKasValue(J(st), s(f)))),
    pairMaxFills: (st) => wrap('pairMaxFills', () => BigInt(raw.pairMaxFills(J(st)))),
    pairFundedFills: (st) => wrap('pairFundedFills', () => BigInt(raw.pairFundedFills(J(st)))),
    ifdPairExitCarrierNeeded: (st) => wrap('ifdPairExitCarrierNeeded', () => ob(raw.ifdPairExitCarrierNeeded(J(st)))),
    condPairLegPrice: (st, leg, tr, t, d) => wrap('condPairLegPrice', () => ob(raw.condPairLegPrice(J(st), s(leg), String(tr), s(t), s(d)))),
    condPairBounds: (st) =>
      wrap('condPairBounds', () => {
        const b = j<Record<'stopWorst' | 'worst' | 'lowest', string>>(raw.condPairBounds(J(st)));
        return { stopWorst: BigInt(b.stopWorst), worst: BigInt(b.worst), lowest: BigInt(b.lowest) };
      }),
    condPairRptProceeds: (st, n) => wrap('condPairRptProceeds', () => ob(raw.condPairRptProceeds(J(st), s(n)))),
    condPairRptBack: (st, n) => wrap('condPairRptBack', () => ob(raw.condPairRptBack(J(st), s(n)))),
    condPairBidEscrow: (st, f) => wrap('condPairBidEscrow', () => ob(raw.condPairBidEscrow(J(st), s(f)))),
    pairArms: (st, ev) => wrap('pairArms', () => raw.pairArms(J(st), JSON.stringify(ev)) ?? null),
    condPairTrailK: (st, ev) => wrap('condPairTrailK', () => ob(raw.condPairTrailK(J(st), JSON.stringify(ev)))),
    condPairTrailCheck: (st, ev, k) => wrap('condPairTrailCheck', () => raw.condPairTrailCheck(J(st), JSON.stringify(ev), s(k))),
    pairMinTouchB: (st) => wrap('pairMinTouchB', () => ob(raw.pairMinTouchB(J(st)))),
    ifdPairExit: (st) => wrap('ifdPairExit', () => j(raw.ifdPairExit(J(st)))),
    ifdPairExitFor: (st, n, c, b) => wrap('ifdPairExitFor', () => j(raw.ifdPairExitFor(J(st), s(n), s(c), b ? b.parent : '', b ? s(b.until) : '0'))),
    ifdPairCommitExit: (x) => wrap('ifdPairCommitExit', () => raw.ifdPairCommitExit(J(x))),
    ifdPairPriceAt: (st, tr, t, d) => wrap('ifdPairPriceAt', () => ob(raw.ifdPairPriceAt(J(st), String(tr), s(t), s(d)))),
    ifdPairAmounts: (st, n, p) =>
      wrap('ifdPairAmounts', () => {
        const a = j<Record<'spend' | 'proceeds' | 'pre' | 'mergeBudget', string | null>>(raw.ifdPairAmounts(J(st), s(n), s(p)));
        return { spend: ob(a.spend), proceeds: ob(a.proceeds), pre: ob(a.pre), mergeBudget: ob(a.mergeBudget) };
      }),
    ifdPairBCustodyNeeded: (st) => wrap('ifdPairBCustodyNeeded', () => ob(raw.ifdPairBCustodyNeeded(J(st)))),
    impliedLe: (a, b, x, sc) => wrap('impliedLe', () => raw.impliedLe(s(a), s(b), s(x), s(sc)) ?? null),
    impliedGe: (a, b, x, sc) => wrap('impliedGe', () => raw.impliedGe(s(a), s(b), s(x), s(sc)) ?? null),
    minTouchB: (m, st, sc) => wrap('minTouchB', () => ob(raw.minTouchB(s(m), s(st), s(sc)))),
    pairEvidenceModes: () => wrap('pairEvidenceModes', () => j(raw.pairEvidenceModes())),
    pairTriggerRule: (st) => wrap('pairTriggerRule', () => j(raw.pairTriggerRule(J(st)))),
  };
}
