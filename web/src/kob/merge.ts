// Merges the wallet's own plain token UTXOs of one token into one, for "Merge tokens", which is the
// UI answer to TOKEN_UTXOS_FRAGMENTED. The module is pure and does no I/O.
//
// - The inputs are token UTXOs the wallet key owns by itself, meaning KCC-20 scheme 0 or KRON
//   address presence. They are plain, with no borrow or minter flag, and carry the token's
//   extension commitment. An order's custody or stray is never an input, because the order's
//   covenant id owns it. Neither is an outpoint the caller reserves. Inputs go smallest first, and
//   ties go by outpoint.
// - A merge is a chain of `sendTokens` transfers to the wallet key. The first link takes up to the
//   program's token inputs, 3 for KOB's standard KCC20Ref. Every next link takes the previous
//   output plus up to inputs - 1 more, so one pass ends with one UTXO. Each link is valid alone.
// - `maxLinks` of `planMerge` bounds the chain. The web app plans exactly one link per click,
//   because a merge is manual and every link costs a fee. The user decides each one.
// - The merged output keeps the largest input carrier, at most the default 1 KAS carrier and at
//   least the program's floor. The other carriers are freed and pay the fee, and the rest returns
//   as KAS change. A wallet KAS UTXO is added only when they do not cover the fee, or for a KRON
//   transfer, which needs a P2PK input of the owner.
// - The fee uses the low urgency of the fee policy, as housekeeping (matcher.md 9).
//
// A link after the first spends an output that exists only once the previous link is accepted.
// `planMerge` builds the whole chain against provisional outpoints, so the count, fee and result
// are known before anything is signed. A caller running the chain rebuilds each later link from the
// node's UTXO after the previous one is accepted, with `mergeCarry` and `buildMergeLink`.
import { buildWithCap, recordFee, withUrgency, type FeeContext, type RatePick } from './fee-policy';
import { MAX_TOKEN_INPUTS } from './guards';
import { DEFAULT_CARRIER, tokenCarrierFloor } from './orders/common';
import type { PlanIssue } from './plan-types';
import { guardRequest } from './request-guard';
import { KRON_MAX_OUTPUT_AMOUNT, extensionOfState, familyOfProgram, isKeyOwned, isPlainState, keyState } from './token-state';
import type { BuiltTx, FeeMode, Hex, KeyUtxo, SendTokensRequest, TokenProgram, TokenState, TokenUtxo } from './types';
import { KobError, type KobWasm } from './wasm';

/**
 * The most transactions one merge runs, each with one signature. A larger set takes several runs.
 */
export const MAX_MERGE_TXS = 10;
const I64_MAX = (1n << 63n) - 1n;
const ONE_KAS = 100_000_000n;

/** The token to merge. A registry `TokenInfo` fits. */
export interface MergeToken {
  covenantId: Hex;
  program: TokenProgram;
  templateHash: Hex;
  /**
   * For KCC-20, the extension commitment, or null to accept any that all inputs share. KRON ignores
   * it.
   */
  extensionCommitment: Hex | null;
  slots: { inputs: number };
}

/** What the merge builder needs. A `CancelEnv` fits. */
export interface MergeEnv {
  kob: KobWasm;
  /** The wallet's x-only key, owner of the inputs and of the merged output. */
  maker: Hex;
  /**
   * P2PK KAS UTXOs of the wallet, used only when the freed carriers cannot pay the fee, and for
   * KRON.
   */
  funding?: KeyUtxo[];
  feeRate?: bigint;
  feeMode?: FeeMode;
  fees?: FeeContext;
  feePick?: RatePick;
}

/** One built transfer of a merge chain. */
export interface MergeLink {
  /** The link's token inputs. From the second link on, the previous link's output comes first. */
  inputs: TokenUtxo[];
  /** The inputs of the wallet's original set that this link merges. */
  fresh: TokenUtxo[];
  built: BuiltTx;
  request: SendTokensRequest;
  fundingUsed: KeyUtxo[];
  /** The merged output. */
  output: { index: number; carrier: bigint; state: TokenState };
  fee: bigint;
  issues: PlanIssue[];
}

/** The result of `planMerge`: the groups, the built chain and what the merge yields. */
export interface MergePlan {
  ok: boolean;
  issues: PlanIssue[];
  /**
   * The wallet's original UTXOs each link merges. Link 0 takes up to `inputs`, then up to `inputs -
   * 1`.
   */
  groups: TokenUtxo[][];
  /** The chain built against provisional outpoints. Link k above 0 spends link k - 1's output. */
  links: MergeLink[];
  /** The number of mergeable UTXOs before the planned links. */
  before: number;
  /**
   * The number of mergeable UTXOs after them. The merged output counts, because it merges again.
   */
  after: number;
  /** The number of the wallet's original UTXOs the planned links merge. */
  merged: number;
  /**
   * KAS the merge returns to the wallet.
   * It is the carriers of the merged token UTXOs, less the one the merged output keeps, less the
   * fees. Freeing carriers is the point of a merge, since each token UTXO carries one, by default
   * 1 KAS. A wallet KAS input added for the fee comes back as change, so it is not counted.
   * It is negative when the fees exceed the freed carriers, as with tiny carriers or KRON.
   */
  freed: bigint;
  /** Base units merged into the final output. */
  amount: bigint;
  /** The sum of the links' fees. The signed transactions pay at most this. */
  fee: bigint;
}

const big = (v: string | number | bigint): bigint => BigInt(v);
/** Returns a UTXO outpoint as `txid:index`. */
export const outpointOf = (u: { transactionId: string; index: number }): string => `${u.transactionId}:${u.index}`;
const issue = (code: string, severity: PlanIssue['severity'], message: string, params?: PlanIssue['params']): PlanIssue => ({ code, severity, message, ...(params ? { params } : {}) });

