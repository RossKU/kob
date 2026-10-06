import { describe, expect, it } from 'vitest';
import type { OrderSnapshot } from '../../kob/cancel';
import { MAX_IDLE_DAA, daaToUnix, gtcRenewalUnix } from '../../kob/daa';
import type { Clock } from '../../kob/plan-types';
import { recordsFromBuilt, type ResolvedRecord } from '../../kob/records';
import type { AnyState, OrderState } from '../../kob/types';
import { loadKobNode } from '../../kob/wasm.node';
import { MAKER, orderViewOf, placeGolden } from '../../testing/chain-fixtures';
import { buildEntries, countByStatus, describeEntry, entryState, recordsToResolve, type OrderTypeKey } from './orders-model';

const kob = loadKobNode();
const CLOCK: Clock = { daa: 1000n, unixSeconds: 1_790_000_000n, rateMilli: 10_000 };
const ctx = { kob, clock: CLOCK };

const rowOf = (name: string, over = {}) => {
  const p = placeGolden(kob, name);
  const [e] = buildEntries([orderViewOf(kob, p.snapshots[0], over)], []);
  return { p, row: describeEntry(e, ctx) };
};

describe('orderTypeKey via describeEntry (real golden orders)', () => {
  const cases: [string, OrderTypeKey, 'sell' | 'buy'][] = [
    ['create.ask', 'limit-gtd', 'sell'],
    ['create.ask.day', 'limit-day', 'sell'],
    ['create.ask.twap', 'twap', 'sell'],
    ['create.ask.dutch', 'dutch', 'sell'],
    ['create.ask.market', 'market', 'sell'],
    ['create.bid', 'limit-gtd', 'buy'],
    ['create.bid.dca', 'dca', 'buy'],
    ['create.bid.market', 'market', 'buy'],
    ['create.condAsk', 'oco', 'sell'],
    ['create.condBid', 'oco', 'buy'],
    ['create.ifdBid', 'ifo', 'buy'],
    ['create.ifdBid.repeat', 'repeat', 'buy'],
    ['create.ifdAsk', 'ifo', 'sell'],
    ['create.ifdAsk.repeat', 'repeat', 'sell'],
  ];
  for (const [name, type, side] of cases) {
    it(`${name} is ${type} (${side})`, () => {
      const { row } = rowOf(name);
      expect(row.typeKey).toBe(type);
      expect(row.side).toBe(side);
      expect(row.live).toBe(true);
    });
  }

  it('a stop entry keeps its trigger price and armed flag', () => {
    const { row } = rowOf('create.ifdBid.stopEntry');
    expect(row.stopPrice).toBe(255_000_000n);
    expect(row.armed).toBe(false);
  });

  it('a GTC limit is recognised from expiry = genesis + 90 days, a shorter one is GTD', () => {
    const p = placeGolden(kob, 'create.ask');
    const snap = p.snapshots[0];
    const genesis = Number(snap.order.blockDaaScore);
    const withExpiry = (daa: bigint): OrderSnapshot => {
      const st = snap.order.state as Extract<OrderState, { kind: 'KobAsk' | 'KobAskKron' }>;
      return { ...snap, order: { ...snap.order, state: { ...st, state: { ...st.state, expiryDaa: daa.toString() } } } };
    };
    const gtc = describeEntry(buildEntries([orderViewOf(kob, withExpiry(BigInt(genesis) + MAX_IDLE_DAA), { last_daa: 1000 })], [])[0], ctx);
    expect(gtc.typeKey).toBe('limit');
    expect(gtc.expiry.kind).toBe('gtc');
    const gtd = describeEntry(buildEntries([orderViewOf(kob, withExpiry(BigInt(genesis) + MAX_IDLE_DAA / 2n))], [])[0], ctx);
    expect(gtd.typeKey).toBe('limit-gtd');
    expect(gtd.expiry.kind).toBe('gtd');
    expect(gtd.expiry.approxUnix).toBe(daaToUnix(CLOCK, BigInt(genesis) + MAX_IDLE_DAA / 2n));
  });
});

