// Intent mode and invoices over the REAL kob-wasm: the TS intent offer equals the Rust builder's, the payer builds and
// signs one intent creation through the Rust SDK, the Rust verifier (preflight) accepts it with the payer's worst case,
// the payer's cancel is built and signed, and the TS invoice id equals the Rust one.

import { test } from 'node:test';
import assert from 'node:assert/strict';
import { buildOffer } from '../src/offers.ts';
import { rustOffer } from '../src/wasm-node.ts';
import { canonicalJson } from '../src/canonical.ts';
import { cancelIntent, expireIntent, payIntent } from '../src/intent.ts';
import { invoiceId, newInvoice } from '../src/invoice.ts';
import { KobX402Error } from '../src/errors.ts';
import type { PaymentRequirements } from '../src/types.ts';
import { loadGolden, realWasm, skipUnlessWasm } from './helpers/real-wasm.ts';

const NOW_MS = 1_800_000_000_000;
const opts = { skip: skipUnlessWasm };
/** KCC20Ref_8x8, the program KOB issues (the router reads any KCC-20 program the intent names). */
const P8 = '40fef59a59bd76991f4d4e2101d1e3e34860997b89fe7714532637cec482a9d7';
const TOKEN = '70'.repeat(32);
const EXT = 'ee'.repeat(32);

function fixture() {
  const wasm = realWasm()!;
  const native = loadGolden().find((c) => c.name === 'pay.native')!.request;
  const payTo: string = loadGolden().find((c) => c.name === 'offer.native')!.request.payTo;
  const payerSpk: string = native.utxos[0].scriptPublicKey;
  const payerPk = payerSpk.slice(6, 70);
  const tokenUtxo = {
    transactionId: '15'.repeat(32),
    index: 21,
    amount: '100000000',
    blockDaaScore: '1000',
    covenantId: TOKEN,
    state: { amount: '9000', owner: payerPk, owner_scheme: 0, borrow_scheme: 0, borrow_guard: '00'.repeat(32), extension_commitment: EXT },
  };
  const spec = { kind: 'swap' as const, mode: 'intent' as const, receive: 'kas' as const, amount: '200000000', payAssets: [{ asset: TOKEN, templateHash: P8, extensionCommitment: EXT }] };
  const offer = buildOffer(spec, { wasm, network: 'kaspa:testnet-10', payTo, maxTimeoutSeconds: 600, finality: 'accepted' });
  return { wasm, native, payTo, offer, tokenUtxo, tokens: [{ covenantId: TOKEN, templateHash: P8, extensionCommitment: EXT, custody: 'unconditional' as const }] };
}

test('the TS intent offer is the Rust builder output', opts, () => {
  const { wasm, payTo, offer } = fixture();
  const rust = rustOffer(wasm.raw, 'intent', {
    network: 'kaspa:testnet-10',
    amount: '200000000',
    payTo,
    maxTimeoutSeconds: 600,
    finality: 'accepted',
    receive: 'kas',
    payAssets: [{ covenantId: TOKEN, templateHash: P8, extensionCommitment: EXT }],
  });
  assert.equal(canonicalJson(offer), canonicalJson(rust));
  assert.equal(offer.extra.route?.binding, 'kob-intent-v1');
});