/** Returns the token inputs one link may take, the program's slots capped at the covenant limit. */
export const mergeSlots = (token: Pick<MergeToken, 'slots'>): number => Math.min(token.slots.inputs, MAX_TOKEN_INPUTS);

/**
 * Returns the UTXOs a merge may spend, smallest first and ties by outpoint.
 * They are plain token UTXOs of `token` that the wallet key owns by itself. They carry the token's
 * extension commitment and a positive amount, and are not in `reserved`, a set of `txid:index`.
 */
export function mergeCandidates(utxos: readonly TokenUtxo[], token: Pick<MergeToken, 'covenantId' | 'program' | 'extensionCommitment'>, maker: Hex, reserved: ReadonlySet<string> = new Set()): TokenUtxo[] {
  const kron = familyOfProgram(token.program) === 'kron';
  const ext = kron ? null : token.extensionCommitment;
  return utxos
    .filter(
      (u) =>
        u.covenantId === token.covenantId &&
        isKeyOwned(u.state, token.program) &&
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
 * Splits `sorted` into the fresh inputs of each link of the chain.
 * `sorted` is in `mergeCandidates` order. The first link takes up to `maxInputs` and each next one
 * up to `maxInputs - 1`. There are at most `maxTxs` links, and the merged amount never exceeds
 * `cap`, which is KRON's output limit. Fewer than two candidates give no link.
 */
export function mergeGroups<T extends { state: { amount: string } }>(sorted: readonly T[], maxInputs: number, maxTxs = MAX_MERGE_TXS, cap = I64_MAX): T[][] {
  const groups: T[][] = [];
  if (maxInputs < 2) return groups;
  let total = 0n;
  let at = 0;
  // The cap allows a prefix of the candidates. Smallest first makes it the longest one.
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

/**
 * Returns the KAS the merged output carries, the largest input carrier within the default and the
 * floor.
 */
export function mergedCarrier(kob: KobWasm, token: Pick<MergeToken, 'templateHash'>, inputs: readonly TokenUtxo[]): bigint {
  const largest = inputs.reduce((m, u) => (big(u.amount) > m ? big(u.amount) : m), 0n);
  const floor = tokenCarrierFloor(kob, token.templateHash);
  const c = largest < DEFAULT_CARRIER ? largest : DEFAULT_CARRIER;
  return c < floor ? floor : c;
}

const INSUFFICIENT = /insufficient funds: need (\d+) sompi, have (\d+) sompi/;
const FUNDING_HEADROOM = 500_000n;

/**
 * Builds one link, which merges `inputs` into one output of the wallet key holding their whole
 * amount. All inputs are of the wallet key and one token. The freed carriers pay the fee. When they
 * cannot, or for KRON, which needs a P2PK input of the owner, wallet KAS UTXOs not in
 * `spentFunding` are added.
 */
export function buildMergeLink(env0: MergeEnv, token: MergeToken, inputs: TokenUtxo[], fresh: TokenUtxo[], spentFunding: ReadonlySet<string> = new Set()): MergeLink | { error: PlanIssue } {
  const env = withUrgency(env0, 'low');
  const total = inputs.reduce((s, u) => s + big(u.state.amount), 0n);
  const carrier = mergedCarrier(env.kob, token, inputs);
  const kron = familyOfProgram(token.program) === 'kron';
  const pool = (env.funding ?? [])
    .filter((f) => f.pubkey === env.maker && !f.covenantId && !spentFunding.has(outpointOf(f)))
    .sort((a, b) => (big(b.amount) > big(a.amount) ? 1 : big(b.amount) < big(a.amount) ? -1 : 0));
  // KRON needs a P2PK input of the owner for address presence. Take the smallest of at least 1 KAS,
  // else the largest.
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
      const state = keyState(familyOfProgram(token.program), total, env.maker, kron ? null : extensionOfState(inputs[0]!.state), token.program);
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

/**
 * Returns the merged output of `link` as the next link's input, once its transaction `txid` is
 * known. `node` is the node's view of that UTXO, when it was read.
 */
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

const failedPlan = (issues: PlanIssue[], before: number): MergePlan => ({ ok: false, issues, groups: [], links: [], before, after: before, merged: 0, freed: 0n, amount: 0n, fee: 0n });

/**
 * Plans the merge of `token` for the wallet key.
 * It picks the candidates, forms the chain's groups of at most `maxLinks` and builds every link,
 * the later ones against the previous link's provisional outpoint. The count, result, fee and
 * freed KAS are therefore known before anything is signed. With `maxLinks = 1`, as in the web app,
 * it is one transaction of the smallest candidates into one output, and `after` says how many
 * mergeable UTXOs are left for the next click.
 */
export function planMerge(env: MergeEnv, token: MergeToken, utxos: readonly TokenUtxo[], reserved: ReadonlySet<string> = new Set(), maxLinks: number = MAX_MERGE_TXS): MergePlan {
  const cands = mergeCandidates(utxos, token, env.maker, reserved);
  const before = cands.length;
  const slots = mergeSlots(token);
  const cap = familyOfProgram(token.program) === 'kron' ? KRON_MAX_OUTPUT_AMOUNT : I64_MAX;
  const groups = mergeGroups(cands, slots, Math.max(1, Math.min(maxLinks, MAX_MERGE_TXS)), cap);
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
  const fee = links.reduce((s, l) => s + l.fee, 0n);
  const carriers = groups.flat().reduce((s, u) => s + big(u.amount), 0n);
  return {
    ok: true,
    issues: links.flatMap((l) => l.issues),
    groups,
    links,
    before,
    after: before - merged + 1,
    merged,
    freed: carriers - last.output.carrier - fee,
    amount: big(last.output.state.amount),
    fee,
  };
}
