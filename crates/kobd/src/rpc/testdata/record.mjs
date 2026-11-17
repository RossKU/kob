// Records the chain-proof fixtures of rpc::proof (read only): a few chain blocks (VSPC v2, Full) from a recent
// start, the start header, the KIP-21 lane proofs at the start and the last block for every lane the blocks touch
// (coinbase aside), and one merged block with its transactions.
// Usage: node record.mjs <JSON wRPC URL of a public node> <out.json> [blocks back] [blocks kept]
// It needs @noble/hashes (blake3): NOBLE_BLAKE3=<path to @noble/hashes/blake3.js> or a resolvable package.
const { blake3 } = await import(process.env.NOBLE_BLAKE3 ? "file:///" + process.env.NOBLE_BLAKE3.replaceAll("\\", "/") : "@noble/hashes/blake3.js");
import { writeFileSync } from 'node:fs';
const [url, out, back = '6', keep = '4'] = process.argv.slice(2);
const key = new Uint8Array(32); key.set(new TextEncoder().encode('SeqCommitLaneKey'));
const hex = (b) => Buffer.from(b).toString('hex');
const nativeLaneKey = hex(blake3(new Uint8Array(20), { key }));
const ws = new WebSocket(url);
let id = 0; const pend = new Map();
ws.onmessage = (m) => { const r = JSON.parse(m.data, (k, v, c) => (typeof v === "number" && c && c.source && /^[0-9]+$/.test(c.source) && !Number.isSafeInteger(Number(c.source))) ? "__BIG__" + c.source : v); const p = pend.get(r.id); if (p) { pend.delete(r.id); p(r); } };
const call = (method, params) => new Promise((res, rej) => { const i = ++id; pend.set(i, (r) => r.error ? rej(new Error(method + ': ' + JSON.stringify(r.error))) : res(r.params)); ws.send(JSON.stringify({ id: i, method, params })); });
ws.onopen = async () => {
  try {
    const dag = await call('getBlockDagInfo', {});
    let h = dag.sink;
    // walk back along selected parents (first direct parent of a chain block)
    for (let i = 0; i < Number(back) + 20; i++) { const b = await call('getBlock', { hash: h, includeTransactions: false }); h = b.block.header.parentsByLevel[0][0]; }
    const start = h;
    const startBlock = await call('getBlock', { hash: start, includeTransactions: false });
    const v = await call('getVirtualChainFromBlockV2', { startHash: start, dataVerbosityLevel: 'Full' });
    const n = Math.min(Number(keep), v.addedChainBlockHashes.length);
    v.addedChainBlockHashes = v.addedChainBlockHashes.slice(0, n);
    v.chainBlockAcceptedTransactions = v.chainBlockAcceptedTransactions.slice(0, n);
    const last = v.addedChainBlockHashes[n - 1];
    const subs = new Set(['0000000000000000000000000000000000000000']);
    for (const b of v.chainBlockAcceptedTransactions) for (const t of b.acceptedTransactions) if (!t.subnetworkId.startsWith('01000000')) subs.add(t.subnetworkId);
    const proofs = {};
    for (const sub of subs) {
      const lk = hex(blake3(Buffer.from(sub, 'hex'), { key }));
      proofs[lk] = { subnetwork: sub, start: await call('getSeqCommitLaneProof', { blockHash: start, laneKey: lk }), end: await call('getSeqCommitLaneProof', { blockHash: last, laneKey: lk }) };
    }
    // one merged block holding a native transaction of the window
    let merged = null;
    for (const b of v.chainBlockAcceptedTransactions) for (const t of b.acceptedTransactions) {
      if (!merged && t.subnetworkId === '0000000000000000000000000000000000000000' && t.verboseData?.blockHash && t.inputs.length > 0) merged = t.verboseData.blockHash;
    }
    const mergedBlock = merged ? await call('getBlock', { hash: merged, includeTransactions: true }) : null;
    writeFileSync(out, JSON.stringify({ network: dag.network, nativeLaneKey, start, startHeader: startBlock.block.header, vspc: v, proofs, mergedBlock: mergedBlock?.block ?? null }, null, 0).replace(/"__BIG__([0-9]+)"/g, "$1"));
    console.log('ok', out, 'blocks', n, 'txs', v.chainBlockAcceptedTransactions.map((b) => b.acceptedTransactions.length).join(','), 'merged', merged);
    process.exit(0);
  } catch (e) { console.log('fail', e.message); process.exit(1); }
};
ws.onerror = (e) => { console.log('err', e.message); process.exit(1); };
setTimeout(() => { console.log('timeout'); process.exit(1); }, 60000);
