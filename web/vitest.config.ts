import { defineConfig } from 'vitest/config';
import { fileURLToPath } from 'node:url';
import { defaultRegistrySha256 } from './registry-pin.mjs';

export default defineConfig({
  define: { __KOB_DEFAULT_REGISTRY_SHA256__: JSON.stringify(defaultRegistrySha256()) },
  resolve: { alias: { '@': fileURLToPath(new URL('./src', import.meta.url)) } },
  test: {
    environment: 'node',
    include: ['src/**/*.test.ts', 'test/**/*.test.ts'],
    testTimeout: 60_000,
  },
});
