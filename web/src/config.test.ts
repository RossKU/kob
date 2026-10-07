import { describe, expect, it } from 'vitest';
import {
  DEFAULT_CONFIG, SETTINGS_KEY, explorerBase, explorerTxUrl, loadConfig, loadConfigWithWarnings, mergeConfig, queryLayer, readSettings, resolveConfig, saveSettings,
  parseMaxFeeKas, parseMaxRate, validateRegistryUrl, validateUrl, type StorageLike,
} from './config';

class MemStorage implements StorageLike {
  m = new Map<string, string>();
  getItem(k: string) { return this.m.get(k) ?? null; }
  setItem(k: string, v: string) { this.m.set(k, v); }
  removeItem(k: string) { this.m.delete(k); }
}
const throwing: StorageLike = {
  getItem() { throw new Error('blocked'); },
  setItem() { throw new Error('blocked'); },
};

describe('validateUrl', () => {
  it('accepts ws/wss/http/https, trims and drops the trailing slash', () => {
    expect(validateUrl(' ws://127.0.0.1:18210/ ')).toBe('ws://127.0.0.1:18210');
    expect(validateUrl('https://kob.example/api//')).toBe('https://kob.example/api');
    expect(validateUrl('')).toBe('');
  });
  it('rejects other schemes, garbage, credentials and non-strings', () => {
    for (const bad of ['ftp://x.example', 'javascript:alert(1)', 'not a url', 'https://u:p@x.example', 42, null, undefined, 'file:///etc/passwd']) {
      expect(validateUrl(bad), String(bad)).toBeNull();
    }
  });
  it('registry urls: relative paths or http(s) only', () => {
    expect(validateRegistryUrl('./registry/tokens.json')).toBe('./registry/tokens.json');
    expect(validateRegistryUrl('https://r.example/t.json')).toBe('https://r.example/t.json');
    expect(validateRegistryUrl('//evil.example/t.json')).toBeNull();
    expect(validateRegistryUrl('ws://r.example/t.json')).toBeNull();
    expect(validateRegistryUrl('')).toBeNull();
  });
});

describe('mergeConfig', () => {
  it('defaults: mainnet, empty indexer/node (public resolver), bundled registry, kastle off, no query overrides', () => {
    expect(mergeConfig()).toEqual({
      network: 'mainnet', indexerUrl: '', extraIndexerUrls: [], nodeUrl: '', registryUrl: './registry/tokens.json',
      features: { kastle: false, test: false, priorityFee: false }, fees: { dynamic: true, maxRate: 1000, maxFeeKas: 1 }, allowQueryOverrides: false,
      quoteTokens: {}, home: 'auto', explorerUrl: '', marketStartToleranceBps: 1000,
    });
    expect(DEFAULT_CONFIG.features.kastle).toBe(false);
  });

  it('later layers win: file < injected < settings', () => {
    const c = mergeConfig({
      file: { network: 'testnet-10', indexerUrl: 'https://file.example', nodeUrl: 'ws://file:1' },
      injected: { indexerUrl: 'https://injected.example' },
      settings: { indexerUrl: 'https://settings.example/' },
    });
    expect(c.network).toBe('testnet-10');
    expect(c.nodeUrl).toBe('ws://file:1');
    expect(c.indexerUrl).toBe('https://settings.example');
  });

  it('accepts wallet-style network spellings', () => {
    expect(mergeConfig({ injected: { network: 'kaspa_testnet_10' } }).network).toBe('testnet-10');
    expect(mergeConfig({ injected: { network: 'Mainnet' } }).network).toBe('mainnet');
  });

  it('ignores invalid values and reports them, keeping the lower layer', () => {
    const r = resolveConfig({
      file: { indexerUrl: 'https://ok.example' },
      injected: { indexerUrl: 'ftp://bad', network: 'testnet-11', registryUrl: '//x.example/a.json', features: { kastle: 'maybe' } },
    });
    expect(r.config.indexerUrl).toBe('https://ok.example');
    expect(r.config.network).toBe('mainnet');
    expect(r.config.registryUrl).toBe('./registry/tokens.json');
    expect(r.config.features.kastle).toBe(false);
    expect(r.warnings.map((w) => w.field).sort()).toEqual(['features.kastle', 'indexerUrl', 'network', 'registryUrl']);
  });

  it('non-object layers are ignored', () => {
    const r = resolveConfig({ file: 'text', injected: [1, 2], settings: 5 });
    expect(r.config).toEqual(mergeConfig());
    expect(r.warnings).toHaveLength(3);
  });

  it('features.test comes from __KOB_CONFIG__ ONLY', () => {
    const t = { features: { test: true } };
    expect(mergeConfig({ injected: t }).features.test).toBe(true);
    expect(mergeConfig({ file: t }).features.test).toBe(false);
    expect(mergeConfig({ settings: t }).features.test).toBe(false);
    expect(mergeConfig({ injected: { allowQueryOverrides: true }, query: '?test=1&features.test=1' }).features.test).toBe(false);
  });

  it('kastle can be enabled by file, injected or settings', () => {
    expect(mergeConfig({ file: { features: { kastle: true } } }).features.kastle).toBe(true);
    expect(mergeConfig({ injected: { features: { kastle: true } }, settings: { features: { kastle: false } } }).features.kastle).toBe(false);
  });
});

