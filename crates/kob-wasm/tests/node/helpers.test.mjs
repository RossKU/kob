// The v3 numeric helpers the wallet calls (quote rule, per-kind arithmetic, default minimums), in base units, against an
// independent BigInt model and against the golden order states.
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { createRequire } from 'node:module';
import { fileURLToPath } from 'node:url';
import { dirname, join } from 'node:path';

const here = dirname(fileURLToPath(import.meta.url));
const require = createRequire(import.meta.url);
const kob = require(join(here, '../../pkg-node/kob_wasm.js'));
const golden = JSON.parse(readFileSync(join(here, '../../../kob-protocol/vectors/golden.json'), 'utf8'));
const s = (v) => JSON.stringify(v);
const state = (name) => s(golden.states.find((x) => x.name === name).state);
const I64 = (1n << 63n) - 1n;

const ceilDiv = (a, b) => (a + b - 1n) / b;

test('the quote rule is n * rate / scale rounded up or down, exactly', () => {
  const ns = [0n, 1n, 2n, 7n, 999n, 1000n, 1001n, 123456789n, 10n ** 12n];
  const rates = [0n, 1n, 999n, 1000n, 250000000n, 245100000n, 3n * 10n ** 12n];
  for (const scale of [1n, 10n, 1000n, 100000000n, 1000000000n]) {
    for (const n of ns) {
      for (const rate of rates) {
        const exact = n * rate;
        const up = ceilDiv(exact, scale);
        const down = exact / scale;
        assert.equal(kob.quote(String(n), String(rate), String(scale), 'up'), up <= I64 ? String(up) : undefined, `up ${n} ${rate} ${scale}`);
        assert.equal(kob.quote(String(n), String(rate), String(scale), 'down'), down <= I64 ? String(down) : undefined, `down ${n} ${rate} ${scale}`);
        assert.equal(kob.quoteExact(String(n), String(rate), String(scale), 'up'), String(up));
        assert.equal(kob.quoteExact(String(n), String(rate), String(scale), 'down'), String(down));
      }
    }
  }
  // outside i64: the covenant's arithmetic fails, the exact value still exists
  assert.equal(kob.quote(String(I64), '2', '1', 'up'), undefined);
  assert.equal(kob.quoteExact(String(I64), '2', '1', 'up'), String(I64 * 2n));
  assert.throws(() => kob.quote('1', '1', '1', 'sideways'), /round/);
  assert.throws(() => kob.quote('x', '1', '1', 'up'), /n:/);
});

test('scale and quote bounds', () => {
  for (const ok of ['1', '10', '1000', '1000000000']) kob.checkScale(ok);
  for (const bad of ['0', '-10', '1500', '2', '10000000000']) assert.throws(() => kob.checkScale(bad), /scale/, bad);
  kob.checkQuote('1000', '250000000', '1000', 'price');
  assert.throws(() => kob.checkQuote(String(I64), '2', '1', 'price'), /2\^62/);
  const ask = golden.states.find((x) => x.name === 'create.ask').state;
  kob.checkNumbers(s(ask));
  assert.throws(() => kob.checkNumbers(s({ kind: ask.kind, state: { ...ask.state, scale: '1500' } })), /power of ten/);
  assert.throws(() => kob.checkNumbers(s({ kind: ask.kind, state: { ...ask.state, price: String(I64 / 2n), amountLeft: '10000000000' } })), /2\^62/);
});

