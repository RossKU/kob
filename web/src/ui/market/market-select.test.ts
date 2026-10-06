import { describe, expect, it } from 'vitest';
import { pairRoute, tokenRoute } from '../../app/router';
import { KAS, assetChoices, changePair, displayedPair, marketFor } from './market-select';

const A = 'aa'.repeat(32);
const B = 'bb'.repeat(32);
const C = 'cc'.repeat(32);

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
    { covenantId: A, ticker: 'AAA', pairable: true },
    { covenantId: B, ticker: 'BBB', pairable: true },
    { covenantId: C, ticker: 'CCC', pairable: false },
  ];
  const label = (r: { ticker: string }) => `${r.ticker} (full)`;
  it('next to KAS: every token; next to a token: KAS and the pairable tokens, never the other side', () => {
    expect(assetChoices(rows, KAS, label).map((c) => c.value)).toEqual([A, B, C]);
    expect(assetChoices(rows, A, label).map((c) => c.value)).toEqual([KAS, B]);
    expect(assetChoices(rows, B, label)[0]).toEqual({ value: KAS, label: 'KAS', short: 'KAS' });
    expect(assetChoices(rows, A, label).at(-1)).toEqual({ value: B, label: 'BBB (full)', short: 'BBB' });
  });
});
