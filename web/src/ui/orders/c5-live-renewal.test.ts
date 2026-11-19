// Tests the GTC renewal reminder of My orders against the real refund deadline.
//
// The on-chain refund of any KobAsk / KobBid / Cond* / Ifd* / Cross is due at
// min(expiryDaa, UTXO DAA + 90 days) (kob-protocol state.rs `refund_due`, KobAsk.sil). The wallet
// writes a GTC's `expiryDaa` as PLACEMENT DAA + 90 days (kob/daa.ts `expiryFor('gtc')`). Fills do
// not move `expiryDaa`, so a partly filled GTC still ends 90 days after PLACEMENT, not after its
// last fill.
//
// The "renew before day 85" reminder of orders-model.ts is therefore derived from
// `renewalUnixForExpiry(clock, expiryDaa)` (daa.ts), not from the LAST FILL (`lastDaa`). A
// reminder counted from the last fill would come up to 85 days too late, after the order is
// refunded or ignored by matchers (matcher.md 2.3: `t >= expiryDaa`). Amend (`limitReplacement`:
// `{...s, price, ...}`) keeps the old `expiryDaa`, so an Amend does not extend the life either.
import { describe, expect, it } from 'vitest';
import type { OrderSnapshot } from '../../kob/cancel';
import { MAX_IDLE_DAA, renewalUnixForExpiry } from '../../kob/daa';
import type { Clock } from '../../kob/plan-types';
import type { OrderState } from '../../kob/types';
import { loadKobNode } from '../../kob/wasm.node';
import { orderViewOf, placeGolden } from '../../testing/chain-fixtures';
import { buildEntries, describeEntry } from './orders-model';

const kob = loadKobNode();
const CLOCK: Clock = { daa: 1000n, unixSeconds: 1_790_000_000n, rateMilli: 10_000 };
const DAY_DAA = 86_400 * 10;

describe('C5-N1: the GTC renewal reminder is computed from the on-chain expiry', () => {
  const p = placeGolden(kob, 'create.ask');
  const genesis = BigInt(p.snapshots[0].order.blockDaaScore ?? 0);
  const expiryDaa = genesis + MAX_IDLE_DAA;
  const st = p.snapshots[0].order.state as Extract<OrderState, { kind: 'KobAsk' | 'KobAskKron' }>;
  const snap: OrderSnapshot = { ...p.snapshots[0], order: { ...p.snapshots[0].order, state: { ...st, state: { ...st.state, expiryDaa: expiryDaa.toString() } } } };

  it('a GTC partly filled on day 80 must be renewed by day 85 AFTER PLACEMENT, not 85 days after the fill', () => {
    const view = orderViewOf(kob, snap, { status: 'partial', filled_amount: '1', last_daa: Number(genesis) + 80 * DAY_DAA });
    const row = describeEntry(buildEntries([view], [])[0], { kob, clock: CLOCK });
    expect(row.live).toBe(true);
    expect(row.expiry.kind).toBe('gtc');
    expect(row.renewalUnix).not.toBeNull();
    expect(row.renewalUnix!).toBeLessThanOrEqual(renewalUnixForExpiry(CLOCK, expiryDaa));
  });

  // control: passes today (no fill after placement), must keep passing after the fix
  it('with no fill at all the reminder is the same date (placement + 85 days)', () => {
    const view = orderViewOf(kob, snap, { last_daa: Number(genesis) });
    const row = describeEntry(buildEntries([view], [])[0], { kob, clock: CLOCK });
    expect(row.renewalUnix).toBe(renewalUnixForExpiry(CLOCK, expiryDaa));
  });
});
