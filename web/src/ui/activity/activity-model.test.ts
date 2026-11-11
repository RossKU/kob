// The activity log's view model: records -> one row per transaction (kind, side, market icons, wallets, amounts, the explorer link per network),
// the filters and their stored form, the hold that keeps rows still under the pointer, and the confirmation clock.
import { describe, expect, it } from 'vitest';
import type { EventView, OrderView, PairFillView } from '../../data/indexer-types';
import {
  ACTIVITY_DEPTH, AVAILABLE_KINDS, DEFAULT_PREFS, PREFS_KEY, SETTLE_DAA, amountCell, confirmationsAt, daaNow, filterRows, fillRows, heldView, inMarket, logHasNext, logPage, logPriceDp, mergePairFills, mergeRows, orientRow, pairFillEvent, parsePrefs,
  placedRows, priceRefOf, readPrefs, rowHref, rowMakers, shownKinds, walletBody, writePrefs,
  type ActivityRow, type OrderFacts, type RowContext,
} from './activity-model';
import { formatRowTime } from '../kit/format';

const T = 'aa'.repeat(32); // a token (8 decimals: scale 1e8)
const U = 'bb'.repeat(32); // another token
const TX1 = '11'.repeat(32);
const TX2 = '22'.repeat(32);
const TX3 = '33'.repeat(32);
const ASK = 'a1'.repeat(32);
const BID = 'b1'.repeat(32);
const MAKER_A = '0a'.repeat(32);
const MAKER_B = '0b'.repeat(32);
const S = 100_000_000n;

let nid = 1;
const fill = (o: Partial<EventView> = {}): EventView => ({
  id: nid++, covenant_id: ASK, block_seq: 1, daa: 1_000, ts: 1_700_000_000_000, txid: TX1, tx_pos: 0, kind: 'fill', token: T, side: 1, amount: '200000000', price: '2500000',
  payout: null, closes: false, detail: null, confirmations: 3, settled: false, ...o,
});

function ctxOf(facts: Record<string, OrderFacts> = {}, o: Partial<RowContext> = {}): RowContext {
  return { tokenScale: (t) => (t === T || t === U ? S : null), order: (id) => facts[id], timeOfDaa: () => null, ...o };
}

const order = (o: Partial<OrderView> & { covenant_id: string }): OrderView =>
  ({
    contract: 'KobAsk', template_hash: '', family: 1, side: 1, maker: MAKER_A, token: T, token_template_hash: null, scale: 100_000_000, min_fill: '1', price: '2500000', tip: '0', tif: null,
    expiry_daa: null, active_from: null, in_book: true, budget_rate: null, reserve: null, initial_amount: '100000000', listed: true, unlisted_reason: null, origin: 'genesis', parent: null,
    genesis: { txid: TX2, out: 0, block_seq: 7, daa: 2_000, confirmations: 40, settled: false }, status: 'open', filled_amount: '0', amount_left: '100000000', amount_estimated: false,
    current: null, state_known: true, last_block: 0, last_daa: 2_000, confirmations: 40, settled: false, expired: false, ...o,
  }) as OrderView;

