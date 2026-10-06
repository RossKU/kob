// The mock server (mock/server.mjs) is test infrastructure for every e2e run: these tests pin its behaviour. REST shapes follow
// crates/kob-executor/src/indexer/reads.rs; the mock node validates submitted transactions with the real script engine (kob-wasm), so the
// round trips below use kob.build + local signatures + kob.finalize and are consensus-checked twice (locally and by the mock node).
import { createRequire } from 'node:module';
import { afterAll, beforeAll, beforeEach, describe, expect, it } from 'vitest';
import { startMockServer, type MockServer } from '../mock/server.mjs';
import { TEST_PUBKEYS, TEST_SECRETS } from '../mock/keys.mjs';
import { addressOfSpk, decodeAddress, encodeAddress, p2pkSpk, p2shSpk, spkOfAddress } from '../mock/address.mjs';
import { buildOrderState } from '../mock/seed.mjs';
import { loadKobNode } from '../src/kob/wasm.node';
import { signBuilt } from '../src/testing/local-signer';
import type { ActionRequest, BuiltTx, KeyUtxo, TokenUtxo, TxJson } from '../src/kob/types';

const kob = loadKobNode();
const require = createRequire(import.meta.url);
const sdk = require('../vendor/kaspa-node/kaspa.js');
const golden = require('../../crates/kob-protocol/vectors/golden.json');

const ALICE = { sk: TEST_SECRETS.alice, pub: TEST_PUBKEYS.alice };
const GOLDEN_MAKER = '1b84c5567b126440995d3ed5aaba0565d71e1834604819ff9c17f5e9d5dd078f';
const GLD = '70'.repeat(32); // the covenant id the golden vectors use
const CARRIER = 1_000_000_000n;

let srv: MockServer;
let EXKCC: string;

interface Res { status: number; body: any; headers: Headers }
const get = async (path: string): Promise<Res> => {
  const r = await fetch(srv.url + path);
  return { status: r.status, body: await r.json(), headers: r.headers };
};
const post = async (path: string, body: unknown = {}): Promise<Res> => {
  const r = await fetch(srv.url + path, { method: 'POST', headers: { 'content-type': 'application/json' }, body: JSON.stringify(body) });
  return { status: r.status, body: await r.json(), headers: r.headers };
};

const pkAddress = (pub: string) => addressOfSpk('kaspatest', p2pkSpk(pub))!;

async function keyUtxos(pub: string): Promise<KeyUtxo[]> {
  const r = await post('/node/utxos', { addresses: [pkAddress(pub)] });
  return r.body.entries.map((e: any) => ({ transactionId: e.transactionId, index: e.index, amount: e.amount, blockDaaScore: e.blockDaaScore, covenantId: null, pubkey: pub }));
}
const tokenUtxoOf = (v: any): TokenUtxo => ({ transactionId: v.txid, index: v.index, amount: v.value, blockDaaScore: String(v.created_daa), covenantId: v.token, state: v.state });
async function tokenUtxos(owner: string, token: string): Promise<TokenUtxo[]> {
  const r = await get(`/v1/token-utxos?owner=${owner}&token=${token}&spent=false`);
  return r.body.items.map(tokenUtxoOf);
}

/** build -> sign locally -> finalize -> validate in the engine -> submit to the mock node. */
async function buildSubmit(request: ActionRequest, keys: string[] = [ALICE.sk]): Promise<{ built: BuiltTx; res: Res; tx: TxJson }> {
  const built = kob.build(request);
  const signed = kob.finalize(built, signBuilt(built, keys), { tightenBudgets: true });
  kob.validate(signed);
  const res = await post('/node/submit', { transaction: signed.tx });
  return { built, res, tx: signed.tx };
}

/** A cancelOrder request assembled from the indexer view alone (what the app does). */
async function cancelRequest(cov: string, funding: KeyUtxo[] = []): Promise<ActionRequest> {
  const v = (await get(`/v1/orders/${cov}`)).body;
  return {
    action: 'cancelOrder',
    order: { transactionId: v.current.txid, index: v.current.index, amount: v.current.value, blockDaaScore: String(v.current_daa), covenantId: cov, state: v.state },
    custody: v.custody?.utxo ? tokenUtxoOf(v.custody.utxo) : null,
    strays: (v.strays ?? []).map(tokenUtxoOf),
    tokens: [],
    funding,
    change: null,
    replace: null,
    lockTime: '0',
    records: [],
    fee: { feeRate: null },
  };
}

/** Golden request of `name` with the golden maker replaced by alice and funding / token inputs replaced by REAL mock UTXOs. */
async function adaptGolden(name: string): Promise<ActionRequest> {
  const v = golden.transactions.find((t: { name: string }) => t.name === name);
  const req = JSON.parse(JSON.stringify(v.request).replaceAll(GOLDEN_MAKER, ALICE.pub));
  for (const f of req.funding ?? []) {
    const g = (await post('/mock/utxo', { key: 'alice', kas: f.amount })).body.kas[0];
    Object.assign(f, { transactionId: g.transactionId, index: g.index });
  }
  for (const t of req.tokens ?? []) {
    const g = (await post('/mock/utxo', { key: 'alice', token: GLD, amount: t.state.amount })).body.tokens[0];
    Object.assign(t, { transactionId: g.transactionId, index: g.index });
  }
  return req;
}

beforeAll(async () => {
  srv = await startMockServer({ port: 0, healthEveryMs: 200 });
  EXKCC = [...srv.chain.tokens.keys()][0];
});
afterAll(async () => {
  await srv.close();
});
beforeEach(async () => {
  await post('/mock/reset', {});
});

// ------------------------------------------------------------------------------------------------------------------------------

describe('address codec', () => {
  it('matches the official kaspa SDK for P2PK (testnet + mainnet) and P2SH addresses', () => {
    for (const sk of Object.values(TEST_SECRETS)) {
      const pk = new sdk.PrivateKey(sk).toPublicKey();
      const xonly = pk.toXOnlyPublicKey().toString();
      expect(encodeAddress('kaspatest', 0, Buffer.from(xonly, 'hex'))).toBe(pk.toAddress('testnet-10').toString());
      expect(encodeAddress('kaspa', 0, Buffer.from(xonly, 'hex'))).toBe(pk.toAddress('mainnet').toString());
      expect(spkOfAddress(pk.toAddress('testnet-10').toString())).toBe(p2pkSpk(xonly));
    }
    const hash = '11'.repeat(32);
    const sdkP2sh = sdk.addressFromScriptPublicKey(new sdk.ScriptPublicKey(0, `aa20${hash}87`), 'testnet-10').toString();
    expect(encodeAddress('kaspatest', 8, Buffer.from(hash, 'hex'))).toBe(sdkP2sh);
    expect(spkOfAddress(sdkP2sh)).toBe(p2shSpk(hash));
    expect(decodeAddress(sdkP2sh)).toMatchObject({ prefix: 'kaspatest', version: 8 });
  });

  it('rejects a corrupted checksum and unsupported versions', () => {
    const a = pkAddress(ALICE.pub);
    const bad = a.slice(0, -1) + (a.endsWith('q') ? 'p' : 'q');
    expect(() => decodeAddress(bad)).toThrow(/checksum/);
    expect(() => spkOfAddress(encodeAddress('kaspatest', 5, Buffer.alloc(32)))).toThrow(/unsupported/);
  });
});

