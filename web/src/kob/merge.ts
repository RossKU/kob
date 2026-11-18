// Merges the wallet's own token UTXOs of one token, for "Merge all" in My orders > Balances
// and for the sell ticket's answer to TOKEN_UTXOS_FRAGMENTED. The module is pure and does no I/O.
//
// - The inputs are token UTXOs the wallet key owns by itself, meaning KCC-20 scheme 0 or KRON
//   address presence, and carry the token's extension commitment. A KRON minter UTXO is never an
//   input. An order's custody or stray is never an input, because the order's covenant id owns it.
//   Neither is an outpoint the caller reserves. Inputs go smallest first, and ties go by outpoint.
// - A KCC-20 borrow setting (scheme and guard) is kept. There is one tree per setting
//   (`groupByBorrow`, plain first), and each merged output carries its group's setting. UTXOs of
//   different settings never share a transaction.
// - A merge is a tree of `sendTokens` transfers to the wallet key, planned level by level. Level 1
//   groups the original UTXOs by the program's token inputs, 3 for KOB's standard KCC20Ref, so its
//   transactions spend only settled coins. Each later level merges the outputs of the level before
//   with the originals left over. A full merge ends in one UTXO with the fewest transactions,
//   ceil((n - 1) / (inputs - 1)). Each transaction is valid alone, so a run stopped part way leaves
//   a valid state, and the next run plans from it.
// - The target mode merges only as much as one placement needs. A placement takes at most
//   `inputs` token UTXOs, so the merge stops once the `inputs` largest resulting UTXOs cover the
//   amount. With t transactions, the `inputs + t * (inputs - 1)` largest UTXOs end in `inputs`
//   UTXOs.
// - The merged output keeps the largest input carrier, at most the default 1 KAS carrier and at
//   least the program's floor. The other carriers are freed and pay the fee, and the rest returns
//   as KAS change. A wallet KAS UTXO is added only when they do not cover the fee, or for a KRON
//   transfer, which needs a P2PK input of the owner.
// - The fee uses the low urgency of the fee policy, as housekeeping (matcher.md 9).
// - "Convert to plain" (`convert`) turns the wallet's holdings with a borrow setting into plain
//   ones. Each setting gets one tree, merged in that setting. Its last transaction sends the whole
//   amount to the wallet key as a plain recipient output, which is a `sendTokens` with the key as
//   its one recipient and no change. A setting held in one UTXO takes one transaction. Plain UTXOs
//   are never inputs.
//
// A transaction above level 1 spends outputs that exist only once their transactions are
// accepted. `planMerge` builds the whole tree against provisional outpoints, so the count, fee and
// result are known before anything is signed. A caller running the tree rebuilds each later
// transaction from the node's UTXOs once its parents are accepted, with `mergeCarry` and
// `buildMergeLink`.
import { buildWithCap, recordFee, withUrgency, type FeeContext, type RatePick } from './fee-policy';
import { MAX_TOKEN_INPUTS } from './guards';
import { DEFAULT_CARRIER, tokenCarrierFloor } from './orders/common';
import type { PlanIssue } from './plan-types';
import { guardRequest } from './request-guard';
import { KRON_MAX_OUTPUT_AMOUNT, PLAIN_BORROW, borrowKeyOf, extensionOfState, familyOfProgram, groupByBorrow, isSpendableHolding, keyState, withBorrowOf } from './token-state';
import type { BuiltTx, FeeMode, Hex, KeyUtxo, SendTokensRequest, TokenProgram, TokenState, TokenUtxo } from './types';
import { KobError, type KobWasm } from './wasm';

/**
 * The most transactions one merge plans, each with one signature. It covers a full merge of 101
 * UTXOs on a 3-input program. A larger set merges its smallest UTXOs in one run and the rest in
 * the next.
 */
export const MAX_MERGE_TXS = 50;
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

