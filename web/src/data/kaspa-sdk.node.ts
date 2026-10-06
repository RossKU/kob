// Node loader of the official kaspa-wasm SDK (web/vendor/kaspa-node, CommonJS): unit tests, the mock server and e2e harnesses.
import { createRequire } from 'node:module';
import type { KaspaSdk } from './kaspa-sdk';

let cached: KaspaSdk | null = null;

export function loadKaspaSdkNode(): KaspaSdk {
  if (!cached) {
    const require = createRequire(import.meta.url);
    cached = require('../../vendor/kaspa-node/kaspa.js') as KaspaSdk;
  }
  return cached;
}