test('wallet defaults', () => {
  assert.equal(kob.defaultScale(8), '100000000');
  assert.equal(kob.defaultScale(18), '1000000000');
  assert.equal(kob.defaultScale(0), '1');
  assert.equal(kob.defaultMinFillSompi(), '1000000000');
  // the amount worth DEFAULT_MIN_FILL_SOMPI at the limit price, clamped to 1..amount
  assert.equal(kob.defaultMinFill('10000', '250000000', '1000'), '4000');
  assert.equal(kob.defaultMinFill('1000', '250000000', '1000'), '1000');
  assert.equal(kob.defaultMinFill('10', '1', '1000'), '10');
  assert.equal(kob.defaultMinFill('10', '0', '1000'), '1');
  assert.equal(kob.defaultMinFillIfd('10000'), '2500');
  assert.equal(kob.defaultMinFillIfd('10001'), '2501');
  assert.equal(kob.defaultMinFillPair('10000', '', '1000'), '2500');
  assert.equal(kob.defaultMinFillPair('10000', '250000000', '1000'), '4000');
  assert.equal(kob.defaultMinFillCross, undefined, 'the cross limit is retired');
  assert.equal(kob.defaultMinTouch('4000'), '4000');
  assert.equal(kob.defaultMinTouch('0'), '1');
  const c = JSON.parse(kob.defaultConstants());
  assert.equal(c.defaultMinFillSompi, kob.defaultMinFillSompi());
  assert.equal(c.defaultMinFillImmediate, '1');
  assert.equal(c.maxScale, '1000000000');
  assert.equal(c.quoteLimit, String(1n << 62n));
  assert.equal(c.minRestDaa, '50');
  // the golden orders carry the defaults the wallet would choose
  assert.equal(kob.minFillOk('1000', '10000', '1000'), true);
  assert.equal(kob.minFillOk('999', '10000', '1000'), false);
  assert.equal(kob.minFillOk('999', '999', '1000'), true, 'taking everything left is always allowed');
  assert.equal(kob.minFillOk('0', '10000', '1000'), false);
});

test('ask helpers (both families)', () => {
  for (const name of ['create.ask', 'kron.create.ask']) {
    const st = state(name);
    const o = JSON.parse(st).state;
    const [price, tip, scale] = [BigInt(o.price), BigInt(o.tip), BigInt(o.scale)];
    for (const n of [1n, 999n, 1000n, 4000n, 10000n]) {
      assert.equal(kob.askProceeds(st, String(n), '0', '0'), String(ceilDiv(n * (price - tip), scale)), `${name} ${n}`);
      assert.equal(kob.askProceedsAt(st, String(n), String(price)), String(ceilDiv(n * (price - tip), scale)));
    }
    assert.equal(kob.askProceedsAt(st, '1', String(tip - 1n)), undefined, 'a price below the tip cannot be filled');
    assert.equal(kob.orderPriceAt(st, '5', '7'), o.price);
    assert.equal(kob.fillOk(st, '1000', ''), true);
    assert.equal(kob.fillOk(st, '999', ''), false);
    assert.equal(kob.fillOk(st, '10000', ''), true);
    assert.equal(kob.fillOk(st, '10001', ''), false);
    assert.throws(() => kob.bidUsed(st, '1'), /not a KobBid/);
  }
  // a decaying ask quotes lower as time passes
  const dutch = golden.states.find((x) => x.name === 'create.ask.dutch').state;
  const [p0, p1] = [BigInt(kob.orderPriceAt(s(dutch), '0', '0')), BigInt(kob.orderPriceAt(s(dutch), '100000', '0'))];
  assert.ok(p1 <= p0 && p1 >= BigInt(dutch.state.priceEnd), `${p0} ${p1}`);
});

