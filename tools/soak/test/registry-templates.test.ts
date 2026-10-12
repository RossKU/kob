// The soak registry's token programs (src/registry-templates.ts): the same pins as the shipped registry, and external tokens
// (a Kasseal launchpad token, program KCC20PublicMint) listed on the reference's published public-mint build.
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { externalRegistryToken, soakTemplates, TEMPLATE_PUBLIC_MINT, TEMPLATE_STANDARD } from '../src/registry-templates.ts';

const shipped = JSON.parse(readFileSync(new URL('../../../registry/tokens.json', import.meta.url), 'utf8')) as {
  templates: Record<string, unknown>[];
};

test('the soak templates pin what the shipped registry pins', () => {
  for (const t of [TEMPLATE_STANDARD, TEMPLATE_PUBLIC_MINT]) {
    const s = shipped.templates.find((x) => x.id === t.id)!;
    assert.ok(s, t.id);
    for (const k of ['template_hash', 'prefix_len', 'suffix_len', 'state_len', 'max_token_inputs', 'max_token_outputs', 'review_status'] as const) {
      assert.deepEqual(t[k], s[k], `${t.id} ${k}`);
    }
  }
});

test('an external public-mint token is listed on the public-mint template, which is added only when used', () => {
  const kobks = { ticker: 'KOBKS', name: 'KOB Order Book Test', covenantId: '4c9948013250170cd5f2c5711392380376615ad357ef67c355dc352da3c164c6', program: 'KCC20PublicMint', decimals: 0 } as const;
  const e = externalRegistryToken(kobks);
  assert.equal(e.template_id, 'kcc20-ref-public-mint');
  assert.equal(e.extension_commitment, '0'.repeat(64));
  assert.deepEqual([e.max_token_inputs, e.max_token_outputs, e.status, e.decimals], [3, 3, 'listed', 0]);
  assert.deepEqual(soakTemplates([]).map((t) => (t as { id: string }).id), ['kcc20-ref-3x3']);
  assert.deepEqual(soakTemplates([kobks]).map((t) => (t as { id: string }).id), ['kcc20-ref-3x3', 'kcc20-ref-public-mint']);
  assert.throws(() => externalRegistryToken({ ...kobks, covenantId: 'ABC' }), /64 lowercase hex/);
});