describe('query overrides', () => {
  const q = '?indexer=https://evil.example&node=wss://evil.example:1&network=testnet-10&kastle=1';

  it('are ignored by default (a link must not repoint the app) and reported', () => {
    const r = resolveConfig({ injected: { indexerUrl: 'https://good.example' }, query: q });
    expect(r.config.indexerUrl).toBe('https://good.example');
    expect(r.config.nodeUrl).toBe('');
    expect(r.config.network).toBe('mainnet');
    expect(r.config.features.kastle).toBe(false);
    expect(r.warnings.some((w) => w.layer === 'query' && /disabled/.test(w.message))).toBe(true);
  });

  it('apply last, over everything, when allowQueryOverrides is true', () => {
    const r = resolveConfig({ injected: { allowQueryOverrides: true, indexerUrl: 'https://good.example' }, settings: { registryUrl: './settings.json' }, query: q });
    expect(r.config).toMatchObject({
      indexerUrl: 'https://evil.example', nodeUrl: 'wss://evil.example:1', network: 'testnet-10', registryUrl: './settings.json', features: { kastle: true, test: false },
    });
  });

  it('can be enabled by the file layer too, and a link cannot unlock or set other keys', () => {
    expect(mergeConfig({ file: { allowQueryOverrides: true }, query: '?kastle=1' }).features.kastle).toBe(true);
    const c = mergeConfig({ injected: { allowQueryOverrides: true }, query: '?allowQueryOverrides=0&registry=x&registryUrl=https://x.example/a.json' });
    expect(c.allowQueryOverrides).toBe(true);
    expect(c.registryUrl).toBe('./registry/tokens.json');
  });

  it('validates query values (bad ones are dropped)', () => {
    const c = mergeConfig({ injected: { allowQueryOverrides: true, nodeUrl: 'ws://n:1' }, query: 'node=javascript:1&kastle=perhaps&network=mainnet' });
    expect(c.nodeUrl).toBe('ws://n:1');
    expect(c.features.kastle).toBe(false);
    expect(queryLayer(new URLSearchParams('kastle=0&network=kaspa_mainnet'))).toEqual({ kastle: false, network: 'mainnet' });
  });
});

