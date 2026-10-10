// Merging the wallet's own plain token UTXOs of one token into one ("Merge tokens"; the UI answer to TOKEN_UTXOS_FRAGMENTED).
//
//   * the inputs: the token UTXOs the wallet key owns by itself (KCC-20 scheme 0, KRON address presence), plain (no borrow / minter flag), of the
//     token's extension commitment, never an order's custody or stray (those are owned by the order's covenant id) nor an outpoint the caller
//     reserves; smallest first, ties by outpoint (the soak's `selectMergeChain` rule);
//   * a CHAIN of `sendTokens` transfers to the wallet key: the first takes up to the program's token inputs (KOB's standard KCC20Ref: 3), every
//     next one the previous output plus up to inputs - 1 more, so one pass ends with ONE UTXO (3 -> 1, then 1 + 2 -> 1, ...). Each link is a
//     transfer of at most the program's inputs into one output, valid on its own;
//   * the merged output keeps the largest input carrier (at most the default 2 KAS carrier, at least the program's floor); the other carriers
//     are freed and pay the fee, the rest returns as KAS change. Only when they do not cover it (or a KRON transfer, which needs a P2PK input of
//     the owner) is a wallet KAS UTXO added;
//   * fee: the LOW bucket of the fee policy (housekeeping, matcher.md 9; the soak's merges and the executor's maintenance pay the same).
// A link after the first spends an output that exists only once the previous link is accepted: `planMerge` builds the whole chain against
// provisional outpoints (the count, fee and result shown before anything is signed); the app rebuilds each later link from the node's UTXO
// after the previous one is accepted (`mergeCarry` + `buildMergeLink`). Pure (no I/O).
import { buildWithCap, recordFee, withUrgency, type FeeContext, type RatePick } from './fee-policy';
import { MAX_TOKEN_INPUTS } from './guards';
import { DEFAULT_CARRIER, tokenCarrierFloor } from './orders/common';
import type { PlanIssue } from './plan-types';
import { guardRequest } from './request-guard';
import { KRON_MAX_OUTPUT_AMOUNT, extensionOfState, familyOfProgram, isKeyOwned, isPlainState, keyState } from './token-state';
import type { BuiltTx, FeeMode, Hex, KeyUtxo, SendTokensRequest, TokenProgram, TokenState, TokenUtxo } from './types';
import { KobError, type KobWasm } from './wasm';

/** Most transactions one merge runs (one signature each): a larger set is merged in several runs. */
export const MAX_MERGE_TXS = 10;
const I64_MAX = (1n << 63n) - 1n;
const ONE_KAS = 100_000_000n;

/** The token to merge (a registry `TokenInfo` fits). */
export interface MergeToken {
  covenantId: Hex;
  program: TokenProgram;
  templateHash: Hex;
  /** KCC-20: the token's extension commitment (null: any one, as long as all inputs share it); ignored for KRON */
  extensionCommitment: Hex | null;
  slots: { inputs: number };
}

/** What the merge builder needs (a `CancelEnv` fits). */
export interface MergeEnv {
  kob: KobWasm;
  /** the wallet's x-only key: owner of the inputs and of the merged output */
  maker: Hex;
  /** P2PK KAS UTXOs of the wallet, used only when the freed carriers cannot pay the fee (and for KRON) */
  funding?: KeyUtxo[];
  feeRate?: bigint;
  feeMode?: FeeMode;
  fees?: FeeContext;
  feePick?: RatePick;
}

export interface MergeLink {
  /** the link's token inputs (from the second link on, the previous link's output first) */
  inputs: TokenUtxo[];
  /** inputs of the wallet's original set merged by this link */
  fresh: TokenUtxo[];
  built: BuiltTx;
  request: SendTokensRequest;
  fundingUsed: KeyUtxo[];
  /** the merged output */
  output: { index: number; carrier: bigint; state: TokenState };
  fee: bigint;
  issues: PlanIssue[];
}

export interface MergePlan {
  ok: boolean;
  issues: PlanIssue[];
  /** the wallet's original UTXOs each link merges (link 0: up to `inputs`, then up to `inputs - 1`) */
  groups: TokenUtxo[][];
  /** the chain built against provisional outpoints (link k > 0 spends link k - 1's output under its unsigned id) */
  links: MergeLink[];
  /** mergeable UTXOs before / after the whole chain */
  before: number;
  after: number;
  /** base units merged into the final output */
  amount: bigint;
  /** sum of the links' fees (the signed transactions pay at most this: finalizing only shrinks the compute budgets) */
  fee: bigint;
}

const big = (v: string | number | bigint): bigint => BigInt(v);
export const outpointOf = (u: { transactionId: string; index: number }): string => `${u.transactionId}:${u.index}`;
const issue = (code: string, severity: PlanIssue['severity'], message: string, params?: PlanIssue['params']): PlanIssue => ({ code, severity, message, ...(params ? { params } : {}) });

