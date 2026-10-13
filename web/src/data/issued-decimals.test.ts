import { describe, expect, it } from 'vitest';
import { issuedDecimals, rememberIssuedDecimals } from './issued-decimals';

const mem = () => {
  const m = new Map<string, string>();
  return { getItem: (k: string) => m.get(k) ?? null, setItem: (k: string, v: string) => void m.set(k, v), m };
};
const A = 'ab'.repeat(32);
const B = 'cd'.repeat(32);

describe('issued decimals (prefill of a first order, per browser)', () => {
  it('remembers the decimals of a token issued here, per network and covenant id', () => {
    const s = mem();
    expect(rememberIssuedDecimals('mainnet', A.toUpperCase(), 8, s)).toBe(true);
    expect(rememberIssuedDecimals('testnet-10', B, 2, s)).toBe(true);
    expect(issuedDecimals('mainnet', A, s)).toBe(8);
    expect(issuedDecimals('mainnet', B, s)).toBeNull();
    expect(issuedDecimals('testnet-10', B.toUpperCase(), s)).toBe(2);
    // a later issuance of the same id (never in practice) replaces it
    rememberIssuedDecimals('mainnet', A, 3, s);
    expect(issuedDecimals('mainnet', A, s)).toBe(3);
  });

  it('refuses what an issuance cannot produce, and reads a damaged or missing store as empty', () => {
    const s = mem();
    expect(rememberIssuedDecimals('mainnet', 'xyz', 8, s)).toBe(false);
    expect(rememberIssuedDecimals('mainnet', A, 19, s)).toBe(false);
    expect(rememberIssuedDecimals('mainnet', A, 1.5, s)).toBe(false);
    expect(rememberIssuedDecimals('mainnet', A, 8, null)).toBe(false);
    expect(issuedDecimals('mainnet', A, null)).toBeNull();
    s.setItem('kob.issuedDecimals.v1', '{not json');
    expect(issuedDecimals('mainnet', A, s)).toBeNull();
    s.setItem('kob.issuedDecimals.v1', JSON.stringify({ mainnet: { [A]: 'x', [B]: 99, bad: 1 } }));
    expect(issuedDecimals('mainnet', A, s)).toBeNull();
    expect(issuedDecimals('mainnet', B, s)).toBeNull();
    // a throwing storage is no storage
    const broken = { getItem: () => { throw new Error('blocked'); }, setItem: () => { throw new Error('blocked'); } };
    expect(rememberIssuedDecimals('mainnet', A, 8, broken)).toBe(false);
    expect(issuedDecimals('mainnet', A, broken)).toBeNull();
  });
});
