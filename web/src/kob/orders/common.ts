// Shared plumbing of the order planners (simple and conditional).
//
// It covers state construction with the token's identity, KAS and token selection, the
// `createOrder` build with an adaptive fee reserve, the recover-and-compare self-check and the
// Disclosure numbers.
//
// A planner validates (guards), maps the intent to an exact OrderState, calls `buildCreate`, then
// `makeDisclosure` and `finishPlan`. All protocol logic (tx layout, state encoding, fees, signing
// digests) stays in kob-wasm. This module only prepares requests.
import type { CarrierLine, Disclosure, OrderPlan, PlanEnv, PlanIssue } from '../plan-types';
import type { AskState, BidState, BuiltTx, CreateOrderRequest, KeyUtxo, OrderState, PayloadRecord, TokenUtxo } from '../types';
import { atFloor, buildWithCap, recordFee } from '../fee-policy';
import { guardRequest } from '../request-guard';
import { KobError } from '../wasm';
import { InsufficientFunds, DEFAULT_FEE_RESERVE, selectFunding, selectTokens } from '../funding';
import { MAX_TOKEN_INPUTS } from '../guards';
import { daaToUnix } from '../daa';
import { isKind, kindFor } from '../order-facts';
import { KRON_MAX_OUTPUT_AMOUNT, extensionOfState, isKeyOwned, isKronState, isPlainState } from '../token-state';
import { issue, hasError } from './common-issues';

export { issue, customIssue, hasError, ISSUE_CATALOG } from './common-issues';
export type { IssueCode } from './common-issues';

/**
 * Default KAS carrier of every covenant UTXO (order, custody, delivery, exit): 1 KAS.
 *
 * Mirrors kob-wasm `defaultConstants().defaultOrderCarrier` and kob-protocol
 * `DEFAULT_ORDER_CARRIER`. A lower carrier raises the storage mass. That lowers the block-template
 * priority under congestion and, on large 8/8 and 10/10 batches, can push the storage mass over
 * the 500,000 limit. KaspaCom's 0.5 KAS token-output floor is below it.
 */
export const DEFAULT_CARRIER = 100_000_000n;
/** Fills a plain GTC bid budgets delivery carriers for by default, one carrier per fill. */
export const DEFAULT_BID_FILLS = 3n;

/** The order carrier of a plan environment: its own override or DEFAULT_CARRIER. */
export const carrierOf = (env: Pick<PlanEnv, 'carrier'>): bigint => env.carrier ?? DEFAULT_CARRIER;

/** Smallest output (0.02 KAS) whose KIP-9 storage mass alone fits the block limit. */
export const DUST_OUTPUT_MIN = 2_000_000n;
/**
 * Smallest output with a covenant id (0.08 KAS) whose KIP-9 storage mass fits the block limit.
 *
 * It is four times the plain bound (kob-protocol `tx::COVENANT_OUTPUT_MIN`) and the least KAS of
 * every carrier of a new order (matcher.md 10.1).
 */
export const COVENANT_OUTPUT_MIN = 8_000_000n;

/**
 * The least KAS a token output of the program `tplHash` can carry (a transfer, a merge).
 *
 * It is the larger of the program's own floor (kob-wasm `templates`, `minTokenOutput`) and the
 * dust bound.
 */
export function tokenCarrierFloor(kob: PlanEnv['kob'], tplHash: string): bigint {
  const t = kob.templates().find((x) => x.hash === tplHash && x.tokenSlots);
  const program = t?.minTokenOutput !== undefined ? BigInt(t.minTokenOutput) : 1n;
  return program > DUST_OUTPUT_MIN ? program : DUST_OUTPUT_MIN;
}

/**
 * The least KAS a carrier of a new order can hold when it becomes a token output of `tplHash`.
 *
 * That applies to the custody and to a delivery. It is the larger of the token output's floor and
 * the KIP-9 covenant output bound (kob-protocol `check_new_order`, matcher.md 10.1).
 */
export function orderCarrierFloor(kob: PlanEnv['kob'], tplHash: string): bigint {
  const floor = tokenCarrierFloor(kob, tplHash);
  return floor > COVENANT_OUTPUT_MIN ? floor : COVENANT_OUTPUT_MIN;
}

