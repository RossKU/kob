// Finding AUD-EXT, off-chain mitigation in the wallet: the stop-trigger evidence of the conditional / if-done covenants does not compare the
// KCC-20 extension commitment, so trades under a second commitment of the same covenant id (a look-alike only a genesis can mint) can arm or trail
// a stop. A token whose genesis the registry verified (`genesis_verified: true`) has one commitment; every other KCC-20 token (all open-list
// tokens) gets STOP_EVIDENCE_GENESIS_UNVERIFIED on any plan with a stop leg, trailing stop or stop entry (also in a committed exit).
import { describe, expect, it } from 'vitest';
import { planOrder, stopEvidenceIssues } from './plan';
import type { Intent } from './plan';
import { CLOCK, defaultBook, makeEnv, market3x3, marketKron, TOK } from '../testing/fixtures';
import { makePairEnv, tokenA, tokenB, B_COV } from './orders/pair-fixtures';
import { market8x8 } from '../testing/fixtures';

const CODE = 'STOP_EVIDENCE_GENESIS_UNVERIFIED';
const flagged = (p: { issues: { code: string; severity: string }[] }) => p.issues.filter((i) => i.code === CODE);

describe('AUD-EXT: stops on a KCC-20 token without a verified single-commitment genesis are flagged', () => {
  const unverified = () => makeEnv({ market: market3x3() });
  const verified = () => makeEnv({ market: market3x3({ genesis_verified: true }) });
  const stops: [string, Intent][] = [
    ['stop market', { type: 'stopMarket', side: 'sell', amount: 10n * TOK, stop: 230_000_000n } as Intent],
    ['trailing stop', { type: 'trailingStop', side: 'sell', amount: 10n * TOK, stop: 230_000_000n, trail: { step: 1_000_000n, gap: 20_000_000n } } as Intent],
    ['oco', { type: 'oco', side: 'sell', amount: 10n * TOK, takeProfit: 300_000_000n, stop: 230_000_000n } as Intent],
    ['stop entry', { type: 'ifd', side: 'buy', amount: 10n * TOK, entry: { price: 260_000_000n, stop: 255_000_000n }, exit: { takeProfit: 300_000_000n } } as Intent],
    ['IFD whose exit is a stop', { type: 'ifd', side: 'buy', amount: 10n * TOK, entry: { price: 240_000_000n }, exit: { stop: 200_000_000n } } as Intent],
  ];

  for (const [name, intent] of stops) {
    it(`${name}: a warning on an unverified KCC-20 token, nothing on a verified one`, () => {
      const p = planOrder(unverified(), intent);
      expect(p.issues.filter((i) => i.severity === 'error'), name).toEqual([]);
      expect(flagged(p), name).toHaveLength(1);
      expect(flagged(p)[0]!.severity).toBe('warning');
      expect(flagged(planOrder(verified(), intent)), name).toEqual([]);
    });
  }

  it('orders without a stop (limit, take-profit, IFD with a take-profit exit) are not flagged', () => {
    const env = unverified();
    for (const intent of [
      { type: 'limit', side: 'sell', amount: 10n * TOK, price: 300_000_000n },
      { type: 'takeProfit', side: 'sell', amount: 10n * TOK, price: 300_000_000n },
      { type: 'ifd', side: 'buy', amount: 10n * TOK, entry: { price: 240_000_000n }, exit: { takeProfit: 300_000_000n } },
    ] as Intent[]) {
      expect(flagged(planOrder(env, intent)), intent.type).toEqual([]);
    }
  });

  it('KRON tokens carry no extension commitment: never flagged', () => {
    const env = makeEnv({ market: marketKron() });
    const p = planOrder(env, { type: 'stopMarket', side: 'sell', amount: 10n, stop: 230_000_000n } as Intent);
    expect(flagged(p)).toEqual([]);
  });

  it('an open-list token (registry json without genesis_verified, as open-token.ts synthesises it) is unverified', () => {
    expect(market3x3().genesisVerified).toBe(false);
    expect(market3x3({ genesis_verified: true }).genesisVerified).toBe(true);
  });

  it('pair stops: each unverified KCC-20 token of the pair is named', () => {
    const env = makePairEnv({ a: tokenA(), b: tokenB() });
    const p = planOrder(env, { type: 'ifd', side: 'buy', amount: 8_003n, entry: { price: 1_600n, stop: 1_550n }, exit: { stop: 1_450n } } as Intent);
    expect(p.issues.filter((i) => i.severity === 'error')).toEqual([]);
    expect(flagged(p).map((i) => (i as { params?: { token?: string } }).params?.token)).toEqual([expect.stringContaining('AAA'), expect.stringContaining('BBB')]);
    const onlyB = makePairEnv({ a: market3x3({ ticker: 'AAA', genesis_verified: true }), b: market8x8({ covenant_id: B_COV, ticker: 'BBB', decimals: 2 }) });
    const q = planOrder(onlyB, { type: 'ifd', side: 'buy', amount: 8_003n, entry: { price: 1_600n, stop: 1_550n }, exit: { stop: 1_450n } } as Intent);
    expect(flagged(q).map((i) => (i as { params?: { token?: string } }).params?.token)).toEqual([expect.stringContaining('BBB')]);
  });

  it('stopEvidenceIssues reads the planned states only (a plan without states is never flagged)', () => {
    const env = { ...makeEnv(), clock: CLOCK, book: defaultBook() };
    expect(stopEvidenceIssues(env, { states: [] })).toEqual([]);
  });
});
