// C5 liveness: the placement record is the wallet's ONLY way to find (and cancel) an order without the indexer (records.ts header,
// RecoverPanel "the fallback that keeps cancel working when the indexer disappears"). resolveRecord enumerates continuations of the
// ORIGINAL state over amountLeft / armed ∈ {recorded, 0, 1} / rptAmount (2,000 candidates). States a live order reaches routinely were
// outside that set, so the record path answered `unknown` and the order could not be cancelled:
//   * an armed stop (bandDaa > 0) that fills partly: the covenant writes the arming UTXO's DAA into `armed`
//     (KobCondAsk.sil `newArmed = OpTxInputDaaScore(self)`, same in KobCondBid / KobIfdAsk);
//   * a repeat entry with "unlimited" repeats (rptAmount = 1 + 10^7·N) after ONE partial fill (amountLeft and rptAmount both move);
//   * a trailing stop after a trail (stopPrice is not enumerated, documented).
// Fixed (C5): records keep the LAST proven state (refreshed from indexer views / node resolutions, original kept for identity) and the
// enumeration walks amountLeft / rptAmount jointly (rpt = rptOrig - filled, amount = amountOrig - filled + merged), in steps of the minimum
// fill once the amount exceeds the candidate budget (a fill of the minimum fill is the case tested here).
import { describe, expect, it } from 'vitest';
import { loadKobNode } from './wasm.node';
import { recordsFromBuilt, refreshFromResolved, resolveRecord } from './records';
import type { OrderState } from './types';
import { FakeChain, MAKER, TOKEN, placeGolden, testAddress, tokenState } from '../testing/chain-fixtures';

const kob = loadKobNode();
const opts = { maker: MAKER.pk, network: 'testnet-10', placedAtUnix: 1_790_000_000n, placedAtDaa: 1000n };
const recordOf = (name: string) => {
  const p = placeGolden(kob, name);
  return { p, rec: recordsFromBuilt(kob, p.built, opts)[0] };
};
const st = (kind: OrderState['kind'], hex: string) => kob.decodeState(kind, hex) as OrderState;
const edit = (o: OrderState, fields: Record<string, string>): OrderState => ({ ...o, state: { ...(o.state as unknown as Record<string, string>), ...fields } } as unknown as OrderState);

describe('C5-R1: the record path finds the states a live order routinely reaches', () => {
  // The arming UTXO's DAA cannot be enumerated (any DAA): the wallet can only know it by having SEEN that UTXO. Realistic case (and the one
  // the record keeps for): the wallet resolved the order while it rested at DAA 1234567 (refreshFromResolved stores that UTXO's DAA), then a
  // band stop was armed inside a fill spending that UTXO (armed = 1234567) and filled partly (one minimum fill, new UTXO at 1234600).
  // (As first written the record had never seen any UTXO of the order, so armed = 1234567 was unknowable without a by-covenant-id lookup.)
  it('an armed stop-loss that filled partly (armed = the arming UTXO DAA) resolves live', async () => {
    const { p, rec: placed } = recordOf('create.condAsk');
    const before = new FakeChain();
    before.applyTx(p.signed.tx, '1234567');
    const seen = await resolveRecord(kob, before, placed, testAddress);
    expect(seen.status).toBe('live');
    const rec = refreshFromResolved(kob, placed, seen);
    const base = st(rec.kind as OrderState['kind'], rec.state);
    const s = base.state as unknown as Record<string, string>;
    expect(BigInt(s.amountLeft)).toBeGreaterThan(BigInt(s.minFill));
    const amount = BigInt(s.amountLeft) - BigInt(s.minFill);
    const cont = edit(base, { armed: '1234567', amountLeft: amount.toString() });
    const chain = new FakeChain();
    chain.add({ transactionId: 'cd'.repeat(32), index: 0, amount: '1000000000', scriptPublicKey: kob.scriptPublicKey(cont), covenantId: rec.covenantId, blockDaaScore: '1234600' });
    chain.add({ transactionId: 'cd'.repeat(32), index: 1, amount: '1000000000', scriptPublicKey: kob.tokenScriptPublicKey(TOKEN.program, tokenState(amount, rec.covenantId, 4)), covenantId: TOKEN.covenantId });
    const r = await resolveRecord(kob, chain, rec, testAddress);
    expect(r.status).toBe('live');
  });

  it('an "unlimited" repeat entry after one partial fill resolves live', async () => {
    const { rec } = recordOf('create.ifdBid.repeat');
    const base = st(rec.kind as OrderState['kind'], rec.state);
    const s = base.state as unknown as Record<string, string>;
    const n = BigInt(s.amountLeft);
    const f = BigInt(s.minFill);
    // the wallet's "unlimited" repeat: rptAmount = 1 + 10,000,000 · N (cond-common.ts UNLIMITED_REPEAT_COUNT)
    const unlimited = edit(base, { rptAmount: (1n + 10_000_000n * n).toString() });
    const rec2 = { ...rec, state: kob.encodeState(unlimited) };
    // one fill of the minimum fill: both move by it
    const cont = edit(unlimited, { amountLeft: (n - f).toString(), rptAmount: (1n + 10_000_000n * n - f).toString() });
    const chain = new FakeChain();
    chain.add({ transactionId: 'ce'.repeat(32), index: 0, amount: '5000000000', scriptPublicKey: kob.scriptPublicKey(cont), covenantId: rec.covenantId, blockDaaScore: '2000' });
    const r = await resolveRecord(kob, chain, rec2, testAddress);
    expect(r.status).toBe('live');
  });
});
