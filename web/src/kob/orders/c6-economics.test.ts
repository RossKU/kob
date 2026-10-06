// C6 differential vectors, web TS <-> Rust: the wallet arithmetic this app takes from kob-protocol `state.rs` (the quote rule, a bid's
// budget, escrow and buying power, stop-band worst prices, the largest band within a limit, the worst price over both legs) on random
// inputs with boundaries and near-overflow values. The expected values are Rust's (`c6-economics.vectors.json`, generated and kept current
// by `crates/kob-tests/tests/c6_diff.rs`, `KOB_REGEN=1`); every TS result, and every kob-wasm helper the planners call, must equal them
// exactly.
import { describe, expect, it } from 'vitest';
import { readFileSync } from 'node:fs';
import { fileURLToPath } from 'node:url';
import { bidBudgetRate, makeBid } from './common';
import { askWorst, bidWorst, slipForLimit, stopWorstPrice, type Legs } from './cond-legs';
import { quoteOf } from '../units';
import { makeEnv } from '../../testing/fixtures';
import type { OrderState } from '../types';

type Row = Record<string, string | null>;
const vectors = JSON.parse(readFileSync(fileURLToPath(new URL('./c6-economics.vectors.json', import.meta.url)), 'utf8')) as Record<string, Row[]>;
const b = (s: string | null | undefined): bigint => BigInt(s as string);
const opt = (s: string | null | undefined): bigint | null => (s === null || s === undefined ? null : BigInt(s));

const env = makeEnv();
const K = env.kob;
/** A KobBid state carrying the vector's terms (every other field a valid default). */
const bidOf = (v: Row): OrderState => {
  const base = makeBid(env, { minFill: 1n, price: 1n, expiryDaa: 99n });
  return {
    ...base,
    state: {
      ...base.state, scale: v.scale!, price: v.price!, tip: v.tip!, slope: v.slope!, priceEnd: v.priceEnd!,
      ...(v.deliveryCarrier != null ? { deliveryCarrier: v.deliveryCarrier } : {}), ...(v.reserve != null ? { reserve: v.reserve } : {}),
    },
  } as OrderState;
};

const legsOf = (tp: bigint, stop: bigint, stopWorst: bigint): Legs =>
  ({ tpPrice: tp, stopPrice: stop, stopWorst, slipBps: 0n, bandDaa: 0n, keeperTip: 0n, minTouch: 0n, minRestDaa: 0n, trailStep: 0n, trailGap: 0n, trailWait: 0n }) as Legs;

describe('C6 wallet arithmetic equals kob-protocol state.rs (Rust vectors)', () => {
  it('has vectors for every mirrored rule', () => {
    for (const k of ['quoteOf', 'bidUsed', 'bidEscrow', 'bidBuyingPower', 'stopWorstPrice', 'slipForLimit', 'legsWorst']) expect(vectors[k]?.length ?? 0, k).toBeGreaterThan(100);
  });

  it('quoteOf = state::quote_of (rounded up for what a maker receives, down for what it pays; null where the covenant fails)', () => {
    let rounded = 0;
    for (const v of vectors.quoteOf) {
      const round = v.round as 'up' | 'down';
      const want = opt(v.value);
      expect(K.quote(b(v.n), b(v.rate), b(v.scale), round), JSON.stringify(v)).toBe(want);
      if (want !== null) {
        expect(quoteOf(b(v.n), b(v.rate), b(v.scale), round), JSON.stringify(v)).toBe(want);
        if (round === 'up' && (b(v.n) * b(v.rate)) % b(v.scale) !== 0n) rounded++;
      }
    }
    expect(rounded).toBeGreaterThan(50);
  });

  it('bidUsed = BidState::used (ceil at the budget rate pMax + tip; a rising bid budgets its priceEnd)', () => {
    for (const v of vectors.bidUsed) {
      const want = opt(v.used);
      expect(K.bidUsed(bidOf(v), b(v.amount)), JSON.stringify(v)).toBe(want);
      if (want !== null) expect(quoteOf(b(v.amount), bidBudgetRate(b(v.price), b(v.tip), b(v.slope), b(v.priceEnd)), b(v.scale), 'up')).toBe(want);
    }
  });

  it('bidEscrow = BidState::escrow', () => {
    for (const v of vectors.bidEscrow) {
      expect(K.bidEscrow(bidOf(v), b(v.amount), b(v.fills)), JSON.stringify(v)).toBe(opt(v.escrow));
    }
  });

  it('bidBuyingPower = BidState::buying_power', () => {
    for (const v of vectors.bidBuyingPower) {
      expect(K.bidBuyingPower(bidOf(v), b(v.value)), JSON.stringify(v)).toBe(b(v.buyingPower));
    }
  });

  it('stopWorstPrice = CondAskState::stop_floor / CondBidState::stop_ceiling (multiply first, rounded for the maker)', () => {
    for (const v of vectors.stopWorstPrice) {
      expect(stopWorstPrice(v.side as 'sell' | 'buy', b(v.stop), b(v.slipBps)), JSON.stringify(v)).toBe(b(v.worst));
    }
  });

  it('slipForLimit = the largest bps whose covenant band stays within the limit', () => {
    for (const v of vectors.slipForLimit) {
      expect(slipForLimit(v.side as 'sell' | 'buy', b(v.stop), b(v.limit)), JSON.stringify(v)).toBe(b(v.slip));
    }
  });

  it('askWorst / bidWorst = the worst leg (KobCondAsk minimum, CondBidState::worst)', () => {
    for (const v of vectors.legsWorst) {
      expect(askWorst(legsOf(b(v.tpPrice), b(v.stopPrice), b(v.askStopWorst))), JSON.stringify(v)).toBe(b(v.askWorst));
      expect(bidWorst(legsOf(b(v.tpPrice), b(v.stopPrice), b(v.bidStopWorst))), JSON.stringify(v)).toBe(b(v.bidWorst));
    }
  });
});
