// My orders rows of orders under an older contract version: a retired template of the cancel-only table is shown with its decoded terms, the
// "older contract version — cancel only" badge and a working Cancel (counted by Cancel all); a template outside the table has no Cancel.
import { describe, expect, it } from 'vitest';
import type { Clock } from '../../kob/plan-types';
import { recordsFromBuilt, type PlacementRecord, type ResolvedRecord } from '../../kob/records';
import type { OrderState } from '../../kob/types';
import { loadKobNode } from '../../kob/wasm.node';
import { MAKER, orderViewOf, placeGolden } from '../../testing/chain-fixtures';
import { cancellableIds } from './list-model';
import { buildEntries, describeEntry } from './orders-model';

const kob = loadKobNode();
const CLOCK: Clock = { daa: 1000n, unixSeconds: 1_790_000_000n, rateMilli: 10_000 };
const ctx = { kob, clock: CLOCK };
const opts = { maker: MAKER.pk, network: 'testnet-10', placedAtUnix: 1_790_000_000n, placedAtDaa: 1000n };
const RETIRED_BID = kob.retiredTemplates().find((x) => x.label === 'KobBid b995661f')!.hash;

const placed = placeGolden(kob, 'create.bid');
const rec0 = recordsFromBuilt(kob, placed.built, opts)[0]!;
const rec: PlacementRecord = { ...rec0, templateHash: RETIRED_BID };
const state = kob.decodeState('KobBid', rec.state) as OrderState;
const live: ResolvedRecord = {
  status: 'live', covenantId: rec.covenantId, order: { ...placed.snapshots[0]!.order, state }, custody: null, strays: [], stateChanged: false, checked: 1, retired: RETIRED_BID,
};

describe('an order under a retired template of the cancel-only table', () => {
  it('resolved on the node: its terms, the cancel-only badge and a working Cancel; no amend, sweep or refund', () => {
    const row = describeEntry(buildEntries([], [rec], new Map([[rec.covenantId, live]]))[0]!, ctx);
    expect(row).toMatchObject({ status: 'open', live: true, cancelOnly: true, oldTemplate: false, canCancel: true, canAmend: false, canRefund: false, side: 'buy', typeKey: 'limit-gtd' });
    expect(row.price).toBe(BigInt((state.state as { price: string }).price));
    expect(cancellableIds([row], null)).toEqual([rec.covenantId]);
  });

  it('before the node answers it still shows its decoded terms (not "unknown"), with the badge', () => {
    const row = describeEntry(buildEntries([], [rec])[0]!, ctx);
    expect(row).toMatchObject({ cancelOnly: true, oldTemplate: false, typeKey: 'limit-gtd', side: 'buy', canCancel: false });
    expect(row.price).not.toBeNull();
  });

  it('served by the indexer\'s single view (cancel_only): cancellable from it', () => {
    const view = { ...orderViewOf(kob, placed.snapshots[0]!), template_hash: RETIRED_BID, cancel_only: true };
    const row = describeEntry(buildEntries([view], [])[0]!, ctx);
    expect(row).toMatchObject({ cancelOnly: true, canCancel: true, canAmend: false });
  });
});

describe('an order under a template neither pinned nor in the table', () => {
  it('is old-template: no Cancel, not counted by Cancel all', () => {
    const unknown = { ...rec, templateHash: '5a'.repeat(32) };
    const resolved: ResolvedRecord = { status: 'old-template', covenantId: rec.covenantId, order: null, custody: null, strays: [], stateChanged: false, checked: 0 };
    const row = describeEntry(buildEntries([], [unknown], new Map([[rec.covenantId, resolved]]))[0]!, ctx);
    expect(row).toMatchObject({ oldTemplate: true, cancelOnly: false, canCancel: false });
    expect(cancellableIds([row], null)).toEqual([]);
  });
});
