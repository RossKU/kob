// REAL-wallet run on Kaspa testnet-10: drives the built KOB web app with the released KasWare / Kaspire / Kastle extensions against a TN10 node and
// proves, per wallet: issue a 3 / 3 token, then place and cancel the main order types (limit GTC sell + buy, amend, IOC, stop, OCO, IFD, pair limit
// sell + buy, market against a counterparty when an indexer is configured), merge the token's UTXOs and check My orders / Balances; every accepted
// transaction is verified on the node independently of the app.
//
//   node e2e-real/run.mjs [--wallet kasware,kaspire,kastle|all] [--only limit,amend,...] [--indexer [url]] [--headless] [--keep-open]
//                         [--skip-issue] [--rebuild] [--port N]
//   node e2e-real/run.mjs --mock [--indexer] ...      the same scenarios against the offline mock stack (web/e2e) with in-page mock wallets
//
// Exit codes: 0 every selected scenario passed (or was skipped with a reason), 1 a scenario failed, 77 skipped (no .env / extension cache / node
// unreachable / no build). On demand only: never part of CI. Prerequisites and results: e2e-real/README.md. Secrets (mnemonics, keys) live in the
// gitignored e2e-real/.env and are never printed.
import { existsSync, readFileSync, mkdirSync, rmSync } from 'node:fs';
import { join } from 'node:path';
import { spawnSync } from 'node:child_process';
import { chromium, OUT_DIR, WEB_ROOT, ENV_PATH, ensureExtension, ensureMnemonic, launchWalletBrowser, loadEnv, log, shot, sleep } from './common.mjs';
import { fund, TN10_NODE } from './fund.mjs';
import { startAppServer, DIST_REAL, REGISTRY_PATH } from './server.mjs';
import { KAS, NodeChecker, WALLET_LABELS, connectWallet, kasBalance, openApp, retryOnce, walletDriver, writeJson } from './driver.mjs';
import { MOCK_KEY_OF, MockNodeChecker, startMock } from './mock.mjs';
import { SCENARIOS, buildTestRegistry, issueTokens, snapshotTracker, sweepOpenOrders } from './scenarios.mjs';

export const SKIP = 77;
const argv = process.argv.slice(2);
const flag = (n) => argv.includes('--' + n);
const optv = (n, d) => {
  const i = argv.indexOf('--' + n);
  return i >= 0 && argv[i + 1] && !argv[i + 1].startsWith('--') ? argv[i + 1] : d;
};
const MOCK = flag('mock');
const ALL_WALLETS = ['kasware', 'kaspire', 'kastle'];
const WALLETS = optv('wallet', 'all') === 'all' ? ALL_WALLETS : optv('wallet', 'all').split(',');
const ALL_IDS = SCENARIOS.map(([id]) => id);
const ONLY = optv('only', ALL_IDS.join(',')).split(',');
const HEADLESS = flag('headless') || MOCK; // the mock wallets need no extension: always headless
const KEEP_OPEN = flag('keep-open');
const SKIP_ISSUE = flag('skip-issue');
const FUND_BELOW = 120n * KAS; // two issuances leave 10 KAS each in their token UTXOs; an IFD locks about 17 KAS while it is open
const FUND_KAS = 100;

for (const w of WALLETS) if (!ALL_WALLETS.includes(w)) throw new Error(`unknown wallet ${w} (kasware, kaspire, kastle)`);
for (const id of ONLY) if (!ALL_IDS.includes(id)) throw new Error(`unknown scenario ${id} (${ALL_IDS.join(', ')})`);
if (MOCK && SKIP_ISSUE) throw new Error('--skip-issue reuses tokens of an earlier TN10 run: not with --mock (the mock chain starts empty)');

const skip = (why) => {
  console.log(`SKIP: ${why}`);
  process.exit(SKIP);
};

/** The indexer the run uses: `--indexer <url>`, INDEXER_URL of .env, or none (node-only mode, as the TN10 runs so far). `--mock --indexer`: the mock's. */
function indexerOf(env, mock) {
  if (!flag('indexer') && !env.INDEXER_URL) return '';
  if (mock) return mock.url;
  return optv('indexer', env.INDEXER_URL ?? '');
}

