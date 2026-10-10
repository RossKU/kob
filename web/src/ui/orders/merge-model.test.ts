// The merge action's model: the outpoints held for open orders, the per-token count behind the "Merge (n)" button and the flow steps with the
// summary every confirmation screen of the chain shows.
import { describe, expect, it } from 'vitest';
import type { OrderView, StrayView } from '../../data/indexer-types';
import type { OrderSnapshot } from '../../kob/cancel';
import { planMerge } from '../../kob/merge';
import { parseRegistry } from '../../kob/registry';
import { loadKobNode } from '../../kob/wasm.node';
import { MAKER, OTHER, TOKEN, keyUtxo, tokenUtxo, tokenUtxoView, tradableRegistryJson } from '../../testing/chain-fixtures';
import { mergeFailure, mergeStep, mergeSummary, mergeTokenOf, mergeableCounts, reservedOutpoints } from './merge-model';

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

describe('merge flow steps', () => {
  it('one step per link; every screen says the whole merge and which transaction it is', () => {
    const set = Array.from({ length: 5 }, (_, i) => tokenUtxo(BigInt(10 * (i + 1)), MAKER.pk, 0x90 + i, 2n * KAS));
    const plan = planMerge({ kob, maker: MAKER.pk, funding: [keyUtxo(MAKER.pk, 10n * KAS)] }, mergeTokenOf(info)!, set);
    expect(plan.ok).toBe(true);
    const summary = mergeSummary(plan, info.ticker);
    expect(summary).toMatch(/^Merge 5 TST UTXOs into 1: 2 transaction\(s\), total fee about 0\.\d+ KAS\.$/);
    const steps = plan.links.map((l, i) => mergeStep(l, i, plan.links.length, summary));
    expect(steps.map((s) => s.title)).toEqual(['Merge, transaction 1 of 2', 'Merge, transaction 2 of 2']);
    expect(steps[1]!.shownAs).toBe(`${summary} This is transaction 2 of 2.`);
    expect(steps[0]!.plan.built).toBe(plan.links[0]!.built);
    expect(steps[0]!.spends).toEqual([]);
    expect(steps[0]!.toast).toBe('Submitted: 3 token UTXOs merged into one.');
    expect(mergeFailure({ issues: [{ code: 'merge.nothing', severity: 'error', message: 'x' }] }).ok).toBe(false);
  });
});
