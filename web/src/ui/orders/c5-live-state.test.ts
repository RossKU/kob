// Tests the row states of My orders, so that a healthy position is never shown as a broken order.
//
// The sibling c5-live-orders.test.ts covers `killed`, list truncation and stray slots.
//
// Two rules are pinned here.
// - S1. A SOLD-OUT buy-first REPEAT entry that waits for its exits has `amount_left: '0'`. The
//   indexer keeps a repeating entry that sold out as `partial` with nothing left (executor.md).
//   `unfundable` must exclude repeat entries (`repeat?.role === 'entry'`). Otherwise the row says
//   "cannot buy a minimum fill ... Refund the KAS", and the banner button of OrderRow runs a plain
//   CANCEL of the entry alone. Booked exits then cannot take profit until their rptUntil
//   (matcher.md 6.1 / 10.13).
// - S2. `canCancel` is `v.state_known && v.current !== null` for any order the indexer lists, and
//   `recordsToResolve` only resolves records the indexer does NOT list. When the indexer lists an
//   order whose current state it has not proven (`state_known: false`, for example a continuation
//   it could not splice, processor.rs derive_after -> None), the wallet's own record must still
//   be asked. So `recordsToResolve` includes records of views with state_known === false, and
//   buildEntries attaches the resolution so canCancel can use it. snapshotFor (actions.ts) falls
//   back to the record and the node and can cancel such an order.
import { describe, expect, it } from 'vitest';
import type { Clock } from '../../kob/plan-types';
import { recordsFromBuilt, type ResolvedRecord } from '../../kob/records';
import { loadKobNode } from '../../kob/wasm.node';
import { MAKER, orderViewOf, placeGolden } from '../../testing/chain-fixtures';
import { buildEntries, describeEntry, recordsToResolve } from './orders-model';

const kob = loadKobNode();
const CLOCK: Clock = { daa: 1000n, unixSeconds: 1_790_000_000n, rateMilli: 10_000 };
const ctx = { kob, clock: CLOCK };

describe('C5-S1: a repeat entry waiting for its exits is not an unfundable bid', () => {
  it('create.ifdBid.repeat with amount 0 left and re-arms available', () => {
    const p = placeGolden(kob, 'create.ifdBid.repeat');
    const view = orderViewOf(kob, p.snapshots[0], { status: 'partial', filled_amount: '5', amount_left: '0', repeat: { role: 'entry', rpt_amount: '6', rearm_amount: '5' } });
    const row = describeEntry(buildEntries([view], [])[0], ctx);
    expect(row.live).toBe(true);
    expect(row.typeKey).toBe('repeat');
    expect(row.unfundable).toBe(false);
  });
});

describe('C5-S2: an order whose indexer state is not proven can still be cancelled through the wallet record', () => {
  const p = placeGolden(kob, 'create.ask');
  const snap = p.snapshots[0];
  const [rec] = recordsFromBuilt(kob, p.built, { maker: MAKER.pk, network: 'testnet-10', placedAtUnix: 1_789_999_000n, placedAtDaa: 900n });
  const view = orderViewOf(kob, snap, { state_known: false, state: null });

  it('the record of such an order is resolved on the node', () => {
    expect(recordsToResolve([view], [rec]).map((r) => r.covenantId)).toEqual([rec.covenantId]);
  });

  it('and the row is cancellable once the record resolves live', () => {
    const resolved: ResolvedRecord = { status: 'live', covenantId: rec.covenantId, order: snap.order, custody: snap.custody, strays: [], stateChanged: false, checked: 2 };
    const [entry] = buildEntries([view], [rec], new Map([[rec.covenantId, resolved]]));
    const row = describeEntry(entry, ctx);
    expect(row.live).toBe(true);
    expect(row.canCancel).toBe(true);
  });
});
