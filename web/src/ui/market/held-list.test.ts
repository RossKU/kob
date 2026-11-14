import { describe, expect, it } from 'vitest';
import type { DeclaredToken } from '../../data/token-declarations';
import { parseRegistry, type TokenRegistry } from '../../kob/registry';
import type { TokenState } from '../../kob/types';
import { loadKobNode } from '../../kob/wasm.node';
import { activeFilters, filterRows, heldIds, heldRows, NO_FILTERS, toggleFilter, withHeldRows, type Holding, type ListFilters } from './held-list';
import { heldEntry } from './market-list';
import { buildTokenRows, idOnlyRow, isNameless, tradableRows, unknownRow, type TokenRow } from './token-model';

const kob = loadKobNode();
const ref = kob.templates().find((t) => t.name === 'KCC20Ref')!;
const reg: TokenRegistry = parseRegistry({ schema_version: 1, network: 'testnet-10', templates: [], tokens: [] }, { kob, network: 'testnet-10' });

const EXT = 'ee'.repeat(32);
const ME = '22'.repeat(32);
const A = 'a1'.repeat(32);
const B = 'b2'.repeat(32);
const C = 'c3'.repeat(32);

const state = (o: Partial<{ amount: string; scheme: number; ext: string }> = {}): TokenState =>
  ({ amount: o.amount ?? '500', owner: ME, owner_scheme: o.scheme ?? 0, borrow_scheme: 0, borrow_guard: '00'.repeat(32), extension_commitment: o.ext ?? EXT }) as unknown as TokenState;
const holding = (covenantId: string, o: Partial<{ amount: string; scheme: number; ext: string; program: string }> = {}): Holding =>
  ({ transactionId: '11'.repeat(32), index: 0, amount: '100000000', covenantId, program: (o.program ?? 'KCC20Ref') as Holding['program'], state: state(o) }) as Holding;

const decl = (covenantId: string, symbol: string, decimals: number): DeclaredToken => ({
  covenantId, name: `${symbol} coin`, symbol, decimals, description: null, image: null, imageHash: null, website: null, x: null, telegram: null, preparationTxid: '33'.repeat(32), json: '{}',
});

const listedView = (id: string, ticker: string) => ({ ticker, covenant_id: id, template_hash: ref.hash, extension_commitment: EXT, scale: 100, decimals: 2, open_asks: 1, open_bids: 0 });

describe('heldIds: the mark', () => {
  it('names every token the wallet holds an output of, in lower case; an empty output holds nothing', () => {
    const ids = heldIds([holding(A.toUpperCase()), holding(B, { scheme: 4 }), holding(C, { amount: '0' })]);
    expect([...ids]).toEqual([A, B]);
    expect(heldIds(null).size).toBe(0);
  });
});

describe('heldRows: a row for a held token no list names', () => {
  it('builds the row as the market page does: its program, its commitment, no orders, `held`', () => {
    const rows = heldRows(kob, reg, [holding(A), holding(A)], new Set(), 'testnet-10');
    expect(rows).toHaveLength(1);
    expect(rows[0]).toMatchObject({ covenantId: A, source: 'held', ticker: '', name: null, openAsks: null, openBids: null, templateId: 'kcc20-ref-3x3' });
    expect(rows[0]!.index).toMatchObject({ template_hash: ref.hash, extension_commitment: EXT });
    // the token page's ticket trades exactly these: the list keeps them (`tradableRows`)
    expect(tradableRows(rows, kob.templates())).toHaveLength(1);
  });

  it('names it by its verified declaration: ticker, name and decimals; without one it is nameless', () => {
    const [named] = heldRows(kob, reg, [holding(A)], new Set(), 'testnet-10', (id) => (id === A ? decl(A, 'MINE', 2) : null));
    expect(named).toMatchObject({ ticker: 'MINE', name: 'MINE coin', decimals: 2, declared: true });
    expect(isNameless(named!)).toBe(false);
    const [bare] = heldRows(kob, reg, [holding(A)], new Set(), 'testnet-10', () => undefined);
    expect(isNameless(bare!)).toBe(true);
    // a declaration of another covenant id names nothing
    const [other] = heldRows(kob, reg, [holding(A)], new Set(), 'testnet-10', () => decl(B, 'OTHR', 2));
    expect(other).toMatchObject({ ticker: '', name: null });
  });

  it('dedupes against the listed tokens, and is sorted by covenant id whatever order the outputs came in', () => {
    const rows = heldRows(kob, reg, [holding(C), holding(B), holding(A)], new Set([B]), 'testnet-10');
    expect(rows.map((r) => r.covenantId)).toEqual([A, C]);
  });

  it('leaves out what the page could not trade: no key-owned output, two commitments, two programs, a program not traded on the network', () => {
    expect(heldRows(kob, reg, [holding(A, { scheme: 4 })], new Set(), 'testnet-10')).toEqual([]);
    expect(heldRows(kob, reg, [holding(A), holding(A, { ext: 'ff'.repeat(32) })], new Set(), 'testnet-10')).toEqual([]);
    expect(heldRows(kob, reg, [holding(A), holding(A, { program: 'KCC20PublicMint' })], new Set(), 'testnet-10')).toEqual([]);
    expect(heldRows(kob, reg, [holding(A, { program: 'KCC20Ref_8x8' })], new Set(), 'mainnet')).toEqual([]);
    expect(heldRows(kob, reg, [holding(A, { program: 'KronToken2433' })], new Set(), 'testnet-10')).toEqual([]);
    expect(heldRows(kob, reg, null, new Set(), 'testnet-10')).toEqual([]);
  });

  it('reads "—" in every figure and sorts after the rows with figures', () => {
    const [row] = heldRows(kob, reg, [holding(A)], new Set(), 'testnet-10');
    const e = heldEntry(row!);
    expect(Object.values(e.cells).every((c) => c.text === null)).toBe(true);
    expect(e).toMatchObject({ loading: false, unavailable: false, noOrders: true });
  });
});

