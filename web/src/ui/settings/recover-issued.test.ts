import { describe, expect, it } from 'vitest';
import { parseDecimalsInput, recoverIssuedToken, type RecoverDeps } from './recover-issued';
import type { DeclaredToken } from '../../data/token-declarations';

const COV = '77'.repeat(32);
const PK = '11'.repeat(32);

function deps(decimals: number | null, found: number, calls: unknown[] = []): RecoverDeps {
  const decl = decimals === null ? null : ({ covenantId: COV, decimals } as DeclaredToken);
  return {
    kob: {} as never,
    declarations: null,
    network: 'testnet-10',
    declaration: async () => decl,
    tracker: { recoverIssued: async (...a: unknown[]) => (calls.push(a), Array.from({ length: found }, (_, i) => ({ index: i }) as never)) },
  };
}

describe('recoverIssuedToken (Settings: find a token you created earlier)', () => {
  it('turns the amount shown in whole tokens into the token\'s units by its declared decimals', async () => {
    const calls: unknown[] = [];
    const r = await recoverIssuedToken(deps(8, 1, calls), PK, ` ${COV.toUpperCase()} `, '1000', '');
    expect(r.kind).toBe('found');
    expect(calls).toEqual([[PK, { covenantId: COV }, [100_000_000_000n]]]);
  });

  it('refuses a malformed id, an amount that is not one of the token or not positive, and asks for decimals only when none are declared', async () => {
    const d = deps(2, 0);
    expect(await recoverIssuedToken(d, PK, 'nope', '1')).toEqual({ kind: 'error', code: 'id' });
    for (const bad of ['', '0', '-1', '1.234', 'abc']) expect(await recoverIssuedToken(d, PK, COV, bad), bad).toEqual({ kind: 'error', code: 'amount' });
    expect(await recoverIssuedToken(d, PK, COV, '5')).toEqual({ kind: 'error', code: 'none' });
    const undeclared = deps(null, 1);
    expect(await recoverIssuedToken(undeclared, PK, COV, '5')).toEqual({ kind: 'error', code: 'decimals' });
    expect(await recoverIssuedToken(undeclared, PK, COV, '5', '19')).toEqual({ kind: 'error', code: 'decimals' });
    expect((await recoverIssuedToken(undeclared, PK, COV, '5', '6')).kind).toBe('found');
  });

  it('parses typed decimals', () => {
    expect([' 8 ', '0', '18', '19', '-1', '1.5', ''].map(parseDecimalsInput)).toEqual([8, 0, 18, null, null, null, null]);
  });
});
