import { describe, expect, it } from 'vitest';
import type { StrayView } from '../../data/indexer-types';
import { loadKobNode } from '../../kob/wasm.node';
import { MAKER, TOKEN, orderViewOf, placeGolden, strayFor, tokenUtxo, tokenUtxoView } from '../../testing/chain-fixtures';
import { balanceColumnFractions, combineBalances, strayTotals, unreviewedProgram } from './balances-model';
import { unitsFraction } from '../kit';

const kob = loadKobNode();
const placed = placeGolden(kob, 'create.ask');
const snap = placed.snapshots[0];
const strayUtxo = strayFor(snap, 7n);
const strayView = (over: Partial<StrayView> = {}): StrayView => ({ ...tokenUtxoView(strayUtxo, 'stray'), order_status: 'open', maker: MAKER.pk, lost: false, ...over });

describe('strayTotals', () => {
  it('splits recoverable and lost strays per token and skips spent ones', () => {
    const t = strayTotals([strayView(), strayView({ amount: '1000000000', state: { ...strayUtxo.state, amount: '5' }, lost: true }), strayView({ spent: true })]);
    expect(t.get(TOKEN.covenantId)).toEqual({ recoverable: 7n, lost: 5n });
  });
});

describe('combineBalances', () => {
  const custody = BigInt(placed.recovered[0].custody!.state.amount);

  it('adds free tokens, escrowed custody and KAS locked; KAS total is free + locked', () => {
    const view = orderViewOf(kob, snap);
    const c = combineBalances({ maker: MAKER.pk, freeKas: 5_000_000_000n, freeTokens: [tokenUtxo(300n, MAKER.pk)], liveOrders: [view], strays: [] });
    expect(c.tokens).toHaveLength(1);
    expect(c.tokens[0]).toMatchObject({ token: TOKEN.covenantId, free: 300n, escrowed: custody, strays: 0n, orderCount: 1 });
    expect(c.tokens[0].kasLocked).toBeGreaterThan(0n);
    expect(c.kas.free).toBe(5_000_000_000n);
    expect(c.kas.total).toBe(5_000_000_000n + c.tokens[0].kasLocked);
  });

  it('the separate stray list replaces strays inside the order view (nothing is counted twice)', () => {
    const view = orderViewOf(kob, { ...snap, strays: [strayUtxo] });
    const withBoth = combineBalances({ maker: MAKER.pk, freeKas: 0n, freeTokens: [], liveOrders: [view], strays: [strayView()] });
    expect(withBoth.tokens[0].strays).toBe(7n);
    // the view alone (no list) contributes no strays: the list is authoritative
    const viewOnly = combineBalances({ maker: MAKER.pk, freeKas: 0n, freeTokens: [], liveOrders: [view], strays: [] });
    expect(viewOnly.tokens[0].strays).toBe(0n);
  });

  it('lost strays are reported separately and never counted as recoverable', () => {
    const c = combineBalances({ maker: MAKER.pk, freeKas: 0n, freeTokens: [], liveOrders: [], strays: [strayView({ lost: true })] });
    expect(c.lostStrayTokens).toBe(7n);
    expect(c.tokens).toEqual([]); // nothing recoverable, nothing free, nothing locked
  });

  it('a token with only free tokens or only strays still appears; empty tokens do not', () => {
    const c = combineBalances({ maker: MAKER.pk, freeKas: 1n, freeTokens: [tokenUtxo(0n, MAKER.pk)], liveOrders: [], strays: [strayView()] });
    expect(c.tokens.map((t) => [t.token, t.strays, t.free])).toEqual([[TOKEN.covenantId, 7n, 0n]]);
  });

  it('tokens of other owners are not free, and snapshots (node-resolved orders) count as escrow', () => {
    const c = combineBalances({ maker: MAKER.pk, freeKas: 0n, freeTokens: [tokenUtxo(999n, 'aa'.repeat(32))], liveOrders: [snap], strays: [] });
    expect(c.tokens[0]).toMatchObject({ free: 0n, escrowed: custody });
  });
});

describe('unreviewedProgram', () => {
  it('marks every program whose profile is not reviewed, and an unknown one', () => {
    expect(unreviewedProgram('KCC20Ref')).toBe(false);
    expect(unreviewedProgram('KCC20PublicMint')).toBe(false);
    expect(unreviewedProgram('KronToken2433')).toBe(false);
    expect(unreviewedProgram('KCC20Ref_8x8')).toBe(true);
    expect(unreviewedProgram('KCC20P2')).toBe(true);
    expect(unreviewedProgram('KCC20KaspaCom_0_2_5')).toBe(true);
    expect(unreviewedProgram(null)).toBe(true);
  });
});

describe('combineBalances: tokens KOB cannot spend', () => {
  it('a KRON token held only by the key (id_type 0) keeps its row, with nothing free', () => {
    const kron = '9a'.repeat(32);
    const u = { transactionId: '81'.repeat(32), index: 0, amount: '1000', covenantId: kron, state: { amount: '12', owner: MAKER.pk, id_type: 0, is_minter: 0 } };
    const c = combineBalances({ maker: MAKER.pk, freeKas: 0n, freeTokens: [u], liveOrders: [], strays: [] });
    expect(c.tokens).toMatchObject([{ token: kron, free: 0n, notSpendable: 12n }]);
  });
});

describe('balanceColumnFractions', () => {
  const row = (token: string, free: bigint) => ({ token, free, notSpendable: 0n, escrowed: 0n, strays: 0n, kasLocked: 0n, kasEscrow: 0n, kasCarriers: 0n, orderCount: 0 });
  it('aligns the known tokens and leaves a token of unknown decimals out (TN10 2026-10-15: 500,000 base units shown as 500,000.00000000)', () => {
    const tusd = 'aa'.repeat(32);
    const unknown = 'bb'.repeat(32);
    const tokens = [row(tusd, 100_296_367n), row(unknown, 500_000n)];
    const decimals = (t: string) => (t === tusd ? 8 : undefined);
    expect(balanceColumnFractions(tokens, decimals, unitsFraction).free).toBe(8);
    // only the unknown token: no fraction at all
    expect(balanceColumnFractions([row(unknown, 500_001n)], decimals, unitsFraction).free).toBe(0);
    // a registry token with 0 decimals still counts (0)
    expect(balanceColumnFractions([row(unknown, 5n)], () => 0, unitsFraction).free).toBe(0);
  });
});
