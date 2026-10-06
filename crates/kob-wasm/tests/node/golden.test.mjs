// Loads the wasm build of kob-protocol (scripts/build-wasm.sh) and reproduces every Rust golden
// vector byte-for-byte: request -> unsigned transaction and signing plan, wallet signatures ->
// signed transaction (with the compute budgets re-measured by the script engine inside the wasm),
// plus the state and KOB1 payload codecs and the embedded template registry.
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

test('embedded artifacts are pinned and match the vectors', () => {
  kob.selfCheck();
  assert.equal(kob.templates(), s(golden.templates));
  // no receipt covenant (the stops arm from a plain fill in the same transaction) and no cross limit (pair orders replace it)
  assert.equal(kob.receiptCovenantId, undefined);
  assert.ok(!JSON.parse(kob.templates()).some((t) => /Receipt|Cross/.test(t.name)));
});

test('touch evidence of a batch leg', () => {
  const v = golden.transactions.find((t) => t.name === 'cond.ask.stop.trigger');
  const b = v.request;
  const k = b.legs[0].evidence;
  assert.equal(b.legs[k].kind, 'ask');
  const t = JSON.parse(kob.touchOf(s(b.legs[k])));
  assert.equal(t.side, 1);
  assert.ok(BigInt(t.price) <= BigInt(b.legs[0].order.state.stopPrice));
  assert.equal(t.amount, b.legs[k].amount);
  assert.equal(t.scale, b.legs[0].order.state.scale);
  assert.equal(typeof t.price, 'string');
  assert.ok(BigInt(t.amount) >= BigInt(b.legs[0].order.state.minTouch));
  assert.throws(() => kob.touchOf(s(b.legs[0])), /plain KobAsk \/ KobBid/);
  const u = golden.transactions.find((t) => t.name === 'cond.ask.update.arm').request;
  assert.equal(u.updates.length, 1);
  assert.equal(JSON.parse(kob.touchOf(s(u.legs[u.updates[0].evidence]))).side, 1);
});

test('every builder reproduces its golden vector', () => {
  assert.ok(golden.transactions.length >= 250);
  for (const v of golden.transactions) {
    const built = kob.build(s(v.request));
    assert.equal(built, s(v.built), `${v.name}: built`);
    const signed = kob.finalize(built, s(v.signatures), s(v.finalize));
    assert.equal(signed, s(v.signed), `${v.name}: signed`);
    const report = JSON.parse(kob.validate(signed));
    assert.ok(BigInt(report.fee) >= BigInt(report.minFee), `${v.name}: fee`);
  }
});

test('state and payload codecs', () => {
  for (const st of golden.states) {
    assert.equal(kob.encodeState(s(st.state)), st.encoded, st.name);
    assert.equal(kob.decodeState(st.state.kind, st.encoded), s(st.state), st.name);
  }
  for (const p of golden.payloads) {
    assert.equal(kob.decodePayload(p.payload), s(p.decoded), p.name);
    if (!p.decoded.legacy) assert.equal(kob.encodePayload(s(p.decoded.records)), p.payload, p.name);
  }
});

test('order recovery from a creation transaction (placement record incl. a day-order deadline)', () => {
  for (const name of ['create.ask', 'create.ask.day', 'create.ifdAsk.repeat', 'kron.create.ask', 'kron.create.ifdAsk.repeat', 'kron2732.create.bid']) {
    const v = golden.transactions.find((t) => t.name === name);
    const rec = JSON.parse(kob.recoverOrders(s(v.signed.tx)));
    assert.equal(rec.length, 1, name);
    assert.deepEqual(rec[0].order, v.request.order, name);
    assert.equal(rec[0].deadline, v.request.deadline ?? null, name);
  }
});

test('wallet defaults: keeper tips per token program and day orders at 00:00 UTC', () => {
  assert.equal(kob.keeperTips(), s(golden.keeperTips));
  assert.equal(kob.pairKeeperTips(), s(golden.pairKeeperTips));
  for (const d of golden.dayOrders) {
    assert.equal(kob.dayOrder(d.d0, d.t0, d.rateMilli ?? ''), s(d.result));
  }
  assert.deepEqual(JSON.parse(kob.mutableWindows('KobAsk')), [['amountLeft', 236, 244]]);
  assert.deepEqual(JSON.parse(kob.mutableWindows('KobIfdBid')), [['amountLeft', 161, 169], ['armed', 287, 295], ['rptAmount', 296, 304]]);
});

