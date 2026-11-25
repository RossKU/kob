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
import { planCancelAll } from '../kob/cancel';
import { KAS as FKAS, OTHER_PK, TOK, keyUtxo as fKeyUtxo, makeEnv, market3x3, tokenUtxo as fTokenUtxo, CLOCK } from '../testing/fixtures';
import { snapshotOfRecovered } from '../testing/chain-fixtures';
import { bundle, continuationAsGenesis } from '../testing/hostile-tx';
import { summarize } from './summary';
import { decodeSigning } from '../kob/decode';

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
      const b = continuationAsGenesis(honest, orderInput(honest), (op, outs) => genesisCovenantId(kob, op, outs));
      const s = frame(b);
      expect(s.problems).toBeGreaterThan(0);
      // the wasm's amend and sweep recovery refuses it by itself now (recover-failed / sweep-invalid);
      // the decoder's own genesis test says so too where the record is recovered
      expect(s.problemCodes.some((c) => ['order-spk-mismatch', 'recover-failed', 'sweep-invalid'].includes(c))).toBe(true);
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

describe('a sale curve placed at its token creation, before its first sale, in the frame', () => {
  it('its cancel is refused there (its custody names no token the frame could check), and passes the app path', async () => {
    const { issueRaw } = await import('../kob/issue');
    const { curveTable } = await import('../kob/curve');
    const { planCancel } = await import('../kob/cancel');
    const { snapshotOfRecovered } = await import('../testing/chain-fixtures');
    const S = 100_000_000n;
    const t = curveTable(kob, { amount: 900n * S, levels: 10, total: 100n * S, multipleMilli: 5000n, scale: S });
    const plan = issueRaw(kob, {
      name: 'Curve Token', ticker: 'CRV', decimals: 8, supply: (1000n * S).toString(),
      holders: [{ owner: '00'.repeat(32), ownerScheme: 4, amount: (900n * S).toString() }, { owner: MAKER.pk, ownerScheme: 0, amount: (100n * S).toString() }],
      funding: [{ transactionId: '07'.repeat(32), index: 1, amount: (500n * S).toString(), blockDaaScore: '5000', pubkey: MAKER.pk }],
      network: 'testnet-10',
      curve: { custody: 0, levelSize: t.levelSize.toString(), table: t.table.map(String), minFill: '1' },
    } as never);
    const rec = kob.recoverOrders(plan.built.tx)[0]!;
    const snap = snapshotOfRecovered(rec, 6000n);
    const c = planCancel({ kob, maker: MAKER.pk, funding: [keyUtxo(MAKER.pk, 10_000_000_000n, 201)], tokenUtxos: [], clock: { daa: 78_000_000n } }, snap);
    expect(c.ok, JSON.stringify(c.issues)).toBe(true);
    const s = frame(c.built!);
    expect(s.problems).toBeGreaterThan(0);
    expect(s.problemCodes).toContain('custody-unreleased');
  });
});

describe('R9FB2-1, both lines: the wasm recovery refuses a genesis-bound amend, and the decoder\'s own test still fires when it does not', () => {
  for (const name of ['amend.ask', 'amend.bid', 'kron.amend.ask', 'amend.curve']) {
    it(name, () => {
      const honest = build(name);
      const at = orderInput(honest);
      const b = continuationAsGenesis(honest, at, (op, outs) => genesisCovenantId(kob, op, outs));
      const g = b.tx.outputs.find((o) => o.covenant?.authorizingInput === at)!.covenant!.covenantId;
      // first line: the recovery refuses it, blocking in the app decode, the frame decode and the
      // frame summary, with the verifier's reason carried
      for (const s of [decodeSigning({ kob, built: b, maker: MAKER.pk, registry: null, expected: { orders: [] } }), decodeSigning({ kob, built: b, maker: MAKER.pk, registry: null })]) {
        const f = s.blocking.find((x) => x.code === 'recover-failed');
        expect(f).toBeTruthy();
        expect(String(f!.params?.reason ?? '')).not.toBe('');
      }
      expect(frame(b).problemCodes).toContain('recover-failed');
      // second line: a recovery that takes the claimed id (as the wasm did before this round) still
      // meets the decoder's genesis test
      const recovered = kob.recoverAmends(honest.tx, honest.plans).map((r) => ({ ...r, covenantId: g }));
      const stub = { ...kob, recoverAmends: () => recovered } as typeof kob;
      const s = decodeSigning({ kob: stub, built: b, maker: MAKER.pk, registry: null });
      expect(s.blocking.map((x) => x.code)).not.toContain('recover-failed');
      expect(s.blocking.map((x) => x.code)).toContain('order-spk-mismatch');
      expect(summarize(stub, b, MAKER.pk, []).problemCodes).toContain('order-spk-mismatch');
    });
  }
});

