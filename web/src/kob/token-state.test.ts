import { describe, expect, it } from 'vitest';
import { withFreeBalances } from './balances';
import { holderOwnerOf, isKeyOwned, isKronUnspendable, keyState, programOfHash } from './token-state';
import { TOKEN_PROGRAMS } from './programs.generated';
import type { Hex, KronState, TokenUtxo } from './types';

const OWNER = '11'.repeat(32) as Hex;
const kron = (id_type: number): KronState => ({ amount: '5', owner: OWNER, id_type, is_minter: 0 });

describe('key-held KRON states follow the program holder owner', () => {
  it('KForge holders are id_type 0, the KRON programs address presence (3)', () => {
    expect(holderOwnerOf('KForgeToken1785', 'kron')).toBe(0);
    expect(holderOwnerOf('KronToken2433', 'kron')).toBe(3);
    expect(holderOwnerOf(null, 'kron')).toBe(3);
    expect(holderOwnerOf(null, 'kcc20')).toBe(0);
  });

  it('a state is spendable by its key under its own program only', () => {
    expect(isKeyOwned(kron(0), 'KForgeToken1785')).toBe(true);
    expect(isKeyOwned(kron(3), 'KForgeToken1785')).toBe(false);
    expect(isKronUnspendable(kron(3), 'KForgeToken1785')).toBe(true);
    expect(isKeyOwned(kron(3), 'KronToken2433')).toBe(true);
    expect(isKeyOwned(kron(0), 'KronToken2433')).toBe(false);
    expect(isKronUnspendable(kron(0), 'KronToken2433')).toBe(true);
    // without a program: the family default (address presence)
    expect(isKeyOwned(kron(3))).toBe(true);
    expect(isKeyOwned(kron(0))).toBe(false);
  });

  it('a key state of KForge is id_type 0', () => {
    expect(keyState('kron', 5n, OWNER, null, 'KForgeToken1785')).toMatchObject({ id_type: 0 });
    expect(keyState('kron', 5n, OWNER, null)).toMatchObject({ id_type: 3 });
  });

  it('the program of a template hash', () => {
    const kf = TOKEN_PROGRAMS.find((p) => p.name === 'KForgeToken1785')!;
    expect(programOfHash(kf.templateHash as Hex)).toBe('KForgeToken1785');
    expect(programOfHash(('00'.repeat(32)) as Hex)).toBeNull();
  });

  it('free balances count KForge id_type 0 as free when the program is known', () => {
    const cov = '38'.repeat(32) as Hex;
    const u: TokenUtxo = { transactionId: 'aa'.repeat(32) as Hex, index: 0, amount: '100000000', blockDaaScore: '0', covenantId: cov, state: kron(0) };
    const [known] = withFreeBalances([], [u], OWNER, () => 'KForgeToken1785');
    expect([known.free, known.notSpendable]).toEqual([5n, 0n]);
    const [unknown] = withFreeBalances([], [u], OWNER);
    expect([unknown.free, unknown.notSpendable]).toEqual([0n, 5n]);
  });
});
