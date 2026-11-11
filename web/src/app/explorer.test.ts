import { describe, expect, it } from 'vitest';
import { explorerAddressUrl } from './explorer';

const ADDR = `kaspatest:q${'p'.repeat(60)}`;

describe('the explorer page of a wallet address', () => {
  it('follows the network: the explorer in force, else the default of the network', () => {
    expect(explorerAddressUrl({ network: 'testnet-10', explorerUrl: '' }, ADDR)).toBe(`https://tn10.kaspa.stream/addresses/${ADDR}`);
    const main = `kaspa:q${'p'.repeat(60)}`;
    expect(explorerAddressUrl({ network: 'mainnet', explorerUrl: '' }, main)).toBe(`https://kaspa.stream/addresses/${main}`);
    expect(explorerAddressUrl({ network: 'mainnet', explorerUrl: 'https://explorer.example' }, main)).toBe(`https://explorer.example/addresses/${main}`);
  });

  it('is null for anything that is not an address (no link is built from a stray string)', () => {
    for (const bad of [null, undefined, '', 'kaspa:', 'javascript:alert(1)', 'https://evil.example/', `kaspa:q${'p'.repeat(60)}/../x`, 'not an address']) {
      expect(explorerAddressUrl({ network: 'mainnet', explorerUrl: '' }, bad as string | null | undefined), String(bad)).toBeNull();
    }
  });
});
