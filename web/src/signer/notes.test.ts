import { describe, expect, it } from 'vitest';
import { noteSentences } from './notes';

describe("the frame's words for the app's notes", () => {
  it("fixed sentences with the numbers filled in, the same wording as the app's own confirmation", () => {
    expect(noteSentences([{ kind: 'belowMin', value: '98800000', min: '100000000', source: 'default' }])).toEqual([
      'Worth 0.988 KAS, under the usual 1 KAS minimum order value: it will not be listed, so it will not fill. Cancel it in My orders.',
    ]);
    expect(noteSentences([{ kind: 'belowMin', value: '50000000', min: '250000000', source: 'indexer' }])).toEqual([
      'Worth 0.5 KAS, under the indexer minimum of 2.5 KAS: it will not be listed, so it will not fill. Cancel it in My orders.',
    ]);
    expect(noteSentences([{ kind: 'farThrough', bps: 1230 }])).toEqual(['Your price is 12.3% through the market. Check for a typing error.']);
  });

  it('keeps the order given and says nothing for no notes', () => {
    expect(noteSentences([])).toEqual([]);
    expect(noteSentences([{ kind: 'farThrough', bps: 1000 }, { kind: 'belowMin', value: '1', min: '2', source: 'default' }]).map((s) => s.slice(0, 10))).toEqual(['Your price', 'Worth 0.00']);
  });
});