/** Checks the carriers of a new order against their floors (token outputs and exit UTXOs). */
function carrierIssues(env: PlanEnv, order: OrderState, tokenCarrier: bigint): PlanIssue[] {
  const out: PlanIssue[] = [];
  const s = order.state as unknown as Record<string, string>;
  const own = s.tokenTplHash;
  const check = (field: string, v: bigint | string | undefined, tplHash: string | null) => {
    if (v === undefined) return;
    const floor = tplHash === null ? COVENANT_OUTPUT_MIN : orderCarrierFloor(env.kob, tplHash);
    if (BigInt(v) < floor) out.push(issue('CARRIER_BELOW_FLOOR', { carrier: BigInt(v), floor }, field));
  };
  if (tokenCarrier > 0n) check('carrier', tokenCarrier, own);
  if (isKind(order, 'KobBid') || isKind(order, 'KobCondBid') || isKind(order, 'KobIfdBid')) check('deliveryCarrier', s.deliveryCarrier, own);
  if (isKind(order, 'KobIfdBid')) check('exitCarrier', s.exitCarrier, null);
  if (isKind(order, 'KobIfdAsk')) check('exitCarrier', s.exitCarrier, own);
  return out;
}
/** Decimal string of a bigint, the form order-state fields take. */
export const str = (v: bigint): string => v.toString();

// ---- order states

/** Parameters of a KobAsk in planner terms: bigint, base-unit amounts, sompi per whole token. */
export interface AskParams {
  /** base units (the custody) */
  amount: bigint;
  /** smallest fill in base units unless a fill takes everything left */
  minFill: bigint;
  price: bigint;
  tip?: bigint;
  /** 0 GTC/GTD (default), 1 IOC, 2 FOK */
  tif?: 0 | 1 | 2;
  activeFrom?: bigint;
  expiryDaa: bigint;
  /** at least the token program's default (matcher.md §5): a lower value is raised to it */
  refundTip?: bigint;
  interval?: bigint;
  /** TWAP / DCA: most base units per fill (0 = off) */
  maxFill?: bigint;
  /** sompi per whole token per decayStep; 0 = no decay */
  slope?: bigint;
  priceEnd?: bigint;
  decayStep?: bigint;
}

/**
 * The refund tip a new order carries: the caller's, but never below the program's default.
 *
 * Wallets must set at least the default (matcher.md §5). A lower tip leaves the order to the
 * maker's own refund or cancel.
 */
export const refundTipFor = (m: Pick<PlanEnv['token'], 'refundTip'>, want?: bigint): bigint =>
  want !== undefined && want > m.refundTip ? want : m.refundTip;

/** Builds the KobAsk state of an order for the token in `env`. */
export function makeAsk(env: PlanEnv, p: AskParams): OrderState {
  const m = env.token;
  const st: AskState = {
    maker: env.maker,
    tokenCovId: m.covenantId,
    tokenTplHash: m.templateHash,
    tplPrefixLen: String(m.prefixLen),
    tplSuffixLen: String(m.suffixLen),
    scale: str(m.scale),
    minFill: str(p.minFill),
    price: str(p.price),
    tip: str(p.tip ?? 0n),
    tif: String(p.tif ?? 0),
    activeFrom: str(p.activeFrom ?? 0n),
    expiryDaa: str(p.expiryDaa),
    refundTip: str(refundTipFor(m, p.refundTip)),
    interval: str(p.interval ?? 0n),
    maxFill: str(p.maxFill ?? 0n),
    slope: str(p.slope ?? 0n),
    priceEnd: str(p.priceEnd ?? 0n),
    decayStep: str(p.decayStep ?? 1n),
    amountLeft: str(p.amount),
    extensionCommitment: m.extensionCommitment,
  };
  return { kind: kindFor('KobAsk', m.family), state: st } as OrderState;
}

/** Parameters of a KobBid: AskParams plus the budget knobs. Its quantity is the escrow. */
export interface BidParams extends Omit<AskParams, 'amount'> {
  reserve?: bigint;
  deliveryCarrier?: bigint;
}

/** Builds the KobBid state of an order for the token in `env`. */
export function makeBid(env: PlanEnv, p: BidParams): OrderState {
  const m = env.token;
  const st: BidState = {
    maker: env.maker,
    tokenCovId: m.covenantId,
    tokenTplHash: m.templateHash,
    tplPrefixLen: String(m.prefixLen),
    tplSuffixLen: String(m.suffixLen),
    extensionCommitment: m.extensionCommitment,
    scale: str(m.scale),
    minFill: str(p.minFill),
    price: str(p.price),
    tip: str(p.tip ?? 0n),
    tif: String(p.tif ?? 0),
    activeFrom: str(p.activeFrom ?? 0n),
    expiryDaa: str(p.expiryDaa),
    refundTip: str(refundTipFor(m, p.refundTip)),
    reserve: str(p.reserve ?? 0n),
    deliveryCarrier: str(p.deliveryCarrier ?? carrierOf(env)),
    interval: str(p.interval ?? 0n),
    maxFill: str(p.maxFill ?? 0n),
    slope: str(p.slope ?? 0n),
    priceEnd: str(p.priceEnd ?? 0n),
    decayStep: str(p.decayStep ?? 1n),
  };
  return { kind: kindFor('KobBid', m.family), state: st } as OrderState;
}

