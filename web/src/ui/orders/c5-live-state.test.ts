// C5 liveness (row states of My orders). Findings pinned here (the sibling c5-live-orders.test.ts covers `killed`, list truncation, stray slots):
//   C5-S1  a SOLD-OUT buy-first REPEAT entry that waits for its exits has `amount_left: '0'` (indexer: a repeating entry that sold out stays
//          `partial` with nothing left, executor.md). orders-model `unfundable: live && side === 'buy' && amount_left === 0` flagged it as
//          "cannot buy a minimum fill ... Refund the KAS", and OrderRow's banner button runs a plain CANCEL of the entry alone:
//          booked exits then cannot take profit until their rptUntil (matcher.md 6.1 / 10.13). A healthy position is shown as a broken order.
//   C5-S2  `canCancel` is `v.state_known && v.current !== null` for any order the indexer lists, and `recordsToResolve` only resolves records the
//          indexer does NOT list. When the indexer lists an order whose current state it has not proven (`state_known: false`, e.g. a continuation
//          it could not splice, processor.rs derive_after -> None) the wallet's own record is never asked: Cancel stays disabled although
//          snapshotFor (actions.ts) WOULD fall back to the record + node and could cancel it.
// Uncompiled when written (2026-10-02).
// C5: expected to fail until (S1) unfundable excludes repeat entries (`repeat?.role === 'entry'`), (S2) recordsToResolve includes records of views
// with state_known === false and buildEntries attaches the resolution so canCancel can use it.
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
