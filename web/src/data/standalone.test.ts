// Standalone mode end to end below the UI: the mode switch (settings, indexer health), the node's blocks read into a partial book that lists only what
// the node confirms, fills followed into trades and figures, and the indexer views the rest of the app reads (the book check included).
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { loadKobNode } from '../kob/wasm.node';
import { snapshotFromOrderView, type OrderSnapshot } from '../kob/cancel';
import { confirmSnapshotsOnNode } from '../kob/node-verify';
import type { OrderState, TokenUtxo, TxJson } from '../kob/types';
import { FakeChain, goldenTx, placeGolden, testAddress } from '../testing/chain-fixtures';
import { assessIndexerHealth } from '../ui/shell/health';
import type { StreamBlock } from './block-stream';
import type { IndexerApi } from './indexer';
import type { BookOrderView, HealthView, LevelView } from './indexer-types';
import { ENTER_CHECK_MS, RECHECK_MS, StandaloneController, VERIFY_DELAY_MS, modeIndexer, type StandalonePrefs } from './standalone';
import { StreamBook, bookViewOf, listingOf, MIN_ORDER_VALUE_SOMPI, orderViewOf, tokensViewOf } from './stream-book';

const kob = loadKobNode();
const TOKEN = '70'.repeat(32);
const EMPTY = { address: null, amount: '0', scriptPublicKey: '', blockDaaScore: '0', isCoinbase: false, covenantId: null };
const asAnnounced = (tx: TxJson): TxJson => ({ ...tx, inputs: tx.inputs.map((i) => ({ ...i, utxo: { ...EMPTY } })) });
const clone = <T>(v: T): T => JSON.parse(JSON.stringify(v)) as T;
const health = (state: string): HealthView => ({ state, ok: true, network: 'testnet-10', cursor_daa: 1, node_daa: 1, lag_daa: 0, lag_seconds: 0, settle_depth_daa: 100 });
const bad = assessIndexerHealth({ health: null, error: new Error('down'), configured: true, network: 'testnet-10' });
const good = assessIndexerHealth({ health: health('following'), error: null, configured: true, network: 'testnet-10' });

/** The resting order a golden batch leg spends, as the book holds it. */
function legSnapshot(name: string): OrderSnapshot {
  const l = (goldenTx(name).request as unknown as { legs: { kind: string; order: { transactionId: string; index: number; amount: string; covenantId: string; blockDaaScore: string; state: Record<string, string> }; custody?: TokenUtxo | null }[] }).legs[0]!;
  return {
    covenantId: l.order.covenantId,
    order: { ...l.order, state: { kind: l.kind === 'ask' ? 'KobAsk' : 'KobBid', state: l.order.state } as unknown as OrderState },
    custody: l.custody ?? null, strays: [], refundDueDaa: null, deadline: null, source: 'chain',
  };
}

function harness(prefs: Partial<StandalonePrefs> = {}) {
  const chain = new FakeChain();
  const subs: { onBlock: (b: StreamBlock) => void; onGap?: (() => void) | undefined }[] = [];
  const node = {
    getUtxosByAddresses: (a: string[]) => chain.getUtxosByAddresses(a),
    subscribeBlocks: vi.fn(async (onBlock: (b: StreamBlock) => void, onGap?: () => void) => {
      const s = { onBlock, onGap };
      subs.push(s);
      return { close: async () => void subs.splice(subs.indexOf(s), 1) };
    }),
  };
  let answer: HealthView | Error = new Error('down');
  const real = { health: vi.fn(async () => (answer instanceof Error ? Promise.reject(answer) : answer)) };
  const c = new StandaloneController({ kob, node, toAddress: testAddress, network: 'testnet-10', indexer: real, prefs });
  let n = 0;
  const block = (txs: TxJson[], daa = 1_000n): void => {
    const b: StreamBlock = { hash: (++n).toString(16).padStart(64, '0'), daaScore: daa, timestampMs: Date.now(), transactions: txs.map(asAnnounced) };
    for (const s of [...subs]) s.onBlock(b);
  };
  return { c, chain, node, real, subs, block, setHealth: (h: HealthView | Error) => void (answer = h) };
}

