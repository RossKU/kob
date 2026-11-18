// Node loader of the official kaspa-wasm SDK (web/vendor/kaspa-node, CommonJS), for unit tests,
// the mock server and e2e harnesses.
import { createRequire } from 'node:module';
import type { KaspaSdk } from './kaspa-sdk';

let cached: KaspaSdk | null = null;

/** Loads the SDK's Node build once and returns its namespace. */
export function loadKaspaSdkNode(): KaspaSdk {
  if (!cached) {
    const require = createRequire(import.meta.url);
    cached = require('../../vendor/kaspa-node/kaspa.js') as KaspaSdk;
  }
  return cached;
}
