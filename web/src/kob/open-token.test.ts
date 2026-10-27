import { describe, expect, it } from 'vitest';
import type { IndexerTokenView } from '../data/indexer-types';
import { displayName } from './registry';
import { checkExtension, extensionReferences, findHeldToken, findPublicToken, heldTokenView, marketScaleOf, parseOpenDecimals, programAllowed, standardScaleOf, synthesizeOpenToken } from './open-token';
import { toTokenMarket } from './token-market';
import { loadKobNode } from './wasm.node';

const kob = loadKobNode();
const kcc = kob.templates().find((t) => t.name === 'KCC20Ref_8x8')!;
const kron = kob.templates().find((t) => t.name === 'KronToken2433')!;
const ID = 'ab'.repeat(32);
const EXT = 'ee'.repeat(32);

const view = (over: Partial<IndexerTokenView> = {}): IndexerTokenView => ({
  ticker: '', covenant_id: ID, template_hash: kcc.hash, extension_commitment: EXT, scale: null, decimals: null, open_asks: 1, open_bids: 0, powers: [], ...over,
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
  it('builds a tradable token from the indexer row and its orders: the decimals are the exponent of the orders\' scale', () => {
    const r = synthesizeOpenToken(kob, view({ powers: ['burn', 'freeze', 'weird'] }), scales);
    expect(r.ok).toBe(true);
    if (!r.ok) return;
    const t = r.info;
    expect(t).toMatchObject({
      covenantId: ID, program: 'KCC20Ref_8x8', family: 'kcc20', templateHash: kcc.hash, extensionCommitment: EXT, decimals: 3, tick: 1n,
      tradable: true, openList: true, capabilities: ['burn', 'freeze'], slots: { inputs: 8, outputs: 8 },
    });
    expect(t.ticker).toBe('abababab…abababab');
    expect(displayName(t)).toBe('abababab…abababab');
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

describe('A1-2: the extension commitment of an open-list KCC-20 token is kept with its source and cross-checked', () => {
  const OTHER = 'cd'.repeat(32);
  const held = (ext: string, cov = ID) =>
    ({ transactionId: '11'.repeat(32), index: 0, amount: '100000000', covenantId: cov, state: { amount: '5', owner: '22'.repeat(32), owner_scheme: 0, borrow_scheme: 0, borrow_guard: '00'.repeat(32), extension_commitment: ext } }) as never;
  const reg = (ext: string, verified: boolean) =>
    ({ tokens: [{ covenantId: ID, family: 'kcc20', extensionCommitment: ext, json: { genesis_verified: verified } }], byCovenantId: new Map() }) as never;

  it('records where the commitment came from: the indexer row, else the listed orders', () => {
    const a = synthesizeOpenToken(kob, view(), scales);
    expect(a.ok && a.info.extension).toEqual({ commitment: EXT, source: 'indexer', references: [], mismatch: [] });
    const b = synthesizeOpenToken(kob, view({ extension_commitment: null }), scales, OTHER);
    expect(b.ok && b.info.extension).toEqual({ commitment: OTHER, source: 'orders', references: [], mismatch: [] });
    // KRON has no commitment: nothing to check
    const k = synthesizeOpenToken(kob, view({ template_hash: kron.hash, extension_commitment: null }), scales);
    expect(k.ok && k.info.extension).toBeUndefined();
  });

  it('references: the genesis-verified registry entry of the id and the distinct commitments of node-verified holdings of the id', () => {
    expect(extensionReferences(ID, reg(EXT, true), [held(EXT), held(EXT), held(OTHER, 'ff'.repeat(32))])).toEqual([
      { commitment: EXT, source: 'registry-genesis' },
      { commitment: EXT, source: 'holdings' },
    ]);
    // a registry entry whose genesis was not verified proves nothing
    expect(extensionReferences(ID, reg(EXT, false), [])).toEqual([]);
    expect(extensionReferences(ID, null, [held(OTHER), held(EXT)]).map((r) => r.commitment)).toEqual([OTHER, EXT].sort());
  });

  it('a pick that differs from a reference is a mismatch; a matching one is not', () => {
    const ok = synthesizeOpenToken(kob, view(), scales, null, null, extensionReferences(ID, reg(EXT, true), [held(EXT)]));
    expect(ok.ok && ok.info.extension?.mismatch).toEqual([]);
    const bad = synthesizeOpenToken(kob, view({ extension_commitment: OTHER }), scales, null, null, extensionReferences(ID, reg(EXT, true), [held(EXT)]));
    expect(bad.ok && bad.info.extension?.mismatch).toEqual([
      { commitment: EXT, source: 'registry-genesis' },
      { commitment: EXT, source: 'holdings' },
    ]);
    expect(checkExtension(EXT.toUpperCase(), 'indexer', [{ commitment: EXT, source: 'holdings' }]).mismatch).toEqual([]);
  });
});

describe('the first order of an open-list token: decimals entered by the user, never assumed', () => {
  it('no orders: the entered decimals (0..=9) give the scale, marked as entered; nothing entered is still no-scale', () => {
    const r = synthesizeOpenToken(kob, view(), [], null, null, [], { decimals: 6 });
    expect(r.ok && r.info).toMatchObject({ decimals: 6, openScale: 'entered', openList: true });
    expect(r.ok && toTokenMarket(kob, r.info.json).scale).toBe(1_000_000n);
    expect(synthesizeOpenToken(kob, view(), [], null, null, [], { decimals: null })).toEqual({ ok: false, reason: 'no-scale' });
    for (const bad of [10, -1, 2.5, Number.NaN]) expect(synthesizeOpenToken(kob, view(), [], null, null, [], { decimals: bad })).toEqual({ ok: false, reason: 'no-scale' });
    const zero = synthesizeOpenToken(kob, view(), [], null, null, [], { decimals: 0 });
    expect(zero.ok && zero.info.decimals).toBe(0);
  });

  it('once an order exists its scale decides: entered decimals of the same scale stand, others are refused, never silently replaced', () => {
    const r = synthesizeOpenToken(kob, view(), scales, null, null, [], { decimals: 3 });
    expect(r.ok && r.info).toMatchObject({ decimals: 3, openScale: 'orders' });
    expect(synthesizeOpenToken(kob, view(), scales, null, null, [], { decimals: 6 })).toEqual({ ok: false, reason: 'scale-conflict' });
    expect(synthesizeOpenToken(kob, view(), scales).ok).toBe(true);
  });

  // TN10 2026-10-15 O-3: every earlier order of the token had ended, the book was empty, and the page offered a "first order" with entered
  // decimals again; other decimals would have put orders of two price scales on one market
  it('the market keeps the scale its first order set after every order of it has ended (`market_scale`)', () => {
    const ended = view({ open_asks: 0, market_scale: 1_000_000 });
    const r = synthesizeOpenToken(kob, ended, []);
    expect(r.ok && r.info).toMatchObject({ decimals: 6, openScale: 'orders' });
    expect(r.ok && toTokenMarket(kob, r.info.json).scale).toBe(1_000_000n);
    expect(synthesizeOpenToken(kob, ended, [], null, null, [], { decimals: 2 })).toEqual({ ok: false, reason: 'scale-conflict' });
    expect(synthesizeOpenToken(kob, ended, [], null, null, [], { decimals: 6 }).ok).toBe(true);
    // it wins over the open orders' scale (orders of another scale placed before the market's scale was kept)
    expect(synthesizeOpenToken(kob, view({ market_scale: 100 }), scales)).toMatchObject({ ok: true, info: { decimals: 2 } });
    // no order ever (null), or an older indexer (absent): the open orders, else the entered decimals
    for (const v of [view({ market_scale: null }), view()]) {
      expect(synthesizeOpenToken(kob, v, scales)).toMatchObject({ ok: true, info: { decimals: 3 } });
      expect(synthesizeOpenToken(kob, v, [], null, null, [], { decimals: 2 })).toMatchObject({ ok: true, info: { decimals: 2, openScale: 'entered' } });
    }
    // a scale no order can have
    for (const bad of [500, 10_000_000_000, '1000' as never]) expect(synthesizeOpenToken(kob, view({ market_scale: bad }), [])).toEqual({ ok: false, reason: 'scale-invalid' });
    expect(marketScaleOf(view({ market_scale: 1000 }), [{ scale: 10 }])).toBe(1000);
    expect(marketScaleOf(view(), [{ scale: 10 }])).toBe(10);
  });

  it('parseOpenDecimals: one digit 0..9, else null', () => {
    expect(['0', ' 9 ', '6'].map(parseOpenDecimals)).toEqual([0, 9, 6]);
    expect(['', '10', '-1', '1.5', 'x', '٣'].map(parseOpenDecimals)).toEqual([null, null, null, null, null, null]);
  });
});

describe('program review: prototypes are not traded on mainnet without a registry entry', () => {
  const tpl = (name: string) => kob.templates().find((t) => t.name === name)!;
  it('mainnet: only reviewed programs; a test network: every embedded program', () => {
    expect(programAllowed('KCC20Ref', 'mainnet')).toBe(true);
    expect(programAllowed('KCC20PublicMint', 'mainnet')).toBe(true);
    expect(programAllowed('KCC20Ref_8x8', 'mainnet')).toBe(false);
    expect(programAllowed('KCC20Ref_8x8', 'testnet-10')).toBe(true);
    expect(programAllowed('NoSuchProgram', 'testnet-10')).toBe(false);
    // a program whose profile lists its networks is allowed on those only (KaspaRocket: mainnet and testnet-10; KForge: testnet-10)
    expect(programAllowed('KaspaRocketToken11458', 'testnet-10')).toBe(true);
    expect(programAllowed('KaspaRocketToken11458', 'mainnet')).toBe(true);
    expect(programAllowed('KForgeToken1785', 'testnet-10')).toBe(true);
    expect(programAllowed('KForgeToken1785', 'mainnet')).toBe(false);
    expect(programAllowed('KaspaRocketToken11458', 'testnet-11')).toBe(false);
    expect(synthesizeOpenToken(kob, view(), scales, null, null, [], { network: 'mainnet' })).toEqual({ ok: false, reason: 'program-unreviewed' });
    const ok = synthesizeOpenToken(kob, view({ template_hash: tpl('KCC20Ref').hash }), scales, null, null, [], { network: 'mainnet' });
    expect(ok.ok && ok.info.program).toBe('KCC20Ref');
  });
});

describe('heldTokenView: a covenant id the indexer does not list, from the wallet\'s node-verified holdings', () => {
  const ref = kob.templates().find((t) => t.name === 'KCC20Ref')!;
  const utxo = (ext: string, cov = ID) =>
    ({ transactionId: '11'.repeat(32), index: 0, amount: '100000000', covenantId: cov, state: { amount: '5', owner: '22'.repeat(32), owner_scheme: 0, borrow_scheme: 0, borrow_guard: '00'.repeat(32), extension_commitment: ext } }) as never;

  it('one program, one commitment: a row of the program, the commitment, no scale; tradable with entered decimals, the commitment from the holdings', () => {
    const r = heldTokenView(kob, ID.toUpperCase(), ['KCC20Ref'], [utxo(EXT), utxo(EXT)], 'mainnet');
    expect(r).toEqual({
      ok: true,
      view: { ticker: '', covenant_id: ID, template_hash: ref.hash, extension_commitment: EXT, scale: null, decimals: null, open_asks: 0, open_bids: 0, powers: [], template_id: 'kcc20-ref-3x3', family: 'kcc20' },
    });
    if (!r.ok) return;
    expect(synthesizeOpenToken(kob, r.view, [], null, null, [], { network: 'mainnet', fromHoldings: true })).toEqual({ ok: false, reason: 'no-scale' });
    const s = synthesizeOpenToken(kob, r.view, [], null, null, extensionReferences(ID, null, [utxo(EXT)]), { network: 'mainnet', fromHoldings: true, decimals: 8 });
    expect(s.ok && s.info).toMatchObject({ program: 'KCC20Ref', decimals: 8, openScale: 'entered', extension: { commitment: EXT, source: 'holdings', mismatch: [] } });
  });

  it('refuses: no holdings of the id, two programs, a prototype on mainnet, a KRON program, two commitments', () => {
    expect(heldTokenView(kob, ID, ['KCC20Ref'], [], 'mainnet')).toEqual({ ok: false, reason: 'not-held' });
    expect(heldTokenView(kob, ID, ['KCC20Ref'], [utxo(EXT, 'ff'.repeat(32))], 'mainnet')).toEqual({ ok: false, reason: 'not-held' });
    expect(heldTokenView(kob, ID, [], [utxo(EXT)], 'mainnet')).toEqual({ ok: false, reason: 'not-held' });
    expect(heldTokenView(kob, ID, ['KCC20Ref', 'KCC20PublicMint'], [utxo(EXT)], 'mainnet')).toEqual({ ok: false, reason: 'program-ambiguous' });
    expect(heldTokenView(kob, ID, ['KCC20Ref_8x8'], [utxo(EXT)], 'mainnet')).toEqual({ ok: false, reason: 'program-unreviewed' });
    expect(heldTokenView(kob, ID, ['KCC20Ref_8x8'], [utxo(EXT)], 'testnet-10').ok).toBe(true);
    expect(heldTokenView(kob, ID, ['KronToken2433'], [utxo(EXT)], 'mainnet')).toEqual({ ok: false, reason: 'not-kcc20' });
    expect(heldTokenView(kob, ID, ['KCC20Ref'], [utxo(EXT), utxo('cd'.repeat(32))], 'mainnet')).toEqual({ ok: false, reason: 'extension-ambiguous' });
  });
});

describe('findHeldToken: the programs of the stored candidates and of the node-confirmed holdings', () => {
  const OWNER = '22'.repeat(32);
  const state = { amount: '5', owner: OWNER, owner_scheme: 0, borrow_scheme: 0, borrow_guard: '00'.repeat(32), extension_commitment: EXT };
  const utxo = { transactionId: '11'.repeat(32), index: 0, amount: '100000000', covenantId: ID, state } as never;
  const tracker = (stored: string[], held: string[]) => ({
    list: () => stored.map((program) => ({ tokenCovId: ID, program }) as never),
    heldTokens: async () => held.map((program) => ({ covenantId: ID, program, state }) as never),
    tokenUtxosFor: async () => [utxo],
  });

  it('a token bought elsewhere (nothing stored) opens from the holdings (TN10 2026-10-14 B-5)', async () => {
    const r = await findHeldToken(tracker([], ['KCC20Ref']), kob, OWNER, ID.toUpperCase(), 'mainnet');
    expect(r.ok && r.view.extension_commitment).toBe(EXT);
    expect((await findHeldToken(tracker(['KCC20Ref'], []), kob, OWNER, ID, 'mainnet')).ok).toBe(true);
    expect(await findHeldToken(tracker([], []), kob, OWNER, ID, 'mainnet')).toEqual({ ok: false, reason: 'not-held' });
    expect(await findHeldToken(tracker(['KCC20Ref'], ['KCC20PublicMint']), kob, OWNER, ID, 'mainnet')).toEqual({ ok: false, reason: 'program-ambiguous' });
    // a failed holdings read: the stored candidates only
    const failing = { ...tracker(['KCC20Ref'], []), heldTokens: async () => Promise.reject(new Error('node down')) };
    expect((await findHeldToken(failing, kob, OWNER, ID, 'mainnet')).ok).toBe(true);
  });

  it('a KRON holding counts under the holder owner of its program (KForge: id_type 0, not address presence)', async () => {
    const kron = (id_type: number) => ({ amount: '5', owner: OWNER, id_type, is_minter: 0 });
    const src = (id_type: number) => ({
      list: () => [],
      heldTokens: async () => [{ covenantId: ID, program: 'KForgeToken1785', state: kron(id_type) } as never],
      tokenUtxosFor: async () => [{ transactionId: '11'.repeat(32), index: 0, amount: '100000000', covenantId: ID, state: kron(id_type) } as never],
    });
    // held: the program is found (and the open-token path, KCC-20 only, names why it stops)
    expect(await findHeldToken(src(0), kob, OWNER, ID, 'testnet-10')).toEqual({ ok: false, reason: 'not-kcc20' });
    // an address-presence state under KForge is not a holding of it
    expect(await findHeldToken(src(3), kob, OWNER, ID, 'testnet-10')).toEqual({ ok: false, reason: 'not-held' });
  });
});

describe('a token of a program with default decimals (KaspaRocket: 3, its convention)', () => {
  const kr = kob.templates().find((t) => t.name === 'KaspaRocketToken11458')!;
  const rocket = (over: Partial<IndexerTokenView> = {}) => view({ template_hash: kr.hash, ...over });

  it('is tradable on mainnet without orders, at 3 decimals (scale 1000), the source named', () => {
    const r = synthesizeOpenToken(kob, rocket(), [], null, null, [], { network: 'mainnet' });
    expect(r.ok).toBe(true);
    if (!r.ok) return;
    expect(r.info).toMatchObject({
      program: 'KaspaRocketToken11458', decimals: 3, openScale: 'program', decimalsConvention: 'KaspaRocket convention', slots: { inputs: 10, outputs: 10 }, tradable: true,
    });
    expect(toTokenMarket(kob, r.info.json)).toMatchObject({ scale: 1000n, decimals: 3 });
    expect(programAllowed('KaspaRocketToken11458', 'mainnet')).toBe(true);
  });

  it('orders of its scale agree; a market of another scale is a scale conflict; entered decimals are not asked for', () => {
    expect(synthesizeOpenToken(kob, rocket(), [{ scale: 1000 }], null, null, [], { network: 'mainnet' })).toMatchObject({ ok: true, info: { decimals: 3, openScale: 'program' } });
    expect(synthesizeOpenToken(kob, rocket(), [{ scale: 100_000_000 }], null, null, [], { network: 'mainnet' })).toEqual({ ok: false, reason: 'scale-conflict' });
    expect(synthesizeOpenToken(kob, rocket({ market_scale: 100 }), [], null, null, [], { network: 'mainnet' })).toEqual({ ok: false, reason: 'scale-conflict' });
    expect(synthesizeOpenToken(kob, rocket(), [], null, null, [], { network: 'mainnet', decimals: 8 })).toMatchObject({ ok: true, info: { decimals: 3, openScale: 'program' } });
  });

  it('a token of the program that declares its decimals (KCC-23) uses its own', () => {
    const declared = { decimals: 6, symbol: 'RKT', name: 'Rocket' };
    const r = synthesizeOpenToken(kob, rocket(), [], null, null, [], { network: 'mainnet', declared });
    expect(r).toMatchObject({ ok: true, info: { decimals: 6, openScale: 'declared', ticker: 'RKT' } });
    expect(r.ok && r.info.decimalsConvention).toBeFalsy();
  });
});

describe('findPublicToken: a covenant id no list names, from token outputs the indexer names and the node confirmed, whoever holds them', () => {
  const state = (ext = EXT) => ({ amount: '5', owner: '22'.repeat(32), owner_scheme: 4, borrow_scheme: 0, borrow_guard: '00'.repeat(32), extension_commitment: ext });
  const out = (program: string, ext = EXT, cov = ID) => ({ transactionId: '11'.repeat(32), index: 0, amount: '100000000', covenantId: cov, program, state: state(ext) }) as never;
  const src = (r: { indexer?: 'ok' | 'unavailable'; claimed?: number; nodeFailed?: boolean; utxos?: unknown[] }) => ({
    publicTokenUtxos: async () => ({ indexer: 'ok' as const, claimed: (r.utxos ?? []).length, nodeFailed: false, utxos: [], ...r }) as never,
  });

  it('confirmed outputs of one pinned program and one commitment: a row with the program and the commitment, no scale, no wallet needed', async () => {
    const r = await findPublicToken(src({ utxos: [out('KCC20Ref'), out('KCC20Ref')] }), kob, ID.toUpperCase(), 'mainnet');
    expect(r).toMatchObject({ ok: true, view: { covenant_id: ID, extension_commitment: EXT, family: 'kcc20', template_id: 'kcc20-ref-3x3', open_asks: 0, open_bids: 0 } });
    if (!r.ok) return;
    // the extension commitment's source is the chain (the node-confirmed outputs); without decimals it still needs the first order's
    expect(synthesizeOpenToken(kob, r.view, [], null, null, [], { network: 'mainnet', fromChain: true })).toEqual({ ok: false, reason: 'no-scale' });
    const s = synthesizeOpenToken(kob, r.view, [], null, null, [], { network: 'mainnet', fromChain: true, decimals: 8 });
    expect(s.ok && s.info).toMatchObject({ program: 'KCC20Ref', decimals: 8, openScale: 'entered', extension: { commitment: EXT, source: 'chain', mismatch: [] } });
  });

  it("a KaspaRocket token trades at once with its program's 3 decimals", async () => {
    const r = await findPublicToken(src({ utxos: [out('KaspaRocketToken11458')] }), kob, ID, 'mainnet');
    expect(r.ok).toBe(true);
    if (!r.ok) return;
    expect(synthesizeOpenToken(kob, r.view, [], null, null, [], { network: 'mainnet', fromChain: true })).toMatchObject({ ok: true, info: { decimals: 3, openScale: 'program', extension: { source: 'chain' } } });
  });

  it('says which fact is missing', async () => {
    expect(await findPublicToken(src({ indexer: 'unavailable' }), kob, ID, 'mainnet')).toEqual({ ok: false, reason: 'indexer-unavailable' });
    expect(await findPublicToken(src({ claimed: 0 }), kob, ID, 'mainnet')).toEqual({ ok: false, reason: 'not-indexed' });
    expect(await findPublicToken(src({ claimed: 3 }), kob, ID, 'mainnet')).toEqual({ ok: false, reason: 'not-confirmed' });
    expect(await findPublicToken(src({ claimed: 3, nodeFailed: true }), kob, ID, 'mainnet')).toEqual({ ok: false, reason: 'node-unavailable' });
  });

  it('the same refusals as the wallet path: two programs, a prototype on mainnet, a KRON program, two commitments', async () => {
    expect(await findPublicToken(src({ utxos: [out('KCC20Ref'), out('KCC20PublicMint')] }), kob, ID, 'mainnet')).toEqual({ ok: false, reason: 'program-ambiguous' });
    expect(await findPublicToken(src({ utxos: [out('KCC20Ref_8x8')] }), kob, ID, 'mainnet')).toEqual({ ok: false, reason: 'program-unreviewed' });
    expect((await findPublicToken(src({ utxos: [out('KCC20Ref_8x8')] }), kob, ID, 'testnet-10')).ok).toBe(true);
    expect(await findPublicToken(src({ utxos: [out('KronToken2433')] }), kob, ID, 'mainnet')).toEqual({ ok: false, reason: 'not-kcc20' });
    expect(await findPublicToken(src({ utxos: [out('KCC20Ref'), out('KCC20Ref', 'cd'.repeat(32))] }), kob, ID, 'mainnet')).toEqual({ ok: false, reason: 'extension-ambiguous' });
    // outputs of another covenant id are no evidence for this one
    expect(await findPublicToken(src({ claimed: 1, utxos: [out('KCC20Ref', EXT, 'ff'.repeat(32))] }), kob, ID, 'mainnet')).toEqual({ ok: false, reason: 'not-confirmed' });
  });

  it("the commitment a wallet holding of the token names must agree with the chain's: a different one is a mismatch (blocking at signing)", async () => {
    const r = await findPublicToken(src({ utxos: [out('KCC20Ref')] }), kob, ID, 'mainnet');
    if (!r.ok) throw new Error('expected a row');
    const held = [{ transactionId: '33'.repeat(32), index: 0, amount: '1', covenantId: ID, state: state('cd'.repeat(32)) }] as never;
    const s = synthesizeOpenToken(kob, r.view, [], null, null, extensionReferences(ID, null, held), { network: 'mainnet', fromChain: true, decimals: 8 });
    expect(s.ok && s.info.extension?.mismatch).toEqual([{ commitment: 'cd'.repeat(32), source: 'holdings' }]);
  });
});
