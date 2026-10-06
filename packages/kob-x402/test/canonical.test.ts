import { test } from 'node:test';
import assert from 'node:assert/strict';
import { canonicalJson, httpRequestHash, normalizeBody, normalizeUrl, requirementsHash, sha256Hex } from '../src/canonical.ts';
import { KobX402Error } from '../src/errors.ts';
import { loadVector } from './helpers/env.ts';

test('sorted, compact, integers only (same cases as canonical.rs)', () => {
  assert.equal(canonicalJson({ b: [1, { z: null, a: true }], a: 'x"y', é: 1 }), '{"a":"x\\"y","b":[1,{"a":true,"z":null}],"é":1}');
  assert.throws(() => canonicalJson({ f: 1.5 }), KobX402Error);
  assert.throws(() => canonicalJson({ n: 2 ** 53 }), KobX402Error);
  assert.throws(() => canonicalJson({ n: 1n }), KobX402Error);
});

test('keys sort by UTF-16 code units, not UTF-8 bytes', () => {
  // U+FF5E (BMP) sorts after U+1F600 (surrogates D83D..) in UTF-16, before it in UTF-8: same case as the Rust test.
  assert.equal(canonicalJson({ '～': 1, '\u{1f600}': 2 }), '{"\u{1f600}":2,"～":1}');
});

test('undefined members are skipped, other non-JSON values and deep nesting are rejected', () => {
  assert.equal(canonicalJson({ a: 1, b: undefined }), '{"a":1}');
  assert.throws(() => canonicalJson({ f: () => 1 }), KobX402Error);
  let deep: unknown = 1;
  for (let i = 0; i < 40; i++) deep = { x: deep };
  assert.throws(() => canonicalJson(deep), KobX402Error);
  assert.throws(() => canonicalJson(new Map()), KobX402Error);
});

test('control characters escape like serde_json', () => {
  assert.equal(canonicalJson('a\u0001b\n\u007f '), '"a\\u0001b\\n\u007f "');
});

test('rc.1 vector: requirements canonical JSON and SHA-256', () => {
  const v = loadVector('exact/interop-v1.json').paymentRequirements;
  assert.equal(canonicalJson(v.value), v.canonicalJsonUtf8);
  assert.equal(requirementsHash(v.value), v.sha256);
  assert.equal(sha256Hex(v.canonicalJsonUtf8), v.sha256);
});

test('rc.1 vector: request authorization canonical JSON and SHA-256', () => {
  const v = loadVector('exact/interop-v1.json').requestAuthorization;
  // the vector's `input` lacks the domain `scope`, which the digest object adds
  const obj = { scope: 'kaspa-x402-exact-request-authorization-v1', ...v.input };
  assert.equal(canonicalJson(obj), v.canonicalJsonUtf8);
  assert.equal(sha256Hex(canonicalJson(obj)), v.sha256);
});

test('http request hash equals the reference fingerprint formula', () => {
  const reqHash = 'dd'.repeat(32);
  const url = 'https://api.example.test/report?x=1';
  const literal = `{"body":null,"method":"GET","paymentRequirementsHash":"${reqHash}","url":"${url}"}`;
  assert.equal(httpRequestHash('GET', url, null, reqHash), sha256Hex(literal));
  assert.equal(httpRequestHash('GET', url, undefined, reqHash), sha256Hex(literal));
  const withBody = `{"body":{"a":1,"z":[true]},"method":"POST","paymentRequirementsHash":"${reqHash}","url":"${url}"}`;
  assert.equal(httpRequestHash('POST', url, { z: [true], a: 1 }, reqHash), sha256Hex(withBody));
});

test('body and url normalization agree for both sides', () => {
  assert.equal(normalizeBody(undefined), null);
  assert.equal(normalizeBody(''), null);
  assert.equal(normalizeBody(new Uint8Array()), null);
  assert.deepEqual(normalizeBody('{"z":1,"a":[2]}'), { z: 1, a: [2] });
  assert.deepEqual(normalizeBody(new TextEncoder().encode('{"z":1}')), { z: 1 });
  assert.equal(normalizeBody('plain text'), 'plain text');
  assert.equal(normalizeBody('{"f":1.5}'), '{"f":1.5}'); // outside the canonical profile: hashed as text
  assert.match(normalizeBody(new Uint8Array([0xff, 0xfe])) as string, /^sha256:[0-9a-f]{64}$/);
  assert.equal(normalizeUrl('HTTP://Example.COM:80/a b'), 'http://example.com/a%20b');
  assert.throws(() => normalizeUrl('/relative'), KobX402Error);
});
