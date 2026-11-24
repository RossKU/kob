// Standalone chain time (R9FB3-1, refined): the last verified present as a witness, a node the user
// named, and takes of fixed-price orders, which sign no time that moves money.
import { afterEach, describe, expect, it } from 'vitest';
import { ANCHOR_MAX_AGE_MS, anchorTolerance, clearAnchor, currentAnchor, judgePresent, recordAnchor, DAA_PER_SECOND } from './chain-present';
import { planOrder } from './plan';
import type { BookOrderUtxo, BookView, Clock, PlanEnv } from './plan-types';
import type { HealthView } from '../data/indexer-types';
import { CLOCK, KAS, OTHER_PK, TOK, keyUtxo, makeEnv, market3x3, tokenUtxo } from '../testing/fixtures';
import { snapshotOfRecovered } from '../testing/chain-fixtures';

const NOW = Number(CLOCK.unixSeconds) * 1000;
const health = (o: Partial<HealthView> = {}): HealthView => ({ ok: true, network: 'testnet-10', cursor_daa: Number(CLOCK.daa), cursor_time_unix_ms: NOW, node_daa: null, lag_daa: 0, lag_seconds: 0, settle_depth_daa: 100, ...o } as HealthView);
afterEach(() => clearAnchor());

/** The clock a planner gets with the indexer down, the anchor taken `ageMs` before, the node off by `offDaa`. */
function anchored(ageMs: number, offDaa = 0n): Clock {
  recordAnchor(health(), NOW - ageMs);
  const nodeDaa = CLOCK.daa + BigInt(Math.floor(ageMs / 1000)) * DAA_PER_SECOND + offDaa;
  const p = judgePresent(nodeDaa, { indexer: null, floor: null, anchor: currentAnchor(NOW), nowMs: NOW });
  return { ...CLOCK, daa: nodeDaa, witness: p.verdict, witnessReason: p.reason, toleranceDaa: p.toleranceDaa };
}
const codes = (env: PlanEnv, intent: unknown) => planOrder(env, intent as never).issues.map((i) => i.code);
const MARKET = { type: 'market', side: 'sell', amount: 5n * TOK, tip: 100_000n };
const GTD = { type: 'limit', side: 'sell', amount: 5n * TOK, price: 900_000_000n, lifetime: { kind: 'gtd', at: CLOCK.unixSeconds + 3n * 86_400n } };

describe('the anchor', () => {
  it('is kept only from an indexer at the tip, and read back bounded', () => {
    recordAnchor(health({ lag_seconds: 30 }), NOW);
    expect(currentAnchor(NOW)).toBeNull();
    recordAnchor(health(), NOW);
    expect(currentAnchor(NOW)).toMatchObject({ daa: CLOCK.daa });
    // never in the future, never older than 7 days
    expect(currentAnchor(NOW - 1)).toBeNull();
    expect(currentAnchor(NOW + ANCHOR_MAX_AGE_MS + 1)).toBeNull();
  });

  it('its tolerance is seconds minutes after, about half an hour after a day', () => {
    expect(anchorTolerance(2 * 60_000) / DAA_PER_SECOND).toBe(7n);
    expect(anchorTolerance(86_400_000) / DAA_PER_SECOND).toBe(1_733n);
  });

  it('a node off by more than the tolerance disagrees', () => {
    expect(anchored(60_000, 20n * DAA_PER_SECOND).witness).toBe('disagree');
    expect(anchored(60_000, 3n * DAA_PER_SECOND).witness).toBe('anchored');
  });
});

describe('orders with the indexer down and an anchor', () => {
  it('a market order a minute after the outage is built, marked; an hour after it is refused as too old', () => {
    expect(codes(makeEnv({ clock: anchored(60_000) }), MARKET)).toContain('CHAIN_TIME_ANCHORED');
    expect(codes(makeEnv({ clock: anchored(3_600_000) }), MARKET)).toContain('CHAIN_TIME_ANCHOR_TOO_OLD');
  });
  it('a GTD of days is built with a day-old anchor', () => {
    const c = codes(makeEnv({ clock: anchored(86_400_000) }), GTD);
    expect(c).toContain('CHAIN_TIME_ANCHORED');
    expect(c).not.toContain('CHAIN_TIME_ANCHOR_TOO_OLD');
  });
  it('a node far from the anchor builds nothing', () => {
    expect(codes(makeEnv({ clock: anchored(60_000, 3_600n * DAA_PER_SECOND) }), GTD)).toContain('CHAIN_TIME_DISAGREE');
  });
});

describe('a node the user named, and no witness at all', () => {
  it('the user\'s node: timed orders are built, marked as from that node', () => {
    const p = judgePresent(CLOCK.daa, { indexer: null, floor: null, anchor: null, nowMs: NOW, userNode: true });
    const env = makeEnv({ clock: { ...CLOCK, witness: p.verdict, witnessReason: p.reason } });
    const c = codes(env, MARKET);
    expect(c).toContain('CHAIN_TIME_OWN_NODE');
    expect(c).not.toContain('CHAIN_TIME_UNVERIFIED_SHORT');
  });

  /** A take of OTHER_PK's resting ask, fixed-price or decaying, with no witness at all. */
  function takeEnv(decaying: boolean): { env: PlanEnv; intent: unknown } {
    const M = market3x3();
    const maker: PlanEnv = { ...makeEnv({ market: M, book: { asks: [], bids: [] } }), maker: OTHER_PK, funding: [keyUtxo(10_000n * KAS, OTHER_PK)], tokenUtxos: [tokenUtxo(M, 1_000n * TOK, 10n * KAS, OTHER_PK)] };
    const placed = planOrder(maker, decaying
      ? ({ type: 'dutch', side: 'sell', amount: 3n * TOK, price: 300_000_000n, priceEnd: 250_000_000n, duration: { seconds: 600n }, minFill: TOK } as never)
      : ({ type: 'limit', side: 'sell', amount: 3n * TOK, price: 250_000_000n, minFill: TOK } as never));
    const snap = snapshotOfRecovered(maker.kob.recoverOrders(placed.built!.tx)[0]!, CLOCK.daa - 1000n);
    const row = { covenantId: snap.covenantId, price: 250_000_000n, amount: 3n * TOK, tip: 0n };
    const book: BookView = { asks: [{ price: row.price, amount: row.amount, orders: 1 }], bids: [], askOrders: [row], bidOrders: [] };
    const u: BookOrderUtxo = { covenantId: snap.covenantId, order: snap.order, custody: snap.custody };
    const env = { ...makeEnv({ book, clock: { ...CLOCK, witness: 'unverified', witnessReason: 'none' } }), bookOrders: new Map([[snap.covenantId.toLowerCase(), u]]) };
    return { env, intent: { type: 'limit', side: 'buy', amount: 2n * TOK, price: 310_000_000n, crossing: 'take' } };
  }
  it('a take of a fixed-price order needs no witness; one of a decaying order is refused', () => {
    const fixed = takeEnv(false);
    const p = planOrder(fixed.env, fixed.intent as never);
    expect(p.take, JSON.stringify(p.issues.map((i) => i.code))).toBeTruthy();
    expect(p.issues.map((i) => i.code).filter((c) => c.startsWith('CHAIN_TIME'))).toEqual([]);
    const dec = takeEnv(true);
    const q = planOrder(dec.env, dec.intent as never);
    expect(q.issues.map((i) => i.code)).toContain('CHAIN_TIME_UNVERIFIED_SHORT');
  });
});
