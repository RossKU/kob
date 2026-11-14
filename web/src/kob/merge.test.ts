// "Merge all" and the sell ticket's merge: the candidates (only the wallet's own plain UTXOs of the token, never an order's custody / stray or a
// reserved outpoint), the tree's shape on the standard 3 / 3 program (level 1 groups the originals in threes, later levels merge the outputs)
// and on the 8 / 8 prototype, the target mode (just enough for one placement), the KRON output cap, and every link built by kob-wasm, signed
// locally, run through the script engine and decoded on the confirmation model (a send to the wallet itself, nothing blocking).
import { describe, expect, it } from 'vitest';
import { decodeSigning } from './decode';
import {
  MAX_MERGE_TXS, buildMergeLink, mergeCandidates, mergeCarry, mergeLeaves, mergeSlots, mergeTree, mergeTxCount, planMerge, planMergeShape, targetSet,
  type MergeNode, type MergeToken,
} from './merge';
import { KRON_MAX_OUTPUT_AMOUNT, keyState } from './token-state';
import { parseRegistry } from './registry';
import type { Hex, TokenUtxo } from './types';
import { loadKobNode } from './wasm.node';
import { MAKER, OTHER, TOKEN, keyUtxo, nodeFactsOf, signAndValidate, tokenUtxo, tradableRegistryJson } from '../testing/chain-fixtures';

const kob = loadKobNode();
const registry = parseRegistry(tradableRegistryJson(), { kob });
const KAS = 100_000_000n;
const slotsOf = (program: string) => {
  const t = kob.templates().find((x) => x.name === program)!;
  return { inputs: t.tokenSlots![0], outputs: t.tokenSlots![1] };
};
const STD: MergeToken = { covenantId: TOKEN.covenantId, program: TOKEN.program, templateHash: TOKEN.templateHash, extensionCommitment: TOKEN.ext, slots: slotsOf('KCC20Ref') };
const kronTpl = kob.templates().find((x) => x.name === 'KronToken2433')!;
const KRON: MergeToken = { covenantId: 'e6'.repeat(32), program: 'KronToken2433', templateHash: kronTpl.hash, extensionCommitment: null, slots: slotsOf('KronToken2433') };
const env = (funding = [keyUtxo(MAKER.pk, 50n * KAS, 201)]) => ({ kob, maker: MAKER.pk, funding });
/** `n` UTXOs of the maker: amounts 10, 20, ... base units, outpoints 0x40.. */
const utxos = (n: number, carrier = 2n * KAS): TokenUtxo[] => Array.from({ length: n }, (_, i) => tokenUtxo(BigInt((i + 1) * 10), MAKER.pk, 0x40 + i, carrier));
const kronUtxos = (amounts: bigint[]): TokenUtxo[] =>
  amounts.map((a, i) => ({
    transactionId: (0xa0 + i).toString(16).repeat(32), index: 0, amount: (2n * KAS).toString(), blockDaaScore: '500', covenantId: KRON.covenantId,
    state: keyState('kron', a, MAKER.pk, null),
  }));
const sum = (xs: TokenUtxo[]) => xs.reduce((s, u) => s + BigInt(u.state.amount), 0n);
const ids = (n: number) => Array.from({ length: n }, (_, i) => i);
/** The UTXOs a tree leaves: the originals it does not spend plus the outputs no later node spends. */
const left = (n: number, nodes: MergeNode<number>[]) => n - nodes.reduce((s, x) => s + x.fresh.length, 0) + mergeLeaves(nodes).length;
const levels = (nodes: MergeNode<unknown>[]) => nodes.reduce<Record<number, number>>((m, x) => ({ ...m, [x.level]: (m[x.level] ?? 0) + 1 }), {});

