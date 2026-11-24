// An order stored as an auction (a crossing limit's default shape, a Dutch order) keeps its start in
// `price` and its limit in `priceEnd`. An amend with a price must leave a plain fixed-price order, or
// it trades at the old limit at once (R9FC2-1). The decoder blocks the old construction, made at the
// transaction level, in the app decode and in the frame decode.
import { describe, expect, it } from 'vitest';
import { planCancelReplace, type CancelEnv, type OrderSnapshot } from '../../kob/cancel';
import { decodeSigning } from '../../kob/decode';
import type { BuiltTx, OrderState } from '../../kob/types';
import { loadKobNode } from '../../kob/wasm.node';
import { MAKER, keyUtxo, placeGolden } from '../../testing/chain-fixtures';
import { buildReplacement } from './amend';

const kob = loadKobNode();
const KAS = 100_000_000n;
const clone = <T>(v: T): T => JSON.parse(JSON.stringify(v)) as T;
const env = (): CancelEnv => ({ kob, maker: MAKER.pk, funding: [keyUtxo(MAKER.pk, 100n * KAS, 201)], tokenUtxos: [], clock: { daa: 78_000_000n } });

/** The golden order re-stated as an auction: start `start`, limit `limit`, origin long past. */
function auctionShaped(name: string, start: bigint, limit: bigint): OrderSnapshot {
  const snap = clone(placeGolden(kob, name).snapshots[0]!);
  const s = snap.order.state.state as unknown as Record<string, string>;
  s.price = start.toString();
  s.priceEnd = limit.toString();
  s.slope = '1000';
  s.decayStep = '1';
  s.activeFrom = '1000';
  return snap;
}

const amendOutput = (b: BuiltTx, cov: string) => b.tx.outputs.find((o) => o.covenant?.covenantId === cov)!;

/**
 * The old construction, made at the transaction level from the honest build: the amend output and
 * its record carry the new price with the schedule copied forward.
 */
function oldConstruction(snap: OrderSnapshot, honest: BuiltTx, price: bigint): BuiltTx {
  const b = clone(honest);
  const old = clone(snap.order.state) as OrderState & { state: Record<string, string> };
  old.state.price = price.toString();
  amendOutput(b, snap.covenantId).scriptPublicKey = kob.scriptPublicKey(old);
  const pay = kob.decodePayload(b.tx.payload as never)!;
  const records = pay.records.map((r) => (r.type === 'amend' ? { ...r, state: kob.encodeState(old) } : r));
  b.tx.payload = kob.encodePayload(records as never) as never;
  return b;
}

describe('an amend with a price leaves a fixed-price order (R9FC2-1)', () => {
  for (const [name, sell] of [['create.ask', true], ['create.bid', false]] as const) {
    for (const dir of ['up', 'down'] as const) {
      it(`${sell ? 'an ask' : 'a bid'} stored as an auction, amended ${dir}: slope 0, no end price, the price typed`, () => {
        const base = BigInt((placeGolden(kob, name).snapshots[0]!.order.state.state as unknown as { price: string }).price);
        const limit = base;
        const start = sell ? base * 2n : base / 2n;
        const snap = auctionShaped(name, start, limit);
        const want = dir === 'up' ? limit + limit / 10n : limit - limit / 10n;
        const r = buildReplacement(snap, 'limit', { price: want }, { kob });
        expect(r.ok).toBe(true);
        const ns = r.spec!.order.state as unknown as Record<string, string>;
        expect(ns.price).toBe(want.toString());
        expect(ns.slope).toBe('0');
        expect(ns.priceEnd).toBe('0');
        expect(r.issues.map((i) => i.code)).toContain('schedule-ends');
        // the order trades at exactly the price typed, now and later
        expect(kob.orderPriceAt(r.spec!.order, 100_000_000n, 1n)).toBe(want);
        expect(kob.orderPriceAt(r.spec!.order, 1_000n, 1n)).toBe(want);
      });
    }
  }

  it('the old construction (new price, schedule copied forward) trades at the old limit, and the decoder blocks it in the app and in the frame', () => {
    const base = BigInt((placeGolden(kob, 'create.ask').snapshots[0]!.order.state.state as unknown as { price: string }).price);
    const snap = auctionShaped('create.ask', base * 2n, base);
    const want = base + base / 10n;
    const old = clone(snap.order.state) as OrderState & { state: Record<string, string> };
    old.state.price = want.toString();
    // what the old amend wrote trades at the old limit once the origin is past
    expect(kob.orderPriceAt(old, 100_000_000n, 1n)).toBe(base);
    const r = buildReplacement(snap, 'limit', { price: want }, { kob });
    const good = planCancelReplace(env(), snap, r.spec!);
    expect(good.ok).toBe(true);
    const ok = (x: { blocking: { code: string }[] }) => x.blocking.map((i) => i.code);
    expect(ok(decodeSigning({ kob, built: good.built!, maker: MAKER.pk, registry: null }))).not.toContain('order-terms-invalid');
    const b = oldConstruction(snap, good.built!, want);
    expect(ok(decodeSigning({ kob, built: b, maker: MAKER.pk, registry: null, expected: { orders: [r.spec!.order] } }))).toContain('order-terms-invalid');
    expect(ok(decodeSigning({ kob, built: b, maker: MAKER.pk, registry: null }))).toContain('order-terms-invalid');
  });
});
