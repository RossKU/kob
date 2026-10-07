// Shared plumbing of the pair planners (pair-simple.ts, pair-cond.ts, pair-ifd.ts): the pair order states (`KobPair`, `KobCondPair`,
// `KobIfdPair`) built from the two TokenMarkets of a PairPlanEnv, the pair tips, the price / notional / tip checks in B, the `createOrder` build
// that draws EACH custody from the maker's UTXOs of its own token (A from `env.tokenUtxos`, B from `env.pair.quoteTokenUtxos`), the self-check
// of the built transaction (placement record, value, deadline, every custody at its amount and owned by its token) and the disclosure assembly.
//
// Units: amounts base units of A (custodies: base units of their own token), prices B base units per WHOLE A (`scale(A)`), tips / carriers /
// keeper tips KAS (sompi; `tip` sompi per whole A). Every rounded amount comes from kob-wasm (custodies, escrows, tips, B totals); TypeScript only
// assembles states and requests.
import type { CarrierLine, Disclosure, PairDisclosure, PairOrderPlan, PairPlanEnv, PairSideFacts, PlanEnv, PlanIssue, TokenMarket } from '../plan-types';
import type { BuiltTx, CondPairState, CreateOrderRequest, Hex, IfdPairState, KeyUtxo, OrderState, PairState, TokenUtxo } from '../types';
import { atFloor, buildWithCap, recordFee } from '../fee-policy';
import { guardRequest } from '../request-guard';
import { KobError } from '../wasm';
import { InsufficientFunds, DEFAULT_FEE_RESERVE, selectFunding, selectTokens } from '../funding';
import { MAX_TOKEN_INPUTS, QUOTE_LIMIT } from '../guards';
import { daaToUnix } from '../daa';
import { KRON_MAX_OUTPUT_AMOUNT, extensionOfState, isKeyOwned, isKronState, isPlainState } from '../token-state';
import { fitsI64 } from '../units';
import { issue, hasError } from './common-issues';
import { DEFAULT_CARRIER, str, tokenCarrierFloor } from './common';
import type { CondSummary } from './cond-common';
import type { Legs } from './cond-legs';
import { pairIssue } from './pair-issues';

export const ZERO32_HEX = '0'.repeat(64);
/** KAS fills a resting pair order budgets a delivery carrier for by default (as a KAS bid: at most 3, one per possible fill). */
export const DEFAULT_PAIR_FILLS = 3n;
/** Fills each if-done EXIT budgets a delivery carrier for inside its exitCarrier (a take-profit filled in up to two parts). */
export const DEFAULT_EXIT_FILLS = 2n;

// ------------------------------------------------------------------------------------------------ tokens, tips, environment

export const familyCode = (m: Pick<TokenMarket, 'family'>): string => (m.family === 'kron' ? '2' : '1');
/** The extension commitment of NEW outputs of a token (zero for KRON, which has none). */
export const extOf = (m: Pick<TokenMarket, 'family' | 'extensionCommitment'>): Hex => (m.family === 'kron' ? ZERO32_HEX : m.extensionCommitment);

export const sideFacts = (m: TokenMarket): PairSideFacts => ({ covenantId: m.covenantId, ticker: m.ticker, decimals: m.decimals, scale: m.scale, family: m.family, program: m.program });

/** Default tips of a pair order of this pair (kob-wasm `pairTips` = `tipsFor` of a pair state: the pair table of the two programs). */
export function pairTipsOf(env: PairPlanEnv): { refundTip: bigint; keeperTip: bigint } {
  const t = env.kob.pairTips(env.token.program, env.pair.quote.program);
  return { refundTip: BigInt(t.refundTip), keeperTip: BigInt(t.keeperTip) };
}

/**
 * The environment the shared leg / expiry helpers (cond-legs.ts, cond-common.ts) run in for a pair: prices are B per whole A, so the KAS tick
 * does not apply (tick 1), and the default keeper / refund tips are the pair's (kob-wasm `pairTips`).
 */
