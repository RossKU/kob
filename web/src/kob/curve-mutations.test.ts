// A curve whose terms the wallet refuses, built for real (the protocol accepts each of them): the pre-sign decoder blocks a new curve order, the
// creation's confirmation and the signer frame refuse a genesis curve; the confirmation shows a curve as a curve, not as a limit.
import { describe, expect, it } from 'vitest';
import { CLOCK, MAKER_PK, kob as kobOf, makeEnv, market3x3 } from '../testing/fixtures';
import { nodeFactsOf } from '../testing/chain-fixtures';
import { pubkeyOf } from '../testing/local-signer';
import { buildCreate, carrierOf } from './orders/common';
import { planOrder } from './plan';
import { decodeSigning } from './decode';
import { issueRaw, type IssueResult } from './issue';
import type { CurveAskState, OrderState } from './types';
import { buildIssuanceModel } from '../ui/confirm/issuance-model';
import { creationSummary } from '../signer/summary';
import { genesisClaimOf } from '../wallet/create';
import { orderRows, orderTitle } from '../ui/confirm/confirm-model';
import { describeOrder } from './order-facts';
import { t } from '../i18n';

const kob = kobOf();

describe('a new curve order with refused terms is blocked by the pre-sign decoder', () => {
  const env = () => makeEnv({ market: market3x3(), tokenAmounts: [100_000n], clock: CLOCK });
  const good = (): CurveAskState => {
    const p = planOrder(env(), { type: 'curve', side: 'sell', amount: 40_000n, levelSize: 10_000n, table: [100_000n, 110_000n, 121_000n, 133_100n] });
    return p.states[0]!.state as CurveAskState;
  };
  const decodeWith = (s: CurveAskState) => {
    const e = env();
    const order = { kind: 'KobCurveAsk', state: s } as OrderState;
    const res = buildCreate(e, { order, value: carrierOf(e), tokenAmount: BigInt(s.amountLeft) });
    expect(res.built, res.issues.map((i) => i.code).join(',')).not.toBeNull();
    return decodeSigning({ kob, built: res.built!, maker: MAKER_PK, registry: null, nodeInputs: nodeFactsOf(res.built!) }).blocking.map((b) => b.code);
  };
  it('accepts a sound curve', () => expect(decodeWith(good())).not.toContain('curve-terms-invalid'));
  it.each([
    ['a falling level', (s: CurveAskState) => ({ ...s, table: [s.table[0]!, '50000', s.table[2]!, s.table[3]!] })],
  ])('refuses %s', (_name, mut) => expect(decodeWith(mut(good()))).toContain('curve-terms-invalid'));
  it('a tip equal to the first price is refused by the builder itself (the library gate)', () => {
    const e = env();
    const res = buildCreate(e, { order: { kind: 'KobCurveAsk', state: { ...good(), tip: good().table[0]! } } as OrderState, value: carrierOf(e), tokenAmount: 40_000n });
    expect(res.built).toBeNull();
    expect(res.issues.map((i) => i.code)).toContain('BUILD_REJECTED');
  });
  it('shows the curve as a curve: levels, first, last, average, the rest, the table', () => {
    const d = describeOrder({ kind: 'KobCurveAsk', state: good() } as OrderState, kob);
    const f = { tr: (k: string, p?: Record<string, unknown>) => t(k, p as never), ref: { covenantId: good().tokenCovId, ticker: 'TST', decimals: 3 }, clock: null, time: (u: bigint) => u.toString() };
    const rows = orderRows(d, { value: null, locked: null, deadline: null }, f as never);
    expect(rows.map((r) => r.id)).toEqual(['amount', 'curveLevels', 'curveFirst', 'curveLast', 'curveAverage', 'curveRest', 'minFill', 'curveTable']);
    expect(rows.find((r) => r.id === 'curveLevels')!.value).toMatch(/^4 of 4 left, /);
    expect(rows.some((r) => r.id === 'price')).toBe(false);
    expect(orderTitle(d, null, f as never)).toMatch(/curve order/);
  });
});

