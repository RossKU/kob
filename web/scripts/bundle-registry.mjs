// Release build helper (`npm run build:release`): copies the pinned default registry (../registry/tokens.json, whose sha256 vite embedded as
// __KOB_DEFAULT_REGISTRY_SHA256__) into dist/registry/tokens.json, the app's default `registryUrl`, so the built app is self-contained. A plain
// `npm run build` leaves the registry to the host (the soak's UI server serves its own TN10 registry there). Fails when the copy would not be
// byte-identical to the file the build pinned (the registry changed between the build and this step).
import { createHash } from 'node:crypto';
import { copyFileSync, existsSync, mkdirSync, readdirSync, readFileSync } from 'node:fs';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';
import { DEFAULT_REGISTRY_PATH } from '../registry-pin.mjs';

const WEB = join(dirname(fileURLToPath(import.meta.url)), '..');
const DIST = join(WEB, 'dist');
if (!existsSync(join(DIST, 'index.html'))) throw new Error('dist/index.html is missing: run vite build first');
if (!existsSync(DEFAULT_REGISTRY_PATH)) throw new Error(`${DEFAULT_REGISTRY_PATH} is missing`);
const sha = createHash('sha256').update(readFileSync(DEFAULT_REGISTRY_PATH)).digest('hex');
// the bundle must pin exactly this file (vite `define` wrote the hash into the app's JavaScript)
const assets = readdirSync(join(DIST, 'assets')).filter((f) => f.endsWith('.js'));
if (!assets.some((f) => readFileSync(join(DIST, 'assets', f), 'utf8').includes(sha))) {
  throw new Error(`the built app does not pin registry sha256 ${sha}: rebuild (the registry changed after vite build?)`);
}
mkdirSync(join(DIST, 'registry'), { recursive: true });
copyFileSync(DEFAULT_REGISTRY_PATH, join(DIST, 'registry', 'tokens.json'));
console.log(`dist/registry/tokens.json = registry/tokens.json (sha256 ${sha})`);
