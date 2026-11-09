// Tiny static file server for the e2e INFRA tests (the placeholder page in e2e/fixtures-site plus the vendored kaspa SDK and the kob-wasm
// web bindings). The real app is served by `vite preview` (see playwright.config.ts); this exists so the infrastructure is testable
// before / without a UI build.   node e2e/serve-static.mjs [--port 4174] [--host 127.0.0.1]
// With `--dist <prefix>` it serves the built app (web/dist) under that sub-path instead, with no response headers beyond the content type: what
// GitHub Pages does for the mirror (playwright.ghpages.config.ts serves `--dist /kob/`). `--dir <name>` serves another build directory of web/ (the landing-only
// build's dist-landing, playwright.landing.config.ts: `--dist / --dir dist-landing`).
import http from 'node:http';
import { once } from 'node:events';
import { createReadStream, statSync } from 'node:fs';
import { extname, join, normalize, resolve, sep } from 'node:path';
import { fileURLToPath, pathToFileURL } from 'node:url';

const WEB = resolve(fileURLToPath(new URL('..', import.meta.url)));
export const DEFAULT_MOUNTS = [
  { prefix: '/sdk/', dir: join(WEB, 'vendor', 'kaspa-web') },
  { prefix: '/kob/', dir: join(WEB, 'wasm', 'web') },
  { prefix: '/', dir: join(WEB, 'e2e', 'fixtures-site') },
];

const TYPES = {
  '.html': 'text/html; charset=utf-8',
  '.js': 'text/javascript; charset=utf-8',
  '.mjs': 'text/javascript; charset=utf-8',
  '.json': 'application/json',
  '.css': 'text/css',
  '.wasm': 'application/wasm',
  '.svg': 'image/svg+xml',
  '.png': 'image/png',
  '.webmanifest': 'application/manifest+json',
  '.txt': 'text/plain; charset=utf-8',
  '.map': 'application/json',
};

/** The built app (web/dist, or another build directory of web/, `dir`) under a sub-path such as `/kob/` (a GitHub Pages project site) or `/`. */
export const distMounts = (prefix, dir = 'dist') => {
  const name = String(prefix).replace(/^\/+|\/+$/g, '');
  const p = name === '' ? '/' : `/${name}/`;
  return [{ prefix: p, dir: join(WEB, dir) }];
};

/** Resolves a URL path against the mounts; null when it escapes its root or does not exist. */
export function resolveStatic(urlPath, mounts = DEFAULT_MOUNTS) {
  let p;
  try {
    p = decodeURIComponent(urlPath);
  } catch {
    return null;
  }
  if (p.includes('\0')) return null;
  for (const m of mounts) {
    if (!p.startsWith(m.prefix)) continue;
    const root = resolve(m.dir);
    const rel = normalize(p.slice(m.prefix.length)).replace(/^[/\\]+/, '');
    let file = resolve(root, rel === '' || rel === '.' ? 'index.html' : rel);
    if (file !== root && !file.startsWith(root + sep)) return null;
    try {
      if (statSync(file).isDirectory()) file = join(file, 'index.html');
      if (statSync(file).isFile()) return file;
    } catch {
      /* try the next mount */
    }
  }
  return null;
}

export async function startStaticServer({ port = 4174, host = '127.0.0.1', mounts = DEFAULT_MOUNTS } = {}) {
  const server = http.createServer((req, res) => {
    const file = req.method === 'GET' || req.method === 'HEAD' ? resolveStatic(new URL(req.url ?? '/', 'http://x').pathname, mounts) : null;
    if (!file) {
      res.writeHead(404, { 'content-type': 'text/plain' });
      res.end('not found');
      return;
    }
    res.writeHead(200, { 'content-type': TYPES[extname(file)] ?? 'application/octet-stream', 'cache-control': 'no-store' });
    if (req.method === 'HEAD') res.end();
    else createReadStream(file).pipe(res);
  });
  server.listen(port, host);
  await once(server, 'listening');
  const actual = server.address().port;
  return {
    url: `http://${host}:${actual}`,
    port: actual,
    async close() {
      server.closeAllConnections?.();
      await new Promise((r) => server.close(() => r(undefined)));
    },
  };
}

if (process.argv[1] && import.meta.url === pathToFileURL(process.argv[1]).href) {
  const args = process.argv.slice(2);
  const arg = (n, d) => (args.includes(n) ? args[args.indexOf(n) + 1] : d);
  const dist = arg('--dist', null);
  const s = await startStaticServer({ port: Number(arg('--port', 4174)), host: arg('--host', '127.0.0.1'), mounts: dist ? distMounts(dist, arg('--dir', 'dist')) : DEFAULT_MOUNTS });
  console.log(dist ? `web/${arg('--dir', 'dist')} under ${distMounts(dist)[0].prefix} on ${s.url}` : `e2e fixtures site on ${s.url}`);
}
