// Static server of the REAL-wallet run: serves the built app (dist-real/) on http://localhost:<port> (a secure context, so wallet extensions inject
// their providers) plus a mutable in-memory TN10 test registry at /registry-tn10.test.json. Nothing runs on import.
import http from 'node:http';
import { once } from 'node:events';
import { createReadStream, statSync } from 'node:fs';
import { extname, join, normalize, resolve, sep } from 'node:path';
import { WEB_ROOT } from './common.mjs';

export const DIST_REAL = join(WEB_ROOT, 'dist-real');
export const REGISTRY_PATH = '/registry-tn10.test.json';

const TYPES = {
  '.html': 'text/html; charset=utf-8',
  '.js': 'text/javascript; charset=utf-8',
  '.mjs': 'text/javascript; charset=utf-8',
  '.json': 'application/json',
  '.css': 'text/css',
  '.wasm': 'application/wasm',
  '.svg': 'image/svg+xml',
  '.png': 'image/png',
  '.map': 'application/json',
};

/** Serves `root` on 127.0.0.1 (localhost); `registry` is the JSON object served at REGISTRY_PATH, replaceable through `setRegistry`. */
export async function startAppServer({ root = DIST_REAL, port = 0, registry = { schema_version: 1, network: 'testnet-10', templates: [], tokens: [] } } = {}) {
  let registryJson = registry;
  const base = resolve(root);
  const server = http.createServer((req, res) => {
    const pathname = new URL(req.url ?? '/', 'http://x').pathname;
    if (pathname === REGISTRY_PATH) {
      res.writeHead(200, { 'content-type': 'application/json', 'cache-control': 'no-store' });
      res.end(JSON.stringify(registryJson));
      return;
    }
    let rel;
    try {
      rel = normalize(decodeURIComponent(pathname)).replace(/^[/\\]+/, '');
    } catch {
      rel = null;
    }
    let file = rel === null ? null : resolve(base, rel === '' || rel === '.' ? 'index.html' : rel);
    if (file && file !== base && !file.startsWith(base + sep)) file = null;
    try {
      if (file && statSync(file).isDirectory()) file = join(file, 'index.html');
      if (!file || !statSync(file).isFile()) file = null;
    } catch {
      file = null;
    }
    if (!file) {
      res.writeHead(404, { 'content-type': 'text/plain' });
      res.end('not found');
      return;
    }
    res.writeHead(200, { 'content-type': TYPES[extname(file)] ?? 'application/octet-stream', 'cache-control': 'no-store' });
    createReadStream(file).pipe(res);
  });
  server.listen(port, '127.0.0.1');
  await once(server, 'listening');
  const actual = server.address().port;
  return {
    url: `http://localhost:${actual}`,
    port: actual,
    setRegistry(json) {
      registryJson = json;
    },
    async close() {
      server.closeAllConnections?.();
      await new Promise((r) => server.close(() => r(undefined)));
    },
  };
}
