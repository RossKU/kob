// Playwright configuration of the GitHub Pages mirror check (e2e/ghpages, docs/ops/web-hosting.md "GitHub Pages mirror").
//   webServer 1: the mock indexer + node        node mock/server.mjs                         (KOB_MOCK_PORT, default 8790)
//   webServer 2: web/dist under /kob/           node e2e/serve-static.mjs --dist /kob/       (KOB_GHPAGES_PORT, default 4176)
// The static server sends no headers beyond the content type, as GitHub Pages: the page has only its <meta> CSP.
// Build first, WITH the flag (nothing here builds):   KOB_MIRROR=github-pages npm run build:release   then   npm run e2e:ghpages
import { defineConfig, devices } from '@playwright/test';

const MOCK_PORT = Number(process.env.KOB_MOCK_PORT ?? 8790);
const PORT = Number(process.env.KOB_GHPAGES_PORT ?? 4176);
const chromePath = process.env.KOB_CHROME_PATH && process.env.KOB_CHROME_PATH !== 'cache' ? process.env.KOB_CHROME_PATH : undefined;

// the fixtures read the mock server's address from here (worker processes inherit the environment)
process.env.KOB_MOCK_URL = process.env.KOB_MOCK_URL ?? `http://127.0.0.1:${MOCK_PORT}`;

const baseURL = `http://127.0.0.1:${PORT}`;

export default defineConfig({
  testDir: './e2e/ghpages',
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
  webServer: [
    {
      command: `node mock/server.mjs --port ${MOCK_PORT}`,
      url: `http://127.0.0.1:${MOCK_PORT}/v1/health`,
      reuseExistingServer: !process.env.CI,
      timeout: 60_000,
    },
    {
      command: `node e2e/serve-static.mjs --port ${PORT} --dist /kob/`,
      url: `${baseURL}/kob/`,
      reuseExistingServer: !process.env.CI,
      timeout: 60_000,
    },
  ],
});
