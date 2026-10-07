// The further indexers that cross-check prices and the market start are compared with the primary one by a normalised form: the same
// indexer spelled differently (upper case, its default port, a trailing slash) is not an independent verifier.
import { describe, expect, it } from 'vitest';
import { independentIndexers } from './services';
import { indexerKey, resolveConfig } from '../config';

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