describe('R9FB2-9: the decoder\'s info findings are shown in the frame', () => {
  it('a sweep says what it sweeps as a note line', () => {
    const snap = placeGolden(kob, 'create.ask').snapshots[0]!;
    const b = planSweep(cancelEnv(), { ...snap, strays: [strayFor(snap, 4n, 92)] }).built!;
    const s = frame(b);
    expect(s.decoded!.info.length).toBeGreaterThan(0);
    // every info finding is a note, but those that only repeat a line above (strays or KAS returned)
    const shown = s.decoded!.info.filter((f) => !['strays-swept', 'other-strays-swept', 'kas-released'].includes(f.code));
    expect(shown.length).toBeGreaterThan(0);
    expect(s.lines.filter((l) => l.label === 'Note').length).toBe(shown.length);
    expect(s.lines.some((l) => /raw amount/.test(l.value))).toBe(false);
  });
});

describe('a take explains where the wallet\'s KAS goes, from bound data (no "script this signer cannot read")', () => {
  it('a partial take of an ask: a payment to the maker by its whole key, the order\'s own remainder apart, scaled token units', () => {
    const s = frame(takeOf('buy'));
    expect(s.problems).toBe(0);
    expect(s.lines.some((l) => /cannot read/.test(l.value))).toBe(false);
    const pay = s.lines.find((l) => l.label === 'Pays')!;
    expect(pay.value).toContain(OTHER_PK);
    expect(pay.mono).toBe(true);
    expect(s.lines.some((l) => /^Stays in the orders you fill/.test(l.label))).toBe(true);
    expect(s.lines.find((l) => l.label === 'You pay, with the fee')!.value).not.toMatch(/ \d{6,}$/);
  });
});

// every action through the frame, its lines as text (written for the review when KOB_FRAME_LINES is set)
it('the frame lines of each action', async () => {
  const out = process.env.KOB_FRAME_LINES;
  if (!out) return;
  const { claim } = await import('../testing/claim-fixtures');
  const fs = await import('node:fs');
  const snapAsk = placeGolden(kob, 'create.ask').snapshots[0]!;
  const fullTake = () => {
    const r = resting('sell', 250_000_000n, 2n * TOK);
    const level = { price: r.row.price, amount: r.row.amount, orders: 1 };
    const env = { ...makeEnv({ book: { asks: [level], bids: [], askOrders: [r.row], bidOrders: [] }, tokenAmounts: [100n * TOK] }), bookOrders: new Map([[r.row.covenantId.toLowerCase(), r.utxo]]) };
    return planOrder(env, { type: 'limit', side: 'buy', amount: 2n * TOK, price: 255_000_000n, crossing: 'take' }).built!;
  };
  const cases: [string, () => BuiltTx][] = [
    ['take of an ask, partial', () => takeOf('buy')],
    ['take of an ask, full', fullTake],
    ['take of a bid (a sell)', () => takeOf('sell')],
    ['send', () => build('send.tokens')],
    ['place an ask', () => build('create.ask')],
    ['place a bid', () => build('create.bid')],
    ['cancel', () => build('cancel.ask')],
    ['cancel-all (two orders)', () => planCancelAll(cancelEnv(), [snapAsk, placeGolden(kob, 'create.condAsk').snapshots[0]!]).find((p) => p.ok)!.built!],
    ['cancel-replace', () => build('cancelReplace.ask')],
    ['amend in place', () => build('amend.ask')],
    ['sweep', () => planSweep(cancelEnv(), { ...snapAsk, strays: [strayFor(snapAsk, 4n, 92)] }).built!],
    ['claim', () => claim('create.bid.acc').built],
    ['refund', () => build('refund.ask.expiry')],
  ];
  const text: string[] = [];
  for (const [name, make] of cases) {
    const s = summarize(kob, make(), MAKER.pk, [{ covenantId: '70'.repeat(32), ticker: 'EXKCC', decimals: 3, source: 'declared' }]);
    text.push(`### ${name}: ${s.title}${s.problems ? ` (PROBLEMS: ${s.problemCodes.join(', ')})` : ''}`);
    for (const l of s.lines) text.push(`  ${l.label}: ${l.value}${l.dir ? ` [${l.dir}]` : ''}`);
    text.push(`  Network fee: ${s.fee}`);
    for (const o of s.orders) text.push(`  [order] ${o.title}: ${o.rows.map((r) => `${r.label} ${r.value}`).join(' | ')}`);
  }
  fs.writeFileSync(out, text.join('\n'));
});