describe('fills: one row per transaction', () => {
  it('an ask filled: the taker bought; the token amount, its KAS value and the price of the order', () => {
    const [r] = fillRows([fill()], ctxOf());
    expect(r).toMatchObject({ key: TX1, kind: 'fill', side: 'buy', market: { kind: 'token', token: T }, amount: { token: T, units: 200_000_000n }, value: { kind: 'kas', sompi: 5_000_000n } });
    expect(r!.price).toEqual({ low: 2_500_000n, high: 2_500_000n, scale: S });
    expect(r!.timeMs).toBe(1_700_000_000_000);
    expect(r!.timeEstimated).toBe(false);
  });

  it('a bid filled: the taker sold', () => {
    expect(fillRows([fill({ side: 2, covenant_id: BID })], ctxOf())[0]!.side).toBe('sell');
  });

  it('a cross of two KOB orders in one transaction is ONE row: the resting (older) order sets side, amount and price; both owners are named', () => {
    const evs = [fill({ covenant_id: ASK, side: 1, amount: '100000000', price: '2600000' }), fill({ covenant_id: BID, side: 2, amount: '100000000', price: '2700000' })];
    const facts = { [ASK]: { maker: MAKER_A, scale: S, genesisDaa: 900 }, [BID]: { maker: MAKER_B, scale: S, genesisDaa: 500 } };
    const rows = fillRows(evs, ctxOf(facts));
    expect(rows).toHaveLength(1);
    // the bid rested (older): the taker sold into it, at the bid's price
    expect(rows[0]).toMatchObject({ side: 'sell', amount: { units: 100_000_000n }, price: { low: 2_700_000n } });
    expect(rows[0]!.orders).toEqual([BID, ASK]);
    expect(rowMakers(rows[0]!, ctxOf(facts))).toEqual([MAKER_B, MAKER_A]);
    // the order ages not known yet: the asks rest (the indexer's tie rule)
    expect(fillRows(evs, ctxOf())[0]!.side).toBe('buy');
  });

  it('several fills of one order at different prices (a ladder taken): the price range, the summed amount and value', () => {
    const rows = fillRows([fill({ price: '2500000' }), fill({ price: '2600000', covenant_id: 'a2'.repeat(32) })], ctxOf());
    expect(rows[0]).toMatchObject({ amount: { units: 400_000_000n }, value: { sompi: 10_200_000n }, price: { low: 2_500_000n, high: 2_600_000n } });
  });

  it('a token without known decimals: the order\'s own scale (from the lookup) prices it; before that the KAS value is unknown', () => {
    const X = 'cc'.repeat(32);
    expect(fillRows([fill({ token: X })], ctxOf())[0]!.value).toBeNull();
    const r = fillRows([fill({ token: X })], ctxOf({ [ASK]: { maker: MAKER_A, scale: 1_000n, genesisDaa: 1 } }))[0]!;
    expect(r.value).toEqual({ kind: 'kas', sompi: 500_000_000_000n });
  });

  it('a pair fill (with the KAS legs of its route) is the pair\'s row: A amount, B amount, no price, how it was filled', () => {
    const pairFill = fill({
      covenant_id: 'f1'.repeat(32), price: null, side: 1, amount: '300000000',
      detail: { pair: { side: 'ask', base: T, quote: U, amount_a: '300000000', amount_b: '4500000', counterparty: 'route' } },
    });
    const legs = [fill({ token: T, side: 2, covenant_id: BID }), fill({ token: U, side: 1, covenant_id: ASK })];
    const [r] = fillRows([pairFill, ...legs], ctxOf());
    expect(r).toMatchObject({ market: { kind: 'pair', base: T, quote: U }, side: 'buy', amount: { token: T, units: 300_000_000n }, value: { kind: 'token', token: U, units: 4_500_000n }, price: null, counterparty: 'route', moreMarkets: 0 });
    // the pair order first, then the KAS legs' orders (their owners are the counterparties)
    expect(r!.orders[0]).toBe('f1'.repeat(32));
    expect(r!.orders).toContain(BID);
  });

  it('a fill of `/v1/pairs/{a}/{b}/fills` builds the same row as the feed\'s event', () => {
    const f: PairFillView = {
      id: 9, txid: TX3, ts: 5, daa: 77, order: 'f1'.repeat(32), contract: 'KobPair', side: 'bid', amount_a: '50', amount_b: '7', price: null, price_num: null, price_den: null, a_scale: 1,
      tip_kas: null, counterparty: 'netting', price_source: 'none', confirmations: 120, settled: true,
    };
    const [r] = fillRows([pairFillEvent(f, T, U)], ctxOf());
    expect(r).toMatchObject({ key: TX3, side: 'sell', market: { kind: 'pair', base: T, quote: U }, amount: { units: 50n }, value: { units: 7n }, counterparty: 'netting', settled: true, confirmations: 120 });
  });

  it('other kinds and malformed records make no fill row', () => {
    expect(fillRows([fill({ kind: 'cancel' }), fill({ txid: 'nope' })], ctxOf())).toEqual([]);
  });
});