describe('indexer REST', () => {
  it('serves health, ready, tokens with the documented shapes and permissive CORS', async () => {
    const h = await get('/v1/health');
    expect(h.status).toBe(200);
    expect(h.headers.get('access-control-allow-origin')).toBe('*');
    expect(h.body).toMatchObject({ state: 'following', ok: true, network: 'testnet-10', settle_depth_daa: 100, lag_daa: 0 });
    expect(typeof h.body.node_daa).toBe('number');
    expect(h.body.counters.orders_total).toBeGreaterThan(20);
    const r = await get('/v1/health/ready');
    expect(r.status).toBe(200);
    expect(r.body).toMatchObject({ ready: true, reason: 'ok' });
    const t = await get('/v1/tokens');
    expect(t.body.tokens).toHaveLength(1);
    expect(t.body.tokens[0]).toMatchObject({ ticker: 'EXKCC', covenant_id: EXKCC, scale: 100_000_000, decimals: 8 });
    expect(t.body.tokens[0]).not.toHaveProperty('lot_size');
    expect(t.body.tokens[0].open_asks).toBeGreaterThan(9);
    const opt = await fetch(srv.url + '/v1/orders', { method: 'OPTIONS' });
    expect(opt.status).toBe(204);
    expect(opt.headers.get('access-control-allow-origin')).toBe('*');
  });

  it('serves the registry standing of tokens: standing, powers, template_id, family, and an empty ticker for a token outside the registry', async () => {
    const t0 = (await get('/v1/tokens')).body.tokens[0];
    expect(t0).toMatchObject({ standing: 'unverified', powers: [], template_id: null, family: 'kcc20' });
    await post('/mock/seed', {
      tokens: [
        { ticker: 'OFFI', covenant_id: 'ab'.repeat(32), decimals: 2, standing: 'official', template_id: 'kcc20-ref-8x8' },
        { ticker: '', covenant_id: 'cd'.repeat(32), decimals: 2, powers: ['freeze', 'seize'], template_id: 'kcc20-ref-8x8' },
      ],
    });
    const list = (await get('/v1/tokens')).body.tokens;
    expect(list.find((x: any) => x.covenant_id === 'ab'.repeat(32))).toMatchObject({ ticker: 'OFFI', standing: 'official', powers: [], template_id: 'kcc20-ref-8x8' });
    expect(list.find((x: any) => x.covenant_id === 'cd'.repeat(32))).toMatchObject({ ticker: '', standing: 'unverified', powers: ['freeze', 'seize'] });
  });

  it('a possibly frozen order is flagged on the order, dropped from the book and its counts, and clears again', async () => {
    const before = (await get(`/v1/books/${EXKCC}?aggregate=false&depth=50`)).body;
    const ask = before.asks[0];
    const asksBefore = (await get('/v1/tokens')).body.tokens[0].open_asks;
    expect((await get(`/v1/orders/${ask.covenant_id}`)).body.possibly_frozen).toBe(false);
    await post('/mock/frozen', { covenant_id: ask.covenant_id });
    expect((await get(`/v1/orders/${ask.covenant_id}`)).body).toMatchObject({ possibly_frozen: true, status: 'open' });
    const after = (await get(`/v1/books/${EXKCC}?aggregate=false&depth=50`)).body;
    expect(after.asks.map((o: any) => o.covenant_id)).not.toContain(ask.covenant_id);
    expect((await get('/v1/tokens')).body.tokens[0].open_asks).toBe(asksBefore - 1);
    expect((await get(`/v1/orders?token=${EXKCC}&status=active&limit=200`)).body.items.find((o: any) => o.covenant_id === ask.covenant_id).possibly_frozen).toBe(true);
    await post('/mock/frozen', { covenant_id: ask.covenant_id, frozen: false });
    expect((await get(`/v1/orders/${ask.covenant_id}`)).body.possibly_frozen).toBe(false);
  });

  it('seeds a bid whose escrow no longer funds one base unit: it is served with amount 0 (the web app hides it from the book)', async () => {
    await post('/mock/reset', { seed: false });
    await post('/mock/seed', { default: true });
    const r = await post('/mock/seed', { orders: [{ side: 'bid', price: 2_600_000, amount: 100_000_000, value: 1_000_000, maker: 'maker' }] });
    expect(r.status).toBe(200);
    const bids = (await get(`/v1/books/${EXKCC}?depth=50`)).body.bids;
    expect(bids[0]).toMatchObject({ price: '2600000', amount: '0', amount_estimated: true, scale: 100_000_000 });
  });

  it('serves an open-list token (no registry entry, no decimals): no standard scale, its orders quote at their own scale', async () => {
    await post('/mock/seed', { open_token: true });
    const row = (await get('/v1/tokens')).body.tokens.find((x: any) => x.covenant_id === '0f'.repeat(32));
    expect(row).toMatchObject({ ticker: '', standing: 'unverified', powers: [], template_id: 'KCC20Ref_8x8', scale: null, decimals: null, family: 'kcc20' });
    expect(row.open_asks).toBe(3);
    const book = (await get(`/v1/books/${row.covenant_id}?aggregate=false&depth=50`)).body;
    expect(book.asks).toHaveLength(3);
    // the orders carry the scale the web app derives the open-list market from (1000 base units), listed whatever their scale
    expect([...book.asks, ...book.bids].every((o: any) => o.scale === 1000 && o.amount_left === '4000' && o.min_fill === '1')).toBe(true);
    expect(book.asks.map((o: any) => o.price)).toEqual(['51000', '52000', '53000']);
    // market data quote per the scale of the token's first order
    expect((await get(`/v1/depth/${row.covenant_id}`)).body).toMatchObject({ price_basis: '1000', decimals: null });
  });

  it('reports a non-following indexer through health, ready and the node', async () => {
    await post('/mock/health', { state: 'catching_up', lag_daa: 600 });
    expect((await get('/v1/health')).body).toMatchObject({ state: 'catching_up', ok: true, lag_daa: 600, lag_seconds: 60 });
    await post('/mock/health', { state: 'node_unavailable' });
    const ready = await get('/v1/health/ready');
    expect(ready.status).toBe(503);
    expect(ready.body.ready).toBe(false);
    const h = (await get('/v1/health')).body;
    expect(h).toMatchObject({ ok: false, node_daa: null, lag_daa: null });
    const o = (await get('/v1/orders?limit=1')).body.items[0];
    expect(o.confirmations).toBeNull();
    expect(o.settled).toBe(false);
    expect((await get('/node/info')).status).toBe(503);
  });

  it('serves an aggregated book: 10 levels per side, asks ascending, bids descending, string sompi', async () => {
    const b = (await get(`/v1/books/${EXKCC}?depth=50`)).body;
    expect(b).toMatchObject({ token: EXKCC, aggregated: true });
    expect(b.asks).toHaveLength(10);
    expect(b.bids).toHaveLength(10);
    const asks = b.asks.map((l: any) => BigInt(l.price));
    const bids = b.bids.map((l: any) => BigInt(l.price));
    expect(asks).toEqual([...asks].sort((x, y) => (x < y ? -1 : 1)));
    expect(bids).toEqual([...bids].sort((x, y) => (x < y ? 1 : -1)));
    expect(asks[0]).toBeGreaterThan(bids[0]);
    expect(b.asks[0]).toMatchObject({ orders: 2, amount: '700000000', amount_estimated: false, scale: 100_000_000 });
    expect(b.bids[0].amount_estimated).toBe(true); // bids are sized by escrow (their buying power)
    expect((await get(`/v1/books/${EXKCC}?depth=3`)).body.asks).toHaveLength(3);
  });

  it('serves an order-level book (aggregate=false) with quote, expiry and confirmations', async () => {
    const b = (await get(`/v1/books/${EXKCC}?aggregate=false&depth=4`)).body;
    expect(b.aggregated).toBe(false);
    expect(b.asks).toHaveLength(4);
    const o = b.asks[0];
    expect(o).toMatchObject({ contract: 'KobAsk', status: 'open', quote: o.price, expired: false, deadline_passed: false, amount_estimated: false, tip: '0', min_fill: '1', scale: 100_000_000 });
    expect(o.maker).toBe(TEST_PUBKEYS.maker);
    expect(typeof o.confirmations).toBe('number');
    expect(BigInt(o.amount_left)).toBeGreaterThan(0n);
    const same = b.asks.filter((x: any) => x.price === o.price);
    expect(same.length).toBe(2); // price-time: equal prices stay in creation order
  });

  it('pages orders newest first with keyset cursors that cover every order exactly once', async () => {
    const total = (await get('/v1/health')).body.counters.orders_total;
    const seen: string[] = [];
    let cursor: string | null = null;
    let pages = 0;
    do {
      const r: Res = await get(`/v1/orders?limit=7${cursor ? `&cursor=${cursor}` : ''}`);
      expect(r.body.items.length).toBeLessThanOrEqual(7);
      seen.push(...r.body.items.map((o: any) => o.covenant_id));
      cursor = r.body.next_cursor;
      pages++;
    } while (cursor);
    expect(seen).toHaveLength(total);
    expect(new Set(seen).size).toBe(total);
    expect(pages).toBe(Math.ceil(total / 7));
    const seqs = (await get('/v1/orders?limit=200')).body.items.map((o: any) => o.genesis.block_seq);
    expect(seqs).toEqual([...seqs].sort((a, b) => b - a));
  });

  it('filters orders by status / maker / token and validates parameters', async () => {
    const filled = (await get('/v1/orders?status=filled&limit=200')).body.items;
    expect(filled).toHaveLength(8);
    expect(filled.every((o: any) => o.status === 'filled' && BigInt(o.filled_amount) > 0n && o.current === null)).toBe(true);
    const active = (await get('/v1/orders?status=active&limit=200')).body.items;
    expect(active.every((o: any) => o.status === 'open' || o.status === 'partial')).toBe(true);
    expect((await get(`/v1/orders?maker=${ALICE.pub}`)).body.items).toHaveLength(0);
    expect((await get(`/v1/orders?token=${EXKCC}&limit=200`)).body.items.length).toBe(active.length + filled.length);
    for (const [path, msg] of [
      ['/v1/orders?status=bogus', /status must be one of/],
      ['/v1/orders?limit=0', /limit must be a positive integer/],
      ['/v1/orders?token=zz', /token must be 32 bytes of hex/],
      ['/v1/orders?cursor=abc', /invalid cursor/],
      ['/v1/orders?maker=xyz', /maker must be hex/],
      ['/v1/books/zz', /token must be 32 bytes of hex/],
      ['/v1/fills?side=up', /side must be 1\|2\|ask\|bid/],
    ] as const) {
      const r = await get(path);
      expect(r.status, path).toBe(400);
      expect(r.body.error.code).toBe('bad_request');
      expect(r.body.error.message).toMatch(msg);
    }
    const nf = await get(`/v1/orders/${'ab'.repeat(32)}`);
    expect(nf.status).toBe(404);
    expect(nf.body).toEqual({ error: { code: 'not_found', message: 'unknown covenant id' } });
    expect((await get('/v1/nothing')).status).toBe(404);
  });

  it('serves one order with state, exact custody, strays, children, refund due and events', async () => {
    const ask = (await get(`/v1/orders?status=open&limit=200`)).body.items.find((o: any) => o.contract === 'KobAsk' && o.side === 1);
    const v = (await get(`/v1/orders/${ask.covenant_id}`)).body;
    expect(v).toMatchObject({ contract: 'KobAsk', side: 1, in_book: true, listed: true, status: 'open', state_known: true, children: [], strays: [], origin: 'seed' });
    expect(v.state.kind).toBe('KobAsk');
    expect(v.state.state.amountLeft).toBe(v.amount_left);
    expect(v).toMatchObject({ scale: 100_000_000, min_fill: '1', tip: '0', initial_amount: v.amount_left, filled_amount: '0', budget_rate: null });
    expect(v.custody.ok).toBe(true);
    expect(v.custody.expected_amount).toBe(v.amount_left);
    expect(v.custody.utxo).toMatchObject({ role: 'custody', owner: v.covenant_id, spent: false, amount: v.custody.expected_amount });
    expect(v.current).toMatchObject({ value: String(CARRIER) });
    expect(v.refund_due_daa).toBeGreaterThan(v.current_daa);
    expect(v.quote).toBe(v.price);
    const ev = (await get(`/v1/orders/${v.covenant_id}/events`)).body;
    expect(ev.items.map((e: any) => e.kind)).toEqual(['create']);
    expect(ev.next_cursor).toBeNull();
    expect((await get(`/v1/orders/${'cd'.repeat(32)}/events`)).status).toBe(404);
  });

  it('marks confirmations and settled from the DAA depth', async () => {
    await post('/mock/seed', { orders: [{ token: EXKCC, side: 'ask', maker: 'alice', price: 2_505_000, amount: 100_000_000 }] });
    const id = (await get(`/v1/orders?maker=${ALICE.pub}`)).body.items[0].covenant_id;
    const before = (await get(`/v1/orders/${id}`)).body;
    expect(before.settled).toBe(false);
    await post('/mock/advance-daa', { daa: 500 });
    const after = (await get(`/v1/orders/${id}`)).body;
    expect(after.confirmations).toBeGreaterThanOrEqual(500);
    expect(after.settled).toBe(true);
    expect(after.genesis.settled).toBe(true);
  });

  it('pages fills newest first, filters by token / side and serves strays and token events', async () => {
    const all = (await get('/v1/fills?limit=3')).body;
    expect(all.items).toHaveLength(3);
    expect(all.items.every((e: any) => e.kind === 'fill' && typeof e.price === 'string' && BigInt(e.amount) > 0n)).toBe(true);
    expect(all.items[0].id).toBeGreaterThan(all.items[1].id);
    const next = (await get(`/v1/fills?limit=100&before=${all.next_cursor}`)).body;
    expect(next.items).toHaveLength(5);
    expect(next.next_cursor).toBeNull();
    expect((await get(`/v1/fills?token=${'00'.repeat(32)}`)).body.items).toHaveLength(0);
    const asks = (await get('/v1/fills?side=ask&limit=50')).body.items;
    expect(asks.every((e: any) => e.side === 1)).toBe(true);
    expect(asks.length).toBe(4);
    expect((await get('/v1/strays')).body.items).toEqual([]);
    const te = (await get('/v1/token-events')).body.items;
    expect(te[0]).toMatchObject({ token: EXKCC, kind: 'seen' });
  });

  it('lists token UTXOs per owner and token with the decoded KCC-20 state', async () => {
    await post('/mock/utxo', { key: 'bob', token: EXKCC, amount: '300000000', count: 2 });
    const r = (await get(`/v1/token-utxos?owner=${TEST_PUBKEYS.bob}&token=${EXKCC}&spent=false&limit=1`)).body;
    expect(r.items).toHaveLength(1);
    expect(r.next_cursor).not.toBeNull();
    const second = (await get(`/v1/token-utxos?owner=${TEST_PUBKEYS.bob}&cursor=${r.next_cursor}`)).body;
    expect(second.items).toHaveLength(1);
    expect(second.next_cursor).toBeNull();
    expect(r.items[0]).toMatchObject({ role: 'owned', amount: '300000000', value: String(CARRIER), spent: false, token: EXKCC });
    expect(r.items[0].state).toMatchObject({ owner: TEST_PUBKEYS.bob, owner_scheme: 0, amount: '300000000' });
    expect(BigInt((await get(`/mock/balance?key=bob`)).body.tokens.EXKCC)).toBe(600_000_000n);
  });
});

