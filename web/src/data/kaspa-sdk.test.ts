import { describe, expect, it } from 'vitest';
import { loadKaspaSdkNode } from './kaspa-sdk.node';
import {
  addressPrefix, addressToSpkString, joinSpkString, normalizeNetwork, pubkeyToAddress, spkStringToAddress, splitSpkString,
} from './kaspa-sdk';
import { loadKobNode } from '../kob/wasm.node';
import { pubkeyOf } from '../testing/local-signer';

const sdk = loadKaspaSdkNode();
const PK = '462779ad4aad39514614751a71085f2f10e1c7a593e4e030efb5b8721ce55b0b';

describe('normalizeNetwork', () => {
  it('maps every spelling wallets and nodes use', () => {
    for (const s of ['mainnet', 'Mainnet', 'kaspa_mainnet', 'kaspa-mainnet', 'kaspa:mainnet']) expect(normalizeNetwork(s), s).toBe('mainnet');
    for (const s of ['testnet-10', 'kaspa_testnet_10', 'kaspa-testnet-10', 'testnet10', 'TESTNET_10', 'Kaspa_Testnet_10']) {
      expect(normalizeNetwork(s), s).toBe('testnet-10');
    }
    expect(normalizeNetwork('kaspa_testnet_11')).toBe('testnet-11');
  });
  it('leaves unknown names lower-cased and tolerates non-strings', () => {
    expect(normalizeNetwork('Foo')).toBe('foo');
    expect(normalizeNetwork(undefined)).toBe('');
    expect(normalizeNetwork(7)).toBe('');
  });
  it('address prefixes', () => {
    expect(addressPrefix('mainnet')).toBe('kaspa');
    expect(addressPrefix('kaspa_testnet_10')).toBe('kaspatest');
  });
});

describe('address helpers (official SDK)', () => {
  it('pubkeyToAddress makes the P2PK Schnorr address and matches payToAddressScript', () => {
    const main = pubkeyToAddress(sdk, PK, 'mainnet');
    const test = pubkeyToAddress(sdk, PK, 'kaspa_testnet_10');
    expect(main).toMatch(/^kaspa:q/);
    expect(test).toMatch(/^kaspatest:q/);
    expect(sdk.Address.validate(main)).toBe(true);
    expect(addressToSpkString(sdk, test)).toBe('0000' + '20' + PK + 'ac');
  });

  it('spkStringToAddress / addressToSpkString round-trip for P2PK and P2SH scripts', () => {
    const p2sh = '0000aa2089ec74ef444de4739a38f76daf992325c6c4b51439704ce1e31f5fb60a9bccaa87';
    const a = spkStringToAddress(sdk, p2sh, 'testnet-10');
    expect(a).toMatch(/^kaspatest:p/);
    expect(addressToSpkString(sdk, a)).toBe(p2sh);
    expect(addressToSpkString(sdk, spkStringToAddress(sdk, '0000' + '20' + PK + 'ac', 'mainnet'))).toBe('0000' + '20' + PK + 'ac');
  });

  it('the address of a kob-wasm token script is a script-hash address', () => {
    const kob = loadKobNode();
    const spk = kob.tokenScriptPublicKey('KCC20Ref', {
      amount: '7000', owner: PK, owner_scheme: 0, borrow_scheme: 0, borrow_guard: '00'.repeat(32), extension_commitment: 'ee'.repeat(32),
    });
    expect(spkStringToAddress(sdk, spk, 'testnet-10')).toMatch(/^kaspatest:p/);
  });

  it('rejects malformed input instead of producing a wrong address', () => {
    expect(() => pubkeyToAddress(sdk, 'abcd', 'mainnet')).toThrow(/32-byte/);
    expect(() => pubkeyToAddress(sdk, '02' + PK, 'mainnet')).toThrow(/32-byte/);
    expect(() => spkStringToAddress(sdk, 'zz', 'mainnet')).toThrow(/invalid script/);
    expect(() => spkStringToAddress(sdk, '0000' + '51', 'mainnet')).toThrow(/no address form/);
  });

  it('spk string helpers', () => {
    expect(splitSpkString('0001ABcd')).toEqual({ version: 1, script: 'abcd' });
    expect(joinSpkString(1, 'abcd')).toBe('0001abcd');
  });

  it('derives the same pubkey address from a locally generated key', () => {
    const sk = '01'.repeat(32);
    const pk = pubkeyOf(sk);
    expect(addressToSpkString(sdk, pubkeyToAddress(sdk, pk, 'testnet-10'))).toBe('000020' + pk + 'ac');
  });
});
