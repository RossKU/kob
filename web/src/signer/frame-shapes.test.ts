// The private-key frame signs only its closed list of shapes, judged on bound data (signer/shapes.ts).
// Every hostile transaction here is made at the transaction level from honest builds and decoded as
// the frame sees it: no node facts, no planner claims, no registry.
import { describe, expect, it } from 'vitest';
import { genesisCovenantId } from '../kob/covenant-id';
import type { BuiltTx } from '../kob/types';
import { loadKobNode } from '../kob/wasm.node';
import { golden, goldenRequest, keyUtxo, MAKER, placeGolden, strayFor } from '../testing/chain-fixtures';
import { planSweep, type CancelEnv } from '../kob/cancel';
import type { BookOrderUtxo, BookView, PlanEnv } from '../kob/plan-types';
import { planOrder } from '../kob/plan';
import { KAS as FKAS, OTHER_PK, TOK, keyUtxo as fKeyUtxo, makeEnv, market3x3, tokenUtxo as fTokenUtxo, CLOCK } from '../testing/fixtures';
import { snapshotOfRecovered } from '../testing/chain-fixtures';
import { bundle, continuationAsGenesis } from '../testing/hostile-tx';
import { summarize } from './summary';

const kob = loadKobNode();
const build = (name: string): BuiltTx => kob.build(goldenRequest(name, MAKER.pk) as never);
const frame = (b: BuiltTx) => summarize(kob, b, MAKER.pk, []);
const cancelEnv = (): CancelEnv => ({ kob, maker: MAKER.pk, funding: [keyUtxo(MAKER.pk, 100n * 100_000_000n, 201)], tokenUtxos: [], clock: { daa: 78_000_000n } });
const M = market3x3();
/** A resting order of OTHER_PK, placed for real (kob/decode-take.test.ts). */
function resting(side: 'sell' | 'buy', price: bigint, amount: bigint): { row: { covenantId: string; price: bigint; amount: bigint; tip: bigint }; utxo: BookOrderUtxo } {
  const env: PlanEnv = { ...makeEnv({ market: M, book: { asks: [], bids: [] } }), maker: OTHER_PK, funding: [fKeyUtxo(10_000n * FKAS, OTHER_PK)], tokenUtxos: [fTokenUtxo(M, 1_000n * TOK, 10n * FKAS, OTHER_PK)] };
  const p = planOrder(env, { type: 'limit', side, amount, price, minFill: TOK });
  const snap = snapshotOfRecovered(kob.recoverOrders(p.built!.tx)[0]!, CLOCK.daa - 1000n);
  return { row: { covenantId: snap.covenantId, price, amount, tip: 0n }, utxo: { covenantId: snap.covenantId, order: snap.order, custody: snap.custody } };
}
/** A take by the wallet of OTHER_PK's resting orders, as the app plans it. */
function takeOf(side: 'buy' | 'sell'): BuiltTx {
  const r = side === 'buy' ? resting('sell', 250_000_000n, 3n * TOK) : resting('buy', 250_000_000n, 3n * TOK);
  const level = { price: r.row.price, amount: r.row.amount, orders: 1 };
  const book: BookView = side === 'buy' ? { asks: [level], bids: [], askOrders: [r.row], bidOrders: [] } : { asks: [], bids: [level], askOrders: [], bidOrders: [r.row] };
  const env = { ...makeEnv({ book, tokenAmounts: [100n * TOK] }), bookOrders: new Map([[r.row.covenantId.toLowerCase(), r.utxo]]) };
  const p = planOrder(env, { type: 'limit', side, amount: 2n * TOK, price: side === 'buy' ? 255_000_000n : 245_000_000n, crossing: 'take' });
  return p.built!;
}
/** The order input whose covenant continues in an output (the amended or swept order). */
const orderInput = (b: BuiltTx) => b.plans.findIndex((p, i) => p.kind === 'entry' && b.tx.outputs.some((o) => o.covenant?.authorizingInput === i && b.tx.outputs.filter((x) => x.covenant?.covenantId === o.covenant!.covenantId).length === 1 && !b.tx.outputs.some((x) => x.covenant && x.covenant.covenantId === o.covenant!.covenantId && x !== o)));

