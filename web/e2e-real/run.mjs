// REAL-wallet run on Kaspa testnet-10: drives the built KOB web app with the released KasWare / Kaspire extensions against a TN10 node and
// proves issue -> limit (sell + buy) -> IFD -> OCO, each placed and cancelled, every accepted tx verified on the node independently of the app.
//
//   node e2e-real/run.mjs [--wallet kasware|kaspire|all] [--headless] [--keep-open] [--skip-issue] [--only limit,ifd,oco] [--rebuild] [--port N]
//
// Exit codes: 0 every selected scenario passed, 1 a scenario failed, 77 skipped (no .env / extension cache / node unreachable / no build).
// Prerequisites and results: e2e-real/README.md. Secrets (mnemonics, keys) live in the gitignored e2e-real/.env and are never printed.
import { existsSync, readFileSync, mkdirSync, rmSync } from 'node:fs';
import { join } from 'node:path';
import { spawnSync } from 'node:child_process';
import { EXT_ROOT, OUT_DIR, WEB_ROOT, ENV_PATH, ensureExtension, extensionVersion, launchWalletBrowser, loadEnv, log, shot, sleep } from './common.mjs';
import { fund, TN10_NODE } from './fund.mjs';
import { startAppServer, DIST_REAL, REGISTRY_PATH } from './server.mjs';
import { ACCEPT_TIMEOUT_MS, KAS, NodeChecker, connectWallet, goto, kasBalance, openApp, retryOnce, signOnConfirmScreen, walletDriver, writeJson } from './driver.mjs';
import { issueToken, buildTestRegistry, runLimit, runIfd, runOco, snapshotTracker, sweepOpenOrders } from './scenarios.mjs';

export const SKIP = 77;
const argv = process.argv.slice(2);
const flag = (n) => argv.includes('--' + n);
const optv = (n, d) => {
  const i = argv.indexOf('--' + n);
  return i >= 0 && argv[i + 1] && !argv[i + 1].startsWith('--') ? argv[i + 1] : d;
};
const WALLETS = optv('wallet', 'all') === 'all' ? ['kasware', 'kaspire'] : optv('wallet', 'all').split(',');
const ONLY = optv('only', 'limit,ifd,oco').split(',');
const HEADLESS = flag('headless');
const KEEP_OPEN = flag('keep-open');
const SKIP_ISSUE = flag('skip-issue');
const FUND_BELOW = 80n * KAS; // an IFD locks 41 KAS, the issuance 10 KAS
const FUND_KAS = 60;

const skip = (why) => {
  console.log(`SKIP: ${why}`);
  process.exit(SKIP);
};