async function preflight(env) {
  if (!existsSync(join(WEB_ROOT, 'vendor', 'kaspa-node', 'kaspa.js'))) skip('web/vendor/kaspa-node is missing (npm run fetch-sdk)');
  if (MOCK) {
    if (!existsSync(join(WEB_ROOT, 'wasm', 'node', 'kob_wasm.js'))) skip('web/wasm/node is missing (npm run build:wasm -- --copy): the mock node needs kob-wasm');
    return null;
  }
  if (!existsSync(ENV_PATH)) skip('web/e2e-real/.env is missing (DEV key, node URL and wallet mnemonics live there, gitignored)');
  for (const k of ['DEV_PRIVATE_KEY', 'DEV_ADDRESS']) if (!env[k]) skip(`${k} is not set in web/e2e-real/.env`);
  for (const w of WALLETS) {
    // a throw-away mnemonic is generated into .env on first use (never printed); the run funds the wallet from the DEV key
    const key = `WALLET_MNEMONIC_${w.toUpperCase()}`;
    if (!env[key]) env[key] = ensureMnemonic(key);
    try {
      await ensureExtension(w, { download: false });
    } catch (e) {
      skip(`${w} extension cache not found (${e.message}); copy it to web/e2e-real/.ext/${w} or run tools/wallet-gate first`);
    }
  }
  const node = new NodeChecker(env.NODE_WS);
  try {
    await Promise.race([node.ping(), sleep(30_000).then(() => Promise.reject(new Error('timeout')))]);
  } catch (e) {
    skip(`TN10 node ${env.NODE_WS || TN10_NODE} is unreachable (${String(e.message ?? e).slice(0, 120)})`);
  }
  return node;
}

function ensureBuild() {
  const idx = join(DIST_REAL, 'index.html');
  if (!flag('rebuild') && existsSync(idx)) return;
  log('building the app into dist-real ...');
  const r = spawnSync('npx', ['vite', 'build', '--outDir', 'dist-real', '--emptyOutDir'], { cwd: WEB_ROOT, shell: true, encoding: 'utf8' });
  if (r.status !== 0 || !existsSync(idx)) skip(`the app build failed: ${(r.stdout + r.stderr).slice(-400)}`);
}

// ------------------------------------------------------------------------------------------------ browsers

/** The browser context of one wallet: the extension in a fresh persistent profile (TN10), or a plain context with the mock wallet. */
async function walletContext(name, drv, mockBrowser) {
  if (MOCK) {
    const ctx = await mockBrowser.newContext({ viewport: { width: 1280, height: 900 } });
    await drv.setup(ctx);
    return { ctx, ext: { version: 'mock' } };
  }
  const ext = await ensureExtension(name, { download: false });
  const ctx = await launchWalletBrowser({ extensionDir: ext.dir, profileName: name, headless: HEADLESS, fresh: true });
  log(`[${name}] onboarding ${drv.label} ${ext.version}`);
  await drv.setup(ctx);
  return { ctx, ext };
}

/**
 * The counterparty of the market scenario: the DEV key (TN10) or the mock `maker` key, driven through the same app in its OWN browser with an
 * in-page signer (src/testing/mock-wallets.ts, enabled by `features.test`, which only an injected config can switch on). Test money only.
 */
function counterpartyFactory({ env, mock, server, appConfig, mockBrowser }) {
  const secret = MOCK ? mock.secrets[MOCK_KEY_OF.dev] : env.DEV_PRIVATE_KEY;
  if (!secret || !/^[0-9a-fA-F]{64}$/.test(secret)) return null;
  return async () => {
    const browser = mockBrowser ?? (await chromium.launch({ headless: true, executablePath: process.env.KOB_CHROME_PATH || undefined }));
    const ctx = await browser.newContext({ viewport: { width: 1280, height: 900 } });
    const drv = walletDriver('kasware', env, { mock: { secretKey: secret.toLowerCase() } });
    drv.label = 'counterparty (DEV key, in-page signer)';
    await drv.setup(ctx);
    if (MOCK) await mock.giveKas(MOCK_KEY_OF.dev, 50, 2);
    const h = await openApp(ctx, server.url, { ...appConfig, features: { ...appConfig.features, test: true } });
    await connectWallet(h, ctx, drv);
    return {
      ctx, drv, h,
      async close() {
        await ctx.close().catch(() => {});
        if (!mockBrowser) await browser.close().catch(() => {});
      },
    };
  };
}

// ------------------------------------------------------------------------------------------------ one wallet