// ------------------------------------------------------------------------------------------------------------------------------

describe('mock node', () => {
  it('reports info, advances the DAA on demand and indexes UTXOs by address', async () => {
    const a = (await get('/node/info')).body;
    expect(a).toMatchObject({ network: 'testnet-10', serverVersion: expect.any(String), daaRateMilli: 10000 });
    expect(BigInt(a.virtualDaaScore)).toBeGreaterThan(0n);
    await post('/mock/advance-daa', { daa: 10_000 });
    const b = (await get('/node/info')).body;
    expect(BigInt(b.virtualDaaScore) - BigInt(a.virtualDaaScore)).toBeGreaterThanOrEqual(10_000n);
    expect(BigInt(b.unixSeconds) - BigInt(a.unixSeconds)).toBeGreaterThanOrEqual(1000n); // 10 DAA/s
    await post('/mock/utxo', { key: 'carol', kas: '250000000', count: 2 });
    const entries = (await post('/node/utxos', { addresses: [pkAddress(TEST_PUBKEYS.carol)] })).body.entries;
    expect(entries).toHaveLength(2);
    expect(entries[0]).toMatchObject({ address: pkAddress(TEST_PUBKEYS.carol), amount: '250000000', isCoinbase: false, covenantId: null, scriptPublicKey: p2pkSpk(TEST_PUBKEYS.carol) });
    // an address produced by the official SDK resolves to the same UTXOs
    const sdkAddr = new sdk.PrivateKey(TEST_SECRETS.carol).toPublicKey().toAddress('testnet-10').toString();
    expect((await post('/node/utxos', { addresses: [sdkAddr] })).body.entries).toHaveLength(2);
    const bad = await post('/node/utxos', { addresses: ['kaspatest:notanaddress'] });
    expect(bad.status).toBe(400);
    expect(typeof bad.body.error).toBe('string');
  });

  it('rejects malformed transactions and injected failures with {error}', async () => {
    const m = await post('/node/submit', { transaction: { id: 'nope' } });
    expect(m.status).toBe(400);
    expect(m.body.error).toMatch(/malformed/);
    await post('/mock/fail-next-submit', { message: 'mempool full', count: 1 });
    const req = await adaptGolden('create.bid');
    const built = kob.build(req);
    const signed = kob.finalize(built, signBuilt(built, [ALICE.sk]), { tightenBudgets: true });
    const f = await post('/node/submit', { transaction: signed.tx });
    expect(f.status).toBe(400);
    expect(f.body).toEqual({ error: 'mempool full' });
    const ok = await post('/node/submit', { transaction: signed.tx });
    expect(ok.status).toBe(200);
    expect(ok.body.transactionId).toBe(signed.tx.id);
  });

  it('refuses double spends, unknown inputs and a UTXO entry that does not match the node', async () => {
    const req = await adaptGolden('create.bid');
    const built = kob.build(req);
    const signed = kob.finalize(built, signBuilt(built, [ALICE.sk]), { tightenBudgets: true });
    expect((await post('/node/submit', { transaction: signed.tx })).status).toBe(200);
    const again = await post('/node/submit', { transaction: signed.tx });
    expect(again.status).toBe(400);
    expect(again.body.error).toMatch(/already accepted/);
    // same funding input, different transaction: double spend
    const req2 = JSON.parse(JSON.stringify(req));
    req2.records = [{ type: 'note', text: 'second' }];
    const b2 = kob.build(req2);
    const s2 = kob.finalize(b2, signBuilt(b2, [ALICE.sk]), { tightenBudgets: true });
    const ds = await post('/node/submit', { transaction: s2.tx });
    expect(ds.status).toBe(400);
    expect(ds.body.error).toMatch(/already spent by/);
    // unknown outpoint
    const req3 = JSON.parse(JSON.stringify(req));
    req3.funding[0].transactionId = 'ee'.repeat(32);
    const b3 = kob.build(req3);
    const s3 = kob.finalize(b3, signBuilt(b3, [ALICE.sk]), { tightenBudgets: true });
    expect((await post('/node/submit', { transaction: s3.tx })).body.error).toMatch(/not in the UTXO set/);
    // entry that lies about its amount
    const fresh = await adaptGolden('create.bid');
    fresh.funding![0].amount = String(BigInt(fresh.funding![0].amount) + 1n);
    const b4 = kob.build(fresh);
    const s4 = kob.finalize(b4, signBuilt(b4, [ALICE.sk]), { tightenBudgets: true });
    expect((await post('/node/submit', { transaction: s4.tx })).body.error).toMatch(/does not match the node/);
  });

  it('runs the script engine: a wrong signature is rejected with the engine message and changes nothing', async () => {
    const req = await adaptGolden('create.bid');
    const built = kob.build(req);
    const signed = kob.finalize(built, signBuilt(built, [ALICE.sk]), { tightenBudgets: true });
    const forged: TxJson = JSON.parse(JSON.stringify(signed.tx));
    const sig = forged.inputs[0].signatureScript;
    forged.inputs[0].signatureScript = sig.slice(0, 4) + '00'.repeat(4) + sig.slice(12);
    const r = await post('/node/submit', { transaction: forged });
    expect(r.status).toBe(400);
    expect(r.body.error).toMatch(/input 0/);
    expect(srv.chain.submissions).toHaveLength(0);
    expect((await post('/node/utxos', { addresses: [pkAddress(ALICE.pub)] })).body.entries.length).toBeGreaterThan(0);
    // the untouched tx is still accepted afterwards
    expect((await post('/node/submit', { transaction: signed.tx })).status).toBe(200);
  });
});

