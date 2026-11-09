import { defineConfig } from 'vitest/config';
import { fileURLToPath } from 'node:url';

export default defineConfig({
  define: { __KOB_REGISTRY_TABLE_HASH__: JSON.stringify(''), __KOB_MIRROR__: JSON.stringify(''), __KOB_LANDING_ONLY__: JSON.stringify(false) },
  resolve: { alias: { '@': fileURLToPath(new URL('./src', import.meta.url)) } },
  test: {
    environment: 'node',
    include: ['src/**/*.test.ts', 'test/**/*.test.ts'],
    testTimeout: 60_000,
  },
});