test('bid helpers (both families)', () => {
  for (const name of ['create.bid', 'kron.create.bid']) {
    const st = state(name);
    const o = JSON.parse(st).state;
    const [price, tip, scale, minFill] = [BigInt(o.price), BigInt(o.tip), BigInt(o.scale), BigInt(o.minFill)];
    const rate = price + tip;
    assert.equal(kob.bidBudgetRate(st), String(rate));
    for (const n of [1n, 999n, 1000n, 10000n]) {
      assert.equal(kob.bidUsed(st, String(n)), String(ceilDiv(n * rate, scale)));
      assert.equal(kob.bidSpend(st, String(n), '0', '0'), String((n * rate) / scale));
      assert.equal(kob.bidSpendAt(st, String(n), String(price)), String((n * rate) / scale));
    }
    // buying power: the largest n whose budget fits what is left after the delivery carrier and the reserve
    const carrier = BigInt(o.deliveryCarrier) + BigInt(o.reserve);
    assert.equal(kob.bidBuyingPower(st, String(carrier)), '0');
    const value = carrier + 5n * rate;
    const power = BigInt(kob.bidBuyingPower(st, String(value)));
    assert.ok(ceilDiv(power * rate, scale) <= value - carrier && ceilDiv((power + 1n) * rate, scale) > value - carrier, name);
    assert.equal(kob.fillOk(st, '1000', String(value)), true);
    assert.throws(() => kob.fillOk(st, '1000', ''), /value/);
    // can_continue: one more minimum fill still funded
    const need = carrier + ceilDiv(minFill * rate, scale);
    assert.equal(kob.bidCanContinue(st, String(need)), true);
    assert.equal(kob.bidCanContinue(st, String(need - 1n)), false);
    // escrow: the budget of the amount, rounding per fill, a carrier per fill, the reserve
    const e = BigInt(kob.bidEscrow(st, '10000', '4'));
    assert.equal(e, ceilDiv(10000n * rate, scale) + 3n + 4n * BigInt(o.deliveryCarrier) + BigInt(o.reserve));
  }
});

test('conditional and if-done helpers', () => {
  const ca = state('create.condAsk');
  const cao = JSON.parse(ca).state;
  assert.equal(kob.condAskProceeds(ca, '1000', cao.tpPrice), String(ceilDiv(1000n * (BigInt(cao.tpPrice) - BigInt(cao.tip)), BigInt(cao.scale))));
  assert.equal(kob.condAskRptBudget(ca, '1000'), String(ceilDiv(1000n * BigInt(cao.rptPrice), BigInt(cao.scale))));
  assert.equal(kob.fillOk(ca, '1000', ''), true);
  const cb = state('create.condBid');
  const cbo = JSON.parse(cb).state;
  assert.equal(kob.condBidSpend(cb, '1000', cbo.tpPrice), String((1000n * (BigInt(cbo.tpPrice) + BigInt(cbo.tip))) / BigInt(cbo.scale)));
  assert.equal(kob.condBidRptProceeds(cb, '1000'), String(ceilDiv(1000n * BigInt(cbo.rptPrice), BigInt(cbo.scale))));
  assert.equal(kob.condBidRptPrefund(cb, '1000'), String(ceilDiv(1000n * BigInt(cbo.rptPre), BigInt(cbo.scale))));
  assert.ok(BigInt(kob.condBidEscrow(cb, '4')) > 0n);
  const ib = state('create.ifdBid.repeat');
  const ibo = JSON.parse(ib).state;
  assert.equal(kob.ifdBidSpend(ib, '1000', ibo.price), String((1000n * (BigInt(ibo.price) + BigInt(ibo.tip))) / BigInt(ibo.scale)));
  assert.equal(kob.ifdBidMergeBudget(ib, '1000'), String(ceilDiv(1000n * (BigInt(ibo.price) + BigInt(ibo.tip)), BigInt(ibo.scale))));
  assert.ok(BigInt(kob.ifdBidEscrow(ib)) > 0n);
  const ia = state('create.ifdAsk.repeat');
  const iao = JSON.parse(ia).state;
  assert.equal(kob.ifdAskProceeds(ia, '1000', iao.price), String(ceilDiv(1000n * (BigInt(iao.price) - BigInt(iao.tip)), BigInt(iao.scale))));
  assert.equal(kob.ifdAskPrefund(ia, '1000'), String(ceilDiv(1000n * BigInt(iao.prefund), BigInt(iao.scale))));
  assert.ok(BigInt(kob.ifdAskEscrow(ia, '100000000')) > 0n);
  assert.ok(kob.ifdAskMergeSelloutBack(ia, '1000', '500000000') !== undefined);
  assert.equal(kob.fillOk(ia, '1', ''), BigInt(iao.minFill) <= 1n);
});

