// Tx v1 (Toccata) construction helpers on top of an injected kaspa-wasm namespace `k`
// (node: vendor/kaspa-node, browser: vendor/kaspa-web). No node built-ins used.
import { hex, unhex } from './script.mjs';

export const TX_VERSION = 1;
export const SUBNETWORK_NATIVE = '0000000000000000000000000000000000000000';
const MIN_FEE_PER_GRAM = 100n;
const GRAMS_PER_BUDGET = 100n; // 1 compute-budget unit = 10_000 script units = 100 grams
const MASS_PER_SPK_BYTE = 10n;

/**
 * Plan format (plain data, wallet-independent):
 *   { inputs:[{ txid, index, amount:bigint, spk:hex, covenantId?:hex, address?, daa?:bigint, isCoinbase?, budget:int, sigscript?:hex }],
 *     outputs:[{ value:bigint, spk:hex, covenant?:{ auth:int, id:hex } }], lockTime?:bigint }
 */

/** transaction_estimated_serialized_size (consensus/core/src/mass) for a v1 tx. */
export function planSize(plan, sigscriptLens) {
  let s = 2 + 8;
  plan.inputs.forEach((inp, i) => {
    const ssLen = sigscriptLens ? sigscriptLens[i] : inp.sigscript ? inp.sigscript.length / 2 : 0;
    s += 32 + 4 + 8 + ssLen + 8 + 2;
  });
  s += 8;
  for (const o of plan.outputs) s += 8 + 2 + 8 + o.spk.length / 2 + (o.covenant ? 2 + 32 : 0);
  s += 8 + 20 + 8 + 32 + 8; // locktime, subnetwork, gas, payload hash, payload len (no payload)
  return BigInt(s);
}
export function planComputeMass(plan, sigscriptLens) {
  const size = planSize(plan, sigscriptLens);
  let spk = 0n;
  for (const o of plan.outputs) spk += 2n + BigInt(o.spk.length / 2);
  const budget = plan.inputs.reduce((a, i) => a + BigInt(i.budget), 0n);
  return { size, compute: size + spk * MASS_PER_SPK_BYTE + budget * GRAMS_PER_BUDGET };
}
/** Node policy floor: 100 sompi * max(compute mass, 2 * size); +3% margin, rounded up. */
export function minFee(plan, sigscriptLens, marginPct = 3n) {
  const { size, compute } = planComputeMass(plan, sigscriptLens);
  const base = (compute > 2n * size ? compute : 2n * size) * MIN_FEE_PER_GRAM;
  return (base * (100n + marginPct) + 99n) / 100n;
}

/** wasm Transaction from a plan. */
export function toWasmTx(k, plan) {
  const inputs = plan.inputs.map((inp) => ({
    previousOutpoint: { transactionId: inp.txid, index: inp.index },
    signatureScript: inp.sigscript || '',
    sequence: 0n,
    sigOpCount: 0,
    computeBudget: inp.budget,
    utxo: {
      address: inp.address,
      outpoint: { transactionId: inp.txid, index: inp.index },
      amount: inp.amount,
      scriptPublicKey: new k.ScriptPublicKey(0, inp.spk),
      blockDaaScore: inp.daa ?? 0n,
      isCoinbase: !!inp.isCoinbase,
      covenantId: inp.covenantId || undefined,
    },
  }));
  const outputs = plan.outputs.map((o) => new k.TransactionOutput(
    o.value,
    new k.ScriptPublicKey(0, o.spk),
    o.covenant ? new k.CovenantBinding(o.covenant.auth, new k.Hash(o.covenant.id)) : undefined,
  ));
  return new k.Transaction({
    version: TX_VERSION, inputs, outputs, lockTime: plan.lockTime ?? 0n,
    subnetworkId: SUBNETWORK_NATIVE, gas: 0n, payload: '', mass: 0n,
  });
}

/** Signature script of a plain P2PK input: <push 65: sig+sighash>. `createInputSignature` already returns it. */
export function signP2pkInput(k, tx, index, privateKey, sighash = k.SighashType.All) {
  return k.createInputSignature(tx, index, privateKey, sighash); // hex of 0x41 || sig64 || sighashByte
}

/** Extracts the 65-byte (sig64 + sighash) signature from a wallet-produced signature script.
 *  Accepts <sig>, <sig><redeem>, <...><sig><...>: the FIRST 65-byte push is taken. */
export function extractSig65(parsePushes, sigscriptHex) {
  const pushes = parsePushes(unhex(sigscriptHex));
  const cand = pushes.filter((p) => p.length === 65);
  if (!cand.length) throw new Error(`no 65-byte signature push in sigscript (${pushes.length} pushes: ${pushes.map((p) => p.length).join(',')})`);
  return cand[0];
}

/** Chooses UTXOs (largest first) to cover `need` sompi. */
export function selectUtxos(utxos, need) {
  const sorted = [...utxos].sort((a, b) => (a.amount < b.amount ? 1 : a.amount > b.amount ? -1 : 0));
  const chosen = [];
  let sum = 0n;
  for (const u of sorted) {
    chosen.push(u);
    sum += u.amount;
    if (sum >= need) return { chosen, sum };
  }
  throw new Error(`insufficient funds: have ${sum}, need ${need}`);
}

/** Normalises an RPC utxo entry (from getUtxosByAddresses; wasm UtxoEntryReference) into a plan input. */
export function utxoToInput(e, budget) {
  const u = e.entry ?? e;
  return {
    txid: u.outpoint.transactionId, index: u.outpoint.index, amount: BigInt(u.amount),
    spk: u.scriptPublicKey.script ?? u.scriptPublicKey, address: u.address?.toString?.() ?? u.address,
    covenantId: u.covenantId?.toString?.() ?? u.covenantId ?? undefined,
    daa: BigInt(u.blockDaaScore ?? 0), isCoinbase: !!u.isCoinbase, budget,
  };
}

export const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

/** Connects a wasm RpcClient (JSON wRPC). nodeWs empty -> public Resolver. */
export async function connectRpc(k, nodeWs, networkId = 'testnet-10') {
  const opts = nodeWs
    ? { url: nodeWs, networkId, encoding: k.Encoding.SerdeJson }
    : { resolver: new k.Resolver(), networkId };
  const rpc = new k.RpcClient(opts);
  await rpc.connect({ timeoutDuration: 15000, blockAsyncConnect: true });
  return rpc;
}

export async function utxosOf(rpc, address) {
  const r = await rpc.getUtxosByAddresses({ addresses: [address] });
  return r.entries;
}

/** Waits until outpoint `txid:index` shows up in the UTXO set of `address` (i.e. the tx was accepted). */
export async function waitAccepted(rpc, address, txid, index = 0, timeoutMs = 120000) {
  const t0 = Date.now();
  while (Date.now() - t0 < timeoutMs) {
    const es = await utxosOf(rpc, address);
    if (es.some((e) => e.outpoint.transactionId === txid && e.outpoint.index === index)) return { accepted: true, ms: Date.now() - t0 };
    await sleep(1500);
  }
  return { accepted: false, ms: Date.now() - t0 };
}

export async function submit(rpc, tx) {
  const r = await rpc.submitTransaction({ transaction: tx, allowOrphan: false });
  return r.transactionId;
}
