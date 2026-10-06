// Collects the chain evidence behind the registry's per-token genesis check (C1) and live-minter check (C2), READ-ONLY.
//
//   node web/scripts/registry-genesis-evidence.mjs [--registry registry/tokens.json] [--out registry/evidence/mainnet-genesis.json]
//        [--node wss://...] [--api https://api.kaspa.org] [--hints https://api.kron.technology/api/registry/tokens]
//        [--genesis <covenant id>=<genesis txid> ...]   (overrides / replaces the hint for a covenant id)
//   cargo run -p kob-cli -- registry verify-genesis --network mainnet --evidence registry/evidence/mainnet-genesis.json
//
// Nothing here is trusted by the verifier: every byte this script records is re-checked by `kob registry verify-genesis`
// against a hash (the genesis txid is recomputed from the recorded fields, the covenant id from the authorising outpoint and
// the group's outputs, each redeem script against its P2SH script public key). The sources:
//
// * Kaspa MAINNET node over wRPC (official kaspa-wasm v2.1.0 SDK, the public `Resolver` unless --node is given): server info,
//   the virtual DAA score, and `getUtxosByAddresses` (which genesis outputs are still unspent, and the live cells of a token's
//   minter lineage). Read-only calls only; this script never builds, signs or submits a transaction.
// * A block explorer (api.kaspa.org, the kaspa-rest-server over the public kaspa-db-filler database) for the genesis
//   transaction and the transactions that revealed the genesis redeem scripts: public nodes prune block bodies after about
//   30 hours, and every KRON genesis is weeks old. The explorer drops `sequence`, `lockTime` and `gas`; they are recorded as
//   0 (the values KRON's builder uses), which the txid recomputation in the verifier confirms or refutes.
// * The KRON API (api.kron.technology) only as a HINT for the genesis txid of a covenant id; the verifier recomputes the
//   covenant id from the genesis transaction, so a wrong hint fails, it cannot pass a wrong genesis.
import { readFileSync, writeFileSync, mkdirSync } from 'node:fs';
import { createRequire } from 'node:module';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

const ROOT = resolve(dirname(fileURLToPath(import.meta.url)), '..', '..');
const args = process.argv.slice(2);
const opt = (name, dflt) => {
  const i = args.indexOf(name);
  return i >= 0 ? args[i + 1] : dflt;
};
const REGISTRY = resolve(opt('--registry', join(ROOT, 'registry', 'tokens.json')));
const OUT = resolve(opt('--out', join(ROOT, 'registry', 'evidence', 'mainnet-genesis.json')));
const API = opt('--api', 'https://api.kaspa.org').replace(/\/$/, '');
const HINTS = opt('--hints', 'https://api.kron.technology/api/registry/tokens');
const NODE = opt('--node', '');
const GENESIS = new Map(
  args.flatMap((a, i) => (a === '--genesis' && args[i + 1] ? [args[i + 1].split('=')] : [])).map(([c, t]) => [c.toLowerCase(), t.toLowerCase()]),
);

const require = createRequire(import.meta.url);
const sdk = require(join(ROOT, 'web', 'vendor', 'kaspa-node', 'kaspa.js')); // `npm run fetch-sdk` in web/ first

const reg = JSON.parse(readFileSync(REGISTRY, 'utf8'));
if (reg.network !== 'mainnet') throw new Error(`${REGISTRY}: network ${reg.network}, this script reads mainnet`);

async function get(url) {
  for (let attempt = 1; ; attempt++) {
    try {
      const res = await fetch(url, { headers: { accept: 'application/json' } });
      if (!res.ok) throw new Error(`HTTP ${res.status}`);
      return await res.json();
    } catch (e) {
      if (attempt >= 4) throw new Error(`GET ${url}: ${e.message}`);
      await new Promise((r) => setTimeout(r, 1500 * attempt));
    }
  }
}
const txUrl = (id) => `${API}/transactions/${id}?inputs=true&outputs=true&resolve_previous_outpoints=no`;
const bigintJson = (_k, v) => (typeof v === 'bigint' ? v.toString() : v);

// explorer transaction -> the fields the txid commits to (v1: no signature scripts, no mass)
function txFields(t) {
  return {
    version: t.version,
    inputs: t.inputs
      .sort((a, b) => a.index - b.index)
      .map((i) => ({ txid: i.previous_outpoint_hash, index: Number(i.previous_outpoint_index), sequence: 0, covenant_id: i.covenant_id ?? null })),
    outputs: t.outputs
      .sort((a, b) => a.index - b.index)
      .map((o) => ({
        value: Number(o.amount),
        spk_version: 0,
        spk: o.script_public_key,
        covenant: o.covenant_id ? { authorizing_input: o.covenant_authorizing_input, covenant_id: o.covenant_id } : null,
      })),
    lock_time: 0,
    subnetwork_id: t.subnetwork_id,
    gas: 0,
    payload: t.payload ?? '',
  };
}

