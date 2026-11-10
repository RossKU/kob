// The merge action's model: the outpoints held for open orders, the per-token count behind the "Merge (n)" button and the flow steps with the
// summary every confirmation screen of the chain shows.
import { describe, expect, it } from 'vitest';
import type { OrderView, StrayView } from '../../data/indexer-types';
import type { OrderSnapshot } from '../../kob/cancel';
import { planMerge } from '../../kob/merge';
import { parseRegistry } from '../../kob/registry';
import { loadKobNode } from '../../kob/wasm.node';
import { MAKER, OTHER, TOKEN, keyUtxo, tokenUtxo, tokenUtxoView, tradableRegistryJson } from '../../testing/chain-fixtures';
import { mergeFailure, mergeStep, mergeSummary, mergeTokenFor, mergeTokenOf, mergeableCounts, nextMergeFrees, reservedOutpoints, unlistedMergeToken } from './merge-model';
import { summarize } from '../../signer/summary';
import { formatKas } from '../../kob/units';

const kob = loadKobNode();
const registry = parseRegistry(tradableRegistryJson(), { kob });
const KAS = 100_000_000n;
const info = registry.byCovenantId.get(TOKEN.covenantId)!;

describe('reservedOutpoints', () => {
  it("collects the live orders' custodies and strays (views, snapshots, the stray list), not those of closed orders", () => {
    const custody = tokenUtxo(5n, 'ab'.repeat(32), 0x71, KAS, 4);
    const stray = tokenUtxo(1n, 'ab'.repeat(32), 0x72, KAS, 4);
    const closed = tokenUtxo(9n, 'ac'.repeat(32), 0x73, KAS, 4);
    const views = [
      { status: 'open', custody: { expected_amount: '5', utxo: tokenUtxoView(custody, 'custody'), ok: true }, strays: [tokenUtxoView(stray, 'stray')] },
      { status: 'filled', custody: { expected_amount: '9', utxo: tokenUtxoView(closed, 'custody'), ok: true }, strays: [] },
    ] as unknown as OrderView[];
    const snapCustody = tokenUtxo(3n, 'ad'.repeat(32), 0x74, KAS, 4);
    const snapshots = [{ custody: snapCustody, prefund: null, strays: [] }] as unknown as OrderSnapshot[];
    const listed = { ...tokenUtxoView(tokenUtxo(2n, 'ae'.repeat(32), 0x75, KAS, 4), 'stray'), order_status: 'open', maker: MAKER.pk, lost: false } as StrayView;
    const got = reservedOutpoints({ views, snapshots, strays: [listed] });
    expect([...got].sort()).toEqual([custody, stray, snapCustody, { transactionId: listed.txid, index: listed.index }].map((u) => `${u.transactionId}:${u.index}`).sort());
    expect(reservedOutpoints(null).size).toBe(0);
  });
});

describe('mergeableCounts', () => {
  it('counts the plain UTXOs of each known token the wallet owns; a single one or a reserved one does not count', () => {
    const mine = [tokenUtxo(1n, MAKER.pk, 0x81), tokenUtxo(2n, MAKER.pk, 0x82), tokenUtxo(3n, MAKER.pk, 0x83)];
    const theirs = tokenUtxo(4n, OTHER.pk, 0x84);
    const counts = mergeableCounts([...mine, theirs], registry.byCovenantId, MAKER.pk, new Set([`${mine[2]!.transactionId}:${mine[2]!.index}`]));
    expect([...counts]).toEqual([[TOKEN.covenantId, 2]]);
    expect(mergeableCounts(mine.slice(0, 1), registry.byCovenantId, MAKER.pk, new Set()).size).toBe(0);
  });
});

