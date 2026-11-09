// Playwright configuration of the landing-only build check (e2e/landing, docs/ops/web-hosting.md "The mainnet front page").
//   one web server: the landing-only build (KOB_LANDING_ONLY=1, built into dist-landing/ so web/dist stays the app's) at /   node e2e/serve-static.mjs --dist / --dir dist-landing
// No mock stack: the page must not talk to any indexer or node, and the spec checks that it does not. Builds first (KOB_E2E_NO_BUILD=1 skips it).
import { defineConfig, devices } from '@playwright/test';

const PORT = Number(process.env.KOB_LANDING_PORT ?? 4177);
const chromePath = process.env.KOB_CHROME_PATH && process.env.KOB_CHROME_PATH !== 'cache' ? process.env.KOB_CHROME_PATH : undefined;
const baseURL = `http://127.0.0.1:${PORT}`;

export default defineConfig({
  testDir: './e2e/landing',
  testMatch: '**/*.spec.ts',
  timeout: 60_000,
  expect: { timeout: 10_000 },
  workers: 1,
  retries: process.env.CI ? 1 : 0,
  reporter: process.env.CI ? [['list'], ['github'], ['html', { open: 'never' }]] : [['list']],
  outputDir: 'test-results',
  use: {
    baseURL,
    trace: 'retain-on-failure',
    screenshot: 'only-on-failure',
    ...devices['Desktop Chrome'],
    launchOptions: chromePath ? { executablePath: chromePath } : {},
  },
  projects: [{ name: 'chromium' }],
  webServer: {
    command: `${process.env.KOB_E2E_NO_BUILD ? '' : 'npm run build -- --outDir dist-landing && '}node e2e/serve-static.mjs --port ${PORT} --dist / --dir dist-landing`,
    url: baseURL,
    env: { KOB_LANDING_ONLY: '1', KOB_NETWORK: 'mainnet' },
    reuseExistingServer: !process.env.CI,
    timeout: 180_000,
  },
});
