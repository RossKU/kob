// The activity log's view model: records -> one row per transaction (kind, side, market icons, wallets, amounts, the explorer link per network),
// the filters and their stored form, the hold that keeps rows still under the pointer, and the confirmation clock.
import { describe, expect, it } from 'vitest';
import type { EventView, OrderView, PairFillView } from '../../data/indexer-types';
import {
  AVAILABLE_KINDS, DEFAULT_PREFS, PREFS_KEY, SETTLE_DAA, confirmationsAt, daaNow, filterRows, fillRows, heldView, inMarket, mergeRows, pairFillEvent, parsePrefs,
  placedRows, readPrefs, rowHref, rowMakers, rowTime, shortWallet, shownKinds, writePrefs,
  type ActivityRow, type OrderFacts, type RowContext,
} from './activity-model';

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

  it('while held, rows that would land among or above the shown ones wait and are counted; older ones (Load older) are appended', () => {
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

  it('HH:MM:SS today, MM-DD HH:MM before; the wallet column drops the network prefix', () => {
    const now = new Date(2026, 10, 6, 12, 0, 0).getTime();
    expect(rowTime(new Date(2026, 10, 6, 9, 5, 7).getTime(), now)).toBe('09:05:07');
    expect(rowTime(new Date(2026, 10, 4, 23, 59, 1).getTime(), now)).toBe('11-04 23:59');
    expect(rowTime(null, now)).toBe('--:--:--');
    expect(shortWallet('kaspatest:qz7vabcdefghk3f2')).toBe('qz7v…k3f2');
    expect(shortWallet('qq9d0hxa')).toBe('qq9d0hxa');
  });
});
