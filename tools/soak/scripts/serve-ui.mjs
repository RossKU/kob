// Serves the web app (a `vite build` of web/ copied to run/web-dist, built against the TN10 kob-wasm) with a config pointing at the soak's
// indexer (exec-a, CORS enabled) and its TN10 node, plus the soak registry. Read-only market pages need no wallet.
//
//   node scripts/serve-ui.mjs [--config run/config.json] [--public]   -> http://<ui.listen>/#/market/<token>
//
// --public (or `ui.public: true`): everything goes through this one origin, so the page can be put behind a tunnel or proxy
// (https): the app is configured with `<origin>/indexer` (read-only GET/HEAD relay to the indexer) and `wss://<host>/node`
// (WebSocket relay to the TN10 node). Nothing else of the soak is reachable through it.
//
// The app is configured with `quoteTokens` (TUSD = USD): it shows KAS/USD, ETH/USD (TETH) and BTC/USD (TBTC) charts through the TUSD book.
import { createServer, request as httpRequest } from 'node:http';
import { connect } from 'node:net';
import { existsSync, readFileSync, statSync } from 'node:fs';
import { dirname, extname, isAbsolute, join, normalize, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

const HERE = dirname(fileURLToPath(import.meta.url));
const argv = process.argv.slice(2);
const configPath = resolve(argv.includes('--config') ? argv[argv.indexOf('--config') + 1] : join(HERE, '..', 'run', 'config.json'));
const cfg = JSON.parse(readFileSync(configPath, 'utf8'));
const RUN = isAbsolute(cfg.runDir) ? cfg.runDir : resolve(dirname(configPath), cfg.runDir);
const DIST = join(RUN, 'web-dist');
const [host, port] = (cfg.ui?.listen ?? '127.0.0.1:8490').split(':');
const PUBLIC = argv.includes('--public') || cfg.ui?.public === true;
const INDEXER = new URL(cfg.ui?.indexerUrl ?? cfg.indexerUrl);
const NODE = new URL(cfg.nodeUrl);
const TYPES = {
  '.html': 'text/html; charset=utf-8',
  '.js': 'text/javascript',
  '.mjs': 'text/javascript',
  '.css': 'text/css',
  '.json': 'application/json',
  '.wasm': 'application/wasm',
  '.svg': 'image/svg+xml',
  '.png': 'image/png',
  '.ico': 'image/x-icon',
  '.map': 'application/json',
};

// the origin the browser sees (a tunnel terminates TLS and forwards the Host header)
const origin = (req) => {
  const proto = String(req.headers['x-forwarded-proto'] ?? '').split(',')[0].trim() || 'http';
  return { proto, host: req.headers.host ?? `${host}:${port}` };
};

// the USD quote token of the soak (TUSD tracks 1 USD worth of KAS): the app shows KAS/USD and every other token in USD through it
const quoteTokens = () => {
  try {
    const st = JSON.parse(readFileSync(join(RUN, 'state.json'), 'utf8'));
    return st.token?.covenantId ? { [st.token.covenantId]: 'USD' } : {};
  } catch {
    return {};
  }
};

const appConfig = (req) => {
  if (!PUBLIC) {
    return { network: 'testnet-10', indexerUrl: INDEXER.href.replace(/\/$/, ''), nodeUrl: cfg.nodeUrl, registryUrl: './registry/tokens.json', features: { kastle: false }, quoteTokens: quoteTokens() };
  }
  const o = origin(req);
  return {
    network: 'testnet-10',
    indexerUrl: `${o.proto}://${o.host}/indexer`,
    nodeUrl: `${o.proto === 'https' ? 'wss' : 'ws'}://${o.host}/node`,
    registryUrl: './registry/tokens.json',
    features: { kastle: false },
    quoteTokens: quoteTokens(),
  };
};

// read-only relay to the indexer: GET/HEAD only, path under /v1
function relayIndexer(req, res, path) {
  if (req.method !== 'GET' && req.method !== 'HEAD') return void res.writeHead(405, { allow: 'GET, HEAD' }).end();
  const rest = path.slice('/indexer'.length) || '/';
  if (!rest.startsWith('/v1/') && rest !== '/v1') return void res.writeHead(404).end();
  const search = new URL(req.url ?? '/', 'http://x').search;
  const up = httpRequest(
    { host: INDEXER.hostname, port: INDEXER.port, method: req.method, path: rest + search, headers: { accept: req.headers.accept ?? '*/*' } },
    (r) => {
      const headers = { ...r.headers };
      delete headers['access-control-allow-origin'];
      res.writeHead(r.statusCode ?? 502, headers);
      r.pipe(res);
    },
  );
  up.on('error', () => res.headersSent || res.writeHead(502).end());
  up.end();
}

// raw WebSocket relay: replay the upgrade request to the target and pipe both sockets
function relayUpgrade(req, socket, head, target, targetPath) {
  const up = connect(Number(target.port || (target.protocol === 'wss:' ? 443 : 80)), target.hostname, () => {
    const lines = [`GET ${targetPath} HTTP/1.1`, `Host: ${target.host}`];
    for (const [k, v] of Object.entries(req.headers)) {
      if (['host', 'origin', 'x-forwarded-for', 'x-forwarded-proto', 'x-forwarded-host', 'cf-connecting-ip', 'cf-ray', 'cf-visitor', 'cf-ipcountry', 'cdn-loop'].includes(k)) continue;
      for (const one of Array.isArray(v) ? v : [v]) lines.push(`${k}: ${one}`);
    }
    up.write(lines.join('\r\n') + '\r\n\r\n');
    if (head?.length) up.write(head);
    up.pipe(socket);
    socket.pipe(up);
  });
  const close = () => {
    up.destroy();
    socket.destroy();
  };
  up.on('error', close);
  socket.on('error', close);
}

const server = createServer((req, res) => {
  const path = decodeURIComponent(new URL(req.url ?? '/', 'http://x').pathname);
  if (path === '/config.json') {
    res.writeHead(200, { 'content-type': 'application/json', 'cache-control': 'no-store' });
    return void res.end(JSON.stringify(appConfig(req)));
  }
  if (path === '/registry/tokens.json') {
    res.writeHead(200, { 'content-type': 'application/json', 'cache-control': 'no-store' });
    return void res.end(readFileSync(join(RUN, 'registry', 'tokens.json')));
  }
  if (PUBLIC && (path === '/indexer' || path.startsWith('/indexer/'))) return relayIndexer(req, res, path);
  if (req.method !== 'GET' && req.method !== 'HEAD') return void res.writeHead(405).end();
  let file = normalize(join(DIST, path === '/' ? 'index.html' : path));
  if (!file.startsWith(DIST)) return void res.writeHead(403).end();
  if (!existsSync(file) || statSync(file).isDirectory()) file = join(DIST, 'index.html');
  res.writeHead(200, { 'content-type': TYPES[extname(file)] ?? 'application/octet-stream' });
  res.end(readFileSync(file));
});

server.on('upgrade', (req, socket, head) => {
  const path = new URL(req.url ?? '/', 'http://x').pathname;
  if (PUBLIC && path === '/node') return relayUpgrade(req, socket, head, NODE, NODE.pathname || '/');
  if (PUBLIC && path.startsWith('/indexer/v1/')) return relayUpgrade(req, socket, head, INDEXER, path.slice('/indexer'.length));
  socket.destroy();
});

server.listen(Number(port), host, () =>
  console.log(JSON.stringify({ t: new Date().toISOString(), msg: 'ui listening', url: `http://${host}:${port}/`, dist: DIST, public: PUBLIC })),
);
