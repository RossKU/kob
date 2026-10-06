import { describe, expect, it } from 'vitest';
import { loadKobNode } from './wasm.node';
import { escrowedBalances, escrowedBalancesDetailed, kasBalanceSummary, withFreeBalances } from './balances';
import { MAKER, OTHER, TOKEN, orderViewOf, placeGolden, strayFor, tokenUtxo } from '../testing/chain-fixtures';

const kob = loadKobNode();
const sumOutputs = (tx: { outputs: { value: string }[] }, idx: number[]) => idx.reduce((s, i) => s + BigInt(tx.outputs[i].value), 0n);

describe('escrowedBalances', () => {
  it('ask: tokens in custody, all KAS is carrier (matches the placed tx outputs)', () => {
    const p = placeGolden(kob, 'create.ask');
    const [b] = escrowedBalances(p.snapshots);
    const r = p.recovered[0];
    expect(b.token).toBe(TOKEN.covenantId);
    expect(b.custody).toBe(BigInt(r.custody!.state.amount));
    expect(b.custody).toBe(10000n);
    expect(b.strays).toBe(0n);
    expect(b.kasEscrow).toBe(0n);
    // order output + custody output carry all the KAS that left the wallet into covenants
    expect(b.kasLocked).toBe(sumOutputs(p.signed.tx, [r.output, r.custody!.output]));
    expect(b.kasLocked).toBe(b.kasCarriers);
    expect(b.orders).toBe(1);
  });

  it('bid: escrow is the value minus refund tip / reserves; total equals the output value', () => {
    const p = placeGolden(kob, 'create.bid');
    const [b] = escrowedBalances(p.snapshots);
    const st = p.recovered[0].order.state as { refundTip: string; reserve: string; deliveryCarrier: string };
    const value = BigInt(p.recovered[0].value);
    const nonBudget = BigInt(st.refundTip) + BigInt(st.reserve) + BigInt(st.deliveryCarrier);
    expect(b.custody).toBe(0n);
    expect(b.kasCarriers).toBe(nonBudget);
    expect(b.kasEscrow).toBe(value - nonBudget);
    expect(b.kasLocked).toBe(sumOutputs(p.signed.tx, [p.recovered[0].output]));
  });

  it('conditional / if-done kinds: keeper tips and carriers are not budget', () => {
    const cb = placeGolden(kob, 'create.condBid');
    const ib = placeGolden(kob, 'create.ifdBid.repeat');
    for (const p of [cb, ib]) {
      const [b] = escrowedBalances(p.snapshots);
      expect(b.kasEscrow + b.kasCarriers).toBe(b.kasLocked);
      expect(b.kasLocked).toBe(BigInt(p.recovered[0].value));
      expect(b.kasEscrow).toBeGreaterThan(0n);
      expect(b.kasCarriers).toBeGreaterThan(0n);
    }
    const st = ib.recovered[0].order.state as { refundTip: string; keeperTip: string; deliveryCarrier: string; exitCarrier: string };
    expect(escrowedBalances(ib.snapshots)[0].kasCarriers).toBe(BigInt(st.refundTip) + BigInt(st.keeperTip) + BigInt(st.deliveryCarrier) + BigInt(st.exitCarrier));
  });

  it('sell-first entry with custody and a stray', () => {
    const p = placeGolden(kob, 'create.ifdAsk');
    const snap = { ...p.snapshots[0], strays: [strayFor(p.snapshots[0], 7n)] };
    const [b] = escrowedBalances([snap]);
    expect(b.custody).toBe(BigInt(p.recovered[0].custody!.state.amount));
    expect(b.strays).toBe(7n);
    expect(b.kasLocked).toBe(BigInt(p.recovered[0].value) + BigInt(p.recovered[0].custody!.value) + 1_000_000_000n);
  });

  it('mixes OrderViews and OrderSnapshots and sums per token', () => {
    const a = placeGolden(kob, 'create.ask');
    const c = placeGolden(kob, 'create.condAsk');
    const stray = strayFor(a.snapshots[0], 5n);
    const view = orderViewOf(kob, { ...a.snapshots[0], strays: [stray] });
    const [b] = escrowedBalances([view, c.snapshots[0]]);
    expect(b.orders).toBe(2);
    expect(b.custody).toBe(BigInt(a.recovered[0].custody!.state.amount) + BigInt(c.recovered[0].custody!.state.amount));
    expect(b.strays).toBe(5n);
    expect(b.kasLocked).toBe(
      sumOutputs(a.signed.tx, [a.recovered[0].output, a.recovered[0].custody!.output]) +
        sumOutputs(c.signed.tx, [c.recovered[0].output, c.recovered[0].custody!.output]) +
        1_000_000_000n,
    );
  });

  it('separates tokens by covenant id, sorted', () => {
    const a = placeGolden(kob, 'create.ask');
    const other = { ...a.snapshots[0], order: { ...a.snapshots[0].order, state: { ...a.snapshots[0].order.state, state: { ...a.snapshots[0].order.state.state, tokenCovId: '10'.repeat(32) } } } } as typeof a.snapshots[0];
    const out = escrowedBalances([a.snapshots[0], other]);
    expect(out.map((x) => x.token)).toEqual(['10'.repeat(32), TOKEN.covenantId]);
    expect(out.every((x) => x.orders === 1)).toBe(true);
  });

  it('skips terminal, unproven and outpoint-less views and reports why', () => {
    const a = placeGolden(kob, 'create.ask');
    const s = a.snapshots[0];
    const ok = orderViewOf(kob, s);
    const filled = orderViewOf(kob, s, { covenant_id: 'aa'.repeat(32), status: 'filled' });
    const cancelled = orderViewOf(kob, s, { covenant_id: 'ab'.repeat(32), status: 'cancelled' });
    const unknown = orderViewOf(kob, s, { covenant_id: 'ac'.repeat(32), state_known: false, state: null });
    const noCur = orderViewOf(kob, s, { covenant_id: 'ad'.repeat(32), current: null });
    const r = escrowedBalancesDetailed([ok, filled, cancelled, unknown, noCur]);
    expect(r.balances).toHaveLength(1);
    expect(r.balances[0].orders).toBe(1);
    expect(r.skipped.map((x) => x.reason)).toEqual(['terminal', 'terminal', 'state-unknown', 'no-current-utxo']);
  });

  it('ignores a spent custody / stray on a view and falls back to the state-implied custody', () => {
    const a = placeGolden(kob, 'create.ask');
    const view = orderViewOf(kob, { ...a.snapshots[0], strays: [strayFor(a.snapshots[0], 9n)] });
    view.strays![0].spent = true;
    view.custody!.utxo!.spent = true;
    const [b] = escrowedBalances([view]);
    expect(b.strays).toBe(0n);
    expect(b.custody).toBe(10000n);
    expect(b.kasCarriers).toBe(BigInt(a.recovered[0].value));
  });
});

