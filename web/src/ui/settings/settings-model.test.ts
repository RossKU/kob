import { describe, expect, it } from 'vitest';
import { DEFAULT_CONFIG, mergeConfig, readSettings, saveSettings, type AppConfig } from '../../config';
import { changedFields, changesServers, clearKobStorage, describeStorageKey, formFromConfig, kobStorageKeys, settingsPatch, validateSettings } from './settings-model';

const base = (): AppConfig => mergeConfig({ injected: { network: 'testnet-10', indexerUrl: 'https://idx.example', nodeUrl: 'wss://node.example:18210' } });

describe('validateSettings', () => {
  it('accepts the running configuration', () => {
    const v = validateSettings(formFromConfig(base()));
    expect(v.ok).toBe(true);
    expect(v.errors).toEqual({});
  });

  it('normalises URLs (trim, trailing slash) and lets empty URLs mean "unset"', () => {
    const v = validateSettings({ ...formFromConfig(base()), indexerUrl: '  https://idx.example/  ', nodeUrl: '' });
    expect(v.ok).toBe(true);
    expect(v.clean.indexerUrl).toBe('https://idx.example');
    expect(v.clean.nodeUrl).toBe('');
  });

  it('rejects wrong schemes, credentials in URLs and unknown networks, field by field', () => {
    const v = validateSettings({
      network: 'devnet' as never, indexerUrl: 'ws://idx.example', nodeUrl: 'ftp://node', registryUrl: '//evil.example/x.json', kastle: false, priorityFee: false, dynamicFee: true,
    });
    expect(v.ok).toBe(false);
    expect(v.errors).toEqual({ network: 'network', indexerUrl: 'url', nodeUrl: 'url', registryUrl: 'registry' });
    expect(validateSettings({ ...formFromConfig(base()), indexerUrl: 'https://user:pw@idx.example' }).errors.indexerUrl).toBe('url');
  });

  it('accepts a bundled relative registry path and an http(s) registry URL, not other schemes', () => {
    const f = formFromConfig(base());
    expect(validateSettings({ ...f, registryUrl: './registry/tokens.json' }).ok).toBe(true);
    expect(validateSettings({ ...f, registryUrl: 'https://reg.example/tokens.json' }).ok).toBe(true);
    expect(validateSettings({ ...f, registryUrl: 'javascript:alert(1)' }).ok).toBe(false);
    expect(validateSettings({ ...f, registryUrl: '' }).ok).toBe(false);
  });
});

describe('changedFields', () => {
  it('lists what differs and flags server changes', () => {
    const c = base();
    const f = formFromConfig(c);
    expect(changedFields(c, f)).toEqual([]);
    const ch = changedFields(c, { ...f, indexerUrl: 'https://other.example/', kastle: true, network: 'mainnet' });
    expect(ch.sort()).toEqual(['indexerUrl', 'kastle', 'network']);
    expect(changesServers(ch)).toBe(true);
    expect(changesServers(['kastle', 'network'])).toBe(false);
    // a trailing slash alone is not a change
    expect(changedFields(c, { ...f, indexerUrl: 'https://idx.example/' })).toEqual([]);
  });

  it('the dynamic fee switch is a form field of its own (default on) and is not a server change', () => {
    const c = base();
    const f = formFromConfig(c);
    expect(f.dynamicFee).toBe(true);
    const ch = changedFields(c, { ...f, dynamicFee: false });
    expect(ch).toEqual(['dynamicFee']);
    expect(changesServers(ch)).toBe(false);
  });
});

describe('settingsPatch + saveSettings', () => {
  const memory = () => {
    const m = new Map<string, string>();
    return { getItem: (k: string) => m.get(k) ?? null, setItem: (k: string, v: string) => void m.set(k, v), removeItem: (k: string) => void m.delete(k), m };
  };

  it('round-trips into the config layers and never stores the test flag', () => {
    const st = memory();
    const clean = validateSettings({ ...formFromConfig(base()), indexerUrl: 'https://new.example/', kastle: true }).clean;
    expect(saveSettings({ ...settingsPatch(clean), features: { kastle: true, test: true } }, st)).toBe(true);
    const stored = readSettings(st) as { features: Record<string, unknown>; indexerUrl: string };
    expect(stored.indexerUrl).toBe('https://new.example');
    expect(stored.features).toEqual({ kastle: true });
    const cfg = mergeConfig({ settings: stored });
    expect(cfg.indexerUrl).toBe('https://new.example');
    expect(cfg.features).toEqual({ kastle: true, test: false, priorityFee: false });
  });

  it('the dynamic fee switch is stored under fees.dynamic and comes back in the config', () => {
    const st = memory();
    const clean = validateSettings({ ...formFromConfig(base()), dynamicFee: false }).clean;
    expect(saveSettings(settingsPatch(clean), st)).toBe(true);
    expect((readSettings(st) as { fees: unknown }).fees).toEqual({ dynamic: false });
    expect(mergeConfig({ settings: readSettings(st) }).fees).toEqual({ dynamic: false, maxRate: 1000, maxFeeKas: 1 });
  });

  it('an empty indexer URL is stored as "no indexer" and overrides an earlier value', () => {
    const st = memory();
    saveSettings({ indexerUrl: 'https://a.example' }, st);
    saveSettings(settingsPatch(validateSettings({ ...formFromConfig(DEFAULT_CONFIG as AppConfig), indexerUrl: '' }).clean), st);
    expect(mergeConfig({ settings: readSettings(st) }).indexerUrl).toBe('');
  });
});

describe('local data', () => {
  const fake = (keys: string[]) => {
    const list = [...keys];
    return { get length() { return list.length; }, key: (i: number) => list[i] ?? null, removeItem: (k: string) => void list.splice(list.indexOf(k), 1), list };
  };

  it('finds and clears only the app keys', () => {
    const s = fake(['kob.settings', 'kob.lang', 'kob.records.v1:testnet-10:aa', 'kob.tokens.v1.testnet-10.bb', 'theme', 'kobalt']);
    expect(kobStorageKeys(s)).toHaveLength(4);
    expect(clearKobStorage(s)).toHaveLength(4);
    expect(s.list).toEqual(['theme', 'kobalt']);
  });

  it('describes keys', () => {
    expect(describeStorageKey('kob.settings')).toBe('settings');
    expect(describeStorageKey('kob.lang')).toBe('other'); // legacy key of the removed language switch
    expect(describeStorageKey('kob.records.v1:x:y')).toBe('records');
    expect(describeStorageKey('kob.tokens.v1.x.y')).toBe('tokens');
    expect(describeStorageKey('kob.other')).toBe('other');
  });
});
