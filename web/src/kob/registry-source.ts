// Which token registry is this UI using? ("official" means "listed in the registry this UI ships / pins", nothing more.)
//
// KOB is open source: anyone can build or host a UI with another token list, run an indexer or a facilitator. The app therefore computes the identity
// of the registry it ACTUALLY loaded (source, network, sha256 of the exact JSON bytes, counts) and compares the hash with the DEFAULT hash pinned at
// build time from registry/tokens.json (web/registry-pin.mjs -> vite `define`). Any other registry is a "non-default registry": a persistent warning
// is shown and no token is labelled official / verified by KOB; its tokens read "listed in custom registry <short hash>" instead.
import { sha256Hex } from './sha256';
import type { TokenInfo, TokenRegistry } from './registry';

declare const __KOB_DEFAULT_REGISTRY_SHA256__: string | undefined;

/** sha256 of registry/tokens.json at build time ('' when the build had no such file: then no registry is the default). */
export const DEFAULT_REGISTRY_SHA256: string = typeof __KOB_DEFAULT_REGISTRY_SHA256__ === 'string' ? __KOB_DEFAULT_REGISTRY_SHA256__ : '';

export type RegistrySourceKind = 'bundled' | 'url';

export interface RegistryIdentity {
  /** `bundled`: a path relative to the app (deployed next to it); `url`: an absolute http(s) URL (a config / Settings override) */
  source: RegistrySourceKind;
  url: string;
  network: string;
  /** sha256 of the exact bytes that were parsed; null when the registry could not be loaded */
  sha256: string | null;
  /** first 8 hex characters of `sha256` */
  shortHash: string | null;
  tokens: number;
  templates: number;
  /** the `schema_version` field of the registry JSON */
  schemaVersion: number | null;
  /** the hash pinned at build time ('' when none) */
  defaultSha256: string;
  /** the loaded registry is byte-for-byte the pinned default. Only then may tokens be shown as official / verified by KOB. */
  isDefault: boolean;
  /** the registry failed to load (an empty registry is used) */
  failed: boolean;
}

export const registrySourceKind = (url: string): RegistrySourceKind => (/^[a-z][a-z0-9+.-]*:/i.test(url) ? 'url' : 'bundled');

export interface IdentifyInput {
  url: string;
  network: string;
  /** the exact bytes fetched; null when loading failed */
  bytes: Uint8Array | null;
  registry: Pick<TokenRegistry, 'tokens' | 'templates'>;
  schemaVersion?: number | null;
  defaultSha256?: string;
}

/** Identity of a loaded registry. Pure (the hash is computed here from the bytes; nothing is taken from the registry's own claims). */
export function identifyRegistry(i: IdentifyInput): RegistryIdentity {
  const defaultSha256 = i.defaultSha256 ?? DEFAULT_REGISTRY_SHA256;
  const sha256 = i.bytes ? sha256Hex(i.bytes) : null;
  return {
    source: registrySourceKind(i.url),
    url: i.url,
    network: i.network,
    sha256,
    shortHash: sha256 ? sha256.slice(0, 8) : null,
    tokens: i.registry.tokens.length,
    templates: i.registry.templates.length,
    schemaVersion: i.schemaVersion ?? null,
    defaultSha256,
    isDefault: sha256 !== null && defaultSha256 !== '' && sha256 === defaultSha256,
    failed: i.bytes === null,
  };
}

/**
 * The registry as the rest of the app must see it when it is NOT the pinned default: no token is `official` any more and every token carries
 * `customRegistry` (the short hash), which the badges, labels and the pre-sign screen show instead of official / verified.
 */
export function markCustomRegistry(reg: TokenRegistry, shortHash: string): TokenRegistry {
  const tokens = reg.tokens.map((t): TokenInfo => ({ ...t, official: false, customRegistry: shortHash }));
  return { ...reg, tokens, byCovenantId: new Map(tokens.map((t) => [t.covenantId, t])), custom: { shortHash } };
}
