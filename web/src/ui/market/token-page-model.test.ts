// What a token page says of a token no registry or indexer list names (a "no orders" row of the list, a covenant id typed in): the row kascov.io gives it
// (`kascovRowOf`), its program and decimals rule (`ProgramNote`), the tab titles, and the facts the node confirmed.
import { describe, expect, it } from 'vitest';
import type { KascovToken } from '../../data/kascov';
import { pageTitle } from '../../app/page-title';
import { t } from '../../i18n';
import { TOKEN_PROGRAMS } from '../../kob/programs.generated';
import type { TokenRegistry } from '../../kob/registry';
import { loadKobNode } from '../../kob/wasm.node';
import { kascovRowOf } from './kascov-merge';
import { decimalsLine, programOfRow, reviewText } from './ProgramNote';
import { unknownRow } from './token-model';

const kob = loadKobNode();
const templates = kob.templates();
const hashOf = (name: string): string => templates.find((x) => x.name === name)!.hash;
const ID = (c: string) => c.repeat(64);
const registry: TokenRegistry = { network: 'testnet-10', templates: [], templateChecks: [], tokens: [], byCovenantId: new Map() };
const k = (over: Partial<KascovToken> = {}): KascovToken => ({
  covenantId: ID('1'), ticker: 'ROCKET', name: 'Rocket Token', decimals: 3, templateHash: hashOf('KaspaRocketToken11458'), imageHash: null, imageUrl: null, ...over,
});
const rowOf = (over: Partial<KascovToken> = {}) => kascovRowOf(ID('1'), [k(over)], { network: 'testnet-10', templates, registry })!;

describe('programOfRow: the pinned program of a row, by its template hash', () => {
  it('a kascov row names the program of its hash in this build\'s table, with its review status', () => {
    const p = programOfRow(rowOf());
    expect(p).toMatchObject({ name: 'KaspaRocketToken11458', review: 'reviewed' });
    expect(programOfRow(rowOf({ templateHash: hashOf('KCC20Ref_8x8') }))).toMatchObject({ name: 'KCC20Ref_8x8', review: 'pending-review' });
    expect(programOfRow(rowOf({ templateHash: hashOf('KCC20PublicMint') }))).toMatchObject({ name: 'KCC20PublicMint', review: 'reviewed' });
  });
  it('a verified program (`info`) wins over a hash; no hash, no program', () => {
    const row = rowOf();
    expect(programOfRow({ ...row, info: { program: 'KCC20Ref', templateHash: hashOf('KCC20Ref') } as never })?.name).toBe('KCC20Ref');
    expect(programOfRow({ info: null, index: null })).toBeNull();
    expect(programOfRow({ info: null, index: { template_hash: 'ee'.repeat(32) } as never })).toBeNull();
  });
  it('every review status of the table has a sentence', () => {
    for (const p of TOKEN_PROGRAMS) expect(reviewText(p.review)).not.toMatch(/^market\./);
  });
});

describe('decimalsLine: where the decimals of a token not yet opened for trading come from, by the rules of the whole app', () => {
  it('a program with default decimals (KaspaRocket: 3) says so and names the convention', () => {
    expect(rowOf().decimals).toBe(3);
    expect(decimalsLine(rowOf())).toBe(t('market.open.decimals.program', { decimals: 3, source: 'KaspaRocket convention' }));
  });
  it('a token with a verified declaration says it declared them', () => {
    const view = { ticker: '', covenant_id: ID('1'), template_hash: hashOf('KCC20PublicMint'), extension_commitment: null, decimals: null, scale: null, open_asks: 0, open_bids: 0 };
    const decl = { covenantId: ID('1'), decimals: 6, symbol: 'DEC', name: 'Declared', description: null, logo: null, website: null, x: null, telegram: null, preparationTxid: ID('2'), json: '{}' };
    const row = unknownRow(registry, view as never, 'kascov', decl);
    expect(row.decimals).toBe(6);
    expect(decimalsLine(row)).toBe(t('market.open.decimals.declared', { decimals: 6 }));
  });
  it('a token that has neither says its decimals are not known until the first order names them, never a number', () => {
    const row = rowOf({ templateHash: hashOf('KCC20PublicMint'), decimals: 2 });
    // kascov's claimed decimals are a label, never the rule
    expect(row.decimals).toBeNull();
    expect(decimalsLine(row)).toBe(t('market.program.decimalsUnknown'));
  });
});

describe('tab titles', () => {
  it('a page is named, then the app', () => {
    expect(pageTitle('Market')).toBe('Market - KOB');
    expect(pageTitle('THEN')).toBe('THEN - KOB');
    expect(pageTitle(`${'ROCKET'} / ${'KAS'}`)).toBe('ROCKET / KAS - KOB');
  });
  it('the pages of the app have names', () => {
    for (const p of ['market', 'orders', 'create', 'settings']) expect(t(`shell.nav.${p}`)).not.toMatch(/^shell\./);
  });
});