describe('a token the registry does not list (open list, foreign KCC-20)', () => {
  const none = new Map<string, never>();
  const unlisted = { kob, programs: new Map([[TOKEN.covenantId, TOKEN.program]]) };
  const mine = [tokenUtxo(1n, MAKER.pk, 0xa1), tokenUtxo(2n, MAKER.pk, 0xa2), tokenUtxo(3n, MAKER.pk, 0xa3)];

  it('is mergeable under its node-verified program: the template hash and slots of that program, the extension commitment of its UTXOs', () => {
    const mt = unlistedMergeToken(TOKEN.covenantId, mine, MAKER.pk, new Set(), unlisted)!;
    expect(mt).toEqual({ covenantId: TOKEN.covenantId, program: TOKEN.program, templateHash: info.templateHash, extensionCommitment: TOKEN.ext, slots: { inputs: info.slots.inputs } });
    expect(mergeableCounts(mine, none, MAKER.pk, new Set(), unlisted)).toEqual(new Map([[TOKEN.covenantId, 3]]));
    expect(nextMergeFrees(mine, none, MAKER.pk, new Set(), unlisted).has(TOKEN.covenantId)).toBe(true);
    // no node-verified program: nothing to merge with (and without the option the registry-only behaviour stays)
    expect(mergeableCounts(mine, none, MAKER.pk, new Set(), { kob, programs: new Map() }).size).toBe(0);
    expect(mergeableCounts(mine, none, MAKER.pk, new Set()).size).toBe(0);
  });

  it('with UTXOs under different extension commitments, merges the commitment most of them carry', () => {
    const other = 'dd'.repeat(32);
    const mixed = [...mine, tokenUtxo(4n, MAKER.pk, 0xa4, 1_000_000_000n, 0, other), tokenUtxo(5n, MAKER.pk, 0xa5, 1_000_000_000n, 0, other), tokenUtxo(6n, MAKER.pk, 0xa6, 1_000_000_000n, 0, other), tokenUtxo(7n, MAKER.pk, 0xa7, 1_000_000_000n, 0, other)];
    const mt = unlistedMergeToken(TOKEN.covenantId, mixed, MAKER.pk, new Set(), unlisted)!;
    expect(mt.extensionCommitment).toBe(other);
    expect(mergeableCounts(mixed, none, MAKER.pk, new Set(), unlisted).get(TOKEN.covenantId)).toBe(4);
  });

  it('a single UTXO, or only UTXOs of another key, is not mergeable; the registry token is still the registry one', () => {
    expect(mergeableCounts(mine.slice(0, 1), none, MAKER.pk, new Set(), unlisted).size).toBe(0);
    expect(unlistedMergeToken(TOKEN.covenantId, [tokenUtxo(4n, OTHER.pk, 0xa8)], MAKER.pk, new Set(), unlisted)).toBeNull();
    expect(mergeTokenFor(TOKEN.covenantId, mine, registry.byCovenantId, MAKER.pk, new Set(), unlisted)).toEqual(mergeTokenOf(info));
  });

  it('the plan merges them: one transaction, the smallest UTXOs into one', () => {
    const mt = unlistedMergeToken(TOKEN.covenantId, mine, MAKER.pk, new Set(), unlisted)!;
    const plan = planMerge({ kob, maker: MAKER.pk, funding: [keyUtxo(MAKER.pk, 10n * KAS)] }, mt, mine, new Set(), 1);
    expect(plan.ok).toBe(true);
    expect(plan.links).toHaveLength(1);
    expect(plan.links[0]!.inputs).toHaveLength(3);
  });
});

describe('merge flow step (one transaction per click)', () => {
  const set = Array.from({ length: 5 }, (_, i) => tokenUtxo(BigInt(10 * (i + 1)), MAKER.pk, 0x90 + i, 2n * KAS));
  const one = () => planMerge({ kob, maker: MAKER.pk, funding: [keyUtxo(MAKER.pk, 10n * KAS)] }, mergeTokenOf(info)!, set, new Set(), 1);

  it('the app plans one link: its confirmation says what it merges, the KAS it frees and how many mergeable UTXOs remain', () => {
    const plan = one();
    expect(plan.ok).toBe(true);
    expect(plan.links).toHaveLength(1);
    // three 2 KAS carriers in, one kept by the merged output at the 1 KAS default: 5 KAS less the fee come back
    expect(plan.freed).toBe(5n * KAS - plan.fee);
    const summary = mergeSummary(plan, info.ticker);
    const freed = (Number(plan.freed) / 1e8).toString().replace('.', '\\.');
    expect(summary).toMatch(
      new RegExp(
        `^Merge your 3 smallest TST UTXOs into one in one transaction: 5 UTXOs become 3\\. About ${freed} KAS of their carriers returns to your wallet \\(after the fee of about 0\\.\\d+ KAS\\)\\. 3 mergeable UTXOs remain: click Merge again to merge more\\.$`,
      ),
    );
    const step = mergeStep(plan.links[0]!, summary);
    expect(step.title).toBe('Merge tokens');
    expect(step.shownAs).toBe(summary);
    expect(step.plan.built).toBe(plan.links[0]!.built);
    expect(step.spends).toEqual([]);
    expect(step.toast).toBe('Submitted: 3 token UTXOs merged into one.');
    expect(mergeFailure({ issues: [{ code: 'merge.nothing', severity: 'error', message: 'x' }] }).ok).toBe(false);
  });

  it('the signer frame says what the merge makes spendable (the KAS line alone reads as the fee) and the button says it before the click', () => {
    const plan = one();
    const signer = summarize(kob, plan.links[0]!.built, MAKER.pk, []);
    const line = signer.lines.find((l) => l.label === 'KAS freed from your token UTXOs (now spendable)');
    expect(line?.value).toBe(`${formatKas(5n * KAS)} KAS`);
    // three 2 KAS carriers, one kept at the 1 KAS default: about 5 KAS before the fee
    expect([...nextMergeFrees(set, registry.byCovenantId, MAKER.pk, new Set())]).toEqual([[TOKEN.covenantId, 5n * KAS]]);
    expect(nextMergeFrees(set.slice(0, 1), registry.byCovenantId, MAKER.pk, new Set()).size).toBe(0);
  });

  it('the last merge says nothing is left; a fee above the freed carriers is stated as a net cost', () => {
    const last = mergeSummary({ ...one(), before: 3, after: 1 }, info.ticker);
    expect(last).toMatch(/ One UTXO remains: nothing more to merge\.$/);
    const cost = mergeSummary({ ...one(), freed: -1_000_000n, fee: 3_000_000n }, info.ticker);
    expect(cost).toContain('The fee of about 0.03 KAS exceeds the carriers it frees: your wallet pays about 0.01 KAS net.');
  });
});
