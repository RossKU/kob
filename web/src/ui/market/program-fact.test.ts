import { describe, expect, it } from 'vitest';
import { TOKEN_PROGRAMS } from '../../kob/programs.generated';
import { has } from '../../i18n';
import { programFact, programFactText } from './ProgramNote';
import type { TokenRow } from './token-model';

const ref = TOKEN_PROGRAMS.find((p) => p.name === 'KCC20Ref')!;
const kaspacom = TOKEN_PROGRAMS.find((p) => p.name === 'KCC20KaspaCom_0_2_5')!;
const index = (hash: string | null, id: string | null = null) => ({ template_hash: hash, template_id: id }) as unknown as TokenRow['index'];
const info = (program: string | null, matches: boolean, openList = false) =>
  ({ program, family: 'kcc20', templateId: 'kcc20-x', templateHash: '', templateMatchesPinned: matches, openList }) as unknown as TokenRow['info'];
const row = (over: Partial<Pick<TokenRow, 'info' | 'index' | 'genesis' | 'source'>>): Pick<TokenRow, 'info' | 'index' | 'genesis' | 'source'> => ({ info: null, index: null, source: 'indexer', ...over });

describe('the Program row', () => {
  it('has a name for every program of the table', () => {
    for (const p of TOKEN_PROGRAMS) expect(has(`market.program.name.${p.name}`), p.name).toBe(true);
  });

  it('a created-on-KOB token (open list, KOB reference program) after the genesis check: checked from its genesis', () => {
    const genesis = { status: 'verified', source: 'kascov', txid: 'a', program: 'KCC20Ref', supply: 1n, mintAllowance: null, mintersUnspent: null } as const;
    expect(programFactText(row({ index: index(ref.templateHash, 'kcc20-ref-3x3'), genesis }))).toBe('KCC-20 (KOB reference, 3x3) · checked from its genesis');
  });

  it('a token named only by its template hash, genesis not checked (or unavailable)', () => {
    expect(programFactText(row({ index: index(ref.templateHash) }))).toBe('KCC-20 (KOB reference, 3x3) · template matched, genesis not checked');
    expect(programFactText(row({ index: index(ref.templateHash), genesis: { status: 'unavailable', reason: 'unreachable' } }))).toBe('KCC-20 (KOB reference, 3x3) · template matched, genesis not checked');
  });

  it('a token on the default issuance program (the published public-mint app) is named as that app, not as a public mint', () => {
    const pm = TOKEN_PROGRAMS.find((p) => p.name === 'KCC20PublicMint')!;
    const genesis = { status: 'verified', source: 'kascov', txid: 'a', program: 'KCC20PublicMint', supply: 1n, mintAllowance: null, mintersUnspent: null } as const;
    expect(programFactText(row({ index: index(pm.templateHash, 'kcc20-ref-public-mint'), genesis }))).toBe('KCC-20 (published reference app, 3x3) · checked from its genesis');
  });

  it('a failed genesis check is stated as a fact, no more', () => {
    expect(programFact(row({ index: index(ref.templateHash), genesis: { status: 'failed', code: 'mismatch', message: 'x' } })).basis).toBe('genesisFailed');
  });

  it('a registry token: template matched, or differs from the pinned program', () => {
    expect(programFactText(row({ source: 'registry', info: info('KCC20KaspaCom_0_2_5', true), index: index(kaspacom.templateHash) }))).toBe('KCC-20 (KaspaCom 0.2.5) · template matched');
    expect(programFact(row({ source: 'registry', info: info('KCC20KaspaCom_0_2_5', false) })).basis).toBe('differs');
  });

  it('a registry template this build has no program for names its family and template id; nothing known: unknown program', () => {
    expect(programFactText(row({ source: 'registry', info: info(null, false) }))).toBe('KCC-20 (kcc20-x) · template differs from the pinned program');
    expect(programFactText(row({}))).toBe('unknown program');
    expect(programFactText(row({ index: index('ab'.repeat(32)) }))).toBe('unknown program');
  });
});
