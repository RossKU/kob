import { describe, expect, it } from 'vitest';
import type { IndexerTokenView } from '../../data/indexer-types';
import { parseKascovToken, type KascovToken } from '../../data/kascov';
import type { TokenInfo, TokenRegistry } from '../../kob/registry';
import { loadKobNode } from '../../kob/wasm.node';
import { applyKascov, kascovTradable, mergeKascov } from './kascov-merge';
import { buildTokenRows, filterTokens, tradableRows, type TokenRow } from './token-model';

const kob = loadKobNode();
const templates = kob.templates();
const hashOf = (name: string): string => templates.find((t) => t.name === name)!.hash;
const PUBLIC_MINT = hashOf('KCC20PublicMint');
const ROCKET = hashOf('KaspaRocketToken11458');
const FORGE = hashOf('KForgeToken1785');
const REF_8X8 = hashOf('KCC20Ref_8x8');
const REF = hashOf('KCC20Ref');
const ID = (c: string) => c.repeat(64);

const registry: TokenRegistry = { network: 'testnet-10', templates: [], templateChecks: [], tokens: [], byCovenantId: new Map() };

const k = (over: Partial<KascovToken> = {}): KascovToken => ({
  covenantId: ID('1'), ticker: 'ROCKET', name: 'Rocket Token', decimals: 3, templateHash: ROCKET, imageHash: 'ab'.repeat(32), imageUrl: 'https://img.example/a.png', verified: false, ...over,
});
const ctx = (network: string, over: Record<string, unknown> = {}) => ({ network, templates, registry, ...over });

const view = (id: string, over: Partial<IndexerTokenView> = {}): IndexerTokenView => ({
  ticker: '', covenant_id: id, template_hash: REF_8X8, extension_commitment: 'ee'.repeat(32), decimals: null, open_asks: 2, open_bids: 1, template_id: 'kcc20-ref-8x8', ...over,
});
/** a registry token row (the registry itself is empty here: `buildTokenRows` over the example registry is covered elsewhere) */
function registryRow(ticker: string, id: string, over: Partial<TokenRow> = {}): TokenRow {
  const base = buildTokenRows(registry, [view(id)])[0]!;
  return { ...base, ticker, name: `${ticker} coin`, source: 'registry', info: { covenantId: id, ticker, templateHash: REF_8X8 } as unknown as TokenInfo, decimals: 8, ...over };
}

describe('kascovTradable: the program hash decides, on this network', () => {
  it('a pinned program enabled on the network', () => {
    expect(kascovTradable(PUBLIC_MINT, 'mainnet', templates)).toBe(true);
    expect(kascovTradable(PUBLIC_MINT, 'testnet-10', templates)).toBe(true);
    expect(kascovTradable(REF, 'mainnet', templates)).toBe(true);
    // pending review: not on mainnet, every embedded program on a test network
    expect(kascovTradable(REF_8X8, 'mainnet', templates)).toBe(false);
    expect(kascovTradable(REF_8X8, 'testnet-10', templates)).toBe(true);
  });

  it('a program of the profile table that is not on this network: KForge is testnet-10 only, KaspaRocket mainnet and testnet-10', () => {
    expect(kascovTradable(ROCKET, 'testnet-10', templates)).toBe(true);
    expect(kascovTradable(ROCKET, 'mainnet', templates)).toBe(true);
    expect(kascovTradable(FORGE, 'testnet-10', templates)).toBe(true);
    expect(kascovTradable(FORGE, 'mainnet', templates)).toBe(false);
    expect(kascovTradable(ROCKET, 'testnet-11', templates)).toBe(false);
  });

  it('an unknown hash, none, or the hash of an order template is not a token program', () => {
    expect(kascovTradable('ff'.repeat(32), 'testnet-10', templates)).toBe(false);
    expect(kascovTradable(null, 'testnet-10', templates)).toBe(false);
    expect(kascovTradable(undefined, 'testnet-10', templates)).toBe(false);
    expect(kascovTradable('', 'testnet-10', templates)).toBe(false);
    const order = templates.find((t) => !t.tokenSlots)!;
    expect(kascovTradable(order.hash, 'testnet-10', templates)).toBe(false);
    // a hash that is only a prefix or in capitals of a pinned one is not that program
    expect(kascovTradable(PUBLIC_MINT.slice(0, 32), 'mainnet', templates)).toBe(false);
  });

  it('kascov\'s own verdicts are never read: a token it calls invalid or rejected is judged by its program like any other', () => {
    const t = parseKascovToken({ covenant_id: ID('1'), status: 'invalid', alive: false, invalid_reason: 'x', standard: { review: 'rejected', view_template_hash: PUBLIC_MINT } })!;
    expect(kascovTradable(t.templateHash, 'mainnet', templates)).toBe(true);
  });
});