describe('placements: one row per placement transaction', () => {
  it('an ask: its amount, value and price; the time is estimated from its DAA score', () => {
    const [r] = placedRows([order({ covenant_id: ASK })], ctxOf({}, { timeOfDaa: (d) => d * 100 }));
    expect(r).toMatchObject({ key: TX2, kind: 'placed', side: 'sell', placed: 1, market: { kind: 'token', token: T }, amount: { units: 100_000_000n }, value: { sompi: 2_500_000n }, timeMs: 200_000, timeEstimated: true, confirmations: 40, seq: 7 });
    expect(placedRows([order({ covenant_id: ASK })], ctxOf())[0]!.timeMs).toBeNull();
  });

  it('a ladder (several orders in one transaction) is one row with the price range and the summed amount', () => {
    const os = [order({ covenant_id: 'c1'.repeat(32), price: '2500000', side: 2 }), order({ covenant_id: 'c2'.repeat(32), price: '2700000', side: 2 }), order({ covenant_id: 'c3'.repeat(32), price: '2600000', side: 2 })];
    const [r] = placedRows(os, ctxOf());
    expect(r).toMatchObject({ placed: 3, side: 'buy', amount: { units: 300_000_000n }, value: { sompi: 7_800_000n }, price: { low: 2_500_000n, high: 2_700_000n } });
  });

  it('a pair order: the pair (in its orientation), its side, the A amount, no price', () => {
    const o = order({ covenant_id: 'd1'.repeat(32), contract: 'KobPair', price: null, pair: { base: T, quote: U, side: 'bid' } as OrderView['pair'] });
    const [r] = placedRows([o], ctxOf());
    expect(r).toMatchObject({ market: { kind: 'pair', base: T, quote: U }, pairSide: 'bid', side: 'buy', amount: { token: T, units: 100_000_000n }, value: null, price: null });
  });

  it('an order without its placement transaction is not listed', () => {
    expect(placedRows([order({ covenant_id: ASK, genesis: { txid: null, out: null, block_seq: 0, daa: 1, confirmations: null, settled: false } })], ctxOf())).toEqual([]);
  });
});

describe('merged rows', () => {
  const ctx = ctxOf();
  it('newest first (DAA score, then sequence); a transaction in two lists keeps the strongest kind and names both', () => {
    const f = fillRows([fill({ txid: TX2, daa: 2_000 }), fill({ txid: TX1, daa: 3_000 })], ctx);
    const p = placedRows([order({ covenant_id: ASK })], ctx); // TX2
    const rows = mergeRows([f, p]);
    expect(rows.map((r) => r.key)).toEqual([TX1, TX2]);
    expect(rows[1]).toMatchObject({ kind: 'fill', kinds: ['fill', 'placed'] });
  });

  it('keeps at most the cap', () => {
    const many = fillRows(Array.from({ length: 30 }, (_, i) => fill({ txid: i.toString(16).padStart(64, '0'), daa: i })), ctx);
    expect(mergeRows([many], 10)).toHaveLength(10);
    expect(mergeRows([many], 10)[0]!.daa).toBe(29);
  });
});

describe('the explorer link of a row follows the network', () => {
  it('kaspa.stream on mainnet, tn10.kaspa.stream on testnet-10, the configured explorer otherwise; the txid lower case', () => {
    const r = { txid: 'AB'.repeat(32) };
    expect(rowHref({ network: 'mainnet', explorerUrl: '' }, r)).toBe(`https://kaspa.stream/transactions/${'ab'.repeat(32)}`);
    expect(rowHref({ network: 'testnet-10', explorerUrl: '' }, r)).toBe(`https://tn10.kaspa.stream/transactions/${'ab'.repeat(32)}`);
    expect(rowHref({ network: 'testnet-10', explorerUrl: 'https://explorer.example' }, r)).toBe(`https://explorer.example/transactions/${'ab'.repeat(32)}`);
    expect(rowHref({ network: 'mainnet', explorerUrl: '' }, { txid: 'x' })).toBeNull();
  });
});

