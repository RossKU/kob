// C5 liveness (records without the indexer). Findings pinned here (the sibling file c5-live-records.test.ts covers partial-fill / armed / repeat
// continuations):
//   C5-K1  a record imported from the maker-recovery file (`exportIndexerRecovery`, the format the Recover panel offers for the INDEXER) has
//          `custody: null`. Ask-side states (KobAsk, KobCondAsk, KobIfdAsk, KobCross; KCC-20) do not carry the extension commitment, so
//          resolveRecord's findCustody returns null ("(family === 'kcc20' && !ext)"): the order resolves `live` WITHOUT custody, the row offers
//          Cancel (orders-model `canCancel: e.resolved?.status === 'live'`) and planCancel fails with `cancel.custody-missing`
//          ("Refresh and try again") - forever, because refreshing cannot help. Only reachable with the indexer down / not knowing the order.
//   C5-K2  resolveRecord never compares `record.templateHash` with the template of this wasm build: after a template change the original script
//          is computed from the NEW template, nothing is found at it, and a live order is reported `spent` (row status "closed", no actions).
//   C5-K3  a trailing stop after a trail: `stopPrice` (stopPrice + k * trailStep, KobCondAsk.sil) is not enumerated -> `unknown`, no cancel.
// Uncompiled when written (2026-10-02).
// C5: expected to fail until (K1) the extension commitment is taken from the registry token / an `opts.extension` hint (or canCancel requires the
// custody to be known), (K2) resolveRecord answers `unknown` for a record whose templateHash is not the build's pinned one, (K3) the trailing
// candidates stopPrice0 + k * trailStep (k = 1..) are enumerated.
import { describe, expect, it } from 'vitest';
import { loadKobNode } from './wasm.node';
import { planCancel, snapshotFromRecord, type CancelEnv } from './cancel';
import { exportIndexerRecovery, importIndexerRecovery, recordsFromBuilt, resolveRecord } from './records';
import type { OrderState } from './types';
import { buildEntries, describeEntry } from '../ui/orders/orders-model';
import { FakeChain, MAKER, keyUtxo, placeGolden, testAddress } from '../testing/chain-fixtures';

const kob = loadKobNode();
const KAS = 100_000_000n;
const opts = { maker: MAKER.pk, network: 'testnet-10', placedAtUnix: 1_790_000_000n, placedAtDaa: 1000n };
const CLOCK = { daa: 1000n, unixSeconds: 1_790_000_000n, rateMilli: 10_000 };
const env: CancelEnv = { kob, maker: MAKER.pk, funding: [keyUtxo(MAKER.pk, 100n * KAS, 201)], tokenUtxos: [], clock: { daa: 1000n } };
const recordOf = (name: string) => {
  const p = placeGolden(kob, name);
  return { p, rec: recordsFromBuilt(kob, p.built, opts)[0] };
};
const edit = (o: OrderState, fields: Record<string, string>): OrderState => ({ ...o, state: { ...(o.state as unknown as Record<string, string>), ...fields } } as unknown as OrderState);

describe('C5-K1: an order restored from the maker-recovery file can be cancelled from the node alone', () => {
  it.each(['create.ask', 'create.condAsk', 'create.ifdAsk'])('%s: if the row offers Cancel, the cancel is buildable', async (name) => {
    const { p, rec } = recordOf(name);
    const back = importIndexerRecovery(exportIndexerRecovery([rec], 'testnet-10'), kob, { maker: MAKER.pk, network: 'testnet-10' });
    expect(back.rejected).toEqual([]);
    const chain = new FakeChain();
    chain.applyTx(p.signed.tx);
    const resolved = await resolveRecord(kob, chain, back.records[0], testAddress);
    expect(resolved.status).toBe('live');
    const [entry] = buildEntries([], back.records, new Map([[rec.covenantId, resolved]]));
    const row = describeEntry(entry, { kob, clock: CLOCK });
    expect(row.live).toBe(true);
    // the invariant: a button the UI shows must lead to a transaction (otherwise the row must not offer it, and must say why)
    const plan = planCancel(env, snapshotFromRecord(resolved));
    expect(row.canCancel && !plan.ok ? plan.issues.map((i) => i.code) : []).toEqual([]);
  });
});

describe('C5-K2: a record of another template version is not reported as spent', () => {
  it('create.ask with a template hash this build does not pin, nothing found on the node: unknown, never spent', async () => {
    const { rec } = recordOf('create.ask');
    const old = { ...rec, templateHash: 'aa'.repeat(32) };
    const r = await resolveRecord(kob, new FakeChain(), old, testAddress).catch(() => null);
    expect(r?.status).not.toBe('spent');
  });
});

describe('C5-K3: a trailing stop that has trailed is found from its record', () => {
  it('stopPrice + 2 * trailStep resolves live', async () => {
    const { rec } = recordOf('create.condAsk');
    const base = kob.decodeState(rec.kind as OrderState['kind'], rec.state) as OrderState;
    const s = base.state as unknown as Record<string, string>;
    const step = 10_000_000n;
    const trailing = edit(base, { trailStep: step.toString(), trailGap: '0', trailWait: '600' });
    const rec2 = { ...rec, state: kob.encodeState(trailing) };
    const trailed = edit(trailing, { stopPrice: (BigInt(s.stopPrice) + 2n * step).toString() });
    const chain = new FakeChain();
    chain.add({ transactionId: 'cf'.repeat(32), index: 0, amount: '1000000000', scriptPublicKey: kob.scriptPublicKey(trailed), covenantId: rec.covenantId, blockDaaScore: '2000' });
    const r = await resolveRecord(kob, chain, rec2, testAddress);
    expect(r.status).toBe('live');
    expect((r.order!.state.state as unknown as Record<string, string>).stopPrice).toBe((BigInt(s.stopPrice) + 2n * step).toString());
  });
});
