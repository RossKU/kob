// Release build helper (`npm run build:release`): copies the default registry (../registry/tokens.json) into dist/registry/tokens.json, the app's
// default `registryUrl`, so the built app is self-contained. A plain `npm run build` leaves the registry to the host (the soak's UI server serves its
// own TN10 registry there).
import { createHash } from 'node:crypto';
import { copyFileSync, existsSync, mkdirSync, readFileSync } from 'node:fs';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';
import { DEFAULT_REGISTRY_PATH } from '../registry-pin.mjs';

const WEB = join(dirname(fileURLToPath(import.meta.url)), '..');
const DIST = join(WEB, 'dist');
if (!existsSync(join(DIST, 'index.html'))) throw new Error('dist/index.html is missing: run vite build first');
if (!existsSync(DEFAULT_REGISTRY_PATH)) throw new Error(`${DEFAULT_REGISTRY_PATH} is missing`);
const sha = createHash('sha256').update(readFileSync(DEFAULT_REGISTRY_PATH)).digest('hex');
mkdirSync(join(DIST, 'registry'), { recursive: true });
copyFileSync(DEFAULT_REGISTRY_PATH, join(DIST, 'registry', 'tokens.json'));
console.log(`dist/registry/tokens.json = registry/tokens.json (sha256 ${sha})`);