describe('user settings storage', () => {
  it('round-trips through localStorage key kob.settings and merges over the injected layer', async () => {
    const st = new MemStorage();
    expect(saveSettings({ indexerUrl: 'https://mine.example/', network: 'testnet-10', features: { kastle: true } }, st)).toBe(true);
    expect(JSON.parse(st.getItem(SETTINGS_KEY)!)).toEqual({ indexerUrl: 'https://mine.example', network: 'testnet-10', features: { kastle: true } });
    const cfg = await loadConfig({ fetch: null, injected: { indexerUrl: 'https://host.example' }, storage: st, search: '' });
    expect(cfg.indexerUrl).toBe('https://mine.example');
    expect(cfg.network).toBe('testnet-10');
    expect(cfg.features.kastle).toBe(true);
  });

  it('saveSettings merges, removes on null, drops invalid fields and never persists features.test', () => {
    const st = new MemStorage();
    saveSettings({ nodeUrl: 'ws://a:1', network: 'testnet-10' }, st);
    saveSettings({ nodeUrl: null, indexerUrl: 'ftp://x', features: { test: true, kastle: false } }, st);
    expect(JSON.parse(st.getItem(SETTINGS_KEY)!)).toEqual({ network: 'testnet-10', features: { kastle: false } });
  });

  it('every storage access is guarded (blocked storage, corrupt JSON, no storage)', async () => {
    expect(saveSettings({ network: 'mainnet' }, throwing)).toBe(false);
    expect(saveSettings({ network: 'mainnet' }, null)).toBe(false);
    expect(readSettings(throwing)).toBeUndefined();
    const st = new MemStorage();
    st.setItem(SETTINGS_KEY, '{not json');
    expect(readSettings(st)).toBeUndefined();
    expect((await loadConfig({ fetch: null, injected: undefined, storage: throwing, search: '' })).network).toBe('mainnet');
  });
});

describe('loadConfig', () => {
  const okFetch = (body: unknown, status = 200) =>
    (async () => ({ ok: status >= 200 && status < 300, status, json: async () => body })) as unknown as typeof fetch;

  it('fetches config.json (layer 2), then injected, settings, query', async () => {
    const st = new MemStorage();
    saveSettings({ registryUrl: './settings.json' }, st);
    const cfg = await loadConfig({
      fetch: okFetch({ network: 'testnet-10', nodeUrl: 'ws://from-file:1', allowQueryOverrides: true }),
      injected: { nodeUrl: 'ws://injected:2' },
      storage: st,
      search: '?indexer=https://q.example',
    });
    expect(cfg).toMatchObject({ network: 'testnet-10', nodeUrl: 'ws://injected:2', registryUrl: './settings.json', indexerUrl: 'https://q.example' });
  });

  it('a missing, failing or malformed config.json is ignored', async () => {
    const boom = (async () => { throw new Error('offline'); }) as unknown as typeof fetch;
    const badJson = (async () => ({ ok: true, json: async () => { throw new SyntaxError('x'); } })) as unknown as typeof fetch;
    for (const f of [okFetch({}, 404), boom, badJson, null]) {
      const r = await loadConfigWithWarnings({ fetch: f, injected: undefined, storage: null, search: '' });
      expect(r.config).toEqual(mergeConfig());
      expect(r.warnings).toEqual([]);
    }
  });

  it('reads window.__KOB_CONFIG__ and location.search from globalThis by default', async () => {
    const g = globalThis as Record<string, unknown>;
    g.__KOB_CONFIG__ = { nodeUrl: 'http://127.0.0.1:9999', features: { test: true } };
    try {
      const cfg = await loadConfig({ fetch: null, storage: null });
      expect(cfg.nodeUrl).toBe('http://127.0.0.1:9999');
      expect(cfg.features.test).toBe(true);
    } finally {
      delete g.__KOB_CONFIG__;
    }
  });
});

describe('extraIndexerUrls (cross-check indexers)', () => {
  it('default none; taken from config.json / injected / settings, validated, de-duplicated, capped; never from the query', () => {
    expect(mergeConfig().extraIndexerUrls).toEqual([]);
    const c = mergeConfig({ file: { extraIndexerUrls: ['https://b.example/', 'https://b.example', 'http://c.example:8080'] } });
    expect(c.extraIndexerUrls).toEqual(['https://b.example', 'http://c.example:8080']);
    expect(mergeConfig({ file: { extraIndexerUrls: ['ftp://x'] } }).extraIndexerUrls).toEqual([]);
    expect(mergeConfig({ file: { extraIndexerUrls: 'https://b.example' } }).extraIndexerUrls).toEqual([]);
    expect(mergeConfig({ file: { extraIndexerUrls: ['https://1', 'https://2', 'https://3', 'https://4', 'https://5'] } }).extraIndexerUrls).toEqual([]);
    expect(mergeConfig({ injected: { allowQueryOverrides: true }, query: '?extraIndexerUrls=https://evil.example' }).extraIndexerUrls).toEqual([]);
  });
});

