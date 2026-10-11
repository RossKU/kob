// Path of the DEFAULT token registry: registry/tokens.json at the repository root (copied into dist/registry/ by `npm run build:release`).
import { fileURLToPath } from 'node:url';

export const DEFAULT_REGISTRY_PATH = fileURLToPath(new URL('../registry/tokens.json', import.meta.url));