// ------------------------------------------------------------------------------------------------------------------------------

describe('order lifecycle through the mock node', () => {
  it('create ask -> registered (custody, book, events) -> cancel -> tokens back', async () => {
    await post('/mock/utxo', { key: 'alice', kas: '5000000000' });
    await post('/mock/utxo', { key: 'alice', token: EXKCC, amount: '1000000000' }); // 10 whole tokens
    const funding = await keyUtxos(ALICE.pub);
    const tokens = await tokenUtxos(ALICE.pub, EXKCC);
    expect(tokens).toHaveLength(1);
    const state = buildOrderState(srv.chain, { token: EXKCC, side: 'ask', maker: 'alice', price: 2_505_000, amount: 300_000_000 });
    const before = (await get(`/v1/books/${EXKCC}`)).body;

    const created = await buildSubmit({ action: 'createOrder', order: state as any, value: String(CARRIER), tokens, tokenCarrier: String(CARRIER), funding, change: null, lockTime: '0', deadline: null, records: [], fee: { feeRate: null } });
    expect(created.res.status).toBe(200);
    const cov = created.built.covenants[0].covenantId;

    // registered by recovering the KOB1 placement record
    const order = (await get(`/v1/orders/${cov}`)).body;
    expect(order).toMatchObject({ contract: 'KobAsk', maker: ALICE.pub, status: 'open', origin: 'chain', amount_left: '300000000', initial_amount: '300000000', amount_estimated: false, listed: true, in_book: true, price: '2505000', scale: 100_000_000 });
    expect(order.genesis.txid).toBe(created.tx.id);
    expect(order.custody).toMatchObject({ ok: true, expected_amount: '300000000' });
    expect(order.custody.utxo.txid).toBe(created.tx.id);
    const mine = (await get(`/v1/orders?maker=${ALICE.pub}&status=active`)).body.items;
    expect(mine.map((o: any) => o.covenant_id)).toEqual([cov]);
    const after = (await get(`/v1/books/${EXKCC}`)).body;
    expect(after.asks.find((l: any) => l.price === '2505000')).toMatchObject({ amount: '300000000', orders: 1 });
    expect(before.asks.find((l: any) => l.price === '2505000')).toBeUndefined();

    // token UTXOs: custody owned by the order, the change owned by alice (states read from the leader's signature script)
    const owned = (await get(`/v1/token-utxos?owner=${ALICE.pub}&spent=false`)).body.items;
    expect(owned).toHaveLength(1);
    expect(owned[0].amount).toBe('700000000');
    expect((await get(`/v1/token-utxos?owner=${cov}&spent=false`)).body.items.map((t: any) => t.role)).toEqual(['custody']);
    expect((await get(`/v1/token-utxos?owner=${ALICE.pub}&spent=true`)).body.items).toHaveLength(2); // spent=true includes the spent holding
    expect((await get('/v1/token-utxos')).status).toBe(400);

    // the submission log exposes what the wallet was asked to sign
    const sub = (await get('/mock/submitted')).body.items;
    expect(sub).toHaveLength(1);
    expect(sub[0]).toMatchObject({ txid: created.tx.id, created: [{ covenantId: cov, kind: 'KobAsk', output: 0 }] });
    expect(sub[0].records[0]).toMatchObject({ type: 'order', template: 'KobAsk' });

    // cancel: the maker's tokens come back as one P2PK-owned UTXO and the order closes
    const cancel = await buildSubmit(await cancelRequest(cov));
    expect(cancel.res.status).toBe(200);
    const done = (await get(`/v1/orders/${cov}`)).body;
    expect(done).toMatchObject({ status: 'cancelled', current: null, state: null });
    expect(done.custody.utxo).toBeNull();
    expect((await get(`/v1/orders/${cov}/events`)).body.items.map((e: any) => e.kind)).toEqual(['create', 'cancel']);
    expect((await get(`/v1/orders/${cov}/events`)).body.items[1].closes).toBe(true);
    expect((await get(`/v1/token-utxos?owner=${cov}&spent=false`)).body.items).toHaveLength(0);
    expect(srv.chain.tokenBalance(ALICE.pub, EXKCC)).toBe(1_000_000_000n);
    const book = (await get(`/v1/books/${EXKCC}`)).body;
    expect(book.asks.find((l: any) => l.price === '2505000')).toBeUndefined();
    expect((await get(`/v1/orders?maker=${ALICE.pub}&status=cancelled`)).body.items).toHaveLength(1);
  });

  it('create bid -> partially filled -> cancel returns the remaining escrow', async () => {
    await post('/mock/utxo', { key: 'alice', kas: '100000000000' });
    const funding = await keyUtxos(ALICE.pub);
    const state = buildOrderState(srv.chain, { token: EXKCC, side: 'bid', maker: 'alice', price: 2_495_000, amount: 400_000_000 });
    // escrow for 4 whole tokens in up to 2 fills: the budget ceil(n * (price + tip) / scale) and two delivery carriers (kob-wasm bidEscrow)
    const value = BigInt(kob.raw.bidEscrow(JSON.stringify(state), '400000000', '2') as string);
    const used = (n: bigint) => (n * 2_495_000n + 100_000_000n - 1n) / 100_000_000n;
    expect(value).toBe(used(400_000_000n) + 1n + 2n * CARRIER);
    const created = await buildSubmit({ action: 'createOrder', order: state as any, value: value.toString(), tokens: [], tokenCarrier: String(CARRIER), funding, change: null, lockTime: '0', deadline: null, records: [], fee: { feeRate: null } });
    expect(created.res.status).toBe(200);
    const cov = created.built.covenants[0].covenantId;
    let v = (await get(`/v1/orders/${cov}`)).body;
    expect(v).toMatchObject({ contract: 'KobBid', side: 2, status: 'open', amount_estimated: true, initial_amount: null, budget_rate: '2495000', scale: 100_000_000 });
    // a bid's amount is its buying power: an upper bound (the second carrier counts as budget until it is spent)
    expect(BigInt(v.amount_left)).toBeGreaterThanOrEqual(400_000_000n);
    expect(v.custody).toBeUndefined(); // bids hold KAS, not tokens

    const fill = await post('/mock/fill', { covenant_id: cov, amount: '100000000', taker: 'bob' });
    expect(fill.status).toBe(200);
    v = (await get(`/v1/orders/${cov}`)).body;
    // buying power of the continuation: floor((escrow left - carrier) x scale / rate), 3 whole tokens and the 40 base units the per-fill
    // rounding slack (the escrow's +1 sompi) buys
    expect(v).toMatchObject({ status: 'partial', filled_amount: '100000000', amount_left: '300000040' });
    // the continuation keeps the escrow less the budget of the fill and its delivery carrier
    expect(BigInt(v.current.value)).toBe(value - used(100_000_000n) - CARRIER);
    expect(srv.chain.tokenBalance(ALICE.pub, EXKCC)).toBe(100_000_000n); // delivered to the buyer
    expect(srv.chain.kasBalance(TEST_PUBKEYS.bob)).toBe(2_495_000n); // floor(n * price / scale)
    expect((await get(`/v1/fills?token=${EXKCC}&limit=1`)).body.items[0]).toMatchObject({ covenant_id: cov, amount: '100000000', price: '2495000', side: 2 });

    const kasBefore = srv.chain.kasBalance(ALICE.pub);
    const cancel = await buildSubmit(await cancelRequest(cov));
    expect(cancel.res.status).toBe(200);
    expect((await get(`/v1/orders/${cov}`)).body.status).toBe('cancelled');
    expect(srv.chain.kasBalance(ALICE.pub) - kasBefore).toBeGreaterThan(CARRIER); // the escrow of the 3 open tokens and a carrier come back
  });

  it('simulated fills of an ask perform a real transition: partial fill, then the remainder is cancellable in the engine', async () => {
    await post('/mock/seed', { orders: [{ token: EXKCC, side: 'ask', maker: 'alice', price: 2_505_000, amount: 500_000_000 }] });
    const cov = (await get(`/v1/orders?maker=${ALICE.pub}`)).body.items[0].covenant_id;
    const kasBefore = srv.chain.kasBalance(ALICE.pub);
    const f = (await post('/mock/fill', { covenant_id: cov, amount: 200_000_000, taker: 'bob' })).body;
    expect(f).toMatchObject({ covenant_id: cov, status: 'partial', filled_amount: '200000000', amount_left: '300000000' });
    const v = (await get(`/v1/orders/${cov}`)).body;
    expect(v).toMatchObject({ status: 'partial', amount_left: '300000000', filled_amount: '200000000', amount_estimated: false, state_known: true });
    expect(v.state.state.amountLeft).toBe('300000000');
    expect(v.custody).toMatchObject({ ok: true, expected_amount: '300000000' });
    expect(v.current.txid).toBe(f.transactionId);
    expect(srv.chain.kasBalance(ALICE.pub) - kasBefore).toBe(5_010_000n); // maker payout ceil(n * (price - tip) / scale), no tip
    expect(srv.chain.tokenBalance(TEST_PUBKEYS.bob, EXKCC)).toBe(200_000_000n);
    // still in the book with the reduced size
    const lvl = (await get(`/v1/books/${EXKCC}?depth=20`)).body.asks.find((l: any) => l.price === '2505000');
    expect(lvl).toMatchObject({ amount: '300000000', orders: 1 });
    // cancel the continuation: the script engine checks the new redeem script and the shrunk custody
    expect((await buildSubmit(await cancelRequest(cov))).res.status).toBe(200);
    expect((await get(`/v1/orders/${cov}`)).body.status).toBe('cancelled');
    expect(srv.chain.tokenBalance(ALICE.pub, EXKCC)).toBe(300_000_000n);
    // the rest of the fill history
    expect((await get(`/v1/orders/${cov}/events`)).body.items.map((e: any) => e.kind)).toEqual(['create', 'fill', 'cancel']);
  });

  it('a fill below the minimum fill is refused unless it takes everything left', async () => {
    await post('/mock/seed', { orders: [{ token: EXKCC, side: 'ask', maker: 'alice', price: 2_505_000, amount: 250_000_000, min_fill: 100_000_000 }] });
    const cov = (await get(`/v1/orders?maker=${ALICE.pub}`)).body.items[0].covenant_id;
    expect((await get(`/v1/orders/${cov}`)).body.min_fill).toBe('100000000');
    const small = await post('/mock/fill', { covenant_id: cov, amount: 99_999_999 });
    expect(small.status).toBe(400);
    expect(small.body.error.message).toMatch(/minimum fill/);
    expect((await post('/mock/fill', { covenant_id: cov, amount: 200_000_000 })).body).toMatchObject({ amount_left: '50000000' });
    // the rest is below the minimum but takes everything left
    expect((await post('/mock/fill', { covenant_id: cov, amount: 50_000_000 })).body).toMatchObject({ status: 'filled', amount_left: '0' });
  });

  it('a full fill closes the order and refuses further fills', async () => {
    await post('/mock/seed', { orders: [{ token: EXKCC, side: 'ask', maker: 'alice', price: 2_505_000, amount: 200_000_000 }] });
    const cov = (await get(`/v1/orders?maker=${ALICE.pub}`)).body.items[0].covenant_id;
    const bad = await post('/mock/fill', { covenant_id: cov, amount: 300_000_000 });
    expect(bad.status).toBe(400);
    expect(bad.body.error.message).toMatch(/only 200000000 base units left/);
    expect((await post('/mock/fill', { covenant_id: cov, amount: 200_000_000 })).body).toMatchObject({ status: 'filled', amount_left: '0' });
    const v = (await get(`/v1/orders/${cov}`)).body;
    expect(v).toMatchObject({ status: 'filled', filled_amount: '200000000', current: null });
    expect((await post('/mock/fill', { covenant_id: cov, amount: 1 })).status).toBe(400);
    expect((await get(`/v1/orders/${cov}/events`)).body.items.at(-1)).toMatchObject({ kind: 'fill', closes: true, amount: '200000000' });
  });

  it('revert-fill takes the last partial fill back out of the indexer view (a re-org), and refuses a closing fill', async () => {
    await post('/mock/seed', { orders: [{ token: EXKCC, side: 'ask', maker: 'alice', price: 2_505_000, amount: 500_000_000 }] });
    const cov = (await get(`/v1/orders?maker=${ALICE.pub}`)).body.items[0].covenant_id;
    expect((await post('/mock/revert-fill', { covenant_id: cov })).status).toBe(400); // no fill yet
    await post('/mock/fill', { covenant_id: cov, amount: 200_000_000, taker: 'bob' });
    expect((await get(`/v1/orders/${cov}`)).body).toMatchObject({ status: 'partial', filled_amount: '200000000', amount_left: '300000000' });
    const r = await post('/mock/revert-fill', { covenant_id: cov, blocks: 2 });
    expect(r.body).toMatchObject({ covenant_id: cov, status: 'open', filled_amount: '0', reverted_amount: '200000000' });
    expect((await get(`/v1/orders/${cov}`)).body).toMatchObject({ status: 'open', filled_amount: '0', amount_left: '500000000' });
    expect((await get(`/v1/orders/${cov}/events`)).body.items.some((e: any) => e.kind === 'fill')).toBe(false);
    await post('/mock/fill', { covenant_id: cov, amount: 500_000_000 }); // closes the order
    expect((await post('/mock/revert-fill', { covenant_id: cov })).status).toBe(400);
  });

  it('creates every golden order kind for real (cond ask, if-done bid), arms a stop and books the exit child of a filled entry', async () => {
    // the golden orders quote GLD at scale 1000 (3 decimals)
    await post('/mock/seed', { tokens: [{ ticker: 'GLD', covenant_id: GLD, program: 'KCC20Ref', extension_commitment: 'ee'.repeat(32), decimals: 3 }] });

    // stop-limit conditional ask: create, arm (auction path appears), cancel
    const condReq = await adaptGolden('create.condAsk');
    const cond = await buildSubmit(condReq);
    expect(cond.res.status).toBe(200);
    const condCov = cond.built.covenants[0].covenantId;
    let c = (await get(`/v1/orders/${condCov}`)).body;
    expect(c).toMatchObject({ contract: 'KobCondAsk', side: 1, in_book: false, price: null, tif: null, status: 'open', amount_left: '10000', min_fill: '1000', scale: 1000, auction: null, listed: true });
    expect(c.custody.ok).toBe(true);
    const listed = (await get(`/v1/books/${GLD}`)).body;
    expect(listed.asks).toHaveLength(0); // conditional orders are not resting liquidity
    await post('/mock/arm', { covenant_id: condCov });
    c = (await get(`/v1/orders/${condCov}`)).body;
    expect(c.auction).toMatchObject({ kind: 'stop', start_price: '200000000', complete: false });
    expect(BigInt(c.state.state.armed)).toBeGreaterThan(1n);
    await post('/mock/advance-daa', { daa: 1000 });
    expect((await get(`/v1/orders/${condCov}`)).body.auction).toMatchObject({ complete: true, current_price: c.auction.end_price });
    expect((await post('/mock/arm', { covenant_id: condCov })).status).toBe(400); // already armed
    expect((await buildSubmit(await cancelRequest(condCov))).res.status).toBe(200);

    // if-done bid: a fill of 3000 base units creates the committed exit order (a sell) as a child with its own custody
    const ifd = await buildSubmit(await adaptGolden('create.ifdBid'));
    expect(ifd.res.status).toBe(200);
    const ifdCov = ifd.built.covenants[0].covenantId;
    expect((await get(`/v1/orders/${ifdCov}`)).body).toMatchObject({ contract: 'KobIfdBid', side: 2, in_book: true, amount_left: '10000', min_fill: '3000', budget_rate: '260100000' });
    expect((await post('/mock/fill', { covenant_id: ifdCov, amount: 2999 })).status).toBe(400); // below its minimum fill
    const fill = (await post('/mock/fill', { covenant_id: ifdCov, amount: 3000 })).body;
    expect(fill.children).toHaveLength(1);
    const entry = (await get(`/v1/orders/${ifdCov}`)).body;
    expect(entry).toMatchObject({ status: 'partial', amount_left: '7000', filled_amount: '3000', children: fill.children });
    const child = (await get(`/v1/orders/${fill.children[0]}`)).body;
    expect(child).toMatchObject({ contract: 'KobCondAsk', parent: ifdCov, side: 1, status: 'open', amount_left: '3000', in_book: false });
    expect(child.custody).toMatchObject({ ok: true, expected_amount: '3000' });
    // both the child exit and the entry remainder can be cancelled by the maker, validated by the script engine
    expect((await buildSubmit(await cancelRequest(fill.children[0]))).res.status).toBe(200);
    expect((await buildSubmit(await cancelRequest(ifdCov))).res.status).toBe(200);
    expect((await get(`/v1/orders?maker=${ALICE.pub}&status=active`)).body.items).toHaveLength(0);
  });

  it('cancel-and-replace closes the old order and registers the new one in one transaction', async () => {
    await post('/mock/seed', { tokens: [{ ticker: 'GLD', covenant_id: GLD, program: 'KCC20Ref', extension_commitment: 'ee'.repeat(32), decimals: 3 }] });
    const first = await buildSubmit(await adaptGolden('create.ask'));
    expect(first.res.status).toBe(200);
    const cov = first.built.covenants[0].covenantId;
    const v = (await get(`/v1/orders/${cov}`)).body;
    const replacement = { ...v.state.state, price: '260000000', amountLeft: '10000' };
    const req = (await cancelRequest(cov, await keyUtxos(ALICE.pub))) as any;
    req.replace = { order: { kind: 'KobAsk', state: replacement }, value: String(CARRIER), tokenCarrier: String(CARRIER) };
    const r = await buildSubmit(req);
    expect(r.res.status).toBe(200);
    expect((await get(`/v1/orders/${cov}`)).body.status).toBe('cancelled');
    const open = (await get(`/v1/orders?maker=${ALICE.pub}&status=active`)).body.items;
    expect(open).toHaveLength(1);
    expect(open[0]).toMatchObject({ contract: 'KobAsk', price: '260000000', amount_left: '10000' });
    expect(open[0].covenant_id).not.toBe(cov);
    expect((await get(`/v1/orders/${open[0].covenant_id}`)).body.custody.ok).toBe(true);
    const sub = (await get('/mock/submitted?full=0')).body.items.at(-1);
    expect(sub.closed).toEqual([{ covenantId: cov, entry: 'cancel', status: 'cancelled' }]);
    expect(sub.created).toHaveLength(1);
    expect(sub.tx).toBeUndefined();
  });

  it('orders whose token identity differs from the registry are indexed but unlisted', async () => {
    await post('/mock/seed', { tokens: [{ ticker: 'GLD', covenant_id: GLD, program: 'KCC20Ref', template_hash: '11'.repeat(32), extension_commitment: 'ee'.repeat(32), decimals: 3 }] });
    const r = await buildSubmit(await adaptGolden('create.ask'));
    expect(r.res.status).toBe(200);
    const o = (await get(`/v1/orders/${r.built.covenants[0].covenantId}`)).body;
    expect(o).toMatchObject({ listed: false, unlisted_reason: 'template_mismatch', status: 'open' });
    expect((await get(`/v1/books/${GLD}`)).body.asks).toHaveLength(0);
    expect((await get('/v1/tokens')).body.tokens.find((t: any) => t.ticker === 'GLD').open_asks).toBe(0);
  });

  it('orders at another scale than the token standard scale are indexed but unlisted (non_standard_scale)', async () => {
    await post('/mock/seed', { tokens: [{ ticker: 'GLD', covenant_id: GLD, program: 'KCC20Ref', extension_commitment: 'ee'.repeat(32), decimals: 8 }] });
    const r = await buildSubmit(await adaptGolden('create.ask'));
    expect(r.res.status).toBe(200);
    const o = (await get(`/v1/orders/${r.built.covenants[0].covenantId}`)).body;
    expect(o).toMatchObject({ listed: false, unlisted_reason: 'non_standard_scale', scale: 1000 });
    expect((await get(`/v1/books/${GLD}`)).body.asks).toHaveLength(0);
  });

  it('reports stray tokens sent to an order and lets the maker sweep them with the cancel', async () => {
    await post('/mock/seed', { orders: [{ token: EXKCC, side: 'ask', maker: 'alice', price: 2_505_000, amount: 200_000_000 }] });
    const cov = (await get(`/v1/orders?maker=${ALICE.pub}`)).body.items[0].covenant_id;
    await post('/mock/stray', { covenant_id: cov, amount: '123000000' });
    const strays = (await get('/v1/strays')).body.items;
    expect(strays).toHaveLength(1);
    expect(strays[0]).toMatchObject({ owner: cov, role: 'stray', amount: '123000000', order_status: 'open', maker: ALICE.pub, lost: false });
    expect((await get(`/v1/strays?maker=${TEST_PUBKEYS.bob}`)).body.items).toHaveLength(0);
    const v = (await get(`/v1/orders/${cov}`)).body;
    expect(v.strays).toHaveLength(1);
    expect(v.custody.ok).toBe(true); // a stray is never liquidity: the exact custody is unchanged
    const cancel = await buildSubmit(await cancelRequest(cov));
    expect(cancel.res.status).toBe(200);
    expect((await get('/v1/strays')).body.items).toEqual([]);
    expect(srv.chain.tokenBalance(ALICE.pub, EXKCC)).toBe(323_000_000n); // custody 2 whole tokens + the stray
  });

  it('flags foreign strays and registers a maker sweep in place: the order lives on at the new outpoint, its strays go to the maker', async () => {
    await post('/mock/seed', { orders: [{ token: EXKCC, side: 'ask', maker: 'alice', price: 2_505_000, amount: 200_000_000 }] });
    const cov = (await get(`/v1/orders?maker=${ALICE.pub}`)).body.items[0].covenant_id;
    const FOREIGN = '81'.repeat(32);
    await post('/mock/stray', { covenant_id: cov, amount: '5000' });
    await post('/mock/stray', { covenant_id: cov, amount: '777', token: FOREIGN, program: 'KCC20Ref_8x8', ticker: 'FRN' });
    const v = (await get(`/v1/orders/${cov}`)).body;
    expect(v.strays.map((s: any) => [s.token, s.foreign ?? false, s.program])).toEqual([[EXKCC, false, 'KCC20Ref_8x8'], [FOREIGN, true, 'KCC20Ref_8x8']]);
    expect((await get('/v1/strays')).body.items.find((s: any) => s.token === FOREIGN)).toMatchObject({ foreign: true, program: 'KCC20Ref_8x8' });
    // the foreign token is tracked but not listed
    expect((await get('/v1/tokens')).body.tokens.some((t: any) => t.covenant_id === FOREIGN)).toBe(false);
    const own = v.strays.filter((s: any) => !s.foreign).map(tokenUtxoOf);
    const foreign = v.strays.filter((s: any) => s.foreign).map(tokenUtxoOf);
    const before = v.current;
    const r = await buildSubmit({
      action: 'sweepOrder',
      order: { transactionId: v.current.txid, index: v.current.index, amount: v.current.value, blockDaaScore: String(v.current_daa), covenantId: cov, state: v.state },
      strays: own, foreign: [{ token: { covenantId: FOREIGN, program: 'KCC20Ref_8x8' }, utxos: foreign }], funding: [], change: null, lockTime: '0', records: [], fee: { feeRate: null },
    });
    expect(r.res.status).toBe(200);
    const after = (await get(`/v1/orders/${cov}`)).body;
    expect(after).toMatchObject({ status: 'open', state_known: true, amount_left: '200000000', current: { txid: r.tx.id, index: 0 } });
    expect(after.current.txid).not.toBe(before.txid);
    expect(after.custody.ok).toBe(true); // the custody did not move
    expect(after.strays).toEqual([]);
    expect((await get(`/v1/orders/${cov}/events`)).body.items.map((e: any) => e.kind)).toContain('sweep');
    expect(srv.chain.tokenBalance(ALICE.pub, EXKCC)).toBe(5000n);
    expect(srv.chain.tokenBalance(ALICE.pub, FOREIGN)).toBe(777n);
    // the swept order is still the maker's to cancel
    expect((await buildSubmit(await cancelRequest(cov))).res.status).toBe(200);
    expect(srv.chain.tokenBalance(ALICE.pub, EXKCC)).toBe(200_005_000n);
  });
});