describe('merge candidates', () => {
  it('only plain UTXOs the wallet key owns, of the token and its extension commitment, not reserved; smallest first', () => {
    const mine = [tokenUtxo(30n, MAKER.pk, 0x51), tokenUtxo(10n, MAKER.pk, 0x52), tokenUtxo(20n, MAKER.pk, 0x53)];
    const other = tokenUtxo(5n, OTHER.pk, 0x54);
    const custody = tokenUtxo(5n, 'ab'.repeat(32), 0x55, 1_000_000_000n, 4);
    const otherExt = tokenUtxo(5n, MAKER.pk, 0x56, 1_000_000_000n, 0, 'cc'.repeat(32));
    const otherToken = { ...tokenUtxo(5n, MAKER.pk, 0x57), covenantId: '71'.repeat(32) };
    const empty = tokenUtxo(0n, MAKER.pk, 0x58);
    const borrowed = { ...tokenUtxo(5n, MAKER.pk, 0x59), state: { ...tokenUtxo(5n, MAKER.pk).state, borrow_scheme: 1 } } as TokenUtxo;
    const reserved = tokenUtxo(7n, MAKER.pk, 0x5a);
    const all = [...mine, other, custody, otherExt, otherToken, empty, borrowed, reserved];
    const got = mergeCandidates(all, STD, MAKER.pk, new Set([`${reserved.transactionId}:${reserved.index}`]));
    expect(got.map((u) => u.state.amount)).toEqual(['10', '20', '30']);
  });
});

describe('tree shape (3 / 3)', () => {
  it('2 and 3 UTXOs: one transaction', () => {
    expect(mergeTree(ids(2), 3)).toEqual([{ level: 1, parents: [], fresh: [0, 1] }]);
    expect(mergeTree(ids(3), 3)).toEqual([{ level: 1, parents: [], fresh: [0, 1, 2] }]);
    expect(mergeTree(ids(1), 3)).toEqual([]);
  });

  it('4 UTXOs: 3 -> 1 on level 1, then that output with the fourth on level 2', () => {
    expect(mergeTree(ids(4), 3)).toEqual([
      { level: 1, parents: [], fresh: [0, 1, 2] },
      { level: 2, parents: [0], fresh: [3] },
    ]);
  });

  it('7 UTXOs: two 3 -> 1 on level 1 (settled coins only), then both outputs with the seventh', () => {
    expect(mergeTree(ids(7), 3)).toEqual([
      { level: 1, parents: [], fresh: [0, 1, 2] },
      { level: 1, parents: [], fresh: [3, 4, 5] },
      { level: 2, parents: [0, 1], fresh: [6] },
    ]);
  });

  it('30 UTXOs: 15 transactions over 4 levels (10, 3, 1, 1), every UTXO and every output spent once, one UTXO left', () => {
    const tree = mergeTree(ids(30), 3);
    expect(tree).toHaveLength(15);
    expect(levels(tree)).toEqual({ 1: 10, 2: 3, 3: 1, 4: 1 });
    // level 1 spends only the originals, in threes
    expect(tree.filter((x) => x.level === 1).every((x) => x.parents.length === 0 && x.fresh.length === 3)).toBe(true);
    expect(tree.flatMap((x) => x.fresh).sort((a, b) => a - b)).toEqual(ids(30));
    const parents = tree.flatMap((x) => x.parents);
    expect(new Set(parents).size).toBe(parents.length);
    expect(parents.sort((a, b) => a - b)).toEqual(ids(14));
    // a node spends only outputs of nodes before it, of a lower level
    tree.forEach((x, k) => x.parents.forEach((p) => expect(p < k && tree[p]!.level < x.level).toBe(true)));
    expect(tree.every((x) => x.parents.length + x.fresh.length <= 3)).toBe(true);
    expect(mergeLeaves(tree)).toEqual([14]);
    expect(left(30, tree)).toBe(1);
  });

  it('always the fewest transactions, ceil((n - 1) / 2), into one UTXO', () => {
    for (let n = 2; n <= 64; n++) {
      const tree = mergeTree(ids(n), 3);
      expect(tree.length, `n = ${n}`).toBe(mergeTxCount(n, 3));
      expect(tree.length, `n = ${n}`).toBe(Math.ceil((n - 1) / 2));
      expect(left(n, tree), `n = ${n}`).toBe(1);
    }
  });

  it('8 / 8: 10 UTXOs in 2 transactions (8, then that output with the last 2)', () => {
    expect(mergeTree(ids(10), 8).map((x) => [x.level, x.parents.length + x.fresh.length])).toEqual([[1, 8], [2, 3]]);
    expect(mergeTxCount(10, 8)).toBe(2);
  });

  it('at most MAX_MERGE_TXS transactions per run: a larger set merges its smallest UTXOs first', () => {
    const set = utxos(120);
    const shape = planMergeShape(STD, set, MAKER.pk);
    expect(shape.ok).toBe(true);
    if (!shape.ok) return;
    expect(shape.nodes).toHaveLength(MAX_MERGE_TXS);
    const merged = shape.nodes.flatMap((x) => x.fresh);
    expect(merged).toHaveLength(1 + 2 * MAX_MERGE_TXS);
    expect(merged.map((u) => u.state.amount).sort((a, b) => Number(a) - Number(b))).toEqual(set.slice(0, 1 + 2 * MAX_MERGE_TXS).map((u) => u.state.amount));
  });

  it('KRON: the merged amount stays within the per-output cap (the smallest prefix that fits)', () => {
    expect(KRON_MAX_OUTPUT_AMOUNT).toBe(1_000_000_000n);
    const shape = planMergeShape(KRON, kronUtxos([400_000_000n, 300_000_000n, 300_000_000n, 200_000_000n]), MAKER.pk);
    expect(shape.ok).toBe(true);
    if (!shape.ok) return;
    // 200 + 300 + 300 = 800 M fit, the 400 M would take the output to 1.2 G
    expect(shape.nodes.flatMap((x) => x.fresh).map((u) => u.state.amount)).toEqual(['200000000', '300000000', '300000000']);
  });
});