test('KRON family: kinds, token programs, token states, placement record', () => {
  const kron = golden.transactions.filter((t) => t.name.startsWith('kron'));
  assert.ok(kron.length >= 100);
  // Every family-generic shape exists for KRON and reproduces (checked above with all vectors).
  for (const name of ['kron.create.ask', 'kron.take.bid.partial', 'kron.rpt.ask.merge.partial', 'kron.cond.ask.stop.trigger', 'kron.cond.bid.update.arm', 'kron2732.cancel.ask']) {
    assert.ok(kron.some((t) => t.name === name), name);
  }
  // The templates list carries the six KRON kinds (the pair kinds are one template each for both families) and the two raw KRON programs.
  const tpls = JSON.parse(kob.templates());
  for (const n of ['KobAskKron', 'KobBidKron', 'KobCondAskKron', 'KobCondBidKron', 'KobIfdBidKron', 'KobIfdAskKron']) {
    assert.ok(tpls.some((t) => t.name === n && t.kindCode !== null), n);
  }
  const prog = tpls.find((t) => t.name === 'KronToken2433');
  assert.deepEqual([prog.prefixLen, prog.stateLen, prog.suffixLen, prog.tokenSlots], [0, 46, 2387, [4, 5]]);
  assert.equal(prog.hash, '2ed46a7edf5b168e67dba56998c58255235bebac436940a85115ca31d5c559f2');
  // KRON token state (46 bytes) and its P2SH under the program; the KCC-20 codec is untouched.
  const st = { owner: 'ab'.repeat(32), id_type: 3, amount: '5', is_minter: 0 };
  const hex = kob.encodeTokenState(s(st));
  assert.equal(hex.length, 92);
  assert.deepEqual(JSON.parse(kob.decodeTokenState(hex)), st);
  assert.match(kob.tokenScriptPublicKey('KronToken2433', s(st)), /^0000aa20[0-9a-f]{64}87$/);
  assert.throws(() => kob.tokenScriptPublicKey('KCC20Ref', s(st)));
  // The mutable windows of the KRON kinds sit 33 bytes earlier on the bid side.
  assert.deepEqual(JSON.parse(kob.mutableWindows('KobCondBidKron')), [['stopPrice', 191, 199], ['amountLeft', 254, 262], ['armed', 263, 271]]);
  assert.deepEqual(JSON.parse(kob.mutableWindows('KobCondBid')), [['stopPrice', 224, 232], ['amountLeft', 287, 295], ['armed', 296, 304]]);
  // The KRON placement record: family byte 0x02, 2-byte custody part.
  const c = golden.transactions.find((t) => t.name === 'kron.create.ask');
  const rec = JSON.parse(kob.decodePayload(c.signed.tx.payload));
  assert.equal(rec.records[0].family, 2);
  assert.equal(rec.records[0].template, 'KobAskKron');
  // A KRON bid cannot carry an extension commitment.
  const bid = golden.transactions.find((t) => t.name === 'kron.create.bid').request.order;
  assert.throws(() => kob.encodeState(s({ kind: bid.kind, state: { ...bid.state, extensionCommitment: 'ee'.repeat(32) } })), /extension commitment/);
});