describe('prices, amounts and expiry', () => {
  it('shows the all-in price: a buy pays limit + tip, a sell receives limit - tip', () => {
    const sell = rowOf('create.ask').row;
    expect(sell.price).toBe(250_000_000n);
    expect(sell.tip).toBe(100_000n);
    expect(sell.allInPrice).toBe(249_900_000n);
    const buy = rowOf('create.bid').row;
    expect(buy.price).toBe(245_000_000n);
    expect(buy.allInPrice).toBe(245_100_000n);
  });

  it('never lets a sell tip push the all-in price below zero', () => {
    const p = placeGolden(kob, 'create.ask');
    const st = p.snapshots[0].order.state as Extract<OrderState, { kind: 'KobAsk' | 'KobAskKron' }>;
    const snap: OrderSnapshot = { ...p.snapshots[0], order: { ...p.snapshots[0].order, state: { ...st, state: { ...st.state, tip: '999999999' } } } };
    expect(describeEntry(buildEntries([orderViewOf(kob, snap)], [])[0], ctx).allInPrice).toBe(0n);
  });

  it('reports the amount left / total in base units, estimates for bids, and the day-order deadline', () => {
    const ask = rowOf('create.ask', { filled_amount: '4', amount_left: '6', initial_amount: '10', status: 'partial' }).row;
    expect(ask).toMatchObject({ amountTotal: 10n, amountLeft: 6n, status: 'partial', live: true, amountEstimated: false });
    const bid = rowOf('create.bid', { amount_estimated: true }).row;
    expect(bid.amountEstimated).toBe(true);
    // the order's scale and minimum fill come from its state
    expect(ask.scale).toBeGreaterThan(0n);
    expect(ask.minFill).toBeGreaterThan(0n);
    const day = rowOf('create.ask.day').row;
    expect(day.expiry.kind).toBe('day');
    expect(day.expiry.deadlineUnix).not.toBeNull();
  });

  it('a timed order that has not started reports its start', () => {
    const p = placeGolden(kob, 'create.ask.dutch'); // activeFrom 1000 with clock daa 1000: already started
    const st = p.snapshots[0].order.state as Extract<OrderState, { kind: 'KobAsk' | 'KobAskKron' }>;
    const future: OrderSnapshot = { ...p.snapshots[0], order: { ...p.snapshots[0].order, state: { ...st, state: { ...st.state, slope: '0', priceEnd: '0', activeFrom: '5000' } } } };
    const row = describeEntry(buildEntries([orderViewOf(kob, future)], [])[0], ctx);
    expect(row.typeKey).toBe('timed');
    expect(row.startsAtDaa).toBe(5000n);
  });
});

describe('bids that can no longer fund one base unit, and orders the token program may have frozen', () => {
  it('a live bid with amount 0 left is unfundable, can be cancelled (refunded) and amended with a top-up', () => {
    const { row } = rowOf('create.bid', { status: 'partial', amount_left: '0', amount_estimated: true });
    expect(row).toMatchObject({ unfundable: true, live: true, canCancel: true, canAmend: true, amountLeft: 0n, amountEstimated: true });
  });

  it('a funded bid, an ask, a finished bid and an unknown amount are not unfundable', () => {
    expect(rowOf('create.bid', { amount_left: '3' }).row.unfundable).toBe(false);
    expect(rowOf('create.bid', { amount_left: null, amount_estimated: true }).row.unfundable).toBe(false);
    expect(rowOf('create.ask', { amount_left: '0' }).row.unfundable).toBe(false);
    expect(rowOf('create.bid', { amount_left: '0', status: 'filled' }).row.unfundable).toBe(false);
  });

  it('possibly_frozen marks a live order and only that; it stays cancellable', () => {
    const frozen = rowOf('create.ask', { possibly_frozen: true }).row;
    expect(frozen).toMatchObject({ possiblyFrozen: true, live: true, canCancel: true });
    expect(rowOf('create.ask').row.possiblyFrozen).toBe(false);
    expect(rowOf('create.ask', { possibly_frozen: false }).row.possiblyFrozen).toBe(false);
    expect(rowOf('create.ask', { possibly_frozen: true, status: 'cancelled' }).row.possiblyFrozen).toBe(false);
  });
});

