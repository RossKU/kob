// The start of a market / close auction is the best price of the indexer book; planning compares it with the best price of every further indexer
// (an independent reference) and refuses a start far on the costly side of one. The last fill comes from the same indexer as the book: it is no
// reference, and without a further indexer the start is accepted without a word.
import { describe, expect, it } from 'vitest';
import { MARKET_START_TOLERANCE_BPS, planOrder } from './plan';
import { errors } from './plan-types';
import { TOK, level, makeEnv } from '../testing/fixtures';
import { resolveConfig } from '../config';

const REAL = 250_000_000n; // 2.5 KAS per token
const codes = (p: { issues: { code: string }[] }) => p.issues.map((i) => i.code);
const startCodes = (p: { issues: { code: string }[] }) => codes(p).filter((c) => c.startsWith('MARKET_START'));
const buy = { type: 'market', side: 'buy', amount: 2n * TOK } as never;
const other = (bestAsk: bigint | null, bestBid: bigint | null) => [{ label: 'https://idx2.example', bestAsk, bestBid }];

function envWithAsk(ask: bigint, bid = REAL) {
  return makeEnv({ book: { asks: [level(ask, 1_000n * TOK, 1)], bids: [level(bid, 1_000n * TOK, 1)] } });
}

describe('market order start against independent references', () => {
  it('another indexer whose best ask is far below the start refuses a market buy (an error, nothing to acknowledge)', () => {
    const env = envWithAsk((REAL * 15n) / 10n);
    env.referenceTouches = other(REAL, REAL - 1n);
    const p = planOrder(env, buy);
    expect(p.ok).toBe(false);
    expect(errors(p).map((i) => i.code)).toEqual(['MARKET_START_VS_INDEXER']);
    expect(p.issues.find((x) => x.code === 'MARKET_START_VS_INDEXER')!.params).toMatchObject({ other: 'https://idx2.example', reference: REAL, percent: '50', direction: 'above' });
  });

  it('the last fill is no reference: a start 1.9x the last fill is accepted without a word', () => {
    const env = envWithAsk((REAL * 19n) / 10n);
    env.lastFillPrice = REAL;
    const p = planOrder(env, buy);
    expect(p.ok).toBe(true);
    expect(startCodes(p)).toEqual([]);
  });

  it('a start within the tolerance, or on the cheap side, passes', () => {
    const near = REAL + (REAL * (MARKET_START_TOLERANCE_BPS - 10n)) / 10_000n;
    const env = envWithAsk(near);
    env.referenceTouches = other(REAL, REAL - 1n);
    expect(planOrder(env, buy).ok).toBe(true);
    expect(startCodes(planOrder(env, buy))).toEqual([]);
    // a buy that starts BELOW the reference costs the user nothing more
    const cheap = envWithAsk(REAL / 2n, REAL / 3n);
    cheap.referenceTouches = other(REAL, REAL - 1n);
    expect(startCodes(planOrder(cheap, buy))).toEqual([]);
  });

  it('the tolerance is configurable', () => {
    const env = envWithAsk((REAL * 105n) / 100n);
    env.referenceTouches = other(REAL, REAL - 1n);
    expect(planOrder(env, buy).ok).toBe(true);
    expect(codes(planOrder({ ...env, marketStartToleranceBps: 200n }, buy))).toContain('MARKET_START_VS_INDEXER');
  });

  it('a sell / close below the reference is refused the same way', () => {
    const env = makeEnv({ book: { asks: [level(REAL + 1_000n, 1_000n * TOK, 1)], bids: [level(REAL / 2n, 1_000n * TOK, 1)] } });
    env.referenceTouches = other(REAL + 1_000n, REAL);
    const sell = planOrder(env, { type: 'market', side: 'sell', amount: 2n * TOK } as never);
    expect(errors(sell).map((i) => i.code)).toEqual(['MARKET_START_VS_INDEXER']);
    expect(sell.issues.find((x) => x.code === 'MARKET_START_VS_INDEXER')!.params).toMatchObject({ direction: 'below', percent: '50' });
    expect(codes(planOrder(env, { type: 'close' } as never))).toContain('MARKET_START_VS_INDEXER');
  });

  it('without any reference the start is accepted, with no note', () => {
    const env = envWithAsk(REAL);
    env.lastFillPrice = null;
    const p = planOrder(env, buy);
    expect(p.ok).toBe(true);
    expect(startCodes(p)).toEqual([]);
    // a sell too: its start (the best bid) is accepted as the book says
    expect(planOrder(env, { type: 'market', side: 'sell', amount: 2n * TOK } as never).ok).toBe(true);
  });

  it('a further indexer that does not answer is said in a warning only, and the start is accepted', () => {
    const env = envWithAsk((REAL * 19n) / 10n);
    env.referenceTouches = [];
    env.referencesUnavailable = ['https://idx2.example'];
    const p = planOrder(env, buy);
    expect(p.ok).toBe(true);
    expect(p.issues.find((i) => i.code === 'MARKET_REFERENCE_INDEXER_UNAVAILABLE')).toMatchObject({ severity: 'warning', params: { others: 'https://idx2.example' } });
  });

  it('a limit order is not affected', () => {
    const env = envWithAsk(REAL * 2n);
    env.referenceTouches = other(REAL, REAL - 1n);
    const p = planOrder(env, { type: 'limit', side: 'sell', amount: 2n * TOK, price: REAL * 3n } as never);
    expect(startCodes(p)).toEqual([]);
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