test('pair orders: three kinds for both families and sides, windows, placement records, custodies', () => {
  const pair = golden.transactions.filter((t) => t.name.startsWith('pair.'));
  // every program-pair family (KCC-20 / KRON for A and B) and every shape family: create, cancel, refund, fills, netting, conditionals in
  // both evidence modes, updates, if-done fills, bookings and repeat merges (all reproduce byte for byte above)
  for (const fam of ['', 'kcc20-kron.', 'kron-kcc20.', 'kron-kron.']) {
    for (const shape of ['create.ask', 'create.bid', 'create.condAsk', 'create.condBid', 'create.ifdBid', 'create.ifdAsk', 'cancel.ifdAsk', 'refund.ifdAsk',
      'ask.rest', 'bid.ioc', 'ask.decay', 'bid.rising', 'ask.twap.tip', 'net.1x1', 'cond.ask.stop.ev0', 'cond.bid.stop.ev1', 'update.ask.trail.ev0',
      'ifd.bid.book', 'ifd.ask.stop.ev0', 'rearm.bid.new', 'rearm.ask.sellout']) {
      assert.ok(pair.some((t) => t.name === `pair.${fam}${shape}`), `pair.${fam}${shape}`);
    }
  }
  // ONE template per kind for both sides and both families; KobCross is retired
  const tpls = JSON.parse(kob.templates());
  for (const [name, code, len] of [['KobPair', 8, 414], ['KobCondPair', 9, 510], ['KobIfdPair', 10, 909]]) {
    const t = tpls.find((x) => x.name === name);
    assert.ok(t && t.kindCode === code && t.stateLen === len, name);
  }
  assert.ok(!tpls.some((t) => t.name === 'KobCross'));
  assert.deepEqual(JSON.parse(kob.mutableWindows('KobPair')).map((w) => w[0]), ['amountLeft', 'custody']);
  assert.deepEqual(JSON.parse(kob.mutableWindows('KobCondPair')).map((w) => w[0]), ['stopPrice', 'armed', 'amountLeft', 'custody']);
  assert.deepEqual(JSON.parse(kob.mutableWindows('KobIfdPair')).map((w) => w[0]), ['armed', 'amountLeft', 'custody', 'rptAmount']);
  // the record's family byte is the family of base token A; the custody parts follow custodies() (a sell-first entry: A then its B prefund)
  for (const [name, family, kind, custodies] of [
    ['pair.create.ask', 1, 'KobPair', 1], ['pair.kron-kcc20.create.bid', 2, 'KobPair', 1], ['pair.kcc20-kron.create.condBid', 1, 'KobCondPair', 1],
    ['pair.kron-kron.create.ifdBid', 2, 'KobIfdPair', 1], ['pair.create.ifdAsk', 1, 'KobIfdPair', 2], ['pair.kcc20-kron.create.ifdAsk', 1, 'KobIfdPair', 2],
  ]) {
    const c = golden.transactions.find((t) => t.name === name);
    const rec = JSON.parse(kob.decodePayload(c.signed.tx.payload)).records[0];
    assert.deepEqual([rec.family, rec.template], [family, kind], name);
    const [o] = JSON.parse(kob.recoverOrders(s(c.signed.tx)));
    assert.equal(o.order.kind, kind, name);
    const cs = JSON.parse(kob.custodies(s(o.order)));
    assert.equal(cs.length, custodies, name);
    assert.ok(o.custody, name);
    if (custodies === 2) assert.equal(o.prefund.state.amount, cs[1].amount, `${name}: the prefund part`);
    kob.checkNewOrder(s(c.request.order));
    assert.ok(BigInt(c.request.value) >= BigInt(kob.minOrderValue(s(c.request.order))), name);
  }
  // A KRON token has no extension commitment; family codes are 1 or 2; A != B.
  const x = golden.transactions.find((t) => t.name === 'pair.kcc20-kron.create.ask').request.order;
  assert.throws(() => kob.encodeState(s({ kind: x.kind, state: { ...x.state, tExt: 'ee'.repeat(32) } })), /extension commitment/);
  assert.throws(() => kob.encodeState(s({ kind: x.kind, state: { ...x.state, tFamily: '3' } })), /family/);
  assert.throws(() => kob.encodeState(s({ kind: x.kind, state: { ...x.state, tCovId: x.state.sCovId } })), /A != B/);
  // Pair keeper tips per program pair; tipsFor picks the table by kind.
  assert.equal(kob.pairKeeperTips(), s(golden.pairKeeperTips));
  for (const name of ['pair.create.ask', 'pair.kron-kron.create.ifdAsk', 'create.ask']) {
    const o = golden.transactions.find((t) => t.name === name).request.order;
    const tips = JSON.parse(kob.tipsFor(s(o)));
    if (o.kind.startsWith('KobPair') || o.kind.startsWith('KobIfdPair')) {
      const p = JSON.parse(kob.pairPrograms(s(o)));
      assert.deepEqual(tips, golden.pairKeeperTips[`${p.a.program}+${p.b.program}`], name);
      assert.equal(kob.pairTips(p.a.program, p.b.program), s(tips));
    } else {
      assert.ok(Object.values(golden.keeperTips).some((t) => s(t) === s(tips)), name);
    }
  }
  // Pair conditionals arm from evidence in mode 0 (two KAS-book legs) or mode 1 (a pair leg); the role names say which.
  for (const [name, mode] of [['pair.cond.ask.stop.ev0', 0], ['pair.cond.ask.stop.ev1', 1], ['pair.update.ask.arm.ev0', 0], ['pair.update.ifdBid.arm.ev1', 1]]) {
    const v = golden.transactions.find((t) => t.name === name);
    assert.ok(v.built.roles.some((r) => r.includes(`.ev${mode}`) || r.includes(`.arm${mode}`)), `${name}: ${v.built.roles.join(' ')}`);
  }
});

test('errors surface as exceptions', () => {
  const v = golden.transactions.find((t) => t.name === 'cancel.ask');
  const sigs = structuredClone(v.signatures);
  sigs[0].signature = '00'.repeat(64);
  assert.throws(() => kob.finalize(s(v.built), s(sigs), '{}'), /signature/);
  assert.throws(() => kob.build('{"action":"batch"}'));
  assert.equal(typeof kob.budgetFor('p2pk'), 'number');
});

test('token issuance plans a genesis as a BuiltTx and rejects rule violations', () => {
  const limits = JSON.parse(kob.issueLimits());
  assert.equal(limits.program, 'KCC20Ref_8x8');
  assert.equal(limits.maxTokenOutputs, 8);
  const key = golden.transactions.find((t) => t.name === 'create.ask').request.funding[0];
  const spec = {
    name: 'Node Token',
    ticker: 'NODE',
    decimals: 8,
    supply: '1000',
    holders: [{ owner: key.pubkey, ownerScheme: 0, amount: '600' }, { owner: 'c4'.repeat(32), ownerScheme: 4, amount: '400' }],
    funding: [{ transactionId: key.transactionId, index: key.index, amount: '50000000000', pubkey: key.pubkey }],
  };
  const res = JSON.parse(kob.issue(s(spec)));
  assert.equal(res.token.outputs.length, 2);
  assert.equal(res.built.tx.outputs.length, 3);
  assert.equal(res.built.sign.length, 1);
  assert.equal(res.built.covenants[0].covenantId, res.token.covenantId);
  assert.equal(res.docs.registryEntry.status, 'pending-review');
  assert.equal(kob.issue(s(spec)), s(res), 'deterministic');
  assert.throws(() => kob.issue(s({ ...spec, supply: '999' })), /supply mismatch/);
  assert.throws(() => kob.issue(s({ ...spec, ticker: 'node' })), /ticker/);
});