describe('filters', () => {
  const ctx = ctxOf({ [ASK]: { maker: MAKER_A, scale: S, genesisDaa: 1 }, [BID]: { maker: MAKER_B, scale: S, genesisDaa: 1 } });
  const rows = mergeRows([
    fillRows([fill({ txid: TX1, daa: 5 }), fill({ txid: TX3, covenant_id: BID, side: 2, daa: 4, detail: { pair: { side: 'bid', base: U, quote: T, amount_a: '1', amount_b: '1' } } })], ctx),
    placedRows([order({ covenant_id: ASK })], ctx),
  ]);
  const makersOf = (r: ActivityRow) => rowMakers(r, ctx);
  const keys = (rs: ActivityRow[]) => rs.map((r) => r.key);

  it('phase 1 offers the kinds the current API lists: placed and fills (cancels are on by default for when they come)', () => {
    expect(AVAILABLE_KINDS).toEqual(['placed', 'fill']);
    expect(DEFAULT_PREFS.kinds).toEqual({ placed: true, fill: true, cancel: true, refund: false, stop: false });
    expect([...shownKinds(DEFAULT_PREFS)]).toEqual(['placed', 'fill']);
  });

  it('the quick switch: Orders = signed by the owner (placed), Executions = sent by an executor (fills)', () => {
    expect(keys(filterRows(rows, { prefs: DEFAULT_PREFS, market: { kind: 'all' }, mine: null, makersOf }))).toEqual([TX2, TX1, TX3]);
    expect(keys(filterRows(rows, { prefs: { ...DEFAULT_PREFS, actor: 'orders' }, market: { kind: 'all' }, mine: null, makersOf }))).toEqual([TX2]);
    expect(keys(filterRows(rows, { prefs: { ...DEFAULT_PREFS, actor: 'executions' }, market: { kind: 'all' }, mine: null, makersOf }))).toEqual([TX1, TX3]);
    expect(keys(filterRows(rows, { prefs: { ...DEFAULT_PREFS, kinds: { ...DEFAULT_PREFS.kinds, fill: false } }, market: { kind: 'all' }, mine: null, makersOf }))).toEqual([TX2]);
  });

  it('the market: a token\'s KAS book, or a pair in either orientation', () => {
    expect(keys(filterRows(rows, { prefs: DEFAULT_PREFS, market: { kind: 'token', token: T }, mine: null, makersOf }))).toEqual([TX2, TX1]);
    expect(keys(filterRows(rows, { prefs: DEFAULT_PREFS, market: { kind: 'pair', base: T, quote: U }, mine: null, makersOf }))).toEqual([TX3]);
    expect(inMarket({ market: { kind: 'pair', base: U, quote: T } }, { kind: 'token', token: U })).toBe(false);
  });

  it('mine only: the rows about the connected wallet\'s orders; without a wallet it does nothing', () => {
    const mine = { ...DEFAULT_PREFS, mine: true };
    expect(keys(filterRows(rows, { prefs: mine, market: { kind: 'all' }, mine: MAKER_B, makersOf }))).toEqual([TX3]);
    expect(keys(filterRows(rows, { prefs: mine, market: { kind: 'all' }, mine: MAKER_A.toUpperCase(), makersOf }))).toEqual([TX2, TX1]);
    expect(keys(filterRows(rows, { prefs: mine, market: { kind: 'all' }, mine: null, makersOf }))).toHaveLength(3);
  });
});

describe('stored filters (per viewer)', () => {
  const memory = () => {
    const m = new Map<string, string>();
    return { getItem: (k: string) => m.get(k) ?? null, setItem: (k: string, v: string) => void m.set(k, v), m };
  };

  it('defaults without storage, with nothing stored, or with a damaged record', () => {
    expect(readPrefs('page', null)).toEqual(DEFAULT_PREFS);
    expect(readPrefs('page', memory())).toEqual(DEFAULT_PREFS);
    const s = memory();
    s.setItem(PREFS_KEY, '{not json');
    expect(readPrefs('market', s)).toEqual(DEFAULT_PREFS);
  });

  it('one record for the Activity page and one shared by the token and pair pages; writing one keeps the other', () => {
    const s = memory();
    expect(writePrefs('page', { ...DEFAULT_PREFS, actor: 'executions' }, s)).toBe(true);
    expect(writePrefs('market', { ...DEFAULT_PREFS, kinds: { ...DEFAULT_PREFS.kinds, placed: false }, mine: true }, s)).toBe(true);
    expect(readPrefs('page', s).actor).toBe('executions');
    expect(readPrefs('page', s).kinds.placed).toBe(true);
    expect(readPrefs('market', s)).toMatchObject({ actor: 'all', mine: true, kinds: { placed: false } });
  });

  it('unknown or malformed fields take their default; a store that throws keeps the defaults and reports the write', () => {
    expect(parsePrefs({ kinds: { placed: 'yes', stop: true, bogus: true }, actor: 'robots', mine: 1 })).toEqual({ ...DEFAULT_PREFS, kinds: { ...DEFAULT_PREFS.kinds, stop: true } });
    const bad = { getItem: () => { throw new Error('denied'); }, setItem: () => { throw new Error('denied'); } };
    expect(readPrefs('page', bad)).toEqual(DEFAULT_PREFS);
    expect(writePrefs('page', DEFAULT_PREFS, bad)).toBe(false);
  });
});

