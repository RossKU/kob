import { beforeEach, describe, expect, it } from 'vitest';
import { LOGO_ACTED_KEY, LOGO_SHOWN_KEY, acquiredTokens, actedTokens, logoGate, logoScope, readShown, withholdsLogo } from './logo-gate';

const A = 'a'.repeat(64);
const B = 'b'.repeat(64);
const C = 'c'.repeat(64);
const D = 'd'.repeat(64);

const mem = () => {
  const m = new Map<string, string>();
  return { m, getItem: (k: string) => m.get(k) ?? null, setItem: (k: string, v: string) => void m.set(k, v) };
};

describe('acquired tokens', () => {
  it('own orders (either side, any status), a pair quote and creation records count', () => {
    const set = acquiredTokens([{ token: A }, { token: B, pair: { quote: C } }], [{ covenantId: D.toUpperCase() }]);
    expect([...set].sort()).toEqual([A, B, C, D]);
  });
  it('unknown or malformed ids are not acquired', () => {
    const set = acquiredTokens([{ token: null }, { token: 'zz' }, { token: A, pair: null }], []);
    expect([...set]).toEqual([A]);
    expect(acquiredTokens([], []).size).toBe(0);
  });
});

describe('logo gate', () => {
  beforeEach(() => logoGate.reset());

  it('a token never acquired nor shown is not allowed; an acquired one is', () => {
    expect(logoGate.allowed(A)).toBe(false);
    logoGate.setAcquired('k', new Set([A]));
    expect(logoGate.allowed(A)).toBe(true);
    expect(logoGate.allowed(B)).toBe(false);
  });

  it('another wallet replaces the set', () => {
    logoGate.setAcquired('k1', new Set([A]));
    logoGate.setAcquired('k2', new Set([B]));
    expect(logoGate.allowed(A)).toBe(false);
    expect(logoGate.allowed(B)).toBe(true);
  });

  it('an unchanged set does not notify; a change does', () => {
    let n = 0;
    const off = logoGate.subscribe(() => n++);
    logoGate.setAcquired('k', new Set([A]));
    logoGate.setAcquired('k', new Set([A]));
    expect(n).toBe(1);
    logoGate.setAcquired('k', new Set([A, B]));
    expect(n).toBe(2);
    off();
  });

  it('"Show logo" is remembered per token, in the page and in storage', () => {
    const st = mem();
    logoGate.show(C, st);
    expect(logoGate.allowed(C)).toBe(true);
    expect(logoGate.allowed(D)).toBe(false);
    expect([...readShown(st)]).toEqual([C]);
    expect(st.m.has(LOGO_SHOWN_KEY)).toBe(true);
  });

  it('a store that throws or holds junk still works', () => {
    const bad = { getItem: () => { throw new Error('x'); }, setItem: () => { throw new Error('x'); } };
    expect(readShown(bad).size).toBe(0);
    logoGate.show(A, bad);
    expect(logoGate.allowed(A)).toBe(true);
    expect(readShown({ getItem: () => '{"a":1}', setItem: () => {} }).size).toBe(0);
    expect([...readShown({ getItem: () => JSON.stringify([A, 5, 'x']), setItem: () => {} })]).toEqual([A]);
  });
});

describe('what a wallet view withholds', () => {
  it('an https logo of a token not acquired, in the wallet scope only', () => {
    expect(withholdsLogo('wallet', false, 'https')).toBe(true);
    expect(withholdsLogo('wallet', true, 'https')).toBe(false);
    expect(withholdsLogo(undefined, false, 'https')).toBe(false);
  });
  it('ipfs:// is never withheld', () => {
    expect(withholdsLogo('wallet', false, 'ipfs')).toBe(false);
  });
});

describe('the scope a logo is gated under', () => {
  it('a row only the holdings name is a wallet view wherever it shows; other market rows are not gated', () => {
    expect(logoScope(undefined, 'held')).toBe('wallet');
    expect(logoScope(undefined, 'indexer')).toBeUndefined();
    expect(logoScope(undefined, undefined)).toBeUndefined();
    expect(logoScope('wallet', 'indexer')).toBe('wallet');
  });
});

