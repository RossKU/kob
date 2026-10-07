// Cancel, cancel-replace, cancel-all (position), sweep and refund builders, plus the OrderSnapshot they consume (matcher.md 10.9, 5, 1.2).
//
//   * every builder returns a kob-wasm request AND the built (unsigned) tx: protocol-level rejections surface before the user signs;
//   * only the maker's spends move strays (tokens sent to an order from outside): the maker's SWEEP in place (`planSweep`: the order continues
//     unchanged, only its strays return) and the cancel, which sweeps the custody and every stray of the order in the same tx; each token returns to
//     the maker in ONE token output. FOREIGN strays (other tokens owned by the order id, with the program the indexer proved them under) ride
//     along, one output per token; foreign strays without a proven state are reported, never built;
//   * pair orders (`KobPair`, `KobCondPair`, `KobIfdPair`) own TWO tokens, A and B: their custodies follow kob-protocol `custodies` (a sell-first
//     entry holds A and a B prefund: `custody` + `prefund`), their strays may be of either token, and every builder treats each of A and B as one
//     transfer (its own program, slots and extension commitment); a position cancel (an if-done pair entry with its exits) covers one pair;
//   * fee: kob-wasm pays the fee out of the released carriers when they suffice (no P2PK funding input, so nothing extra to sign);
//     when they do not (a tiny bid), maker funding UTXOs are added automatically; a change too small to be worth its storage mass rides on the
//     new order instead of coming back as a dust output (kob-protocol's change rule of the amend builders);
//   * an amendment of a plain ask that keeps its scale and amount, or of a plain bid that keeps its scale, is IN PLACE (`amendsInPlace`): the maker's
//     cancel continues the covenant id, an ask's custody never moves (the order's carrier pays the fee) and a bid's escrow pays the fee or is topped up;
//   * TypeScript never re-implements a protocol rule: the builder (crates/kob-protocol/src/build.rs) is the reference and its rejections
//     are reported as `PlanIssue`s.
import type { OrderView, TokenUtxoView } from '../data/indexer-types';
import type { ExpectedSigning } from './decode';
import {
  asOrderState, baseKind, big, custodiesOf, extensionOf, familyOfKind, isKind, pairExtFor, pairFactsOf, quoteCovIdOf, tokenCovIdOf,
  type CustodyFact,
} from './order-facts';
import { pairOfView } from './pair-view';
import type { PlanIssue } from './plan-types';
import type { ResolvedRecord } from './records';
import type {
  ActionRequest, AmendOrderRequest, BuiltTx, FeeMode, CancelItem, CancelOrderRequest, CancelPositionRequest, Family, ForeignStrays, Hex, KeyUtxo, OrderState, OrderUtxo,
  Replacement, SweepOrderRequest, TokenProgram, TokenState, TokenUtxo, U64, Utxo,
} from './types';
import { custodyState, extensionMatches, extensionOfState, familyOfProgram, familyOfState, isCovenantOwned, isKeyOwned } from './token-state';
import { atFloor, buildWithCap, recordFee, withUrgency, type FeeContext, type RatePick } from './fee-policy';
import { guardRequest } from './request-guard';
import { KobError, type KobWasm } from './wasm';

const ONE_KAS = 100_000_000n;
/** Headroom added when picking funding UTXOs for the (still unknown) fee of the enlarged tx; the surplus returns as change. */
const FUNDING_HEADROOM = 500_000n;
/**
 * Most KAS a builder may fold into the maker's new order instead of a change output: kob-protocol takes a change only while its storage mass
 * (10^12 / value) exceeds the transaction's fee mass, i.e. below 10^12 / feeMass sompi, and every KOB transaction has a fee mass above 2,000 grams.
 */
const TINY_CHANGE_MAX = 5n * ONE_KAS;

// ------------------------------------------------------------------------------------------------ snapshot

/**
 * Everything needed to spend one live order: its current covenant UTXO with decoded state, its custody token UTXO(s) (ask kinds and pair orders
 * holding tokens) and the strays owned by its covenant id. Built from the indexer's `OrderView` or from a `PlacementRecord` resolved on a node.
 */
export interface OrderSnapshot {
  covenantId: Hex;
  order: OrderUtxo<OrderState>;
  /** the custody (a pair order: the FIRST of kob-protocol `custodies`, of A or of B) */
  custody: TokenUtxo | null;
  /** a sell-first `KobIfdPair`'s B prefund custody (the second of `custodies`) */
  prefund?: TokenUtxo | null;
  /** strays of the order's own token(s) (a pair order: of A and of B) */
  strays: TokenUtxo[];
  /** DAA from which anyone may refund (indexer `refund_due_daa`); informational, kob-wasm re-checks at build time */
  refundDueDaa: bigint | null;
  /** day-order deadline (UTC unix seconds) from the placement record */
  deadline: bigint | null;
  source: 'indexer' | 'record' | 'chain';
  /** the order's strays could not be (fully) read: a cancel may leave strays behind, the plan warns (C5 W-13) */
  straysUnknown?: boolean;
  /**
   * FOREIGN strays (matcher.md 1.2): tokens of other covenant ids owned by the order id, one group per token with the program the indexer proved
   * their state under. The maker's cancel and sweep return them to the maker (one output per token); after the order ends nothing can move them.
   */
  foreign?: ForeignStrays[];
  /** foreign strays without a proven state or program: nothing can build their spend, they stay with the order id (reported) */
  foreignUnproven?: UnprovenStray[];
}

export class SnapshotError extends Error {
  readonly code: 'state-unknown' | 'no-current-utxo' | 'no-extension-commitment' | 'bad-state' | 'not-live' | 'node-mismatch' | 'old-template';
  constructor(code: SnapshotError['code'], message: string) {
    super(message);
    this.name = 'SnapshotError';
    this.code = code;
  }
}

function tokenUtxoFromView(v: TokenUtxoView, owner: Hex, ext: Hex | null, family: Family): TokenUtxo {
  const state: TokenState | undefined = v.state ?? (family === 'kron' || ext ? custodyState(family, v.amount, owner, ext) : undefined);
  if (!state) throw new SnapshotError('no-extension-commitment', `token UTXO ${v.txid}:${v.index}: the extension commitment is unknown, cannot rebuild its state`);
  return { transactionId: v.txid, index: v.index, amount: v.value, blockDaaScore: String(v.created_daa), covenantId: v.token, state };
}

/** One token an order's own transfers move: its program (template hash), family and the extension commitment the state names for it (if any). */
export interface OwnToken { covenantId: Hex; tplHash: Hex; family: Family; ext: Hex | null; role: 'base' | 'quote' }

/**
 * The tokens an order's own transfers move: a KAS-quoted order's token; a pair order's A and B (each with the program, family and the extension
 * commitment its state names).
 */
export function ownTokenFacts(st: OrderState): OwnToken[] {
  const pf = pairFactsOf(st);
  if (pf) {
    const one = (t: typeof pf.a, role: 'base' | 'quote'): OwnToken => ({ covenantId: t.covId, tplHash: t.tplHash, family: t.family ?? 'kcc20', ext: t.family === 'kron' ? null : t.ext, role });
    return pf.a.covId === pf.b.covId ? [one(pf.a, 'base')] : [one(pf.a, 'base'), one(pf.b, 'quote')];
  }
  const s = st.state as unknown as Record<string, string | undefined>;
  return [{ covenantId: s.tokenCovId ?? '', tplHash: s.tokenTplHash ?? '', family: familyOfKind(st.kind), ext: extensionOf(st), role: 'base' }];
}

/** The covenant ids of the tokens an order's own transfers move (a pair order: A and B). */
export const ownTokensOf = (st: OrderState): Hex[] => ownTokenFacts(st).map((t) => t.covenantId);

/** The second own token of an order (a pair order's B), null for a KAS-quoted order. */
export function secondTokenOf(st: OrderState): { covenantId: Hex; tplHash: Hex; family: Family; ext: Hex | null } | null {
  const t = ownTokenFacts(st).find((x) => x.role === 'quote');
  return t ? { covenantId: t.covenantId, tplHash: t.tplHash, family: t.family, ext: t.ext } : null;
}

/**
 * OrderView (indexer) -> snapshot. Needs the decoded `state` of the CURRENT utxo, `current`, and the custody UTXO(s) the state holds; the token
 * states of custodies and strays are taken from the view (`state`) or rebuilt from owner + amount + extension commitment. A pair order's custodies
 * come from the view's `pair.custodies` (record order: a sell-first entry's A, then its B prefund), its strays may be of A or of B.
 */
