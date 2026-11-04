// serve-ui --public: the /indexer relay is read-only (GET/HEAD) except for exactly `POST /v1/logo-reports`, whose body is capped. The relay runs as a
// real child process against a fake indexer, so what is checked is what the public site does.
import { test, before, after } from 'node:test';
import assert from 'node:assert/strict';
import { spawn, type ChildProcess } from 'node:child_process';
import { createServer, request, type IncomingMessage, type Server } from 'node:http';
import { mkdtempSync, mkdirSync, rmSync, writeFileSync } from 'node:fs';
import type { AddressInfo } from 'node:net';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { fileURLToPath } from 'node:url';

const SCRIPT = fileURLToPath(new URL('../scripts/serve-ui.mjs', import.meta.url));
interface Seen {
  method: string;
  url: string;
  headers: IncomingMessage['headers'];
  body: string;
}
const seen: Seen[] = [];
let upstreamStatus = 200;
let indexer: Server;
let ui: ChildProcess;
let dir: string;
let uiPort = 0;

const freePort = (): Promise<number> =>
  new Promise((resolve) => {
    const s = createServer().listen(0, '127.0.0.1', () => {
      const p = (s.address() as AddressInfo).port;
      s.close(() => resolve(p));
    });
  });

before(async () => {
  indexer = createServer((req, res) => {
    const chunks: Buffer[] = [];
    req.on('data', (c: Buffer) => chunks.push(c));
    req.on('end', () => {
      seen.push({ method: req.method ?? '', url: req.url ?? '', headers: req.headers, body: Buffer.concat(chunks).toString('utf8') });
      res.writeHead(upstreamStatus, { 'content-type': 'application/json', 'access-control-allow-origin': '*' });
      res.end(upstreamStatus === 200 ? '{"result":"hidden"}' : '{"error":{"code":"bad_request"}}');
    });
  });
  await new Promise<void>((r) => indexer.listen(0, '127.0.0.1', r));
  const indexerPort = (indexer.address() as AddressInfo).port;
  dir = mkdtempSync(join(tmpdir(), 'kob-serve-ui-'));
  mkdirSync(join(dir, 'web-dist'));
  writeFileSync(join(dir, 'web-dist', 'index.html'), '<!doctype html><title>t</title>');
  uiPort = await freePort();
  const cfg = join(dir, 'ui.json');
  writeFileSync(cfg, JSON.stringify({ runDir: dir, nodeUrl: 'ws://127.0.0.1:1', ui: { listen: `127.0.0.1:${uiPort}`, indexerUrl: `http://127.0.0.1:${indexerPort}` } }));
  ui = spawn(process.execPath, [SCRIPT, '--config', cfg, '--public'], { stdio: ['ignore', 'pipe', 'inherit'] });
  await new Promise<void>((resolve, reject) => {
    ui.once('error', reject);
    ui.once('exit', (code) => reject(new Error(`serve-ui exited ${code}`)));
    ui.stdout!.on('data', (d: Buffer) => d.toString().includes('ui listening') && resolve());
  });
});

after(() => {
  ui?.kill();
  indexer?.close();
  if (dir) rmSync(dir, { recursive: true, force: true });
});

interface Reply {
  status: number;
  headers: IncomingMessage['headers'];
  body: string;
}
const call = (method: string, path: string, body?: string | Buffer, headers: Record<string, string> = {}): Promise<Reply> =>
  new Promise((resolve, reject) => {
    const req = request({ host: '127.0.0.1', port: uiPort, method, path, agent: false, headers: body === undefined ? headers : { 'content-type': 'application/json', ...headers } }, (res) => {
      const chunks: Buffer[] = [];
      res.on('data', (c: Buffer) => chunks.push(c));
      res.on('end', () => resolve({ status: res.statusCode ?? 0, headers: res.headers, body: Buffer.concat(chunks).toString('utf8') }));
    });
    req.on('error', reject);
    req.end(body);
  });

const ID = 'ab'.repeat(32);
const REPORT = JSON.stringify({ covenant_id: ID, reason: 'other' });

