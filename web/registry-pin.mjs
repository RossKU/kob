// Build-time pin of the DEFAULT token registry: sha256 of the exact bytes of registry/tokens.json at the repository root (vite `define`
// __KOB_DEFAULT_REGISTRY_SHA256__, see vite.config.ts / vitest.config.ts). The app compares the sha256 of the registry it actually loaded with this
// value: a third-party build or deployment that swaps the list is visible ("non-default registry", never shown as KOB-official).
import { createHash } from 'node:crypto';
import { existsSync, readFileSync } from 'node:fs';
import { fileURLToPath } from 'node:url';

export const DEFAULT_REGISTRY_PATH = fileURLToPath(new URL('../registry/tokens.json', import.meta.url));

/** sha256 hex of registry/tokens.json, or '' when the file is not there (a build outside the repository: every registry is then non-default). */
export function defaultRegistrySha256() {
  return existsSync(DEFAULT_REGISTRY_PATH) ? createHash('sha256').update(readFileSync(DEFAULT_REGISTRY_PATH)).digest('hex') : '';
}