export function snapshotFromOrderView(view: OrderView): OrderSnapshot {
  const st = asOrderState(view.state);
  if (!view.state_known || !st) throw new SnapshotError('state-unknown', `order ${view.covenant_id}: the indexer has not proven its current state`);
  if (!view.current || view.current.value == null) throw new SnapshotError('no-current-utxo', `order ${view.covenant_id}: no current outpoint`);
  const toks = ownTokenFacts(st);
  const famOf = (token: Hex): Family => toks.find((t) => t.covenantId === token)?.family ?? familyOfKind(st.kind);
  const cs: CustodyFact[] = custodiesOf(st);
  const pv = pairOfView(view);
  // the custody UTXO views in record order: a pair view lists them all; otherwise the single `custody`
  const cuViews: (TokenUtxoView | null)[] = pv
    ? cs.map((c, k) => {
        const x = pv.custodies[k];
        return (x && x.token === c.token ? (x.utxo ?? null) : null) ?? (k === 0 ? (view.custody?.utxo ?? null) : null);
      })
    : [view.custody?.utxo ?? null];
  const extOf = (token: Hex, k: number | null): Hex | null => {
    const u = k !== null ? cuViews[k] : null;
    const fromUtxo = u?.state ? extensionOfState(u.state) : null;
    const named = pairExtFor(st, token);
    // the view's `extension_commitment` is the first custody's (a KAS order: its token's)
    const first = k === 0 || (k === null && token === (cs[0]?.token ?? tokenCovIdOf(st))) ? (view.extension_commitment ?? null) : null;
    return fromUtxo ?? named ?? first ?? (token === tokenCovIdOf(st) ? extensionOf(st) : null) ?? null;
  };
  const live = (k: number): TokenUtxo | null => {
    const u = cuViews[k];
    const token = cs[k]?.token ?? tokenCovIdOf(st);
    return u && !u.spent ? tokenUtxoFromView(u, view.covenant_id, extOf(token, k), famOf(token)) : null;
  };
  const custody = live(0);
  const prefund = cs.length > 1 ? live(1) : null;
  // strays of any OTHER token are foreign: kept apart, with the program their state was proven under
  const split = splitStrayViews((view.strays ?? []).filter((s) => !s.spent), st);
  const custodyExt = (token: Hex): Hex | null => {
    const k = cs.findIndex((c) => c.token === token);
    const u = k === 0 ? custody : k === 1 ? prefund : null;
    return u ? extensionOfState(u.state) : extOf(token, k >= 0 ? k : null);
  };
  const strays = split.own.map((s) => tokenUtxoFromView(s, view.covenant_id, custodyExt(s.token), famOf(s.token)));
  return {
    ...(split.foreign.length ? { foreign: split.foreign } : {}),
    ...(split.unproven.length ? { foreignUnproven: split.unproven } : {}),
    covenantId: view.covenant_id,
    order: {
      transactionId: view.current.txid, index: view.current.index, amount: view.current.value, blockDaaScore: String(view.last_daa),
      covenantId: view.covenant_id, state: st,
    },
    custody,
    ...(prefund ? { prefund } : {}),
    strays,
    refundDueDaa: view.refund_due_daa != null ? BigInt(view.refund_due_daa) : null,
    deadline: view.deadline != null ? BigInt(view.deadline) : null,
    source: 'indexer',
  };
}

/** A foreign stray nothing can move (no proven state / program): reported, never built. */
export interface UnprovenStray { outpoint: string; token: Hex; amount: bigint }

/**
 * Splits an order's stray views into its OWN tokens' (a pair order: A and B) and FOREIGN ones (the indexer's `foreign` flag, or any other token).
 * A foreign stray joins a group of its token only with a proven state, a token program name and a covenant-owned state of the program's family
 * owned by the order id; anything else is `unproven` (nothing could build its spend).
 */
export function splitStrayViews(views: readonly TokenUtxoView[], st: OrderState): { own: TokenUtxoView[]; foreign: ForeignStrays[]; unproven: UnprovenStray[] } {
  const ownTokens = ownTokensOf(st);
  const own: TokenUtxoView[] = [];
  const foreign: ForeignStrays[] = [];
  const unproven: UnprovenStray[] = [];
  for (const v of views) {
    // the order's own tokens are never foreign, whatever a flag says: the builders sweep them with the order's own transfers
    if (ownTokens.includes(v.token)) {
      own.push(v);
      continue;
    }
    const program = v.program as TokenProgram | undefined;
    const st0 = v.state;
    const usable = !!program && !!st0 && familyOfState(st0) === familyOfProgram(program) && isCovenantOwned(st0) && st0.owner === v.owner;
    if (!usable) {
      unproven.push({ outpoint: `${v.txid}:${v.index}`, token: v.token, amount: big(st0?.amount ?? v.amount) });
      continue;
    }
    const utxo: TokenUtxo = { transactionId: v.txid, index: v.index, amount: v.value, blockDaaScore: String(v.created_daa), covenantId: v.token, state: st0! };
    const g = foreign.find((x) => x.token.covenantId === v.token);
    if (g && g.token.program !== program) unproven.push({ outpoint: `${v.txid}:${v.index}`, token: v.token, amount: big(st0!.amount) });
    else if (g) g.utxos.push(utxo);
    else foreign.push({ token: { covenantId: v.token, program: program! }, utxos: [utxo] });
  }
  return { own, foreign, unproven };
}

/** Resolved placement record (records.ts `resolveRecord`, status 'live') -> snapshot; `strays` come from the indexer when known. */
export function snapshotFromRecord(resolved: ResolvedRecord, strays: TokenUtxo[] = [], deadline: bigint | null = null): OrderSnapshot {
  if (resolved.status !== 'live' || !resolved.order) throw new SnapshotError('not-live', `order ${resolved.covenantId} is ${resolved.status}, nothing to spend`);
  return {
    covenantId: resolved.covenantId, order: resolved.order, custody: resolved.custody, ...(resolved.prefund ? { prefund: resolved.prefund } : {}), strays, refundDueDaa: null, deadline, source: 'record',
  };
}

/** Snapshot straight from a kob-wasm `CancelItem` (order UTXO + custody + strays), e.g. for tests and CLI-provided data. */
export function snapshotFromItem(item: CancelItem): OrderSnapshot {
  const covenantId = item.order.covenantId;
  if (!covenantId) throw new SnapshotError('bad-state', 'order UTXO has no covenant id');
  return {
    covenantId, order: item.order, custody: item.custody ?? null, ...(item.prefund ? { prefund: item.prefund } : {}), strays: item.strays ?? [], refundDueDaa: null, deadline: null,
    source: 'chain',
  };
}

// ------------------------------------------------------------------------------------------------ plan types

export interface CancelEnv {
  kob: KobWasm;
  /** the wallet's x-only key: maker of every cancelled order, owner of funding and top-up tokens */
  maker: Hex;
  /** where change returns (default: maker) */
  changeTo?: Hex;
  /** P2PK KAS UTXOs of the maker, used only when the released carriers cannot pay the fee */
  funding?: KeyUtxo[];
  /** P2PK-owned token UTXOs of the maker (of any token: each top-up takes its own token's): automatic top-up source of a cancel-replace */
  tokenUtxos?: TokenUtxo[];
  /** current DAA + UTC clock (refunds need the DAA as lockTime) */
  clock?: { daa: bigint; unixSeconds?: bigint };
  /** sompi per mass unit (default 100 = relay minimum). An explicit rate: the fee policy (`fees`) is then not asked. */
  feeRate?: bigint;
  /** `priority` = storage-inclusive fee (default `relay`: exactly the node's relay floor) */
  feeMode?: FeeMode;
  /** dynamic fee policy + the node's estimate (kob/fee-policy.ts): every cancel / amend / refund pays the NORMAL bucket; absent = the floor */
  fees?: FeeContext;
  /** set by the planner: the pick behind `feeRate` (disclosure) */
  feePick?: RatePick;
}

export interface CancelOptions {
  /** when strays do not fit the token program's input slots: cancel anyway and abandon (lose) the smallest strays instead of refusing */
  allowAbandonStrays?: boolean;
  /** cancelAll: at most this many orders per transaction (default 8) */
  maxOrdersPerTx?: number;
  /** planCancelReplace: always cancel + re-place, even where an in-place amend would do */
  noInPlace?: boolean;
}

