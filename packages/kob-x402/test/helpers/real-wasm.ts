// The real kob-wasm node build, or a skip when it has not been built (scripts/build-wasm.sh sets KOB_REQUIRE_WASM=1
// for --test, which turns the skip into a failure).

import { existsSync, readFileSync } from 'node:fs';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';
import { createKobWasm } from '../../src/wasm-node.ts';
import type { KobWasmNode } from '../../src/wasm-node.ts';

const here = dirname(fileURLToPath(import.meta.url));
const pkg = join(here, '../../../../crates/kob-wasm/pkg-node');
export const GOLDEN = join(here, '../../../../crates/kob-x402/vectors/golden/x402-payments.json');

export const wasmBuilt = existsSync(join(pkg, 'kob_wasm.js'));
if (!wasmBuilt && process.env.KOB_REQUIRE_WASM === '1') throw new Error(`kob-wasm is not built at ${pkg} (scripts/build-wasm.sh)`);

export const skipUnlessWasm = wasmBuilt ? false : 'kob-wasm is not built (scripts/build-wasm.sh)';

/** `undefined` (tests skip) when the wasm build is absent. */
export function realWasm(): KobWasmNode | undefined {
  return wasmBuilt ? createKobWasm({ pkgDir: pkg }) : undefined;
}

export interface GoldenCase {
  name: string;
  function: string;
  request: any;
  expected: any;
  signatureHeader?: string;
}

export function loadGolden(): GoldenCase[] {
  return (JSON.parse(readFileSync(GOLDEN, 'utf8')) as { cases: GoldenCase[] }).cases;
}
