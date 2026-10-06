// "official" = listed in the registry this UI ships / pins. A registry that is not byte-identical to the build-pinned default is "custom".
import { readFileSync } from 'node:fs';
import { fileURLToPath } from 'node:url';
import { describe, expect, it } from 'vitest';
import { createServices } from '../app/services';
import { mergeConfig } from '../config';
import { buildTokenRows, tokenBadges, tokenLabel } from '../ui/market/token-model';
import { displayName, parseRegistry } from './registry';
import { DEFAULT_REGISTRY_SHA256, identifyRegistry, markCustomRegistry, registrySourceKind } from './registry-source';
import { sha256Hex } from './sha256';
import { loadKobNode } from './wasm.node';
import { officialGenesis, tradableRegistryJson } from '../testing/chain-fixtures';

const kob = loadKobNode();
const REPO_REGISTRY = fileURLToPath(new URL('../../../registry/tokens.json', import.meta.url));
const shippedBytes = new Uint8Array(readFileSync(REPO_REGISTRY));
const enc = (o: unknown) => new TextEncoder().encode(JSON.stringify(o));

describe('the build pin', () => {
  it('is the sha256 of registry/tokens.json at the repository root', () => {
    expect(DEFAULT_REGISTRY_SHA256).toMatch(/^[0-9a-f]{64}$/);
    expect(DEFAULT_REGISTRY_SHA256).toBe(sha256Hex(shippedBytes));
  });
});

describe('identifyRegistry', () => {
  const reg = parseRegistry(tradableRegistryJson(), { kob });
  it('hashes the exact bytes: the shipped file is the default, one changed byte is not', () => {
    const def = identifyRegistry({ url: './registry/tokens.json', network: 'mainnet', bytes: shippedBytes, registry: reg, schemaVersion: 1 });
    expect(def).toMatchObject({ source: 'bundled', isDefault: true, failed: false, sha256: DEFAULT_REGISTRY_SHA256, tokens: reg.tokens.length, schemaVersion: 1 });
    expect(def.shortHash).toBe(DEFAULT_REGISTRY_SHA256.slice(0, 8));
    const changed = new Uint8Array(shippedBytes.length + 1);
    changed.set(shippedBytes);
    changed[shippedBytes.length] = 0x20; // a trailing space
    expect(identifyRegistry({ url: './registry/tokens.json', network: 'mainnet', bytes: changed, registry: reg }).isDefault).toBe(false);
  });
  it('a registry from an absolute URL is source url; with different content it is non-default', () => {
    expect(registrySourceKind('https://evil.example/tokens.json')).toBe('url');
    expect(registrySourceKind('./registry/tokens.json')).toBe('bundled');
    const id = identifyRegistry({ url: 'https://evil.example/tokens.json', network: 'mainnet', bytes: enc({ x: 1 }), registry: reg });
    expect(id).toMatchObject({ source: 'url', isDefault: false });
  });
  it('a failed load is never the default; without a build pin nothing is the default', () => {
    expect(identifyRegistry({ url: './r.json', network: 'mainnet', bytes: null, registry: reg })).toMatchObject({ failed: true, isDefault: false, sha256: null });
    expect(identifyRegistry({ url: './r.json', network: 'mainnet', bytes: shippedBytes, registry: reg, defaultSha256: '' }).isDefault).toBe(false);
  });
});

describe('a custom registry is never shown as KOB-official', () => {
  const raw = JSON.parse(JSON.stringify(tradableRegistryJson()));
  Object.assign(raw.tokens[0], { official: true }, officialGenesis());
  const parsed = parseRegistry(raw, { kob });
  const custom = markCustomRegistry(parsed, 'abcd1234');

  it('the parsed registry says official; the custom marking removes it and labels the token with the registry hash', () => {
    expect(parsed.tokens[0].official).toBe(true);
    expect(displayName(parsed.tokens[0])).toContain('[official]');
    const t0 = custom.tokens[0];
    expect(t0.official).toBe(false);
    expect(t0.customRegistry).toBe('abcd1234');
    expect(displayName(t0)).toContain('[listed in custom registry abcd1234]');
    expect(displayName(t0)).not.toMatch(/official|verified\]/);
    expect(custom.custom).toEqual({ shortHash: 'abcd1234' });
    expect(custom.byCovenantId.get(t0.covenantId)).toBe(t0);
  });

  it('badges and label state read "custom registry"; an indexer standing cannot bring official back', () => {
    const t0 = custom.tokens[0];
    expect(tokenBadges(t0).map((b) => b.kind)[0]).toBe('custom-registry');
    expect(tokenBadges(t0, 'official').map((b) => b.kind)[0]).toBe('custom-registry');
    const row = buildTokenRows(custom, [{ ticker: t0.ticker, covenant_id: t0.covenantId, template_hash: null, extension_commitment: null, scale: null, decimals: null, open_asks: 0, open_bids: 0, standing: 'official' }])[0];
    expect(row.labelState).toBe('custom');
    expect(row.customHash).toBe('abcd1234');
    expect(row.badges.map((b) => b.kind)).not.toContain('official');
    // delisted still wins
    expect(tokenBadges({ ...t0, status: 'delisted' })[0].kind).toBe('delisted');
    expect(tokenLabel(row, 'x')).toContain('[x]');
  });
});

describe('createServices identifies the registry it loaded', () => {
  const stubs = {
    kob,
    sdk: {} as never,
    node: { kind: 'http-mock', connect: async () => ({ network: 'mainnet', virtualDaaScore: '0' }), disconnect: async () => undefined, getClock: async () => ({ daa: 0n, unixSeconds: 0n, rateMilli: null }), getUtxosByAddresses: async () => [], submitTransaction: async () => '' } as never,
    indexer: null,
    feed: null,
  };
  const respond = (bytes: Uint8Array) => (async () => new Response(bytes as BodyInit, { status: 200 })) as typeof fetch;

  it('the shipped registry file (or what a deployment serves byte-identical) is the default; tokens keep their registry standing', async () => {
    const s = await createServices(mergeConfig({}), { ...stubs, fetch: respond(shippedBytes) });
    expect(s.registryIdentity).toMatchObject({ isDefault: true, sha256: DEFAULT_REGISTRY_SHA256, source: 'bundled' });
    expect(s.registry.custom).toBeUndefined();
  });

  it('any other list (config override URL, different bytes) makes the registry custom: no token official, hash in the label', async () => {
    const raw2 = JSON.parse(JSON.stringify(tradableRegistryJson()));
    Object.assign(raw2.tokens[0], { official: true }, officialGenesis());
    const s = await createServices(mergeConfig({ injected: { network: 'testnet-10', registryUrl: 'https://lists.example/tokens.json' } }), { ...stubs, fetch: respond(enc(raw2)) });
    expect(s.registryIdentity).toMatchObject({ isDefault: false, source: 'url', failed: false });
    expect(s.registry.custom?.shortHash).toBe(s.registryIdentity.shortHash);
    expect(s.registry.tokens.every((t) => !t.official && t.customRegistry === s.registryIdentity.shortHash)).toBe(true);
  });

  it('a registry that fails to load is reported as failed, not as default', async () => {
    const s = await createServices(mergeConfig({}), { ...stubs, fetch: (async () => new Response('nope', { status: 404 })) as typeof fetch });
    expect(s.registryError).toMatch(/404/);
    expect(s.registryIdentity).toMatchObject({ failed: true, isDefault: false, tokens: 0 });
  });
});
