// A token outside the registry is identified by its covenant id alone: it is shown with 8 + 8 hex (64 bits), and a token whose 4 + 4 short id or
// ticker is the same as a registered or another listed token's is flagged.
import { describe, expect, it } from 'vitest';
import { displayName, longId, lookalikeReport, parseRegistry, shortId } from './registry';
import { TOKEN, tradableRegistryJson } from '../testing/chain-fixtures';
import { loadKobNode } from './wasm.node';
import { tokenLabel } from '../ui/confirm/confirm-model';
import { buildTokenRows, tickerOrId, tokenLabel as rowLabel } from '../ui/market/token-model';
import { t } from '../i18n';
import type { IndexerTokenView } from '../data/indexer-types';

const kob = loadKobNode();
const reg = () => parseRegistry(tradableRegistryJson(), { kob });
const ID = '0123456789abcdef'.repeat(4);

describe('covenant id forms', () => {
  it('short is 4 + 4 hex, long is 8 + 8 hex', () => {
    expect(shortId(ID)).toBe('0123…cdef');
    expect(longId(ID)).toBe('01234567…89abcdef');
  });

  it('the confirmation names a token outside the registry by 8 + 8 hex', () => {
    const label = tokenLabel({ covenantId: ID, ticker: null, decimals: null, display: '', inRegistry: false, tradable: false }, t, reg());
    expect(label).toContain('01234567…89abcdef');
  });

  it('a token synthesised with its long id as ticker is shown once by it', () => {
    expect(displayName({ ticker: longId(ID), covenantId: ID, status: 'listed', verified: false })).toBe('01234567…89abcdef [unverified]');
  });
});

describe('short id and ticker collisions', () => {
  it('a token outside the registry with the short id of a registered token is a strong look-alike', () => {
    const r = reg();
    // same first 4 and last 4 hex as the registered TOKEN (70...70), different in between
    const copy = `7070${'ab'.repeat(28)}7070`;
    expect(copy).not.toBe(TOKEN.covenantId);
    const rep = lookalikeReport(r, '', copy);
    expect(rep.level).toBe('strong');
    expect(rep.lookalikes[0]).toMatchObject({ kind: 'same-short-id' });
    expect(rep.lookalikes[0]!.token.covenantId).toBe(TOKEN.covenantId);
  });

  it('two unregistered tokens with the same short id or the same ticker are flagged as a collision', () => {
    const r = reg();
    const a = `abcd${'11'.repeat(28)}ef01`;
    const b = `abcd${'22'.repeat(28)}ef01`;
    const c = 'cd'.repeat(32);
    expect(lookalikeReport(r, '', a, undefined, [{ covenantId: b, ticker: '' }])).toMatchObject({ level: 'collision', collisions: [{ covenantId: b, kind: 'same-short-id' }] });
    expect(lookalikeReport(r, 'ZZZ', a, undefined, [{ covenantId: c, ticker: 'ZZZ' }])).toMatchObject({ level: 'collision', collisions: [{ covenantId: c, kind: 'same-ticker' }] });
    expect(lookalikeReport(r, 'ZZZ', a, undefined, [{ covenantId: c, ticker: 'YYY' }]).level).toBe('unknown');
    // the token itself is not its own collision
    expect(lookalikeReport(r, '', a, undefined, [{ covenantId: a, ticker: '' }]).level).toBe('unknown');
  });

  it('the token list compares every unregistered token with the others', () => {
    const r = reg();
    const view = (id: string): IndexerTokenView => ({ covenant_id: id, ticker: '', standing: 'unverified', open_asks: 1, open_bids: 0 }) as unknown as IndexerTokenView;
    const a = `abcd${'11'.repeat(28)}ef01`;
    const b = `abcd${'22'.repeat(28)}ef01`;
    const rows = buildTokenRows(r, [view(a), view(b), view('cd'.repeat(32))]);
    expect(rows.find((x) => x.covenantId === a)!.lookalike!.level).toBe('collision');
    expect(rows.find((x) => x.covenantId === b)!.lookalike!.level).toBe('collision');
    expect(rows.find((x) => x.covenantId === 'cd'.repeat(32))!.lookalike!.level).toBe('unknown');
  });

  it('a token the wallet holds counts even when the indexer leaves it out of its list', () => {
    const r = reg();
    const view = (id: string): IndexerTokenView => ({ covenant_id: id, ticker: '', standing: 'unverified', open_asks: 1, open_bids: 0 }) as unknown as IndexerTokenView;
    const held = `abcd${'11'.repeat(28)}ef01`;
    const copy = `abcd${'22'.repeat(28)}ef01`;
    // the indexer lists only the copy: without the wallet's own record nothing is said
    expect(buildTokenRows(r, [view(copy)]).find((x) => x.covenantId === copy)!.lookalike!.level).toBe('unknown');
    const row = buildTokenRows(r, [view(copy)], [], [held]).find((x) => x.covenantId === copy)!;
    expect(row.lookalike).toMatchObject({ level: 'collision', collisions: [{ covenantId: held, kind: 'same-short-id' }] });
  });
});

describe('every shortened id of a token outside the registry is 8 + 8 hex', () => {
  it('the market list label, the ticker stand-in and an indexer ticker', () => {
    expect(rowLabel({ ticker: '', covenantId: ID, info: null }, 'unverified')).toBe('01234567…89abcdef [unverified]');
    expect(tickerOrId('', ID)).toBe('01234567…89abcdef');
    expect(rowLabel({ ticker: 'KASPER', covenantId: ID, info: null }, 'unverified')).toBe('KASPER (01234567…89abcdef) [unverified]');
    // a registry ticker keeps the 4 + 4 fragment next to it (registry tickers are unique and look-alike checked)
    expect(rowLabel({ ticker: 'TST', covenantId: ID }, 'verified')).toBe('TST (0123…cdef) [verified]');
  });
});
