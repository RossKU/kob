// A transaction re-timed after the user confirmed it is signed without asking again only when the decoding of both shows the same orders with
// times not earlier and prices not worse for the user, the wallet's KAS and the fee within a tiny tolerance (retime.ts).
import { describe, expect, it } from 'vitest';
import { decodeSigning, type SigningSummary } from '../kob/decode';
import { planOrder } from '../kob/plan';
import type { OrderPlan } from '../kob/plan-types';
import { nodeFactsOf } from '../testing/chain-fixtures';
import { MAKER_PK, kob, level } from '../testing/fixtures';
import { buildIntent } from '../ui/ticket/form-state';
import { CASES, CTX, TOKEN, form, ticketEnv } from '../ui/ticket/ticket-fixtures';
import { auctionSubmitBy } from '../wallet/sign';
import { RETIME_MAX_DELAY_DAA, sameOrBetter } from './retime';

const K = kob();
const decode = (p: OrderPlan): SigningSummary => decodeSigning({ kob: K, built: p.built!, maker: MAKER_PK, registry: null, nodeInputs: nodeFactsOf(p.built!) });
const at = (daa: bigint, o: Parameters<typeof ticketEnv>[0] = {}) => {
  const env = ticketEnv(o);
  return { ...env, clock: { ...env.clock, daa: env.clock.daa + daa, unixSeconds: env.clock.unixSeconds + daa / 10n } };
};
const market = buildIntent(form('market', 'buy', { amount: '2' }), CTX).intent!;

describe('sameOrBetter: a re-time is bounded in distance (audit 2026-10-29 S3)', () => {
  it('times later by a slow approval pass, by more than an hour of DAA they do not', () => {
    const approved = decode(planOrder(at(0n), market));
    expect(sameOrBetter(approved, decode(planOrder(at(150n), market)))).toBe(true);
    expect(sameOrBetter(approved, decode(planOrder(at(RETIME_MAX_DELAY_DAA + 1_000n), market)))).toBe(false);
  });
});

describe('sameOrBetter', () => {
  it('the same market buy planned 15 s later (a later auction start and life): no second confirmation', () => {
    const approved = decode(planOrder(at(0n), market));
    const later = decode(planOrder(at(150n), market));
    expect(later.orders[0]!.description.activeFrom).toBeGreaterThan(approved.orders[0]!.description.activeFrom);
    expect(sameOrBetter(approved, later)).toBe(true);
    // never the other way round: an earlier start is not what was approved
    expect(sameOrBetter(later, approved)).toBe(false);
  });

  it('a market buy of several fills (a resting auction) planned 15 s later: only its times moved, no second confirmation', () => {
    const multi = buildIntent(form('market', 'buy', { amount: '10', maxFills: '3' }), CTX).intent!;
    const a = planOrder(at(0n), multi);
    expect(auctionSubmitBy(a.states, at(0n).clock.daa)).not.toBeNull();
    const approved = decode(a);
    const later = decode(planOrder(at(150n), multi));
    expect(approved.orders[0]!.description.tif).toBe('gtc');
    expect(later.orders[0]!.description.expiryDaa - approved.orders[0]!.description.expiryDaa).toBe(150n);
    expect(sameOrBetter(approved, later)).toBe(true);
    expect(sameOrBetter(later, approved)).toBe(false);
  });

  // the ticket re-times an order with an auction starting now (wallet/sign.ts `auctionSubmitBy`: the only orders re-timed) on the environment it
  // was planned on (OrderTicket.tsx `retimer`): only the clock moves, by a long read of the confirmation (10 to 60 s) or a slow wallet (its
  // signing time, capped at 60 s, plus three re-timing steps: ui/confirm/signing-time.ts, up to 70 s) plus the re-timing lead;
  // whatever the type, the signer then signs it without a second prompt
  const KAS_TYPED = { ...form('market', 'buy', { amount: '20' }), kas: true };
  for (const [name, f] of [...CASES.map(([n, type, side, values]) => [n, form(type, side, values)] as const), ['market buy typed in KAS', KAS_TYPED] as const]) {
    for (const seconds of [10n, 60n, 70n]) {
      it(`${name}: the same plan ${seconds} s later needs no second confirmation`, () => {
        const intent = buildIntent(f, CTX).intent;
        if (!intent) return;
        const first = planOrder(at(0n), intent);
        if (!first.ok || !first.built || auctionSubmitBy(first.states, at(0n).clock.daa) === null) return;
        const again = planOrder(at(seconds * 10n + 30n), intent);
        expect(sameOrBetter(decode(first), decode(again))).toBe(true);
      });
    }
  }

  it('a dearer book (a higher start, a higher worst price) is shown again; a cheaper one is not', () => {
    const approved = decode(planOrder(at(0n), market));
    const dearer = { book: { asks: [level(260_000_000n, 100n * TOKEN, 1)], bids: [level(245_000_000n, 10n * TOKEN, 1)] } };
    expect(sameOrBetter(approved, decode(planOrder(at(150n, dearer), market)))).toBe(false);
    const cheaper = { book: { asks: [level(240_000_000n, 100n * TOKEN, 1)], bids: [level(235_000_000n, 10n * TOKEN, 1)] } };
    // a cheaper start buys the same amount with a smaller escrow: better for the user
    expect(sameOrBetter(approved, decode(planOrder(at(150n, cheaper), market)))).toBe(true);
  });

  it('another amount, side or limit is shown again', () => {
    const approved = decode(planOrder(at(0n), market));
    expect(sameOrBetter(approved, decode(planOrder(at(150n), buildIntent(form('market', 'buy', { amount: '3' }), CTX).intent!)))).toBe(false);
    const limit = (price: string) => decode(planOrder(at(0n), buildIntent(form('limit', 'sell', { amount: '5', price }), CTX).intent!));
    expect(sameOrBetter(limit('2.6'), limit('2.6'))).toBe(true);
    expect(sameOrBetter(limit('2.6'), limit('2.5'))).toBe(false); // a sell at a lower price is worse
    expect(sameOrBetter(limit('2.6'), decode(planOrder(at(0n), buildIntent(form('limit', 'buy', { amount: '5', price: '2.3' }), CTX).intent!)))).toBe(false);
  });
});
