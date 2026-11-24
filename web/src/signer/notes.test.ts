import { describe, expect, it } from 'vitest';
import { noteSentences } from './notes';

describe("the frame's words for the app's notes", () => {
  it("fixed sentences with the numbers filled in, the same wording as the app's own confirmation", () => {
    expect(noteSentences([{ kind: 'belowMin', value: '98800000', min: '100000000', source: 'default' }])).toEqual([
      'Worth 0.988 KAS, under the usual 1 KAS minimum order value: it will not be listed, so it will not fill. Cancel it in Portfolio.',
    ]);
    expect(noteSentences([{ kind: 'belowMin', value: '50000000', min: '250000000', source: 'indexer' }])).toEqual([
      'Worth 0.5 KAS, under the indexer minimum of 2.5 KAS: it will not be listed, so it will not fill. Cancel it in Portfolio.',
    ]);
    expect(noteSentences([{ kind: 'congestion' }])).toEqual(['Blocks are contended, so this transaction may wait. The priority fee is in Settings.']);
    expect(noteSentences([{ kind: 'farThrough', bps: 1230 }])).toEqual(['Your price is 12.3% through the market. Check for a typing error.']);
  });

  it('keeps the order given and says nothing for no notes', () => {
    expect(noteSentences([])).toEqual([]);
    expect(noteSentences([{ kind: 'farThrough', bps: 1000 }, { kind: 'belowMin', value: '1', min: '2', source: 'default' }]).map((s) => s.slice(0, 10))).toEqual(['Your price', 'Worth 0.00']);
  });
});

describe('the note of token UTXOs a closing order leaves behind (R8F4-2)', () => {
  it('passes the protocol as a count only and is worded by the frame', async () => {
    const { sanitizeNotes } = await import('./protocol');
    expect(sanitizeNotes([{ kind: 'straysLost', utxos: 2 }])).toEqual([{ kind: 'straysLost', utxos: 2 }]);
    expect(sanitizeNotes([{ kind: 'straysLost', utxos: 0 }, { kind: 'straysLost', utxos: 'x' }, { kind: 'straysLost', utxos: 1, text: 'hi' }])).toEqual([]);
    expect(noteSentences([{ kind: 'straysLost', utxos: 2 }])).toEqual(['This closes an order that owns 2 token UTXO(s) its maker cannot move. They stay behind and cannot be recovered.']);
  });
});

describe('the note of a closing order whose strays could not be checked (R8B-M1)', () => {
  it('passes the protocol with no payload and is worded by the frame', async () => {
    const { sanitizeNotes } = await import('./protocol');
    expect(sanitizeNotes([{ kind: 'straysUnchecked' }])).toEqual([{ kind: 'straysUnchecked' }]);
    expect(sanitizeNotes([{ kind: 'straysUnchecked', text: 'x' }])).toEqual([]);
    expect(noteSentences([{ kind: 'straysUnchecked' }])[0]).toMatch(/could not be checked/);
  });
});
