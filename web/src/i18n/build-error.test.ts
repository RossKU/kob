// C5 R-3: builder / node refusals reach the user as plain sentences; the raw developer text only behind "Details".
import { describe, expect, it } from 'vitest';
import { buildErrorText, classifyBuildError, withoutRaw } from './build-error';

describe('classifyBuildError', () => {
  it.each([
    ['kob-wasm build: invalid request: token-holding order: custody token UTXO required', 'custody-missing'],
    ['insufficient funds: need 300000 sompi, have 1000 sompi', 'insufficient-funds'],
    ['not refundable before DAA 77761000', 'not-yet-refundable'],
    ['token inputs mix extension commitments', 'mixed-extension'],
    ['token template ab is not a supported KCC-20 or KRON program', 'unsupported-token'],
    ['token inputs hold 5 < the 10 base units of amountLeft', 'not-enough-tokens'],
    ['minFill must be positive (at least one base unit)', 'invalid-terms'],
    ['scale must be a power of ten (got 3)', 'invalid-terms'],
    ['storage mass 120000 exceeds the standard limit', 'too-large'],
    ['the order is not active yet', 'not-active'],
    ['something nobody planned for', 'other'],
  ])('%s -> %s', (raw, code) => {
    expect(classifyBuildError(raw).code).toBe(code);
  });

  it('never puts the raw text in the sentence', () => {
    const text = buildErrorText('kob-wasm build: invalid request: token-holding order: custody token UTXO required');
    expect(text).not.toContain('custody token UTXO required');
    expect(text.length).toBeGreaterThan(5);
    expect(buildErrorText('not refundable before DAA 42')).toContain('42');
  });

  it('withoutRaw swaps the raw param of the raw-carrying codes only', () => {
    expect(withoutRaw({ code: 'BUILD_REJECTED', message: 'm', params: { reason: 'the order is not active yet' } }).params!.reason).toBe('the order is not active yet');
    expect(withoutRaw({ code: 'cancel.build-failed', message: 'storage mass too big' }).message).toMatch(/too large/);
    const other = { code: 'cancel.not-maker', message: 'm' };
    expect(withoutRaw(other)).toBe(other);
  });
});