describe('the hold: rows never move under the pointer', () => {
  const r = (key: string) => ({ key });
  it('without a hold every row shows', () => {
    expect(heldView([r('c'), r('b'), r('a')], null)).toEqual({ rows: [r('c'), r('b'), r('a')], held: 0 });
  });

  it('while held, rows that would land among or above the shown ones wait and are counted; older ones (a read further back) are appended', () => {
    const snap = new Set(['b', 'a']);
    // `d` and `c` are new at the top, `x` lands between the shown ones, `z` is older than all of them
    const v = heldView([r('d'), r('c'), r('b'), r('x'), r('a'), r('z')], snap);
    expect(v.rows.map((x) => x.key)).toEqual(['b', 'a', 'z']);
    expect(v.held).toBe(3);
  });

  it('a shown row that disappeared (a reorg) is simply gone; nothing else moves', () => {
    const v = heldView([r('n'), r('a')], new Set(['b', 'a']));
    expect(v.rows.map((x) => x.key)).toEqual(['a']);
    expect(v.held).toBe(1);
  });
});

describe('the pages of the log', () => {
  const rows = (n: number, from = 0) => Array.from({ length: n }, (_, i) => ({ key: `r${from + i}` }));

  it('a page never shows more than the chosen rows', () => {
    for (const size of [20, 50, 100]) {
      for (let page = 0; page < 8; page++) expect(logPage(rows(300), size, page).shown.length).toBeLessThanOrEqual(size);
    }
  });

  it('the depth is the same whatever the size: 20 rows make 5 pages, 50 make 2, 100 make 1', () => {
    expect(ACTIVITY_DEPTH).toBe(100);
    expect(logPage(rows(300), 20, 0).pages).toBe(5);
    expect(logPage(rows(300), 50, 0).pages).toBe(2);
    expect(logPage(rows(300), 100, 0).pages).toBe(1);
    expect(logPage(rows(300), 20, 99)).toMatchObject({ page: 4, from: 80, to: 100 });
    expect(logPage(rows(300), 20, 4).shown.at(-1)).toEqual({ key: 'r99' });
    expect(logPage(rows(300), 20, 0).kept).toHaveLength(100);
  });

  it('a new row pushes the oldest off', () => {
    const before = logPage(rows(100, 1), 100, 0).shown;
    const after = logPage([{ key: 'r0' }, ...rows(100, 1)], 100, 0).shown;
    expect(after).toHaveLength(100);
    expect(after[0]).toEqual({ key: 'r0' });
    expect(after.at(-1)).toEqual(before.at(-2));
  });

  it('the hold of one older page counts no row of the other pages as new', () => {
    const all = rows(100);
    const snapshot = new Set(logPage(all, 20, 2).kept.map((r) => r.key));
    const v = heldView([{ key: 'n2' }, { key: 'n1' }, ...all], snapshot);
    expect(v.held).toBe(2);
    expect(logPage(v.rows, 20, 2).shown).toEqual(all.slice(40, 60));
  });

  it('it turns on while the rows are there, or older ones may be and the depth is not reached', () => {
    const w = (n: number, size: number, page: number) => logPage(rows(n), size, page);
    expect(logHasNext(w(48, 20, 0), 20, 0, false)).toBe(true);
    expect(logHasNext(w(48, 20, 2), 20, 2, false)).toBe(false);
    expect(logHasNext(w(48, 20, 2), 20, 2, true)).toBe(true);
    // the depth: page 5 of 20, page 2 of 50, the only page of 100
    expect(logHasNext(w(300, 20, 4), 20, 4, true)).toBe(false);
    expect(logHasNext(w(300, 50, 1), 50, 1, true)).toBe(false);
    expect(logHasNext(w(300, 100, 0), 100, 0, true)).toBe(false);
    // a page asked for and not there yet: no second press
    expect(logHasNext(w(48, 20, 3), 20, 3, true)).toBe(false);
  });
});

