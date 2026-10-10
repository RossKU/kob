// "Merge tokens": the candidates (only the wallet's own plain UTXOs of the token, never an order's custody / stray or a reserved outpoint), the
// chain's shape on the standard 3 / 3 program (3 -> 1, then 1 + 2 -> 1, ...) and on the 8 / 8 prototype, and every link built by kob-wasm, signed
// locally, run through the script engine and decoded on the confirmation model (a send to the wallet itself, nothing blocking).
import { describe, expect, it } from 'vitest';
import { decodeSigning } from './decode';
import { MAX_MERGE_TXS, buildMergeLink, mergeCandidates, mergeCarry, mergeGroups, mergeSlots, planMerge, type MergeToken } from './merge';
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
const env = (funding = [keyUtxo(MAKER.pk, 50n * KAS, 201)]) => ({ kob, maker: MAKER.pk, funding });
/** `n` UTXOs of the maker: amounts 10, 20, ... base units, outpoints 0x40.. */
const utxos = (n: number, carrier = 2n * KAS): TokenUtxo[] => Array.from({ length: n }, (_, i) => tokenUtxo(BigInt((i + 1) * 10), MAKER.pk, 0x40 + i, carrier));
const sum = (xs: TokenUtxo[]) => xs.reduce((s, u) => s + BigInt(u.state.amount), 0n);

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

describe('chain shape', () => {
  it('3 / 3: the first link takes 3, every next one 2 more (with the previous output): 7 UTXOs in 3 transactions', () => {
    expect(mergeSlots(STD)).toBe(3);
    expect(mergeGroups(utxos(7), 3).map((g) => g.length)).toEqual([3, 2, 2]);
    expect(mergeGroups(utxos(3), 3).map((g) => g.length)).toEqual([3]);
    expect(mergeGroups(utxos(2), 3).map((g) => g.length)).toEqual([2]);
    expect(mergeGroups(utxos(4), 3).map((g) => g.length)).toEqual([3, 1]);
    expect(mergeGroups(utxos(1), 3)).toEqual([]);
  });

  it('8 / 8: 10 UTXOs in 2 transactions (8, then 1 + 2)', () => {
    expect(mergeGroups(utxos(10), 8).map((g) => g.length)).toEqual([8, 2]);
  });

  it('at most MAX_MERGE_TXS transactions per run; the merged amount stays under a cap (KRON output limit)', () => {
    const groups = mergeGroups(utxos(40), 3);
    expect(groups).toHaveLength(MAX_MERGE_TXS);
    expect(groups.flat()).toHaveLength(3 + 2 * (MAX_MERGE_TXS - 1));
    // 10 + 20 + 30 + 40 = 100 <= 100: four of the five fit under a cap of 100
    expect(mergeGroups(utxos(5), 3, MAX_MERGE_TXS, 100n).flat().map((u) => u.state.amount)).toEqual(['10', '20', '30', '40']);
    expect(KRON_MAX_OUTPUT_AMOUNT).toBe(1_000_000_000n);
  });
});

describe('planMerge on the standard program (real kob-wasm builds)', () => {
  it('7 UTXOs: 3 transfers to the wallet itself, each at most 3 token inputs into one output, valid and decoded as a send', () => {
    const set = utxos(7);
    const plan = planMerge(env(), STD, set);
    expect(plan.ok, JSON.stringify(plan.issues)).toBe(true);
    expect(plan.links).toHaveLength(3);
    expect([plan.before, plan.after]).toEqual([7, 1]);
    expect(plan.amount).toBe(sum(set));
    expect(plan.fee).toBe(plan.links.reduce((s, l) => s + l.fee, 0n));
    let running = 0n;
    plan.links.forEach((l, k) => {
      expect(l.inputs.length).toBeLessThanOrEqual(3);
      if (k > 0) {
        // the previous link's output (its unsigned id) leads the link
        expect(l.inputs[0]!.transactionId).toBe(plan.links[k - 1]!.built.tx.id);
        expect(l.inputs[0]!.index).toBe(plan.links[k - 1]!.output.index);
      }
      running += sum(l.fresh);
      expect(BigInt(l.output.state.amount)).toBe(running);
      expect(l.output.state.owner).toBe(MAKER.pk);
      const tokenOuts = l.built.tx.outputs.filter((o) => o.covenant?.covenantId === TOKEN.covenantId);
      expect(tokenOuts).toHaveLength(1);
      // the freed carriers pay the fee: no wallet KAS input, the merged output keeps one carrier (2 KAS), the rest is KAS change
      expect(l.fundingUsed).toEqual([]);
      expect(l.output.carrier).toBe(2n * KAS);
      expect(l.built.sign.every((r) => r.pubkey === MAKER.pk)).toBe(true);
      signAndValidate(kob, l.built, [MAKER.sk]);
      const s = decodeSigning({ kob, built: l.built, maker: MAKER.pk, registry, expected: { cancelIds: [] }, nodeInputs: nodeFactsOf(l.built) });
      expect(s.kind).toBe('send');
      expect(s.blocking).toEqual([]);
      expect(s.warnings).toEqual([]);
      expect(s.outputs.filter((o) => o.kind === 'token-change')).toHaveLength(1);
    });
  });

  it('a later link rebuilt over the node-read output equals the planned one', () => {
    const plan = planMerge(env(), STD, utxos(5));
    const carry = mergeCarry(plan.links[0]!, plan.links[0]!.built.tx.id, STD, { amount: (2n * KAS).toString(), blockDaaScore: '900' });
    const again = buildMergeLink(env(), STD, [carry, ...plan.groups[1]!], plan.groups[1]!);
    expect('error' in again).toBe(false);
    if ('error' in again) return;
    expect(again.built.tx.outputs).toEqual(plan.links[1]!.built.tx.outputs);
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
    const tpl = kob.templates().find((x) => x.name === 'KronToken2433')!;
    const KRON: MergeToken = { covenantId: 'e6'.repeat(32), program: 'KronToken2433', templateHash: tpl.hash, extensionCommitment: null, slots: slotsOf('KronToken2433') };
    const set: TokenUtxo[] = Array.from({ length: KRON.slots.inputs + 1 }, (_, i) => ({
      transactionId: (0xa0 + i).toString(16).repeat(32), index: 0, amount: (2n * KAS).toString(), blockDaaScore: '500', covenantId: KRON.covenantId,
      state: keyState('kron', BigInt(100 * (i + 1)), MAKER.pk, null),
    }));
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