// ------------------------------------------------------------------------------------------------------------------------------

describe('WebSocket /v1/ws', () => {
  const connect = async () => {
    const ws = new WebSocket(srv.url.replace('http', 'ws') + '/v1/ws');
    const frames: any[] = [];
    ws.addEventListener('message', (e) => frames.push(JSON.parse(String(e.data))));
    await new Promise((res, rej) => {
      ws.addEventListener('open', res, { once: true });
      ws.addEventListener('error', rej, { once: true });
    });
    const waitFor = async (pred: (f: any[]) => boolean, ms = 3000) => {
      const t0 = Date.now();
      while (!pred(frames)) {
        if (Date.now() - t0 > ms) throw new Error(`timeout; frames: ${JSON.stringify(frames)}`);
        await new Promise((r) => setTimeout(r, 10));
      }
    };
    const call = async (msg: unknown) => {
      const n = frames.length;
      ws.send(JSON.stringify(msg));
      // the initial health frame of a `health` subscription can arrive between the request and its reply: skip it
      await waitFor((f) => f.slice(n).some((x) => x.type !== 'health'));
      return frames.slice(n).find((x) => x.type !== 'health');
    };
    return { ws, frames, waitFor, call };
  };

  it('answers ping, subscribe (with per-channel errors), unsubscribe and bad frames like the indexer', async () => {
    const { ws, call } = await connect();
    expect(await call({ op: 'ping' })).toEqual({ type: 'pong' });
    const id = 'ab'.repeat(32);
    const sub = await call({ op: 'subscribe', channels: ['health', 'fills', `book:${id}`, `order:${id.toUpperCase()}`, 'bogus', 'book:zz', 'other:00'] });
    expect(sub.type).toBe('subscribed');
    expect(sub.data.channels).toEqual(['book:' + id, 'fills', 'health', 'order:' + id].sort());
    expect(sub.data.errors).toEqual(['unknown channel `bogus`', 'invalid channel `book:zz`: id must be 64 hex characters', 'unknown channel `other:00`']);
    const un = await call({ op: 'unsubscribe', channels: ['health', 'nonsense'] });
    expect(un).toEqual({ type: 'unsubscribed', data: { channels: ['book:' + id, 'fills', 'order:' + id].sort() } });
    expect((await call({ op: 'dance' })).data.message).toBe('unknown op');
    ws.send('not json');
    ws.close();
  });

  it('pushes fill / book / order / cursor frames for a simulated fill and only to subscribers', async () => {
    const a = await connect();
    const b = await connect();
    const ask = (await get(`/v1/books/${EXKCC}?aggregate=false&depth=1`)).body.asks[0];
    const cov = ask.covenant_id;
    await a.call({ op: 'subscribe', channels: ['health', 'reorg', `fills:${EXKCC}`, `fills:${'00'.repeat(32)}`, `book:${EXKCC}`, `order:${cov}`, `order:${'11'.repeat(32)}`] });
    await b.call({ op: 'subscribe', channels: [`book:${'22'.repeat(32)}`] });
    await post('/mock/fill', { covenant_id: cov, amount: 100_000_000 });
    await a.waitFor((f) => f.length >= 5);
    const byChannel = Object.fromEntries(a.frames.slice(1).map((f) => [f.channel, f]));
    expect(byChannel[`fills:${EXKCC}`]).toMatchObject({ type: 'fill', data: { order: cov, token: EXKCC, side: 1, amount: 100_000_000, price: Number(ask.price) } });
    expect(byChannel[`book:${EXKCC}`]).toMatchObject({ type: 'book', data: { token: EXKCC } });
    expect(byChannel[`order:${cov}`]).toMatchObject({ type: 'order', data: { covenant_id: cov } });
    expect(byChannel.health).toMatchObject({ type: 'cursor', data: { added_blocks: 1, reverted_blocks: 0 } });
    expect(byChannel[`fills:${'00'.repeat(32)}`]).toBeUndefined();
    expect(byChannel.reorg).toBeUndefined();
    expect(b.frames).toHaveLength(1); // only its own subscribed reply
    await post('/mock/reorg', { blocks: 2 });
    await a.waitFor((f) => f.some((x) => x.channel === 'reorg'));
    expect(a.frames.find((f) => f.channel === 'reorg')).toMatchObject({ type: 'reorg', data: { reverted_blocks: 2, added_blocks: 2 } });
    a.ws.close();
    b.ws.close();
  });

  it('streams order frames for a submitted create and cancel, plus periodic health frames, resync and forced disconnects', async () => {
    const a = await connect();
    await a.call({ op: 'subscribe', channels: ['health'] });
    await a.waitFor((f) => f.some((x) => x.type === 'health'), 2000); // healthEveryMs = 200 in this suite
    await post('/mock/resync', { missed: 3 });
    await a.waitFor((f) => f.some((x) => x.type === 'resync'));
    expect(a.frames.find((x) => x.type === 'resync')).toEqual({ type: 'resync', data: { missed_events: 3 } });
    const closed = new Promise((r) => a.ws.addEventListener('close', r, { once: true }));
    expect((await post('/mock/ws-close')).body.closed).toBeGreaterThanOrEqual(1);
    await closed;
    expect(srv.wsClients()).toBe(0);

    const b = await connect();
    await post('/mock/seed', { orders: [{ token: EXKCC, side: 'ask', maker: 'alice', price: 2_505_000, amount: 200_000_000 }] });
    const cov = (await get(`/v1/orders?maker=${ALICE.pub}`)).body.items[0].covenant_id;
    await b.call({ op: 'subscribe', channels: [`order:${cov}`, `book:${EXKCC}`] });
    await buildSubmit(await cancelRequest(cov));
    await b.waitFor((f) => f.some((x) => x.channel === `order:${cov}`) && f.some((x) => x.channel === `book:${EXKCC}`));
    b.ws.close();
  });

  it('closes a connection that sends an oversized frame', async () => {
    const { ws } = await connect();
    const closed = new Promise<number>((r) => ws.addEventListener('close', (e) => r(e.code), { once: true }));
    ws.send('x'.repeat(5000));
    expect(await closed).toBe(1009);
  });
});