describe('a token the frame cannot name: the unit is "token", the token is named once in full (founder, TN10 TUSD)', () => {
  it('a market buy with a scale hint and the app\'s name', () => {
    const b = build('create.bid.market');
    const cov = (kob.recoverOrders(b.tx)[0]!.order.state as unknown as { state: { tokenCovId: string } }).state?.tokenCovId ?? (kob.recoverOrders(b.tx)[0]!.order.state as unknown as { tokenCovId: string }).tokenCovId;
    const s = summarize(kob, b, MAKER.pk, [{ covenantId: cov, ticker: '', decimals: 8, source: 'scale', appName: 'TUSD' }]);
    const o = s.orders[0]!;
    expect(o.token).toBe(`${cov} (8 decimals, from its price scale) the app calls it TUSD (not verified by this signer)`);
    const values = o.rows.map((r) => r.value).join(' | ');
    expect(values).toMatch(/KAS per token/);
    expect(values).not.toMatch(/[0-9a-f]{8}…[0-9a-f]{8}/);
    expect(values).not.toMatch(/TUSD/);
    expect(o.rows.some((r) => r.label === 'Buys with this budget')).toBe(true);
  });
});

describe('an amend in place changes the order, it does not close it', () => {
  it('"Changes order" with the whole id and the price before and after; no "Closes order"', () => {
    const s = frame(build('amend.ask'));
    expect(s.problems).toBe(0);
    expect(s.lines.some((l) => l.label === 'Closes order')).toBe(false);
    const c = s.lines.find((l) => l.label === 'Changes order')!;
    expect(c.value).toMatch(/^[0-9a-f]{64} \(the id is the app.s word\): KobAsk, sell; price .+ KAS per .+ → .+ KAS per /);
    expect(c.mono).toBe(true);
  });
});

describe('a flipped market (KAS/TOKEN) on the signing screens', () => {
  const inv = (cov: string, ticker = 'EXKCC') => [{ covenantId: cov, ticker, decimals: 3, source: 'declared' as const, inverted: true }];
  const covOf = (b: BuiltTx) => (kob.recoverOrders(b.tx)[0]!.order.state as unknown as { tokenCovId: string }).tokenCovId;
  it('a market buy reads as a sale of KAS; the budget row is not repeated, and "You sell" is the whole budget (no refund tip taken out)', () => {
    const b = build('create.bid.market');
    const s = summarize(kob, b, MAKER.pk, inv(covOf(b)));
    const o = s.orders[0]!;
    expect(o.title).toMatch(/^Sell KAS/);
    const labels = o.rows.map((r) => r.label);
    expect(labels).not.toContain('KAS budget');
    expect(labels).not.toContain('Buys with this budget');
    const rec = kob.recoverOrders(b.tx)[0]!;
    const st = rec.order.state as unknown as { deliveryCarrier: string };
    const sell = o.rows.find((r) => r.label === 'You sell')!.value;
    // one fill: the value less its one delivery carrier (9.892 KAS, said as 9.90 rounded against the
    // user); with the refund tip taken out it would read 9.86
    expect(BigInt(rec.value) - BigInt(st.deliveryCarrier)).toBe(989_200_000n);
    expect(sell).toBe('up to 9.90 KAS');
    expect(o.rows.map((r) => r.value).join(' ')).toMatch(/per KAS/);
  });
  it('a conditional buy on a flipped market says it is shown as the chain holds it', () => {
    const b = build('create.condBid');
    const s = summarize(kob, b, MAKER.pk, inv(covOf(b)));
    expect(s.orders[0]!.token).toMatch(/shown as the chain holds it \(TOKEN\/KAS\), not in your flipped KAS\/TOKEN view$/);
  });
  it('a take on a flipped market says its prices are in KAS per token', () => {
    const b = takeOf('buy');
    const cov = (b.plans.find((p) => p.kind === 'entry') as unknown as { state?: string }) ? (kob.recoverOrders as unknown as never, '') : '';
    void cov;
    const tokenId = frame(b).decoded!.take!.ref.covenantId;
    const s = summarize(kob, b, MAKER.pk, inv(tokenId));
    expect(s.lines.some((l) => l.label === 'Orientation' && /KAS per token/.test(l.value))).toBe(true);
  });
});
