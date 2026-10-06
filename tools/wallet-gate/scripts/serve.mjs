// Tiny static server for the gate page (no dependencies). Serves ONLY: web/, lib/, artifacts/, state/, vendor/kaspa-web/.
//   node scripts/serve.mjs [port]      -> http://localhost:8787/
// http://localhost is a secure context, so wallet extensions inject their providers.
import { createServer } from 'node:http';
import { readFile, appendFile, mkdir } from 'node:fs/promises';
import { existsSync } from 'node:fs';
import { join, extname, normalize, sep } from 'node:path';
import { ROOT } from '../lib/node-kaspa.mjs';
import { loadEnv } from '../lib/env.mjs';

const PORT = Number(process.argv[2] || process.env.PORT || 8787);
const ALLOWED = ['web', 'lib', 'artifacts', 'state', join('vendor', 'kaspa-web')];
const MIME = { '.html': 'text/html; charset=utf-8', '.mjs': 'text/javascript; charset=utf-8', '.js': 'text/javascript; charset=utf-8', '.json': 'application/json', '.wasm': 'application/wasm', '.css': 'text/css', '.md': 'text/plain; charset=utf-8' };
const env = loadEnv();

const server = createServer(async (req, res) => {
  try {
    const url = new URL(req.url, 'http://localhost');
    if (url.pathname === '/api/config') {
      res.setHeader('content-type', 'application/json');
      return res.end(JSON.stringify({ nodeWs: env.NODE_WS || '', network: 'testnet-10', recipientPubkey: env.DEV_PUBKEY || '' }));
    }
    if (url.pathname === '/api/result' && req.method === 'POST') {
      let body = '';
      for await (const c of req) body += c;
      await mkdir(join(ROOT, 'out'), { recursive: true });
      await appendFile(join(ROOT, 'out', 'page-results.jsonl'), body.replace(/\n\s*/g, ' ') + '\n');
      res.statusCode = 204;
      return res.end();
    }
    let p = url.pathname === '/' ? '/web/index.html' : decodeURIComponent(url.pathname);
    const rel = normalize(p).replace(/^[/\\]+/, '');
    if (rel.includes('..') || !ALLOWED.some((d) => rel === d || rel.startsWith(d + sep) || rel.startsWith(d + '/'))) { res.statusCode = 404; return res.end('not found'); }
    const file = join(ROOT, rel);
    if (!existsSync(file)) { res.statusCode = 404; return res.end('not found'); }
    res.setHeader('content-type', MIME[extname(file)] || 'application/octet-stream');
    res.setHeader('cache-control', 'no-store');
    res.end(await readFile(file));
  } catch (e) {
    res.statusCode = 500;
    res.end(String(e));
  }
});
server.listen(PORT, '127.0.0.1', () => console.log(`wallet gate page: http://localhost:${PORT}/  (node ${env.NODE_WS || 'public resolver'})`));