/**
 * Budget rate of a bid: the all-in price at its cap, `priceMax + tip` sompi per whole token.
 *
 * A rising bid's cap is its priceEnd. A fill of n consumes `ceil(n * rate / scale)` of the escrow
 * (kob-wasm `bidUsed`), whatever the auction price was.
 */
export function bidBudgetRate(price: bigint, tip: bigint, slope: bigint, priceEnd: bigint): bigint {
  const cap = slope > 0n && priceEnd > price ? priceEnd : price;
  return cap + tip;
}

/**
 * KAS to put on a bid (its whole escrow) for `amount` base units in at most `fills` fills.
 *
 * The result is kob-wasm `bidEscrow`, equal to kob-protocol `BidState::escrow`:
 * `used(amount) + (fills - 1) + fills * deliveryCarrier + reserve`. Every fill pays one delivery
 * carrier out of the escrow. The last fill's carrier rides on the delivery. Each fill's budget is
 * rounded up, hence one sompi of slack per extra fill. Budgeting fewer fills than happen leaves
 * the tail of the order unfillable. Returns null when the covenant arithmetic overflows.
 */
export const bidEscrowOf = (kob: PlanEnv['kob'], bid: OrderState, amount: bigint, fills: bigint): bigint | null => kob.bidEscrow(bid, amount, fills);

/**
 * The wallet's default minimum fill of a resting order (kob-wasm `defaultMinFill`).
 *
 * It is the amount worth 10 KAS at the limit `price` (a notional, not a carrier), at least 1 base
 * unit and at most the amount. IOC, FOK and market orders use 1 because they live one auction.
 */
export const defaultMinFillFor = (env: Pick<PlanEnv, 'kob' | 'token'>, amount: bigint, price: bigint): bigint =>
  amount <= 0n ? 1n : env.kob.defaultMinFill(amount, price > 0n ? price : 0n, env.token.scale);

// ---- selecting and building

/** The maker's P2PK-owned token UTXOs that can fund an order (same token and extension). */
export function usableTokenUtxos(env: Pick<PlanEnv, 'token' | 'maker' | 'tokenUtxos'>): TokenUtxo[] {
  const m = env.token;
  return env.tokenUtxos.filter(
    (u) =>
      u.covenantId === m.covenantId &&
      isKeyOwned(u.state, m.program) &&
      u.state.owner === env.maker &&
      isPlainState(u.state) &&
      (m.family === 'kron' ? isKronState(u.state) : extensionOfState(u.state) === m.extensionCommitment),
  );
}

/** Base units held in usable token UTXOs (for "close all"). */
export const heldAmount = (env: PlanEnv): bigint => usableTokenUtxos(env).reduce((s, u) => s + BigInt(u.state.amount), 0n);

/** What `buildCreate` needs to create one order: the state, the KAS on it and the custody. */
export interface CreateSpec {
  order: OrderState;
  /** KAS on the order UTXO: carrier (token kinds) or the whole escrow (bid kinds) */
  value: bigint;
  /** token base units the custody must hold (`amountLeft`); 0 for bid kinds */
  tokenAmount: bigint;
  /** day orders: wall-clock deadline for the placement record */
  deadline?: bigint | null;
  records?: PayloadRecord[];
}

/** The outcome of `buildCreate`: the request and built transaction, or error issues. */
export interface CreateResult {
  request: CreateOrderRequest | null;
  built: BuiltTx | null;
  /** KAS carrier used for the custody token UTXO and the token change (0 for bids) */
  tokenCarrier: bigint;
  /** token base units returned as change (0 when the selection was exact or for bids) */
  tokenChange: bigint;
  /** total KAS of the token inputs (their carriers are recycled into the outputs) */
  tokenInputsKas: bigint;
  issues: PlanIssue[];
}

