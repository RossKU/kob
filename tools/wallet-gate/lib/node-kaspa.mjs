// Loads the vendored official kaspa-wasm v2.1.0 nodejs build (CommonJS) from ESM.
import { createRequire } from 'node:module';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';
export const ROOT = join(dirname(fileURLToPath(import.meta.url)), '..');
const require = createRequire(import.meta.url);
export const kaspa = require(join(ROOT, 'vendor', 'kaspa-node', 'kaspa.js'));
export default kaspa;