describe('a genesis curve with refused terms', () => {
  const SCALE = 100_000_000n;
  const maker = pubkeyOf('33'.repeat(32));
  const ZERO = '0'.repeat(64);
  const genesis = (over: { table?: string[]; tip?: string; minFill?: string; levelSize?: string } = {}): IssueResult =>
    issueRaw(kob, {
      name: 'Curve Token', ticker: 'CRV', decimals: 8, supply: (1000n * SCALE).toString(),
      holders: [{ owner: ZERO, ownerScheme: 4, amount: (400n * SCALE).toString() }, { owner: maker, ownerScheme: 0, amount: (600n * SCALE).toString() }],
      funding: [{ transactionId: '07'.repeat(32), index: 1, amount: (500n * SCALE).toString(), blockDaaScore: '5000', pubkey: maker }],
      network: 'testnet-10',
      curve: { custody: 0, levelSize: over.levelSize ?? (100n * SCALE).toString(), table: over.table ?? ['1000000', '2000000', '3000000', '4000000'], minFill: over.minFill ?? '1', ...(over.tip ? { tip: over.tip } : {}) },
    } as never);
  const verdicts = (g: IssueResult) => {
    const m = buildIssuanceModel({ kob, built: g.built, prep: g.prep, oneApproval: true, token: g.token, curve: g.curve!, maker, tr: (k, q) => t(k, q) });
    let frame = 'ok';
    try {
      creationSummary(kob, g.prep, g.built, maker, [], genesisClaimOf(g.token, g.curve));
    } catch (e) {
      frame = (e as Error).message;
    }
    return { app: m.blocking.map((b) => b.code), frame };
  };
  it('a sound one passes both, and the frame lists its terms', () => {
    const g = genesis();
    expect(verdicts(g)).toEqual({ app: [], frame: 'ok' });
    const s = creationSummary(kob, g.prep, g.built, maker, [], genesisClaimOf(g.token, g.curve));
    const labels = s.lines.map((l) => l.label);
    for (const l of ['Sell on a curve', 'Curve: tokens per level', 'Curve: average price if sold out', 'Curve: total if sold out', 'Curve: tip taken from every level', 'Curve: minimum fill', 'Curve: KAS per token, level by level']) expect(labels).toContain(l);
    const m = buildIssuanceModel({ kob, built: g.built, prep: g.prep, oneApproval: true, token: g.token, curve: g.curve!, maker, tr: (k, q) => t(k, q) });
    const ids = m.sections.find((x) => x.id === 'curve')!.rows.map((r) => r.id);
    for (const id of ['curve-average', 'curve-level-size', 'curve-tip', 'curve-min-fill', 'curve-table']) expect(ids).toContain(id);
  });
  it.each([
    ['a falling level', { table: ['1000000', '500000', '3000000', '4000000'] }],
  ])('refuses %s in the app and in the frame', (_n, over) => {
    expect(verdicts(genesis(over))).toEqual({ app: ['curve'], frame: 'claim' });
  });
  it('a tip equal to the first price is refused when the genesis is built (the library gate)', () => {
    expect(() => genesis({ tip: '1000000' })).toThrow(/tip/);
  });
  it('the frame refuses a claimed state that differs from the output in any field', () => {
    const g = genesis();
    for (const [k, v] of [['tip', '1'], ['minFill', '2'], ['levelSize', '1'], ['scale', '1000'], ['total', '1'], ['maker', '44'.repeat(32)], ['tokenCovId', '55'.repeat(32)], ['activeFrom', '9']] as const) {
      const c = JSON.parse(JSON.stringify(g.curve));
      c.state.state[k] = v;
      expect(() => creationSummary(kob, g.prep, g.built, maker, [], genesisClaimOf(g.token, c)), k).toThrow('claim');
    }
    const c = JSON.parse(JSON.stringify(g.curve));
    c.state.state.table[2] = '2500000';
    expect(() => creationSummary(kob, g.prep, g.built, maker, [], genesisClaimOf(g.token, c))).toThrow('claim');
  });
});