export function legEnvOf(env: PairPlanEnv): PlanEnv {
  const t = pairTipsOf(env);
  return { ...env, token: { ...env.token, tick: 1n, keeperTip: t.keeperTip, refundTip: t.refundTip } };
}

/** The KAS carrier of every covenant output of a pair order (deliveries, custodies, exits): the intent's, else the env's, else `DEFAULT_CARRIER` (2 KAS). */
export const pairCarrierOf = (env: Pick<PlanEnv, 'carrier'>, want?: bigint): bigint => want ?? env.carrier ?? DEFAULT_CARRIER;

/** The market of a pair token by covenant id (A or B), with the maker's UTXO list of that token. */
function tokenOf(env: PairPlanEnv, covId: Hex): { market: TokenMarket; utxos: TokenUtxo[] } | null {
  if (covId === env.token.covenantId) return { market: env.token, utxos: env.tokenUtxos };
  if (covId === env.pair.quote.covenantId) return { market: env.pair.quote, utxos: env.pair.quoteTokenUtxos };
  return null;
}

/** The maker's P2PK-owned plain UTXOs of `market` (same token, same extension commitment for KCC-20). */
export function usableUtxosOf(market: TokenMarket, maker: Hex, utxos: readonly TokenUtxo[]): TokenUtxo[] {
  return utxos.filter(
    (u) =>
      u.covenantId === market.covenantId &&
      isKeyOwned(u.state) &&
      u.state.owner === maker &&
      isPlainState(u.state) &&
      (market.family === 'kron' ? isKronState(u.state) : extensionOfState(u.state) === market.extensionCommitment),
  );
}

/** Base units of `market` the maker holds in usable UTXOs ("close all" of a pair: the A balance). */
export const heldOf = (market: TokenMarket, maker: Hex, utxos: readonly TokenUtxo[]): bigint => usableUtxosOf(market, maker, utxos).reduce((s, u) => s + BigInt(u.state.amount), 0n);

// ------------------------------------------------------------------------------------------------ checks in B

/** A price in B base units per whole A: positive and an i64 (no tick: the KAS tick of A does not apply to a B price). */
export function checkPairPrice(price: bigint, field = 'price'): PlanIssue[] {
  if (price <= 0n) return [issue('PRICE_NOT_POSITIVE', undefined, field)];
  if (!fitsI64(price)) return [issue('PRICE_TOO_LARGE', undefined, field)];
  return [];
}

/** The 2^62 bound of the order's B value at `rate` (kob-protocol `check_quote` with the scale of A). */
export function checkPairNotional(env: PairPlanEnv, amount: bigint, rate: bigint, field = 'price'): PlanIssue[] {
  if (amount <= 0n || rate <= 0n) return [];
  return (amount * rate + env.token.scale - 1n) / env.token.scale >= QUOTE_LIMIT ? [pairIssue('PAIR_NOTIONAL_TOO_LARGE', { ticker: env.pair.quote.ticker }, field)] : [];
}

/** The KAS tip (sompi per whole A): not negative and its total over the amount below 2^62. */
export function checkPairTip(env: PairPlanEnv, amount: bigint, tip: bigint, field = 'tip'): PlanIssue[] {
  if (tip < 0n) return [issue('TIP_NEGATIVE', undefined, field)];
  if (!fitsI64(tip) || (amount * tip + env.token.scale - 1n) / env.token.scale >= QUOTE_LIMIT) return [issue('TIP_TOO_LARGE', undefined, field)];
  return [];
}

// ------------------------------------------------------------------------------------------------ states