describe('withHeldRows: merging', () => {
  it('adds the held rows after the list, once per token, whatever the case of the id', () => {
    const listed = buildTokenRows(reg, [listedView(B, 'BEE')]);
    const held = heldRows(kob, reg, [holding(A), holding(C)], new Set(), 'testnet-10');
    const merged = withHeldRows(listed, [...held, { ...held[0]!, covenantId: A.toUpperCase() }, unknownRow(reg, { ...listedView(B, ''), ticker: '' }, 'held')]);
    expect(merged.map((r) => r.covenantId)).toEqual([B, A, C]);
    expect(merged[0]!.ticker).toBe('BEE');
  });
});

describe('filters', () => {
  const named = (id: string, ticker: string): TokenRow => unknownRow(reg, listedView(id, ticker));
  const nameless = (id: string): TokenRow => unknownRow(reg, { ...listedView(id, ''), decimals: null, scale: null, template_hash: 'ff'.repeat(32) });
  const D = 'd4'.repeat(32);
  const rows = [named(A, 'AAA'), named(B, 'BBB'), nameless(C), named(D, 'DDD')];
  const base = { rows, favoriteRows: [rows[0]!, idOnlyRow('e5'.repeat(32))], hiddenRows: [rows[3]!], hiddenIds: new Set([D]), heldIds: new Set([A, C, D]), query: '' };
  const ids = (f: Partial<ListFilters>, query = '') => filterRows({ ...base, filters: { ...NO_FILTERS, ...f }, query }).map((r) => r.covenantId);

  it('the main list leaves out the nameless and the hidden tokens; a search looks through all', () => {
    expect(ids({})).toEqual([A, B]);
    expect(ids({ nameless: true })).toEqual([A, B, C]);
    expect(ids({}, 'ddd')).toEqual([D]);
  });

  it('Held keeps the wallet\'s tokens of whichever view is shown, and respects "show nameless"', () => {
    expect(ids({ held: true })).toEqual([A]);
    expect(ids({ held: true, nameless: true })).toEqual([A, C]);
    expect(ids({ held: true, favorites: true })).toEqual([A]);
    expect(ids({ held: true, hidden: true })).toEqual([D]);
    expect(ids({ held: true }, 'bbb')).toEqual([]);
  });

  it('Favorites and Hidden list their own rows', () => {
    expect(ids({ favorites: true })).toEqual([A, 'e5'.repeat(32)]);
    expect(ids({ hidden: true })).toEqual([D]);
  });

  it('Favorites and Hidden exclude each other; Held and nameless combine with either; the count is what is on', () => {
    let f = toggleFilter(NO_FILTERS, 'favorites');
    f = toggleFilter(f, 'held');
    expect(f).toEqual({ favorites: true, hidden: false, held: true, nameless: false });
    expect(activeFilters(f)).toBe(2);
    f = toggleFilter(f, 'hidden');
    expect(f).toEqual({ favorites: false, hidden: true, held: true, nameless: false });
    f = toggleFilter(toggleFilter(f, 'nameless'), 'hidden');
    expect(f).toEqual({ favorites: false, hidden: false, held: true, nameless: true });
    expect(activeFilters(NO_FILTERS)).toBe(0);
  });
});