async function preflight(env) {
  if (!existsSync(ENV_PATH)) skip('web/e2e-real/.env is missing (DEV key, node URL and wallet mnemonics live there, gitignored)');
  for (const k of ['DEV_PRIVATE_KEY', 'DEV_ADDRESS']) if (!env[k]) skip(`${k} is not set in web/e2e-real/.env`);
  for (const w of WALLETS) {
    const key = w === 'kasware' ? 'WALLET_MNEMONIC_KASWARE' : 'WALLET_MNEMONIC_KASPIRE';
    if (!env[key]) skip(`${key} is not set in web/e2e-real/.env`);
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
  if (!existsSync(join(WEB_ROOT, 'vendor', 'kaspa-node', 'kaspa.js'))) skip('web/vendor/kaspa-node is missing (npm run fetch-sdk)');
  return node;
}

function ensureBuild() {
  const idx = join(DIST_REAL, 'index.html');
  if (!flag('rebuild') && existsSync(idx)) return;
  log('building the app into dist-real ...');
  const r = spawnSync('npx', ['vite', 'build', '--outDir', 'dist-real', '--emptyOutDir'], { cwd: WEB_ROOT, shell: true, encoding: 'utf8' });
  if (r.status !== 0 || !existsSync(idx)) skip(`the app build failed: ${(r.stdout + r.stderr).slice(-400)}`);
}

// ------------------------------------------------------------------------------------------------ one wallet

async function runWallet(name, env, node, server) {
  const drv = walletDriver(name, env);
  const ext = await ensureExtension(name, { download: false });
  const result = { wallet: drv.label, walletVersion: ext.version, startedAt: new Date().toISOString(), address: null, scenarios: {}, steps: [] };
  rmSync(join(OUT_DIR, 'screens', name), { recursive: true, force: true });
  const ctx = await launchWalletBrowser({ extensionDir: ext.dir, profileName: name, headless: HEADLESS, fresh: true });
  try {
    log(`[${name}] onboarding ${drv.label} ${ext.version}`);
    await drv.setup(ctx);
    const registryUrl = REGISTRY_PATH;
    let h = await openApp(ctx, server.url, { registryUrl, nodeUrl: env.NODE_WS || TN10_NODE });
    const conn = await connectWallet(h, ctx, drv);
    result.address = conn.address;
    result.connect = { network: conn.network, shownAddress: conn.shownAddress, popups: conn.popups.map((p) => ({ url: p.url, summary: p.result?.summary ?? p.result?.text, error: p.error })) };
    if (conn.network !== 'testnet-10') throw new Error(`the app shows network ${conn.network}, expected testnet-10`);
    if (drv.expectedAddress && conn.address !== drv.expectedAddress) throw new Error('the wallet address differs from the one derived from the mnemonic');
    log(`[${name}] connected ${conn.address.slice(0, 16)}... network ${conn.network}`);

    // fund the wallet when it is low
    let bal = await kasBalance(node, conn.address);
    if (bal < FUND_BELOW) {
      log(`[${name}] balance ${Number(bal) / 1e8} KAS < ${Number(FUND_BELOW) / 1e8}: funding ${FUND_KAS} KAS from the DEV key`);
      result.fundTxid = await retryOnce('fund', () => fund([{ address: conn.address, kas: FUND_KAS }], { env }));
      bal = await kasBalance(node, conn.address);
    }
    result.balanceKas = Number(bal) / 1e8;

    const ctxRun = { ctx, drv, node, server, result, name, env, address: conn.address, get h() { return h; }, set h(v) { h = v; } };

    // issue a token (or reuse the one of the previous run)
    const tokenFile = join(OUT_DIR, `token-${name}.json`);
    if (SKIP_ISSUE) {
      if (!existsSync(tokenFile)) throw new Error(`--skip-issue needs ${tokenFile} from an earlier run`);
      ctxRun.token = JSON.parse(readFileSync(tokenFile, 'utf8'));
      server.setRegistry(buildTestRegistry(ctxRun.token.registryEntry));
      // the fresh browser profile has no tracker: restore the token UTXOs the previous run knew (the app re-verifies every one on the node)
      await h.page.evaluate((ls) => { for (const [k, v] of Object.entries(ls ?? {})) localStorage.setItem(k, v); }, ctxRun.token.trackerLS);
      await h.page.reload();
      await connectWallet(h, ctx, drv);
    } else {
      const step = await issueToken(ctxRun);
      result.scenarios.issue = step.scenario;
      if (!step.scenario.ok) throw new Error(`token issuance failed: ${step.scenario.error}`);
      ctxRun.token = step.token;
      await snapshotTracker(ctxRun, tokenFile);
      // reload with the generated TN10 registry: the token becomes tradable
      server.setRegistry(buildTestRegistry(ctxRun.token.registryEntry));
      writeJson(`registry-tn10.test.json`, buildTestRegistry(ctxRun.token.registryEntry));
      await h.page.reload();
      await connectWallet(h, ctx, drv);
    }

    for (const [id, fn] of [['limit', runLimit], ['ifd', runIfd], ['oco', runOco]]) {
      if (!ONLY.includes(id)) continue;
      result.scenarios[id] = await fn(ctxRun);
      await snapshotTracker(ctxRun, tokenFile);
    }
    const swept = await sweepOpenOrders(ctxRun);
    if (swept.length) result.sweptOpenOrders = swept;
    await snapshotTracker(ctxRun, tokenFile);
    ctxRun.h.badResponses.length && (result.badResponses = [...new Set(ctxRun.h.badResponses)].slice(0, 20));
    ctxRun.h.consoleErrors.length && (result.consoleErrors = ctxRun.h.consoleErrors.slice(0, 20));
    ctxRun.h.pageErrors.length && (result.pageErrors = ctxRun.h.pageErrors.slice(0, 20));
    result.finalBalanceKas = Number(await kasBalance(node, conn.address)) / 1e8;
  } catch (e) {
    result.fatal = String(e?.stack ?? e).slice(0, 2000);
    log(`[${name}] FATAL: ${String(e?.message ?? e).slice(0, 400)}`);
    try {
      for (const p of ctx.pages()) await shot(p, name, 'fatal');
    } catch {
      /* ignore */
    }
  } finally {
    result.finishedAt = new Date().toISOString();
    writeJson(`real-${name}.json`, result);
    if (!KEEP_OPEN) await ctx.close().catch(() => {});
    else log(`[${name}] --keep-open: leaving the browser open`);
  }
  return result;
}

// ------------------------------------------------------------------------------------------------ main

function summarize(results) {
  const rows = [];
  for (const r of results) {
    for (const id of ['issue', 'limit', 'ifd', 'oco']) {
      const s = r.scenarios[id];
      if (!s) {
        if (id === 'issue' && SKIP_ISSUE) continue;
        if (id !== 'issue' && !ONLY.includes(id)) continue;
        rows.push({ wallet: r.wallet, scenario: id, result: r.fatal ? 'FAIL (fatal)' : 'NOT RUN', txids: '', note: (r.fatal ?? '').split('\n')[0].slice(0, 100) });
        continue;
      }
      rows.push({ wallet: r.wallet, scenario: id, result: s.ok ? 'PASS' : 'FAIL', txids: s.steps.map((x) => (x.txid ? x.txid.slice(0, 10) : '-')).join(' '), note: s.ok ? '' : String(s.error ?? '').slice(0, 100) });
    }
  }
  console.log('\n' + ['wallet', 'scenario', 'result', 'txids (first 10 hex)', 'note'].join(' | '));
  for (const r of rows) console.log([r.wallet, r.scenario, r.result, r.txids, r.note].join(' | '));
  return rows;
}

async function main() {
  const env = { ...loadEnv(), ...(process.env.NODE_WS ? { NODE_WS: process.env.NODE_WS } : {}) };
  const node = await preflight(env);
  ensureBuild();
  mkdirSync(OUT_DIR, { recursive: true });
  const server = await startAppServer({ port: Number(optv('port', 0)) });
  log(`app served on ${server.url}`);
  const results = [];
  try {
    for (const w of WALLETS) results.push(await runWallet(w, { ...env, DEV_PRIVATE_KEY: env.DEV_PRIVATE_KEY }, node, server));
  } finally {
    await server.close();
  }
  const rows = summarize(results);
  writeJson('real-summary.json', { at: new Date().toISOString(), rows });
  const failed = rows.some((r) => r.result !== 'PASS');
  process.exit(failed ? 1 : 0);
}

main().catch((e) => {
  console.error('run failed:', e?.stack ?? e);
  process.exit(1);
});