describe('target mode (just enough for one placement)', () => {
  it('the fewest transactions after which the 3 largest UTXOs cover the amount', () => {
    // 30 UTXOs of 10 .. 300: the largest 3 hold 870, 5 hold 1400, 7 hold 1890, 9 hold 2340
    const set = utxos(30);
    const take = (target: bigint) => {
      const r = targetSet(mergeCandidates(set, STD, MAKER.pk), 3, target);
      return 'take' in r ? r.take.map((u) => Number(u.state.amount)) : r.code;
    };
    expect(take(870n)).toBe('merge.not-needed');
    expect(take(1000n)).toEqual([260, 270, 280, 290, 300]);
    expect(take(2000n)).toEqual([220, 230, 240, 250, 260, 270, 280, 290, 300]);
    expect(take(4651n)).toBe('merge.short');
  });

  it('plans a tree that stops at 3 UTXOs: 9 originals into 3 on level 1, the 3 largest then cover the amount', () => {
    const shape = planMergeShape(STD, utxos(30), MAKER.pk, new Set(), { target: 2000n });
    expect(shape.ok).toBe(true);
    if (!shape.ok) return;
    expect(shape.nodes.map((x) => [x.level, x.parents.length, x.fresh.length])).toEqual([[1, 0, 3], [1, 0, 3], [1, 0, 3]]);
    const outs = shape.nodes.map((x) => sum(x.fresh)).sort((a, b) => (a > b ? -1 : 1));
    expect(outs[0]! + outs[1]! + outs[2]!).toBeGreaterThanOrEqual(2000n);
    // 11 largest needed: 4 transactions, the fourth on level 2
    const deeper = planMergeShape(STD, utxos(30), MAKER.pk, new Set(), { target: 2700n });
    expect(deeper.ok && deeper.nodes.map((x) => x.level)).toEqual([1, 1, 1, 2]);
  });

  it('says when no merge is needed, when the UTXOs do not hold the amount, and when KRON would exceed its cap', () => {
    const code = (s: ReturnType<typeof planMergeShape>) => (s.ok ? 'ok' : s.issue.code);
    expect(code(planMergeShape(STD, utxos(5), MAKER.pk, new Set(), { target: 120n }))).toBe('merge.not-needed');
    expect(code(planMergeShape(STD, utxos(5), MAKER.pk, new Set(), { target: 151n }))).toBe('merge.short');
    // the KRON program takes 4 token inputs: 1.5 G needs all six of 300 M, one 3 -> 1 output of 900 M within the cap
    expect(KRON.slots.inputs).toBe(4);
    const kron = kronUtxos(Array.from({ length: 6 }, () => 300_000_000n));
    expect(code(planMergeShape(KRON, kron, MAKER.pk, new Set(), { target: 1_500_000_000n }))).toBe('ok');
    // seven of 400 M for 1.7 G: the 4 -> 1 output would hold 1.6 G, above the cap
    const big = kronUtxos(Array.from({ length: 7 }, () => 400_000_000n));
    expect(code(planMergeShape(KRON, big, MAKER.pk, new Set(), { target: 1_700_000_000n }))).toBe('merge.over-cap');
  });

  it('a stopped run leaves a valid state: planned again, the rest takes the transactions that were not sent', () => {
    const set = utxos(30);
    const plan = planMergeShape(STD, set, MAKER.pk);
    expect(plan.ok && plan.nodes).toHaveLength(15);
    if (!plan.ok) return;
    // four level-1 transactions went through, the fifth was rejected: 18 originals and 4 merged outputs remain
    const sent = plan.nodes.slice(0, 4);
    const spent = new Set(sent.flatMap((x) => x.fresh));
    const outputs = sent.map((x, k) => tokenUtxo(sum(x.fresh), MAKER.pk, 0xc0 + k, KAS));
    const now = [...set.filter((u) => !spent.has(u)), ...outputs];
    const again = planMergeShape(STD, now, MAKER.pk);
    expect(again.ok && [again.before, again.nodes.length]).toEqual([22, 11]);
  });
});