// the last data push of a signature script (a P2SH spend reveals its redeem script there); the verifier re-checks the hash
function lastPush(hex) {
  const b = Buffer.from(hex, 'hex');
  let i = 0;
  let last = null;
  while (i < b.length) {
    const op = b[i++];
    let n;
    if (op >= 0x01 && op <= 0x4b) n = op;
    else if (op === 0x4c) n = b[i++];
    else if (op === 0x4d) (n = b.readUInt16LE(i)), (i += 2);
    else if (op === 0x4e) (n = b.readUInt32LE(i)), (i += 4);
    else {
      last = null; // an opcode: the redeem script is the final push only when nothing follows it
      continue;
    }
    last = b.subarray(i, i + n).toString('hex');
    i += n;
  }
  return last;
}

// the transaction that spent `txid:index` (an output at `address`), from the explorer's history of that address
async function spender(address, txid, index) {
  for (let offset = 0; offset < 2000; offset += 50) {
    const page = await get(`${API}/addresses/${address}/full-transactions?limit=50&offset=${offset}&resolve_previous_outpoints=no`);
    for (const t of page) {
      const inp = t.inputs?.find((i) => i.previous_outpoint_hash === txid && Number(i.previous_outpoint_index) === index);
      if (inp) return { txid: t.transaction_id, input: inp.index, sigscript: inp.signature_script, accepted: t.is_accepted };
    }
    if (page.length < 50) return null;
  }
  return null;
}

async function connect() {
  const cfg = NODE
    ? { url: NODE, networkId: 'mainnet', encoding: sdk.Encoding.Borsh }
    : { resolver: new sdk.Resolver(), networkId: 'mainnet', encoding: sdk.Encoding.Borsh }; // public resolvers serve borsh
  const rpc = new sdk.RpcClient(cfg);
  await rpc.connect({ blockAsyncConnect: true, timeoutDuration: 20000 });
  const info = await rpc.getServerInfo();
  if (info.networkId !== 'mainnet') throw new Error(`node is on ${info.networkId}`);
  if (!info.hasUtxoIndex || !info.isSynced) throw new Error('node needs --utxoindex and to be synced');
  return { rpc, info };
}

async function liveAt(rpc, addresses) {
  const out = [];
  for (let i = 0; i < addresses.length; i += 50) {
    const r = await rpc.getUtxosByAddresses({ addresses: addresses.slice(i, i + 50) });
    for (const e of r.entries ?? []) {
      const u = e.utxoEntry ?? e.entry ?? e;
      out.push({
        address: e.address?.toString?.() ?? String(e.address),
        txid: e.outpoint.transactionId,
        index: e.outpoint.index,
        amount: Number(u.amount),
        spk: typeof u.scriptPublicKey === 'string' ? u.scriptPublicKey.replace(/^0000/, '') : String(u.scriptPublicKey?.script),
        covenant_id: u.covenantId ? String(u.covenantId) : null,
        block_daa_score: Number(u.blockDaaScore),
      });
    }
  }
  return out;
}

const hints = await get(HINTS).catch((e) => {
  console.warn(`KRON hints unavailable (${e.message}); give the genesis txids with --genesis <covenant id>=<txid>`);
  return { tokens: [] };
});
const hintTok = (cov) => hints.tokens.find((t) => t.cp?.tokenCovid === cov) ?? null;
const hintOf = (cov) => GENESIS.get(cov) ?? hintTok(cov)?.cp?.genesisTxid ?? null;

// KRON state span: 0x20 owner | 0x01 id_type | 0x08 amount (LE i64) | 0x01 is_minter (46 bytes, offset 0)
function kronState(owner, idType, amount, minter) {
  const amt = Buffer.alloc(8);
  amt.writeBigInt64LE(BigInt(amount));
  return Buffer.concat([Buffer.from([0x20]), Buffer.from(owner, 'hex'), Buffer.from([0x01, idType, 0x08]), amt, Buffer.from([0x01, minter])]).toString('hex');
}
const p2sh = (redeem) => String(sdk.payToScriptHashScript(redeem).script);

// An output that was never spent has not revealed its redeem script. For a KRON launch the unspent genesis output is the
// creator's (or the vesting covenant's) allocation: try the states the KRON API record describes over the program suffix of a
// revealed sibling; the P2SH hash decides, and the verifier re-checks it.
function reconstruct(cov, spk, suffix) {
  const h = hintTok(cov);
  if (!h || !suffix) return null;
  // every 32-byte key / covenant id and every positive integer of the hint record (creator key, vesting covenant, devAmount,
  // vesting total, ...): a few hundred candidates, and only the one that hashes to the output counts
  const owners = new Set();
  const amounts = new Set();
  (function walk(v) {
    if (typeof v === 'string' && /^[0-9a-f]{64}$/.test(v)) owners.add(v);
    else if (typeof v === 'number' && Number.isSafeInteger(v) && v > 0 && v <= 1e9) amounts.add(v);
    else if (v && typeof v === 'object') Object.values(v).forEach(walk);
  })(h);
  for (const owner of owners)
    for (const idType of [0, 1, 2, 3])
      for (const amount of amounts)
        for (const minter of [0, 1]) {
          const redeem = kronState(owner, idType, amount, minter) + suffix;
          if (p2sh(redeem) === spk) return { redeem, owner, idType, amount, minter };
        }
  return null;
}

