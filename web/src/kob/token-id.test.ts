// A token outside the registry is identified by its covenant id alone: it is shown with 8 + 8 hex (64 bits). Tokens that share a ticker are told apart
// by covenant id (`sharedTickerIds`).
import { describe, expect, it } from 'vitest';
import { displayName, longId, parseRegistry, shortId } from './registry';
import { tradableRegistryJson } from '../testing/chain-fixtures';
import { loadKobNode } from './wasm.node';
import { tokenLabel } from '../ui/confirm/confirm-model';
import { buildTokenRows, sharedTickerIds, tickerOrId, tokenLabel as rowLabel } from '../ui/market/token-model';
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
    expect(displayName({ ticker: longId(ID), covenantId: ID })).toBe('01234567…89abcdef');
  });
});

describe('shared tickers', () => {
  const view = (id: string, ticker: string): IndexerTokenView => ({ covenant_id: id, ticker, standing: 'unverified', open_asks: 1, open_bids: 0 }) as unknown as IndexerTokenView;
  const a = `abcd${'11'.repeat(28)}ef01`;
  const b = `abcd${'22'.repeat(28)}ef01`;
  const c = 'cd'.repeat(32);

  it('two tokens with the same ticker are both flagged; a unique ticker and an empty one are not', () => {
    const rows = buildTokenRows(reg(), [view(a, 'ZZZ'), view(b, 'ZZZ'), view(c, 'YYY'), view('ef'.repeat(32), ''), view('ee'.repeat(32), '')]);
    expect([...sharedTickerIds(rows)].sort()).toEqual([a, b].sort());
  });

  it('case and look-alike letters count as the same ticker (KR0N is KRON); a token is not its own namesake', () => {
    expect([...sharedTickerIds([{ covenantId: a, ticker: 'KRON' }, { covenantId: b, ticker: 'kr0n' }])].sort()).toEqual([a, b].sort());
    expect(sharedTickerIds([{ covenantId: a, ticker: 'KRON' }, { covenantId: a, ticker: 'KRON' }]).size).toBe(0);
    expect(sharedTickerIds([]).size).toBe(0);
  });
});

describe('every shortened id of a token outside the registry is 8 + 8 hex', () => {
  it('the market list label, the ticker stand-in and an indexer ticker', () => {
    expect(rowLabel({ ticker: '', covenantId: ID, info: null })).toBe('01234567…89abcdef');
    expect(tickerOrId('', ID)).toBe('01234567…89abcdef');
    expect(rowLabel({ ticker: 'KASPER', covenantId: ID, info: null })).toBe('KASPER (01234567…89abcdef)');
    // a registry ticker keeps the 4 + 4 fragment next to it
    expect(rowLabel({ ticker: 'TST', covenantId: ID })).toBe('TST (0123…cdef)');
  });
});