/** Token inputs one link may take: the program's slots, never above the covenant input limit. */
export const mergeSlots = (token: Pick<MergeToken, 'slots'>): number => Math.min(token.slots.inputs, MAX_TOKEN_INPUTS);

/**
 * The UTXOs a merge may spend: plain token UTXOs of `token` the wallet key owns by itself, with the token's extension commitment, a positive amount,
 * not in `reserved` (outpoints `txid:index`); smallest first, ties by outpoint.
 */
export function mergeCandidates(utxos: readonly TokenUtxo[], token: Pick<MergeToken, 'covenantId' | 'program' | 'extensionCommitment'>, maker: Hex, reserved: ReadonlySet<string> = new Set()): TokenUtxo[] {
  const kron = familyOfProgram(token.program) === 'kron';
  const ext = kron ? null : token.extensionCommitment;
  return utxos
    .filter(
      (u) =>
        u.covenantId === token.covenantId &&
        isKeyOwned(u.state) &&
        isPlainState(u.state) &&
        u.state.owner === maker &&
        big(u.state.amount) > 0n &&
        (kron ? 'id_type' in u.state : !('id_type' in u.state) && (ext === null || extensionOfState(u.state) === ext)) &&
        !reserved.has(outpointOf(u)),
    )
    .sort((a, b) => {
      const x = big(a.state.amount);
      const y = big(b.state.amount);
      if (x !== y) return x < y ? -1 : 1;
      if (a.transactionId !== b.transactionId) return a.transactionId < b.transactionId ? -1 : 1;
      return a.index - b.index;
    });
}

/**
 * The fresh inputs of each link of the chain over `sorted` (mergeCandidates order): the first up to `maxInputs`, each next up to `maxInputs - 1`,
 * at most `maxTxs` links, the merged amount never above `cap` (KRON's output limit). Fewer than two candidates: no link.
 */
export function mergeGroups<T extends { state: { amount: string } }>(sorted: readonly T[], maxInputs: number, maxTxs = MAX_MERGE_TXS, cap = I64_MAX): T[][] {
  const groups: T[][] = [];
  if (maxInputs < 2) return groups;
  let total = 0n;
  let at = 0;
  // the candidates the cap allows (smallest first, so the longest prefix)
  let usable = 0;
  for (const u of sorted) {
    if (total + big(u.state.amount) > cap) break;
    total += big(u.state.amount);
    usable++;
  }
  while (groups.length < maxTxs) {
    const first = groups.length === 0;
    const k = Math.min(first ? maxInputs : maxInputs - 1, usable - at);
    if (k < (first ? 2 : 1)) break;
    groups.push(sorted.slice(at, at + k));
    at += k;
  }
  return groups;
}

/** KAS the merged output carries: the largest input carrier, at most the default carrier, at least the program's floor. */
export function mergedCarrier(kob: KobWasm, token: Pick<MergeToken, 'templateHash'>, inputs: readonly TokenUtxo[]): bigint {
  const largest = inputs.reduce((m, u) => (big(u.amount) > m ? big(u.amount) : m), 0n);
  const floor = tokenCarrierFloor(kob, token.templateHash);
  const c = largest < DEFAULT_CARRIER ? largest : DEFAULT_CARRIER;
  return c < floor ? floor : c;
}

const INSUFFICIENT = /insufficient funds: need (\d+) sompi, have (\d+) sompi/;
const FUNDING_HEADROOM = 500_000n;

/**
 * One link: `inputs` (all of the wallet key, one token) into ONE output of the wallet key holding their whole amount. The freed carriers pay the
 * fee; when they cannot (or for KRON, which needs a P2PK input of the owner) wallet KAS UTXOs not in `spentFunding` are added.
 */
