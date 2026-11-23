// The bot key's outputs read from a node over JSON wRPC (R3T-3), against a local WebSocket server.
import assert from 'node:assert/strict';
import { createHash } from 'node:crypto';
import { createServer } from 'node:http';
import { test } from 'node:test';
import { isJsonWrpc, keyOutputs, wrpcCall } from '../src/node.mjs';

const KEY = 'a1'.repeat(32);
const SPK = `000020${KEY}ac`;

/** A server that answers each client text frame with `answer(request)` (null: no answer). */
async function node(answer) {
  const sockets = new Set();
  const server = createServer();
  server.on('upgrade', (req, socket) => {
    sockets.add(socket);
    const accept = createHash('sha1').update(`${req.headers['sec-websocket-key']}258EAFA5-E914-47DA-95CA-C5AB0DC85B11`).digest('base64');
    socket.write(`HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Accept: ${accept}\r\n\r\n`);
    socket.on('error', () => {});
    socket.on('data', (buf) => {
      if ((buf[0] & 0x0f) !== 1) return; // text frames only
      let len = buf[1] & 0x7f;
      let at = 2;
      if (len === 126) { len = buf.readUInt16BE(2); at = 4; }
      const mask = buf.subarray(at, at + 4);
      const body = Buffer.from(buf.subarray(at + 4, at + 4 + len).map((b, i) => b ^ mask[i % 4]));
      const reply = answer(JSON.parse(body.toString()));
      if (reply == null) return;
      const text = Buffer.from(typeof reply === 'string' ? reply : JSON.stringify(reply));
      const head = text.length < 126 ? Buffer.from([0x81, text.length]) : Buffer.from([0x81, 126, text.length >> 8, text.length & 255]);
      socket.write(Buffer.concat([head, text]));
    });
  });
  await new Promise((r) => server.listen(0, '127.0.0.1', r));
  return { url: `ws://127.0.0.1:${server.address().port}`, close: () => { for (const s of sockets) s.destroy(); server.close(); } };
}

test('keyOutputs: the key\'s plain outputs with their transaction ids; covenant and other outputs left out', async () => {
  const entry = (txid, amount, extra = {}) => ({ address: 'x', outpoint: { transactionId: txid, index: 0 }, utxoEntry: { amount: String(amount), scriptPublicKey: SPK, blockDaaScore: '1', isCoinbase: false, covenantId: null, ...extra } });
  const n = await node((m) => (m.method === 'getUtxosByAddresses' ? { id: m.id, method: m.method, params: { entries: [
    entry('aa'.repeat(32), 500),
    entry('bb'.repeat(32), 700, { covenantId: 'cc'.repeat(32) }),
    entry('dd'.repeat(32), 900, { scriptPublicKey: '0000aa' }),
  ] } } : null));
  try {
    assert.deepEqual(await keyOutputs(n.url, 'kaspatest:x', KEY, 2000), [{ txid: 'aa'.repeat(32), index: 0, amount: 500 }]);
  } finally {
    n.close();
  }
});

test('wrpcCall: a node that never answers or sends a frame that is not JSON fails the call', async () => {
  const silent = await node(() => null);
  try {
    await assert.rejects(wrpcCall(silent.url, 'getUtxosByAddresses', {}, 300), /did not answer/);
  } finally {
    silent.close();
  }
  const junk = await node(() => 'not json');
  try {
    await assert.rejects(wrpcCall(junk.url, 'getUtxosByAddresses', {}, 2000), /not JSON/);
  } finally {
    junk.close();
  }
});

test('isJsonWrpc: a JSON port yes; Borsh, the resolver default and other schemes no', () => {
  assert.equal(isJsonWrpc('ws://127.0.0.1:18210'), true);
  assert.equal(isJsonWrpc('wss://node.example/json'), true);
  assert.equal(isJsonWrpc('ws://127.0.0.1:17210'), false);
  assert.equal(isJsonWrpc('wss://node.example/borsh'), false);
  assert.equal(isJsonWrpc(null), false);
  assert.equal(isJsonWrpc('resolver'), false);
});