async function runWallet(name, env, node, server, { mock, mockBrowser, indexerUrl }) {
  const drv = MOCK ? walletDriver(name, env, { mock: { secretKey: mock.secrets[MOCK_KEY_OF[name]] } }) : walletDriver(name, env);
  const result = { wallet: drv.label, mode: MOCK ? 'mock' : 'tn10', indexer: indexerUrl || null, startedAt: new Date().toISOString(), address: null, scenarios: {}, steps: [] };
  rmSync(join(OUT_DIR, 'screens', name), { recursive: true, force: true });
  const { ctx, ext } = await walletContext(name, drv, mockBrowser);
  result.walletVersion = ext.version;
  const appConfig = {
    network: 'testnet-10',
    indexerUrl,
    nodeUrl: MOCK ? mock.url : env.NODE_WS || TN10_NODE,
    registryUrl: REGISTRY_PATH,
    // kastle is optional in the app; `test` exposes the SDK to the in-page mock wallets (only in --mock)
    features: { kastle: true, test: MOCK },
  };
  let rc = null;
  try {
    if (MOCK) await mock.giveKas(MOCK_KEY_OF[name], 100, 4);
    let h = await openApp(ctx, server.url, appConfig);
    const conn = await connectWallet(h, ctx, drv);
    result.address = conn.address;
    result.connect = { network: conn.network, shownAddress: conn.shownAddress, popups: conn.popups.map((p) => ({ url: p.url, summary: p.result?.summary ?? p.result?.text, error: p.error })) };
    if (conn.network !== 'testnet-10') throw new Error(`the app shows network ${conn.network}, expected testnet-10`);
    if (drv.expectedAddress && conn.address !== drv.expectedAddress) throw new Error('the wallet address differs from the one derived from the mnemonic / key');
    log(`[${name}] connected ${conn.address.slice(0, 16)}... network ${conn.network}`);

    // fund the wallet when it is low (the mock wallet got its KAS before the app opened)
    let bal = await kasBalance(node, conn.address);
    if (!MOCK && bal < FUND_BELOW) {
      log(`[${name}] balance ${Number(bal) / 1e8} KAS < ${Number(FUND_BELOW) / 1e8}: funding ${FUND_KAS} KAS from the DEV key`);
      result.fundTxid = await retryOnce('fund', () => fund([{ address: conn.address, kas: FUND_KAS }], { env }));
      bal = await kasBalance(node, conn.address);
    }
    result.balanceKas = Number(bal) / 1e8;

    rc = {
      ctx, drv, node, server, result, name, env, address: conn.address, indexer: !!indexerUrl,
      openCounterparty: counterpartyFactory({ env, mock, server, appConfig, mockBrowser }),
      get h() { return h; }, set h(v) { h = v; },
    };
    const reconnect = async () => {
      await h.page.reload();
      await h.page.getByTestId('main').waitFor({ timeout: 60_000 }).catch(() => {});
      if (!(await h.page.getByTestId('wallet-address').count())) await connectWallet(h, ctx, drv);
    };
    const setRegistry = async () => {
      const reg = buildTestRegistry([rc.token?.registryEntry, rc.tokenB?.registryEntry]);
      server.setRegistry(reg);
      writeJson(`registry-tn10.test.${name}.json`, reg);
      if (MOCK) {
        // the mock (indexer and node in one process) books orders only for tokens it knows: it learns the issued tokens as the real indexer
        // learns them from its registry. A real node needs nothing of the kind.
        for (const t of [rc.token, rc.tokenB].filter(Boolean)) {
          const e = t.registryEntry;
          await mock.registerToken({ ticker: e.ticker, covenant_id: e.covenant_id, program: 'KCC20Ref', extension_commitment: e.extension_commitment, decimals: e.decimals, template_id: e.template_id, standing: 'official' });
        }
      }
      await reconnect();
    };

    // issue the tokens (or reuse the ones of the previous run)
    const withPair = ONLY.includes('pair');
    const tokenFile = MOCK ? null : join(OUT_DIR, `token-${name}.json`);
    if (SKIP_ISSUE) {
      if (!existsSync(tokenFile)) throw new Error(`--skip-issue needs ${tokenFile} from an earlier run`);
      const saved = JSON.parse(readFileSync(tokenFile, 'utf8'));
      if (!saved.token?.tokenId) throw new Error(`${tokenFile} is from the old run format (lots): run once without --skip-issue`);
      rc.token = saved.token;
      rc.tokenB = saved.tokenB ?? null;
      if (withPair && !rc.tokenB) throw new Error('--skip-issue: the saved run has no token B for the pair scenario (run once without --skip-issue)');
      // the fresh browser profile has no tracker: restore the token UTXOs / records the previous run knew (the app re-verifies every one on the node)
      await h.page.evaluate((ls) => { for (const [k, v] of Object.entries(ls ?? {})) localStorage.setItem(k, v); }, saved.trackerLS);
      await setRegistry();
    } else {
      const issued = await issueTokens(rc, { withPair });
      result.scenarios.issue = issued.scenario;
      if (!issued.scenario.ok) throw new Error(`token issuance failed: ${issued.scenario.error}`);
      rc.token = issued.token;
      rc.tokenB = issued.tokenB;
      await snapshotTracker(rc, tokenFile);
      // reload with the generated TN10 registry: the tokens become tradable
      await setRegistry();
    }
    result.tokens = { A: rc.token, B: rc.tokenB ?? null };

    for (const [id, fn] of SCENARIOS) {
      if (!ONLY.includes(id)) continue;
      result.scenarios[id] = await fn(rc);
      await snapshotTracker(rc, tokenFile);
    }
  } catch (e) {
    result.fatal = String(e?.stack ?? e).slice(0, 2000);
    log(`[${name}] FATAL: ${String(e?.message ?? e).slice(0, 400)}`);
    try {
      for (const p of ctx.pages()) await shot(p, name, 'fatal');
    } catch {
      /* ignore */
    }
  } finally {
    if (rc) {
      const swept = await sweepOpenOrders(rc);
      if (swept.length) result.sweptOpenOrders = swept;
      await snapshotTracker(rc, MOCK ? null : join(OUT_DIR, `token-${name}.json`)).catch(() => {});
      rc.h.badResponses.length && (result.badResponses = [...new Set(rc.h.badResponses)].slice(0, 20));
      rc.h.consoleErrors.length && (result.consoleErrors = rc.h.consoleErrors.slice(0, 20));
      rc.h.pageErrors.length && (result.pageErrors = rc.h.pageErrors.slice(0, 20));
      result.finalBalanceKas = Number(await kasBalance(node, rc.address).catch(() => 0n)) / 1e8;
    }
    result.finishedAt = new Date().toISOString();
    writeJson(`real-${MOCK ? 'mock-' : ''}${name}.json`, result);
    if (!KEEP_OPEN || MOCK) await ctx.close().catch(() => {});
    else log(`[${name}] --keep-open: leaving the browser open`);
  }
  return result;
}