/** `refundOrder` request of kob-wasm (not in the shared `ActionRequest` union). */
export interface RefundOrderRequest {
  action: 'refundOrder';
  order: OrderUtxo<OrderState>;
  custody?: TokenUtxo | null;
  /** a sell-first `KobIfdPair`'s B prefund custody */
  prefund?: TokenUtxo | null;
  lockTime: U64;
  funding?: KeyUtxo[];
  change?: Hex | null;
  fee?: { feeRate?: U64 | null };
  ownKeys?: Hex[];
}

export type CancelRequest = CancelOrderRequest | AmendOrderRequest | CancelPositionRequest | RefundOrderRequest | SweepOrderRequest;

export interface CancelPlan {
  ok: boolean;
  issues: PlanIssue[];
  request: CancelRequest | null;
  built: BuiltTx | null;
  /** covenant ids spent by maker cancels / refunds of this tx */
  cancelIds: Hex[];
  /** feed for decodeSigning({expected}) */
  expected: ExpectedSigning;
  /** tokens of the order's token (a pair order: A) returning to the maker as ONE P2PK token output (custody + strays + unused top-up), base units */
  tokensReturned: bigint;
  /** a pair order: its quote token B returning to the maker (custody / prefund + strays + unused top-up), base units of B */
  quoteReturned?: bigint;
  /** KAS released from covenants (order + custody + stray inputs), sompi */
  kasReleased: bigint;
  /** maker funding UTXOs that had to be added to pay the fee */
  fundingUsed: KeyUtxo[];
  /** foreign strays returned to the maker (one token output each): token, base units, UTXOs */
  foreignReturned?: { covenantId: Hex; amount: bigint; utxos: number }[];
  /** a sweep in place (`planSweep`): what returns and what stays for a later sweep */
  sweep?: SweepSummary;
}

/** What a sweep moves (per token) and leaves; the confirmation screen states the same from the transaction itself. */
export interface SweepSummary {
  covenantId: Hex;
  /** stray UTXOs moved */
  utxos: number;
  /** KAS carried by the swept strays (returns with them) */
  kas: bigint;
  tokens: { covenantId: Hex; amount: bigint; utxos: number; foreign: boolean }[];
  /** strays left with the order for a later sweep (another extension commitment, over the program's inputs) */
  later: number;
  /** foreign strays without a proven state: nothing can move them */
  unproven: number;
}

const issue = (code: string, severity: PlanIssue['severity'], message: string, params?: PlanIssue['params']): PlanIssue => ({ code, severity, message, ...(params ? { params } : {}) });

const failed = (issues: PlanIssue[], cancelIds: Hex[] = []): CancelPlan => ({
  ok: false, issues, request: null, built: null, cancelIds, expected: { cancelIds }, tokensReturned: 0n, kasReleased: 0n, fundingUsed: [],
});

const feeOpts = (env: CancelEnv) => ({ feeRate: env.feeRate != null ? env.feeRate.toString() : null, ...(env.feeMode ? { feeMode: env.feeMode } : {}) });

/**
 * A change key other than the maker is an explicit user choice: the plan announces that payment (recipient key, amount read from the built change
 * output) so the pre-sign decoder does not flag it, while any OTHER redirection of funds stays blocking.
 */
function withChange(env: CancelEnv, built: BuiltTx, expected: ExpectedSigning): ExpectedSigning {
  const ci = built.fee.changeOutput;
  if (!env.changeTo || env.changeTo === env.maker || ci == null) return expected;
  return { ...expected, payments: [...(expected.payments ?? []), { pubkey: env.changeTo, amount: big(built.tx.outputs[ci].value) }] };
}

const kobBuild = (env: CancelEnv, r: CancelRequest): BuiltTx => {
  const g = guardRequest(r, { maker: env.maker, changeTo: env.changeTo });
  return env.kob.build(g as unknown as ActionRequest);
};

/** Slot limits of the token program that an order's token template hash names (from the pinned templates of the wasm build). */
export function tokenSlotsFor(kob: KobWasm, tokenTplHash: Hex): { inputs: number; outputs: number } | null {
  const t = kob.templates().find((x) => x.hash === tokenTplHash && x.tokenSlots);
  return t?.tokenSlots ? { inputs: t.tokenSlots[0], outputs: t.tokenSlots[1] } : null;
}

/** Token input slots of a token program by NAME (foreign strays carry their program's name), null for anything that is not a pinned token program. */
export function programSlots(kob: KobWasm, program: string): number | null {
  const t = kob.templates().find((x) => x.name === program && x.tokenSlots);
  return t?.tokenSlots ? t.tokenSlots[0] : null;
}

/**
 * One token's strays as one transfer can take them: those sharing `ext` (default: the first one's), the largest first, at most `room`. The rest
 * is returned apart (another extension commitment / over the program's inputs).
 */
function pickStrays(utxos: readonly TokenUtxo[], ext: Hex | null | undefined, room: number): { take: TokenUtxo[]; otherExt: TokenUtxo[]; over: TokenUtxo[] } {
  if (!utxos.length) return { take: [], otherExt: [], over: [] };
  const e = ext !== undefined && ext !== null && utxos.some((u) => extensionMatches(u.state, ext)) ? ext : extensionOfState(utxos[0]!.state);
  const same = utxos.filter((u) => extensionMatches(u.state, e));
  const otherExt = utxos.filter((u) => !extensionMatches(u.state, e));
  const sorted = [...same].sort((a, b) => (big(b.state.amount) > big(a.state.amount) ? 1 : big(b.state.amount) < big(a.state.amount) ? -1 : 0));
  const n = Math.max(room, 0);
  return { take: sorted.slice(0, n), otherExt, over: sorted.slice(n) };
}

const outpointOf = (u: { transactionId: Hex; index: number }): string => `${u.transactionId}:${u.index}`;

// ------------------------------------------------------------------------------------------------ funding loop

/**
 * Builds with no P2PK funding first; when kob-wasm reports insufficient funds (carriers smaller than the fee) adds maker UTXOs (smallest
 * single one that covers the shortfall, else largest-first) and retries. Any other build error is returned as an issue.
 */
function buildWithFunding(
  env: CancelEnv,
  make: (funding: KeyUtxo[]) => CancelRequest,
  initial: KeyUtxo[] = [],
): { built: BuiltTx; request: CancelRequest; fundingUsed: KeyUtxo[]; issues: PlanIssue[] } | { error: PlanIssue } {
  const r = buildWithFundingAt(env, make, initial);
  if (!('error' in r)) return r;
  // the wallet cannot pay the fee at the picked rate: try the floor before giving up (the estimate must not make a payable cancel impossible)
  const low = atFloor(env);
  if (!low) return r;
  const floorRequest = (funding: KeyUtxo[]): CancelRequest => {
    const q = make(funding) as CancelRequest & { fee?: Record<string, unknown> };
    return { ...q, fee: { ...(q.fee ?? {}), feeRate: low.feeRate!.toString() } } as CancelRequest;
  };
  const again = buildWithFundingAt(low, floorRequest, initial);
  return 'error' in again ? r : again;
}

function buildWithFundingAt(
  env: CancelEnv,
  make: (funding: KeyUtxo[]) => CancelRequest,
  initial: KeyUtxo[] = [],
): { built: BuiltTx; request: CancelRequest; fundingUsed: KeyUtxo[]; issues: PlanIssue[] } | { error: PlanIssue } {
  const pool = (env.funding ?? []).filter((f) => f.pubkey === env.maker).sort((a, b) => (big(b.amount) > big(a.amount) ? 1 : big(b.amount) < big(a.amount) ? -1 : 0));
  let used: KeyUtxo[] = [...initial];
  for (let attempt = 0; attempt < 8; attempt++) {
    const first = make(used);
    try {
      // the total fee cap: a fee above it is rebuilt once at a lower rate (fee-policy.ts)
      const { built, request, rate } = buildWithCap(env.fees?.policy, first, (r) => kobBuild(env, r));
      recordFee(env, built, rate);
      const issues = used.length > initial.length ? [issue('cancel.funding-added', 'info', 'the released carriers do not cover the network fee: a maker funding UTXO pays it', { inputs: used.length })] : [];
      return { built, request, fundingUsed: used, issues };
    } catch (e) {
      const msg = e instanceof KobError ? e.message : String(e);
      const m = /insufficient funds: need (\d+) sompi, have (\d+) sompi/.exec(msg);
      if (!m) return { error: issue('cancel.build-failed', 'error', msg) };
      const deficit = BigInt(m[1]) - BigInt(m[2]) + FUNDING_HEADROOM;
      const rest = pool.filter((p) => !used.includes(p));
      if (!rest.length) {
        return { error: issue('cancel.insufficient-funds', 'error', `not enough KAS: ${deficit - FUNDING_HEADROOM} sompi short and no funding UTXO left`, { shortfall: deficit - FUNDING_HEADROOM }) };
      }
      const single = [...rest].reverse().find((p) => big(p.amount) >= deficit);
      if (single) used = [...used, single];
      else used = [...used, rest[0]];
    }
  }
  return { error: issue('cancel.insufficient-funds', 'error', 'could not fund the network fee within 8 attempts') };
}

