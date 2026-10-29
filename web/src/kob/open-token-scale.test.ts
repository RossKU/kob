// Audit 2026-10-29 F3 on the web side: the scale and the program of an open-list token. The executor now lists only orders at the scale the token's
// decimals fix (a declaration, a program convention), serves no `market_scale` beside resolved decimals and names no program when placements claim
// several; the web keeps its own refusals for an older or hostile executor.
import { describe, expect, it } from 'vitest';
import type { IndexerTokenView } from '../data/indexer-types';
import { synthesizeOpenToken } from './open-token';
import { loadKobNode } from './wasm.node';

const kob = loadKobNode();
const ID = 'ab'.repeat(32);
const EXT = 'ee'.repeat(32);
const tpl = (name: string) => kob.templates().find((t) => t.name === name)!;
const view = (name: string, over: Partial<IndexerTokenView> = {}): IndexerTokenView => ({
  ticker: '', covenant_id: ID, template_hash: tpl(name).hash, extension_commitment: EXT, scale: null, decimals: null, open_asks: 1, open_bids: 0, powers: [], ...over,
});

describe('the scale of an open-list token whose decimals are fixed', () => {
  it('KaspaRocket program (convention 3), orders at the convention: tradable at scale 1000, no market_scale needed', () => {
    const r = synthesizeOpenToken(kob, view('KaspaRocketToken11458', { decimals: 3 }), [{ scale: 1000 }], null, null, [], { network: 'testnet-10' });
    expect(r.ok && r.info.openScale).toBe('program');
    expect(r.ok && r.info.decimals).toBe(3);
  });

  it('an executor that still serves a first order at another scale beside the program decimals: refused as a scale conflict (never re-denominated)', () => {
    for (const s of [1, 10, 100, 10_000, 100_000_000]) {
      expect(synthesizeOpenToken(kob, view('KaspaRocketToken11458', { market_scale: s, decimals: 3 }), [{ scale: s }], null, null, [], { network: 'testnet-10' })).toEqual({ ok: false, reason: 'scale-conflict' });
    }
  });

  it('declared decimals 8 (KCC-23) against a scale-1 market: a scale conflict', () => {
    expect(
      synthesizeOpenToken(kob, view('KCC20PublicMint', { market_scale: 1 }), [{ scale: 1 }], null, null, [], { network: 'mainnet', declared: { decimals: 8, symbol: 'DCL', name: 'Declared' } }),
    ).toEqual({ ok: false, reason: 'scale-conflict' });
  });

  it('declared decimals 8 and orders at 10^8: tradable', () => {
    const r = synthesizeOpenToken(kob, view('KCC20PublicMint'), [{ scale: 100_000_000 }], null, null, [], { network: 'mainnet', declared: { decimals: 8, symbol: 'DCL', name: 'Declared' } });
    expect(r.ok && r.info.openScale).toBe('declared');
    expect(r.ok && r.info.decimals).toBe(8);
  });

  it('a token whose decimals nothing fixes takes the scale of its first order (public-mint program, no declaration)', () => {
    const r = synthesizeOpenToken(kob, view('KCC20PublicMint', { market_scale: 1_000_000 }), [{ scale: 1_000_000 }], null, null, [], { network: 'mainnet' });
    expect(r.ok && r.info.decimals).toBe(6);
    expect(r.ok && r.info.openScale).toBe('orders');
  });
});

describe('a token whose placements name several programs', () => {
  it('is not traded: the executor names none (program_ambiguous), and so does the web', () => {
    const ambiguous = view('KCC20PublicMint', { template_hash: null, extension_commitment: null, program_ambiguous: true, candidate_templates: [tpl('KCC20PublicMint').hash, tpl('KaspaRocketToken11458').hash] });
    expect(synthesizeOpenToken(kob, ambiguous, [{ scale: 1000 }], null, null, [], { network: 'testnet-10' })).toEqual({ ok: false, reason: 'program-ambiguous' });
    // even with a template hash in the row (a hostile executor), the flag decides
    expect(synthesizeOpenToken(kob, { ...ambiguous, template_hash: tpl('KCC20PublicMint').hash }, [{ scale: 1000 }], null, null, [], { network: 'testnet-10' })).toEqual({ ok: false, reason: 'program-ambiguous' });
  });
});
