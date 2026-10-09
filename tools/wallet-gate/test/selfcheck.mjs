// OFFLINE self-check (no network, no wallet): verifies encoders against kaspa-wasm's ScriptBuilder,
// contract templates, tx shapes, fee formula and sigscript/signature placement using a local key.
//   node test/selfcheck.mjs
import { readFileSync, mkdirSync, writeFileSync } from 'node:fs';
import { join } from 'node:path';
import assert from 'node:assert/strict';
import k, { ROOT } from '../lib/node-kaspa.mjs';
import * as S from '../lib/script.mjs';
import * as C from '../lib/contracts.mjs';
import * as F from '../lib/flows.mjs';

const kcc = JSON.parse(readFileSync(join(ROOT, 'artifacts', 'KCC20Ref.json'), 'utf8'));
const bidT = JSON.parse(readFileSync(join(ROOT, 'artifacts', 'BidOrder.template.json'), 'utf8'));
let n = 0;
const ok = (name, fn) => { fn(); n++; console.log('  ok', name); };

ok('push encoders == kaspa-wasm ScriptBuilder', () => {
  const rnd = (len) => Uint8Array.from({ length: len }, () => Math.floor(Math.random() * 256));
  for (const len of [0, 1, 1, 1, 1, 2, 8, 32, 65, 66, 75, 76, 255, 256, 520]) {
    const d = rnd(len);
    if (len === 1) d[0] = [0, 1, 5, 16, 17, 0x81, 0x80, 0xff][Math.floor(Math.random() * 8)];
    const sb = new k.ScriptBuilder(); sb.addData(d);
    assert.equal(sb.drain(), S.hex(S.pushData(d)));
  }
  for (const v of [0n, 1n, -1n, 16n, 17n, 127n, 128n, 255n, 256n, -128n, 1000n, 250000000n, 2n ** 62n, -(2n ** 62n)]) {
    const sb = new k.ScriptBuilder(); sb.addI64(v);
    assert.equal(sb.drain(), S.hex(S.pushI64(v)));
  }
});

const abiK = C.loadToken(kcc);
ok('KCC-20 state encoder reproduces the compiled artifact byte-for-byte', () => {
  const st = C.kcc20State({ amount: 1000, owner: '03'.repeat(32), ownerScheme: 4, ext: C.EXT_HEX });
  assert.ok(S.eq(C.kcc20Redeem(abiK, st), abiK.bytecode));
  assert.equal(abiK.encodeState(st).length, 112);
});

const walletKey = new k.PrivateKey('a1'.repeat(32));
const W = walletKey.toPublicKey().toXOnlyPublicKey().toString();
const walletAddr = C.addressOfPubkey(k, W);
assert.equal(walletAddr, walletKey.toAddress('testnet').toString());
assert.equal(C.pubkeyOfAddress(k, walletAddr), W);
const RECIP = '5b'.repeat(32);
const TOKCOV = 'c0'.repeat(32);

ok('BidOrder patching: maker + tokenCovId land in the redeem script', () => {
  const b = C.bidRedeem(bidT, { maker: W, tokenCovId: TOKCOV });
  assert.equal(b.redeem.length, 821);
  const h = S.hex(b.redeem);
  assert.ok(h.includes(W) && h.includes(TOKCOV));
  assert.ok(!h.includes(C.SENTINEL_MAKER) && !h.includes(C.SENTINEL_TOKCOV));
});

// fake setup + UTXOs
const setup = { walletPubkey: W, walletAddress: walletAddr, tokenCovId: TOKCOV, extHex: C.EXT_HEX, tokenUnits: '1000', tokens: [], bids: [] };
const bid = C.bidRedeem(bidT, { maker: W, tokenCovId: TOKCOV });
const tokRedeem = C.kcc20Redeem(abiK, C.kcc20State({ amount: 1000, owner: W, ext: C.EXT_HEX }));
const spkOf = (r) => C.p2shSpk(k, r);
setup.tokens = [{ address: k.addressFromScriptPublicKey(spkOf(tokRedeem), 'testnet-10').toString() }];
setup.bids = [{ address: k.addressFromScriptPublicKey(spkOf(bid.redeem), 'testnet-10').toString() }];
const ctx = F.deriveContext(k, setup, kcc, bidT);
ok('derived P2SH addresses match setup addresses', () => assert.deepEqual(ctx.checks, { tokenAddressMatches: true, bidAddressMatches: true }));