const INSUFFICIENT_RE = /insufficient funds: need (\d+) sompi, have (\d+) sompi/;
const spendable = (u: KeyUtxo[]): bigint => u.filter((x) => !x.covenantId).reduce((s, x) => s + BigInt(x.amount), 0n);

/**
 * Selects tokens and funding, builds the `createOrder` transaction and self-checks it.
 *
 * It never throws for user problems. A shortfall or a protocol refusal comes back as an error
 * issue with `built: null`. The network fee depends on the number of inputs and the token program,
 * so the reserve added to the KAS need starts at DEFAULT_FEE_RESERVE. It grows by the shortfall
 * kob-wasm reports until the build succeeds or the wallet cannot pay.
 */
export function buildCreate(env: PlanEnv, spec: CreateSpec): CreateResult {
  const res = buildCreateAt(env, spec);
  if (res.built) return res;
  // Retry at the fee floor so a fee estimate never makes a payable order impossible.
  const low = res.issues.some((i) => i.code === 'INSUFFICIENT_KAS') ? atFloor(env) : null;
  return low ? buildCreateAt(low, spec) : res;
}

function buildCreateAt(env: PlanEnv, spec: CreateSpec): CreateResult {
  const issues: PlanIssue[] = [];
  const fail = (extra: PlanIssue): CreateResult => ({ request: null, built: null, tokenCarrier: 0n, tokenChange: 0n, tokenInputsKas: 0n, issues: [...issues, extra] });
  const carrier = carrierOf(env);
  // A carrier below its floor makes every fill unminable, so refuse it here with its own message.
  const low = carrierIssues(env, spec.order, spec.tokenAmount > 0n ? carrier : 0n);
  if (low.length > 0) return { request: null, built: null, tokenCarrier: 0n, tokenChange: 0n, tokenInputsKas: 0n, issues: [...issues, ...low] };
  let tokens: TokenUtxo[] = [];
  let tokenChange = 0n;
  let tokenCarrier = 0n;
  let tokenInputsKas = 0n;
  let need = spec.value;

  if (spec.tokenAmount > 0n) {
    try {
      // The covering set, then spare slots take the wallet's smallest other UTXOs of the token.
      // They ride on the one token change, their carriers are recycled (tokenInputsKas) and the
      // surplus returns as KAS change.
      tokens = selectTokens(usableTokenUtxos(env), spec.tokenAmount, Math.min(env.token.slots.inputs, MAX_TOKEN_INPUTS), env.token.program, { ...(env.token.family === 'kron' ? { changeCap: KRON_MAX_OUTPUT_AMOUNT } : {}), ...(env.freshTxids ? { fresh: env.freshTxids } : {}) });
    } catch (e) {
      if (e instanceof InsufficientFunds) {
        // `needed` lets the sell ticket merge just enough for this amount (ui/ticket/OrderTicket.tsx).
        return fail(
          e.kind === 'fragmented'
            ? issue('TOKEN_UTXOS_FRAGMENTED', { max: e.maxInputs ?? 0, needed: e.needed })
            : issue('INSUFFICIENT_TOKENS', { needed: e.needed, have: e.have, shortfall: e.shortfall }),
        );
      }
      throw e;
    }
    tokenCarrier = carrier;
    const have = tokens.reduce((s, u) => s + BigInt(u.state.amount), 0n);
    tokenChange = have - spec.tokenAmount;
    tokenInputsKas = tokens.reduce((s, u) => s + BigInt(u.amount), 0n);
    // Outputs are the order, the custody and the token change. The token inputs' KAS is recycled.
    need = spec.value + tokenCarrier + (tokenChange > 0n ? tokenCarrier : 0n);
    need = need > tokenInputsKas ? need - tokenInputsKas : 0n;
  }

  const balance = spendable(env.funding);
  if (balance < need) return fail(issue('INSUFFICIENT_KAS', { needed: need, have: balance, shortfall: need - balance }));

  const mk = (funding: KeyUtxo[]): CreateOrderRequest => {
    const r: CreateOrderRequest = { action: 'createOrder', order: spec.order, value: str(spec.value), funding };
    if (tokens.length) {
      r.tokens = tokens;
      r.tokenCarrier = str(tokenCarrier);
    }
    if (env.changeTo) r.change = env.changeTo;
    if (spec.deadline != null) r.deadline = str(spec.deadline);
    if (spec.records?.length) r.records = spec.records;
    if (env.feeRate !== undefined || env.feeMode) r.fee = { ...(env.feeRate !== undefined ? { feeRate: str(env.feeRate) } : {}), ...(env.feeMode ? { feeMode: env.feeMode } : {}) };
    return r;
  };

  let reserve = DEFAULT_FEE_RESERVE;
  for (let attempt = 0; attempt < 5; attempt++) {
    const room = balance - need;
    const r = reserve < room ? reserve : room;
    let funding: KeyUtxo[];
    try {
      funding = selectFunding(env.funding, need, r, undefined, env.freshTxids);
    } catch (e) {
      if (e instanceof InsufficientFunds) {
        return fail(e.kind === 'fragmented' ? issue('FUNDING_FRAGMENTED', { max: e.maxInputs ?? 0 }) : issue('INSUFFICIENT_KAS', { needed: e.needed, have: e.have, shortfall: e.shortfall }));
      }
      throw e;
    }
    const first = mk(funding);
    try {
      // A fee above the policy cap is rebuilt once at a lower rate (fee-policy.ts).
      const { built, request, rate } = buildWithCap(env.fees?.policy, first, (r) => env.kob.build(guardRequest(r, { maker: env.maker, changeTo: env.changeTo })));
      const mismatch = verifyBuilt(env, built, spec);
      if (mismatch) return fail(issue('PLAN_MISMATCH', { reason: mismatch }));
      recordFee(env, built, rate);
      return { request, built, tokenCarrier, tokenChange, tokenInputsKas, issues };
    } catch (e) {
      const msg = e instanceof KobError ? e.message : String(e);
      const m = INSUFFICIENT_RE.exec(msg);
      if (!m) return fail(issue('BUILD_REJECTED', { reason: msg.replace(/^kob-wasm build: (invalid request: )?/, '') }));
      const shortBy = BigInt(m[1]) - BigInt(m[2]);
      if (r >= room) return fail(issue('INSUFFICIENT_KAS', { needed: balance + shortBy, have: balance, shortfall: shortBy }));
      reserve = r + shortBy + 1_000_000n;
    }
  }
  return fail(issue('BUILD_REJECTED', { reason: 'could not size the network fee' }));
}

