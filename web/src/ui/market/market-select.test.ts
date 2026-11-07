import { describe, expect, it } from 'vitest';
import { pairRoute, tokenRoute } from '../../app/router';
import { KAS, assetChoices, changePair, displayedPair, filterChoices, marketFor } from './market-select';
import { idOnlyRow, type TokenRow } from './token-model';

const A = 'aa'.repeat(32);
const B = 'bb'.repeat(32);
const C = 'cc'.repeat(32);

describe('filterChoices', () => {
  const row = (id: string, ticker: string, name: string | null): TokenRow => ({ ...idOnlyRow(id), ticker, name });
  const rows = [row(A, 'TBTC', 'Test Bitcoin'), row(B, 'TBTC', 'Another token'), row(C, 'KRON', null)];
  const choices = assetChoices(rows, 'none', (r) => r.ticker || r.covenantId);
  const found = (q: string) => filterChoices(choices, rows, q).map((c) => c.value);

  it('finds a token by ticker, name or covenant id, look-alike letters too; two tokens with one ticker are both found', () => {
    expect(found('tbtc')).toEqual([A, B]);
    expect(found('kr0n')).toEqual([C]);
    expect(found('another')).toEqual([B]);
    expect(found(C.slice(10, 40))).toEqual([C]);
    expect(found(C.toUpperCase())).toEqual([C]);
  });

  it('an empty search leaves all; KAS only by its name', () => {
    expect(found('  ')).toEqual([KAS, A, B, C]);
    expect(found('ka')).toEqual([KAS]);
    expect(found('zzz')).toEqual([]);
  });
});

describe('displayedPair', () => {
  it('TOKEN/KAS natively, KAS/TOKEN inverted, a token pair as routed', () => {
    expect(displayedPair(A, null, false)).toEqual({ left: A, right: KAS });
    expect(displayedPair(A.toUpperCase(), null, true)).toEqual({ left: KAS, right: A });
    expect(displayedPair(A, B, false)).toEqual({ left: A, right: B });
    expect(displayedPair(A, B, true)).toEqual({ left: A, right: B }); // a pair has no stored flip: its swap is a route
  });
});

describe('changePair', () => {
  it('replaces one side', () => {
    expect(changePair({ left: A, right: KAS }, 'right', B)).toEqual({ left: A, right: B });
    expect(changePair({ left: A, right: KAS }, 'left', B)).toEqual({ left: B, right: KAS });
    expect(changePair({ left: KAS, right: A }, 'right', B)).toEqual({ left: KAS, right: B });
  });
  it('picking the other side asset swaps the sides', () => {
    expect(changePair({ left: A, right: KAS }, 'left', KAS)).toEqual({ left: KAS, right: A });
    expect(changePair({ left: A, right: B }, 'right', A)).toEqual({ left: B, right: A });
  });
});

describe('marketFor', () => {
  it('the KAS market of a token with its orientation, or the pair route', () => {
    expect(marketFor({ left: A, right: KAS })).toEqual({ route: tokenRoute(A), inverted: false });
    expect(marketFor({ left: KAS, right: A })).toEqual({ route: tokenRoute(A), inverted: true });
    expect(marketFor({ left: A, right: B })).toEqual({ route: pairRoute(A, B), inverted: null });
    expect(marketFor({ left: KAS, right: KAS })).toBeNull();
  });
});

describe('assetChoices', () => {
  const rows = [
    { covenantId: A, ticker: 'AAA' },
    { covenantId: B, ticker: 'BBB' },
    { covenantId: C, ticker: 'CCC' },
  ];
  const label = (r: { ticker: string }) => `${r.ticker} (full)`;
  it('next to KAS: every token; next to a token: KAS and every other token, registered or not, never the other side', () => {
    expect(assetChoices(rows, KAS, label).map((c) => c.value)).toEqual([A, B, C]);
    expect(assetChoices(rows, A, label).map((c) => c.value)).toEqual([KAS, B, C]);
    expect(assetChoices(rows, B, label)[0]).toEqual({ value: KAS, label: 'KAS', short: 'KAS' });
    expect(assetChoices(rows, A, label)[1]).toEqual({ value: B, label: 'BBB (full)', short: 'BBB' });
  });
});
