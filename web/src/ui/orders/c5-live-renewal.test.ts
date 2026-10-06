// C5 liveness (GTC): the on-chain refund of any KobAsk / KobBid / Cond* / Ifd* / Cross is due at min(expiryDaa, UTXO DAA + 90 days)
// (kob-protocol state.rs `refund_due`, KobAsk.sil line 193-202), and the wallet writes a GTC's `expiryDaa` as PLACEMENT DAA + 90 days
// (kob/daa.ts `expiryFor('gtc')`). Fills do not move `expiryDaa`, so a partly filled GTC still ends 90 days after PLACEMENT, not after its last fill.
// orders-model.ts computes the "renew before day 85" reminder from the LAST FILL (`lastDaa`) instead of from the expiry, so for any GTC that
// had a fill the reminder comes up to 85 days too late: the order is refunded / ignored by matchers (matcher.md 2.3: `t >= expiryDaa`)
// before the user is ever told to renew. (The existing orders-model.test.ts 'GTC renewal is day 85 counted from the last activity' pins the
// wrong semantics; ticket.help.lifetime says "90 days without activity" too.) Amend (`limitReplacement`: `{...s, price, ...}`) keeps the old
// `expiryDaa` as well, so even a "renewal" by Amend does not extend the life: see the report.
// Uncompiled when written (2026-10-02).
// C5: expected to fail until the reminder is derived from `renewalUnixForExpiry(clock, expiryDaa)` (daa.ts, already exists and is unused).
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