test('pair order helpers: B per whole A in the maker\'s favour, custodies, evidence, exits', () => {
  const big = (x) => BigInt(x);
  const up = (n, r, sc) => ceilDiv(big(n) * big(r), big(sc));
  const down = (n, r, sc) => (big(n) * big(r)) / big(sc);
  for (const fam of ['', 'kcc20-kron.', 'kron-kcc20.', 'kron-kron.']) {
    // KobPair ASK: sells n of A, the maker receives ceil(n * p / scale(A)) of B; BID: pays exactly floor(n * p / scale(A)) of B
    const ask = state(`pair.${fam}create.ask`);
    const ao = JSON.parse(ask).state;
    for (const [n, p] of [['1000', ao.price], ['1333', '1001'], [ao.amountLeft, ao.price]]) {
      assert.equal(kob.pairSOut(ask, n, p), n);
      assert.equal(kob.pairTOutMin(ask, n, p), String(up(n, p, ao.sScale)));
      assert.equal(kob.pairTipKas(ask, n), String(down(n, ao.tip, ao.sScale)));
    }
    assert.equal(kob.orderPriceAt(ask, '0', '0'), ao.price);
    assert.equal(kob.fillOk(ask, ao.minFill, ''), true);
    assert.equal(kob.fillOk(ask, String(big(ao.minFill) - 1n), ''), false);
    const f = JSON.parse(kob.pairFill(ask, ao.minFill, ao.price, '100000000000'));
    assert.deepEqual([f.sOut, f.tOut, f.rest, f.outAmount], [ao.minFill, String(up(ao.minFill, ao.price, ao.sScale)), true, String(big(ao.amountLeft) - big(ao.minFill))]);
    assert.throws(() => kob.pairFill(ask, '1', ao.price, '100000000000'), /minFill/);
    assert.equal(kob.pairMaxTakeable(ask, ao.price, '100000000000'), ao.amountLeft);
    const bid = state(`pair.${fam}create.bid`);
    const bo = JSON.parse(bid).state;
    assert.equal(kob.pairSOut(bid, '1333', '1001'), String(down('1333', '1001', bo.tScale)));
    assert.equal(kob.pairTOutMin(bid, '1333', '1001'), '1333');
    // exact floors are subadditive: the escrow is floor(amount * pMax / scale(A)) + 1, whatever the number of fills
    assert.equal(kob.pairBidEscrow(bid, bo.amountLeft, '4'), String(down(bo.amountLeft, bo.price, bo.tScale) + 1n));
    assert.equal(kob.pairBidEscrow(bid, bo.amountLeft, '4'), bo.custody);
    assert.equal(kob.pairPriceMax(bid), bo.price);
    assert.equal(kob.pairFundedFills(bid), '2');
    assert.equal(kob.pairKasValue(bid, kob.pairFundedFills(bid)), kob.minOrderValue(bid));
    assert.equal(kob.pairMaxFills(bid), String(ceilDiv(big(bo.amountLeft), big(bo.minFill))));
    const t = JSON.parse(kob.pairTokens(bid));
    assert.deepEqual([t.kind, t.side, t.a.covId, t.b.covId, t.a.scale], ['KobPair', 'bid', bo.tCovId, bo.sCovId, bo.tScale]);
    assert.deepEqual(JSON.parse(kob.custodies(bid)), [{ token: bo.sCovId, amount: bo.custody }]);
    // KobCondPair: legs and the trigger rule (a sell stop arms on a resting ask of A and a resting bid of B, or a pair ASK at or below it)
    const ca = state(`pair.${fam}create.condAsk`);
    const co = JSON.parse(ca).state;
    assert.equal(kob.condPairLegPrice(ca, '0', 'false', '0', '0'), co.tpPrice);
    const bounds = JSON.parse(kob.condPairBounds(ca));
    assert.equal(bounds.stopWorst, String(big(co.stopPrice) - (big(co.stopPrice) * big(co.slipBps)) / 10000n));
    assert.equal(kob.pairTOutMin(ca, '1000', co.tpPrice), String(up('1000', co.tpPrice, co.sScale)));
    const rule = JSON.parse(kob.pairTriggerRule(ca));
    assert.deepEqual([rule.direction, rule.arm.kasBooks.a, rule.arm.kasBooks.b, rule.arm.pair, rule.minTouch], ['fallsTo', 'ask', 'bid', 'ask', co.minTouch]);
    assert.equal(rule.minTouchB, String(up(co.minTouch, co.stopPrice, co.sScale)));
    assert.equal(kob.pairArms(ca, s({ mode: 'pair', price: co.stopPrice })), true);
    assert.equal(kob.pairArms(ca, s({ mode: 'pair', price: String(big(co.stopPrice) + 1n) })), false);
    // mode 0: the implied rate a * scale(B) / b B per whole A, here exactly the stop and one sompi above it
    const bScale = co.tScale;
    assert.equal(kob.pairArms(ca, s({ mode: 'kasBooks', a: String(big(co.stopPrice) * 1000n), b: String(big(bScale) * 1000n) })), true);
    assert.equal(kob.pairArms(ca, s({ mode: 'kasBooks', a: String(big(co.stopPrice) * 1000n + 1n), b: String(big(bScale) * 1000n) })), false);
    assert.equal(kob.impliedLe(String(big(co.stopPrice) * 1000n), String(big(bScale) * 1000n), co.stopPrice, bScale), true);
    const cb = state(`pair.${fam}create.condBid`);
    const cbo = JSON.parse(cb).state;
    const r2 = JSON.parse(kob.pairTriggerRule(cb));
    assert.deepEqual([r2.direction, r2.arm.kasBooks.a, r2.arm.kasBooks.b, r2.arm.pair], ['risesTo', 'bid', 'ask', 'bid']);
    assert.equal(kob.condPairBidEscrow(cb, String(ceilDiv(big(cbo.amountLeft), big(cbo.minFill)))), cbo.custody);
    // KobIfdPair: the committed exit round-trips; exits of a fill; the sell-first entry's two custodies
    for (const side of ['ifdBid', 'ifdAsk']) {
      const e = state(`pair.${fam}create.${side}`);
      const eo = JSON.parse(e).state;
      const exit = kob.ifdPairExit(e);
      assert.equal(kob.ifdPairCommitExit(exit), eo.exitState);
      const x = JSON.parse(kob.ifdPairExitFor(e, '1000', '1000', '', '0'));
      assert.deepEqual([x.kind, x.state.side, x.state.amountLeft, x.state.custody], ['KobCondPair', side === 'ifdBid' ? '1' : '2', '1000', '1000']);
      const a = JSON.parse(kob.ifdPairAmounts(e, '1333', eo.price));
      assert.equal(a.spend, String(down('1333', eo.price, eo.aScale)));
      assert.equal(a.proceeds, String(up('1333', eo.price, eo.aScale)));
      assert.equal(a.pre, String(up('1333', eo.prefund, eo.aScale)));
      assert.equal(kob.ifdPairPriceAt(e, 'false', '0', '0'), eo.price);
      assert.equal(kob.ifdPairBCustodyNeeded(e), eo.custody);
      assert.ok(BigInt(kob.ifdPairExitCarrierNeeded(e)) <= BigInt(eo.exitCarrier), `${fam}${side}: the golden entry funds its exits`);
      const cs = JSON.parse(kob.custodies(e));
      assert.equal(cs.length, side === 'ifdBid' ? 1 : 2);
      assert.equal(cs.at(-1).token, eo.bCovId);
    }
  }
  assert.equal(kob.minTouchB('1000', '1500', '1000'), '1500');
  assert.deepEqual(JSON.parse(kob.pairEvidenceModes()).map((m) => [m.mode, m.name]), [[0, 'kasBooks'], [1, 'pair']]);
  assert.throws(() => kob.pairTipKas(state('create.ask'), '1'), /not a KobPair/);
  assert.equal(kob.crossBOutMin, undefined);
});