beforeEach(() => {
  vi.useFakeTimers();
  vi.setSystemTime(new Date('2026-11-05T12:00:00Z'));
});
afterEach(() => vi.useRealTimers());

describe('the mode', () => {
  it('starts when the indexer stays bad (asked once more), ends when it is usable again', async () => {
    const h = harness();
    h.c.report(bad);
    expect(h.c.state.active).toBe(false);
    await vi.advanceTimersByTimeAsync(ENTER_CHECK_MS + 10);
    expect(h.real.health).toHaveBeenCalledTimes(1);
    expect(h.c.state).toMatchObject({ active: true, reason: 'auto', phase: 'live', sinceMs: Date.parse('2026-11-05T12:00:05.000Z') });
    expect(h.node.subscribeBlocks).toHaveBeenCalledTimes(1);
    h.c.report(good);
    expect(h.c.state).toMatchObject({ active: false, reason: null, phase: 'off' });
    expect(h.subs).toHaveLength(0);
  });

  it('a blip does not start it: the indexer answers the second look', async () => {
    const h = harness();
    h.c.report(bad);
    h.setHealth(health('following'));
    await vi.advanceTimersByTimeAsync(ENTER_CHECK_MS + 10);
    expect(h.c.state.active).toBe(false);
    // a usable report in between cancels the look
    h.setHealth(new Error('down'));
    h.c.report(bad);
    h.c.report(good);
    await vi.advanceTimersByTimeAsync(ENTER_CHECK_MS + 10);
    expect(h.c.state.active).toBe(false);
  });

  it('without automatic fallback it never starts by itself', async () => {
    const h = harness({ auto: false });
    h.c.report(bad);
    await vi.advanceTimersByTimeAsync(ENTER_CHECK_MS * 3);
    expect(h.c.state.active).toBe(false);
  });

  it('without automatic return it stays and says the indexer is back; the user switches back', async () => {
    const h = harness({ autoReturn: false });
    h.c.report(bad);
    await vi.advanceTimersByTimeAsync(ENTER_CHECK_MS + 10);
    h.c.report(good);
    expect(h.c.state).toMatchObject({ active: true, indexerBack: true });
    h.c.report(bad);
    expect(h.c.state.indexerBack).toBe(false);
    h.c.report(good);
    h.c.returnToIndexer();
    expect(h.c.state.active).toBe(false);
  });

  it('switched on in Settings: on from the start, the indexer reports change nothing, health is not routed to the indexer; switched off it ends', async () => {
    const h = harness({ manual: true });
    expect(h.c.state).toMatchObject({ active: true, reason: 'manual' });
    h.c.report(good);
    expect(h.c.state.active).toBe(true);
    const real = { health: vi.fn(async () => health('following')), book: vi.fn() } as unknown as IndexerApi;
    const ix = modeIndexer(real, h.c);
    await expect(ix.health()).rejects.toThrow(/Standalone mode/);
    expect((real.health as unknown as ReturnType<typeof vi.fn>)).not.toHaveBeenCalled();
    await ix.book(TOKEN);
    expect(real.book).not.toHaveBeenCalled();
    h.c.setPrefs({ manual: false });
    expect(h.c.state.active).toBe(false);
    await ix.health();
    expect(real.health).toHaveBeenCalledTimes(1);
  });

  it('routes to the indexer while the mode is off, to the standalone views (unavailable where the indexer is needed) while on', async () => {
    const h = harness();
    const real = { book: vi.fn(async () => ({ asks: [], bids: [] })), pairs: vi.fn(), health: vi.fn(async () => health('following')) } as unknown as IndexerApi;
    const ix = modeIndexer(real, h.c);
    await ix.book(TOKEN);
    expect(real.book).toHaveBeenCalledTimes(1);
    h.c.setPrefs({ manual: true });
    await expect(ix.pairs()).rejects.toMatchObject({ code: 'standalone_mode' });
    await expect(ix.allOrders({ maker: 'aa'.repeat(32) })).rejects.toMatchObject({ code: 'standalone_mode' });
    await expect(ix.order('bb'.repeat(32))).rejects.toMatchObject({ code: 'standalone_mode' });
    expect((await ix.book(TOKEN)).asks).toEqual([]);
    expect(real.book).toHaveBeenCalledTimes(1);
  });
});

