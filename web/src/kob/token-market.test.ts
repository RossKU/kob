import { describe, expect, it } from 'vitest';
import { readFileSync } from 'node:fs';
import { fileURLToPath } from 'node:url';
import { loadKobNode } from './wasm.node';
import { TokenMarketError, toTokenMarket, type RegistryTemplateLike, type RegistryTokenLike } from './token-market';

const kob = loadKobNode();
const registry = JSON.parse(readFileSync(fileURLToPath(new URL('../../../registry/tokens.example.json', import.meta.url)), 'utf8')) as {
  templates: RegistryTemplateLike[];
  tokens: RegistryTokenLike[];
};

describe('toTokenMarket', () => {
  it('derives the 8x8 market of the registry example token from the embedded templates and tips; the scale from its decimals', () => {
    const t = registry.tokens.find((x) => x.ticker === 'EXKCC') as RegistryTokenLike;
    const m = toTokenMarket(kob, { ...t, tick: 100 }, registry.templates);
    expect(m.program).toBe('KCC20Ref_8x8');
    expect(m.templateHash).toBe('666da060d02663939efdc534ea10cce219e5564af86f4cc8eeea2ca129f7c032');
    expect(m.prefixLen).toBe(1);
    expect(m.suffixLen).toBe(6237);
    expect(m.slots).toEqual({ inputs: 8, outputs: 8 });
    expect(m.refundTip).toBe(BigInt(kob.keeperTips().KCC20Ref_8x8.refundTip));
    expect(m.keeperTip).toBe(BigInt(kob.keeperTips().KCC20Ref_8x8.keeperTip));
    expect(m.extensionCommitment).toBe('ee'.repeat(32));
    expect(m.covenantId).toBe('e5'.repeat(32));
    expect(m.decimals).toBe(8);
    expect(m.scale).toBe(100_000_000n);
    expect(m.tick).toBe(100n);
  });

  it('derives the reference 3x3 program (golden template: prefix 1, suffix 2802)', () => {
    const m = toTokenMarket(kob, { ticker: 'REF', covenant_id: '70'.repeat(32), template_id: 'kcc20-ref-3x3', extension_commitment: 'ee'.repeat(32), decimals: 3, tick: 100n });
    expect(m.program).toBe('KCC20Ref');
    expect(m.templateHash.startsWith('173ca6a7')).toBe(true);
    expect(m.suffixLen).toBe(2802);
    expect(m.slots).toEqual({ inputs: 3, outputs: 3 });
    expect(m.refundTip).toBe(3_400_000n);
    expect(m.scale).toBe(1000n);
  });

  it('the order scale is 10^decimals capped at 10^9 (kob-wasm defaultScale); the tick defaults to 1 sompi; a registry lot size is ignored', () => {
    const base: RegistryTokenLike = { ticker: 'X', covenant_id: '70'.repeat(32), template_id: 'kcc20-ref-3x3', extension_commitment: 'ee'.repeat(32), decimals: 3 };
    for (const [decimals, scale] of [[0, 1n], [3, 1000n], [9, 1_000_000_000n], [12, 1_000_000_000n], [18, 1_000_000_000n]] as const) {
      expect(toTokenMarket(kob, { ...base, decimals }).scale, String(decimals)).toBe(scale);
      expect(kob.defaultScale(decimals)).toBe(scale);
    }
    expect(toTokenMarket(kob, base).tick).toBe(1n);
    expect(toTokenMarket(kob, { ...base, tick: null }).tick).toBe(1n);
    expect(toTokenMarket(kob, { ...base, lot_size: 7 } as RegistryTokenLike).scale).toBe(1000n);
  });

  it('refuses tokens KOB cannot trade, loudly', () => {
    const base: RegistryTokenLike = { ticker: 'X', covenant_id: '70'.repeat(32), template_id: 'kcc20-ref-3x3', extension_commitment: 'ee'.repeat(32), decimals: 3, tick: 100 };
    expect(() => toTokenMarket(kob, { ...base, extension_commitment: null })).toThrow(/extension commitment/);
    expect(() => toTokenMarket(kob, { ...base, family: 'kron' })).toThrow(/does not match the program/);
    expect(() => toTokenMarket(kob, { ...base, template_id: 'nope' })).toThrow(TokenMarketError);
    expect(() => toTokenMarket(kob, { ...base, tick: 0 })).toThrow(/positive/);
    expect(() => toTokenMarket(kob, { ...base, decimals: 19 })).toThrow(/decimals/);
    // registry template that disagrees with the embedded program
    expect(() => toTokenMarket(kob, base, [{ id: 'kcc20-ref-3x3', template_hash: '00'.repeat(32) }])).toThrow(/hash differs/);
    expect(() => toTokenMarket(kob, base, [{ id: 'kcc20-ref-3x3', template_hash: '173ca6a796c2c05f171c31b9a73aaca161a3226a9e8f57d2f3d833ff18dbe41b', max_token_inputs: 8 }])).toThrow(/input slots/);
  });

  it('accepts a 0x-prefixed hex and normalises it', () => {
    const m = toTokenMarket(kob, { ticker: 'X', covenant_id: '0x' + 'AB'.repeat(32), template_id: 'kcc20-ref-3x3', extension_commitment: '0x' + 'ee'.repeat(32), decimals: 0, tick: 1 });
    expect(m.covenantId).toBe('ab'.repeat(32));
    expect(m.extensionCommitment).toBe('ee'.repeat(32));
  });
});
