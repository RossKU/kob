// A token missing from a cut token list is read by its id, so a token opened by id or link stays
// tradable (kobd GET /v1/tokens/{id}).
import { describe, expect, it } from 'vitest';
import { tokensWith } from './indexer';
import type { IndexerTokenView } from './indexer-types';

const A = 'aa'.repeat(32);
const B = 'bb'.repeat(32);
const view = (id: string) => ({ covenant_id: id }) as IndexerTokenView;

describe('tokensWith', () => {
  it('a cut list lacking the id reads it by id; a whole list is not asked again', async () => {
    let asked: string[] = [];
    const ix = { tokens: async () => Object.assign([view(A)], { truncated: true as const }), tokenById: async (id: string) => (asked.push(id), id === B ? view(B) : null) };
    expect((await tokensWith(ix, [B])).map((t) => t.covenant_id)).toEqual([A, B]);
    expect(asked).toEqual([B]);
    asked = [];
    expect((await tokensWith(ix, [A])).map((t) => t.covenant_id)).toEqual([A]);
    expect(asked).toEqual([]);
    const whole = { tokens: async () => [view(A)], tokenById: async () => view(B) };
    expect((await tokensWith(whole, [B])).map((t) => t.covenant_id)).toEqual([A]);
    // an id the indexer does not know stays out
    expect((await tokensWith(ix, ['cc'.repeat(32)])).map((t) => t.covenant_id)).toEqual([A]);
  });
});