// ------------------------------------------------------------------------------------------------ main

function summarize(results) {
  const rows = [];
  for (const r of results) {
    for (const id of ['issue', ...ALL_IDS]) {
      const s = r.scenarios[id];
      if (!s) {
        if (id === 'issue' && SKIP_ISSUE) continue;
        if (id !== 'issue' && !ONLY.includes(id)) continue;
        rows.push({ wallet: r.wallet, scenario: id, result: r.fatal ? 'FAIL (fatal)' : 'NOT RUN', txids: '', note: (r.fatal ?? '').split('\n')[0].slice(0, 140) });
        continue;
      }
      const txids = s.steps.flatMap((x) => (x.links ? x.links.map((l) => l.txid) : [x.txid])).filter(Boolean).map((t) => t.slice(0, 10)).join(' ');
      rows.push({ wallet: r.wallet, scenario: id, result: s.skipped ? 'SKIP' : s.ok ? 'PASS' : 'FAIL', txids, note: s.skipped ?? (s.ok ? '' : String(s.error ?? '').slice(0, 140)) });
    }
  }
  console.log('\n' + ['wallet', 'scenario', 'result', 'txids (first 10 hex)', 'note'].join(' | '));
  for (const r of rows) console.log([r.wallet, r.scenario, r.result, r.txids, r.note].join(' | '));
  return rows;
}

async function main() {
  const env = { ...loadEnv(), ...(process.env.NODE_WS ? { NODE_WS: process.env.NODE_WS } : {}), ...(process.env.INDEXER_URL ? { INDEXER_URL: process.env.INDEXER_URL } : {}) };
  let node = await preflight(env);
  ensureBuild();
  mkdirSync(OUT_DIR, { recursive: true });
  const mock = MOCK ? await startMock() : null;
  if (MOCK) node = await new MockNodeChecker(mock.url).init();
  const indexerUrl = indexerOf(env, mock);
  const server = await startAppServer({ port: Number(optv('port', 0)) });
  const mockBrowser = MOCK ? await chromium.launch({ headless: true, executablePath: process.env.KOB_CHROME_PATH || undefined }) : null;
  log(`app served on ${server.url}${MOCK ? `, mock stack on ${mock.url}` : `, node ${env.NODE_WS || TN10_NODE}`}${indexerUrl ? `, indexer ${indexerUrl}` : ' (node only, no indexer)'}`);
  const results = [];
  try {
    for (const w of WALLETS) {
      log(`==== ${WALLET_LABELS[w]}${MOCK ? ' (mock)' : ''}`);
      results.push(await runWallet(w, env, node, server, { mock, mockBrowser, indexerUrl }));
    }
  } finally {
    await server.close();
    if (mockBrowser) await mockBrowser.close().catch(() => {});
    if (mock) await mock.close();
  }
  const rows = summarize(results);
  writeJson(`real-${MOCK ? 'mock-' : ''}summary.json`, { at: new Date().toISOString(), mode: MOCK ? 'mock' : 'tn10', indexer: indexerUrl || null, rows });
  const failed = rows.some((r) => r.result !== 'PASS' && r.result !== 'SKIP');
  process.exit(failed ? 1 : 0);
}

main().catch((e) => {
  console.error('run failed:', e?.stack ?? e);
  process.exit(1);
});
