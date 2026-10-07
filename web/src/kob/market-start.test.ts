// The start of a market / close auction is the best price of the indexer book; planning compares it with the references that do not come from
// that book (the last fill, other indexers' best prices) and holds a start far on the costly side until the user acknowledges it.
import { describe, expect, it } from 'vitest';
import { MARKET_START_TOLERANCE_BPS, marketStartAckOf, planOrder } from './plan';
import { errors } from './plan-types';
import { TOK, level, makeEnv } from '../testing/fixtures';
import type { BidState } from './types';
import { resolveConfig } from '../config';

const REAL = 250_000_000n; // 2.5 KAS per token
const codes = (p: { issues: { code: string }[] }) => p.issues.map((i) => i.code);

function envWithAsk(ask: bigint, bid = REAL) {
  return makeEnv({ book: { asks: [level(ask, 1_000n * TOK, 1)], bids: [level(bid, 1_000n * TOK, 1)] } });
}

describe('market order start against independent references', () => {
  it('a best ask 1.9x the last fill holds a market buy until acknowledged', () => {
    const fake = (REAL * 19n) / 10n;
    const env = envWithAsk(fake);
    env.lastFillPrice = REAL;
    const p = planOrder(env, { type: 'market', side: 'buy', amount: 2n * TOK } as never);
    expect(codes(p)).toContain('MARKET_START_VS_LAST_FILL');
    expect(errors(p).map((i) => i.code)).toEqual(['MARKET_START_VS_LAST_FILL']);
    expect(p.ok).toBe(false);
    const i = p.issues.find((x) => x.code === 'MARKET_START_VS_LAST_FILL')!;
    expect(i.params).toMatchObject({ start: fake, reference: REAL, percent: '90', direction: 'above' });

    const acked = planOrder({ ...env, marketStartAck: marketStartAckOf(i) }, { type: 'market', side: 'buy', amount: 2n * TOK } as never);
    expect(acked.ok).toBe(true);
    expect(errors(acked)).toEqual([]);
    expect(codes(acked)).toContain('MARKET_START_ACKNOWLEDGED');
    expect(BigInt((acked.states[0]!.state as BidState).price)).toBe(fake);
  });

  it('a start within the tolerance, or on the cheap side, passes', () => {
    const near = REAL + (REAL * (MARKET_START_TOLERANCE_BPS - 10n)) / 10_000n;
    const env = envWithAsk(near);
    env.lastFillPrice = REAL;
    const p = planOrder(env, { type: 'market', side: 'buy', amount: 2n * TOK } as never);
    expect(p.ok).toBe(true);
    // the last fill is from the same indexer: a small order is told so (a warning), nothing more
    expect(codes(p).filter((c) => c.startsWith('MARKET_START'))).toEqual(['MARKET_START_NO_INDEPENDENT_REFERENCE']);
    // a buy that starts BELOW the last fill costs the user nothing more
    const cheap = envWithAsk(REAL / 2n, REAL / 3n);
    cheap.lastFillPrice = REAL;
    expect(codes(planOrder(cheap, { type: 'market', side: 'buy', amount: 2n * TOK } as never)).filter((c) => c.startsWith('MARKET_START'))).toEqual([
      'MARKET_START_NO_INDEPENDENT_REFERENCE',
    ]);
    // with another indexer agreeing nothing is said
    const checked = planOrder({ ...env, referenceTouches: [{ label: 'https://idx2.example', bestAsk: REAL, bestBid: REAL - 1n }] }, { type: 'market', side: 'buy', amount: 2n * TOK } as never);
    expect(codes(checked).filter((c) => c.startsWith('MARKET_START'))).toEqual([]);
  });

  it('the tolerance is configurable', () => {
    const env = envWithAsk((REAL * 105n) / 100n);
    env.lastFillPrice = REAL;
    expect(planOrder(env, { type: 'market', side: 'buy', amount: 2n * TOK } as never).ok).toBe(true);
    const tight = planOrder({ ...env, marketStartToleranceBps: 200n }, { type: 'market', side: 'buy', amount: 2n * TOK } as never);
    expect(codes(tight)).toContain('MARKET_START_VS_LAST_FILL');
  });

  it('another indexer best price is a reference too, named in the finding', () => {
    const env = envWithAsk((REAL * 15n) / 10n);
    env.lastFillPrice = null;
    env.referenceTouches = [{ label: 'https://idx2.example', bestAsk: REAL, bestBid: REAL - 1n }];
    const p = planOrder(env, { type: 'market', side: 'buy', amount: 2n * TOK } as never);
    expect(p.ok).toBe(false);
    const i = p.issues.find((x) => x.code === 'MARKET_START_VS_INDEXER')!;
    expect(i.params).toMatchObject({ other: 'https://idx2.example', reference: REAL, percent: '50' });
  });

  it('a sell / close below the reference is held the same way', () => {
    const env = makeEnv({ book: { asks: [level(REAL + 1_000n, 1_000n * TOK, 1)], bids: [level(REAL / 2n, 1_000n * TOK, 1)] } });
    env.lastFillPrice = REAL;
    const sell = planOrder(env, { type: 'market', side: 'sell', amount: 2n * TOK } as never);
    expect(codes(sell)).toContain('MARKET_START_VS_LAST_FILL');
    expect(sell.issues.find((x) => x.code === 'MARKET_START_VS_LAST_FILL')!.params).toMatchObject({ direction: 'below', percent: '50' });
    const close = planOrder(env, { type: 'close' } as never);
    expect(codes(close)).toContain('MARKET_START_VS_LAST_FILL');
  });

  it('without any reference the plan says the start is unchecked (a warning)', () => {
    const env = envWithAsk(REAL);
    env.lastFillPrice = null;
    const p = planOrder(env, { type: 'market', side: 'buy', amount: 2n * TOK } as never);
    expect(p.ok).toBe(true);
    expect(p.issues.find((x) => x.code === 'MARKET_START_UNCHECKED')?.severity).toBe('warning');
  });

  it('a limit order is not affected', () => {
    const env = envWithAsk(REAL * 2n);
    env.lastFillPrice = REAL;
    const p = planOrder(env, { type: 'limit', side: 'sell', amount: 2n * TOK, price: REAL * 3n } as never);
    expect(codes(p).filter((c) => c.startsWith('MARKET_START'))).toEqual([]);
  });
});

describe('config marketStartToleranceBps', () => {
  it('defaults to 1000 bps and is read from the deployment layers only', () => {
    expect(resolveConfig({}).config.marketStartToleranceBps).toBe(1_000);
    expect(resolveConfig({ file: { marketStartToleranceBps: 300 } }).config.marketStartToleranceBps).toBe(300);
    const stored = resolveConfig({ settings: { marketStartToleranceBps: 5_000 } });
    expect(stored.config.marketStartToleranceBps).toBe(1_000);
    expect(stored.warnings.map((w) => w.field)).toContain('marketStartToleranceBps');
    const bad = resolveConfig({ file: { marketStartToleranceBps: 9_999 } });
    expect(bad.config.marketStartToleranceBps).toBe(1_000);
    expect(bad.warnings.map((w) => w.field)).toContain('marketStartToleranceBps');
  });
});
