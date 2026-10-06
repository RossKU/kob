// Mock KOB indexer read API + mock Kaspa node in ONE process, with deterministic control endpoints for tests.
//   node mock/server.mjs [--port 8790] [--host 127.0.0.1] [--fund] [--no-seed] [--history] [--pair]
// Importable: `startMockServer({port}) -> {url, close, chain, ...}`. See e2e/README.md for the endpoint list.
import http from 'node:http';
import { once } from 'node:events';
import { readFileSync } from 'node:fs';
import { pathToFileURL } from 'node:url';
import { WebSocketServer } from 'ws';
import { MockChain, DEFAULT_CARRIER } from './chain.mjs';
import { loadKobMock } from './kob.mjs';
import { TEST_PUBKEYS } from './keys.mjs';
import { applyDefaultSeed, applySeed, giveSpec, resolveKey, seedHistory, summary } from './seed.mjs';
import { ApiError, HEX64, Rejection, badRequest, notFound, sleep } from './util.mjs';
import * as V from './views.mjs';
import * as M from './market.mjs';
import * as P from './pairs.mjs';

const CORS = {
  'access-control-allow-origin': '*',
  'access-control-allow-methods': 'GET, POST, OPTIONS',
  'access-control-allow-headers': 'content-type',
  'access-control-max-age': '600',
};
const registryPath = new URL('../../registry/tokens.example.json', import.meta.url);
const MAX_WS_MESSAGE = 4096;
const MAX_WS_SUBSCRIPTIONS = 64;

// ------------------------------------------------------------------------------------------------ WebSocket protocol (api/ws.rs)

/** Canonical channel name, or throws with the indexer's error text. */
export function parseChannel(s) {
  const idErr = () => new Error(`invalid channel \`${s}\`: id must be 64 hex characters`);
  const i = s.indexOf(':');
  if (i < 0) {
    if (s === 'health' || s === 'reorg' || s === 'fills') return s;
    throw new Error(`unknown channel \`${s}\``);
  }
  const kind = s.slice(0, i);
  const id = s.slice(i + 1).toLowerCase();
  if (kind !== 'fills' && kind !== 'book' && kind !== 'order') throw new Error(`unknown channel \`${s}\``);
  if (!HEX64.test(id)) throw idErr();
  return `${kind}:${id}`;
}

/** Applies one client frame to a subscription set and returns the reply (`handle_op`). */
export function handleOp(text, subs) {
  let msg;
  try {
    msg = JSON.parse(text);
  } catch {
    return { type: 'error', data: { message: 'invalid json' } };
  }
  const names = Array.isArray(msg?.channels) ? msg.channels.filter((x) => typeof x === 'string') : [];
  switch (msg?.op) {
    case 'ping':
      return { type: 'pong' };
    case 'subscribe': {
      const errors = [];
      for (const n of names) {
        try {
          const c = parseChannel(n);
          if (!subs.has(c) && subs.size >= MAX_WS_SUBSCRIPTIONS) {
            errors.push(`subscription limit of ${MAX_WS_SUBSCRIPTIONS} reached`);
            break;
          }
          subs.add(c);
        } catch (e) {
          errors.push(e.message);
        }
      }
      return { type: 'subscribed', data: { channels: [...subs].sort(), errors } };
    }
    case 'unsubscribe':
      for (const n of names) {
        try {
          subs.delete(parseChannel(n));
        } catch {
          /* ignored, like the indexer */
        }
      }
      return { type: 'unsubscribed', data: { channels: [...subs].sort() } };
    default:
      return { type: 'error', data: { message: 'unknown op' } };
  }
}

/** The frames one committed batch produces for a set of subscriptions (`frames_for`). */
export function framesFor(ev, subs) {
  const out = [];
  for (const ch of [...subs].sort()) {
    if (ch === 'health') {
      out.push({ channel: ch, type: 'cursor', data: { cursor_hash: ev.cursorHash, cursor_daa: ev.cursorDaa, added_blocks: ev.added, reverted_blocks: ev.reverted } });
    } else if (ch === 'reorg') {
      if (ev.reverted > 0) out.push({ channel: ch, type: 'reorg', data: { reverted_blocks: ev.reverted, added_blocks: ev.added, cursor_daa: ev.cursorDaa } });
    } else if (ch === 'fills' || ch.startsWith('fills:')) {
      const t = ch === 'fills' ? null : ch.slice(6);
      for (const f of ev.fills) if (t === null || f.token === t) out.push({ channel: ch, type: 'fill', data: f });
    } else if (ch.startsWith('book:')) {
      const t = ch.slice(5);
      if (ev.tokens.includes(t)) out.push({ channel: ch, type: 'book', data: { token: t, cursor_daa: ev.cursorDaa } });
    } else if (ch.startsWith('order:')) {
      const c = ch.slice(6);
      if (ev.orders.includes(c)) out.push({ channel: ch, type: 'order', data: { covenant_id: c, cursor_daa: ev.cursorDaa } });
    }
  }
  return out;
}

