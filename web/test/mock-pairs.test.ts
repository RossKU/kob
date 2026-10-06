// Token/token pair orders in the mock indexer + node (mock/pairs.mjs, mock/pair-fill.mjs): the pair seed (KobPair, KobCondPair, KobIfdPair),
// the pair endpoints of docs/ops/executor.md 5.4 (book with direct / entry / route levels, pairs summary, candles DERIVED from the two KAS series,
// fills as volume with counterparty and price_source none), order views with the `pair` object, and simulated fills / arms / trails that are
// REAL transactions (kob-wasm batches signed by the mock's filler and validated by the script engine): inventory, netting and route fills, IOC
// return, stops armed in both evidence modes and triggered, a trailing ratchet, if-done entries creating their exits (both sides), a repeat
// merge; placements and cancels / refunds built by the app's own kob layer and submitted to the mock node.
import { afterAll, beforeAll, describe, expect, it } from 'vitest';
import { seedPairOrder, pairOrderState } from '../mock/pairs.mjs';
import { startMockServer, type MockServer } from '../mock/server.mjs';
import { TEST_PUBKEYS, TEST_SECRETS } from '../mock/keys.mjs';
import { seedTrade } from '../mock/seed.mjs';
import { planCancel, planRefund, snapshotFromOrderView } from '../src/kob/cancel';
import { custodiesOf } from '../src/kob/order-facts';
import { loadKobNode } from '../src/kob/wasm.node';
import { signBuilt } from '../src/testing/local-signer';
import type { OrderView } from '../src/data/indexer-types';
import type { BuiltTx, CreateOrderRequest, KeyUtxo, OrderState, TokenUtxo } from '../src/kob/types';

const kob = loadKobNode();
const ALICE = { pk: TEST_PUBKEYS.alice, sk: TEST_SECRETS.alice };
const SA = 100_000_000n; // one whole EXKCC (A)
const ratio = (l: { price_num: string; price_den: string }) => Number(l.price_num) / Number(l.price_den);