// ------------------------------------------------------------------------------------------------ helpers over snapshots

const totalAmount = (xs: TokenUtxo[]): bigint => xs.reduce((s, x) => s + big(x.state.amount), 0n);
const custodyUtxos = (s: { custody?: TokenUtxo | null; prefund?: TokenUtxo | null }): TokenUtxo[] => [s.custody ?? null, s.prefund ?? null].filter((x): x is TokenUtxo => !!x);
const kasOf = (s: OrderSnapshot): bigint =>
  big(s.order.amount) + custodyUtxos(s).reduce((a, x) => a + big(x.amount), 0n) + s.strays.reduce((a, x) => a + big(x.amount), 0n) +
  (s.foreign ?? []).reduce((a, g) => a + g.utxos.reduce((b, x) => b + big(x.amount), 0n), 0n);
const foreignReturnedOf = (gs: readonly ForeignStrays[]): CancelPlan['foreignReturned'] =>
  gs.length ? gs.map((g) => ({ covenantId: g.token.covenantId, amount: totalAmount(g.utxos), utxos: g.utxos.length })) : undefined;

/** One own token of a prepared order: its program's input slots, the extension commitment of its transfer (null: no input), its inputs. */
interface TokenGroup { token: Hex; role: 'base' | 'quote'; slots: number; ext: Hex | null; inputs: number }
interface Prepared {
  item: CancelItem; snap: OrderSnapshot; issues: PlanIssue[];
  /** the order token's (A's) slots and extension commitment */
  slots: number; ext: Hex | null;
  /** every own token (a pair order: A and B) */
  groups: TokenGroup[];
  foreign: ForeignStrays[];
}

/** The custody UTXOs of a snapshot that hold `token` (by the state's custodies: the first is `custody`, the second `prefund`). */
function custodiesOfToken(snap: OrderSnapshot, cs: readonly CustodyFact[], token: Hex): TokenUtxo[] {
  return [cs[0]?.token === token ? snap.custody : null, cs[1]?.token === token ? (snap.prefund ?? null) : null].filter((x): x is TokenUtxo => !!x);
}

/** What returns to the maker per role when the custodies and strays of a prepared order are released: base (A) and quote (B). */
function returnedOf(item: CancelItem, snap: OrderSnapshot): { base: bigint; quote: bigint } {
  const toks = ownTokenFacts(snap.order.state);
  const roleOf = (u: TokenUtxo, k: number | null): 'base' | 'quote' => {
    const cs = custodiesOf(snap.order.state);
    const token = k !== null ? cs[k]?.token : u.covenantId;
    return toks.find((t) => t.covenantId === token)?.role ?? 'base';
  };
  let base = 0n;
  let quote = 0n;
  const add = (u: TokenUtxo | null | undefined, k: number | null) => {
    if (!u) return;
    if (roleOf(u, k) === 'quote') quote += big(u.state.amount);
    else base += big(u.state.amount);
  };
  add(item.custody, 0);
  add(item.prefund, 1);
  for (const s of item.strays ?? []) add(s, null);
  return { base, quote };
}

/**
 * Foreign strays a CANCEL takes (it is the last spend of the order: after it nothing can move them): per token those sharing one extension
 * commitment within the program's inputs; the rest, and every foreign stray without a proven state / program, is reported (left behind).
 */
function prepareForeign(env: CancelEnv, snap: OrderSnapshot, opts: CancelOptions, issues: PlanIssue[]): ForeignStrays[] | { error: PlanIssue[] } {
  const left = (u: { outpoint: string; amount: bigint }) =>
    issues.push(issue('cancel.stray-other-token', 'warning', 'a stray of another token cannot be swept by this cancel and is left behind', { outpoint: u.outpoint, amount: u.amount }));
  for (const u of snap.foreignUnproven ?? []) left(u);
  const own = ownTokensOf(snap.order.state);
  const out: ForeignStrays[] = [];
  for (const g of snap.foreign ?? []) {
    const room = programSlots(env.kob, g.token.program);
    if (room === null || own.includes(g.token.covenantId)) {
      // not a pinned token program: nothing here can build its spend
      for (const u of g.utxos) left({ outpoint: outpointOf(u), amount: big(u.state.amount) });
      continue;
    }
    const p = pickStrays(g.utxos, null, room);
    for (const u of p.otherExt) {
      issues.push(issue('cancel.stray-other-extension', 'warning', 'a stray token with another extension commitment cannot be swept together with the rest and is left behind', { outpoint: outpointOf(u), amount: big(u.state.amount) }));
    }
    if (p.over.length) {
      if (!opts.allowAbandonStrays) {
        const n = p.take.length + p.over.length;
        return { error: [issue('cancel.strays-exceed-slots', 'error', `the order has ${n} stray token UTXOs but one transaction can move only ${room} of them: sweeping them is impossible, cancelling would abandon the rest`, { strays: n, room })] };
      }
      issues.push(issue('cancel.strays-abandoned', 'warning', `${p.over.length} stray token UTXO(s) do not fit the transaction and are abandoned (lost when the order closes)`, { count: p.over.length, amount: totalAmount(p.over), order: snap.covenantId }));
    }
    if (p.take.length) out.push({ token: g.token, utxos: p.take });
  }
  return out;
}

/**
 * Checks maker / custodies / strays of one snapshot and trims strays to what one transaction can carry: per own token (a pair order: A and B,
 * each its own transfer with its own program slots and extension commitment). `extra` = top-up inputs per token (cancel-replace).
 */
function prepare(env: CancelEnv, snap: OrderSnapshot, opts: CancelOptions, extra: ReadonlyMap<Hex, number> = new Map()): Prepared | { error: PlanIssue[] } {
  const issues: PlanIssue[] = [];
  const st = snap.order.state;
  if (st.state.maker !== env.maker) {
    return { error: [issue('cancel.not-maker', 'error', 'this order belongs to another key: only its maker can cancel it', { covenantId: snap.covenantId })] };
  }
  const cs = custodiesOf(st);
  if ((cs[0]?.amount ?? 0n) > 0n && !snap.custody) {
    return { error: [issue('cancel.custody-missing', 'error', 'the order\'s custody token UTXO is unknown: refresh the order from the indexer or node', { covenantId: snap.covenantId })] };
  }
  if ((cs[1]?.amount ?? 0n) > 0n && !snap.prefund) {
    return { error: [issue('cancel.prefund-missing', 'error', 'the order\'s prefund custody (its second token UTXO) is unknown: refresh the order from the indexer or node', { covenantId: snap.covenantId })] };
  }
  const toks = ownTokenFacts(st);
  const ownIds = toks.map((t) => t.covenantId);
  // a stray of another token (none of the order's own) cannot join the order's own transfers: the builder would refuse the whole cancel, so it is
  // left behind (reported) instead of making the order uncancellable
  const own = snap.strays.filter((s) => {
    if (ownIds.includes(s.covenantId ?? '')) return true;
    issues.push(issue('cancel.stray-other-token', 'warning', 'a stray of another token cannot be swept by this cancel and is left behind', { outpoint: outpointOf(s), amount: big(s.state.amount) }));
    return false;
  });
  if (snap.straysUnknown) {
    issues.push(issue('cancel.strays-unknown', 'warning', 'the stray tokens of this order could not be read: any that exist are not swept by this cancel and are lost when it closes'));
  }
  const kept: TokenUtxo[] = [];
  const groups: TokenGroup[] = [];
  for (const t of toks) {
    const held = custodiesOfToken(snap, cs, t.covenantId);
    const mine = own.filter((s) => s.covenantId === t.covenantId);
    // the transfer's extension commitment: the custody's (strays must share it), else the one the state names when a stray carries it, else the first stray's
    const named = t.ext !== null && mine.some((s) => extensionMatches(s.state, t.ext)) ? t.ext : null;
    const ext = (held[0] ? extensionOfState(held[0].state) : null) ?? named ?? (mine[0] ? extensionOfState(mine[0].state) : null) ?? (t.role === 'base' ? extensionOf(st) : null);
    // strays with another extension commitment cannot share one transfer with the rest (kob-wasm rule); they are reported, not silently moved
    let strays = mine.filter((s) => {
      if (!extensionMatches(s.state, ext)) {
        issues.push(issue('cancel.stray-other-extension', 'warning', 'a stray token with another extension commitment cannot be swept together with the rest and is left behind', { outpoint: outpointOf(s), amount: big(s.state.amount) }));
        return false;
      }
      return true;
    });
    const slots = tokenSlotsFor(env.kob, t.tplHash)?.inputs ?? 3;
    const room = slots - held.length - (extra.get(t.covenantId) ?? 0);
    if (strays.length > room) {
      if (!opts.allowAbandonStrays) {
        return {
          error: [issue('cancel.strays-exceed-slots', 'error', `the order has ${strays.length} stray token UTXOs but one transaction can move only ${Math.max(room, 0)} of them: sweeping them is impossible, cancelling would abandon the rest`, { strays: strays.length, room: Math.max(room, 0) })],
        };
      }
      strays = [...strays].sort((a, b) => (big(b.state.amount) > big(a.state.amount) ? 1 : -1));
      const dropped = strays.slice(Math.max(room, 0));
      strays = strays.slice(0, Math.max(room, 0));
      issues.push(issue('cancel.strays-abandoned', 'warning', `${dropped.length} stray token UTXO(s) do not fit the transaction and are abandoned (lost when the order closes)`, { count: dropped.length, amount: totalAmount(dropped), order: snap.covenantId }));
    }
    kept.push(...strays);
    groups.push({ token: t.covenantId, role: t.role, slots, ext: held.length || strays.length ? ext : null, inputs: held.length + strays.length });
  }
  const foreign = prepareForeign(env, snap, opts, issues);
  if (!Array.isArray(foreign)) return foreign;
  const item: CancelItem = { order: snap.order, custody: snap.custody, ...(snap.prefund ? { prefund: snap.prefund } : {}), strays: kept };
  const first = groups[0]!;
  const baseExt = (snap.custody && cs[0]?.token === first.token ? extensionOfState(snap.custody.state) : null) ?? first.ext ?? extensionOf(st) ?? null;
  return { item, snap: { ...snap, strays: kept, foreign }, issues, slots: first.slots, ext: baseExt, groups, foreign };
}