describe('the book from the node', () => {
  it('lists a placement seen in a block once the node holds it; the book check confirms the view it gives', async () => {
    const h = harness({ manual: true });
    await vi.advanceTimersByTimeAsync(0);
    const ask = placeGolden(kob, 'create.ask');
    const bid = placeGolden(kob, 'create.bid');
    h.block([ask.signed.tx, bid.signed.tx]);
    await vi.advanceTimersByTimeAsync(10);
    // seen, not yet on the node: not listed
    expect(h.c.bookView(TOKEN).asks).toEqual([]);
    h.chain.applyTx(ask.signed.tx);
    h.chain.applyTx(bid.signed.tx);
    await vi.advanceTimersByTimeAsync(VERIFY_DELAY_MS + 10);
    const book = h.c.bookView(TOKEN);
    expect(book.asks.map((r) => (r as BookOrderView).covenant_id)).toEqual([ask.recovered[0]!.covenantId]);
    expect(book.bids.map((r) => (r as BookOrderView).covenant_id)).toEqual([bid.recovered[0]!.covenantId]);
    expect(book.asks[0]).toMatchObject({ price: '250000000', amount_left: '10000', amount_estimated: false, scale: 1000, cur_value: '1000000000' });
    expect((book.bids[0] as BookOrderView).amount_estimated).toBe(true);
    expect(h.c.state.orders).toBe(2);
    // the order view the book check reads becomes the snapshot it confirms on the node, exactly
    const view = h.c.orderView(ask.recovered[0]!.covenantId)!;
    const snap = snapshotFromOrderView(view);
    const [confirmed] = await confirmSnapshotsOnNode(kob, h.chain, [{ ...snap, strays: [] }], testAddress);
    expect(confirmed).toMatchObject({ covenantId: ask.recovered[0]!.covenantId, custody: { state: { amount: '10000' } } });
    // the token list: one token with an ask and a bid, no decimals of its own
    expect(h.c.tokensView()).toEqual([expect.objectContaining({ covenant_id: TOKEN, open_asks: 1, open_bids: 1, decimals: null, scale: null, template_hash: '173ca6a796c2c05f171c31b9a73aaca161a3226a9e8f57d2f3d833ff18dbe41b' })]);
    // aggregated: levels
    expect((h.c.bookView(TOKEN, { aggregate: true }).asks[0] as LevelView)).toMatchObject({ price: '250000000', amount: '10000', orders: 1, scale: 1000 });
  });

  it('a placement the node never shows is dropped after a few looks', async () => {
    const h = harness({ manual: true });
    await vi.advanceTimersByTimeAsync(0);
    const ask = placeGolden(kob, 'create.ask');
    h.block([ask.signed.tx]);
    await vi.advanceTimersByTimeAsync(VERIFY_DELAY_MS + 2_000 + 4_000 + 8_000 + 16_000 + 2_000);
    expect(h.c.orderView(ask.recovered[0]!.covenantId)).toBeNull();
    // seen in a later block (the same transaction): not read twice
    h.chain.applyTx(ask.signed.tx);
    h.block([ask.signed.tx]);
    await vi.advanceTimersByTimeAsync(VERIFY_DELAY_MS + 10);
    expect(h.c.bookView(TOKEN).asks).toEqual([]);
  });

  it('follows a fill: the continuation replaces the order, the fill becomes a trade once the node shows its transaction', async () => {
    const h = harness({ manual: true });
    await vi.advanceTimersByTimeAsync(0);
    const ask = legSnapshot('take.ask.partial');
    h.c.adopt([ask], 1_000n);
    expect(h.c.bookView(TOKEN).asks).toHaveLength(1);
    const take = clone(goldenTx('take.ask.partial').signed.tx) as TxJson;
    h.block([take], 1_200n);
    await vi.advanceTimersByTimeAsync(10);
    // the continuation waits for the node; the spent order is gone
    expect(h.c.bookView(TOKEN).asks).toEqual([]);
    h.chain.applyTx(take);
    await vi.advanceTimersByTimeAsync(VERIFY_DELAY_MS + 10);
    expect(h.c.bookView(TOKEN).asks[0]).toMatchObject({ covenant_id: ask.covenantId, amount_left: '6000', status: 'partial' });
    const trades = h.c.tradesView(TOKEN);
    expect(trades.price_basis).toBe('1000');
    expect(trades.items).toEqual([expect.objectContaining({ txid: take.id, price: '250000000', amount: '4000', quote: '1000000000', side: 'buy', fills: 1 })]);
    const stats = h.c.statsView(TOKEN);
    expect(stats).toMatchObject({ last: '250000000', last_side: 'buy', trades_24h: 1, volume_24h: '4000', best_ask: '250000000', best_bid: null, open_asks: 1 });
    const candles = h.c.candlesView(TOKEN, { interval: '1m' });
    expect(candles.items).toEqual([expect.objectContaining({ o: '250000000', c: '250000000', volume: '4000', trades: 1 })]);
    expect(h.c.depthView(TOKEN).asks[0]).toMatchObject({ price: '250000000', amount: '6000', cum_quote: '1500000000' });
  });

  it('a confirmed order the node no longer holds (a spend the stream missed) leaves at the next recheck', async () => {
    const h = harness({ manual: true });
    await vi.advanceTimersByTimeAsync(0);
    const ask = placeGolden(kob, 'create.ask');
    h.chain.applyTx(ask.signed.tx);
    h.block([ask.signed.tx]);
    await vi.advanceTimersByTimeAsync(VERIFY_DELAY_MS + 10);
    expect(h.c.bookView(TOKEN).asks).toHaveLength(1);
    // spent elsewhere: gone from the UTXO set
    h.chain.applyTx({ ...ask.signed.tx, id: 'cd'.repeat(32), inputs: [{ ...ask.signed.tx.inputs[0]!, transactionId: ask.signed.tx.id, index: ask.recovered[0]!.output }], outputs: [] });
    await vi.advanceTimersByTimeAsync(RECHECK_MS + 6_000);
    expect(h.c.bookView(TOKEN).asks).toEqual([]);
  });

  it('the wallet own orders: resolved receipts are listed at once and offered to the own-order check', async () => {
    const h = harness({ manual: true });
    const own = legSnapshot('take.ask.partial');
    h.c.adopt([own], 1_000n);
    const maker = (own.order.state.state as unknown as { maker: string }).maker;
    expect(h.c.ownOrders(maker, TOKEN).map((v) => v.covenant_id)).toEqual([own.covenantId]);
    expect(h.c.ownOrders('aa'.repeat(32))).toEqual([]);
    expect(h.c.orderView(own.covenantId)!.origin).toBe('receipt');
  });

  it('leaving the mode forgets everything: a new mode starts empty', async () => {
    const h = harness({ manual: true });
    h.c.adopt([legSnapshot('take.ask.partial')], 1_000n);
    h.c.setPrefs({ manual: false });
    h.c.setPrefs({ manual: true });
    expect(h.c.bookView(TOKEN).asks).toEqual([]);
  });
});