describe('fees (dynamic priority fee)', () => {
  it('default: dynamic on, maxRate 1000 sompi per gram, 1 KAS per transaction', () => {
    expect(mergeConfig().fees).toEqual({ dynamic: true, maxRate: 1000, maxFeeKas: 1 });
    expect(DEFAULT_CONFIG.fees).toEqual({ dynamic: true, maxRate: 1000, maxFeeKas: 1 });
  });

  it('every key can be set by file, injected and settings (later wins); keys are independent', () => {
    expect(mergeConfig({ file: { fees: { dynamic: false } } }).fees).toEqual({ dynamic: false, maxRate: 1000, maxFeeKas: 1 });
    expect(mergeConfig({ file: { fees: { maxRate: 400, maxFeeKas: 0.25 } }, injected: { fees: { maxRate: 600 } }, settings: { fees: { dynamic: 'off' } } }).fees)
      .toEqual({ dynamic: false, maxRate: 600, maxFeeKas: 0.25 });
    expect(mergeConfig({ injected: { fees: { maxRate: '250', maxFeeKas: '0' } } }).fees).toEqual({ dynamic: true, maxRate: 250, maxFeeKas: 0 });
  });

  it('invalid values are ignored with a warning, the lower layer stays', () => {
    const r = resolveConfig({
      file: { fees: { maxRate: 300, maxFeeKas: 2 } },
      injected: { fees: { dynamic: 'maybe', maxRate: 50, maxFeeKas: -1 } },
      settings: { fees: 'high' },
    });
    expect(r.config.fees).toEqual({ dynamic: true, maxRate: 300, maxFeeKas: 2 });
    expect(r.warnings.map((w) => w.field).sort()).toEqual(['fees', 'fees.dynamic', 'fees.maxFeeKas', 'fees.maxRate']);
  });

  it('maxRate: whole sompi per gram from the relay floor 100 up to 1,000,000', () => {
    for (const ok of [100, 1000, 1_000_000, '100', ' 250 ']) expect(parseMaxRate(ok), String(ok)).not.toBeNull();
    for (const bad of [99, 99.5, 100.5, 1_000_001, -5, Number.NaN, Infinity, '1e3', '12.5', '', null, undefined, true, {}]) expect(parseMaxRate(bad), String(bad)).toBeNull();
  });

  it('maxFeeKas: 0 (no cap) to 1000 KAS, at most 8 decimals', () => {
    for (const ok of [0, 1, 0.5, 0.00000001, 1000, '2.5', '0']) expect(parseMaxFeeKas(ok), String(ok)).not.toBeNull();
    for (const bad of [-0.1, 1000.01, 0.000000001, Number.NaN, Infinity, '1e2', 'abc', '', null, undefined, false]) expect(parseMaxFeeKas(bad), String(bad)).toBeNull();
  });

  it('a link cannot change the fee policy (no query key)', () => {
    expect(queryLayer('?fees=off&maxRate=100')).toEqual({});
  });

  it('saveSettings stores a valid fees object (merging keys) and drops invalid ones', () => {
    const st = new MemStorage();
    expect(saveSettings({ fees: { dynamic: false } }, st)).toBe(true);
    expect(saveSettings({ fees: { maxRate: 500, maxFeeKas: 3 } }, st)).toBe(true);
    expect(readSettings(st)).toEqual({ fees: { dynamic: false, maxRate: 500, maxFeeKas: 3 } });
    expect(saveSettings({ fees: { maxRate: 5 } }, st)).toBe(true);
    expect(readSettings(st)).toEqual({ fees: { dynamic: false, maxFeeKas: 3 } });
    expect(mergeConfig({ settings: readSettings(st) }).fees).toEqual({ dynamic: false, maxRate: 1000, maxFeeKas: 3 });
  });
});

