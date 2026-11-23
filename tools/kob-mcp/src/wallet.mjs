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

/**
 * A few JSON wRPC calls on one connection: `fn(call)` gets `call(method, params)`. A node that
 * does not answer within `timeoutMs`, closes the socket or sends a frame that is not JSON fails
 * every pending call, so the caller (and the write queue behind it) goes on (R3T-4).
 */
export async function withNode(url, fn, timeoutMs = 20_000) {
  const shown = url.replace(/\/\/[^@/]*@/, '//');
  const ws = new WebSocket(url);
  const pend = new Map();
  let id = 0;
  let failed = null;
  let failOpen = null;
  const fail = (e) => {
    failed ??= e;
    failOpen?.(failed);
    for (const p of pend.values()) p.reject(failed);
    pend.clear();
  };
  const opened = new Promise((resolve, reject) => {
    ws.onopen = resolve;
    failOpen = reject;
  });
  ws.onerror = () => fail(new Error(`cannot reach the node at ${shown}`));
  ws.onclose = () => fail(new Error(`the node at ${shown} closed the connection`));
  ws.onmessage = (e) => {
    let m;
    try {
      m = JSON.parse(e.data);
    } catch {
      fail(new Error(`the node at ${shown} sent a frame that is not JSON`));
      ws.close();
      return;
    }
    const p = pend.get(m?.id);
    if (!p) return;
    pend.delete(m.id);
    p.settle(m);
  };
  const timer = setTimeout(() => {
    fail(new Error(`the node at ${shown} did not answer within ${Math.round(timeoutMs / 1000)} s`));
    ws.close();
  }, timeoutMs);
  try {
    await opened;
    const call = (method, params = {}) => new Promise((resolve, reject) => {
      if (failed) return reject(failed);
      const i = ++id;
      pend.set(i, { reject, settle: (m) => (m.error ? reject(new Error(`${method}: ${JSON.stringify(m.error)}`)) : resolve(m.params)) });
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
export async function pickFundingUtxo(nodeUrl, xOnly, network, needSompi, timeoutMs = 20_000) {
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
  }, timeoutMs);
}