describe('withFreeBalances', () => {
  it('counts only maker-owned P2PK token UTXOs and keeps one-sided tokens', () => {
    const a = placeGolden(kob, 'create.ask');
    const esc = escrowedBalances(a.snapshots);
    const free = [
      tokenUtxo(300n, MAKER.pk, 1),
      tokenUtxo(50n, MAKER.pk, 2),
      tokenUtxo(999n, OTHER.pk, 3), // someone else's
      tokenUtxo(777n, a.snapshots[0].covenantId, 4, 1_000_000_000n, 4), // covenant-owned (custody-like)
      { ...tokenUtxo(11n, MAKER.pk, 5), covenantId: '10'.repeat(32) }, // another token, nothing escrowed
    ];
    const out = withFreeBalances(esc, free, MAKER.pk);
    expect(out.map((x) => x.token)).toEqual(['10'.repeat(32), TOKEN.covenantId]);
    expect(out[0]).toMatchObject({ free: 11n, escrowed: 0n, strays: 0n, kasLocked: 0n, orderCount: 0 });
    expect(out[1]).toMatchObject({ free: 350n, escrowed: 10000n, orderCount: 1 });
    // determinism: input order does not matter
    expect(withFreeBalances(esc, [...free].reverse(), MAKER.pk)).toEqual(out);
  });

  it('a token with only escrow (no free UTXO) appears with free = 0', () => {
    const a = placeGolden(kob, 'create.ask');
    const [b] = withFreeBalances(escrowedBalances(a.snapshots), [], MAKER.pk);
    expect(b.free).toBe(0n);
    expect(b.escrowed).toBe(10000n);
  });

  it('kasBalanceSummary adds locked KAS across tokens', () => {
    const a = placeGolden(kob, 'create.ask');
    const bal = withFreeBalances(escrowedBalances(a.snapshots), [], MAKER.pk);
    const s = kasBalanceSummary(5n, bal);
    expect(s).toEqual({ free: 5n, locked: bal[0].kasLocked, total: 5n + bal[0].kasLocked });
  });
});