describe('confirmations and time', () => {
  it('the chain DAA advances 10 per second after a report, for at most a minute', () => {
    expect(daaNow(null, 5)).toBeNull();
    expect(daaNow({ daa: 1_000, atMs: 0 }, 2_500)).toBe(1_025);
    expect(daaNow({ daa: 1_000, atMs: 0 }, 3_600_000)).toBe(1_600);
  });

  it('a row confirms from the chain DAA (never fewer than reported) and settles at 100', () => {
    const row = { daa: 1_000, confirmations: 5, settled: false };
    expect(confirmationsAt(row, null)).toEqual({ n: 5, settled: false });
    expect(confirmationsAt(row, 1_064)).toEqual({ n: 64, settled: false });
    expect(confirmationsAt(row, 1_000 + SETTLE_DAA)).toEqual({ n: SETTLE_DAA, settled: true });
    expect(confirmationsAt({ ...row, settled: true }, null).settled).toBe(true);
  });

  it('the time column is the kit time: HH:MM:SS today, YYYY-MM-DD HH:MM before, a dash without a time', () => {
    const now = new Date(2026, 10, 6, 12, 0, 0).getTime();
    expect(formatRowTime(new Date(2026, 10, 6, 9, 5, 7).getTime(), now)).toBe('09:05:07');
    expect(formatRowTime(new Date(2026, 10, 4, 23, 59, 1).getTime(), now)).toBe('2026-11-04 23:59');
    expect(formatRowTime(null, now)).toBe('—');
  });

  it('the wallet column drops the network prefix and keeps the first and last six characters', () => {
    // whole, the prefix dropped: never a middle ellipsis (founder 2026-11-12); the cell ends it with an ellipsis where it has no room
    expect(walletBody('kaspatest:qz7vabcdefghk3f2')).toBe('qz7vabcdefghk3f2');
    expect(walletBody('qq9d0hxa')).toBe('qq9d0hxa');
  });
});

describe('number formats of the log (review H-05, M-03, L-05)', () => {
  it('an amount shows at most four fraction digits, plain digits, with the exact amount for the title', () => {
    expect(amountCell(1_500_000_000n, 8)).toEqual({ text: '15', exact: null });
    expect(amountCell(123_456_789n, 8)).toEqual({ text: '1.2346', exact: '1.23456789' });
    // never a subscript-zero form, never a zero for something that is there
    expect(amountCell(85_000n, 8).text).toBe('0.0009');
    expect(amountCell(85n, 8)).toEqual({ text: '<0.0001', exact: '0.00000085' });
    expect(amountCell(0n, 8).text).toBe('0');
    expect(amountCell(1n, 0).text).toBe('1');
  });

  it('the Price column has ONE number of decimals: the finest the rows on the page need, at most 6', () => {
    const row = (high: bigint, scale = S): Pick<ActivityRow, 'price' | 'market'> => ({ price: { low: high, high, scale }, market: { kind: 'token', token: T } });
    const dec = () => 8;
    // 33.4 KAS needs 3 decimals, 0.0255 KAS needs 6 (5 significant digits, capped)
    expect(priceRefOf(row(3_341_892_984n), dec)).toBe(3_341_892_984n);
    expect(logPriceDp([row(3_341_892_984n)], dec)).toBe(3);
    expect(logPriceDp([row(2_550_000n)], dec)).toBe(6);
    expect(logPriceDp([row(3_341_892_984n), row(2_550_000n)], dec)).toBe(6);
    expect(logPriceDp([row(1n)], dec)).toBe(6);
    // a pair row has no price: the log falls back to the rule's default
    expect(priceRefOf({ price: null, market: { kind: 'pair', base: T, quote: U } }, dec)).toBeNull();
    expect(logPriceDp([], dec)).toBe(4);
  });
});