/** S (sold, held) and T (bought) of a `KobPair` / `KobCondPair` of `side`: a sell sells A for B, a buy sells B for A. */
function stFields(env: PairPlanEnv, side: 'sell' | 'buy') {
  const [s, t] = side === 'sell' ? [env.token, env.pair.quote] : [env.pair.quote, env.token];
  return {
    sCovId: s.covenantId, sTplHash: s.templateHash, sPre: String(s.prefixLen), sSuf: String(s.suffixLen), sFamily: familyCode(s), sScale: str(s.scale),
    tCovId: t.covenantId, tTplHash: t.templateHash, tPre: String(t.prefixLen), tSuf: String(t.suffixLen), tFamily: familyCode(t), tExt: extOf(t), tScale: str(t.scale),
    sExt: extOf(s),
  };
}

export interface PairStateParams {
  side: 'sell' | 'buy';
  amount: bigint;
  minFill: bigint;
  /** B per whole A (a decay's start) */
  price: bigint;
  /** KAS, sompi per whole A */
  tip: bigint;
  tif: 0 | 1 | 2;
  activeFrom: bigint;
  expiryDaa: bigint;
  refundTip: bigint;
  deliveryCarrier: bigint;
  interval: bigint;
  maxFill: bigint;
  slope: bigint;
  priceEnd: bigint;
  decayStep: bigint;
  /** the exact custody of S (an ask: the amount; a bid: its B escrow; 0 while the escrow is being computed) */
  custody: bigint;
}

/** A new `KobPair` of the maker (side 1 ASK sells A, 2 BID buys A). */
export function makePairState(env: PairPlanEnv, p: PairStateParams): OrderState {
  const st: PairState = {
    maker: env.maker,
    side: p.side === 'sell' ? '1' : '2',
    ...stFields(env, p.side),
    minFill: str(p.minFill),
    price: str(p.price),
    tip: str(p.tip),
    tif: String(p.tif),
    activeFrom: str(p.activeFrom),
    expiryDaa: str(p.expiryDaa),
    refundTip: str(p.refundTip),
    deliveryCarrier: str(p.deliveryCarrier),
    interval: str(p.interval),
    maxFill: str(p.maxFill),
    slope: str(p.slope),
    priceEnd: str(p.priceEnd),
    decayStep: str(p.decayStep),
    amountLeft: str(p.amount),
    custody: str(p.custody),
  };
  return { kind: 'KobPair', state: st };
}

export interface CondPairParams {
  side: 'sell' | 'buy';
  amount: bigint;
  minFill: bigint;
  tip: bigint;
  activeFrom: bigint;
  expiryDaa: bigint;
  refundTip: bigint;
  deliveryCarrier: bigint;
  /** an exit committed by an entry: 0 (the entry writes it); a placed order: its custody */
  custody: bigint;
}

/** A `KobCondPair` (side ASK sells A with a stop below the market; BID buys A with a stop above it) from validated legs; not armed, no repeat fields. */
export function makeCondPairState(env: PairPlanEnv, p: CondPairParams, l: Legs): OrderState {
  const st: CondPairState = {
    maker: env.maker,
    side: p.side === 'sell' ? '1' : '2',
    ...stFields(env, p.side),
    minFill: str(p.minFill),
    tip: str(p.tip),
    activeFrom: str(p.activeFrom),
    expiryDaa: str(p.expiryDaa),
    refundTip: str(p.refundTip),
    deliveryCarrier: str(p.deliveryCarrier),
    tpPrice: str(l.tpPrice),
    slipBps: str(l.slipBps),
    trailStep: str(l.trailStep),
    trailGap: str(l.trailGap),
    trailWait: str(l.trailWait),
    minTouch: str(l.minTouch),
    minRestDaa: str(l.minRestDaa),
    bandDaa: str(l.bandDaa),
    keeperTip: str(l.keeperTip),
    stopPrice: str(l.stopPrice),
    armed: '0',
    amountLeft: str(p.amount),
    custody: str(p.custody),
    parent: ZERO32_HEX,
    rptPrice: '0',
    rptPre: '0',
    rptUntil: '0',
  };
  return { kind: 'KobCondPair', state: st };
}