describe('mergeKascov: nothing from kascov (down, empty, off) leaves the rows as they are', () => {
  const rows = [registryRow('EXKCC', ID('a'))];
  it('null, undefined and an empty list', () => {
    for (const none of [null, undefined, []]) expect(mergeKascov(rows, none, ctx('testnet-10'))).toEqual(rows);
  });
});

describe('mergeKascov: tokens kascov adds', () => {
  it('a token of an enabled program that no KOB source knows becomes a "no orders" row named by kascov, with unknown order counts and nothing tradable-by-registry', () => {
    const out = mergeKascov([], [k()], ctx('testnet-10'));
    expect(out).toHaveLength(1);
    const r = out[0]!;
    expect(r).toMatchObject({
      covenantId: ID('1'), ticker: 'ROCKET', name: 'Rocket Token', source: 'kascov', info: null, openAsks: null, openBids: null, decimals: 3, scale: 1000n, reason: 'unregistered',
      templateId: 'kasparocket-token-11458',
    });
    expect(r.kascov).toMatchObject({ usedTicker: true, usedName: true, tickerDiffers: false, decimalsDiffers: false, decimals: 3, imageHash: 'ab'.repeat(32), imageUrl: 'https://img.example/a.png' });
    // its decimals are the program's default (the KaspaRocket convention), which kascov's claim agrees with
    // the facts a trader needs are the ones of any token KOB has not verified
    expect(r.facts).toContain('genesis_unverified');
  });

  it('not on mainnet when the program is testnet-10 only, not without a program hash, not when the program is not pinned', () => {
    expect(mergeKascov([], [k({ templateHash: FORGE })], ctx('mainnet'))).toEqual([]);
    expect(mergeKascov([], [k()], ctx('mainnet'))).toHaveLength(1);
    expect(mergeKascov([], [k({ templateHash: null })], ctx('testnet-10'))).toEqual([]);
    expect(mergeKascov([], [k({ templateHash: 'ff'.repeat(32) })], ctx('testnet-10'))).toEqual([]);
    expect(mergeKascov([], [k({ templateHash: PUBLIC_MINT })], ctx('mainnet'))).toHaveLength(1);
  });

  it('a token KOB already has (a row, or an id its sources know but the list does not show) is never added again', () => {
    const rows = [registryRow('EXKCC', ID('1'))];
    expect(mergeKascov(rows, [k()], ctx('testnet-10')).map((r) => r.source)).toEqual(['registry']);
    expect(mergeKascov([], [k()], ctx('testnet-10', { known: new Set([ID('1')]) }))).toEqual([]);
  });

  it('they follow the KOB rows, by ticker then id; the KOB rows keep their order', () => {
    const rows = [registryRow('ZED', ID('f')), registryRow('ABC', ID('e'))];
    const out = mergeKascov(rows, [k({ covenantId: ID('3'), ticker: 'beta' }), k({ covenantId: ID('2'), ticker: 'Alpha' }), k({ covenantId: ID('4'), ticker: null })], ctx('testnet-10'));
    expect(out.map((r) => r.covenantId)).toEqual([ID('f'), ID('e'), ID('4'), ID('2'), ID('3')]);
  });

  it('a verified KCC-23 declaration that is already known names the row before kascov does', () => {
    const declared = (id: string) => (id === ID('1') ? { covenantId: ID('1'), symbol: 'CHAIN', name: 'Chain Name', decimals: 6 } : null);
    const r = mergeKascov([], [k({ ticker: 'KASCOV', name: 'Kascov Name', decimals: 3 })], ctx('testnet-10', { declared }))[0]!;
    expect(r).toMatchObject({ ticker: 'CHAIN', name: 'Chain Name', decimals: 6, declared: true });
    expect(r.kascov).toMatchObject({ usedTicker: false, usedName: false, tickerDiffers: true, decimalsDiffers: true });
  });

  it('the kascov rows are searchable by the kascov ticker and name, and a tradableRows pass keeps them', () => {
    const out = mergeKascov([], [k()], ctx('testnet-10'));
    expect(filterTokens(out, 'rocket')).toHaveLength(1);
    expect(filterTokens(out, 'token')).toHaveLength(1);
    expect(tradableRows(out, templates)).toHaveLength(1);
  });
});

