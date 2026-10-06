import { describe, expect, it } from 'vitest';
import { MAKER_SK, makeEnv } from '../testing/fixtures';
import { signBuilt } from '../testing/local-signer';
import type { BuiltTx, InputSignature } from '../kob/types';
import { covenantSigningKnown, probeCovenantSigning, probeTx, storedVerdict } from './covenant-probe';
import { WalletError, type WalletAdapter } from './types';

const env = makeEnv();
const kob = env.kob;
const store = () => {
  const m = new Map<string, string>();
  return { m, getItem: (k: string) => m.get(k) ?? null, setItem: (k: string, v: string) => void m.set(k, v) };
};
const adapter = (sign: (b: BuiltTx) => Promise<InputSignature[]>, covenantSigning?: 'proven' | 'unknown'): WalletAdapter =>
  ({ id: 'kastle', label: 'Kastle', detect: () => true, connect: async () => { throw new Error('no'); }, signTx: sign, ...(covenantSigning ? { covenantSigning } : {}) }) as WalletAdapter;

describe('covenant-signing probe (C5-10)', () => {
  it('the test transaction is a maker cancel of a synthetic order: one covenant input to sign, nothing else', () => {
    const b = probeTx(kob, env);
    expect(b.sign).toHaveLength(1);
    expect(b.sign[0]!.redeemScript).not.toBeNull();
    expect(b.sign[0]!.pubkey).toBe(env.maker);
    expect(b.tx.inputs).toHaveLength(1);
    expect(b.tx.inputs[0]!.transactionId).toBe('c0'.repeat(32));
  });

  it('a wallet that signs the covenant input passes and is remembered', async () => {
    const st = store();
    const ok = adapter(async (b) => signBuilt(b, [MAKER_SK]));
    expect(covenantSigningKnown(ok, env.maker, st)).toBeNull();
    expect(await probeCovenantSigning(kob, ok, env, 'testnet-10', st)).toBe('ok');
    expect(storedVerdict('kastle', env.maker, st)).toBe('ok');
    expect(covenantSigningKnown(ok, env.maker, st)).toBe('ok');
  });

  it('a wallet that returns the covenant input unsigned (Kastle #353) or a wrong signature is refused for good', async () => {
    const st = store();
    const none = adapter(async () => { throw new WalletError('no-signature', 'input 0 unsigned'); });
    expect(await probeCovenantSigning(kob, none, env, 'testnet-10', st)).toBe('unsupported');
    expect(covenantSigningKnown(none, env.maker, st)).toBe('unsupported');
    const st2 = store();
    const wrong = adapter(async (b) => signBuilt(b, ['33'.repeat(32)]));
    expect(await probeCovenantSigning(kob, wrong, env, 'testnet-10', st2)).toBe('unsupported');
  });

  it('a declined popup or a timeout is not a verdict; a declared capable wallet needs no check', async () => {
    const st = store();
    expect(await probeCovenantSigning(kob, adapter(async () => { throw new WalletError('rejected', 'no'); }), env, 'testnet-10', st)).toBe('declined');
    expect(await probeCovenantSigning(kob, adapter(async () => { throw new WalletError('timeout', 'slow'); }), env, 'testnet-10', st)).toBe('error');
    expect(st.m.size).toBe(0);
    expect(covenantSigningKnown(adapter(async () => [], 'proven'), env.maker, st)).toBe('ok');
  });
});
