// The bot key's plain KAS outputs, read from the node over JSON wRPC (R3T-3: the proof that a
// fill paid the bot). No `kob` command lists them with their transaction ids, so this is the one
// call the bot makes to the node itself: getUtxosByAddresses for its own address.

/** Whether `url` is a JSON wRPC endpoint (not Borsh, not the CLI's resolver default). */
export function isJsonWrpc(url) {
  if (typeof url !== 'string' || !/^wss?:\/\//.test(url)) return false;
  const base = url.split(/[?#]/)[0].replace(/\/+$/, '');
  if (base.endsWith('/borsh')) return false;
  const rest = base.split('://')[1];
  return !(!rest.includes('/') && /:(17110|17210|17510|17610)$/.test(rest));
}

/**
 * One JSON wRPC call. A node that does not answer within `timeoutMs`, closes the socket or sends
 * a frame that is not JSON fails the call.
 */
export function wrpcCall(url, method, params, timeoutMs = 15_000) {
  const shown = url.replace(/\/\/[^@/]*@/, '//').replace(/\?.*$/, '');
  return new Promise((resolve, reject) => {
    let done = false;
    let ws;
    const finish = (err, v) => {
      if (done) return;
      done = true;
      clearTimeout(timer);
      try { ws?.close(); } catch { /* closed */ }
      if (err) reject(err); else resolve(v);
    };
    const timer = setTimeout(() => finish(new Error(`the node at ${shown} did not answer within ${Math.round(timeoutMs / 1000)} s`)), timeoutMs);
    try {
      ws = new WebSocket(url);
    } catch (e) {
      finish(new Error(`cannot reach the node at ${shown}: ${e.message}`));
      return;
    }
    ws.onopen = () => ws.send(JSON.stringify({ id: 1, method, params }));
    ws.onerror = () => finish(new Error(`cannot reach the node at ${shown}`));
    ws.onclose = () => finish(new Error(`the node at ${shown} closed the connection`));
    ws.onmessage = (e) => {
      let m;
      try { m = JSON.parse(e.data); } catch { finish(new Error(`the node at ${shown} sent a frame that is not JSON`)); return; }
      if (m?.id !== 1) return;
      if (m.error) finish(new Error(`${method}: ${JSON.stringify(m.error)}`));
      else finish(null, m.params);
    };
  });
}

/**
 * The plain (no covenant) outputs at the key's own P2PK script: [{ txid, index, amount }] with
 * the amount in sompi.
 */
export async function keyOutputs(url, address, xOnly, timeoutMs) {
  const spk = `000020${xOnly}ac`;
  const r = await wrpcCall(url, 'getUtxosByAddresses', { addresses: [address] }, timeoutMs);
  const out = [];
  for (const e of r?.entries ?? []) {
    const u = e?.utxoEntry;
    const txid = e?.outpoint?.transactionId;
    if (!u || u.covenantId != null || u.scriptPublicKey !== spk || !/^[0-9a-f]{64}$/.test(txid ?? '')) continue;
    const amount = Number(u.amount);
    if (Number.isSafeInteger(amount) && amount > 0) out.push({ txid, index: Number(e.outpoint.index), amount });
  }
  return out;
}
