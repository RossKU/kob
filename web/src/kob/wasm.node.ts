// Node loader of kob-wasm (wasm-bindgen `--target nodejs` bindings copied to web/wasm/node): unit tests, the mock server, e2e harnesses.
import { createRequire } from 'node:module';
import { createKob, type KobWasm, type RawKobWasm } from './wasm';

let cached: KobWasm | null = null;

export function loadKobNode(): KobWasm {
  if (!cached) {
    const require = createRequire(import.meta.url);
    const raw = require('../../wasm/node/kob_wasm.js') as RawKobWasm;
    cached = createKob(raw);
    cached.selfCheck();
  }
  return cached;
}