test('one signature creates an intent the Rust verifier accepts; the payer can cancel it', opts, () => {
  const { wasm, native, offer, tokenUtxo, tokens } = fixture();
  const rh = 'c1'.repeat(32);
  const base = {
    requirements: offer,
    requestHash: rh,
    payAsset: TOKEN,
    privateKeys: [native.secretKey],
    utxos: native.utxos,
    tokenUtxos: [tokenUtxo],
    nowMs: NOW_MS,
    tokens,
    paymentId: 'intent-wasm-test-0001',
  };
  const p = payIntent(wasm, { ...base, options: { maxSell: '3000' } });
  assert.equal(p.paymentPayload.payload.route?.binding, 'kob-intent-v1');
  assert.equal(p.paymentPayload.payload.route?.intent?.actor, 'TokenToKas_sell');
  assert.equal(p.payerSpent, '3000', 'the worst case: 3000 units of the pay token');
  assert.equal(p.intent.actor, 'TokenToKas_sell');
  const v = wasm.preflight({ requirements: offer, paymentPayload: p.paymentPayload, requestHash: rh, nowMs: NOW_MS + 500, tokens });
  assert.equal(v.ok, true, JSON.stringify(v));
  assert.equal(v.transactionId, p.transactionId);
  assert.equal(v.payerSpent, '3000');
  // the payer's own bound is checked by preflight
  const over = wasm.preflight({ requirements: offer, paymentPayload: p.paymentPayload, requestHash: rh, nowMs: NOW_MS + 500, tokens, maxPay: '2999', maxPayAsset: TOKEN });
  assert.equal(over.ok, false);
  assert.equal(over.diagnostic, 'overpayment');
  // a bound counted in KAS (the default) does not bound a token-paid intent: refused
  const kasBound = wasm.preflight({ requirements: offer, paymentPayload: p.paymentPayload, requestHash: rh, nowMs: NOW_MS + 500, tokens, maxPay: '1000000' });
  assert.equal(kasBound.ok, false);
  assert.equal(kasBound.diagnostic, 'pay_asset_not_accepted');
  // presented for another request: refused
  const other = wasm.preflight({ requirements: offer, paymentPayload: p.paymentPayload, requestHash: 'd2'.repeat(32), nowMs: NOW_MS + 500, tokens });
  assert.equal(other.ok, false);
  // a cancel signed locally, and a wallet cancel to sign
  const c = cancelIntent(wasm, { intent: p.intent, privateKeys: [native.secretKey] }) as { transactionId: string };
  assert.match(c.transactionId, /^[0-9a-f]{64}$/);
  const w = cancelIntent(wasm, { intent: p.intent }) as { built: { sign: { inputIndex: number }[] } };
  assert.deepEqual(w.built.sign.map((s) => s.inputIndex), [0], 'the payer signs the intent input only');
  // anyone's expiry (no signature), from the intent's deadline on (C5 X-10: no facilitator needed)
  const x = expireIntent(wasm, { intent: p.intent });
  assert.match(x.transactionId, /^[0-9a-f]{64}$/);
  assert.equal(x.lockTime, String(NOW_MS + 300_000), 'its lock time is the deadline (the authorization expiry)');
  // the KAS pay asset is not accepted by this offer
  assert.throws(() => payIntent(wasm, { ...base, payAsset: 'KAS', options: { maxPay: '1' } }), (e: unknown) => e instanceof KobX402Error && e.diagnostic === 'pay_asset_not_accepted');
});

test('the TS invoice id is the Rust invoice id; checkInvoice validates a fetched invoice', opts, () => {
  const { wasm, offer } = fixture();
  const kas: PaymentRequirements = loadGolden().find((c) => c.name === 'offer.native')!.expected;
  const inv = newInvoice({ network: 'kaspa:testnet-10', reference: 'order-77', expiresAtMs: NOW_MS + 600_000, memo: 'café', accepts: [kas, offer] });
  const id = invoiceId(inv);
  assert.equal(wasm.invoiceId!(inv), id);
  const c = wasm.checkInvoice!(inv, id, NOW_MS);
  assert.equal(c.expiresAtMs, NOW_MS + 600_000);
  assert.equal(c.kaspaUri, `${kas.payTo}?amount=${Number(kas.amount) / 1e8}`);
  assert.throws(() => wasm.checkInvoice!(inv, 'ab'.repeat(32), NOW_MS));
  assert.throws(() => wasm.checkInvoice!(inv, id, NOW_MS + 600_000), (e: unknown) => e instanceof KobX402Error && e.diagnostic === 'invoice_expired');
});