describe('listing rules (stream-book)', () => {
  const ctx = { kob, daa: 1_000n, nowUnix: 1_790_000_000n };
  const put = (snap: OrderSnapshot) => {
    const b = new StreamBook();
    b.put(snap, { origin: 'stream', nowMs: 1, confirmed: true });
    return b;
  };
  const askSnap = () => legSnapshot('take.ask.partial');
  const withState = (s: OrderSnapshot, f: Record<string, string>): OrderSnapshot => ({ ...s, order: { ...s.order, state: { ...s.order.state, state: { ...s.order.state.state, ...f } } as OrderState } });

  it('lists a confirmed, live, plain limit order; an unconfirmed one is not listed', () => {
    const b = new StreamBook();
    const e = b.put(askSnap(), { origin: 'stream', nowMs: 1 });
    expect(listingOf(e, ctx)).toBeNull();
    expect(listingOf(put(askSnap()).get(askSnap().covenantId)!, ctx)).toMatchObject({ side: 'sell', price: 250000000n, amount: 10000n });
  });

  it('leaves out IOC / FOK, auctions, inactive, expired and past-deadline orders, and dust', () => {
    const cases: [string, OrderSnapshot][] = [
      ['ioc', withState(askSnap(), { tif: '1' })],
      ['auction', withState(askSnap(), { slope: '5', priceEnd: '1', interval: '10' })],
      ['inactive', withState(askSnap(), { activeFrom: '5000' })],
      ['expired', withState(askSnap(), { expiryDaa: '900' })],
      ['deadline', { ...askSnap(), deadline: 1_700_000_000n }],
      ['dust', withState(askSnap(), { price: '1' })],
      ['no custody', { ...askSnap(), custody: null }],
    ];
    for (const [what, s] of cases) expect(listingOf(put(s).get(s.covenantId)!, ctx), what).toBeNull();
    expect(MIN_ORDER_VALUE_SOMPI).toBe(100_000_000n);
  });

  it('an order of a registry token is listed only with the registry program, commitment and scale', () => {
    const s = askSnap();
    const e = put(s).get(s.covenantId)!;
    const reg = { templateHash: '173ca6a796c2c05f171c31b9a73aaca161a3226a9e8f57d2f3d833ff18dbe41b', extensionCommitment: 'ee'.repeat(32), scale: 1000n };
    expect(listingOf(e, { ...ctx, registry: () => reg })).not.toBeNull();
    expect(listingOf(e, { ...ctx, registry: () => ({ ...reg, scale: 100n }) })).toBeNull();
    expect(listingOf(e, { ...ctx, registry: () => ({ ...reg, templateHash: '00'.repeat(32) }) })).toBeNull();
    expect(listingOf(e, { ...ctx, registry: () => ({ ...reg, extensionCommitment: '11'.repeat(32) }) })).toBeNull();
  });

  it('sorts asks up and bids down by price per base unit, then by arrival', () => {
    const b = new StreamBook();
    const a1 = askSnap();
    const a2 = { ...withState(askSnap(), { price: '240000000' }), covenantId: 'a2'.repeat(32) };
    const a3 = { ...askSnap(), covenantId: 'a3'.repeat(32) };
    b.put(a1, { origin: 'stream', nowMs: 1, confirmed: true });
    b.put(a2, { origin: 'stream', nowMs: 2, confirmed: true });
    b.put(a3, { origin: 'stream', nowMs: 3, confirmed: true });
    const v = bookViewOf(b.entries(), TOKEN, ctx);
    expect(v.asks.map((r) => (r as BookOrderView).covenant_id)).toEqual([a2.covenantId, a1.covenantId, a3.covenantId]);
    expect(bookViewOf(b.entries(), TOKEN, ctx, { depth: 1 }).asks).toHaveLength(1);
    expect((bookViewOf(b.entries(), TOKEN, ctx, { aggregate: true }).asks as LevelView[]).map((l) => [l.price, l.orders])).toEqual([['240000000', 1], ['250000000', 2]]);
    expect(tokensViewOf(b.entries(), ctx)[0]).toMatchObject({ open_asks: 3, open_bids: 0 });
    expect(orderViewOf(b.get(a2.covenantId)!, ctx)).toMatchObject({ state_known: true, status: 'open', side: 1, price: '240000000' });
  });
});

