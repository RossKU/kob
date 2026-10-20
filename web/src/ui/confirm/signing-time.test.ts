// The lead an auction gets from a wallet's signing time (signing-time.ts): a re-timing after `clock-moved` leaves at least the measured signing
// time, a slow wallet's first attempt starts later by its shortfall against the window, a measured lead is capped, a fast wallet (the
// private-key signer) is unaffected, the memory works without storage, and the later plan keeps every term the user approved.
import { afterEach, describe, expect, it, vi } from 'vitest';
import { decodeSigning } from '../../kob/decode';
import { planOrder } from '../../kob/plan';
import type { Clock, OrderPlan } from '../../kob/plan-types';
import { sameOrBetter } from '../../signer/retime';
import { nodeFactsOf } from '../../testing/chain-fixtures';
import { MAKER_PK, kob } from '../../testing/fixtures';
import { auctionSubmitBy } from '../../wallet/sign';
import { buildIntent } from '../ticket/form-state';
import { CTX, form, ticketEnv } from '../ticket/ticket-fixtures';
import {
  AUCTION_WINDOW_DAA, MAX_MEASURED_LEAD_DAA, RETIME_LEAD_DAA, SigningTimer, daaOfMs, firstAttemptLead, forgetSigningTimes, laterClock,
  rememberSigningMs, rememberedSigningMs, retimeLead,
} from './signing-time';

afterEach(() => {
  forgetSigningTimes();
  vi.unstubAllGlobals();
});

describe('daaOfMs / laterClock', () => {
  it('converts at the measured rate, rounded up, and at 10 DAA/s without one', () => {
    expect(AUCTION_WINDOW_DAA).toBe(50n);
    expect(daaOfMs(19_000)).toBe(190n);
    expect(daaOfMs(19_000, 10_000)).toBe(190n);
    expect(daaOfMs(19_000, 9_500)).toBe(181n); // 180.5 up
    expect(daaOfMs(1)).toBe(1n);
    expect(daaOfMs(0)).toBe(0n);
    expect(daaOfMs(-5)).toBe(0n);
    expect(daaOfMs(NaN)).toBe(0n);
    expect(daaOfMs(1_000, 0)).toBe(10n);
  });

  it('moves the DAA and the unix time together, only later', () => {
    const c: Clock = { daa: 1_000n, unixSeconds: 5_000n, rateMilli: 10_000 };
    expect(laterClock(c, 170n)).toEqual({ daa: 1_170n, unixSeconds: 5_017n, rateMilli: 10_000 });
    expect(laterClock({ ...c, rateMilli: 9_999.6 }, 100n).unixSeconds).toBe(5_010n);
    expect(laterClock(c, 0n)).toEqual(c);
  });
});

describe('retimeLead: a re-timing from the measured signing time', () => {
  it('keeps the old step when nothing was measured or the signature was quick', () => {
    expect(retimeLead(30n, undefined)).toBe(30n);
    expect(retimeLead(30n, 0)).toBe(30n);
    expect(retimeLead(60n, 2_000)).toBe(60n); // 20 + 30 < 60
  });

  it('a slow signature (19 s in KasWare): at least its time plus RETIME_LEAD_DAA', () => {
    expect(retimeLead(30n, 19_000)).toBe(190n + RETIME_LEAD_DAA);
    // the next step grows from the lead used, so the chain never shortens
    const used = retimeLead(30n, 19_000);
    expect(retimeLead(used + RETIME_LEAD_DAA, 5_000)).toBe(250n);
    // at the node's measured rate
    expect(retimeLead(30n, 19_000, 9_500)).toBe(181n + RETIME_LEAD_DAA);
  });

  it('caps the measured part at MAX_MEASURED_LEAD_DAA, never below the step', () => {
    expect(MAX_MEASURED_LEAD_DAA).toBe(600n);
    expect(retimeLead(30n, 120_000)).toBe(MAX_MEASURED_LEAD_DAA);
    expect(retimeLead(660n, 120_000)).toBe(660n);
  });
});

describe('firstAttemptLead: a slow wallet\'s first attempt', () => {
  it('nothing measured, or a signature within the window: no shift', () => {
    expect(firstAttemptLead(null)).toBe(0n);
    expect(firstAttemptLead(800)).toBe(0n); // a private-key signer approving at once
    expect(firstAttemptLead(5_000)).toBe(0n); // exactly the window
  });

  it('a signature longer than the window: the shortfall plus RETIME_LEAD_DAA', () => {
    expect(firstAttemptLead(5_100)).toBe(1n + RETIME_LEAD_DAA);
    expect(firstAttemptLead(19_000)).toBe(190n - AUCTION_WINDOW_DAA + RETIME_LEAD_DAA);
    expect(firstAttemptLead(19_000, 9_500)).toBe(181n - AUCTION_WINDOW_DAA + RETIME_LEAD_DAA);
  });

  it('is capped at MAX_MEASURED_LEAD_DAA', () => {
    expect(firstAttemptLead(64_000)).toBe(MAX_MEASURED_LEAD_DAA);
    expect(firstAttemptLead(3_000_000)).toBe(MAX_MEASURED_LEAD_DAA);
  });
});