const isErr = (p: Prepared | { error: PlanIssue[] }): p is { error: PlanIssue[] } => 'error' in p;

/** `quoteReturned` of a plan when the order owns a quote token (a pair order), else nothing. */
const quoteField = (snap: OrderSnapshot, quote: bigint): { quoteReturned?: bigint } => (ownTokenFacts(snap.order.state).some((t) => t.role === 'quote') ? { quoteReturned: quote } : {});

// ------------------------------------------------------------------------------------------------ planCancel

/** Cancels one order (maker signature, SIGHASH_ALL). Custodies and strays (a pair order: of A and of B) are swept to the maker in the same tx. */
export function planCancel(env0: CancelEnv, order: OrderSnapshot, opts: CancelOptions = {}): CancelPlan {
  const env = withUrgency(env0, 'normal');
  const p = prepare(env, order, opts);
  if (isErr(p)) return failed(p.error);
  const make = (funding: KeyUtxo[]): CancelRequest => ({
    action: 'cancelOrder', order: p.item.order, custody: p.item.custody ?? null, ...(p.item.prefund ? { prefund: p.item.prefund } : {}), strays: p.item.strays ?? [],
    tokens: [], funding, change: env.changeTo ?? null, replace: null, ...(p.foreign.length ? { foreign: p.foreign } : {}), lockTime: '0', records: [], fee: feeOpts(env),
  });
  const r = buildWithFunding(env, make);
  if ('error' in r) return failed([...p.issues, r.error], [order.covenantId]);
  const foreignReturned = foreignReturnedOf(p.foreign);
  const back = returnedOf(p.item, p.snap);
  return {
    ok: true, issues: [...p.issues, ...r.issues], request: r.request, built: r.built, cancelIds: [order.covenantId], expected: withChange(env, r.built, { cancelIds: [order.covenantId] }),
    tokensReturned: back.base, ...quoteField(order, back.quote), kasReleased: kasOf(p.snap), fundingUsed: r.fundingUsed,
    ...(foreignReturned ? { foreignReturned } : {}),
  };
}

// ------------------------------------------------------------------------------------------------ planCancelReplace

export interface ReplacementSpec {
  order: OrderState;
  /** KAS on the new order UTXO (carrier / escrow) */
  value: bigint;
  /** KAS on the new custody + token change outputs (default: the carrier of the old custody) */
  tokenCarrier?: bigint | null;
  /** day orders: deadline (UTC unix seconds) for the placement record */
  deadline?: bigint | null;
}

/** Cheapest set of maker P2PK token UTXOs of `token` (fewest inputs, smallest sufficient single first) covering `amount`. */
function pickTokens(pool: TokenUtxo[], token: Hex, amount: bigint, ext: Hex | null, maker: Hex, maxInputs: number): TokenUtxo[] | null {
  const usable = pool.filter((t) => (!t.covenantId || t.covenantId === token) && isKeyOwned(t.state) && t.state.owner === maker && extensionMatches(t.state, ext));
  const sorted = [...usable].sort((a, b) => (big(b.state.amount) > big(a.state.amount) ? 1 : big(b.state.amount) < big(a.state.amount) ? -1 : 0));
  const single = [...sorted].reverse().find((t) => big(t.state.amount) >= amount);
  if (single) return [single];
  const picked: TokenUtxo[] = [];
  let sum = 0n;
  for (const t of sorted) {
    if (picked.length >= maxInputs) break;
    picked.push(t);
    sum += big(t.state.amount);
    if (sum >= amount) return picked;
  }
  return null;
}

/** Order-state fields an in-place amend keeps (kob-protocol `payload::check_amend_terms`): an ask's custody, owned by the order's id, stays exact. */
const AMEND_KEEPS = ['maker', 'tokenCovId', 'tokenTplHash', 'tplPrefixLen', 'tplSuffixLen', 'scale', 'amountLeft'] as const;
/** A plain bid owns no custody (its quantity is its escrow): it keeps its maker, its token (with the pinned delivery extension) and its scale. */
const AMEND_KEEPS_BID = ['maker', 'tokenCovId', 'tokenTplHash', 'tplPrefixLen', 'tplSuffixLen', 'extensionCommitment', 'scale'] as const;

/**
 * Whether an amendment can be IN PLACE (kob-protocol `AmendOrder`): a plain ask amended to the same kind, keeping the maker, the token, the scale and
 * `amountLeft` (price, tip, minimum fill, time in force, timing and decay may change), with its custody known and no strays (only a cancel sweeps strays, an
 * in-place amend leaves them where they are) and no top-up tokens. The maker's cancel then continues the order's covenant id and the custody never
 * moves: no token program is revealed (an 8/8 ask amend is a fifth of a cancel-replace). Pair orders are never amended in place (cancel-replace).
 */
export function amendsInPlace(order: OrderSnapshot, rep: OrderState, extraTokens?: TokenUtxo[]): boolean {
  const old = order.order.state;
  if (old.kind !== rep.kind) return false;
  const a = old.state as unknown as Record<string, string>;
  const b = rep.state as unknown as Record<string, string>;
  // a bid: same safety rules as an ask without the custody (nothing to keep exact), strays stay where they are so only a cancel-replace sweeps them
  const foreign = (order.foreign ?? []).length > 0;
  if (isKind(old, 'KobBid')) return !order.strays.length && !foreign && !order.straysUnknown && (extraTokens?.length ?? 0) === 0 && AMEND_KEEPS_BID.every((k) => a[k] === b[k]);
  if (!isKind(old, 'KobAsk')) return false;
  if (order.strays.length || foreign || (extraTokens?.length ?? 0) > 0 || !order.custody) return false;
  return AMEND_KEEPS.every((k) => a[k] === b[k]);
}