describe('status, refund and renewal', () => {
  it('finished orders are not live and cannot be cancelled', () => {
    for (const status of ['filled', 'cancelled', 'refunded', 'closed'] as const) {
      const { row } = rowOf('create.ask', { status });
      expect(row).toMatchObject({ status, live: false, canCancel: false, canRefund: false, canAmend: false });
    }
    expect(rowOf('create.ask', { status: 'weird' as never }).row.status).toBe('unknown');
  });

  it('an order is refundable from its due DAA on, and only while live', () => {
    expect(rowOf('create.ask', { refund_due_daa: 999 }).row.canRefund).toBe(true);
    expect(rowOf('create.ask', { refund_due_daa: 1000 }).row.canRefund).toBe(true); // due exactly now
    expect(rowOf('create.ask', { refund_due_daa: 1001 }).row.canRefund).toBe(false);
    expect(rowOf('create.ask', { refund_due_daa: 10, status: 'cancelled' }).row.canRefund).toBe(false);
    expect(rowOf('create.ask', { expired: true }).row.expired).toBe(true);
  });

  // C5 W-15: between its deadline (00:00 UTC) and its on-chain expiry a day order is ended (matchers stop at the deadline), not live
  it('a day order past its deadline reads ended (refund pending), from the indexer flag or the clock', () => {
    expect(rowOf('create.ask.day', { deadline_passed: true }).row.expired).toBe(true);
    expect(rowOf('create.ask.day', { deadline: Number(CLOCK.unixSeconds) - 1, deadline_passed: false }).row.expired).toBe(true);
    expect(rowOf('create.ask.day', { deadline: Number(CLOCK.unixSeconds) + 60, deadline_passed: false }).row.expired).toBe(false);
    expect(rowOf('create.ask.day', { deadline_passed: true, status: 'cancelled' }).row.expired).toBe(false);
  });

  it('cancel needs proven state and a current outpoint', () => {
    expect(rowOf('create.ask').row.canCancel).toBe(true);
    expect(rowOf('create.ask', { state_known: false }).row.canCancel).toBe(false);
    expect(rowOf('create.ask', { current: null }).row.canCancel).toBe(false);
  });

  // C5 W-3: the reminder comes from the on-chain expiry (a GTC's expiryDaa = placement + 90 days; fills do not extend it). This test pinned
  // "counted from the last activity", which is the wrong rule (min(expiryDaa, utxoDaa + 90 days)); with no fill both give the same date.
  it('GTC renewal is day 85 of the on-chain expiry (placement + 90 days), and turns due when the time comes', () => {
    const p = placeGolden(kob, 'create.ask');
    const st = p.snapshots[0].order.state as Extract<OrderState, { kind: 'KobAsk' | 'KobAskKron' }>;
    const genesis = Number(p.snapshots[0].order.blockDaaScore);
    const snap: OrderSnapshot = { ...p.snapshots[0], order: { ...p.snapshots[0].order, state: { ...st, state: { ...st.state, expiryDaa: (BigInt(genesis) + MAX_IDLE_DAA).toString() } } } };
    const view = orderViewOf(kob, snap, { last_daa: 1000 });
    const early = describeEntry(buildEntries([view], [])[0], ctx);
    expect(early.renewalUnix).toBe(gtcRenewalUnix(daaToUnix(CLOCK, BigInt(genesis))));
    expect(early.renewalDue).toBe(false);
    // the chain advances 85 days (10 DAA per second): the reminder is due exactly then, not a second before
    const at = (secs: bigint): Clock => ({ daa: 1000n + secs * 10n, unixSeconds: CLOCK.unixSeconds + secs, rateMilli: 10_000 });
    const later = describeEntry(buildEntries([view], [])[0], { kob, clock: at(85n * 86_400n) });
    const justBefore = describeEntry(buildEntries([view], [])[0], { kob, clock: at(85n * 86_400n - 1n) });
    expect(justBefore.renewalDue).toBe(false);
    expect(later.renewalDue).toBe(true);
  });
});

