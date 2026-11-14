// The merge action's model: the outpoints held for open orders, the per-token count behind the "Merge all (n)" button, the KAS it frees, and
// the flow steps with the summary the run's confirmation states (transactions, approvals, KAS freed).
import { describe, expect, it } from 'vitest';
import type { OrderView, StrayView } from '../../data/indexer-types';
import type { OrderSnapshot } from '../../kob/cancel';
import { planMerge } from '../../kob/merge';
import { parseRegistry } from '../../kob/registry';
import { loadKobNode } from '../../kob/wasm.node';
import { MAKER, OTHER, TOKEN, keyUtxo, tokenUtxo, tokenUtxoView, tradableRegistryJson } from '../../testing/chain-fixtures';
import { mergeAllFrees, mergeFailure, mergeRunLine, mergeStep, mergeSummary, mergeTokenFor, mergeTokenOf, mergeableCounts, reservedOutpoints, unlistedMergeToken } from './merge-model';
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
    expect(mergeAllFrees(mine, none, MAKER.pk, new Set(), unlisted).has(TOKEN.covenantId)).toBe(true);
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

  it('the plan merges them: one transaction, the three UTXOs into one', () => {
    const mt = unlistedMergeToken(TOKEN.covenantId, mine, MAKER.pk, new Set(), unlisted)!;
    const plan = planMerge({ kob, maker: MAKER.pk, funding: [keyUtxo(MAKER.pk, 10n * KAS)] }, mt, mine);
    expect(plan.ok).toBe(true);
    expect(plan.links).toHaveLength(1);
    expect(plan.links[0]!.inputs).toHaveLength(3);
  });
});

describe('Merge all: the confirmation and the flow steps', () => {
  const set = (n: number) => Array.from({ length: n }, (_, i) => tokenUtxo(BigInt(10 * (i + 1)), MAKER.pk, 0x90 + i, 2n * KAS));
  const plan = (n: number, o = {}) => planMerge({ kob, maker: MAKER.pk, funding: [keyUtxo(MAKER.pk, 10n * KAS)] }, mergeTokenOf(info)!, set(n), new Set(), o);

  it('30 UTXOs: the confirmation states 15 transactions, 15 wallet approvals and the KAS freed', () => {
    const p = plan(30);
    expect(p.ok).toBe(true);
    expect(p.links).toHaveLength(15);
    // thirty 2 KAS carriers in, the final UTXO keeps one at the 1 KAS default: 59 KAS less the fees come back
    expect(p.freed).toBe(59n * KAS - p.fee);
    const freed = formatKas(p.freed).replace('.', '\\.');
    expect(mergeSummary(p, info.ticker)).toMatch(
      new RegExp(
        `^Merge your 30 TST UTXOs into one: 15 transactions, one after the other, each approved in your wallet \\(15 approvals\\)\\. About ${freed} KAS of their carriers returns to your wallet \\(after network fees of about 0\\.\\d+ KAS\\)\\. One UTXO remains\\.$`,
      ),
    );
    // the button's figure before the click: the carriers less the one the final UTXO keeps, before the fees
    expect(mergeAllFrees(set(30), registry.byCovenantId, MAKER.pk, new Set()).get(TOKEN.covenantId)).toBe(59n * KAS);
    expect(mergeAllFrees(set(1), registry.byCovenantId, MAKER.pk, new Set()).size).toBe(0);
  });

  it('one transaction says so in the singular; a merge that cannot take every UTXO says how many it takes and what remains', () => {
    expect(mergeSummary(plan(3), info.ticker)).toMatch(/^Merge your 3 TST UTXOs into one: 1 transaction, approved in your wallet \(1 approval\)\. About /);
    const some = mergeSummary({ ...plan(3), before: 120, merged: 101, after: 20 }, info.ticker);
    expect(some).toMatch(/^Merge your 101 smallest TST UTXOs \(of 120\) into one: /);
    expect(some).toMatch(/ 20 mergeable UTXOs remain: Merge all again to merge them\.$/);
  });

  it('the sell ticket\'s merge says the sale comes after it, with its own confirmation', () => {
    const p = plan(30, { target: 2000n });
    expect(p.links).toHaveLength(3);
    expect(mergeSummary(p, info.ticker)).toMatch(
      /^Your TST needs merging before the sale: 3 transactions, one after the other, each approved in your wallet \(3 approvals\)\. Then the sale is planned again and you confirm it as usual\. About [0-9.]+ KAS /,
    );
  });

  it('every step is confirmed up front (the wallet approval is its one prompt); a run of several reports in its card, not by toasts', () => {
    const p = plan(7);
    const summary = mergeSummary(p, info.ticker);
    const steps = p.links.map((l, i) => mergeStep(l, i, p.links.length, summary));
    expect(steps.map((x) => x.title)).toEqual(['Merge 1 of 3', 'Merge 2 of 3', 'Merge 3 of 3']);
    expect(steps.every((x) => x.preconfirmed && x.silent && x.shownAs === summary && x.spends.length === 0)).toBe(true);
    expect(steps[2]!.plan.built).toBe(p.links[2]!.built);
    const one = plan(3);
    const single = mergeStep(one.links[0]!, 0, 1, mergeSummary(one, info.ticker));
    expect(single.title).toBe('Merge tokens');
    expect(single.silent).toBeUndefined();
    expect(single.preconfirmed).toBe(true);
    expect(single.toast).toBe('Submitted: 3 token UTXOs merged into one.');
    expect(mergeFailure({ issues: [{ code: 'merge.nothing', severity: 'error', message: 'x' }] }).ok).toBe(false);
  });

  it('the signer frame says what a merge makes spendable (the KAS line alone reads as the fee)', () => {
    const p = plan(5);
    const signer = summarize(kob, p.links[0]!.built, MAKER.pk, []);
    const line = signer.lines.find((l) => l.label === 'KAS freed from your token UTXOs (now spendable)');
    expect(line?.value).toBe(`${formatKas(5n * KAS)} KAS`);
  });

  it('the running card says it in one line: the transactions and about the KAS freed', () => {
    const p = plan(30);
    expect(mergeRunLine(p)).toBe(`15 transactions · about +${formatKas(p.freed)} KAS`);
    expect(mergeRunLine({ ...plan(3), freed: -1_000_000n })).toBe('1 transaction · about −0.01 KAS net');
  });

  it('a fee above the freed carriers is stated as a net cost', () => {
    const cost = mergeSummary({ ...plan(3), freed: -1_000_000n, fee: 3_000_000n }, info.ticker);
    expect(cost).toContain('The network fees of about 0.03 KAS exceed the carriers it frees: your wallet pays about 0.01 KAS net.');
  });
});