/** The in-place amend: the maker's cancel continues the order's covenant id with the new state; the order's carrier pays the fee (no change). */
function planAmendInPlace(env: CancelEnv, order: OrderSnapshot, replacement: ReplacementSpec): CancelPlan {
  if (order.order.state.state.maker !== env.maker) {
    return failed([issue('cancel.not-maker', 'error', 'this order belongs to another key: only its maker can amend it', { covenantId: order.covenantId })]);
  }
  // a bid topped up (more escrow than the order holds) states its value even before funding is added: the builder then reports the shortfall and the
  // funding loop adds maker UTXOs; otherwise the order's own value pays the fee and the continuation keeps the rest
  const topUp = replacement.value > big(order.order.amount);
  const make = (funding: KeyUtxo[]): CancelRequest => ({
    action: 'amendOrder', order: order.order, amended: replacement.order, value: funding.length || topUp ? replacement.value.toString() : null, funding,
    change: env.changeTo ?? null, lockTime: '0', deadline: replacement.deadline != null ? replacement.deadline.toString() : null, records: [], fee: feeOpts(env),
  });
  const r = buildWithFunding(env, make);
  if ('error' in r) return failed([r.error], [order.covenantId]);
  // the continuation is output 0: the order's value less the fee it paid (no funding), or the planned value plus at most a tiny change it took
  const kasLocked = big(r.built.tx.outputs[0].value);
  const base = r.fundingUsed.length ? replacement.value : big(order.order.amount);
  if (kasLocked < base - big(r.built.fee.fee) || kasLocked > base + TINY_CHANGE_MAX) {
    return failed([issue('cancel.build-failed', 'error', `the built amend leaves ${kasLocked} sompi on the order, not about ${base}`)], [order.covenantId]);
  }
  const inPlaceIssue = isKind(order.order.state, 'KobBid')
    ? issue('cancel.amend-in-place-bid', 'info', 'amended in place: the buy order keeps its id and its KAS stays on the order (no new order, a much smaller fee)')
    : issue('cancel.amend-in-place', 'info', 'amended in place: the order keeps its id and its tokens stay in custody (no token transfer)');
  return {
    ok: true, issues: [inPlaceIssue, ...r.issues],
    request: r.request, built: r.built, cancelIds: [order.covenantId],
    expected: withChange(env, r.built, { cancelIds: [order.covenantId], orders: [replacement.order], kasLocked }),
    tokensReturned: 0n, kasReleased: big(order.order.amount), fundingUsed: r.fundingUsed,
  };
}

/**
 * Cancel + new placement in ONE atomic transaction (an amendment). The replacement takes its tokens from the old custodies, the swept strays and,
 * when those are not enough, maker tokens (`extraTokens`, or picked from `env.tokenUtxos`), per token (a pair order: A and B; the replacement must
 * trade the same pair). An armed stop is replaced UNARMED (matcher.md 10.9). A plain ask whose new terms keep its custody exact is amended IN PLACE
 * instead ([`amendsInPlace`]; `opts.noInPlace` forces the cancel-replace).
 */
export function planCancelReplace(env0: CancelEnv, order: OrderSnapshot, replacement: ReplacementSpec, extraTokens?: TokenUtxo[], opts: CancelOptions = {}): CancelPlan {
  const env = withUrgency(env0, 'normal');
  const rep = replacement.order;
  const old = order.order.state;
  const issues: PlanIssue[] = [];
  if (rep.state.maker !== env.maker) return failed([issue('cancel.replacement-wrong-maker', 'error', 'the replacement must belong to the wallet key')], [order.covenantId]);
  if (tokenCovIdOf(rep) !== tokenCovIdOf(old) || quoteCovIdOf(rep) !== quoteCovIdOf(old)) {
    return failed([issue('cancel.replacement-wrong-token', 'error', 'the replacement must trade the same token (a pair order: the same pair) as the cancelled order')], [order.covenantId]);
  }
  if (!opts.noInPlace && amendsInPlace(order, rep, extraTokens)) return planAmendInPlace(env, order, replacement);
  let newState = rep;
  const armedNow = 'armed' in rep.state ? big((rep.state as { armed: string }).armed) : 0n;
  if (armedNow !== 0n) {
    newState = { ...rep, state: { ...rep.state, armed: '0' } } as OrderState;
    issues.push(issue('cancel.replacement-restarts-unarmed', 'info', 'an amended stop starts unarmed and must trigger again'));
  } else if ('armed' in old.state && big((old.state as { armed: string }).armed) !== 0n && ('armed' in rep.state)) {
    issues.push(issue('cancel.replacement-restarts-unarmed', 'info', 'an amended stop starts unarmed and must trigger again'));
  }
  // what the replacement must hold, per token (a pair order: its custodies of A and / or B)
  const needs = new Map<Hex, bigint>();
  for (const c of custodiesOf(newState)) if (c.amount > 0n) needs.set(c.token, (needs.get(c.token) ?? 0n) + c.amount);
  const p0 = prepare(env, order, opts);
  if (isErr(p0)) return failed(p0.error, [order.covenantId]);
  const haveOf = (p: Prepared, token: Hex): bigint => totalAmount([...custodyUtxos(p.item), ...(p.item.strays ?? [])].filter((u) => (u.covenantId ?? tokenCovIdOf(old)) === token));
  let tokens = extraTokens ?? [];
  const topUps = new Map<Hex, number>();
  for (const [token, need] of needs) {
    const supplied = totalAmount(tokens.filter((t) => (t.covenantId ?? token) === token));
    const have = haveOf(p0, token);
    if (need <= have + supplied) {
      topUps.set(token, tokens.filter((t) => (t.covenantId ?? token) === token).length);
      continue;
    }
    if (extraTokens) return failed([...p0.issues, issue('cancel.insufficient-tokens', 'error', 'the supplied top-up tokens do not cover the replacement\'s amount', { need, have: have + supplied })], [order.covenantId]);
    const g = p0.groups.find((x) => x.token === token);
    const ext = g?.ext ?? (token === tokenCovIdOf(newState) ? extensionOf(newState) : pairExtFor(newState, token));
    const picked = pickTokens(env.tokenUtxos ?? [], token, need - have, ext ?? null, env.maker, Math.max((g?.slots ?? 3) - (g?.inputs ?? 0), 0));
    if (!picked) return failed([...p0.issues, issue('cancel.insufficient-tokens', 'error', 'not enough free tokens to top the replacement up', { need: need - have })], [order.covenantId]);
    tokens = [...tokens, ...picked];
    topUps.set(token, picked.length);
    issues.push(issue('cancel.top-up', 'info', 'the replacement needs more tokens than the order holds: free tokens are added', { amount: totalAmount(picked) }));
  }
  // top-up inputs share the token transfer: re-run the stray trimming with the extra token inputs counted
  const p = prepare(env, order, opts, topUps);
  if (isErr(p)) return failed(p.error, [order.covenantId]);
  const firstCustody = p.item.custody ?? p.item.prefund ?? null;
  const carrier = replacement.tokenCarrier ?? (firstCustody ? big(firstCustody.amount) : tokens[0] ? big(tokens[0].amount) : 0n);
  const rp: Replacement = {
    order: newState, value: replacement.value.toString(), tokenCarrier: needs.size ? carrier.toString() : null, deadline: replacement.deadline != null ? replacement.deadline.toString() : null,
  };
  const make = (funding: KeyUtxo[]): CancelRequest => ({
    action: 'cancelOrder', order: p.item.order, custody: p.item.custody ?? null, ...(p.item.prefund ? { prefund: p.item.prefund } : {}), strays: p.item.strays ?? [], tokens, funding,
    change: env.changeTo ?? null, replace: rp, ...(p.foreign.length ? { foreign: p.foreign } : {}), lockTime: '0', records: [], fee: feeOpts(env),
  });
  const r = buildWithFunding(env, make);
  if ('error' in r) return failed([...issues, ...p.issues, r.error], [order.covenantId]);
  // the replacement is output 0; it holds `replacement.value`, or more when a tiny change rode on it (kob-protocol's change rule)
  const extra = big(r.built.tx.outputs[0].value) - replacement.value;
  if (extra < 0n || extra > TINY_CHANGE_MAX) {
    return failed([issue('cancel.build-failed', 'error', `the built replacement holds ${extra} sompi more than planned`)], [order.covenantId]);
  }
  const roleOfToken = (token: Hex): 'base' | 'quote' => ownTokenFacts(newState).find((t) => t.covenantId === token)?.role ?? 'base';
  const needBase = [...needs].filter(([t]) => roleOfToken(t) === 'base').reduce((a, [, v]) => a + v, 0n);
  const needQuote = [...needs].filter(([t]) => roleOfToken(t) === 'quote').reduce((a, [, v]) => a + v, 0n);
  const pairOrder = !!pairFactsOf(newState);
  const kasLocked = replacement.value + extra + carrier * BigInt(needs.size);
  const back = returnedOf(p.item, p.snap);
  const topBase = totalAmount(tokens.filter((t) => roleOfToken(t.covenantId ?? tokenCovIdOf(newState)) === 'base'));
  const topQuote = totalAmount(tokens.filter((t) => roleOfToken(t.covenantId ?? tokenCovIdOf(newState)) === 'quote'));
  return {
    ok: true, issues: [...issues, ...p.issues, ...r.issues], request: r.request, built: r.built, cancelIds: [order.covenantId],
    expected: withChange(env, r.built, {
      cancelIds: [order.covenantId], orders: [newState],
      ...(needs.size || custodiesOf(newState).length ? { tokensEscrowed: needBase } : {}), ...(pairOrder ? { quoteEscrowed: needQuote } : {}), kasLocked,
    }),
    tokensReturned: back.base + topBase - needBase, ...quoteField(order, back.quote + topQuote - needQuote), kasReleased: kasOf(p.snap), fundingUsed: r.fundingUsed,
  };
}