describe('merging with placement records', () => {
  const placed = placeGolden(kob, 'create.ask');
  const [rec] = recordsFromBuilt(kob, placed.built, { maker: MAKER.pk, network: 'testnet-10', placedAtUnix: 1_789_990_000n, placedAtDaa: 900n, label: 'limit sell' });
  const view = orderViewOf(kob, placed.snapshots[0]);

  it('a record the indexer knows is attached, not duplicated; unknown ones are listed for node resolution', () => {
    const other = { ...rec, covenantId: 'ab'.repeat(32) };
    expect(buildEntries([view], [rec, other]).map((e) => [e.id, e.view !== null, e.record !== null])).toEqual(
      expect.arrayContaining([[rec.covenantId, true, true], [other.covenantId, false, true]]),
    );
    expect(buildEntries([view], [rec, other])).toHaveLength(2);
    expect(recordsToResolve([view], [rec, other]).map((r) => r.covenantId)).toEqual([other.covenantId]);
  });

  it('sorts newest first', () => {
    const older = { ...rec, covenantId: 'cd'.repeat(32), placedAtDaa: '10' };
    const newer = { ...rec, covenantId: 'ef'.repeat(32), placedAtDaa: '5000' };
    expect(buildEntries([], [older, newer]).map((e) => e.id)).toEqual([newer.covenantId, older.covenantId]);
  });

  const live: ResolvedRecord = { status: 'live', covenantId: rec.covenantId, order: placed.snapshots[0].order, custody: placed.snapshots[0].custody, strays: [], stateChanged: false, checked: 1 };

  it('a fresh placement the indexer has not seen yet still shows up, cancellable once resolved live on the node', () => {
    const [e] = buildEntries([], [rec], new Map([[rec.covenantId, live]]));
    const row = describeEntry(e, ctx);
    expect(row).toMatchObject({ status: 'open', live: true, source: 'node', canCancel: true, side: 'sell', typeKey: 'limit-gtd', label: 'limit sell', placementTx: placed.txid });
    expect(row.price).toBe(250_000_000n);
  });

  it('a record whose UTXO is gone is closed; an unresolved one is unknown (nothing can be cancelled)', () => {
    const spent = describeEntry(buildEntries([], [rec], new Map([[rec.covenantId, { ...live, status: 'spent' as const, order: null, custody: null }]]))[0], ctx);
    expect(spent).toMatchObject({ status: 'closed', live: false, canCancel: false });
    const unknown = describeEntry(buildEntries([], [rec])[0], ctx);
    expect(unknown).toMatchObject({ status: 'unknown', live: false, canCancel: false, source: 'record' });
    expect(unknown.price).toBe(250_000_000n); // the recorded terms still describe it
  });

  it('entryState prefers the proven indexer state over the node and the record', () => {
    const [e] = buildEntries([view], [rec], new Map());
    expect(entryState(e, kob)?.source).toBe('indexer');
    const [n] = buildEntries([], [rec], new Map([[rec.covenantId, live]]));
    expect(entryState(n, kob)?.source).toBe('node');
    expect(entryState(buildEntries([], [rec])[0], kob)?.source).toBe('record');
    expect(entryState(buildEntries([], [rec])[0], null)).toBeNull();
  });

  it('counts by status', () => {
    const rows = [
      describeEntry(buildEntries([view], [])[0], ctx),
      describeEntry(buildEntries([orderViewOf(kob, placed.snapshots[0], { status: 'filled' })], [])[0], ctx),
      describeEntry(buildEntries([], [rec])[0], ctx),
    ];
    expect(countByStatus(rows)).toEqual({ live: 1, done: 1, unknown: 1 });
  });

  it('typing survives a view without a decoded state (list view of an older indexer)', () => {
    const row = describeEntry(buildEntries([{ ...view, state: undefined, state_known: false }], [])[0], ctx);
    expect(row.typeKey).toBe('unknown');
    expect(row.canCancel).toBe(false);
    expect((view.state as AnyState).kind).toBe('KobAsk');
  });
});
