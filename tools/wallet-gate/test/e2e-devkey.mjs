// LIVE end-to-end on TN10 with a LOCAL key standing in for the wallet (no browser, no wallet):
//   setup (if needed) -> T1 P2PK spend, T2 BidOrder.cancel, T3 KCC-20 transfer -> submit -> wait for acceptance.
// Proves the tx shapes / sigscript assembly the page will broadcast are valid on the real network.
//   node test/e2e-devkey.mjs [--fresh]
import { readFileSync, existsSync, mkdirSync, writeFileSync } from 'node:fs';
import { join } from 'node:path';
import k, { ROOT } from '../lib/node-kaspa.mjs';
import { loadEnv } from '../lib/env.mjs';
import * as C from '../lib/contracts.mjs';
import * as T from '../lib/txbuild.mjs';
import * as F from '../lib/flows.mjs';
import { runSetup } from '../lib/setup-core.mjs';

const env = loadEnv();
if (!env.SIM_WALLET_PRIVATE_KEY) throw new Error('SIM_WALLET_PRIVATE_KEY missing in .env (see README: simulated wallet)');
const simKey = new k.PrivateKey(env.SIM_WALLET_PRIVATE_KEY);
const W = simKey.toPublicKey().toXOnlyPublicKey().toString();
const kcc = JSON.parse(readFileSync(join(ROOT, 'artifacts', 'KCC20Ref.json'), 'utf8'));
const bidT = JSON.parse(readFileSync(join(ROOT, 'artifacts', 'BidOrder.template.json'), 'utf8'));
const devPub = new k.PrivateKey(env.DEV_PRIVATE_KEY).toPublicKey().toXOnlyPublicKey().toString();

const rpc = await T.connectRpc(k, env.NODE_WS);
const results = [];
try {
  const file = join(ROOT, 'state', `setup-${W}.json`);
  let setup;
  if (existsSync(file) && !process.argv.includes('--fresh')) setup = JSON.parse(readFileSync(file, 'utf8'));
  else {
    setup = await runSetup({ k, rpc, devKey: new k.PrivateKey(env.DEV_PRIVATE_KEY), walletPubkey: W, kcc20Artifact: kcc, bidTemplate: bidT });
    mkdirSync(join(ROOT, 'state'), { recursive: true });
    writeFileSync(file, JSON.stringify(setup, null, 2));
  }
  const ctx = F.deriveContext(k, setup, kcc, bidT);
  console.log('context checks', ctx.checks);
  if (!ctx.checks.tokenAddressMatches || !ctx.checks.bidAddressMatches) throw new Error('derived addresses differ from setup file');

  const live = async (addr, txid, index) => (await T.utxosOf(rpc, addr)).find((e) => (e.entry ?? e).outpoint.transactionId === txid && (e.entry ?? e).outpoint.index === index);
  const run = async (t) => {
    const rec = { id: t.id, name: t.name, wallet: 'local-key (simulation)', fee: t.fee.toString() };
    try {
      const sig65 = F.signLocal(k, t, simKey);
      const { tx, sigscript } = F.finalize(t, sig65);
      rec.sigscriptBytes = sigscript.length / 2;
      rec.txid = await T.submit(rpc, tx);
      const acc = await T.waitAccepted(rpc, t.outAddress, rec.txid, t.outIndex);
      rec.accepted = acc.accepted; rec.acceptMs = acc.ms;
    } catch (e) { rec.error = String(e?.message ?? e); rec.accepted = false; }
    results.push(rec);
    console.log(rec.accepted ? 'PASS' : 'FAIL', t.id, t.name, JSON.stringify(rec));
    return rec;
  };

  // T1: first funding UTXO
  const f = setup.funds[0];
  const fu = await live(setup.walletAddress, f.txid, f.index);
  if (!fu) throw new Error('funding UTXO gone (already spent?) - rerun with --fresh');
  await run(F.buildT1(k, ctx, fu));
  // T2: first bid
  const b = setup.bids[0];
  const bu = await live(b.address, b.txid, b.index);
  if (!bu) throw new Error('bid UTXO gone (already cancelled?) - rerun with --fresh');
  await run(F.buildT2(k, ctx, b, bu));
  // T3: first token, to the dev key
  const tk = setup.tokens[0];
  const tu = await live(tk.address, tk.txid, tk.index);
  if (!tu) throw new Error('token UTXO gone (already transferred?) - rerun with --fresh');
  await run(F.buildT3(k, ctx, tu, devPub));
} catch (e) {
  console.error('E2E ERROR:', e?.stack ?? e);
  process.exitCode = 1;
} finally {
  mkdirSync(join(ROOT, 'out'), { recursive: true });
  writeFileSync(join(ROOT, 'out', 'e2e-devkey.json'), JSON.stringify({ at: new Date().toISOString(), node: env.NODE_WS, results }, null, 2));
  try { await rpc.disconnect(); } catch {}
  if (results.some((r) => !r.accepted)) process.exitCode = 1;
  process.exit(process.exitCode ?? 0);
}