// ------------------------------------------------------------------------------------------------ planCancelAll

/**
 * Cancels several orders (a position: an if-done entry with all its exits, a repeat with its booked exits, or any set) with the fewest
 * transactions: kob-wasm `cancelPosition` covers ONE maker and ONE token (a pair position: ONE pair, A and B) per tx, so orders are grouped by
 * their own tokens (and the extension commitments of their transfers), and a group is split when any token's inputs exceed its program's input
 * slots or `maxOrdersPerTx`. A group of one order is a plain cancel. Returns one plan per transaction (submit them in order; they spend disjoint
 * UTXOs, maker funding included: a funding UTXO used by one plan is not offered to the later ones).
 */
export function planCancelAll(env0: CancelEnv, orders: OrderSnapshot[], opts: CancelOptions = {}): CancelPlan[] {
  const env = withUrgency(env0, 'normal');
  if (!orders.length) return [failed([issue('cancel.nothing', 'error', 'nothing to cancel')])];
  const maxOrders = Math.max(opts.maxOrdersPerTx ?? 8, 1);
  const plans: CancelPlan[] = [];
  // groups of orders that one cancelPosition can take: the same own tokens, and per token one extension commitment (null: no input of that token)
  const groups: { key: string; exts: Map<Hex, Hex | null>; members: Prepared[] }[] = [];
  for (const o of orders) {
    const p = prepare(env, o, opts);
    if (isErr(p)) {
      plans.push(failed(p.error, [o.covenantId]));
      continue;
    }
    // an order with foreign strays (a plain cancelOrder returns them; cancelPosition moves no other token) is cancelled in a transaction of its own
    if (p.foreign.length) {
      groups.push({ key: `own:${o.covenantId}`, exts: new Map(), members: [p] });
      continue;
    }
    const key = p.groups.map((g) => g.token).join('|');
    const fits = groups.find((g) => g.key === key && p.groups.every((t) => t.ext === null || (g.exts.get(t.token) ?? null) === null || g.exts.get(t.token) === t.ext));
    if (fits) {
      fits.members.push(p);
      for (const t of p.groups) if (t.ext !== null) fits.exts.set(t.token, t.ext);
    } else groups.push({ key, exts: new Map(p.groups.map((t) => [t.token, t.ext])), members: [p] });
  }
  // the transactions of one cancel-all are submitted one after the other: a maker funding UTXO spent by one plan must not be offered to
  // the next (it would be "already spent" and stop the sequence at step 2)
  const spent = new Set<string>();
  const key = (u: KeyUtxo): string => `${u.transactionId}:${u.index}`;
  for (const group of groups) {
    let chunk: Prepared[] = [];
    let used = new Map<Hex, number>();
    const flush = () => {
      if (chunk.length) {
        const plan = buildPosition({ ...env, funding: (env.funding ?? []).filter((f) => !spent.has(key(f))) }, chunk);
        for (const f of plan.fundingUsed) spent.add(key(f));
        plans.push(plan);
      }
      chunk = [];
      used = new Map();
    };
    for (const p of group.members) {
      const over = p.groups.some((g) => (used.get(g.token) ?? 0) + g.inputs > g.slots);
      if (chunk.length && (over || chunk.length >= maxOrders)) flush();
      chunk.push(p);
      for (const g of p.groups) used.set(g.token, (used.get(g.token) ?? 0) + g.inputs);
    }
    flush();
  }
  return plans;
}

function buildPosition(env: CancelEnv, chunk: Prepared[]): CancelPlan {
  const carried = chunk.flatMap((c) => c.issues);
  if (chunk.length === 1) {
    const only = chunk[0];
    const plan = planCancel(env, only.snap, { allowAbandonStrays: true });
    return { ...plan, issues: [...carried, ...plan.issues.filter((i) => !carried.some((c) => c.code === i.code))] };
  }
  const ids = chunk.map((c) => c.snap.covenantId);
  const make = (funding: KeyUtxo[]): CancelRequest => ({
    action: 'cancelPosition', orders: chunk.map((c) => c.item), funding, change: env.changeTo ?? null, lockTime: '0', tokenCarrier: null, records: [], fee: feeOpts(env),
  });
  const r = buildWithFunding(env, make);
  if ('error' in r) return failed([...carried, r.error], ids);
  const back = chunk.reduce((s, c) => {
    const x = returnedOf(c.item, c.snap);
    return { base: s.base + x.base, quote: s.quote + x.quote };
  }, { base: 0n, quote: 0n });
  return {
    ok: true, issues: [...carried, ...r.issues], request: r.request, built: r.built, cancelIds: ids, expected: withChange(env, r.built, { cancelIds: ids }),
    tokensReturned: back.base, ...quoteField(chunk[0]!.snap, back.quote), kasReleased: chunk.reduce((s, c) => s + kasOf(c.snap), 0n), fundingUsed: r.fundingUsed,
  };
}

// ------------------------------------------------------------------------------------------------ planSweep

/**
 * The maker's SWEEP of an order's strays IN PLACE (kob-protocol `SweepOrder`, matcher.md 1.2): the maker's cancel continues the order under the
 * SAME script (same state, price, amount, custodies; only its UTXO, so its 90-day idle window restarts) and returns its strays to the maker, one
 * token output per token: strays of its own token(s) (a pair order: A and B, each one extension commitment within its program's inputs) and
 * FOREIGN strays (other tokens owned by its id, with the program their state was proven under). What one sweep cannot take (another extension
 * commitment, more than the program's inputs) stays with the order for a later sweep (the order is still there); foreign strays without a proven
 * state are reported. Fee: a plain ask's carrier pays it without funding; every other kind's value is an escrow, a prefund, carriers or keeper
 * tips, so one maker funding UTXO is added from the start (more when the fee needs it).
 */
