import { describe, expect, it } from 'vitest';
import { withFreeBalances } from './balances';
import { PLAIN_BORROW, borrowKeyOf, groupByBorrow, holderOwnerOf, isKeyOwned, isKronUnspendable, isSpendableHolding, keyState, programOfHash, withBorrowOf } from './token-state';
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

describe('borrow settings (option A)', () => {
  const k = (amount: string, borrow_scheme = 0, borrow_guard = '00'.repeat(32), owner_scheme = 0) => ({
    state: { amount, owner: OWNER, owner_scheme, borrow_scheme, borrow_guard, extension_commitment: '00'.repeat(32) },
  });

  it('a key-owned KCC-20 holding of any borrow setting is spendable; another owner scheme or a KRON minter is not', () => {
    expect(isSpendableHolding(k('5').state)).toBe(true);
    expect(isSpendableHolding(k('5', 2, 'ab'.repeat(32)).state)).toBe(true);
    expect(isSpendableHolding(k('5', 1, 'ab'.repeat(32), 1).state)).toBe(false);
    expect(isSpendableHolding(kron(3), 'KronToken2433')).toBe(true);
    expect(isSpendableHolding({ ...kron(3), is_minter: 1 }, 'KronToken2433')).toBe(false);
  });

  it('borrowKeyOf: one key per (scheme, guard); every plain state and KRON share PLAIN_BORROW', () => {
    expect(borrowKeyOf(k('5').state)).toBe(PLAIN_BORROW);
    expect(borrowKeyOf(kron(3))).toBe(PLAIN_BORROW);
    expect(borrowKeyOf(k('5', 2, 'AB'.repeat(32)).state)).toBe(`2:${'ab'.repeat(32)}`);
    expect(borrowKeyOf(k('5', 2, 'ab'.repeat(32)).state)).not.toBe(borrowKeyOf(k('5', 2, 'cd'.repeat(32)).state));
  });

  it('groupByBorrow: plain first, then by total, largest first; order kept inside a group', () => {
    const a = k('1', 2, 'ab'.repeat(32));
    const b = k('9', 3, 'cd'.repeat(32));
    const c = k('2');
    const d = k('4', 2, 'ab'.repeat(32));
    const e = k('3');
    expect(groupByBorrow([a, b, c, d, e])).toEqual([[c, e], [b], [a, d]]);
    expect(groupByBorrow([])).toEqual([]);
  });

  it('withBorrowOf copies scheme and guard (KCC-20) and leaves KRON alone', () => {
    const from = k('1', 3, '5a'.repeat(32)).state;
    expect(withBorrowOf(k('7').state, from)).toMatchObject({ amount: '7', borrow_scheme: 3, borrow_guard: '5a'.repeat(32) });
    expect(withBorrowOf(kron(3), from)).toEqual(kron(3));
  });
});