/** One built transfer of a merge tree. */
export interface MergeLink {
  /** The link's token inputs: the outputs of its parents first, then its fresh inputs. */
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
  /** The tree level, from 1. Level 1 spends only the wallet's original UTXOs. */
  level: number;
  /** The earlier links whose outputs this link spends, in input order. */
  parents: number[];
  /** The output is plain, as the last link of a conversion. Else it keeps the inputs' setting. */
  plain: boolean;
}

/** One transaction of a planned tree, before it is built. */
export interface MergeNode<T> {
  level: number;
  /** The earlier nodes whose outputs this one spends. */
  parents: number[];
  /** The original UTXOs it spends. */
  fresh: T[];
  /** Its output is plain, as the last transaction of a conversion (`MergeOptions.convert`). */
  plain?: boolean;
}

/** The result of `planMerge`: the groups, the built tree and what the merge yields. */
export interface MergePlan {
  ok: boolean;
  issues: PlanIssue[];
  /** The wallet's original UTXOs each link merges. */
  groups: TokenUtxo[][];
  /**
   * The tree built against provisional outpoints, level by level. A link spends only outputs of
   * links before it.
   */
  links: MergeLink[];
  /** The number of levels. */
  levels: number;
  /** The amount the target mode plans for, or null for a full merge. */
  target: bigint | null;
  /** The number of mergeable UTXOs before the planned links. */
  before: number;
  /** The number of borrow settings the planned links merge, one tree each. Plain alone is 1. */
  settings: number;
  /**
   * A conversion to plain (`MergeOptions.convert`). Then `before` counts the UTXOs with a borrow
   * setting, and each tree ends in a plain UTXO.
   */
  convert: boolean;
  /**
   * The number of mergeable UTXOs after them. The merged outputs count, because they merge again.
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
  /** Base units of the original UTXOs the links merge. A full merge puts them in one output. */
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
 * They are token UTXOs of `token` that the wallet key owns by itself, of any KCC-20 borrow setting
 * (`isSpendableHolding`). They carry the token's extension commitment and a positive amount, and
 * are not in `reserved`, a set of `txid:index`. `mergeableGroups` splits them by setting.
 */
export function mergeCandidates(utxos: readonly TokenUtxo[], token: Pick<MergeToken, 'covenantId' | 'program' | 'extensionCommitment'>, maker: Hex, reserved: ReadonlySet<string> = new Set()): TokenUtxo[] {
  const kron = familyOfProgram(token.program) === 'kron';
  const ext = kron ? null : token.extensionCommitment;
  return utxos
    .filter(
      (u) =>
        u.covenantId === token.covenantId &&
        isSpendableHolding(u.state, token.program) &&
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
 * Returns `sorted` (smallest first) per borrow setting, each group in that order. The plain group
 * comes first, then the others by total, largest first. Only groups of two or more can merge.
 */
export const mergeableGroups = <T extends TokenUtxo>(sorted: readonly T[]): T[][] => groupByBorrow(sorted).filter((g) => g.length >= 2);

/** The number of UTXOs a merge may take, those in a borrow setting group of two or more. */
export const mergeableCount = (sorted: readonly TokenUtxo[]): number => mergeableGroups(sorted).reduce((n, g) => n + g.length, 0);

/**
 * Returns the longest prefix of `sorted` whose total stays within `cap`, KRON's output limit.
 * Every output of a full merge holds at most the total, so a prefix within the cap is safe, and
 * smallest first makes it the longest one.
 */
function withinCap<T extends { state: { amount: string } }>(sorted: readonly T[], cap: bigint): T[] {
  let total = 0n;
  let usable = 0;
  for (const u of sorted) {
    if (total + big(u.state.amount) > cap) break;
    total += big(u.state.amount);
    usable++;
  }
  return sorted.slice(0, usable);
}

/**
 * Plans the tree that merges `items` until at most `keep` UTXOs are left, level by level.
 * Each level takes groups of `maxInputs` in order, and what does not fill a group waits for the
 * next level behind the new outputs. A group smaller than `maxInputs` forms only as the last
 * transaction, or when a level holds fewer than `maxInputs`. Every transaction but the last then
 * removes `maxInputs - 1` UTXOs, which is the fewest transactions. Level 1 holds only originals.
 */
export function mergeTree<T>(items: readonly T[], maxInputs: number, keep = 1): MergeNode<T>[] {
  const nodes: MergeNode<T>[] = [];
  if (maxInputs < 2 || keep < 1) return nodes;
  type Item = { utxo: T } | { node: number };
  let level: Item[] = items.map((utxo) => ({ utxo }));
  for (let depth = 1; level.length > keep; depth++) {
    // The UTXOs still to remove, at maxInputs - 1 per full group.
    let rest = level.length - keep;
    const next: Item[] = [];
    let at = 0;
    let formed = 0;
    while (at < level.length && rest > 0) {
      const left = level.length - at;
      const want = Math.min(maxInputs, rest + 1);
      // A short group at the end of a level waits, unless it is the level's only group.
      const size = want <= left ? want : formed === 0 ? left : 0;
      if (size < 2) break;
      const group = level.slice(at, at + size);
      nodes.push({
        level: depth,
        parents: group.flatMap((x) => ('node' in x ? [x.node] : [])),
        fresh: group.flatMap((x) => ('utxo' in x ? [x.utxo] : [])),
      });
      next.push({ node: nodes.length - 1 });
      rest -= size - 1;
      at += size;
      formed++;
    }
    if (formed === 0) break;
    level = [...next, ...level.slice(at)];
  }
  return nodes;
}

/** The number of transactions a full merge of `n` UTXOs takes with `maxInputs` token inputs. */
export const mergeTxCount = (n: number, maxInputs: number): number => (n < 2 || maxInputs < 2 ? 0 : Math.ceil((n - 1) / (maxInputs - 1)));

/**
 * Returns the UTXOs the target mode merges, in `sorted` order, or why there are none.
 * A placement takes at most `slots` token inputs. With t transactions the `slots` largest
 * resulting UTXOs hold the `slots + t * (slots - 1)` largest originals, so the smallest t whose
 * originals cover `target` is the fewest transactions.
 */
export function targetSet<T extends { state: { amount: string } }>(sorted: readonly T[], slots: number, target: bigint): { take: T[] } | { code: 'merge.not-needed' | 'merge.short' } {
  const desc = [...sorted].reverse();
  const top = (k: number) => desc.slice(0, k).reduce((s, u) => s + big(u.state.amount), 0n);
  if (top(slots) >= target) return { code: 'merge.not-needed' };
  if (slots < 2 || top(desc.length) < target) return { code: 'merge.short' };
  let k = slots;
  while (k < desc.length && top(k) < target) k += slots - 1;
  const take = new Set(desc.slice(0, Math.min(k, desc.length)));
  return { take: sorted.filter((u) => take.has(u)) };
}

/** What `planMergeShape` and `planMerge` take besides the UTXOs. */
export interface MergeOptions {
  /**
   * The target mode. It merges only until the program's input count of largest UTXOs covers this
   * amount, in base units. Omitted, the merge is full.
   */
  target?: bigint;
  /**
   * Convert to plain. The UTXOs with a borrow setting become plain ones, one per setting, and plain
   * UTXOs are left as they are. With `target`, only the settings needed for the plain UTXOs to hold
   * that amount are converted, largest first.
   */
  convert?: boolean;
}

/**
 * The planned tree before it is built, or the issue that stops it. `settings` counts the borrow
 * settings it merges, one tree each.
 */
export type MergeShape = { ok: true; before: number; settings: number; nodes: MergeNode<TokenUtxo>[] } | { ok: false; before: number; issue: PlanIssue };

/**
 * Joins the trees of several groups into one list ordered by level, so that every node still
 * spends only outputs of nodes before it. Parent indices are renumbered.
 */
export function joinTrees<T>(trees: MergeNode<T>[][]): MergeNode<T>[] {
  const all = trees.flatMap((nodes, g) => nodes.map((n, i) => ({ n, g, i })));
  all.sort((a, b) => a.n.level - b.n.level || a.g - b.g || a.i - b.i);
  const at = new Map(all.map((x, k) => [`${x.g}:${x.i}`, k]));
  return all.map(({ n, g }) => ({ ...n, parents: n.parents.map((p) => at.get(`${g}:${p}`)!) }));
}

/**
 * Plans the tree of `token` for the wallet key without building it. This is what a button states
 * before the click: the transaction count and the UTXOs merged.
 */
export function planMergeShape(token: MergeToken, utxos: readonly TokenUtxo[], maker: Hex, reserved: ReadonlySet<string> = new Set(), o: MergeOptions = {}): MergeShape {
  const cands = mergeCandidates(utxos, token, maker, reserved);
  const before = cands.length;
  const slots = mergeSlots(token);
  const cap = familyOfProgram(token.program) === 'kron' ? KRON_MAX_OUTPUT_AMOUNT : I64_MAX;
  const fail = (code: string, message: string, params?: PlanIssue['params']): MergeShape => ({ ok: false, before, issue: issue(code, 'error', message, params) });
  if (o.convert) return convertShape(cands, slots, o.target ?? null, fail);
  if (o.target === undefined) {
    // One tree per borrow setting, plain first, within MAX_MERGE_TXS in all.
    const trees: MergeNode<TokenUtxo>[][] = [];
    let budget = MAX_MERGE_TXS;
    for (const g of mergeableGroups(cands)) {
      if (budget <= 0) break;
      const nodes = mergeTree(withinCap(g, cap).slice(0, 1 + budget * (slots - 1)), slots);
      if (!nodes.length) continue;
      trees.push(nodes);
      budget -= nodes.length;
    }
    if (!trees.length) return fail('merge.nothing', 'fewer than two token UTXOs of this token can be merged', { count: before });
    // A UTXO alone in its borrow setting cannot merge, so it does not count as mergeable.
    return { ok: true, before: mergeableCount(cands), settings: trees.length, nodes: joinTrees(trees) };
  }
  // The target mode merges in the one setting the sale spends. That is the first group (plain
  // first) that holds the amount, as `selectTokens` picks it.
  const target = o.target;
  const group = groupByBorrow(cands).find((g) => g.reduce((n, u) => n + big(u.state.amount), 0n) >= target);
  const picked = group ? targetSet(group, slots, target) : ({ code: 'merge.short' } as const);
  if ('code' in picked) {
    return picked.code === 'merge.not-needed'
      ? fail('merge.not-needed', 'the largest token UTXOs already cover the amount: no merge is needed')
      : fail('merge.short', 'the mergeable token UTXOs do not hold the amount', { count: before });
  }
  const nodes = mergeTree(picked.take, slots, slots);
  if (nodes.length > MAX_MERGE_TXS) return fail('merge.too-many', `the amount needs ${nodes.length} merge transactions, more than ${MAX_MERGE_TXS}`, { txs: nodes.length, max: MAX_MERGE_TXS });
  // Every output holds what its subtree merges, and a KRON output may not exceed the cap.
  const held: bigint[] = [];
  for (const n of nodes) {
    const amount = n.parents.reduce((s, p) => s + held[p]!, 0n) + n.fresh.reduce((s, u) => s + big(u.state.amount), 0n);
    if (amount > cap) return fail('merge.over-cap', "a merged UTXO would exceed the token's per-output limit", { cap });
    held.push(amount);
  }
  return { ok: true, before, settings: 1, nodes };
}

/**
 * Plans the conversion tree. Each borrow setting, largest first, gets a tree that merges it in its
 * setting, and the tree's last node has a plain output. A setting in one UTXO is one node.
 * - With `target`, it takes only the settings the plain UTXOs need to hold that amount.
 * - Without, it takes every setting that fits in MAX_MERGE_TXS transactions. A setting that does
 *   not fit waits for the next run.
 */
function convertShape(cands: readonly TokenUtxo[], slots: number, target: bigint | null, fail: (code: string, message: string, params?: PlanIssue['params']) => MergeShape): MergeShape {
  const groups = groupByBorrow(cands);
  const plainGroup = groups.find((g) => borrowKeyOf(g[0]!.state) === PLAIN_BORROW) ?? [];
  const borrowed = groups.filter((g) => g !== plainGroup);
  const total = (g: readonly TokenUtxo[]) => g.reduce((n, u) => n + big(u.state.amount), 0n);
  if (!borrowed.length) return fail('merge.no-borrow', 'no token UTXO of this token has a borrow setting');
  let take = borrowed;
  if (target !== null) {
    let have = total(plainGroup);
    if (have >= target) return fail('merge.not-needed', 'the plain token UTXOs already hold the amount: no conversion is needed');
    take = [];
    for (const g of borrowed) {
      if (have >= target) break;
      take.push(g);
      have += total(g);
    }
    if (have < target) return fail('merge.short', 'the token UTXOs do not hold the amount', { count: cands.length });
  }
  const trees: MergeNode<TokenUtxo>[][] = [];
  let budget = MAX_MERGE_TXS;
  for (const g of take) {
    const nodes = g.length === 1 ? [{ level: 1, parents: [], fresh: [...g] }] : mergeTree(g, slots);
    if (nodes.length > budget) {
      if (target !== null) return fail('merge.too-many', `the amount needs more than ${MAX_MERGE_TXS} conversion transactions`, { txs: MAX_MERGE_TXS + 1, max: MAX_MERGE_TXS });
      break;
    }
    nodes[nodes.length - 1] = { ...nodes[nodes.length - 1]!, plain: true };
    trees.push(nodes);
    budget -= nodes.length;
  }
  if (!trees.length) return fail('merge.too-many', `a conversion needs more than ${MAX_MERGE_TXS} transactions`, { txs: MAX_MERGE_TXS + 1, max: MAX_MERGE_TXS });
  return { ok: true, before: borrowed.reduce((n, g) => n + g.length, 0), settings: trees.length, nodes: joinTrees(trees) };
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
 * `spentFunding` are added. `at` is the link's place in the tree.
 */
export function buildMergeLink(
  env0: MergeEnv, token: MergeToken, inputs: TokenUtxo[], fresh: TokenUtxo[], spentFunding: ReadonlySet<string> = new Set(),
  at: Pick<MergeNode<TokenUtxo>, 'level' | 'parents' | 'plain'> = { level: 1, parents: [] },
): MergeLink | { error: PlanIssue } {
  const plain = at.plain === true;
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
  // Without a recipient, the whole amount is the wallet's token change, which keeps the inputs' one
  // borrow setting. The builder refuses inputs of different settings. A conversion's last link
  // instead names the wallet key as its one recipient of the whole amount. A recipient output is
  // plain, and there is no change.
  const make = (funding: KeyUtxo[]): SendTokensRequest => ({
    action: 'sendTokens',
    token: { covenantId: token.covenantId, program: token.program },
    tokens: inputs,
    recipients: plain ? [{ pubkey: env.maker, amount: total.toString(), carrier: carrier.toString() }] : [],
    tokenChange: env.maker,
    tokenChangeCarrier: carrier.toString(),
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
      const held = keyState(familyOfProgram(token.program), total, env.maker, kron ? null : extensionOfState(inputs[0]!.state), token.program);
      const state = plain ? held : withBorrowOf(held, inputs[0]!.state);
      const out = r.built.tx.outputs[index];
      if (!out || r.built.tx.outputs.filter((o) => o.covenant?.covenantId === token.covenantId).length !== 1 || out.scriptPublicKey !== env.kob.tokenScriptPublicKey(token.program, state)) {
        return { error: issue('merge.build-failed', 'error', 'the built transfer does not end in one token output of the wallet holding the whole amount') };
      }
      const issues = used.length > (seed ? 1 : 0) ? [issue('merge.funding-added', 'info', 'the freed carriers do not cover the network fee: a wallet KAS UTXO pays it', { inputs: used.length })] : [];
      return { inputs, fresh, built: r.built, request: r.request, fundingUsed: used, output: { index, carrier: big(out.value), state }, fee: big(r.built.fee.fee), issues, level: at.level, parents: [...at.parents], plain };
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
 * Returns the merged output of `link` as an input of a later link, once its transaction `txid` is
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

const failedPlan = (issues: PlanIssue[], before: number): MergePlan => ({ ok: false, issues, groups: [], links: [], levels: 0, target: null, before, settings: 0, convert: false, after: before, merged: 0, freed: 0n, amount: 0n, fee: 0n });

/**
 * Plans the merge of `token` for the wallet key.
 * It plans the tree (`planMergeShape`) and builds every link, the later ones against their
 * parents' provisional outpoints. The count, result, fee and freed KAS are therefore known before
 * anything is signed.
 */
export function planMerge(env: MergeEnv, token: MergeToken, utxos: readonly TokenUtxo[], reserved: ReadonlySet<string> = new Set(), o: MergeOptions = {}): MergePlan {
  const shape = planMergeShape(token, utxos, env.maker, reserved, o);
  if (!shape.ok) return failedPlan([shape.issue], shape.before);
  const links: MergeLink[] = [];
  const spent = new Set<string>();
  for (const n of shape.nodes) {
    const carried = n.parents.map((p) => mergeCarry(links[p]!, links[p]!.built.tx.id, token));
    const l = buildMergeLink(env, token, [...carried, ...n.fresh], n.fresh, spent, n);
    if ('error' in l) return failedPlan([l.error], shape.before);
    for (const f of l.fundingUsed) spent.add(outpointOf(f));
    links.push(l);
  }
  return summarizeMerge(links, shape.before, o.target ?? null, shape.settings, o.convert === true);
}

/** Returns the plan of the built `links`, with `before` mergeable UTXOs before them. */
function summarizeMerge(links: MergeLink[], before: number, target: bigint | null = null, settings = 1, convert = false): MergePlan {
  const groups = links.map((l) => l.fresh);
  const fee = links.reduce((s, l) => s + l.fee, 0n);
  // Each link frees its inputs' carriers less the one its output keeps. A carrier kept by a link
  // that a later link spends is freed there.
  const carriersIn = links.reduce((s, l) => s + l.inputs.reduce((t, u) => t + big(u.amount), 0n), 0n);
  const carriersKept = links.reduce((s, l) => s + l.output.carrier, 0n);
  return {
    ok: links.length > 0,
    issues: links.flatMap((l) => l.issues),
    groups,
    links,
    levels: links.reduce((m, l) => Math.max(m, l.level), 0),
    target,
    before,
    settings,
    convert,
    // For a conversion, the UTXOs with a borrow setting it leaves. Each tree ends in a plain one.
    after: before - links.reduce((n, l) => n + l.inputs.length - 1, 0) - (convert ? links.filter((l) => l.plain).length : 0),
    merged: groups.reduce((n, g) => n + g.length, 0),
    freed: carriersIn - carriersKept - fee,
    amount: groups.flat().reduce((s, u) => s + big(u.state.amount), 0n),
    fee,
  };
}

/** Returns the links no later link spends. Their outputs are what the merge leaves. */
export function mergeLeaves(links: readonly Pick<MergeLink, 'parents'>[]): number[] {
  const spent = new Set(links.flatMap((l) => l.parents));
  return links.map((_, i) => i).filter((i) => !spent.has(i));
}