export function planSweep(env0: CancelEnv, snap: OrderSnapshot): CancelPlan {
  const env = withUrgency(env0, 'normal');
  const st = snap.order.state;
  const cov = snap.covenantId;
  if (st.state.maker !== env.maker) {
    return failed([issue('cancel.not-maker', 'error', 'this order belongs to another key: only its maker can sweep its strays', { covenantId: cov })]);
  }
  const issues: PlanIssue[] = [];
  let later = 0;
  const leaveLater = (xs: TokenUtxo[], why: 'extension' | 'slots') => {
    if (!xs.length) return;
    later += xs.length;
    const params = { count: xs.length, amount: totalAmount(xs) };
    issues.push(why === 'extension'
      ? issue('sweep.later-extension', 'info', `${xs.length} stray token UTXO(s) with another extension commitment stay with the order for another sweep`, params)
      : issue('sweep.later-slots', 'info', `${xs.length} stray token UTXO(s) do not fit this transaction and stay with the order for another sweep`, params));
  };
  const cs = custodiesOf(st);
  const toks = ownTokenFacts(st);
  const ownIds = toks.map((t) => t.covenantId);
  const picks = toks.map((t) => {
    const held = custodiesOfToken(snap, cs, t.covenantId);
    const ext = (held[0] ? extensionOfState(held[0].state) : null) ?? t.ext ?? (t.role === 'base' ? extensionOf(st) : null);
    return { t, pick: pickStrays(snap.strays.filter((s) => s.covenantId === t.covenantId), ext, tokenSlotsFor(env.kob, t.tplHash)?.inputs ?? 3) };
  });
  leaveLater(picks.flatMap((x) => x.pick.otherExt), 'extension');
  leaveLater(picks.flatMap((x) => x.pick.over), 'slots');
  let unproven = 0;
  const unprovenIssue = (u: { outpoint: string; amount: bigint }) => {
    unproven++;
    issues.push(issue('sweep.stray-unproven', 'warning', 'a stray of another token has no proven state: nothing can move it, it stays with the order', { outpoint: u.outpoint, amount: u.amount }));
  };
  // a stray of another token passed among the order's own (a caller's snapshot): never built as one, reported
  for (const s of snap.strays.filter((x) => !ownIds.includes(x.covenantId ?? ''))) unprovenIssue({ outpoint: outpointOf(s), amount: big(s.state.amount) });
  for (const u of snap.foreignUnproven ?? []) unprovenIssue(u);
  const foreign: ForeignStrays[] = [];
  for (const g of snap.foreign ?? []) {
    const room = programSlots(env.kob, g.token.program);
    if (room === null || ownIds.includes(g.token.covenantId)) {
      for (const u of g.utxos) unprovenIssue({ outpoint: outpointOf(u), amount: big(u.state.amount) });
      continue;
    }
    const p = pickStrays(g.utxos, null, room);
    leaveLater(p.otherExt, 'extension');
    leaveLater(p.over, 'slots');
    if (p.take.length) foreign.push({ token: g.token, utxos: p.take });
  }
  if (snap.straysUnknown) issues.push(issue('sweep.strays-unknown', 'warning', 'the stray tokens of this order could not be fully read: a later sweep may find more'));
  const strays = picks.flatMap((x) => x.pick.take);
  if (!strays.length && !foreign.length) return failed([...issues, issue('sweep.nothing', 'error', 'this order has no stray tokens this app can sweep')], [cov]);

  // a plain ask's carrier pays the fee; every other kind needs a maker funding UTXO from the start (the smallest of at least 1 KAS, else the largest)
  const carrierPays = baseKind(st.kind) === 'KobAsk';
  const pool = (env.funding ?? []).filter((f) => f.pubkey === env.maker).sort((x, y) => (big(x.amount) < big(y.amount) ? -1 : big(x.amount) > big(y.amount) ? 1 : 0));
  const seed = carrierPays ? null : (pool.find((f) => big(f.amount) >= ONE_KAS) ?? pool[pool.length - 1] ?? null);
  if (!carrierPays && !seed) {
    return failed([...issues, issue('sweep.needs-funding', 'error', 'sweeping the strays of this order needs a KAS UTXO of the wallet to pay the network fee (its own value is not a carrier)')], [cov]);
  }
  const initial = seed ? [seed] : [];
  const make = (funding: KeyUtxo[]): CancelRequest => ({
    action: 'sweepOrder', order: snap.order, strays, ...(foreign.length ? { foreign } : {}), funding, change: env.changeTo ?? null, tokenCarrier: null, lockTime: '0',
    records: [], fee: feeOpts(env),
  });
  const r = buildWithFunding(env, make, initial);
  if ('error' in r) return failed([...issues, r.error], [cov]);
  // the continuation is output 0 under the SAME script: with funding it keeps the order's whole value, without it the fee comes off the carrier
  const cont = r.built.tx.outputs[0];
  const kept = cont ? big(cont.value) : -1n;
  const value = big(snap.order.amount);
  if (!cont || kept > value || (r.fundingUsed.length > 0 && kept !== value) || kept < value - big(r.built.fee.fee)) {
    return failed([...issues, issue('cancel.build-failed', 'error', `the built sweep leaves ${kept} sompi on the order, not about ${value}`)], [cov]);
  }
  const tokens = [
    ...picks.filter((x) => x.pick.take.length).map((x) => ({ covenantId: x.t.covenantId, amount: totalAmount(x.pick.take), utxos: x.pick.take.length, foreign: false })),
    ...foreign.map((g) => ({ covenantId: g.token.covenantId, amount: totalAmount(g.utxos), utxos: g.utxos.length, foreign: true })),
  ];
  const all = [...strays, ...foreign.flatMap((g) => g.utxos)];
  const kas = all.reduce((a, x) => a + big(x.amount), 0n);
  const foreignReturned = foreignReturnedOf(foreign);
  const base = totalAmount(picks.filter((x) => x.t.role === 'base').flatMap((x) => x.pick.take));
  const quote = totalAmount(picks.filter((x) => x.t.role === 'quote').flatMap((x) => x.pick.take));
  const untouched = custodyUtxos(snap).map(outpointOf);
  return {
    ok: true,
    issues: [issue('sweep.in-place', 'info', 'swept in place: the order keeps its id, terms and custody; only the strays move'), ...issues, ...r.issues],
    request: r.request, built: r.built, cancelIds: [cov],
    expected: withChange(env, r.built, { sweepIds: [cov], orders: [st], ...(untouched.length ? { untouched } : {}) }),
    tokensReturned: base, ...quoteField(snap, quote), kasReleased: kas, fundingUsed: r.fundingUsed,
    ...(foreignReturned ? { foreignReturned } : {}),
    sweep: { covenantId: cov, utxos: all.length, kas, tokens, later, unproven },
  };
}

// ------------------------------------------------------------------------------------------------ planRefund

/**
 * The maker refunds their OWN order once it is refundable (expiry / 90 days idle / IOC-FOK kill). Refunds are permissionless and their fee is
 * paid from the order's `refundTip`:
 *   * default: NO signature at all (the contracts authorise it); the whole tip is spent as the network fee (a change output of a few 0.01 KAS
 *     costs more storage mass than it returns);
 *   * `reclaimTip`: one maker funding UTXO (>= 1 KAS, the smallest) is added so the tip minus the minimum fee returns to the maker as change:
 *     costs one wallet signature and saves ~0.02 KAS.
 * A refund never moves strays (they stay with the order id: cancel instead if there are any). A sell-first pair entry's refund returns both its
 * custodies (A and the B prefund).
 * Whether the order is due yet is decided by kob-wasm (`lockTime` = current DAA); a refusal comes back as `refund.not-yet`.
 */
export function planRefund(env0: CancelEnv, order: OrderSnapshot, opts: { reclaimTip?: boolean } = {}): CancelPlan {
  const env = withUrgency(env0, 'normal');
  if (order.order.state.state.maker !== env.maker) {
    return failed([issue('cancel.not-maker', 'error', 'this order belongs to another key', { covenantId: order.covenantId })]);
  }
  if (!env.clock) return failed([issue('refund.no-clock', 'error', 'the current DAA score is needed to refund')], [order.covenantId]);
  const issues: PlanIssue[] = [];
  const strayCount = order.strays.length + (order.foreign ?? []).reduce((a, g) => a + g.utxos.length, 0) + (order.foreignUnproven?.length ?? 0);
  if (strayCount) {
    issues.push(issue('refund.strays-stay', 'warning', 'a refund does not move stray tokens sent to this order; cancel it instead to recover them', { count: strayCount }));
  }
  const cs = custodiesOf(order.order.state);
  if ((cs[0]?.amount ?? 0n) > 0n && !order.custody) return failed([issue('cancel.custody-missing', 'error', "the order's custody token UTXO is unknown")], [order.covenantId]);
  if ((cs[1]?.amount ?? 0n) > 0n && !order.prefund) return failed([issue('cancel.prefund-missing', 'error', "the order's prefund custody (its second token UTXO) is unknown")], [order.covenantId]);
  const holding = cs.length > 0;
  const make = (funding: KeyUtxo[]): CancelRequest => ({
    action: 'refundOrder', order: order.order, custody: holding ? order.custody : null, ...(cs.length > 1 && order.prefund ? { prefund: order.prefund } : {}),
    lockTime: env.clock!.daa.toString(), funding, change: env.changeTo ?? env.maker, fee: feeOpts(env),
  });
  const seed = opts.reclaimTip
    ? [...(env.funding ?? [])].filter((f) => f.pubkey === env.maker && big(f.amount) >= ONE_KAS).sort((a, b) => (big(a.amount) < big(b.amount) ? -1 : 1)).slice(0, 1)
    : [];
  const r = buildWithFunding(env, make, seed);
  if ('error' in r) {
    const m = /not refundable before DAA (\d+)/.exec(r.error.message);
    const e = m ? issue('refund.not-yet', 'error', `not refundable before DAA ${m[1]} (now ${env.clock.daa})`, { dueDaa: BigInt(m[1]), nowDaa: env.clock.daa }) : r.error;
    return failed([...issues, e], [order.covenantId]);
  }
  const back = returnedOf({ order: order.order, custody: holding ? order.custody : null, prefund: order.prefund ?? null, strays: [] }, order);
  return {
    ok: true, issues: [...issues, ...r.issues], request: r.request, built: r.built, cancelIds: [order.covenantId], expected: withChange(env, r.built, { cancelIds: [order.covenantId] }),
    tokensReturned: back.base, ...quoteField(order, back.quote),
    kasReleased: big(order.order.amount) + custodyUtxos({ custody: holding ? order.custody : null, prefund: order.prefund }).reduce((a, x) => a + big(x.amount), 0n), fundingUsed: r.fundingUsed,
  };
}
