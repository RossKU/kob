import { describe, expect, it } from 'vitest';
import type { IndexerTokenView } from '../data/indexer-types';
import { displayName } from './registry';
import { standardScaleOf, synthesizeOpenToken } from './open-token';
import { toTokenMarket } from './token-market';
import { loadKobNode } from './wasm.node';

const kob = loadKobNode();
const kcc = kob.templates().find((t) => t.name === 'KCC20Ref_8x8')!;
const kron = kob.templates().find((t) => t.name === 'KronToken2433')!;
const ID = 'ab'.repeat(32);
const EXT = 'ee'.repeat(32);

const view = (over: Partial<IndexerTokenView> = {}): IndexerTokenView => ({
  ticker: '', covenant_id: ID, template_hash: kcc.hash, extension_commitment: EXT, scale: null, decimals: null, open_asks: 1, open_bids: 0, standing: 'unverified', powers: [], ...over,
});
const scales = [{ scale: 1000 }, { scale: 100 }, { scale: 1000 }];

describe('standardScaleOf', () => {
  it('is the order scale shared by most orders; ties take the smaller scale', () => {
    expect(standardScaleOf(scales)).toBe(1000);
    expect(standardScaleOf([{ scale: 100 }, { scale: 10 }])).toBe(10);
    expect(standardScaleOf([{ scale: 100 }, { scale: 10 }, { scale: 100 }])).toBe(100);
  });
  it('ignores rows without a scale or with one that is not a power of ten up to 10^9; no orders, no scale', () => {
    expect(standardScaleOf([])).toBeNull();
    expect(standardScaleOf([{ scale: null }, { scale: 0 }, { scale: 500 }, { scale: 10_000_000_000 }, {}])).toBeNull();
    expect(standardScaleOf([{ scale: 1 }])).toBe(1);
  });
});

describe('synthesizeOpenToken', () => {
  it('builds a tradable, unverified token from the indexer row and its orders: the decimals are the exponent of the orders\' scale', () => {
    const r = synthesizeOpenToken(kob, view({ powers: ['burn', 'freeze', 'weird'] }), scales);
    expect(r.ok).toBe(true);
    if (!r.ok) return;
    const t = r.info;
    expect(t).toMatchObject({
      covenantId: ID, program: 'KCC20Ref_8x8', family: 'kcc20', templateHash: kcc.hash, extensionCommitment: EXT, decimals: 3, tick: 1n,
      tradable: true, verified: false, official: false, openList: true, capabilities: ['burn', 'freeze'], slots: { inputs: 8, outputs: 8 },
    });
    expect(t.ticker).toBe('abababab…abababab');
    expect(displayName(t)).toBe('abababab…abababab [unverified]');
    // the planners accept it like a registry token, and quote at the book's scale
    const m = toTokenMarket(kob, t.json);
    expect(m).toMatchObject({ covenantId: ID, program: 'KCC20Ref_8x8', scale: 1000n, tick: 1n, decimals: 3, extensionCommitment: EXT });
  });

  it('a KCC-20 token without an extension commitment takes it from an order, else it cannot be traded', () => {
    expect(synthesizeOpenToken(kob, view({ extension_commitment: null }), scales)).toEqual({ ok: false, reason: 'no-extension' });
    const r = synthesizeOpenToken(kob, view({ extension_commitment: null }), scales, 'cd'.repeat(32));
    expect(r.ok && r.info.extensionCommitment).toBe('cd'.repeat(32));
  });

  it('no orders: no scale yet', () => {
    expect(synthesizeOpenToken(kob, view(), [])).toEqual({ ok: false, reason: 'no-scale' });
  });

  it('a template hash this build does not embed is "program unknown", also without a hash', () => {
    expect(synthesizeOpenToken(kob, view({ template_hash: 'ff'.repeat(32) }), scales)).toEqual({ ok: false, reason: 'program-unknown' });
    expect(synthesizeOpenToken(kob, view({ template_hash: null }), scales)).toEqual({ ok: false, reason: 'program-unknown' });
  });

  it('a KRON token needs no extension commitment; scale 1 is a token of 0 decimals', () => {
    const k = synthesizeOpenToken(kob, view({ template_hash: kron.hash, extension_commitment: null }), [{ scale: 1 }]);
    expect(k.ok && k.info).toMatchObject({ family: 'kron', program: 'KronToken2433', extensionCommitment: null, decimals: 0 });
    expect(k.ok && toTokenMarket(kob, k.info.json).scale).toBe(1n);
  });
});
