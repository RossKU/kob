// The soak registry's token programs (src/registry-templates.ts): the same pins as the shipped registry, and external tokens
// (a Kasseal launchpad token, program KCC20PublicMint) listed on the reference's published public-mint build.
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { externalRegistryToken, soakTemplates, TEMPLATE_PUBLIC_MINT, TEMPLATE_STANDARD, TEST_PROGRAM_TEMPLATES } from '../src/registry-templates.ts';

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
  assert.deepEqual([e.max_token_inputs, e.max_token_outputs, e.decimals, 'status' in e], [3, 3, 0, false]);
  assert.deepEqual(soakTemplates([]).map((t) => (t as { id: string }).id), ['kcc20-ref-3x3']);
  assert.deepEqual(soakTemplates([kobks]).map((t) => (t as { id: string }).id), ['kcc20-ref-3x3', 'kcc20-ref-public-mint']);
  assert.throws(() => externalRegistryToken({ ...kobks, covenantId: 'ABC' }), /64 lowercase hex/);
});

test('the launchpad test programs pin what the embedded profiles say, and are added when named or used', () => {
  const programs = JSON.parse(readFileSync(new URL('../../../crates/kob-protocol/data/programs.json', import.meta.url), 'utf8')) as {
    programs: { name: string; registry_id: string; template_hash: string; codec: string; layout: Record<string, number>; slots: { inputs: number; outputs: number }; owners: { custody: number; holder: number }; networks?: string[] }[];
  };
  for (const [name, t] of Object.entries(TEST_PROGRAM_TEMPLATES)) {
    const p = programs.programs.find((x) => x.name === name)!;
    assert.ok(p, name);
    assert.deepEqual(
      [t.id, t.family, t.template_hash, t.prefix_len, t.state_len, t.suffix_len, t.max_token_inputs, t.max_token_outputs],
      [p.registry_id, p.codec, p.template_hash, p.layout.prefix_len, p.layout.state_len, p.layout.suffix_len, p.slots.inputs, p.slots.outputs],
      name,
    );
    assert.ok(p.networks?.includes('testnet-10'), name); // KaspaRocket's program is on mainnet too since the 2026-10-27 review
    // the custody owner the order templates pin; a KRON program's holder type is the build of the order kinds (0: *KronD0)
    if (t.family === 'kron') assert.deepEqual(t.escrow, { id_type: p.owners.custody, is_minter: 0, delivery_id_type: p.owners.holder });
    else assert.deepEqual(t.escrow, { owner_scheme: p.owners.custody, borrow_scheme: 0 });
    // the shipped (mainnet) registry does not carry them
    if (!p.networks?.includes('mainnet')) assert.ok(!shipped.templates.some((x) => x.template_hash === t.template_hash), name);
  }
  const ids = (ts: unknown[]) => ts.map((t) => (t as { id: string }).id);
  assert.deepEqual(ids(soakTemplates([], ['KaspaRocketToken11458', 'KForgeToken1785'])), ['kcc20-ref-3x3', 'kforge-token-1785', 'kasparocket-token-11458']);
  const kf = { ticker: 'KFT', name: 'KForge test', covenantId: '38'.repeat(32), program: 'KForgeToken1785', decimals: 8 } as const;
  assert.deepEqual(ids(soakTemplates([kf])), ['kcc20-ref-3x3', 'kforge-token-1785']);
  const e = externalRegistryToken(kf);
  assert.deepEqual([e.template_id, e.family, e.extension_commitment, e.max_token_inputs, e.max_token_outputs], ['kforge-token-1785', 'kron', null, 1, 3]);
  assert.throws(() => soakTemplates([], ['KForge']), /unknown program KForge/);
});
