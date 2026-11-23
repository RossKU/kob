// The funding lookup against a node that accepts the socket and then misbehaves (R3T-4).
import assert from 'node:assert/strict';
import { createHash } from 'node:crypto';
import { createServer } from 'node:http';
import { test } from 'node:test';
import { KobService } from '../src/service.mjs';
import { withNode } from '../src/wallet.mjs';
import { setup } from './helpers.mjs';

/** A WebSocket server that completes the handshake, then answers each frame with `reply(socket)`. */
async function node(reply = () => {}) {
  const sockets = new Set();
  const server = createServer();
  server.on('upgrade', (req, socket) => {
    sockets.add(socket);
    const accept = createHash('sha1').update(`${req.headers['sec-websocket-key']}258EAFA5-E914-47DA-95CA-C5AB0DC85B11`).digest('base64');
    socket.write(`HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Accept: ${accept}\r\n\r\n`);
    socket.on('data', () => reply(socket));
    socket.on('error', () => {});
  });
  await new Promise((r) => server.listen(0, '127.0.0.1', r));
  return {
    url: `ws://127.0.0.1:${server.address().port}`,
    close: () => { for (const s of sockets) s.destroy(); server.close(); },
  };
}

/** One unmasked text frame, as a server sends it. */
const frame = (text) => Buffer.concat([Buffer.from([0x81, Buffer.byteLength(text)]), Buffer.from(text)]);

test('a node that never answers: the call is rejected at the timeout (R3T-4)', async () => {
  const n = await node();
  try {
    const t0 = Date.now();
    await assert.rejects(withNode(n.url, (call) => call('getBlockDagInfo'), 300), /did not answer/);
    assert.ok(Date.now() - t0 < 5_000);
  } finally {
    n.close();
  }
});

test('a node that answers with a frame that is not JSON: rejected, nothing thrown uncaught (R3T-4)', async () => {
  const n = await node((s) => s.write(frame('not json')));
  try {
    await assert.rejects(withNode(n.url, (call) => call('getBlockDagInfo'), 2_000), /not JSON/);
  } finally {
    n.close();
  }
});

test('a node that closes the socket: the pending call is rejected (R3T-4)', async () => {
  const n = await node((s) => s.destroy());
  try {
    await assert.rejects(withNode(n.url, (call) => call('getBlockDagInfo'), 5_000), /closed|did not answer/);
  } finally {
    n.close();
  }
});

test('a silent node holds the write queue only until the funding lookup times out: a cancel goes on (R3T-4)', async () => {
  const n = await node();
  try {
    const t = setup({ mode: 'auto' });
    const svc = new KobService(t.cfg);
    // The config check wants a wRPC port; a user name of `x:18210` passes it and the socket goes
    // to the test server.
    svc.cfg.node = n.url.replace('ws://', 'ws://x:18210@');
    svc.nodeTimeoutMs = 300;
    const create = svc.write('kob_token_create', { name: 'Test MCP', ticker: 'TMCP', decimals: 2, supply: '100000' });
    const cancel = svc.write('kob_cancel', { id: 'ab'.repeat(32) });
    const r = await create.catch((e) => ({ error: e.message }));
    assert.match(r.reason ?? r.error, /did not answer within/);
    const c = await Promise.race([cancel.then(() => 'done', () => 'done'), new Promise((res) => setTimeout(() => res('stuck'), 5_000))]);
    assert.equal(c, 'done', 'the next write is not blocked');
  } finally {
    n.close();
  }
});