export interface IfdPairParams {
  /** the entry side: buy = buy-first (side 2 BID), sell = sell-first (side 1 ASK) */
  side: 'sell' | 'buy';
  amount: bigint;
  price: bigint;
  prefund: bigint;
  tip: bigint;
  activeFrom: bigint;
  expiryDaa: bigint;
  refundTip: bigint;
  deliveryCarrier: bigint;
  exitCarrier: bigint;
  minFill: bigint;
  entryStop: bigint;
  bandDaa: bigint;
  minTouch: bigint;
  minRestDaa: bigint;
  keeperTip: bigint;
  custody: bigint;
  rptAmount: bigint;
  exitState: Hex;
}

/** A new `KobIfdPair` entry. */
export function makeIfdPairState(env: PairPlanEnv, p: IfdPairParams): OrderState {
  const a = env.token;
  const b = env.pair.quote;
  const st: IfdPairState = {
    maker: env.maker,
    side: p.side === 'sell' ? '1' : '2',
    aCovId: a.covenantId, aTplHash: a.templateHash, aPre: String(a.prefixLen), aSuf: String(a.suffixLen), aFamily: familyCode(a), aScale: str(a.scale), aExt: extOf(a),
    bCovId: b.covenantId, bTplHash: b.templateHash, bPre: String(b.prefixLen), bSuf: String(b.suffixLen), bFamily: familyCode(b), bScale: str(b.scale), bExt: extOf(b),
    price: str(p.price),
    prefund: str(p.prefund),
    tip: str(p.tip),
    activeFrom: str(p.activeFrom),
    expiryDaa: str(p.expiryDaa),
    refundTip: str(p.refundTip),
    deliveryCarrier: str(p.deliveryCarrier),
    exitCarrier: str(p.exitCarrier),
    minFill: str(p.minFill),
    entryStop: str(p.entryStop),
    bandDaa: str(p.bandDaa),
    minTouch: str(p.minTouch),
    minRestDaa: str(p.minRestDaa),
    keeperTip: str(p.keeperTip),
    armed: '0',
    amountLeft: str(p.amount),
    custody: str(p.custody),
    rptAmount: str(p.rptAmount),
    exitState: p.exitState,
  };
  return { kind: 'KobIfdPair', state: st };
}

/** A copy of a pair state with another `custody` (the escrow is computed from the state itself, then written into it). */
export function withCustody(o: OrderState, custody: bigint): OrderState {
  return { kind: o.kind, state: { ...o.state, custody: str(custody) } } as OrderState;
}

// ------------------------------------------------------------------------------------------------ KRON caps

/**
 * The KRON output cap (1e9 base units) of what the order holds and delivers: every custody of a KRON token (kob-wasm `custodies`), and the T a
 * minimum fill delivers (`tOfMinFill`, in `tMarket`'s base units) when T is KRON. The builders refuse both; this names the token.
 */
export function kronCapIssues(env: PairPlanEnv, order: OrderState, delivery: { market: TokenMarket; amount: bigint } | null): PlanIssue[] {
  const out: PlanIssue[] = [];
  for (const c of env.kob.custodies(order)) {
    const t = tokenOf(env, c.token);
    if (t && t.market.family === 'kron' && c.amount > KRON_MAX_OUTPUT_AMOUNT) {
      out.push(pairIssue('PAIR_KRON_CUSTODY_TOO_LARGE', { ticker: t.market.ticker, amount: c.amount, max: KRON_MAX_OUTPUT_AMOUNT }, t.market === env.token ? 'amount' : 'price'));
    }
  }
  if (delivery && delivery.market.family === 'kron' && delivery.amount > KRON_MAX_OUTPUT_AMOUNT) {
    out.push(pairIssue('PAIR_KRON_DELIVERY_TOO_LARGE', { ticker: delivery.market.ticker, max: KRON_MAX_OUTPUT_AMOUNT }, 'minFill'));
  }
  return out;
}

// ------------------------------------------------------------------------------------------------ building