describe('mergeKascov: words and icons laid under KOB\'s own', () => {
  it('KOB\'s ticker and name win; a different kascov ticker or decimals are noted, the same ones are not', () => {
    const row = registryRow('EXKCC', ID('1'));
    const same = mergeKascov([row], [k({ templateHash: null, ticker: 'exkcc', name: 'Other name', decimals: 8 })], ctx('testnet-10'))[0]!;
    expect(same).toMatchObject({ ticker: 'EXKCC', name: 'EXKCC coin', decimals: 8 });
    expect(same.kascov).toMatchObject({ usedTicker: false, usedName: false, tickerDiffers: false, decimalsDiffers: false });
    const diff = mergeKascov([row], [k({ templateHash: null, ticker: 'FAKE', decimals: 2 })], ctx('testnet-10'))[0]!;
    expect(diff).toMatchObject({ ticker: 'EXKCC', name: 'EXKCC coin', decimals: 8 });
    expect(diff.kascov).toMatchObject({ ticker: 'FAKE', usedTicker: false, tickerDiffers: true, decimalsDiffers: true, decimals: 2 });
  });

  it('a token the KOB indexer lists without a ticker takes kascov\'s, and says so; a ticker only, a name only', () => {
    const row = buildTokenRows(registry, [view(ID('1'))])[0]!;
    expect(row.ticker).toBe('');
    const both = mergeKascov([row], [k({ templateHash: REF_8X8 })], ctx('testnet-10'))[0]!;
    expect(both).toMatchObject({ ticker: 'ROCKET', name: 'Rocket Token', source: 'indexer' });
    expect(both.kascov).toMatchObject({ usedTicker: true, usedName: true });
    const tickerOnly = mergeKascov([row], [k({ templateHash: REF_8X8, name: null })], ctx('testnet-10'))[0]!;
    expect(tickerOnly.kascov).toMatchObject({ usedTicker: true, usedName: false });
    expect(tickerOnly.name).toBeNull();
    // the indexer's order counts are untouched
    expect(both).toMatchObject({ openAsks: 2, openBids: 1 });
  });

  it('a row KOB knows the program of, which kascov contradicts, is left alone: kascov is describing another program', () => {
    const row = buildTokenRows(registry, [view(ID('1'))])[0]!;
    expect(mergeKascov([row], [k({ templateHash: PUBLIC_MINT })], ctx('testnet-10'))[0]).toBe(row);
    // no program named by kascov (the 46-byte KRON family) cannot contradict
    expect(mergeKascov([row], [k({ templateHash: null })], ctx('testnet-10'))[0]!.kascov).toBeTruthy();
  });

  it('a row kascov does not list is returned as it was (the same object), so the list still shows every KOB token', () => {
    const row = registryRow('EXKCC', ID('a'));
    expect(mergeKascov([row], [k()], ctx('testnet-10'))[0]).toBe(row);
  });

  it('ids compare case-insensitively', () => {
    const row = registryRow('EXKCC', ID('a'));
    expect(mergeKascov([{ ...row, covenantId: ID('a').toUpperCase() }], [k({ covenantId: ID('a'), templateHash: null })], ctx('testnet-10'))[0]!.kascov).toBeTruthy();
  });
});