describe('R9FB2-1: a continuation bound to the genesis id of its order input is the order\'s end, not an amend or sweep', () => {
  const sweepOf = (name: string): BuiltTx => {
    const snap = placeGolden(kob, name).snapshots[0]!;
    return planSweep(cancelEnv(), { ...snap, strays: [strayFor(snap, 4n, 92)] }).built!;
  };
  const cases: [string, () => BuiltTx][] = [
    ...['amend.ask', 'amend.bid', 'kron.amend.ask', 'amend.curve'].map((n) => [n, () => build(n)] as [string, () => BuiltTx]),
    ...['create.ask', 'create.bid', 'create.curve', 'pair.create.ask'].map((n) => [`sweep of ${n}`, () => sweepOf(n)] as [string, () => BuiltTx]),
  ];
  for (const [name, make] of cases) {
    it(name, () => {
      const honest = make();
      expect(frame(honest).problems).toBe(0);
      const b = continuationAsGenesis(honest, orderInput(honest), genesisCovenantId);
      const s = frame(b);
      expect(s.problems).toBeGreaterThan(0);
      expect(s.problemCodes).toContain('order-spk-mismatch');
    });
  }
});

describe('R9FB2-2: a fill bundled with a spend of an own order is refused in the frame', () => {
  for (const [side, own] of [['buy', 'sweep'], ['buy', 'amend.bid'], ['buy', 'cancel.bid'], ['sell', 'cancel.ask'], ['buy', 'create.ask']] as const) {
    it(`a ${side} take with ${own}`, () => {
      const ownTx = own === 'sweep' ? (() => { const snap = placeGolden(kob, 'create.bid').snapshots[0]!; return planSweep(cancelEnv(), { ...snap, strays: [strayFor(snap, 4n, 92)] }).built!; })() : build(own);
      const fill = takeOf(side);
      expect(frame(fill).problems).toBe(0);
      const s = frame(bundle(kob, fill, ownTx));
      expect(s.problems).toBeGreaterThan(0);
      expect(s.problemCodes).toContain('frame-shape');
    });
  }
});

describe('the honest shapes the app sends pass in the frame, with their lines', () => {
  const refusedLabel = 'This signer does not sign this transaction';
  // every golden action the frame signs as the wallet's own, by family
  const honest = golden().transactions.map((g) => g.name).filter((n) => /^(kron\d*\.|pair\.[a-z0-9-]*\.?)?(create|cancel|cancelReplace|amend|refund|send|take)\./.test(n) || /^(pair\.)?[a-z0-9-]*\.?(ask|bid)\.(ioc|fok|market|close)$/.test(n));
  it('none of them is refused by its shape', () => {
    const refused: string[] = [];
    for (const n of honest) {
      let b: BuiltTx;
      try {
        b = build(n);
      } catch {
        continue;
      }
      if (!b.sign.some((r) => r.pubkey === MAKER.pk)) continue;
      const s = frame(b);
      if (s.problemCodes.includes('frame-shape') && s.problemCodes.length === 1) refused.push(n);
    }
    expect(refused).toEqual([]);
  });

  it('a take lists each payment to a maker and the bound net, not the claimed fills', () => {
    const s = frame(takeOf('buy'));
    expect(s.problems).toBe(0);
    expect(s.lines.some((l) => l.label === 'Pays')).toBe(true);
    expect(s.lines.some((l) => l.label === 'You pay, with the fee')).toBe(true);
    expect(s.lines.some((l) => l.label === 'Fill price')).toBe(false);
    expect(s.lines.some((l) => l.label === refusedLabel)).toBe(false);
  });

  it('a sell take lists the tokens it delivers to each maker', () => {
    const s = frame(takeOf('sell'));
    expect(s.problems).toBe(0);
    expect(s.lines.some((l) => l.label === 'Delivers')).toBe(true);
  });

  it('a cancel names the order with its bound terms; a send names the key it pays', () => {
    const c = frame(build('cancel.ask'));
    expect(c.problems).toBe(0);
    expect(c.lines.find((l) => l.label === 'Closes order')!.value).toMatch(/KobAsk, sell/);
    const s = frame(build('send.tokens'));
    expect(s.problems).toBe(0);
    expect(s.lines.some((l) => /to other keys/.test(l.label))).toBe(true);
  });
});

describe('R9FB2-8: a pair order is checked against the decimals the frame shows for both its tokens', () => {
  it('a hint whose decimals do not match the order state blocks', () => {
    const b = build('pair.create.ask');
    const order = frame(b).decoded!.orders[0]!.description.pair!;
    const wrong = (covenantId: string, scale: bigint) => ({ covenantId, ticker: 'XX', decimals: Math.max(0, scale.toString().length - 1) + 2, source: 'declared' as const });
    expect(summarize(kob, b, MAKER.pk, [wrong(order.base.covId, order.base.scale)]).problemCodes).toContain('token-scale');
    expect(summarize(kob, b, MAKER.pk, [wrong(order.quote.covId, order.quote.scale)]).problemCodes).toContain('token-scale');
  });
});