describe('the remembered signing time of each wallet', () => {
  it('is kept per wallet for the session without storage (none in this environment)', () => {
    expect(rememberedSigningMs('kasware')).toBeNull();
    rememberSigningMs('kasware', 19_000.4);
    rememberSigningMs('local-key', 700);
    expect(rememberedSigningMs('kasware')).toBe(19_000);
    expect(rememberedSigningMs('local-key')).toBe(700);
    expect(rememberedSigningMs('kaspire')).toBeNull();
    // the last measurement counts
    rememberSigningMs('kasware', 3_000);
    expect(rememberedSigningMs('kasware')).toBe(3_000);
    // no reading that is no signing time
    rememberSigningMs('kasware', NaN);
    rememberSigningMs('kasware', -1);
    rememberSigningMs('kasware', 7_200_000);
    expect(rememberedSigningMs('kasware')).toBe(3_000);
  });

  it('survives a reload through localStorage when it is there, and ignores what it cannot read', () => {
    const data = new Map<string, string>();
    vi.stubGlobal('localStorage', { getItem: (k: string) => data.get(k) ?? null, setItem: (k: string, v: string) => void data.set(k, v) });
    rememberSigningMs('kastle', 12_000);
    forgetSigningTimes();
    expect(rememberedSigningMs('kastle')).toBe(12_000);
    data.set('kob.signing-ms.v1:kaspire', 'garbage');
    expect(rememberedSigningMs('kaspire')).toBeNull();
  });

  it('a storage that throws (private mode, blocked site data): the page memory still works', () => {
    const boom = () => {
      throw new Error('denied');
    };
    vi.stubGlobal('localStorage', { getItem: boom, setItem: boom });
    expect(rememberedSigningMs('kasware')).toBeNull();
    rememberSigningMs('kasware', 19_000);
    expect(rememberedSigningMs('kasware')).toBe(19_000);
  });
});

describe('SigningTimer', () => {
  it('counts from the hand-over of the transaction signed (a later update restarts it)', () => {
    let now = 1_000;
    const timer = new SigningTimer(() => now);
    expect(timer.since('a')).toBeNull();
    timer.handed('a');
    now = 4_000;
    timer.handed('b');
    now = 9_000;
    expect(timer.since('a')).toBe(8_000);
    expect(timer.since('b')).toBe(5_000);
  });
});

describe('a shifted first attempt keeps the approved terms', () => {
  const K = kob();
  const decode = (p: OrderPlan) => decodeSigning({ kob: K, built: p.built!, maker: MAKER_PK, registry: null, nodeInputs: nodeFactsOf(p.built!) });
  const kinds: [string, ReturnType<typeof form>][] = [
    ['market buy', form('market', 'buy', { amount: '2' })],
    ['market sell', form('market', 'sell', { amount: '2' })],
    ['dutch sell', form('dutch', 'sell', { amount: '5', price: '2.8', priceEnd: '2.6', duration: '10' })],
  ];
  for (const [name, f] of kinds) {
    it(`${name}: a 19 s wallet starts the auction and its send-by later by the same lead, nothing else changes`, () => {
      const env = ticketEnv();
      const intent = buildIntent(f, CTX).intent!;
      const p = planOrder(env, intent);
      const by = auctionSubmitBy(p.states, env.clock.daa);
      expect(by).not.toBeNull();
      const extra = firstAttemptLead(19_000, env.clock.rateMilli);
      const later = laterClock(env.clock, extra);
      const q = planOrder({ ...env, clock: later }, intent);
      expect(q.ok && q.built).toBeTruthy();
      expect(decode(q).orders[0]!.description.activeFrom).toBe(decode(p).orders[0]!.description.activeFrom + extra);
      // the wallet now has the window plus the shift: its last signing time and RETIME_LEAD_DAA more
      const byLater = auctionSubmitBy(q.states, later.daa)!;
      expect(byLater).toBe(by! + extra);
      expect(byLater - env.clock.daa).toBe(daaOfMs(19_000) + RETIME_LEAD_DAA);
      // the private-key signer (and the app's popup) take it as the same order, only later
      expect(sameOrBetter(decode(p), decode(q))).toBe(true);
    });
  }
});
