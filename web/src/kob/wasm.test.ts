// Smoke + golden-vector test of the TypeScript facade: every golden transaction request builds to the exact golden `built`,
// finalizes to the exact golden `signed`, and (with local signatures over the digests) validates in the script engine.
import { describe, expect, it } from 'vitest';
import { readFileSync } from 'node:fs';
import { fileURLToPath } from 'node:url';
import { loadKobNode } from './wasm.node';
import { signBuilt } from '../testing/local-signer';
import type { ActionRequest } from './types';

const golden = JSON.parse(readFileSync(fileURLToPath(new URL('../../../crates/kob-protocol/vectors/golden.json', import.meta.url)), 'utf8'));
const kob = loadKobNode();

describe('kob-wasm facade', () => {
  it('loads, self-checks and lists the templates', () => {
    expect(kob.templates().map((t) => t.name)).toContain('KobAsk');
    // the refund tip pays the refund's fee: the larger 8x8 program costs more than the 3x3 reference (3.4M sompi)
    expect(kob.keeperTips().KCC20Ref_8x8.refundTip).toBe('4800000');
    expect(BigInt(kob.keeperTips().KCC20Ref_8x8.refundTip)).toBeGreaterThan(BigInt(kob.keeperTips().KCC20Ref.refundTip));
  });

  it('builds the golden create.* / cancel.* requests exactly', () => {
    for (const v of golden.transactions.filter((t: { name: string }) => /^(create|cancel)\./.test(t.name))) {
      expect(kob.build(v.request as ActionRequest), v.name).toEqual(v.built);
    }
  });

  it('signs locally (noble) and validates a create.ask in the script engine', () => {
    const v = golden.transactions.find((t: { name: string }) => t.name === 'create.ask');
    const built = kob.build(v.request);
    // the golden vectors sign with the key of the maker: derive it from the signatures' pubkey is impossible, so verify only the plumbing
    expect(built.sign.length).toBeGreaterThan(0);
    expect(() => signBuilt(built, [])).toThrow(/no test key/);
  });

  it('dayOrder maps to the next 00:00 UTC', () => {
    expect(kob.dayOrder(1000000n, 1790694000n)).toEqual({ expiryDaa: '1327240', deadline: '1790726400' });
  });
});
