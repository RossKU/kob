import { describe, expect, it } from 'vitest';
import { resolveConfig, saveSettings, SETTINGS_KEY, type StorageLike } from '../../config';
import { networkSwitchPatch, networkSwitchState, otherNetwork, storedServers } from './network-switch';

class MemStorage implements StorageLike {
  m = new Map<string, string>();
  getItem(k: string) { return this.m.get(k) ?? null; }
  setItem(k: string, v: string) { this.m.set(k, v); }
}

const file = { network: 'testnet-10', indexerUrl: 'https://tn.example', nodeUrl: 'wss://tn.example/wrpc' };

describe('network switch model', () => {
  it('the other network of the two', () => {
    expect(otherNetwork('mainnet')).toBe('testnet-10');
    expect(otherNetwork('testnet-10')).toBe('mainnet');
  });

  it('a deployment that pins its network offers no switch, whatever the browser saved', () => {
    const c = resolveConfig({ file: { ...file, lockNetwork: true } }).config;
    expect(networkSwitchState(c, undefined)).toEqual({ kind: 'pinned' });
    expect(networkSwitchState(c, { indexerUrl: 'https://mine.example' })).toEqual({ kind: 'pinned' });
  });

  it('is open with nothing saved, or with a saved network, flags and fees (they are no servers)', () => {
    const c = resolveConfig({ file }).config;
    expect(networkSwitchState(c, undefined)).toEqual({ kind: 'open' });
    expect(networkSwitchState(c, { network: 'mainnet', features: { kastle: true }, fees: { dynamic: false }, allowQueryOverrides: true })).toEqual({ kind: 'open' });
  });

  it('is blocked by every saved server: the indexer, the node, the registry and the further indexers', () => {
    const c = resolveConfig({ file }).config;
    expect(networkSwitchState(c, { indexerUrl: 'https://mine.example', registryUrl: './r.json', nodeUrl: 'wss://n.example', extraIndexerUrls: ['https://x.example'], network: 'mainnet' }))
      .toEqual({ kind: 'blocked', fields: ['indexerUrl', 'nodeUrl', 'registryUrl', 'extraIndexerUrls'] });
    expect(storedServers(undefined)).toEqual([]);
  });

  it('choosing saves the network alone, like Settings, and the next load is on it with none of the deployment\'s servers', () => {
    const c = resolveConfig({ file }).config;
    expect(networkSwitchPatch(c, 'mainnet')).toEqual({ network: 'mainnet' });
    const st = new MemStorage();
    saveSettings({ features: { kastle: true } }, st);
    expect(saveSettings(networkSwitchPatch(c, 'mainnet'), st)).toBe(true);
    expect(JSON.parse(st.getItem(SETTINGS_KEY)!)).toEqual({ network: 'mainnet', features: { kastle: true } });
    const next = resolveConfig({ file, settings: JSON.parse(st.getItem(SETTINGS_KEY)!) }).config;
    expect(next).toMatchObject({ network: 'mainnet', indexerUrl: '', nodeUrl: '' });
    // and back: the deployment's own servers apply again
    expect(saveSettings(networkSwitchPatch(next, 'testnet-10'), st)).toBe(true);
    expect(resolveConfig({ file, settings: JSON.parse(st.getItem(SETTINGS_KEY)!) }).config).toMatchObject({ network: 'testnet-10', indexerUrl: 'https://tn.example', nodeUrl: 'wss://tn.example/wrpc' });
  });
});