describe('a pair market shows both orientations, turned to the page', () => {
  const A = 'a'.repeat(64);
  const B = 'b'.repeat(64);
  const fill = (id: number, side: 'ask' | 'bid', a: string, b: string) => ({ id, txid: id.toString(16).padStart(64, '0'), ts: 1000 + id, daa: id, order: 'c'.repeat(64), contract: 'KobPair', side, amount_a: a, amount_b: b, price: null, price_num: null, price_den: null, a_scale: 1, tip_kas: null, counterparty: 'netting', price_source: 'none', confirmations: 1, settled: true });

  it('merges the two views: the other orientation keeps its pair, the volume adds it swapped, newest first, the newer cursor', () => {
    const ab = { base: A, quote: B, volume_24h: { amount_a: '10', amount_b: '20', fills: 1 }, items: [fill(5, 'ask', '10', '20')], next_cursor: '5' };
    const ba = { base: B, quote: A, volume_24h: { amount_a: '6', amount_b: '3', fills: 1 }, items: [fill(7, 'ask', '6', '3')], next_cursor: '7' };
    const m = mergePairFills(ab, ba);
    expect(m.volume_24h).toEqual({ amount_a: '13', amount_b: '26', fills: 2 });
    expect(m.items.map((f) => [f.id, f.base ?? null])).toEqual([[7, B], [5, null]]);
    expect(m.next_cursor).toBe('7');
    expect(mergePairFills(ab, null)).toBe(ab);
    // one record is one id: the other view's copy of it is not taken again
    expect(mergePairFills(ab, { ...ba, items: [fill(5, 'bid', '20', '10')] }).items.map((f) => [f.id, f.base ?? null])).toEqual([[5, null]]);
  });

  it('a row of QUOTE/BASE is turned round: the side swaps and so do the amounts', () => {
    const row = {
      key: 'k', txid: 'aa', kind: 'fill' as const, kinds: ['fill' as const], daa: 1, seq: 1, timeMs: null, timeEstimated: false, confirmations: 1, settled: true, placed: 0,
      side: 'buy' as const, market: { kind: 'pair' as const, base: B, quote: A }, moreMarkets: 0, orders: [],
      amount: { token: B, units: 6n }, value: { kind: 'token' as const, token: A, units: 3n }, price: null, pairSide: null, counterparty: 'netting',
    };
    const r = orientRow(row as never, { kind: 'pair', base: A, quote: B });
    expect([r.side, r.market, r.amount, r.value]).toEqual(['sell', { kind: 'pair', base: A, quote: B }, { token: A, units: 3n }, { kind: 'token', token: B, units: 6n }]);
    expect(orientRow(row as never, { kind: 'pair', base: B, quote: A })).toBe(row);
  });
});

describe('a pair placement shows the quote it asks or offers', () => {
  it('units x price_num / price_den, in the pair order own quote; turned round on the other orientation', () => {
    const A = 'a'.repeat(64);
    const B = 'b'.repeat(64);
    const o = {
      covenant_id: 'c'.repeat(64), contract: 'KobPair', side: 1, token: B, scale: 100, maker: 'd'.repeat(64), initial_amount: '20', amount_left: '20', price: null, status: 'open',
      genesis: { txid: 'e'.repeat(64), out: 0, block_seq: 1, daa: 10, confirmations: 1, settled: true },
      pair: { base: B, quote: A, side: 'ask', price_num: '1', price_den: '4', price: '25', amount_left: '20' },
    };
    const ctx = { tokenScale: () => 100n, order: () => undefined, timeOfDaa: () => null };
    const [row] = placedRows([o as never], ctx as never);
    expect(row!.amount).toEqual({ token: B, units: 20n });
    expect(row!.value).toEqual({ kind: 'token', token: A, units: 5n });
    const turned = orientRow(row!, { kind: 'pair', base: A, quote: B });
    expect([turned.side, turned.amount, turned.value]).toEqual(['buy', { token: A, units: 5n }, { kind: 'token', token: B, units: 20n }]);
  });
});
