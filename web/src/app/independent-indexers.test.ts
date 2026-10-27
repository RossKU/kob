// The further indexers that cross-check prices and the market start are compared with the primary one by a normalised form: the same
// indexer spelled differently (upper case, its default port, a trailing slash) is not an independent verifier.
import { describe, expect, it, vi } from 'vitest';
import { createServices, independentIndexers } from './services';
import { DEFAULT_CONFIG, indexerKey, resolveConfig, type AppConfig } from '../config';
import type { KaspaSdk } from '../data/kaspa-sdk';
import type { NodeApi } from '../data/node';
import type { KobWasm } from '../kob/wasm';

describe('indexer URLs are compared normalised', () => {
  it('one indexer in several spellings has one key', () => {
    const k = indexerKey('https://idx.example');
    for (const u of ['https://IDX.example', 'https://idx.example:443', 'https://idx.example/', 'HTTPS://idx.EXAMPLE:443//']) expect(indexerKey(u)).toBe(k);
    expect(indexerKey('https://idx.example:8443')).not.toBe(k);
    expect(indexerKey('http://idx.example')).not.toBe(k);
    expect(indexerKey('https://idx.example/v2')).not.toBe(k);
  });

  it('the primary indexer in another spelling is not a verifier', () => {
    expect(independentIndexers({ indexerUrl: 'https://idx.example', extraIndexerUrls: ['https://IDX.example:443/', 'https://other.example'] })).toEqual(['https://other.example']);
  });

  it('the configured list keeps one entry per indexer', () => {
    const r = resolveConfig({ file: { extraIndexerUrls: ['https://a.example', 'https://A.example:443/', 'https://b.example'] } });
    expect(r.config.extraIndexerUrls).toEqual(['https://A.example:443', 'https://b.example']);
  });
});

describe('createServices: failover between the configured indexers', () => {
  const cfg = (o: Partial<AppConfig>): AppConfig => ({ ...DEFAULT_CONFIG, network: 'testnet-10', registryUrl: 'http://reg.invalid/tokens.json', ...o, features: { ...DEFAULT_CONFIG.features, test: true } });
  const stubs = { kob: {} as KobWasm, sdk: {} as KaspaSdk, node: {} as NodeApi, fetch: (async () => new Response('x', { status: 500 })) as typeof fetch };

  it('further indexers: the indexer is the active one, the verifiers follow it, the feed follows it', async () => {
    const s = await createServices(cfg({ indexerUrl: 'https://main.example', extraIndexerUrls: ['https://b1.example', 'https://MAIN.example:443/', 'https://b2.example'] }), stubs);
    expect(s.failover).not.toBeNull();
    expect(s.failover!.state).toMatchObject({ active: 0, label: 'https://main.example', verifiers: [1, 2] });
    // the primary spelled again is not a backup
    expect(s.verifiers.map((v) => v.label)).toEqual(['https://b1.example', 'https://b2.example']);
    const same = s.verifiers;
    expect(s.verifiers).toBe(same);
    const retarget = vi.spyOn(s.feed!, 'retarget');
    const healthy = { state: 'following', ok: true, network: 'testnet-10', cursor_daa: 1, node_daa: 1, lag_daa: 0, lag_seconds: 0, settle_depth_daa: 100, book_complete: true, payload_version: 4 };
    vi.stubGlobal('fetch', async (url: string) => {
      if (url.startsWith('https://main.example')) throw new Error('refused');
      return new Response(JSON.stringify(url.includes('/v1/health') ? healthy : { tokens: [] }), { status: 200, headers: { 'content-type': 'application/json' } });
    });
    try {
      // the primary is down: the first backup becomes the indexer, the feed follows it, the verifiers are the others
      s.failover!.report(0, { health: null, error: new Error('down') });
      await vi.waitFor(() => expect(s.failover!.state.active).toBe(1));
      expect(retarget).toHaveBeenLastCalledWith('https://b1.example');
      expect(s.verifiers.map((v) => v.label)).toEqual(['https://b2.example']);
      expect(s.pinnedIndexer()).not.toBe(s.indexer);
      expect(await s.indexer!.tokens()).toEqual([]);
    } finally {
      vi.unstubAllGlobals();
      s.failover!.dispose();
    }
  });

  it('no further indexer, or an indexer given by the caller: no failover, the old behaviour', async () => {
    const single = await createServices(cfg({ indexerUrl: 'https://main.example' }), stubs);
    expect(single.failover).toBeNull();
    expect(single.verifiers).toEqual([]);
    const own = {} as never;
    const given = await createServices(cfg({ indexerUrl: 'https://main.example', extraIndexerUrls: ['https://b1.example'] }), { ...stubs, indexer: own });
    expect(given.failover).toBeNull();
    expect(given.indexer).toBe(own);
    expect(given.verifiers.map((v) => v.label)).toEqual(['https://b1.example']);
    const none = await createServices(cfg({ indexerUrl: '', extraIndexerUrls: ['https://b1.example'] }), stubs);
    expect(none.failover).toBeNull();
    expect(none.indexer).toBeNull();
  });
});
