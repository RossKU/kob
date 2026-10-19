// How a token is shown: the registry's decimals and ticker, else those of this browser's issuance of it (unverified), else the assumed 8 decimals
// and its covenant id as its name (founder 2026-10-17: never "base units" in front of a user).
import { describe, expect, it } from 'vitest';
import type { TokenInfo } from '../kob/registry';
import { rememberIssuedToken } from '../data/issued-decimals';
import { ASSUMED_DECIMALS, decimalsNote, displayToken } from './token-display';
import { tokenLabel } from '../ui/market/token-model';

const mem = () => {
  const m = new Map<string, string>();
  return { getItem: (k: string) => m.get(k) ?? null, setItem: (k: string, v: string) => void m.set(k, v), removeItem: (k: string) => void m.delete(k) };
};
const ID = '83478d88fd44039ee17ad798c9b18a0405836389e09a603d29737a7e6daacd64';
const REG = 'ab'.repeat(32);
const registry = { byCovenantId: new Map([[REG, { covenantId: REG, ticker: 'TUSD', decimals: 6 } as unknown as TokenInfo]]) };

describe('displayToken: three sources, in order', () => {
  it('the registry first: its own entry, unmarked', () => {
    const s = mem();
    rememberIssuedToken('testnet-10', REG, { decimals: 2, ticker: 'NOPE' }, s);
    const tk = displayToken(registry, 'testnet-10', REG, s)!;
    expect(tk).toMatchObject({ ticker: 'TUSD', decimals: 6 });
    expect(tk.decimalsSource).toBeUndefined();
    expect(decimalsNote(tk)).toBeNull();
  });

  it('else this browser\'s issuance of it: its ticker, name and decimals, marked unverified', () => {
    const s = mem();
    rememberIssuedToken('testnet-10', ID, { decimals: 8, ticker: 'XYZ', name: 'Xyz coin' }, s);
    const tk = displayToken(registry, 'testnet-10', ID, s)!;
    expect(tk).toMatchObject({ ticker: 'XYZ', name: 'Xyz coin', decimals: 8, decimalsSource: 'issued', tradable: false });
    expect(decimalsNote(tk)).toMatchObject({ text: 'unverified' });
    expect(tokenLabel(tk)).toBe('XYZ (83478d88…6daacd64)');
    // another network's issuance is not this one's
    expect(displayToken(registry, 'mainnet', ID, s)!.decimalsSource).toBe('assumed');
  });

  it('else the assumed 8 decimals (the Kaspa convention), named by its covenant id: 1e8 base units read as 1 token', () => {
    const tk = displayToken(registry, 'testnet-10', ID, mem())!;
    expect(ASSUMED_DECIMALS).toBe(8);
    expect(tk).toMatchObject({ ticker: '83478d88…6daacd64', decimals: 8, decimalsSource: 'assumed', tradable: false });
    expect(decimalsNote(tk)).toMatchObject({ text: 'decimals assumed 8' });
    // the name is said once
    expect(tokenLabel(tk)).toBe('83478d88…6daacd64');
    expect(displayToken(registry, 'testnet-10', null, mem())).toBeUndefined();
  });

  it('reads an older entry (decimals only) and drops a hand-written ticker that is not a label', () => {
    const s = mem();
    s.setItem('kob.issuedDecimals.v1', JSON.stringify({ 'testnet-10': { [ID]: 2, [REG]: { decimals: 4, ticker: '<script>\u0000' } } }));
    expect(displayToken({ byCovenantId: new Map() }, 'testnet-10', ID, s)).toMatchObject({ decimals: 2, ticker: '83478d88…6daacd64', decimalsSource: 'issued' });
    expect(displayToken({ byCovenantId: new Map() }, 'testnet-10', REG, s)).toMatchObject({ decimals: 4, ticker: 'abababab…abababab' });
  });
});
