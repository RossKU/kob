// Drives the REAL gate page in headless Chrome against TN10, with a MOCK KasWare (window.kasware) that signs with the
// local SIM key through kaspa-wasm. Verifies the page logic (build -> wallet sign -> extract -> assemble -> broadcast -> accept)
// without any real wallet. Real-wallet runs use test/wallets/*.mjs.
//   node test/page-mock.mjs
import { spawn } from 'node:child_process';
import { readFileSync, writeFileSync, mkdirSync, existsSync } from 'node:fs';
import { join } from 'node:path';
import { chromium } from 'playwright-core';
import { ROOT } from '../lib/node-kaspa.mjs';
import { loadEnv } from '../lib/env.mjs';

const env = loadEnv();
const PORT = 8791;
const CHROME = process.env.CHROME_PATH || ['C:/Program Files/Google/Chrome/Application/chrome.exe', 'C:/Program Files (x86)/Microsoft/Edge/Application/msedge.exe'].find((p) => existsSync(p));
const server = spawn(process.execPath, [join(ROOT, 'scripts', 'serve.mjs'), String(PORT)], { stdio: 'inherit' });
await new Promise((r) => setTimeout(r, 1200));
const browser = await chromium.launch({ executablePath: CHROME, headless: true });
const results = [];
try {
  const page = await browser.newPage();
  page.on('console', (m) => { if (m.type() === 'error') console.log('[page error]', m.text()); });
  page.on('pageerror', (e) => console.log('[pageerror]', e.message));
  await page.addInitScript(({ sk, pub, addr }) => {
    // mock KasWare: signs input(s) with the local key via the page's kaspa-wasm
    window.kasware = {
      requestAccounts: async () => [addr], getNetwork: async () => 'testnet-10', switchNetwork: async () => 'testnet-10',
      getPublicKey: async () => pub, getVersion: async () => 'mock-1.0',
      signPskt: async ({ txJsonString, options }) => {
        const k = window.__gate.k;
        const tx = k.Transaction.deserializeFromSafeJSON(txJsonString);
        const key = new k.PrivateKey(sk);
        for (const si of options.signInputs) tx.inputs[si.index].signatureScript = k.createInputSignature(tx, si.index, key, k.SighashType.All);
        return tx.serializeToSafeJSON();
      },
    };
  }, { sk: env.SIM_WALLET_PRIVATE_KEY, pub: env.SIM_WALLET_PUBKEY, addr: env.SIM_WALLET_ADDRESS });
  await page.goto(`http://localhost:${PORT}/`);
  await page.waitForFunction(() => window.__gateReady === true, null, { timeout: 60000 });
  await page.click('#connect-kasware');
  await page.waitForFunction(() => window.__gate.ctx, null, { timeout: 30000 });
  for (const id of ['T1', 'T2', 'T3']) {
    const rec = await page.evaluate((id) => window.__gate.runTest(id, { skip: 1, popup: 'no', displayed: 'mock wallet, no popup' }), id);
    results.push(rec);
    console.log(rec.result, id, rec.txid ?? '', rec.error ?? '', 'sigscript==ours:', rec.walletSigscriptEqualsOurs, 'kept:', JSON.stringify(rec.walletKeptFields));
  }
} finally {
  await browser.close();
  server.kill();
  mkdirSync(join(ROOT, 'out'), { recursive: true });
  writeFileSync(join(ROOT, 'out', 'page-mock.json'), JSON.stringify(results.map(({ unsignedTxJson, ...r }) => r), null, 2));
}
process.exit(results.length === 3 && results.every((r) => r.result === 'PASS') ? 0 : 1);
