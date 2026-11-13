// Node loader of kob-wasm. It uses the wasm-bindgen `--target nodejs` bindings copied to
// web/wasm/node, and serves unit tests, the mock server and the e2e harnesses.
import { createRequire } from 'node:module';
import { createKob, type KobWasm, type RawKobWasm } from './wasm';

let cached: KobWasm | null = null;

/** Loads kob-wasm once in node and verifies the embedded templates with `selfCheck`. */
export function loadKobNode(): KobWasm {
  if (!cached) {
    const require = createRequire(import.meta.url);
    const raw = require('../../wasm/node/kob_wasm.js') as RawKobWasm;
    cached = createKob(raw);
    cached.selfCheck();
  }
  return cached;
}