test('POST /indexer/v1/logo-reports reaches the indexer as POST /v1/logo-reports with its body, and the answer comes back', async () => {
  seen.length = 0;
  upstreamStatus = 200;
  const r = await call('POST', '/indexer/v1/logo-reports', REPORT, { 'cf-connecting-ip': '203.0.113.7' });
  assert.equal(r.status, 200);
  assert.equal(r.body, '{"result":"hidden"}');
  assert.equal(r.headers['access-control-allow-origin'], undefined, 'the relay never passes CORS: the app is same-origin');
  assert.equal(seen.length, 1);
  assert.equal(seen[0]!.method, 'POST');
  assert.equal(seen[0]!.url, '/v1/logo-reports');
  assert.equal(seen[0]!.body, REPORT);
  assert.equal(seen[0]!.headers['content-type'], 'application/json');
  assert.equal(seen[0]!.headers['cf-connecting-ip'], '203.0.113.7', 'the visitor address the tunnel saw goes along (the indexer rate-limits per client)');
});

test("the indexer's validation error is passed through (not 405), and a malformed or forged client header is not forwarded", async () => {
  seen.length = 0;
  upstreamStatus = 400;
  const r = await call('POST', '/indexer/v1/logo-reports?x=1', '{"nope":true}', { 'cf-connecting-ip': 'not-an-ip', 'x-forwarded-for': '198.51.100.9' });
  assert.equal(r.status, 400);
  assert.match(r.body, /bad_request/);
  assert.equal(seen[0]!.url, '/v1/logo-reports', 'the query string is not forwarded');
  assert.equal(seen[0]!.headers['cf-connecting-ip'], undefined);
  assert.equal(seen[0]!.headers['x-forwarded-for'], undefined);
  upstreamStatus = 200;
});

test('a body over the cap is refused with 413 and never reaches the indexer (declared or chunked)', async () => {
  seen.length = 0;
  const declared = await call('POST', '/indexer/v1/logo-reports', Buffer.alloc(4096, 0x61)).catch(() => ({ status: 413 }));
  assert.equal(declared.status, 413);
  // no content-length: chunked
  const chunked = await new Promise<number>((resolve) => {
    const req = request({ host: '127.0.0.1', port: uiPort, method: 'POST', path: '/indexer/v1/logo-reports', headers: { 'content-type': 'application/json', 'transfer-encoding': 'chunked' } }, (res) => {
      res.resume();
      resolve(res.statusCode ?? 0);
    });
    // the relay may close the connection while the body is still being sent: the status line is what counts
    req.on('error', () => resolve(413));
    req.write(Buffer.alloc(1500, 0x61));
    req.end(Buffer.alloc(1500, 0x61));
  });
  assert.equal(chunked, 413);
  assert.equal(seen.length, 0);
  // a body inside the cap still passes
  const edge = JSON.stringify({ covenant_id: ID, reason: 'other', pad: 'x'.repeat(1900) });
  assert.ok(Buffer.byteLength(edge) <= 2048);
  assert.equal((await call('POST', '/indexer/v1/logo-reports', edge)).status, 200);
  assert.equal(seen.length, 1);
});

test("every other write stays refused with 405: other indexer paths, other methods, near-miss paths, and the app's own pages", async () => {
  seen.length = 0;
  for (const [method, path] of [
    ['POST', '/indexer/v1/orders'],
    ['POST', '/indexer/v1/logo-blocklist'],
    ['POST', '/indexer/v1/logo-reports/'],
    ['POST', '/indexer/v1/logo-reports/extra'],
    ['POST', '/indexer/v1/logo-reportsx'],
    ['POST', '/indexer/v1/logo-reports/../orders'],
    ['POST', '/indexer/v1'],
    ['POST', '/indexer'],
    ['PUT', '/indexer/v1/logo-reports'],
    ['DELETE', '/indexer/v1/logo-reports'],
    ['PATCH', '/indexer/v1/logo-reports'],
    ['POST', '/v1/logo-reports'],
    ['POST', '/'],
  ] as const) {
    const r = await call(method, path, REPORT);
    assert.equal(r.status, 405, `${method} ${path}`);
  }
  assert.equal(seen.length, 0, 'none of them reached the indexer');
});

test('reads are relayed as before (GET under /v1 only)', async () => {
  seen.length = 0;
  const ok = await call('GET', '/indexer/v1/logo-blocklist');
  assert.equal(ok.status, 200);
  assert.equal(seen[0]!.method, 'GET');
  assert.equal(seen[0]!.url, '/v1/logo-blocklist');
  assert.equal((await call('GET', '/indexer/other')).status, 404);
});