describe('USD quote tokens and the landing screen', () => {
  const ID = 'ab'.repeat(32);
  it('quoteTokens come from config.json / __KOB_CONFIG__ only', () => {
    expect(resolveConfig({}).config.quoteTokens).toEqual({});
    expect(resolveConfig({ file: { quoteTokens: { [ID.toUpperCase()]: 'USD' } } }).config.quoteTokens).toEqual({ [ID]: 'USD' });
    const r = resolveConfig({ settings: { quoteTokens: { [ID]: 'USD' } } });
    expect(r.config.quoteTokens).toEqual({});
    expect(r.warnings.some((w) => w.field === 'quoteTokens')).toBe(true);
    const bad = resolveConfig({ file: { quoteTokens: { [ID]: 'EUR' } } });
    expect(bad.config.quoteTokens).toEqual({});
    expect(bad.warnings.some((w) => w.field === 'quoteTokens')).toBe(true);
  });
  it('home: auto by default, validated', () => {
    expect(resolveConfig({}).config.home).toBe('auto');
    expect(resolveConfig({ file: { home: 'list' } }).config.home).toBe('list');
    expect(resolveConfig({ file: { home: `usd:${ID}` } }).config.home).toBe(`usd:${ID}`);
    expect(resolveConfig({ settings: { home: 'kas-usd' } }).config.home).toBe('kas-usd');
    const r = resolveConfig({ file: { home: 'usd:nope' } });
    expect(r.config.home).toBe('auto');
    expect(r.warnings.some((w) => w.field === 'home')).toBe(true);
  });
});

describe('block explorer links (kaspa.stream)', () => {
  const TX = 'ab'.repeat(32);
  it('the default follows the network: tn10.kaspa.stream on testnet-10, kaspa.stream on mainnet', () => {
    expect(explorerTxUrl(resolveConfig({ file: { network: 'testnet-10' } }).config, TX)).toBe(`https://tn10.kaspa.stream/transactions/${TX}`);
    expect(explorerTxUrl(resolveConfig({ file: { network: 'mainnet' } }).config, TX)).toBe(`https://kaspa.stream/transactions/${TX}`);
    expect(explorerBase(resolveConfig({}).config)).toBe('https://kaspa.stream');
  });
  it('explorerUrl overrides the base (config.json / __KOB_CONFIG__ only) and bad ids give no link', () => {
    const c = resolveConfig({ file: { network: 'testnet-10', explorerUrl: 'https://explorer.example/' } }).config;
    expect(explorerTxUrl(c, TX.toUpperCase())).toBe(`https://explorer.example/transactions/${TX}`);
    const r = resolveConfig({ settings: { explorerUrl: 'https://evil.example' } });
    expect(r.config.explorerUrl).toBe('');
    expect(r.warnings.some((w) => w.field === 'explorerUrl')).toBe(true);
    expect(resolveConfig({ file: { explorerUrl: 'ws://x' } }).warnings.some((w) => w.field === 'explorerUrl')).toBe(true);
    expect(explorerTxUrl(c, 'xyz')).toBeNull();
    expect(explorerTxUrl(c, null)).toBeNull();
  });
});

describe('stored node / indexer overrides', () => {
  it('a node or indexer saved in this browser that differs from the deployment is reported', () => {
    const r = resolveConfig({ file: { indexerUrl: 'https://idx.example' }, settings: { indexerUrl: 'https://other.example', nodeUrl: 'wss://node.example' } });
    expect(r.storedOverrides).toEqual([
      { field: 'indexerUrl', value: 'https://other.example', deployment: 'https://idx.example' },
      { field: 'nodeUrl', value: 'wss://node.example', deployment: '' },
    ]);
  });

  it('nothing is reported for the same value, no stored value, or a value a link replaced', () => {
    expect(resolveConfig({ file: { indexerUrl: 'https://idx.example' }, settings: { indexerUrl: 'https://idx.example' } }).storedOverrides).toEqual([]);
    expect(resolveConfig({ file: { indexerUrl: 'https://idx.example' } }).storedOverrides).toEqual([]);
    const linked = resolveConfig({ file: { allowQueryOverrides: true }, settings: { indexerUrl: 'https://other.example' }, query: '?indexer=https://q.example' });
    expect(linked.config.indexerUrl).toBe('https://q.example');
    expect(linked.storedOverrides).toEqual([]);
  });
});
