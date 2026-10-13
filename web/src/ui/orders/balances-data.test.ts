import { describe, expect, it } from 'vitest';
import type { Services } from '../../app/services';
import type { HeldTokenUtxo } from '../../data/token-tracker';
import type { TokenInfo } from '../../kob/registry';
import type { TokenUtxo } from '../../kob/types';
import { loadFreeBalances } from './balances-data';

const ME = '11'.repeat(32);
const LISTED = '70'.repeat(32);
const OUTSIDE = '5a'.repeat(32);
const utxo = (n: number, token: string, amount: string): TokenUtxo => ({
  transactionId: n.toString(16).padStart(2, '0').repeat(32), index: 0, amount: '1000', covenantId: token,
  state: { amount, owner: ME, owner_scheme: 0, borrow_scheme: 0, borrow_guard: '00'.repeat(32), extension_commitment: 'ee'.repeat(32) },
});

function services(held: HeldTokenUtxo[] | Error, calls: unknown[]): Services {
  return {
    utxos: { fundingFor: async () => [{ amount: '500' }] },
    tracker: {
      tokenUtxosFor: async (_pk: string, t: { covenantId: string }) => (t.covenantId === LISTED ? [utxo(1, LISTED, '7')] : []),
      heldTokens: async (_pk: string, o: unknown) => {
        calls.push(o);
        if (held instanceof Error) throw held;
        return held;
      },
    },
  } as unknown as Services;
}

describe('loadFreeBalances', () => {
  const tokens = [{ covenantId: LISTED, program: 'KCC20Ref' }] as unknown as TokenInfo[];

  it('reads the registry tokens only, unless asked for every token', async () => {
    const calls: unknown[] = [];
    const r = await loadFreeBalances(services([], calls), ME, tokens);
    expect(r.freeTokens.map((u) => u.covenantId)).toEqual([LISTED]);
    expect(calls).toEqual([]);
    expect(r.programs.size).toBe(0);
  });

  it('anyToken: adds the other tokens the key holds, with their verified program, and skips the registry ones it read', async () => {
    const calls: { skip: Set<string> }[] = [];
    const held: HeldTokenUtxo[] = [{ ...utxo(2, OUTSIDE, '30'), covenantId: OUTSIDE, program: 'KCC20Ref_8x8' }];
    const r = await loadFreeBalances(services(held, calls as unknown[]), ME, tokens, { anyToken: true });
    expect([...calls[0]!.skip]).toEqual([LISTED]);
    expect(r.freeTokens.map((u) => [u.covenantId, u.state.amount])).toEqual([[LISTED, '7'], [OUTSIDE, '30']]);
    expect(r.freeTokens.some((u) => 'program' in u)).toBe(false);
    expect([...r.programs]).toEqual([[OUTSIDE, 'KCC20Ref_8x8']]);
    expect(r.kasFree).toBe(500n);
    expect(r.errors).toEqual([]);
  });

  it('anyToken: a failing read degrades to the registry tokens and reports the source', async () => {
    const r = await loadFreeBalances(services(new Error('indexer down'), []), ME, tokens, { anyToken: true });
    expect(r.freeTokens.map((u) => u.covenantId)).toEqual([LISTED]);
    expect(r.errors).toEqual([{ source: 'tokens', message: 'indexer down' }]);
  });
});