export interface PairCustodyDraw {
  token: Hex;
  market: TokenMarket;
  /** base units the custody holds (kob-wasm `custodies`) */
  amount: bigint;
  /** base units of that token returned as change */
  change: bigint;
}

export interface PairCreateResult {
  request: CreateOrderRequest | null;
  built: BuiltTx | null;
  /** KAS on each custody and token change output */
  tokenCarrier: bigint;
  custodies: PairCustodyDraw[];
  issues: PlanIssue[];
}

const INSUFFICIENT_RE = /insufficient funds: need (\d+) sompi, have (\d+) sompi/;
const spendable = (u: KeyUtxo[]): bigint => u.filter((x) => !x.covenantId).reduce((s, x) => s + BigInt(x.amount), 0n);

/**
 * Selects the custodies' tokens (each from the maker's UTXOs of its OWN token) and the funding, builds the `createOrder` transaction with kob-wasm
 * (adaptive fee reserve, the fee policy's total cap, a retry at the floor when the picked rate is unaffordable) and self-checks it. Never throws for
 * user problems: a shortfall or a protocol refusal comes back as an error issue with `built: null`.
 */
export function buildPairCreate(env: PairPlanEnv, spec: { order: OrderState; value: bigint; deadline: bigint | null }): PairCreateResult {
  const res = buildPairCreateAt(env, spec);
  if (res.built) return res;
  const low = res.issues.some((i) => i.code === 'INSUFFICIENT_KAS') ? atFloor(env) : null;
  return low ? buildPairCreateAt(low, spec) : res;
}

