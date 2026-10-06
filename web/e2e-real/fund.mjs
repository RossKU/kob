// Funds TN10 addresses from the DEV key (a plain P2PK tx v1 with computeBudget, signed with the official SDK). Used by the real-wallet run;
// the DEV key comes from web/e2e-real/.env (gitignored: DEV_PRIVATE_KEY, NODE_WS) or the environment, never from the repo.
//   node e2e-real/fund.mjs <address> <kas> [<address> <kas> ...]
import { createRequire } from 'node:module';
import { existsSync, readFileSync } from 'node:fs';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';
import * as T from '../../tools/wallet-gate/lib/txbuild.mjs';

const HERE = dirname(fileURLToPath(import.meta.url));
const require = createRequire(import.meta.url);
export const TN10_NODE = process.env.KOB_TN10_WRPC || 'ws://127.0.0.1:18210';
const COINBASE_MATURITY = 1010n;
const KAS = 100_000_000n;

export function loadRealEnv() {
  const out = {};
  const f = join(HERE, '.env');
  if (existsSync(f)) {
    for (const line of readFileSync(f, 'utf8').split(/\r?\n/)) {
      const m = /^\s*([A-Za-z0-9_]+)\s*=\s*(.*?)\s*$/.exec(line);
      if (m && !line.trim().startsWith('#')) out[m[1]] = m[2].replace(/^["']|["']$/g, '');
    }
  }
  for (const k of ['NODE_WS', 'DEV_PRIVATE_KEY', 'DEV_ADDRESS', 'WALLET_MNEMONIC_KASWARE', 'WALLET_MNEMONIC_KASPIRE', 'WALLET_MNEMONIC_KASTLE']) if (process.env[k]) out[k] = process.env[k];
  return out;
}

export function loadSdk() {
  return require(join(HERE, '..', 'vendor', 'kaspa-node', 'kaspa.js'));
}

/** Sends `outputs` ([{address, kas}]) from the dev key in one transaction; resolves with the txid once the first output is visible. */
export async function fund(outputs, { env = loadRealEnv(), log = console.log } = {}) {
  const k = loadSdk();
  if (!env.DEV_PRIVATE_KEY) throw new Error('DEV_PRIVATE_KEY missing (web/e2e-real/.env)');
  const devKey = new k.PrivateKey(env.DEV_PRIVATE_KEY);
  const devAddr = devKey.toAddress('testnet-10').toString();
  const rpc = await T.connectRpc(k, env.NODE_WS || TN10_NODE, 'testnet-10');
  try {
    const dag = await rpc.getBlockDagInfo();
    const vdaa = BigInt(dag.virtualDaaScore);
    const all = await T.utxosOf(rpc, devAddr);
    const mature = all.map((e) => T.utxoToInput(e, 12)).filter((u) => !u.covenantId && (!u.isCoinbase || vdaa - u.daa >= COINBASE_MATURITY));
    const outs = outputs.map((o) => ({ value: BigInt(Math.round(o.kas * 1e8)), spk: k.payToAddressScript(new k.Address(o.address)).script }));
    const need = outs.reduce((a, o) => a + o.value, 0n) + 2n * KAS;
    const { chosen } = T.selectUtxos(mature, need);
    const inputs = chosen.slice(0, 40).map((u) => ({ ...u, budget: 12 }));
    const change = { value: 0n, spk: k.payToAddressScript(devKey.toAddress('testnet-10')).script };
    const plan = { inputs, outputs: [...outs, change] };
    const fee = T.minFee(plan, inputs.map(() => 66));
    change.value = inputs.reduce((a, u) => a + u.amount, 0n) - outs.reduce((a, o) => a + o.value, 0n) - fee;
    if (change.value < KAS) throw new Error('dev key change too small: fund the DEV address');
    const tx = T.toWasmTx(k, plan);
    for (let i = 0; i < inputs.length; i++) tx.inputs[i].signatureScript = T.signP2pkInput(k, tx, i, devKey);
    const txid = await T.submit(rpc, tx);
    log(`fund: submitted ${txid} (${outs.length} outputs, fee ${fee} sompi)`);
    const acc = await T.waitAccepted(rpc, outputs[0].address, txid, 0, 90_000);
    if (!acc.accepted) throw new Error('funding tx not accepted in time: ' + txid);
    return txid;
  } finally {
    try { await rpc.disconnect(); } catch { /* ignore */ }
  }
}

/** Balance of an address in KAS (number) and its spendable UTXO count. */
export async function balance(address, { env = loadRealEnv() } = {}) {
  const k = loadSdk();
  const rpc = await T.connectRpc(k, env.NODE_WS || TN10_NODE, 'testnet-10');
  try {
    const es = await T.utxosOf(rpc, address);
    const sum = es.reduce((a, e) => a + BigInt((e.entry ?? e).amount), 0n);
    return { kas: Number(sum) / 1e8, utxos: es.length };
  } finally {
    try { await rpc.disconnect(); } catch { /* ignore */ }
  }
}

if (process.argv[1] && fileURLToPath(import.meta.url) === process.argv[1]) {
  const args = process.argv.slice(2);
  if (args.length < 2 || args.length % 2) {
    console.error('usage: node e2e-real/fund.mjs <address> <kas> [<address> <kas> ...]');
    process.exit(2);
  }
  const outs = [];
  for (let i = 0; i < args.length; i += 2) outs.push({ address: args[i], kas: Number(args[i + 1]) });
  fund(outs).then((t) => { console.log(t); process.exit(0); }, (e) => { console.error(e?.message ?? e); process.exit(1); });
}