describe('planMerge on the standard program (real kob-wasm builds)', () => {
  const checkLink = (l: ReturnType<typeof planMerge>['links'][number]) => {
    expect(l.inputs.length).toBeLessThanOrEqual(3);
    expect(l.output.state.owner).toBe(MAKER.pk);
    expect(l.built.tx.outputs.filter((o) => o.covenant?.covenantId === TOKEN.covenantId)).toHaveLength(1);
    expect(l.built.sign.every((r) => r.pubkey === MAKER.pk)).toBe(true);
    signAndValidate(kob, l.built, [MAKER.sk]);
    const s = decodeSigning({ kob, built: l.built, maker: MAKER.pk, registry, expected: { cancelIds: [] }, nodeInputs: nodeFactsOf(l.built) });
    expect(s.kind).toBe('send');
    expect(s.blocking).toEqual([]);
    expect(s.warnings).toEqual([]);
  };

  it('7 UTXOs: 3 transfers to the wallet itself, the level-2 one led by both level-1 outputs, valid and decoded as a send', () => {
    const set = utxos(7);
    const plan = planMerge(env(), STD, set);
    expect(plan.ok, JSON.stringify(plan.issues)).toBe(true);
    expect(plan.links.map((l) => [l.level, l.parents])).toEqual([[1, []], [1, []], [2, [0, 1]]]);
    expect([plan.before, plan.after, plan.merged, plan.levels]).toEqual([7, 1, 7, 2]);
    expect(plan.amount).toBe(sum(set));
    expect(plan.fee).toBe(plan.links.reduce((s, l) => s + l.fee, 0n));
    const top = plan.links[2]!;
    // the parents' outputs (their unsigned ids) lead the link, then the original left over
    expect(top.inputs.slice(0, 2).map((u) => `${u.transactionId}:${u.index}`)).toEqual(plan.links.slice(0, 2).map((l) => `${l.built.tx.id}:${l.output.index}`));
    expect(BigInt(top.output.state.amount)).toBe(sum(set));
    for (const l of plan.links) {
      checkLink(l);
      // the freed carriers pay the fee: no wallet KAS input, the merged output keeps one carrier at the 1 KAS default
      expect(l.fundingUsed).toEqual([]);
      expect(l.output.carrier).toBe(KAS);
    }
  });

  it('30 UTXOs: 15 transactions, each valid; the KAS freed is the KAS change the links return', () => {
    const plan = planMerge(env(), STD, utxos(30));
    expect(plan.ok, JSON.stringify(plan.issues)).toBe(true);
    expect(plan.links).toHaveLength(15);
    expect([plan.before, plan.after, plan.levels]).toEqual([30, 1, 4]);
    for (const l of plan.links) checkLink(l);
    // 30 carriers of 2 KAS in, the final UTXO keeps 1 KAS: 59 KAS less the fees come back as KAS change
    expect(plan.freed).toBe(59n * KAS - plan.fee);
    const change = plan.links.reduce((s, l) => s + l.built.tx.outputs.filter((o) => !o.covenant).reduce((t, o) => t + BigInt(o.value), 0n), 0n);
    expect(change).toBe(plan.freed);
    // a wallet coin added for the fee comes back as change: it is not counted as freed
    const funded = planMerge(env(), STD, utxos(3, 2_000_000n));
    expect(funded.freed).toBe(3n * 2_000_000n - funded.links[0]!.output.carrier - funded.fee);
  });

  it('target mode: only the transactions the amount needs, built and valid', () => {
    const plan = planMerge(env(), STD, utxos(30), new Set(), { target: 2000n });
    expect(plan.ok, JSON.stringify(plan.issues)).toBe(true);
    expect(plan.target).toBe(2000n);
    expect(plan.links).toHaveLength(3);
    expect([plan.before, plan.after]).toEqual([30, 24]);
    for (const l of plan.links) checkLink(l);
  });

  it('a later link rebuilt over the node-read outputs equals the planned one', () => {
    const plan = planMerge(env(), STD, utxos(7));
    const top = plan.links[2]!;
    const carried = top.parents.map((p) => mergeCarry(plan.links[p]!, plan.links[p]!.built.tx.id, STD, { amount: plan.links[p]!.output.carrier.toString(), blockDaaScore: '900' }));
    const again = buildMergeLink(env(), STD, [...carried, ...top.fresh], top.fresh, new Set(), top);
    expect('error' in again).toBe(false);
    if ('error' in again) return;
    expect(again.built.tx.outputs).toEqual(top.built.tx.outputs);
    expect([again.level, again.parents]).toEqual([2, [0, 1]]);
    signAndValidate(kob, again.built, [MAKER.sk]);
  });

  it('the smallest carriers (0.02 KAS) still pay the fee themselves', () => {
    const plan = planMerge(env([]), STD, utxos(3, 2_000_000n));
    expect(plan.ok, JSON.stringify(plan.issues)).toBe(true);
    expect(plan.links[0]!.fundingUsed).toEqual([]);
    expect(plan.links[0]!.output.carrier).toBe(2_000_000n);
    signAndValidate(kob, plan.links[0]!.built, [MAKER.sk]);
  });

  it('KRON: every link carries one wallet KAS UTXO (the owner presence); without one the plan says so', () => {
    const set = kronUtxos(Array.from({ length: KRON.slots.inputs + 1 }, (_, i) => BigInt(100 * (i + 1))));
    const plan = planMerge(env([keyUtxo(MAKER.pk, 5n * KAS, 211), keyUtxo(MAKER.pk, 6n * KAS, 212)]), KRON, set);
    expect(plan.ok, JSON.stringify(plan.issues)).toBe(true);
    expect(plan.links).toHaveLength(2);
    // two links, two different wallet coins (the first one is spent by link 1)
    expect(plan.links.map((l) => l.fundingUsed.length)).toEqual([1, 1]);
    expect(plan.links[0]!.fundingUsed[0]).not.toEqual(plan.links[1]!.fundingUsed[0]);
    for (const l of plan.links) signAndValidate(kob, l.built, [MAKER.sk]);
    const broke = planMerge(env([]), KRON, set);
    expect(broke.issues.map((i) => i.code)).toEqual(['merge.needs-funding']);
  });

  it('nothing to merge: fewer than two candidates', () => {
    const plan = planMerge(env(), STD, [tokenUtxo(10n, MAKER.pk, 0x61), tokenUtxo(10n, OTHER.pk, 0x62)]);
    expect(plan.ok).toBe(false);
    expect(plan.issues.map((i) => [i.code, i.params?.count])).toEqual([['merge.nothing', 1]]);
  });

  it('reserved outpoints are never inputs', () => {
    const set = utxos(4);
    const reserved = new Set([`${set[0]!.transactionId}:${set[0]!.index}`]);
    const plan = planMerge(env(), STD, set, reserved);
    const spent = plan.links.flatMap((l) => l.built.tx.inputs.map((i) => `${i.transactionId}:${i.index}`));
    expect(spent).not.toContain([...reserved][0]);
    expect([plan.before, plan.after]).toEqual([3, 1]);
  });

  it('the merged state is the one the output script commits to', () => {
    const plan = planMerge(env(), STD, utxos(3));
    const l = plan.links[0]!;
    expect(l.built.tx.outputs[l.output.index]!.scriptPublicKey).toBe(kob.tokenScriptPublicKey(STD.program, l.output.state));
    expect((l.output.state as { extension_commitment: Hex }).extension_commitment).toBe(TOKEN.ext);
  });
});