const utxo = (spk, amount, cov, i) => ({ outpoint: { transactionId: String(i).repeat(64).slice(0, 64), index: i }, amount, scriptPublicKey: { script: spk }, address: undefined, covenantId: cov, blockDaaScore: 1n, isCoinbase: false });
const results = {};
const tests = [
  F.buildT1(k, ctx, utxo(C.p2pkScriptHex(W), 500_000_000n, undefined, 1)),
  F.buildT2(k, ctx, setup.bids[0], utxo(spkOf(bid.redeem).script, 300_000_000n, 'd1'.repeat(32), 2)),
  F.buildT3(k, ctx, utxo(spkOf(tokRedeem).script, 200_000_000n, TOKCOV, 3), RECIP),
];
for (const t of tests) {
  ok(`${t.id} ${t.name}: local signature, sigscript layout, tx round-trip`, () => {
    const sig65 = F.signLocal(k, t, walletKey);
    assert.equal(sig65.length, 65);
    assert.equal(sig65[64], 1, 'sighash byte = SIGHASH_ALL');
    const { tx, sigscript } = F.finalize(t, sig65);
    const pushes = S.parsePushes(S.unhex(sigscript));
    if (t.id === 'T1') assert.deepEqual(pushes.map((p) => p.length), [65]);
    if (t.id === 'T2') {
      assert.deepEqual(pushes.map((p) => p.length), [65, 4, 821]);
      assert.equal(S.hex(pushes[1]), 'a0893109');
      assert.ok(S.eq(pushes[0], sig65) && S.eq(pushes[2], bid.redeem));
    }
    if (t.id === 'T3') {
      // amounts[8], owners[32], owner_scheme[1] (0x01 0x00), borrow_scheme[1], guard[32], ext[32], witness[66], tag[4], redeem[2915]
      assert.deepEqual(pushes.map((p) => p.length), [8, 32, 1, 1, 32, 32, 66, 4, 2915]);
      assert.equal(pushes[6][0], 0x00);
      assert.ok(S.eq(pushes[6].slice(1), sig65));
      assert.equal(S.hex(pushes[7]), '79c71c23');
      assert.equal(S.hex(pushes[1]), RECIP);
    }
    const json = tx.serializeToSafeJSON();
    const back = k.Transaction.deserializeFromSafeJSON(json);
    assert.equal(back.version, 1);
    assert.equal(back.inputs[0].computeBudget, t.plan.inputs[0].budget);
    assert.equal(back.inputs[0].signatureScript, sigscript);
    const rec = F.sigFromSignedTx(json);
    assert.ok(S.eq(rec.sig65, sig65));
    results[t.id] = { sigscriptBytes: sigscript.length / 2, fee: t.fee.toString(), txid: tx.id };
  });
}
ok('signature commits to the outputs (changing an output changes the signature input)', () => {
  const a = F.buildT1(k, ctx, utxo(C.p2pkScriptHex(W), 500_000_000n, undefined, 1));
  const b = F.buildT1(k, ctx, utxo(C.p2pkScriptHex(W), 500_000_001n, undefined, 1));
  assert.notEqual(S.hex(F.signLocal(k, a, walletKey)), S.hex(F.signLocal(k, b, walletKey)));
});
ok('T3 output is a covenant output bound to the input covenant id, owner = recipient', () => {
  const t = tests[2];
  const o = t.tx.outputs[0];
  assert.equal(o.covenant.covenantId.toString(), TOKCOV);
  assert.equal(o.covenant.authorizingInput, 0);
  const expect = C.kcc20Redeem(abiK, C.kcc20State({ amount: 1000, owner: RECIP, ext: C.EXT_HEX }));
  assert.equal(o.scriptPublicKey.script, spkOf(expect).script);
});
ok('fee formula sane (100 sompi * max(compute, 2*size))', () => {
  for (const t of tests) assert.ok(t.fee > 100_000n && t.fee < 20_000_000n, t.id + ' fee ' + t.fee);
});
ok('Kaspire ordered-args template == our sigscript when assembled wallet-style', () => {
  const t = tests[2];
  const a = t.kaspire.signatureScript.args;
  assert.equal(a.length, 6 + 2);
  assert.equal(a[6].type, 'signature');
  assert.equal(a[6].prefixHex, '00');
  assert.deepEqual(tests[1].kaspire.signatureScript.args.map((x) => x.type), ['signature', 'data']);
  for (const tt of [tests[1], tests[2]]) {
    const sig = F.signLocal(k, tt, walletKey);
    const parts = tt.kaspire.signatureScript.args.map((x) => x.type === 'signature' ? S.pushData(S.concat(S.unhex(x.prefixHex), sig)) : S.pushData(S.unhex(x.hex)));
    const emu = S.hex(S.concat(...parts, S.pushData(tt.redeem)));
    assert.equal(emu, tt.assemble(sig), tt.id + ': wallet-style ordered-args assembly must equal our assembly');
  }
});

mkdirSync(join(ROOT, 'out'), { recursive: true });
writeFileSync(join(ROOT, 'out', 'selfcheck.json'), JSON.stringify(results, null, 2));
console.log(`\nselfcheck: ${n} checks passed`, results);