/**
 * Independent check that what was built is what was planned.
 *
 * The KOB1 placement record recovered from the transaction (kob-wasm `recoverOrders`, which trusts
 * nothing) must yield the planned state, value and deadline. Returns a reason on mismatch.
 */
export function verifyBuilt(env: PlanEnv, built: BuiltTx, spec: CreateSpec): string | null {
  let recovered;
  try {
    recovered = env.kob.recoverOrders(built.tx);
  } catch (e) {
    return `placement record does not verify: ${e instanceof Error ? e.message : String(e)}`;
  }
  const r = recovered.find((x) => x.output === 0) ?? recovered[0];
  if (!r) return 'no placement record';
  if (env.kob.encodeState(r.order) !== env.kob.encodeState(spec.order)) return 'recovered order state differs';
  if (BigInt(r.value) !== spec.value) return 'recovered order value differs';
  const dl = r.deadline == null ? null : BigInt(r.deadline);
  if ((spec.deadline ?? null) !== dl) return 'recovered deadline differs';
  if (spec.tokenAmount > 0n && (!r.custody || BigInt(r.custody.state.amount) !== spec.tokenAmount)) return 'custody amount differs';
  return null;
}

// ---- disclosure

/** Inputs of `makeDisclosure`: the planned order and the prices to show. */
export interface DescribeParams {
  /** the order (ask or bid) the disclosure is about */
  order: OrderState;
  /** base units traded (asks: amountLeft; bids: the amount the escrow was sized for) */
  amount: bigint;
  /** the order's limit or worst bound, sompi per whole token */
  limitPrice: bigint | null;
  expectedPrice: bigint | null;
  worstPrice: bigint | null;
  expiryKind: Disclosure['expiry']['kind'];
  /** timed activation to show ("starts at") */
  activatesAt?: bigint | null;
  /** bids: deliveryCarriers budgeted (fills) */
  fills?: bigint;
  notes?: string[];
}

/**
 * The numbers the entry screen shows, derived from the planned state and the built tx.
 *
 * They never come from form text. Carrier lines (returned on cancel, refund or fill) itemise every
 * KAS the order locks. `kasLocked` sums the locked lines. The token-change carrier stays in a UTXO
 * the wallet owns and is marked `kept`.
 */