describe('applyKascov (a token page)', () => {
  it('takes kascov\'s ticker only where the row\'s is the stand-in for none', () => {
    const standIn = `${ID('1').slice(0, 8)}…${ID('1').slice(-8)}`;
    const row = { ...buildTokenRows(registry, [view(ID('1'))])[0]!, ticker: standIn };
    const r = applyKascov(row, [k({ templateHash: REF_8X8 })], standIn);
    expect(r).toMatchObject({ ticker: 'ROCKET', name: 'Rocket Token' });
    expect(r.kascov).toMatchObject({ usedTicker: true, tickerDiffers: false });
    // a real ticker stays
    const named = applyKascov({ ...row, ticker: 'KOBS' }, [k({ templateHash: REF_8X8 })], standIn);
    expect(named.ticker).toBe('KOBS');
    expect(named.kascov).toMatchObject({ usedTicker: false, tickerDiffers: true });
    expect(applyKascov(row, [], standIn)).toBe(row);
    expect(applyKascov(row, null, standIn)).toBe(row);
  });
});

describe('kascov\'s remark: a token it marks verified shows the chip "Verified by kascov" in place of "Genesis not verified" (third party, no fact)', () => {
  const row = (over: Partial<TokenRow> = {}): TokenRow => ({ ...buildTokenRows(registry, [view(ID('1'))])[0]!, ...over });

  it('swaps the unverified line for the chip, in a list and on a page, and only for a token kascov marks verified', () => {
    expect(row().facts).toEqual(['genesis_unverified']);
    const [chip] = mergeKascov([row()], [k({ templateHash: REF_8X8, verified: true })], ctx('testnet-10'));
    expect(chip!.facts).toEqual(['genesis_kascov']);
    expect(chip!.kascov).toMatchObject({ verified: true });
    expect(applyKascov(row(), [k({ templateHash: REF_8X8, verified: true })]).facts).toEqual(['genesis_kascov']);
    // not verified (any other status), or kascov names another program: untouched
    expect(mergeKascov([row()], [k({ templateHash: REF_8X8, verified: false })], ctx('testnet-10'))[0]!.facts).toEqual(['genesis_unverified']);
    expect(mergeKascov([row()], [k({ templateHash: ROCKET, verified: true })], ctx('testnet-10'))[0]!.facts).toEqual(['genesis_unverified']);
    // a kascov-only row it adds (its program is pinned) carries the chip too; the other facts stay
    const added = mergeKascov([], [k({ covenantId: ID('2'), templateHash: PUBLIC_MINT, verified: true })], ctx('testnet-10'));
    expect(added[0]!.facts).toEqual(['genesis_kascov']);
    const withMint = mergeKascov([row({ facts: ['genesis_unverified', 'live_mint_authority', 'public-mint'] })], [k({ templateHash: REF_8X8, verified: true })], ctx('testnet-10'));
    expect(withMint[0]!.facts).toEqual(['genesis_kascov', 'live_mint_authority', 'public-mint']);
  });

  it('a token whose genesis the registry verified has no genesis line to replace', () => {
    const verified = row({ facts: [] });
    expect(mergeKascov([verified], [k({ templateHash: REF_8X8, verified: true })], ctx('testnet-10'))[0]!.facts).toEqual([]);
  });

  it('is never a warning: the indexer\'s warnings are untouched and an indexer cannot state the chip', () => {
    const r = buildTokenRows(registry, [view(ID('1'), { warnings: ['genesis_kascov', 'genesis_verified', 'genesis_failed', 'mint_allowance', 'genesis_unverified'] })])[0]!;
    expect(r.facts).toEqual(['genesis_unverified']);
  });
});
