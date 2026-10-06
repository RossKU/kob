import { test } from 'node:test';
import assert from 'node:assert/strict';
import {
  decodeHeader,
  decodePaymentRequired,
  decodePaymentResponse,
  decodePaymentSignature,
  encodePaymentRequired,
  encodePaymentResponse,
  encodePaymentSignature,
} from '../src/headers.ts';
import { KobX402Error } from '../src/errors.ts';
import { loadVector } from './helpers/env.ts';

const v = loadVector('x402-http/exact-transaction.json');

test('rc.1 http vector: PAYMENT-REQUIRED / PAYMENT-SIGNATURE / PAYMENT-RESPONSE re-encode byte for byte', () => {
  assert.equal(encodePaymentRequired(v.paymentRequired), v.headers.paymentRequired);
  assert.equal(encodePaymentSignature(v.paymentPayload), v.headers.paymentSignature);
  assert.equal(encodePaymentResponse(v.settlementResponse), v.headers.paymentResponse);
});

test('rc.1 http vector: headers decode to the vector objects', () => {
  assert.deepEqual(decodePaymentRequired(v.headers.paymentRequired), v.paymentRequired);
  assert.deepEqual(decodePaymentSignature(v.headers.paymentSignature), v.paymentPayload);
  assert.deepEqual(decodePaymentResponse(v.headers.paymentResponse), v.settlementResponse);
});

test('malformed headers are rejected', () => {
  for (const bad of ['', '   ', 'not base64!!', 'abc', Buffer.from('not json').toString('base64'), Buffer.from([0xff, 0xfe, 0xfd, 0xfc]).toString('base64')]) {
    assert.throws(() => decodeHeader(bad), (e: unknown) => e instanceof KobX402Error && e.code === 'invalid_header', bad);
  }
  assert.throws(() => decodeHeader('QUJD'.repeat(100), 100), KobX402Error);
});