describe('mock pair orders', () => {
  let srv: MockServer;
  let base = '';
  let quote = '';
  const frames: { tokens: string[]; orders: string[] }[] = [];
  const get = async (p: string) => {
    const r = await fetch(srv.url + p);
    return { status: r.status, body: await r.json() };
  };
  const post = async (p: string, body: unknown) => {
    const r = await fetch(srv.url + p, { method: 'POST', headers: { 'content-type': 'application/json' }, body: JSON.stringify(body) });
    return { status: r.status, body: await r.json() };
  };
  const view = async (id: string) => (await get(`/v1/orders/${id}`)).body;
  const seed = (spec: Record<string, unknown>) => seedPairOrder(srv.chain, { base, quote, maker: 'maker', minFill: SA / 10n, ...spec } as never).covenantId;
  const fill = async (id: string, body: Record<string, unknown> = {}) => {
    const r = await post('/mock/fill', { covenant_id: id, ...body });
    expect(r.status, JSON.stringify(r.body)).toBe(200);
    return r.body;
  };
  const submit = async (built: BuiltTx, sk: string) => {
    const signed = kob.finalize(built, signBuilt(built, [sk]), { tightenBudgets: true });
    const r = await post('/node/submit', { transaction: signed.tx });
    expect(r.status, JSON.stringify(r.body)).toBe(200);
    return signed.tx.id;
  };

  beforeAll(async () => {
    srv = await startMockServer({ port: 0, pair: true, healthEveryMs: 600_000 });
    const tokens = (await get('/v1/tokens')).body.tokens;
    base = tokens[0].covenant_id;
    quote = tokens[1].covenant_id;
    srv.chain.listeners.add((ev: { tokens: string[]; orders: string[] }) => frames.push({ tokens: ev.tokens, orders: ev.orders }));
  });
  afterAll(async () => srv?.close());

  it('seeds EXUSD next to EXKCC and pair orders of every kind (never in the KAS books), each with its pair view', async () => {
    const tokens = (await get('/v1/tokens')).body.tokens;
    expect(tokens.map((t: { ticker: string }) => t.ticker)).toEqual(['EXKCC', 'EXUSD']);
    const pairs = (await get('/v1/orders?status=active&limit=200')).body.items.filter((o: OrderView & { pair?: unknown }) => o.pair);
    expect(pairs.map((o: { contract: string }) => o.contract).sort()).toEqual(['KobCondPair', 'KobIfdPair', 'KobPair', 'KobPair', 'KobPair', 'KobPair']);
    for (const o of pairs) {
      expect(o).toMatchObject({ in_book: false, listed: true, price: null, quote: null, token: base });
      expect(o.pair).toMatchObject({ base, quote, base_scale: 100_000_000, quote_scale: 1_000_000, base_family: 'kcc20', quote_family: 'kcc20' });
    }
    const book = (await get(`/v1/books/${base}?aggregate=false&depth=200`)).body;
    expect([...book.asks, ...book.bids].some((r: { contract: string }) => /Pair/.test(r.contract))).toBe(false);
    // a pair order is listed under both its tokens
    expect((await get(`/v1/orders?token=${quote}&status=active&limit=200`)).body.items.filter((o: { pair?: unknown }) => o.pair)).toHaveLength(6);
    // the detail view: every custody of the state (role, expected amount, the live UTXO, ok) and the first one as `custody`
    const entry = pairs.find((o: { contract: string }) => o.contract === 'KobIfdPair');
    const d = await view(entry.covenant_id);
    expect(d.pair).toMatchObject({ side: 'bid', price: '49000', price_num: '49', price_den: '100000', amount_left: '200000000' });
    expect(d.pair.custodies).toHaveLength(1);
    expect(d.pair.custodies[0]).toMatchObject({ token: quote, role: 'quote', ok: true });
    expect(d.custody).toMatchObject({ ok: true, expected_amount: d.pair.custodies[0].expected_amount });
  });

  it('serves the pair book: direct, entry and route levels, exact prices, asks ascending and bids descending, pair orders first at a price', async () => {
    const r = await get(`/v1/pairs/${base}/${quote}/book?depth=40`);
    expect(r.status).toBe(200);
    const b = r.body;
    expect(b).toMatchObject({ base, quote });
    expect(typeof b.daa_score).toBe('number');
    const direct = b.asks.filter((l: { source: string }) => l.source === 'direct');
    expect(direct.map((l: { price_num: string; price_den: string; amount: string; orders: number }) => [l.price_num, l.price_den, l.amount, l.orders])).toEqual([
      ['101', '200000', '300000000', 2], ['8', '15625', '100000000', 1],
    ]);
    expect(b.bids.filter((l: { source: string }) => l.source === 'direct').map((l: { amount: string }) => l.amount)).toEqual(['300000000']);
    expect(b.bids.filter((l: { source: string }) => l.source === 'entry').map((l: { price_num: string; price_den: string; amount: string }) => [l.price_num, l.price_den, l.amount])).toEqual([['49', '100000', '200000000']]);
    expect(b.asks.some((l: { source: string }) => l.source === 'route')).toBe(true);
    for (let i = 1; i < b.asks.length; i++) expect(ratio(b.asks[i])).toBeGreaterThanOrEqual(ratio(b.asks[i - 1]));
    for (let i = 1; i < b.bids.length; i++) expect(ratio(b.bids[i])).toBeLessThanOrEqual(ratio(b.bids[i - 1]));
    // ORIENTED: the reversed pair shows no pair order (an order of A/B is never inverted into B/A), only the route
    const rev = (await get(`/v1/pairs/${quote}/${base}/book`)).body;
    expect([...rev.asks, ...rev.bids].every((l: { source: string }) => l.source === 'route')).toBe(true);
    expect((await get(`/v1/pairs/${base}/${base}/book`)).status).toBe(400);
    expect((await get(`/v1/pairs/xyz/${quote}/book`)).status).toBe(400);
    expect((await get(`/v1/pairs/${'00'.repeat(32)}/${quote}/book`)).body).toMatchObject({ asks: [], bids: [] });
  });

  it('lists the oriented pairs with their counts per kind', async () => {
    expect((await get(`/v1/pairs?token=${quote}`)).body).toEqual([{ base, quote, direct_asks: 3, direct_bids: 1, entry_asks: 0, entry_bids: 1, conditionals: 1 }]);
    expect((await get(`/v1/pairs?token=${'ab'.repeat(32)}`)).body).toEqual([]);
  });

  it('fills a KobPair ask from inventory, by netting and through the KAS route: real transactions, volume only, the counterparty recorded', async () => {
    const id = seed({ kind: 'KobPair', side: 'ask', amount: 3n * SA, price: 50_500n });
    frames.length = 0;
    const tradesBefore = (await get(`/v1/trades/${base}`)).body.items.length;
    for (const [via, left] of [['inventory', '270000000'], ['netting', '240000000'], ['route', '210000000']] as const) {
      const r = await fill(id, { amount: String(SA / 10n * 3n), via });
      expect(r).toMatchObject({ status: 'partial', amount_left: left, state_known: true, counterparty: via, kind: 'fill' });
    }
    // book notices for both tokens
    expect(frames.some((f) => f.tokens.includes(base) && f.tokens.includes(quote) && f.orders.includes(id))).toBe(true);
    const v = await view(id);
    expect(v).toMatchObject({ status: 'partial', filled_amount: '90000000', amount_left: '210000000', state_known: true });
    expect(v.pair.custodies[0]).toMatchObject({ expected_amount: '210000000', ok: true });
    // fill events: no price, the pair fields
    const ev = (await get(`/v1/orders/${id}/events`)).body.items.filter((e: { kind: string }) => e.kind === 'fill');
    expect(ev).toHaveLength(3);
    for (const e of ev) {
      expect(e.price).toBeNull();
      expect(e.detail.pair).toMatchObject({ side: 'ask', base, quote, amount_a: '30000000', amount_b: '15150', price: '50500', price_source: 'none' });
    }
    // the pair fills: volume with counterparty; the route's KAS legs made KAS trades, the pair fills none
    const fills = (await get(`/v1/pairs/${base}/${quote}/fills`)).body;
    const mine = fills.items.filter((f: { order: string }) => f.order === id);
    expect(mine.map((f: { counterparty: string }) => f.counterparty)).toEqual(['route', 'netting', 'inventory']);
    expect(mine[0]).toMatchObject({ contract: 'KobPair', side: 'ask', amount_a: '30000000', amount_b: '15150', price_num: '101', price_den: '200000', price_source: 'none' });
    expect(BigInt(fills.volume_24h.amount_a)).toBeGreaterThanOrEqual(90_000_000n);
    const trades = (await get(`/v1/trades/${base}`)).body.items;
    expect(trades.length).toBe(tradesBefore + 1);
    expect((await get(`/v1/pairs/${base}/${quote}/fills?limit=1`)).body.next_cursor).not.toBeNull();
  });

  it('fills a KobPair bid completely (closed), and an IOC ask partly (its rest returned)', async () => {
    const bid = seed({ kind: 'KobPair', side: 'bid', amount: SA, price: 49_000n });
    expect(await fill(bid, { amount: String(SA) })).toMatchObject({ status: 'filled', amount_left: '0' });
    expect((await view(bid)).pair.custodies).toEqual([]);
    const ioc = seed({ kind: 'KobPair', side: 'ask', amount: SA, price: 50_000n, tif: 1 });
    expect(await fill(ioc, { amount: String(SA / 2n) })).toMatchObject({ status: 'filled', filled_amount: '50000000', amount_left: '0' });
    expect(srv.chain.liveTokenUtxos(ioc, 'custody')).toEqual([]);
  });

  it('arms pair stops in both evidence modes (update and in-fill) and fills the armed stop leg; the evidence is on the events', async () => {
    for (const mode of [0, 1]) {
      const stop = seed({ kind: 'KobCondPair', side: 'ask', amount: SA, stop: 45_000n, minTouch: SA / 10n });
      const a = await post('/mock/arm', { covenant_id: stop, mode });
      expect(a.status, JSON.stringify(a.body)).toBe(200);
      expect(a.body).toMatchObject({ kind: 'arm', evidence: { mode } });
      expect(a.body.armed).not.toBe('0');
      const arm = (await get(`/v1/orders/${stop}/events`)).body.items.find((e: { kind: string }) => e.kind === 'arm');
      expect(arm.detail.evidence).toMatchObject({ mode });
      expect(await fill(stop, { amount: String(SA / 2n), leg: 1 })).toMatchObject({ status: 'partial', amount_left: '50000000' });
      // armed in its own fill (an unarmed stop leg filled next to the evidence)
      const inFill = seed({ kind: 'KobCondPair', side: 'ask', amount: SA, stop: 45_000n, minTouch: SA / 10n });
      const f = await fill(inFill, { amount: String(SA / 2n), leg: 1, mode });
      expect(f).toMatchObject({ status: 'partial', evidence: { mode } });
      expect((await view(inFill)).state.state.armed).not.toBe('0');
    }
    // a buy stop armed by two KAS-book fills (an ask of B and a bid of A at quotes implying a rate at or above the stop)
    const buy = seed({ kind: 'KobCondPair', side: 'bid', amount: SA, stop: 55_000n, minTouch: SA / 10n });
    expect((await post('/mock/arm', { covenant_id: buy, mode: 0 })).body).toMatchObject({ kind: 'arm', evidence: { mode: 0 } });
  });

  it('a trailing pair stop ratchets one step on evidence beyond it (both modes)', async () => {
    for (const mode of [0, 1]) {
      const id = seed({ kind: 'KobCondPair', side: 'ask', amount: SA, stop: 45_000n, minTouch: SA / 10n, trailStep: 500n, trailGap: 1000n });
      const r = await post('/mock/trail', { covenant_id: id, mode });
      expect(r.status, JSON.stringify(r.body)).toBe(200);
      expect(r.body).toMatchObject({ kind: 'trail', stop_price: '45500' });
      expect((await view(id)).pair.stop_price).toBe('45500');
    }
  });

  it('if-done pair entries create their exits (buy-first: an ask exit holding A; sell-first: a bid exit holding the proceeds and prefund)', async () => {
    const buyFirst = seed({ kind: 'KobIfdPair', side: 'bid', amount: SA, price: 49_000n, exit: { tp: 54_000n } });
    const r = await fill(buyFirst, { amount: String(SA / 2n) });
    expect(r.children).toHaveLength(1);
    const exit = await view(r.children[0]);
    expect(exit).toMatchObject({ contract: 'KobCondPair', parent: buyFirst, status: 'open' });
    expect(exit.pair).toMatchObject({ side: 'ask', price: '54000', amount_left: '50000000' });
    expect(exit.pair.custodies).toEqual([expect.objectContaining({ token: base, role: 'base', expected_amount: '50000000', ok: true })]);
    expect((await view(buyFirst)).children).toEqual([r.children[0]]);
    expect(await fill(r.children[0], { amount: String(SA / 4n) })).toMatchObject({ status: 'partial', amount_left: '25000000' });

    const sellFirst = seed({ kind: 'KobIfdPair', side: 'ask', amount: SA, price: 51_000n, prefund: 1000n, exit: { tp: 47_000n } });
    const sf = await view(sellFirst);
    expect(sf.pair.custodies.map((c: { role: string }) => c.role)).toEqual(['base', 'quote']);
    expect(sf.pair.prefund).toBe('1000');
    const s = await fill(sellFirst, { amount: String(SA / 2n) });
    const bx = await view(s.children[0]);
    // proceeds ceil(0.5 x 51000) = 25500 plus the prefund of the fill ceil(0.5 x 1000) = 500, in B
    expect(bx.pair).toMatchObject({ side: 'bid', price: '47000' });
    expect(bx.pair.custodies).toEqual([expect.objectContaining({ token: quote, role: 'quote', expected_amount: '26000', ok: true })]);
    expect(await fill(s.children[0], { amount: String(SA / 4n) })).toMatchObject({ status: 'partial' });
  });

  it('a stop entry arms in its fill and a booked exit\'s take-profit merges its repeating entry (a re-arm)', async () => {
    const se = seed({ kind: 'KobIfdPair', side: 'bid', amount: SA, price: 50_000n, entryStop: 49_000n, minTouch: SA / 10n, exit: { tp: 54_000n } });
    const f = await fill(se, { amount: String(SA / 2n), mode: 0 });
    expect(f).toMatchObject({ status: 'partial', evidence: { mode: 0 } });
    expect(f.children).toHaveLength(1);
    const rp = seed({ kind: 'KobIfdPair', side: 'bid', amount: SA, price: 49_000n, rptAmount: 2n * SA + 1n, exit: { tp: 54_000n } });
    const r = await fill(rp, { amount: String(SA / 2n) });
    const exit = await view(r.children[0]);
    expect(exit.repeat).toMatchObject({ role: 'exit', parent: rp });
    await fill(r.children[0], { amount: String(SA / 4n) });
    const e = await view(rp);
    expect(e.amount_left).toBe('75000000');
    expect((await get(`/v1/orders/${rp}/events`)).body.items.map((x: { kind: string }) => x.kind)).toEqual(['create', 'fill', 'rearm']);
  });

  it('pair candles are DERIVED from the two KAS series (pair fills add volume only)', async () => {
    const now = Date.now();
    seedTrade(srv.chain, { token: base, ts: now - 60_000, legs: [{ side: 'ask', price: '5000000', amount: String(SA) }, { side: 'bid', price: '5000000', amount: String(SA) }] });
    seedTrade(srv.chain, { token: quote, ts: now - 60_000, legs: [{ side: 'ask', price: '100000000', amount: '1000000' }, { side: 'bid', price: '100000000', amount: '1000000' }] });
    const c = (await get(`/v1/pairs/${base}/${quote}/candles?interval=1d`)).body;
    expect(c).toMatchObject({ base, quote, price_source: 'kas_books', price_basis: '100000000', quote_price_basis: '1000000', decimals: 8, quote_decimals: 6 });
    const last = c.items[c.items.length - 1];
    expect(last).toMatchObject({ a_traded: true, b_traded: true });
    // 0.05 KAS per EXKCC over 1 KAS per EXUSD = 0.05 EXUSD per EXKCC = 50000 EXUSD base units per whole EXKCC
    expect(last.c).toEqual({ value: '50000', num: '50000', den: '1' });
    expect(BigInt(last.pair_volume_a)).toBeGreaterThan(0n);
    expect(last.pair_fills).toBeGreaterThan(0);
    expect((await get(`/v1/pairs/${base}/${quote}/candles?interval=7m`)).status).toBe(400);
  });

  it('a pair order placed by the app is recovered (both custodies of a sell-first entry), and the app cancels and refunds pair orders', async () => {
    const chain = srv.chain;
    const tokenUtxo = (g: { transactionId: string }): TokenUtxo => {
      const rec = chain.tokenUtxos.find((t: { txid: string; index: number }) => t.txid === g.transactionId && t.index === 0);
      return { transactionId: rec.txid, index: 0, amount: rec.value.toString(), blockDaaScore: String(rec.created_daa), covenantId: rec.token, state: rec.state };
    };
    const kas = (sompi: bigint): KeyUtxo => {
      const g = chain.giveKas(ALICE.pk, sompi);
      return { transactionId: g.transactionId, index: 0, amount: g.amount, blockDaaScore: String(chain.daa()), covenantId: null, pubkey: ALICE.pk };
    };
    const { any, value } = pairOrderState(chain, { kind: 'KobIfdPair', side: 'ask', base, quote, amount: SA, price: 51_000n, prefund: 1000n, maker: ALICE.pk, minFill: SA / 4n, exit: { tp: 47_000n } });
    const cs = custodiesOf(any as unknown as OrderState, kob);
    const req: CreateOrderRequest = {
      action: 'createOrder', order: any as unknown as OrderState, value: value.toString(),
      tokens: [tokenUtxo(chain.giveTokens(ALICE.pk, base, cs[0]!.amount)), tokenUtxo(chain.giveTokens(ALICE.pk, quote, cs[1]!.amount))],
      tokenCarrier: '1000000000', funding: [kas(100n * 100_000_000n)], change: ALICE.pk,
    };
    const txid = await submit(kob.build(req), ALICE.sk);
    const sub = (await get('/mock/submitted?full=0')).body.items.find((x: { txid: string }) => x.txid === txid);
    expect(sub.created).toHaveLength(1);
    const id = sub.created[0].covenantId;
    const v = await view(id);
    expect(v).toMatchObject({ contract: 'KobIfdPair', origin: 'chain', listed: true, maker: ALICE.pk });
    expect(v.pair.custodies.map((c: { token: string; ok: boolean }) => [c.token, c.ok])).toEqual([[base, true], [quote, true]]);
    // the app's cancel from the indexer view: both custodies return to the maker
    const before = { a: chain.tokenBalance(ALICE.pk, base), b: chain.tokenBalance(ALICE.pk, quote) };
    const plan = planCancel({ kob, maker: ALICE.pk, funding: [kas(10n * 100_000_000n)] }, snapshotFromOrderView(v));
    expect(plan.ok, JSON.stringify(plan.issues)).toBe(true);
    await submit(plan.built!, ALICE.sk);
    expect((await view(id)).status).toBe('cancelled');
    expect(chain.tokenBalance(ALICE.pk, base) - before.a).toBe(cs[0]!.amount);
    expect(chain.tokenBalance(ALICE.pk, quote) - before.b).toBe(cs[1]!.amount);
    // a refund once due (here: an IOC pair order past its kill time)
    const ioc = pairOrderState(chain, { kind: 'KobPair', side: 'bid', base, quote, amount: SA, price: 49_000n, tif: 1, maker: ALICE.pk });
    const c2 = custodiesOf(ioc.any as unknown as OrderState, kob);
    const tx2 = await submit(kob.build({
      action: 'createOrder', order: ioc.any as unknown as OrderState, value: ioc.value.toString(), tokens: [tokenUtxo(chain.giveTokens(ALICE.pk, quote, c2[0]!.amount))],
      tokenCarrier: '1000000000', funding: [kas(100n * 100_000_000n)], change: ALICE.pk,
    } as CreateOrderRequest), ALICE.sk);
    const id2 = (await get('/mock/submitted?full=0')).body.items.find((x: { txid: string }) => x.txid === tx2).created[0].covenantId;
    await post('/mock/advance-daa', { daa: 700 });
    const due = await view(id2);
    const refund = planRefund({ kob, maker: ALICE.pk, clock: { daa: BigInt(chain.daa()) } }, snapshotFromOrderView(due));
    expect(refund.ok, JSON.stringify(refund.issues)).toBe(true);
    const signed = kob.finalize(refund.built!, signBuilt(refund.built!, []), { tightenBudgets: true });
    expect((await post('/node/submit', { transaction: signed.tx })).status).toBe(200);
    expect((await view(id2)).status).toBe('refunded');
  });
});