// ------------------------------------------------------------------------------------------------ server

async function readJson(req) {
  const chunks = [];
  for await (const c of req) chunks.push(c);
  if (chunks.length === 0) return {};
  try {
    return JSON.parse(Buffer.concat(chunks).toString('utf8'));
  } catch {
    throw badRequest('invalid JSON body');
  }
}

/**
 * @param {{port?: number, host?: string, seed?: boolean, fund?: boolean, history?: boolean | object, settleDepthDaa?: number, daaPerSecond?: number,
 *          startDaa?: number, network?: string, healthEveryMs?: number, log?: (line: string) => void}} [opts]
 */
export async function startMockServer(opts = {}) {
  const { port = 8790, host = '127.0.0.1', seed = true, fund = false, history = false, pair = false, healthEveryMs = 15_000, log = () => {} } = opts;
  const kob = loadKobMock();
  const chain = new MockChain(kob, { network: opts.network, settleDepthDaa: opts.settleDepthDaa, daaPerSecond: opts.daaPerSecond, startDaa: opts.startDaa });
  const ctl = { latencyMs: 0 };
  let feeEstimate = null;
  const sessions = new Set();

  const seedAll = (withSeed, withFund, withHistory = false, withPair = false) => {
    if (withSeed) applyDefaultSeed(chain);
    // opt-in token/token pair (EXKCC/EXUSD with resting pair orders of every kind and a KAS book for EXUSD); never part of the default seed
    if (withPair && chain.tokens.size) P.seedPair(chain, withPair === true ? {} : withPair);
    if (withFund) fundDevKeys(chain);
    // opt-in market history (48 h of seeded trades) for charts in dev mode; never part of the default seed the e2e specs count on
    if (withHistory) seedHistory(chain, withHistory === true ? {} : withHistory);
  };
  seedAll(seed, fund, history, pair);

  chain.listeners.add((ev) => {
    for (const s of sessions) {
      for (const f of framesFor(ev, s.subs)) send(s.ws, f);
    }
  });

  const send = (ws, obj) => {
    if (ws.readyState === 1) ws.send(JSON.stringify(obj));
  };

  const wss = new WebSocketServer({ noServer: true, maxPayload: MAX_WS_MESSAGE });
  wss.on('connection', (ws) => {
    const session = { ws, subs: new Set() };
    sessions.add(session);
    ws.on('message', (data) => send(ws, handleOp(data.toString('utf8'), session.subs)));
    ws.on('close', () => sessions.delete(session));
    ws.on('error', () => sessions.delete(session));
  });
  const healthTimer = setInterval(() => {
    const h = V.healthView(chain);
    for (const s of sessions) {
      if (s.subs.has('health')) send(s.ws, { channel: 'health', type: 'health', data: { state: h.state, cursor_daa: h.cursor_daa, node_daa: h.node_daa, lag_daa: h.lag_daa } });
    }
  }, healthEveryMs);
  healthTimer.unref();

  const server = http.createServer(async (req, res) => {
    const reply = (status, body) => {
      const text = JSON.stringify(body);
      res.writeHead(status, { ...CORS, 'content-type': 'application/json', 'content-length': Buffer.byteLength(text) });
      res.end(text);
    };
    if (req.method === 'OPTIONS') {
      res.writeHead(204, CORS);
      res.end();
      return;
    }
    const url = new URL(req.url ?? '/', 'http://mock');
    const path = url.pathname.replace(/\/+$/, '') || '/';
    const isNode = path.startsWith('/node/');
    try {
      if (!path.startsWith('/mock/') && ctl.latencyMs > 0) await sleep(ctl.latencyMs);
      const out = await route(req.method ?? 'GET', path, url.searchParams, req);
      reply(out.status ?? 200, out.body);
    } catch (e) {
      if (e instanceof Rejection) reply(400, { error: e.message });
      else if (e instanceof ApiError) reply(e.status, isNode ? { error: e.message } : { error: { code: e.code, message: e.message } });
      else {
        log(`internal error on ${req.method} ${path}: ${e?.stack ?? e}`);
        reply(500, isNode ? { error: String(e?.message ?? e) } : { error: { code: 'internal', message: String(e?.message ?? e) } });
      }
    }
  });
  server.on('upgrade', (req, socket, head) => {
    if (new URL(req.url ?? '/', 'http://mock').pathname !== '/v1/ws') {
      socket.destroy();
      return;
    }
    wss.handleUpgrade(req, socket, head, (ws) => wss.emit('connection', ws, req));
  });

  const key = (v) => resolveKey(v);

  async function route(method, path, q, req) {
    const get = method === 'GET';
    const post = method === 'POST';
    const ok = (body) => ({ body });
    let m;
    // ---- indexer read API
    if (get && path === '/v1/health') return ok(V.healthView(chain));
    if (get && path === '/v1/health/ready') return V.readyView(chain);
    if (get && path === '/v1/tokens') return ok(V.tokensView(chain));
    if (get && (m = /^\/v1\/books\/([^/]+)$/.exec(path))) return ok(V.bookView(chain, m[1], q));
    if (get && path === '/v1/orders') return ok(V.ordersPage(chain, q));
    if (get && (m = /^\/v1\/orders\/([^/]+)$/.exec(path))) {
      const id = V.parseHash('covenant_id', m[1]);
      const o = chain.orders.get(id);
      if (!o) throw notFound('unknown covenant id');
      return ok(V.orderView(chain, o, V.ctxOf(chain), true));
    }
    if (get && (m = /^\/v1\/orders\/([^/]+)\/events$/.exec(path))) {
      const id = V.parseHash('covenant_id', m[1]);
      if (!chain.orders.has(id)) throw notFound('unknown covenant id');
      return ok(V.orderEventsPage(chain, id, q));
    }
    if (get && path === '/v1/fills') return ok(V.fillsPage(chain, q));
    if (get && path === '/v1/strays') return ok(V.straysView(chain, q));
    if (get && path === '/v1/token-events') return ok(V.tokenEventsView(chain, q));
    if (get && path === '/v1/token-utxos') return ok(V.tokenUtxosPage(chain, q));
    // ---- market data: trades, candles, 24 h stats, depth
    if (get && (m = /^\/v1\/trades\/([^/]+)$/.exec(path))) return ok(M.tradesPage(chain, m[1], q));
    if (get && (m = /^\/v1\/candles\/([^/]+)$/.exec(path))) return ok(M.candlesView(chain, m[1], q));
    if (get && (m = /^\/v1\/stats\/([^/]+)$/.exec(path))) return ok(M.statsView(chain, m[1]));
    if (get && (m = /^\/v1\/depth\/([^/]+)$/.exec(path))) return ok(M.depthView(chain, m[1], q));
    // ---- token/token pairs (pair orders): the pair book, the pair list, candles derived from the two KAS series, pair fills (pairs.mjs)
    if (get && (m = /^\/v1\/pairs\/([^/]+)\/([^/]+)\/book$/.exec(path))) return ok(P.pairBookView(chain, m[1], m[2], q));
    if (get && (m = /^\/v1\/pairs\/([^/]+)\/([^/]+)\/candles$/.exec(path))) return ok(P.pairCandlesView(chain, m[1], m[2], q));
    if (get && (m = /^\/v1\/pairs\/([^/]+)\/([^/]+)\/fills$/.exec(path))) return ok(P.pairFillsView(chain, m[1], m[2], q));
    if (get && path === '/v1/pairs') return ok(P.pairsView(chain, q));

    // ---- token registry (the app's registryUrl points here in e2e runs)
    if (get && path === '/registry/tokens.json') return ok(JSON.parse(readFileSync(registryPath, 'utf8')));

    // ---- mock node
    if (get && path === '/node/info') {
      if (chain.health.state === 'node_unavailable') throw new ApiError(503, 'node_unavailable', 'mock: node unavailable');
      return ok(chain.nodeInfo());
    }
    // the node's getFeeEstimate (the wallet's fee policy reads it); `POST /mock/fee-estimate` { priority, normal, low } sets it, { off: true } removes the route
    if (get && path === '/node/fee-estimate') {
      if (!feeEstimate) throw notFound('no such route');
      return ok({ estimate: { priorityBucket: { feerate: feeEstimate.priority, estimatedSeconds: 1 }, normalBuckets: [{ feerate: feeEstimate.normal, estimatedSeconds: 30 }], lowBuckets: [{ feerate: feeEstimate.low, estimatedSeconds: 1800 }] } });
    }
    if (post && path === '/node/utxos') {
      const body = await readJson(req);
      return ok({ entries: chain.utxosByAddresses(body.addresses) });
    }
    if (post && path === '/node/submit') {
      const body = await readJson(req);
      return ok(chain.submit(body.transaction));
    }

    // ---- control API
    if (path.startsWith('/mock/')) return control(method, path, q, req);
    throw notFound('no such route');
  }

  async function control(method, path, q, req) {
    const get = method === 'GET';
    const post = method === 'POST';
    const ok = (body) => ({ body });
    const body = post ? await readJson(req) : {};
    if (post && path === '/mock/reset') {
      chain.reset();
      ctl.latencyMs = 0;
      seedAll(body.seed !== false, body.fund === true, body.history ?? false, body.pair ?? false);
      return ok(summary(chain));
    }
    if (post && path === '/mock/fee-estimate') {
      feeEstimate = body.off ? null : { priority: Number(body.priority ?? 100), normal: Number(body.normal ?? 100), low: Number(body.low ?? 100) };
      return ok({ feeEstimate });
    }
    if (post && path === '/mock/seed') {
      const out = applySeed(chain, body);
      return ok(body.pair ? { ...out, pair: P.seedPair(chain, body.pair === true ? {} : body.pair) } : out);
    }
    if (post && path === '/mock/utxo') {
      const spec = { ...body };
      if (spec.token !== undefined && spec.amount !== undefined) spec.tokens = [{ token: spec.token, amount: spec.amount, count: spec.count, carrier: spec.carrier }];
      return ok(giveSpec(chain, spec));
    }
    if (post && path === '/mock/fill') {
      if (!HEX64.test(body.covenant_id ?? '')) throw badRequest('covenant_id must be 64 hex characters');
      // `amount`: base units (default: one whole token or the minimum fill if larger, at most everything left); `price`: sompi per whole token.
      // A pair order is filled by a real batch: `via` inventory | netting | route, `against` (netting), `leg` (a conditional), `mode` (evidence 0 | 1)
      return ok(chain.simulateFill(body.covenant_id, {
        amount: body.amount, price: body.price, taker: body.taker ? key(body.taker) : undefined, via: body.via, against: body.against, leg: body.leg, mode: body.mode, merge: body.merge,
      }));
    }
    if (post && path === '/mock/stray') {
      if (!HEX64.test(body.covenant_id ?? '')) throw badRequest('covenant_id must be 64 hex characters');
      // `token` (optional): a FOREIGN stray of another token: a known ticker / covenant id, or a new covenant id (registered unlisted, `program`, `ticker`)
      let token = body.token ?? null;
      if (typeof token === 'string' && HEX64.test(token) && !chain.tokens.has(token)) token = { covenant_id: token, program: body.program, ticker: body.ticker, decimals: body.decimals };
      if (token && typeof token === 'object' && !HEX64.test(token.covenant_id ?? '')) throw badRequest('token.covenant_id must be 64 hex characters');
      return ok(chain.seedStray(body.covenant_id, BigInt(body.amount), body.carrier === undefined ? undefined : BigInt(body.carrier), token));
    }
    if (post && path === '/mock/arm') {
      if (!HEX64.test(body.covenant_id ?? '')) throw badRequest('covenant_id must be 64 hex characters');
      // a pair stop: `mode` 0 (two KAS-book fills) or 1 (a resting pair order, default) of evidence, in a real update batch
      return ok(chain.armOrder(body.covenant_id, { mode: body.mode }));
    }
    if (post && path === '/mock/trail') {
      if (!HEX64.test(body.covenant_id ?? '')) throw badRequest('covenant_id must be 64 hex characters');
      // a pair trailing stop ratchets one step on evidence beyond its stop (`mode` as /mock/arm)
      return ok(chain.armOrder(body.covenant_id, { mode: body.mode, trail: true }));
    }
    if (post && path === '/mock/frozen') {
      if (!HEX64.test(body.covenant_id ?? '')) throw badRequest('covenant_id must be 64 hex characters');
      return ok(chain.setPossiblyFrozen(body.covenant_id, body.frozen !== false));
    }
    if (post && path === '/mock/advance-daa') {
      const n = body.daa !== undefined ? Number(body.daa) : body.seconds !== undefined ? Number(body.seconds) * 10 : NaN;
      return ok({ daa: chain.advanceDaa(n) });
    }
    if (post && path === '/mock/fail-next-submit') {
      chain.failNext = { remaining: Number(body.count ?? 1), message: String(body.message ?? 'mock: injected submit failure') };
      return ok({ remaining: chain.failNext.remaining });
    }
    if (post && path === '/mock/latency') {
      ctl.latencyMs = Math.max(0, Number(body.ms ?? 0));
      return ok({ ms: ctl.latencyMs });
    }
    if (post && path === '/mock/health') {
      chain.health = { state: String(body.state ?? 'following'), lagDaa: Number(body.lag_daa ?? 0) };
      chain.emit({ orders: [], tokens: [], fills: [], added: 0, reverted: 0 });
      return ok(V.healthView(chain));
    }
    if (post && path === '/mock/reorg') {
      chain.reorg(Number(body.blocks ?? 1));
      return ok({ reverted: Number(body.blocks ?? 1) });
    }
    if (post && path === '/mock/revert-fill') {
      if (!HEX64.test(body.covenant_id ?? '')) throw badRequest('covenant_id must be 64 hex characters');
      return ok(chain.revertLastFill(body.covenant_id, Number(body.blocks ?? 1)));
    }
    if (post && path === '/mock/resync') {
      for (const s of sessions) send(s.ws, { type: 'resync', data: { missed_events: Number(body.missed ?? 1) } });
      return ok({ sessions: sessions.size });
    }
    if (post && path === '/mock/ws-close') {
      const n = sessions.size;
      for (const s of sessions) s.ws.terminate();
      sessions.clear();
      return ok({ closed: n });
    }
    if (get && path === '/mock/submitted') {
      const full = q.get('full') !== '0';
      return ok({ items: chain.submissions.map((s) => (full ? s : { ...s, tx: undefined })) });
    }
    if (get && path === '/mock/state') {
      return ok({ ...summary(chain), unix: chain.nowUnix(), ws_clients: sessions.size, latency_ms: ctl.latencyMs, health: chain.health, rejects: chain.rejects });
    }
    if (get && path === '/mock/balance') {
      const pk = key(q.get('key') ?? q.get('pubkey'));
      const tokens = {};
      for (const t of chain.tokens.values()) tokens[t.ticker] = chain.tokenBalance(pk, t.covenant_id).toString();
      return ok({ pubkey: pk, kas: chain.kasBalance(pk).toString(), tokens });
    }
    throw notFound('no such mock route');
  }

  server.listen(port, host);
  await once(server, 'listening');
  const addr = server.address();
  const actualPort = typeof addr === 'object' && addr ? addr.port : port;
  const url = `http://${host}:${actualPort}`;

  return {
    url,
    port: actualPort,
    chain,
    /** live state (alias of `chain`, kept for callers that want the brief's name) */
    state: chain,
    wsClients: () => sessions.size,
    async close() {
      clearInterval(healthTimer);
      for (const s of sessions) s.ws.terminate();
      wss.close();
      server.closeAllConnections?.();
      await new Promise((r) => server.close(() => r(undefined)));
    },
  };
}

