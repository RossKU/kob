// Playwright configuration of the mock e2e stack (see e2e/README.md).
//   webServer 1: the mock indexer + node       node mock/server.mjs           (KOB_MOCK_PORT, default 8790; skipped with KOB_MOCK_PER_WORKER=1)
//   webServer 2: the app under test            npm run preview   -> dist/     (KOB_E2E_TARGET=app, default when dist/index.html exists)
//                or the infra placeholder page  node e2e/serve-static.mjs      (KOB_E2E_TARGET=infra, default while there is no dist/)
// The browser is the Chromium already in Playwright's cache (no downloads). If Playwright cannot resolve it, set KOB_CHROME_PATH to a chrome.exe.
import { defineConfig, devices } from '@playwright/test';
import { existsSync, readdirSync } from 'node:fs';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';

const HERE = dirname(fileURLToPath(import.meta.url));

const MOCK_PORT = Number(process.env.KOB_MOCK_PORT ?? 8790);
const APP_PORT = Number(process.env.KOB_APP_PORT ?? 4173);
const INFRA_PORT = Number(process.env.KOB_INFRA_PORT ?? 4174);
const perWorker = process.env.KOB_MOCK_PER_WORKER === '1';
const target: 'app' | 'infra' = process.env.KOB_E2E_TARGET === 'app' || process.env.KOB_E2E_TARGET === 'infra' ? process.env.KOB_E2E_TARGET : existsSync(join(HERE, 'dist', 'index.html')) ? 'app' : 'infra';

/** The newest Chromium in the local Playwright cache (used only when KOB_CHROME_PATH is set to `cache`). */
function cachedChromium(): string | undefined {
  const root = process.env.PLAYWRIGHT_BROWSERS_PATH ?? join(process.env.LOCALAPPDATA ?? '', 'ms-playwright');
  if (!existsSync(root)) return undefined;
  const dirs = readdirSync(root).filter((d) => /^chromium-\d+$/.test(d)).sort();
  for (const d of dirs.reverse()) {
    for (const sub of ['chrome-win64', 'chrome-win', 'chrome-linux', 'chrome-mac']) {
      for (const exe of ['chrome.exe', 'chrome', 'Chromium.app/Contents/MacOS/Chromium']) {
        const p = join(root, d, sub, exe);
        if (existsSync(p)) return p;
      }
    }
  }
  return undefined;
}

const chromePath = process.env.KOB_CHROME_PATH === 'cache' ? cachedChromium() : process.env.KOB_CHROME_PATH || undefined;

// the fixtures read the shared mock server's address from here (worker processes inherit the environment)
process.env.KOB_MOCK_URL = process.env.KOB_MOCK_URL ?? `http://127.0.0.1:${MOCK_PORT}`;

const baseURL = target === 'app' ? `http://127.0.0.1:${APP_PORT}` : `http://127.0.0.1:${INFRA_PORT}`;

export default defineConfig({
  testDir: './e2e',
  testMatch: target === 'infra' ? 'infra/**/*.spec.ts' : '**/*.spec.ts',
  testIgnore: target === 'app' ? ['infra/**'] : undefined,
  timeout: 60_000,
  expect: { timeout: 10_000 },
  // the shared mock server holds global state: one worker unless every worker gets its own server
  workers: perWorker ? undefined : 1,
  fullyParallel: perWorker,
  retries: process.env.CI ? 1 : 0,
  reporter: process.env.CI ? [['list'], ['html', { open: 'never' }]] : [['list']],
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
    ...(perWorker
      ? []
      : [
          {
            command: `node mock/server.mjs --port ${MOCK_PORT}`,
            url: `http://127.0.0.1:${MOCK_PORT}/v1/health`,
            reuseExistingServer: !process.env.CI,
            timeout: 60_000,
          },
        ]),
    target === 'app'
      ? {
          // build first: preview serves dist/, and a stale dist silently tests old code (KOB_E2E_NO_BUILD=1 skips it)
          command: `${process.env.KOB_E2E_NO_BUILD ? '' : 'npm run build && '}npm run preview -- --port ${APP_PORT} --strictPort --host 127.0.0.1`,
          url: baseURL,
          reuseExistingServer: !process.env.CI,
          timeout: 180_000,
        }
      : {
          command: `node e2e/serve-static.mjs --port ${INFRA_PORT}`,
          url: baseURL,
          reuseExistingServer: !process.env.CI,
          timeout: 60_000,
        },
  ],
});
