// A sell ladder placed as one curve order (`CurveIntent`): one KobCurveAsk with the ladder's prices
// as its table, validated in the script engine and decoded with nothing blocking; the bad shapes
// are refused.
import { describe, expect, it } from 'vitest';
import { CLOCK, MAKER_PK, consensusCheck, kob, makeEnv, market3x3 } from '../../testing/fixtures';
import { planOrder } from '../plan';
import { errors } from '../plan-types';
import { decodeSigning } from '../decode';
import { expectedFromPlan } from '../../ui/ticket/plan-expected';
import { nodeFactsOf } from '../../testing/chain-fixtures';
import type { CurveIntent } from '../intent-simple';

const K = kob();
const env = () => makeEnv({ market: market3x3(), tokenAmounts: [100_000n], clock: CLOCK });
const curve = (over: Partial<CurveIntent> = {}): CurveIntent => ({ type: 'curve', side: 'sell', amount: 40_000n, levelSize: 10_000n, table: [100_000n, 110_000n, 121_000n, 133_100n], ...over });

describe('a sell ladder as one curve order', () => {
  it('plans one KobCurveAsk holding the whole ladder; the transaction validates and decodes clean', () => {
    const p = planOrder(env(), curve());
    expect(errors(p)).toEqual([]);
    expect(p.ok).toBe(true);
    expect(p.states).toHaveLength(1);
    const st = p.states[0]!;
    expect(st.kind).toBe('KobCurveAsk');
    expect(st.state).toMatchObject({ maker: MAKER_PK, levelSize: '10000', total: '40000', amountLeft: '40000', table: ['100000', '110000', '121000', '133100'] });
    // what a sell-out pays: each level at its price, per whole token of the market scale
    const scale = env().token.scale;
    expect(p.disclosure!.allInTotal).toBe([100_000n, 110_000n, 121_000n, 133_100n].reduce((a, x) => a + (10_000n * x + scale - 1n) / scale, 0n));
    expect(p.disclosure!.tokensEscrowed).toBe(40_000n);
    consensusCheck(K, p.built!);
    const s = decodeSigning({ kob: K, built: p.built!, maker: MAKER_PK, registry: null, expected: expectedFromPlan(p), nodeInputs: nodeFactsOf(p.built!) });
    expect(s.blocking).toEqual([]);
  });

  it('refuses a table that does not fit the amount, and more than the wallet holds', () => {
    expect(planOrder(env(), curve({ amount: 50_000n })).ok).toBe(false);
    expect(planOrder(env(), curve({ amount: 30_000n })).ok).toBe(false);
    expect(errors(planOrder(env(), curve({ amount: 400_000n, levelSize: 100_000n }))).map((i) => i.code)).toContain('INSUFFICIENT_TOKENS');
  });
});