const { rpc, info } = await connect();
const dag = await rpc.getBlockDagInfo();
const evidence = {
  network: 'mainnet',
  collected_at: new Date().toISOString(),
  sources: {
    node: { url: rpc.url, server_version: info.serverVersion, virtual_daa_score: Number(info.virtualDaaScore), pruning_point: dag.pruningPointHash },
    explorer: API,
    hints: HINTS,
    note: 'Genesis transactions and their redeem-script reveals are older than the node pruning point, so they come from the explorer; every recorded byte is re-verified by hash (txid, covenant id, P2SH). Liveness (unspent genesis outputs, live minter cells) comes from the node UTXO index.',
  },
  tokens: [],
};

for (const tok of reg.tokens) {
  const cov = tok.covenant_id;
  const rec = { ticker: tok.ticker, covenant_id: cov, template_id: tok.template_id };
  try {
    const gtxid = hintOf(cov);
    if (!gtxid) throw new Error('no genesis txid hint');
    const t = await get(txUrl(gtxid));
    if (!t.is_accepted) throw new Error(`genesis ${gtxid} is not accepted`);
    const hdr = await get(`${API}/blocks/${t.accepting_block_hash}?includeTransactions=false`);
    rec.genesis = {
      txid: t.transaction_id,
      accepting_block_hash: t.accepting_block_hash,
      accepting_block_daa_score: Number(hdr.header?.daaScore ?? hdr.header?.daa_score),
      accepting_block_blue_score: Number(t.accepting_block_blue_score),
      block_time_ms: Number(t.block_time),
      source: `${API}/transactions/${gtxid} (explorer; the block is below the node pruning point)`,
      tx: txFields(t),
    };
    const group = t.outputs.filter((o) => o.covenant_id === cov);
    rec.genesis.reveals = [];
    for (const o of group) {
      const s = await spender(o.script_public_key_address, gtxid, o.index);
      rec.genesis.reveals.push(
        s
          ? { index: o.index, redeem_script: lastPush(s.sigscript), revealed_by: `${s.txid}:${s.input}`, source: `${API}/addresses/${o.script_public_key_address}/full-transactions` }
          : { index: o.index, redeem_script: null, revealed_by: null, source: 'not spent according to the explorer' },
      );
    }
    rec.genesis_outputs_live = await liveAt(rpc, group.map((o) => o.script_public_key_address));
    const revealed = rec.genesis.reveals.find((r) => r.redeem_script);
    const suffix = revealed ? revealed.redeem_script.slice(46 * 2) : null;
    for (const r of rec.genesis.reveals.filter((r) => !r.redeem_script)) {
      const o = group.find((g) => g.index === r.index);
      const live = rec.genesis_outputs_live.some((u) => u.txid === gtxid && u.index === r.index);
      const c = reconstruct(cov, o.script_public_key, suffix);
      if (c) {
        r.redeem_script = c.redeem;
        r.source = `reconstructed (output ${live ? 'unspent per the node UTXO index' : 'not spent per the explorer'}): state owner ${c.owner}, id_type ${c.idType}, amount ${c.amount}, is_minter ${c.minter} (candidates taken from the KRON API record of the token), followed by the program suffix revealed by output ${revealed.index}; matched against the output's P2SH hash`;
      }
    }
  } catch (e) {
    rec.error = String(e.message ?? e);
  }
  console.log(`${tok.ticker.padEnd(7)} ${cov.slice(0, 8)}… ${rec.error ?? `genesis ${rec.genesis.txid.slice(0, 12)}… DAA ${rec.genesis.accepting_block_daa_score}, ${rec.genesis.reveals.length} output(s), ${rec.genesis.reveals.filter((r) => r.redeem_script).length} revealed`}`);
  evidence.tokens.push(rec);
}

// C2 (no live minter) is decided by the verifier from the genesis states: the KRON program lets a transaction create an
// is_minter output only when it spends a minter (every non-minter token input refuses minter outputs), and a covenant id's
// only other source of outputs is its genesis, so a genesis without a minter output proves the token can never have one. A
// genesis WITH a minter output needs its lineage followed to the live cells; this script does not do that, and the verifier
// then reports C2 as undetermined.
await rpc.disconnect();
mkdirSync(dirname(OUT), { recursive: true });
writeFileSync(OUT, JSON.stringify(evidence, bigintJson, 2) + '\n');
console.log(`wrote ${OUT}`);
process.exit(0);
