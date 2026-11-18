// The wallet's address, and a plain KAS UTXO to fund a token creation.
// It talks JSON wRPC, and the key is never read here.

const CHARSET = 'qpzry9x8gf2tvdw0s3jn54khce6mua7l';
const GENERATORS = [0x98f2bc8e61n, 0x79b76d99e2n, 0xf33e5fb3c4n, 0xae2eabe2a8n, 0x1e4f43e470n];

function polymod(values) {
  let c = 1n;
  for (const d of values) {
    const c0 = c >> 35n;
    c = ((c & 0x07ffffffffn) << 5n) ^ BigInt(d);
    for (let i = 0; i < 5; i++) if ((c0 >> BigInt(i)) & 1n) c ^= GENERATORS[i];
  }
  return c ^ 1n;
}

function toWords(bytes) {
  const out = [];
  let acc = 0;
  let bits = 0;
  for (const b of bytes) {
    acc = (acc << 8) | b;
    bits += 8;
    while (bits >= 5) {
      bits -= 5;
      out.push((acc >> bits) & 31);
    }
  }
  if (bits > 0) out.push((acc << (5 - bits)) & 31);
  return out;
}

/** The P2PK address of a 64-hex x-only key (`kaspa:` on mainnet, `kaspatest:` on testnet-10). */
export function addressOfXOnly(xOnlyHex, network) {
  if (!/^[0-9a-f]{64}$/.test(xOnlyHex)) throw new Error('an x-only key is 64 lower-case hex');
  const prefix = network === 'mainnet' ? 'kaspa' : 'kaspatest';
  const payload = toWords([0, ...Buffer.from(xOnlyHex, 'hex')]);
  const pre = [...prefix].map((ch) => ch.charCodeAt(0) & 31);
  const sum = polymod([...pre, 0, ...payload, 0, 0, 0, 0, 0, 0, 0, 0]);
  const check = [];
  for (let i = 0; i < 8; i++) check.push(Number((sum >> BigInt(5 * (7 - i))) & 31n));
  return `${prefix}:${[...payload, ...check].map((w) => CHARSET[w]).join('')}`;
}

/** A few JSON wRPC calls on one connection: `fn(call)` gets `call(method, params)`. */
export async function withNode(url, fn, timeoutMs = 20_000) {
  const ws = new WebSocket(url);
  const pend = new Map();
  let id = 0;
  const opened = new Promise((resolve, reject) => {
    ws.onopen = resolve;
    ws.onerror = () => reject(new Error(`cannot reach the node at ${url.replace(/\/\/[^@/]*@/, '//')}`));
  });
  ws.onmessage = (e) => {
    const m = JSON.parse(e.data);
    pend.get(m.id)?.(m);
    pend.delete(m.id);
  };
  const timer = setTimeout(() => ws.close(), timeoutMs);
  try {
    await opened;
    const call = (method, params = {}) => new Promise((resolve, reject) => {
      const i = ++id;
      pend.set(i, (m) => (m.error ? reject(new Error(`${method}: ${JSON.stringify(m.error)}`)) : resolve(m.params)));
      ws.send(JSON.stringify({ id: i, method, params }));
    });
    return await fn(call);
  } finally {
    clearTimeout(timer);
    ws.close();
  }
}

/**
 * The smallest plain, mature UTXO of the key worth at least `needSompi` that no mempool transaction
 * spends: `{ txid, index, amount }` (amount a BigInt), or null.
 */
export async function pickFundingUtxo(nodeUrl, xOnly, network, needSompi) {
  const address = addressOfXOnly(xOnly, network);
  const plainSpk = `000020${xOnly}ac`;
  return withNode(nodeUrl, async (call) => {
    const dag = await call('getBlockDagInfo');
    const { entries } = await call('getUtxosByAddresses', { addresses: [address] });
    const mp = await call('getMempoolEntriesByAddresses', { addresses: [address], includeOrphanPool: true, filterTransactionPool: false });
    const busy = new Set();
    for (const a of mp?.entries ?? []) {
      for (const s of a.sending ?? []) for (const i of s.transaction?.inputs ?? []) busy.add(`${i.previousOutpoint.transactionId}:${i.previousOutpoint.index}`);
    }
    const daa = BigInt(dag.virtualDaaScore);
    const ok = (entries ?? []).filter((e) => {
      const u = e.utxoEntry;
      return u && u.covenantId == null && u.scriptPublicKey === plainSpk
        && (!u.isCoinbase || daa - BigInt(u.blockDaaScore) > 1000n)
        && !busy.has(`${e.outpoint.transactionId}:${e.outpoint.index}`)
        && BigInt(u.amount) >= needSompi;
    });
    ok.sort((a, b) => (BigInt(a.utxoEntry.amount) < BigInt(b.utxoEntry.amount) ? -1 : 1));
    const e = ok[0];
    return e ? { txid: e.outpoint.transactionId, index: e.outpoint.index, amount: BigInt(e.utxoEntry.amount) } : null;
  });
}