function buildPairCreateAt(env: PairPlanEnv, spec: { order: OrderState; value: bigint; deadline: bigint | null }): PairCreateResult {
  const carrier = pairCarrierOf(env);
  const draws: PairCustodyDraw[] = [];
  const fail = (...extra: PlanIssue[]): PairCreateResult => ({ request: null, built: null, tokenCarrier: carrier, custodies: draws, issues: extra });
  const tokens: TokenUtxo[] = [];
  let tokenInputsKas = 0n;
  let outputsKas = 0n;
  for (const c of env.kob.custodies(spec.order)) {
    const t = tokenOf(env, c.token);
    if (!t) return fail(issue('PLAN_MISMATCH', { reason: `custody of an unknown token ${c.token}` }));
    // the custody and the token change are token outputs of their program: the carrier must clear its floor
    const floor = tokenCarrierFloor(env.kob, t.market.templateHash);
    if (carrier < floor) return fail(issue('CARRIER_BELOW_FLOOR', { carrier, floor }, 'carrier'));
    let picked: TokenUtxo[];
    try {
      picked = selectTokens(usableUtxosOf(t.market, env.maker, t.utxos), c.amount, Math.min(t.market.slots.inputs, MAX_TOKEN_INPUTS));
    } catch (e) {
      if (e instanceof InsufficientFunds) {
        return fail(
          e.kind === 'fragmented'
            ? pairIssue('PAIR_TOKEN_UTXOS_FRAGMENTED', { ticker: t.market.ticker, max: e.maxInputs ?? 0 })
            : pairIssue('PAIR_INSUFFICIENT_TOKENS', { ticker: t.market.ticker, needed: e.needed, have: e.have, shortfall: e.shortfall }, t.market === env.token ? 'amount' : 'price'),
        );
      }
      throw e;
    }
    const have = picked.reduce((s, u) => s + BigInt(u.state.amount), 0n);
    draws.push({ token: c.token, market: t.market, amount: c.amount, change: have - c.amount });
    tokens.push(...picked);
    tokenInputsKas += picked.reduce((s, u) => s + BigInt(u.amount), 0n);
    outputsKas += carrier + (have > c.amount ? carrier : 0n);
  }
  // outputs: the order, each custody (+ each token change); the token inputs' own KAS is recycled into them
  let need = spec.value + outputsKas;
  need = need > tokenInputsKas ? need - tokenInputsKas : 0n;
  const balance = spendable(env.funding);
  if (balance < need) return fail(issue('INSUFFICIENT_KAS', { needed: need, have: balance, shortfall: need - balance }));

  const mk = (funding: KeyUtxo[]): CreateOrderRequest => {
    const r: CreateOrderRequest = { action: 'createOrder', order: spec.order, value: str(spec.value), funding };
    if (tokens.length) {
      r.tokens = tokens;
      r.tokenCarrier = str(carrier);
    }
    if (env.changeTo) r.change = env.changeTo;
    if (spec.deadline != null) r.deadline = str(spec.deadline);
    if (env.feeRate !== undefined || env.feeMode) r.fee = { ...(env.feeRate !== undefined ? { feeRate: str(env.feeRate) } : {}), ...(env.feeMode ? { feeMode: env.feeMode } : {}) };
    return r;
  };

  let reserve = DEFAULT_FEE_RESERVE;
  for (let attempt = 0; attempt < 5; attempt++) {
    const room = balance - need;
    const r = reserve < room ? reserve : room;
    let funding: KeyUtxo[];
    try {
      funding = selectFunding(env.funding, need, r);
    } catch (e) {
      if (e instanceof InsufficientFunds) {
        return fail(e.kind === 'fragmented' ? issue('FUNDING_FRAGMENTED', { max: e.maxInputs ?? 0 }) : issue('INSUFFICIENT_KAS', { needed: e.needed, have: e.have, shortfall: e.shortfall }));
      }
      throw e;
    }
    try {
      const { built, request, rate } = buildWithCap(env.fees?.policy, mk(funding), (q) => env.kob.build(guardRequest(q, { maker: env.maker, changeTo: env.changeTo })));
      const mismatch = verifyPairBuilt(env, built, spec, draws);
      if (mismatch) return fail(issue('PLAN_MISMATCH', { reason: mismatch }));
      recordFee(env, built, rate);
      return { request, built, tokenCarrier: carrier, custodies: draws, issues: [] };
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
 * Independent check that the built transaction is the planned order: the placement record recovered by kob-wasm `recoverOrders` (which trusts
 * nothing) yields the planned state, value and deadline, and every custody the state holds is an output of the transaction holding exactly its
 * amount, owned by the new covenant id, of its own token (the first custody, and a sell-first entry's B prefund). Returns a reason on mismatch.
 */
export function verifyPairBuilt(env: PlanEnv, built: BuiltTx, spec: { order: OrderState; value: bigint; deadline: bigint | null }, draws: readonly PairCustodyDraw[]): string | null {
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
  const parts = [r.custody, r.prefund ?? null];
  for (let i = 0; i < 2; i++) {
    const want = draws[i];
    const got = parts[i];
    if (!want) {
      if (got) return 'an unexpected custody was created';
      continue;
    }
    if (!got || BigInt(got.state.amount) !== want.amount) return i === 0 ? 'custody amount differs' : 'prefund custody amount differs';
    const out = built.tx.outputs[got.output];
    if (!out || out.covenant?.covenantId !== want.token) return i === 0 ? 'custody is not an output of its token' : 'prefund custody is not an output of token B';
    if (isKeyOwned(got.state) || got.state.owner !== r.covenantId) return 'custody is not owned by the new order';
  }
  return null;
}

// ------------------------------------------------------------------------------------------------ placement and disclosure

/** Everything a pair order type decides; `placePair` checks, builds and discloses it. */
export interface PairPlaceSpec {
  order: OrderState;
  /** the committed exit of an if-done entry (listed last in `states`) */
  exit?: OrderState | null;
  /** KAS on the order UTXO, itemised (its value is their sum) */
  lines: CarrierLine[];
  deadline: bigint | null;
  side: 'sell' | 'buy';
  amount: bigint;
  expiry: Disclosure['expiry'];
  activatesAt: bigint | null;
  /** B per whole A */
  limitPrice: bigint | null;
  expectedPrice: bigint | null;
  worstPrice: bigint | null;
  /** KAS per whole A */
  tip: bigint;
  minTouch: bigint | null;
  keeperTip: bigint;
  /** the B of the whole amount at the limit by the covenant's rule (a sell receives at least, a buy pays at most) */
  allInTotal: bigint | null;
  receiveMinB: bigint | null;
  payMaxB: bigint | null;
  expectedB: bigint | null;
  minFillB: bigint | null;
  tipKasTotal: bigint;
  deliveries: bigint;
  deliveryCarrier: bigint;
  exitCarrier: bigint | null;
  /** the T a minimum fill delivers at the order's highest price, for the KRON output cap (null: checked by kob-wasm only) */
  delivery: { market: TokenMarket; amount: bigint } | null;
  exitFacts: PairDisclosure['exit'];
  repeat: PairDisclosure['repeat'];
  /** generic disclosure tags (the KAS planners' tags where the meaning is the same) */
  notes: string[];
  /** pair disclosure tags */
  pairNotes: string[];
  cond: CondSummary | null;
  /** KAS on each custody / token change output: the intent's carrier (default: the env's, else 2 KAS) */
  carrier?: bigint;
}

const linesTotal = (lines: readonly CarrierLine[]): bigint => lines.reduce((s, l) => s + l.amount * BigInt(l.count), 0n);

/** A plan that failed before anything was built. */
export const failedPairPlan = (issues: PlanIssue[], states: OrderState[] = [], cond: CondSummary | null = null): PairOrderPlan & { cond: CondSummary | null } => ({
  ok: false, issues, states, request: null, built: null, disclosure: null, pair: null, cond,
});

/**
 * The common tail of every pair planner: the KRON caps, kob-wasm `checkNewOrder` (its message as BUILD_REJECTED) and `minOrderValue`, the build
 * (both custodies drawn from their own token's UTXOs), and the two disclosures. `issues` are the findings so far (warnings / infos).
 */
export function placePair(env: PairPlanEnv, pre: PlanIssue[], spec: PairPlaceSpec): PairOrderPlan & { cond: CondSummary | null } {
  const issues = [...pre];
  const states = spec.exit ? [spec.order, spec.exit] : [spec.order];
  const value = linesTotal(spec.lines);
  issues.push(...kronCapIssues(env, spec.order, spec.delivery));
  if (hasError(issues)) return failedPairPlan(issues, states, spec.cond);
  try {
    env.kob.checkNewOrder(spec.order);
  } catch (e) {
    issues.push(issue('BUILD_REJECTED', { reason: (e instanceof Error ? e.message : String(e)).replace(/^kob-wasm checkNewOrder: (invalid request: )?/, '') }));
    return failedPairPlan(issues, states, spec.cond);
  }
  const minValue = env.kob.minOrderValue(spec.order);
  if (value < minValue || !fitsI64(value)) {
    issues.push(issue('BUILD_REJECTED', { reason: `the order UTXO needs at least ${minValue} sompi (planned ${value})` }));
    return failedPairPlan(issues, states, spec.cond);
  }
  const res = buildPairCreate(spec.carrier === undefined ? env : { ...env, carrier: spec.carrier }, { order: spec.order, value, deadline: spec.deadline });
  issues.push(...res.issues);
  if (res.built === null || res.request === null || hasError(issues)) {
    return { ok: false, issues, states, request: null, built: null, disclosure: null, pair: null, cond: spec.cond };
  }
  const carriers: CarrierLine[] = [...spec.lines];
  for (const c of res.custodies) {
    carriers.push({ kind: c.market === env.token ? 'tokenCarrier' : 'quoteTokenCarrier', amount: res.tokenCarrier, count: 1 });
    if (c.change > 0n) carriers.push({ kind: c.market === env.token ? 'tokenChangeCarrier' : 'quoteTokenChangeCarrier', amount: res.tokenCarrier, count: 1, kept: true });
  }
  const kasLocked = linesTotal(carriers.filter((c) => !c.kept));
  const st = spec.order.state as unknown as Record<string, string>;
  const escrowA = res.custodies.filter((c) => c.market === env.token).reduce((s, c) => s + c.amount, 0n);
  const escrowB = res.custodies.filter((c) => c.market !== env.token).reduce((s, c) => s + c.amount, 0n);
  const disclosure: Disclosure = {
    side: spec.side,
    tokenAmount: spec.amount,
    scale: env.token.scale,
    minFill: BigInt(st.minFill),
    minTouch: spec.minTouch,
    limitPrice: spec.limitPrice,
    // the tip is KAS, never part of a B price: the all-in price is the limit itself
    allInPrice: spec.limitPrice,
    allInTotal: spec.allInTotal,
    expectedPrice: spec.expectedPrice,
    worstPrice: spec.worstPrice,
    tip: spec.tip,
    carriers,
    kasLocked,
    tokensEscrowed: escrowA,
    fee: BigInt(res.built.fee.fee),
    refundTip: BigInt(st.refundTip),
    keeperTip: spec.keeperTip,
    expiry: { ...spec.expiry, deadlineUnixSeconds: res.request.deadline != null ? BigInt(res.request.deadline) : spec.expiry.deadlineUnixSeconds },
    activatesAt: spec.activatesAt != null && spec.activatesAt > env.clock.daa ? { daa: spec.activatesAt, approxUnixSeconds: daaToUnix(env.clock, spec.activatesAt) } : null,
    notes: [...new Set([...spec.notes, 'carrierReturned'])],
  };
  const pair: PairDisclosure = {
    kind: spec.order.kind as PairDisclosure['kind'],
    side: spec.side,
    base: sideFacts(env.token),
    quote: sideFacts(env.pair.quote),
    amount: spec.amount,
    escrowA,
    escrowB,
    receiveMinB: spec.receiveMinB,
    payMaxB: spec.payMaxB,
    expectedB: spec.expectedB,
    minFillB: spec.minFillB,
    tipKasTotal: spec.tipKasTotal,
    deliveries: spec.deliveries,
    deliveryCarrier: spec.deliveryCarrier,
    exitCarrier: spec.exitCarrier,
    orderValue: value,
    trigger: hasStopState(spec.order) ? env.kob.pairTriggerRule(spec.order) : null,
    exit: spec.exitFacts,
    repeat: spec.repeat,
    notes: [...new Set(['pairPricesFromKasBooks', 'pairTipKas', 'pairRoute', 'pairNetting', 'pairInventory', ...spec.pairNotes])],
  };
  return { ok: true, issues, states, request: res.request, built: res.built, disclosure, pair, cond: spec.cond };
}

/** A conditional with a stop leg, or a stop entry: kob-wasm `pairTriggerRule` describes it. */
function hasStopState(o: OrderState): boolean {
  const s = o.state as unknown as Record<string, string>;
  if (o.kind === 'KobCondPair') return BigInt(s.stopPrice) > 0n;
  if (o.kind === 'KobIfdPair') return BigInt(s.entryStop) > 0n;
  return false;
}

/** KAS lines of a `KobPair` / `KobCondPair` order UTXO: `fills` delivery carriers and the tip prefund (kob-wasm `pairKasValue` = their sum). */
export function pairKasLines(env: PairPlanEnv, order: OrderState, fills: bigint, deliveryCarrier: bigint): { lines: CarrierLine[]; tipKasTotal: bigint } | null {
  const total = env.kob.pairKasValue(order, fills);
  if (total === null) return null;
  const tipKasTotal = total - deliveryCarrier * (fills > 0n ? fills : 1n);
  const lines: CarrierLine[] = [{ kind: 'deliveryCarrier', amount: deliveryCarrier, count: Number(fills > 0n ? fills : 1n) }];
  if (tipKasTotal > 0n) lines.push({ kind: 'tipPrefund', amount: tipKasTotal, count: 1 });
  return { lines, tipKasTotal };
}
