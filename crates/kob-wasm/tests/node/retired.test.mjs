// Retired templates in the wasm build: the v2 / v3 payload records of the protocol v2.6 lot templates still decode, their
// orders read as legacy states, and the maker can still cancel them (spend-only).
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { createRequire } from 'node:module';
import { fileURLToPath } from 'node:url';
import { dirname, join } from 'node:path';

const here = dirname(fileURLToPath(import.meta.url));
const require = createRequire(import.meta.url);
const kob = require(join(here, '../../pkg-node/kob_wasm.js'));
const vectors = join(here, '../../../kob-protocol/vectors');
const golden = JSON.parse(readFileSync(join(vectors, 'golden.json'), 'utf8'));
const v2 = JSON.parse(readFileSync(join(vectors, 'payload_v2.json'), 'utf8'));
const v3 = JSON.parse(readFileSync(join(vectors, 'payload_v3.json'), 'utf8'));
const s = (v) => JSON.stringify(v);

test('the retired templates are listed with their family and kind name', () => {
  const list = JSON.parse(kob.retiredTemplates());
  assert.ok(list.length >= 40, `${list.length}`);
  for (const r of list) {
    assert.match(r.hash, /^[0-9a-f]{64}$/);
    assert.ok([1, 2].includes(r.family) && r.stateLen > 0 && r.note.length > 0 && r.kind && r.kindName, s(r));
  }
  // the one KobCross template of today has two retired predecessors: KobCross and KobCrossKron
  assert.ok(list.some((r) => r.kindName === 'KobCrossKron' && r.family === 2 && r.kind === 'KobCross'));
  assert.ok(list.some((r) => r.kindName === 'KobCross' && r.family === 1));
  // none is a pinned template of today
  const pinned = new Set(JSON.parse(kob.templates()).map((t) => t.hash));
  assert.ok(list.every((r) => !pinned.has(r.hash)));
});

test('payload versions 2 and 3 decode into retired records; the encoder refuses them', () => {
  for (const v of [v2, v3]) {
    assert.ok(v.payloads.length >= 40);
    for (const p of v.payloads) {
      assert.equal(kob.decodePayload(p.payload), s(p.decoded), p.name);
    }
  }
  const rec = JSON.parse(kob.decodePayload(v3.payloads.find((p) => p.name === 'create.ask').payload)).records[0];
  assert.equal(rec.type, 'retiredOrder');
  assert.throws(() => kob.encodePayload(s([rec])), /retired|encode|cannot/i);
  // today's payload is version 4
  assert.equal(JSON.parse(kob.decodePayload(golden.payloads.find((p) => p.name === 'create.ask').payload)).version, 4);
});

test('a retired order reads as its legacy state and has its own script', () => {
  const list = JSON.parse(kob.retiredTemplates());
  for (const name of ['create.ask', 'create.bid', 'create.condAsk', 'create.ifdBid.repeat', 'kron.create.ask', 'cross.create', 'cross.kron-kcc20.create']) {
    const rec = JSON.parse(kob.decodePayload(v3.payloads.find((p) => p.name === name).payload)).records[0];
    assert.ok(list.some((r) => r.hash === rec.templateHash), name);
    const st = JSON.parse(kob.decodeRetired(rec.templateHash, rec.state));
    assert.equal(st.kind, rec.template.replace(/Kron$/, ''), name);
    // the lot layout: the legacy field names
    assert.ok(Object.keys(st.state).some((k) => /^lot|Lot/.test(k)) || name.startsWith('create.bid'), `${name}: ${Object.keys(st.state)}`);
    assert.match(kob.retiredScriptPublicKey(rec.templateHash, rec.state), /^0000aa20[0-9a-f]{64}87$/, name);
    // a state span of the wrong length is refused
    assert.throws(() => kob.retiredScriptPublicKey(rec.templateHash, rec.state.slice(2)), /bytes/);
  }
  assert.throws(() => kob.decodeRetired('00'.repeat(32), ''), /not a retired template/);
  assert.throws(() => kob.retiredScriptPublicKey('00'.repeat(32), ''), /not a retired template/);
});

test('the maker cancels an order of a retired template', () => {
  const base = golden.transactions.find((t) => t.name === 'cancel.ask').request;
  const rec = JSON.parse(kob.decodePayload(v3.payloads.find((p) => p.name === 'create.ask').payload)).records[0];
  const lot = JSON.parse(kob.decodeRetired(rec.templateHash, rec.state)).state;
  const amount = BigInt(lot.lotsLeft) * BigInt(lot.lotUnits) * BigInt(lot.unit);
  const { state: _o, ...order } = base.order;
  const { state: _c, ...custodyUtxo } = base.custody;
  const funding = golden.transactions.find((t) => t.name === 'create.ask').request.funding;
  const request = {
    templateHash: rec.templateHash,
    state: rec.state,
    order,
    custody: { ...custodyUtxo, state: { ...base.custody.state, amount: String(amount) } },
    strays: [],
    funding,
    change: null,
    fee: base.fee,
  };
  const built = JSON.parse(kob.buildCancelRetired(s(request)));
  assert.equal(built.plans[0].kind, 'retired');
  assert.equal(built.plans[0].templateHash, rec.templateHash);
  assert.ok(built.tx.outputs.length >= 2, 'the custody tokens and the carrier go back to the maker');
  // with ownKeys: the maker (and the change key) must be the wallet's
  const mine = kob.buildCancelRetired(s({ ...request, ownKeys: [lot.maker] }));
  assert.equal(JSON.parse(mine).tx.outputs.length, built.tx.outputs.length);
  assert.throws(() => kob.buildCancelRetired(s({ ...request, ownKeys: ['ab'.repeat(32)] })), /not one of the wallet/);
  assert.throws(() => kob.buildCancelRetired(s({ ...request, templateHash: '00'.repeat(32) })), /retired template/);
});

test('the retired v3 cross limit (no lots) reads in its own layout', () => {
  const j = JSON.parse(readFileSync(join(here, '../../../../contracts/retired/KobCross-ea23f1fe.json'), 'utf8'));
  const c = j.contracts[0] ?? Object.values(j.contracts)[0];
  const hex = typeof c.compiled.bytecode === 'string' ? c.compiled.bytecode : Buffer.from(c.compiled.bytecode).toString('hex');
  const { offset, len } = c.compiled.state_span;
  const span = hex.slice(offset * 2, (offset + len) * 2);
  const list = JSON.parse(kob.retiredTemplates());
  const r = list.find((x) => x.hash.startsWith('ea23f1fe'));
  assert.ok(r && r.kind === 'KobCross' && r.stateLen === 360, s(r));
  const st = JSON.parse(kob.decodeRetired(r.hash, span));
  assert.equal(st.kind, 'KobCross');
  for (const k of ['amountLeft', 'aFamily', 'bFamily', 'scale', 'price', 'minFill']) assert.ok(k in st.state, k);
  assert.match(kob.retiredScriptPublicKey(r.hash, span), /^0000aa20[0-9a-f]{64}87$/);
});