export function buildMergeLink(env0: MergeEnv, token: MergeToken, inputs: TokenUtxo[], fresh: TokenUtxo[], spentFunding: ReadonlySet<string> = new Set()): MergeLink | { error: PlanIssue } {
  const env = withUrgency(env0, 'low');
  const total = inputs.reduce((s, u) => s + big(u.state.amount), 0n);
  const carrier = mergedCarrier(env.kob, token, inputs);
  const kron = familyOfProgram(token.program) === 'kron';
  const pool = (env.funding ?? [])
    .filter((f) => f.pubkey === env.maker && !f.covenantId && !spentFunding.has(outpointOf(f)))
    .sort((a, b) => (big(b.amount) > big(a.amount) ? 1 : big(b.amount) < big(a.amount) ? -1 : 0));
  // KRON: the owner's address presence is a P2PK input (the smallest of at least 1 KAS, else the largest)
  const seed = kron ? ([...pool].reverse().find((f) => big(f.amount) >= ONE_KAS) ?? pool[0] ?? null) : null;
  if (kron && !seed) return { error: issue('merge.needs-funding', 'error', 'merging KRON tokens needs a KAS UTXO of the wallet in the transaction') };
  let used: KeyUtxo[] = seed ? [seed] : [];
  const make = (funding: KeyUtxo[]): SendTokensRequest => ({
    action: 'sendTokens',
    token: { covenantId: token.covenantId, program: token.program },
    tokens: inputs,
    recipients: [{ pubkey: env.maker, amount: total.toString(), carrier: carrier.toString() }],
    funding,
    change: env.maker,
    fee: { feeRate: env.feeRate != null ? env.feeRate.toString() : null, ...(env.feeMode ? { feeMode: env.feeMode } : {}) },
  });
  for (let attempt = 0; attempt < 8; attempt++) {
    try {
      const request = guardRequest(make(used), { maker: env.maker });
      const r = buildWithCap(env.fees?.policy, request, (q) => env.kob.build(q));
      recordFee(env, r.built, r.rate);
      const index = r.built.tx.outputs.findIndex((o) => o.covenant?.covenantId === token.covenantId);
      const state = keyState(familyOfProgram(token.program), total, env.maker, kron ? null : extensionOfState(inputs[0]!.state));
      const out = r.built.tx.outputs[index];
      if (!out || r.built.tx.outputs.filter((o) => o.covenant?.covenantId === token.covenantId).length !== 1 || out.scriptPublicKey !== env.kob.tokenScriptPublicKey(token.program, state)) {
        return { error: issue('merge.build-failed', 'error', 'the built transfer does not end in one token output of the wallet holding the whole amount') };
      }
      const issues = used.length > (seed ? 1 : 0) ? [issue('merge.funding-added', 'info', 'the freed carriers do not cover the network fee: a wallet KAS UTXO pays it', { inputs: used.length })] : [];
      return { inputs, fresh, built: r.built, request: r.request, fundingUsed: used, output: { index, carrier: big(out.value), state }, fee: big(r.built.fee.fee), issues };
    } catch (e) {
      const msg = e instanceof KobError || e instanceof Error ? e.message : String(e);
      const m = INSUFFICIENT.exec(msg);
      if (!m) return { error: issue('merge.build-failed', 'error', msg) };
      const deficit = big(m[1]!) - big(m[2]!) + FUNDING_HEADROOM;
      const rest = pool.filter((p) => !used.includes(p));
      if (!rest.length) return { error: issue('merge.insufficient-funds', 'error', `not enough KAS for the network fee: ${deficit - FUNDING_HEADROOM} sompi short`, { shortfall: deficit - FUNDING_HEADROOM }) };
      used = [...used, [...rest].reverse().find((p) => big(p.amount) >= deficit) ?? rest[0]!];
    }
  }
  return { error: issue('merge.insufficient-funds', 'error', 'could not fund the network fee within 8 attempts') };
}

/** The merged output of `link` as the next link's input, once its transaction `txid` is known (`node`: the node's view of that UTXO, when read). */
export function mergeCarry(link: MergeLink, txid: Hex, token: Pick<MergeToken, 'covenantId'>, node?: { amount: string; blockDaaScore?: string }): TokenUtxo {
  return {
    transactionId: txid,
    index: link.output.index,
    amount: node?.amount ?? link.output.carrier.toString(),
    ...(node?.blockDaaScore !== undefined ? { blockDaaScore: node.blockDaaScore } : {}),
    covenantId: token.covenantId,
    state: link.output.state,
  };
}

const failedPlan = (issues: PlanIssue[], before: number): MergePlan => ({ ok: false, issues, groups: [], links: [], before, after: before, amount: 0n, fee: 0n });

/**
 * The whole merge of `token` for the wallet key: the candidates, the chain's groups, and every link built (the later ones against the previous
 * link's provisional outpoint), so the count, the result and the fee are known before anything is signed.
 */
export function planMerge(env: MergeEnv, token: MergeToken, utxos: readonly TokenUtxo[], reserved: ReadonlySet<string> = new Set()): MergePlan {
  const cands = mergeCandidates(utxos, token, env.maker, reserved);
  const before = cands.length;
  const slots = mergeSlots(token);
  const cap = familyOfProgram(token.program) === 'kron' ? KRON_MAX_OUTPUT_AMOUNT : I64_MAX;
  const groups = mergeGroups(cands, slots, MAX_MERGE_TXS, cap);
  if (!groups.length) return failedPlan([issue('merge.nothing', 'error', 'fewer than two token UTXOs of this token can be merged', { count: before })], before);
  const links: MergeLink[] = [];
  const spent = new Set<string>();
  for (const g of groups) {
    const prev = links[links.length - 1];
    const inputs = prev ? [mergeCarry(prev, prev.built.tx.id, token), ...g] : g;
    const l = buildMergeLink(env, token, inputs, g, spent);
    if ('error' in l) return failedPlan([l.error], before);
    for (const f of l.fundingUsed) spent.add(outpointOf(f));
    links.push(l);
  }
  const merged = groups.reduce((n, g) => n + g.length, 0);
  const last = links[links.length - 1]!;
  return {
    ok: true,
    issues: links.flatMap((l) => l.issues),
    groups,
    links,
    before,
    after: before - merged + 1,
    amount: big(last.output.state.amount),
    fee: links.reduce((s, l) => s + l.fee, 0n),
  };
}
