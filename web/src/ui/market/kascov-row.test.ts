// kascovRowOf (kascov-merge.ts): the row of ONE token only kascov.io names, for its own token page. It is the row the list shows for it.
import { describe, expect, it } from 'vitest';
import type { KascovToken } from '../../data/kascov';
import type { TokenRegistry } from '../../kob/registry';
import { loadKobNode } from '../../kob/wasm.node';
import { kascovRowOf, mergeKascov } from './kascov-merge';

const kob = loadKobNode();
const templates = kob.templates();
const hashOf = (name: string): string => templates.find((x) => x.name === name)!.hash;
const ROCKET = hashOf('KaspaRocketToken11458');
const REF_8X8 = hashOf('KCC20Ref_8x8');
const FORGE = hashOf('KForgeToken1785');
const ID = (c: string) => c.repeat(64);
const registry: TokenRegistry = { network: 'testnet-10', templates: [], templateChecks: [], tokens: [], byCovenantId: new Map() };
const k = (over: Partial<KascovToken> = {}): KascovToken => ({
  covenantId: ID('1'), ticker: 'ROCKET', name: 'Rocket Token', decimals: 3, templateHash: ROCKET, imageHash: 'ab'.repeat(32), imageUrl: 'https://img.example/a.png', verified: false, ...over,
});
const ctx = (network: string) => ({ network, templates, registry });

describe('kascovRowOf', () => {
  it('is the row the list shows: no orders, named by the creator, of the pinned program', () => {
    const row = kascovRowOf(ID('1'), [k()], ctx('testnet-10'));
    expect(row).toMatchObject({ covenantId: ID('1'), source: 'kascov', ticker: 'ROCKET', name: 'Rocket Token', openAsks: null, openBids: null, tradable: false, decimals: 3 });
    expect(row?.kascov).toMatchObject({ usedTicker: true, usedName: true });
    expect(row?.index?.template_hash).toBe(ROCKET);
    expect(row).toEqual(mergeKascov([], [k()], ctx('testnet-10'))[0]);
    // the covenant id of a page may be typed in capitals
    expect(kascovRowOf(ID('1').toUpperCase(), [k()], ctx('testnet-10'))?.covenantId).toBe(ID('1'));
  });

  it('nothing for a token kascov does not name, names no program of, or names one this build does not trade on the network', () => {
    expect(kascovRowOf(ID('2'), [k()], ctx('testnet-10'))).toBeNull();
    expect(kascovRowOf(ID('1'), null, ctx('testnet-10'))).toBeNull();
    expect(kascovRowOf(ID('1'), [k({ templateHash: null })], ctx('testnet-10'))).toBeNull();
    expect(kascovRowOf(ID('1'), [k({ templateHash: 'ff'.repeat(32) })], ctx('testnet-10'))).toBeNull();
    expect(kascovRowOf(ID('1'), [k({ templateHash: REF_8X8 })], ctx('mainnet'))).toBeNull();
    expect(kascovRowOf(ID('1'), [k({ templateHash: FORGE })], ctx('mainnet'))).toBeNull();
  });
});