describe('what the indexer gave before the mode is kept', () => {
  const OTHER = '71'.repeat(32);
  const TEMPLATE = '173ca6a796c2c05f171c31b9a73aaca161a3226a9e8f57d2f3d833ff18dbe41b';
  const row = (id: string, over: Record<string, unknown> = {}) => ({ ticker: id === TOKEN ? 'TUSD' : '', covenant_id: id, template_hash: TEMPLATE, extension_commitment: null, decimals: null, scale: null, market_scale: 1000, open_asks: 5, open_bids: 2, family: 'kcc20', ...over });
  const OLD_BAR = Date.parse('2026-11-05T11:00:00Z');
  /** an indexer that has loaded a market: two tokens, one hour bar, trades and 24 h figures priced per 1000 base units */
  function loaded(scaleOfToken = 1000) {
    return {
      health: vi.fn(async () => health('following')),
      tokens: vi.fn(async () => [row(TOKEN, { market_scale: scaleOfToken }), row(OTHER)]),
      candles: vi.fn(async (token: string, q: { interval: string }) => ({ token, interval: q.interval, price_basis: '1000', items: [{ t: OLD_BAR, o: '230000000', h: '250000000', l: '220000000', c: '240000000', volume: '1000', quote_volume: '100', trades: 4 }] })),
      trades: vi.fn(async (token: string) => ({ token, price_basis: '1000', next_cursor: null, items: [{ id: 777, txid: 'cc'.repeat(32), ts: OLD_BAR, daa: 5, price: '240000000', amount: '1000', quote: '100', side: 'sell', fills: 1, confirmations: 30, settled: true }] })),
      stats: vi.fn(async (token: string) => ({
        token, price_basis: '1000', ts: Date.now(), last: '240000000', last_ts: OLD_BAR, last_side: 'sell', open_24h: '230000000', high_24h: '250000000', low_24h: '220000000', change_24h_bps: 434, volume_24h: '1000',
        quote_volume_24h: '100', trades_24h: 4, best_bid: null, best_ask: null, mid: null, spread_bps: null, open_asks: 5, open_bids: 2,
      })),
      book: vi.fn(async () => ({ asks: [], bids: [] })),
    } as unknown as IndexerApi;
  }

  it('the token list, candles, trades and 24 h figures read before the mode stay, with the node fills added; the book is the node one', async () => {
    const h = harness();
    const real = loaded();
    const ix = modeIndexer(real, h.c);
    await ix.tokens();
    await ix.candles(TOKEN, { interval: '1h' });
    await ix.candles(TOKEN, { interval: '1m' });
    await ix.trades(TOKEN, { limit: 50 });
    await ix.stats(TOKEN);
    h.c.setPrefs({ manual: true });
    await vi.advanceTimersByTimeAsync(0);
    const calls = (real.stats as unknown as ReturnType<typeof vi.fn>).mock.calls.length;

    // the tokens: both stay with the facts the indexer gave (TUSD, its scale), no orders known on the node
    const tokens = await ix.tokens();
    expect(tokens.map((t) => [t.covenant_id, t.ticker, t.market_scale, t.open_asks])).toEqual([[TOKEN, 'TUSD', 1000, 0], [OTHER, '', 1000, 0]]);
    expect(h.c.carry.tokenAt(TOKEN)).not.toBeNull();
    // the market figures as read, before the node showed anything; the book is the node one (empty), a stale indexer book is not offered
    expect((await ix.candles(TOKEN, { interval: '1h' }))!.items.map((b) => b.t)).toEqual([OLD_BAR]);
    expect(await ix.stats(TOKEN)).toMatchObject({ last: '240000000', volume_24h: '1000', trades_24h: 4, price_basis: '1000' });
    expect((await ix.trades(TOKEN, { limit: 50 }))!.items.map((t) => t.id)).toEqual([777]);
    expect((await ix.book(TOKEN)).asks).toEqual([]);
    expect((real.stats as unknown as ReturnType<typeof vi.fn>).mock.calls.length).toBe(calls);
    expect(real.book).not.toHaveBeenCalled();

    // a fill the node shows: added to the kept bars, the trades and the figures
    const ask = legSnapshot('take.ask.partial');
    h.c.adopt([ask], 1_000n);
    const take = clone(goldenTx('take.ask.partial').signed.tx) as TxJson;
    h.block([take], 1_200n);
    await vi.advanceTimersByTimeAsync(10);
    h.chain.applyTx(take);
    await vi.advanceTimersByTimeAsync(VERIFY_DELAY_MS + 10);
    expect((await ix.tokens()).find((t) => t.covenant_id === TOKEN)).toMatchObject({ ticker: 'TUSD', open_asks: 1 });
    const bars = (await ix.candles(TOKEN, { interval: '1h' }))!.items;
    expect(bars.map((b) => b.t)).toEqual([OLD_BAR, Math.floor(Date.now() / 3_600_000) * 3_600_000]);
    expect(bars[1]).toMatchObject({ o: '250000000', c: '250000000', volume: '4000', trades: 1 });
    // an incremental read from the newest bar on screen gets the merged bars from there
    expect((await ix.candles(TOKEN, { interval: '1h', from: bars[1]!.t }))!.items.map((b) => b.t)).toEqual([bars[1]!.t]);
    expect((await ix.trades(TOKEN, { limit: 50 }))!.items.map((t) => t.id === 777 ? 'kept' : t.txid)).toEqual([take.id, 'kept']);
    expect(await ix.stats(TOKEN)).toMatchObject({ last: '250000000', last_side: 'buy', open_24h: '230000000', high_24h: '250000000', low_24h: '220000000', volume_24h: '5000', trades_24h: 5, price_basis: '1000' });
    // what the node saw is priced on the indexer's basis, and the order listed is of the token as the indexer gave it
    expect(h.c.bookView(TOKEN).asks).toHaveLength(1);

    // back to the indexer: its data is used again, what the mode collected is dropped, the carry goes on recording
    h.c.setPrefs({ manual: false });
    expect(h.c.state.active).toBe(false);
    expect((await ix.candles(TOKEN, { interval: '1h' }))!.items.map((b) => b.t)).toEqual([OLD_BAR]);
    expect(h.c.tradesView(TOKEN).items.map((t) => t.id)).toEqual([777]);
  });

  it('an order of a token is listed against the facts the indexer gave for it: another scale or program is not', async () => {
    const h = harness();
    const ix = modeIndexer(loaded(100), h.c);
    await ix.tokens();
    h.c.setPrefs({ manual: true });
    h.c.adopt([legSnapshot('take.ask.partial')], 1_000n);
    // the golden order prices per 1000 base units: not the scale 100 the indexer gave this market
    expect(h.c.bookView(TOKEN).asks).toEqual([]);
    expect(h.c.carry.scaleOf(TOKEN)).toBe(100);
  });

  it('a token the page never read from the indexer is not listed and has no kept figures; nothing is kept when the mode is on from the start', async () => {
    const h = harness({ manual: true });
    const ix = modeIndexer(loaded(), h.c);
    expect(await ix.tokens()).toEqual([]);
    expect(h.c.carry.tokenAt(TOKEN)).toBeNull();
    expect(h.c.carry.marketAt(TOKEN)).toBeNull();
    expect((await ix.candles(TOKEN, { interval: '1h' }))!.items).toEqual([]);
    expect((await ix.stats(TOKEN))!.last).toBeNull();
  });
});