/** Dev convenience (`--fund`): alice and bob get 1000 KAS and 100 whole tokens of the first token each (two UTXOs of 50). */
export function fundDevKeys(chain) {
  const first = [...chain.tokens.keys()][0];
  for (const name of ['alice', 'bob']) {
    giveSpec(chain, { key: name, kas: 500_00000000n, count: 2 });
    if (first) giveSpec(chain, { key: name, tokens: [{ token: first, amount: 50n * BigInt(chain.tokens.get(first).scale ?? 100_000_000), count: 2 }] });
  }
}

// ------------------------------------------------------------------------------------------------ CLI

if (process.argv[1] && import.meta.url === pathToFileURL(process.argv[1]).href) {
  const args = process.argv.slice(2);
  const arg = (name, dflt) => {
    const i = args.indexOf(name);
    return i >= 0 ? args[i + 1] : dflt;
  };
  const srv = await startMockServer({
    port: Number(arg('--port', process.env.MOCK_PORT ?? 8790)),
    host: arg('--host', '127.0.0.1'),
    seed: !args.includes('--no-seed'),
    fund: args.includes('--fund'),
    history: args.includes('--history'),
    pair: args.includes('--pair'),
    log: (l) => console.error(l),
  });
  console.log(`kob mock server (indexer + node) listening on ${srv.url}`);
  console.log(`  indexer REST  ${srv.url}/v1/health   WebSocket ${srv.url.replace('http', 'ws')}/v1/ws`);
  console.log(`  mock node     ${srv.url}/node/info   control ${srv.url}/mock/state`);
  console.log(`  test keys (x-only pubkeys): ${Object.entries(TEST_PUBKEYS).map(([k, v]) => `${k}=${v}`).join(' ')}`);
  console.log(`  default carrier ${DEFAULT_CARRIER} sompi`);
  const stop = () => srv.close().then(() => process.exit(0));
  process.on('SIGINT', stop);
  process.on('SIGTERM', stop);
}