export function makeDisclosure(env: PlanEnv, res: CreateResult, p: DescribeParams): Disclosure {
  const o = p.order;
  const notes = [...(p.notes ?? [])];
  const carriers: CarrierLine[] = [];
  let side: 'sell' | 'buy';
  let tip: bigint;
  let refundTip: bigint;
  let expiryDaa: bigint;
  let tokensEscrowed = 0n;
  // All-in total of the whole amount at the limit by the covenant's quote rule. An ask receives at
  // least ceil(n (p - tip) / scale). A bid pays at most floor(n (p + tip) / scale).
  let allInTotal: bigint | null = null;
  if (isKind(o, 'KobAsk')) {
    side = 'sell';
    const s = o.state;
    tip = BigInt(s.tip);
    refundTip = BigInt(s.refundTip);
    expiryDaa = BigInt(s.expiryDaa);
    tokensEscrowed = BigInt(s.amountLeft);
    if (p.limitPrice !== null) allInTotal = env.kob.askProceedsAt(o, p.amount, p.limitPrice);
    carriers.push({ kind: 'orderCarrier', amount: BigInt(res.request?.value ?? 0), count: 1 });
    carriers.push({ kind: 'tokenCarrier', amount: res.tokenCarrier, count: 1 });
    if (res.tokenChange > 0n) carriers.push({ kind: 'tokenChangeCarrier', amount: res.tokenCarrier, count: 1, kept: true });
    notes.push('carrierReturned');
  } else if (isKind(o, 'KobBid')) {
    side = 'buy';
    const s = o.state;
    tip = BigInt(s.tip);
    refundTip = BigInt(s.refundTip);
    expiryDaa = BigInt(s.expiryDaa);
    const fills = p.fills ?? 1n;
    const carrier = BigInt(s.deliveryCarrier);
    const reserve = BigInt(s.reserve);
    // The escrow is kob-wasm bidEscrow: the budget at the cap, the carriers and the reserve.
    const total = res.request ? BigInt(res.request.value) : (env.kob.bidEscrow(o, p.amount, fills) ?? 0n);
    carriers.push({ kind: 'escrow', amount: total - carrier * fills - reserve, count: 1 });
    carriers.push({ kind: 'deliveryCarrier', amount: carrier, count: Number(fills) });
    if (reserve > 0n) carriers.push({ kind: 'reserve', amount: reserve, count: 1 });
    if (p.limitPrice !== null) allInTotal = env.kob.bidSpendAt(o, p.amount, p.limitPrice);
    notes.push('carrierReturned');
  } else {
    throw new Error(`makeDisclosure: ${o.kind} is described by the conditional planner`);
  }
  const kasLocked = carriers.filter((c) => !c.kept).reduce((s, c) => s + c.amount * BigInt(c.count), 0n);
  const allInPrice = p.limitPrice === null ? null : side === 'sell' ? p.limitPrice - tip : p.limitPrice + tip;
  const showActive = p.activatesAt != null && p.activatesAt > env.clock.daa;
  return {
    side,
    tokenAmount: p.amount,
    scale: BigInt(o.state.scale),
    minFill: BigInt(o.state.minFill),
    minTouch: null,
    limitPrice: p.limitPrice,
    allInPrice,
    allInTotal,
    expectedPrice: p.expectedPrice,
    worstPrice: p.worstPrice,
    tip,
    carriers,
    kasLocked,
    tokensEscrowed,
    fee: res.built ? BigInt(res.built.fee.fee) : null,
    refundTip,
    keeperTip: 0n,
    expiry: {
      kind: p.expiryKind,
      daa: expiryDaa,
      approxUnixSeconds: daaToUnix(env.clock, expiryDaa),
      deadlineUnixSeconds: res.request?.deadline != null ? BigInt(res.request.deadline) : null,
    },
    activatesAt: showActive ? { daa: p.activatesAt as bigint, approxUnixSeconds: daaToUnix(env.clock, p.activatesAt as bigint) } : null,
    notes,
  };
}


// ---- plan assembly

/** A plan that failed before anything was built. */
export const failedPlan = (issues: PlanIssue[], states: OrderState[] = []): OrderPlan => ({ ok: false, issues, states, request: null, built: null, disclosure: null });

/** Assembles the OrderPlan: ok only if no error issue exists and the transaction was built. */
export function finishPlan(issues: PlanIssue[], states: OrderState[], res: CreateResult, disclosure: Disclosure | null): OrderPlan {
  const all = [...issues, ...res.issues];
  return {
    ok: !hasError(all) && res.built !== null,
    issues: all,
    states,
    request: res.request,
    built: res.built,
    disclosure: res.built ? disclosure : null,
  };
}