describe('a token the connected wallet holds (R2B-M1)', () => {
  beforeEach(() => logoGate.reset());

  it('is gated wherever it shows, a listed row included; a token it does not hold is not', () => {
    logoGate.expectHoldings('k', '*');
    expect(logoGate.holding('k', A)).toBe('pending');
    logoGate.noteHoldings('k', '*', [A]);
    expect(logoGate.holding('k', A)).toBe('held');
    expect(logoGate.holding('k', B)).toBe('not-held');
    expect(logoScope(undefined, 'indexer', 'held')).toBe('wallet');
    expect(logoScope(undefined, 'indexer', 'not-held')).toBeUndefined();
    expect(logoScope(undefined, 'held', 'not-held')).toBe('wallet');
  });

  it('a fresh check of one token waits again, whatever an older full read said', () => {
    logoGate.noteHoldings('k', '*', []);
    logoGate.expectHoldings('k', B);
    expect(logoGate.holding('k', B)).toBe('pending');
    logoGate.noteHoldings('k', B, [B]);
    expect(logoGate.holding('k', B)).toBe('held');
  });

  it('once seen held in this visit it stays held; a token never checked is unknown', () => {
    logoGate.noteHoldings('k', '*', [A]);
    logoGate.noteHoldings('k', '*', []);
    expect(logoGate.holding('k', A)).toBe('held');
    logoGate.reset();
    expect(logoGate.holding('k', A)).toBe('unknown');
    expect(logoGate.holding(null, A)).toBe('unknown');
  });

  it('a token in an order naming the key counts as held, and stays so after a holdings read without it (R3B-M1)', () => {
    logoGate.noteHoldings('k', '*', []);
    expect(logoGate.holding('k', A)).toBe('not-held');
    logoGate.noteRelated('k', [A.toUpperCase(), 'junk']);
    expect(logoGate.holding('k', A)).toBe('held');
    logoGate.noteHoldings('k', '*', []);
    expect(logoGate.holding('k', A)).toBe('held');
    expect(logoScope(undefined, 'indexer', logoGate.holding('k', A))).toBe('wallet');
    // another key's orders say nothing of this one
    logoGate.noteRelated('k2', [B]);
    expect(logoGate.holding('k', B)).toBe('unknown');
  });

  it('another wallet starts over', () => {
    logoGate.noteHoldings('k1', '*', [A]);
    expect(logoGate.holding('k2', A)).toBe('unknown');
    logoGate.noteHoldings('k2', '*', []);
    expect(logoGate.holding('k2', A)).toBe('not-held');
  });
});

describe('what the wallet acquired by its own action (R2B-M1)', () => {
  beforeEach(() => logoGate.reset());

  const ref = (covenantId: string) => ({ covenantId });
  it('the token of a take, of a new order (a pair quote too) and of tokens it spent', () => {
    const ids = actedTokens({
      take: { ref: ref(A) },
      orders: [{ description: { tokenCovId: B, pair: { quote: { covId: C } } } }],
      net: { tokens: [{ ref: ref(D), fromWallet: 5n }, { ref: ref('e'.repeat(64)), fromWallet: 0n }] },
    });
    expect([...ids].sort()).toEqual([A, B, C, D]);
  });

  it('a token that only arrived (a second unit, an airdrop beside a purchase) is not acquired', () => {
    expect(actedTokens({ take: { ref: ref(A) }, orders: [], net: { tokens: [{ ref: ref(B), fromWallet: 0n }] } }).has(B)).toBe(false);
    expect(actedTokens({ take: null, orders: [], net: { tokens: [] } }).size).toBe(0);
  });

  it('is remembered in storage, apart from "Show logo", and allows the logo', () => {
    const st = mem();
    logoGate.remember([A], st);
    expect(logoGate.allowed(A)).toBe(true);
    expect([...readShown(st, LOGO_ACTED_KEY)]).toEqual([A]);
    expect(st.m.has(LOGO_SHOWN_KEY)).toBe(false);
  });
});
