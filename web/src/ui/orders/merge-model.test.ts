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

describe('merge flow step (one transaction per click)', () => {
  const set = Array.from({ length: 5 }, (_, i) => tokenUtxo(BigInt(10 * (i + 1)), MAKER.pk, 0x90 + i, 2n * KAS));
  const one = () => planMerge({ kob, maker: MAKER.pk, funding: [keyUtxo(MAKER.pk, 10n * KAS)] }, mergeTokenOf(info)!, set, new Set(), 1);

  it('the app plans one link: its confirmation says what it merges, the KAS it frees and how many mergeable UTXOs remain', () => {
    const plan = one();
    expect(plan.ok).toBe(true);
    expect(plan.links).toHaveLength(1);
    // three 2 KAS carriers in, one kept by the merged output: 4 KAS less the fee come back
    expect(plan.freed).toBe(4n * KAS - plan.fee);
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

  it('the last merge says nothing is left; a fee above the freed carriers is stated as a net cost', () => {
    const last = mergeSummary({ ...one(), before: 3, after: 1 }, info.ticker);
    expect(last).toMatch(/ One UTXO remains: nothing more to merge\.$/);
    const cost = mergeSummary({ ...one(), freed: -1_000_000n, fee: 3_000_000n }, info.ticker);
    expect(cost).toContain('The fee of about 0.03 KAS exceeds the carriers it frees: your wallet pays about 0.01 KAS net.');
  });
});
