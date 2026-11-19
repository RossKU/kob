// Cancel-all liveness: planCancelAll returns one plan per transaction, and TxFlow / OrdersView rely
// on the plans spending disjoint UTXOs, so that a partial sequence leaves everything consistent
// (TxFlow.tsx header).
//
// The maker funding UTXO is the shared resource. Every plan calls buildWithFunding(env, ...) on the
// same `env.funding` pool, which always picks the same UTXO (the smallest single one covering the
// deficit). When two or more transactions of one cancel-all need funding (tiny bids, two tokens,
// more orders than `maxOrdersPerTx`), planCancelAll must remove the funding UTXOs used by an
// earlier plan from the pool of the later ones. Otherwise the later plans spend an input that is
// already spent and the flow stops after the first step.
//
// Fixtures are the ones of cancel.test.ts.
import { describe, expect, it } from 'vitest';
import { planCancelAll, type CancelEnv, type OrderSnapshot } from './cancel';
import { loadKobNode } from './wasm.node';
import { MAKER, keyUtxo, placeGolden } from '../testing/chain-fixtures';

const kob = loadKobNode();
const KAS = 100_000_000n;
const env = (over: Partial<CancelEnv> = {}): CancelEnv => ({
  kob, maker: MAKER.pk, funding: [keyUtxo(MAKER.pk, 100n * KAS, 201), keyUtxo(MAKER.pk, 3n * KAS, 202)], tokenUtxos: [], clock: { daa: 78_000_000n }, ...over,
});

/**
 * The golden bid with its escrow shrunk to dust (a bid that bought almost everything): its cancel
 * cannot pay its own fee.
 */
const bid = placeGolden(kob, 'create.bid').snapshots[0];
const dust = (n: number): OrderSnapshot => {
  const id = n.toString(16).padStart(2, '0').repeat(32);
  return { ...bid, covenantId: id, order: { ...bid.order, transactionId: id, covenantId: id, amount: '1000' } };
};

describe('C5-F1: the transactions of one cancel-all do not share a funding UTXO', () => {
  it('two plans that each need funding use different maker UTXOs', () => {
    const plans = planCancelAll(env(), [dust(0xa1), dust(0xa2)], { maxOrdersPerTx: 1 });
    expect(plans).toHaveLength(2);
    for (const p of plans) expect(p.ok, p.issues.map((i) => i.code).join(',')).toBe(true);
    const used = plans.flatMap((p) => p.fundingUsed.map((f) => `${f.transactionId}:${f.index}`));
    expect(used.length).toBeGreaterThan(1);
    expect(new Set(used).size).toBe(used.length);
  });
});
