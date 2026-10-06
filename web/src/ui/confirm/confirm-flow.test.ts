import { describe, expect, it } from 'vitest';
import { NodeError } from '../../data/node-error';
import { SignFlowError } from '../../wallet/sign';
import { WalletError } from '../../wallet/types';
import { STAGES, classifyFailure, explorerTxUrl, stepStates } from './confirm-flow';

const flow = (stage: ConstructorParameters<typeof SignFlowError>[0], cause: unknown, message: string, code: string) => new SignFlowError(stage, cause, message, code);

describe('stage stepper', () => {
  it('marks done, active and pending steps', () => {
    expect(stepStates(null).map((s) => s.state)).toEqual(['pending', 'pending', 'pending', 'pending']);
    expect(stepStates('finalizing').map((s) => s.state)).toEqual(['done', 'active', 'pending', 'pending']);
    expect(stepStates('submitting').map((s) => s.state)).toEqual(['done', 'done', 'done', 'active']);
    expect(stepStates('submitted').map((s) => s.state)).toEqual(['done', 'done', 'done', 'done']);
    expect(stepStates('signing').map((s) => s.stage)).toEqual([...STAGES]);
  });
});

describe('failure classification', () => {
  it('a wallet rejection is neutral', () => {
    const e = flow('signing', new WalletError('rejected', 'User rejected'), 'User rejected', 'rejected');
    const f = classifyFailure(e);
    expect(f.kind).toBe('rejected');
    expect(f.text).toMatch(/declined/i);
  });

  it('a wallet timeout or a bad signature may be retried with the same transaction', () => {
    expect(classifyFailure(flow('signing', new WalletError('timeout', 'x'), 'x', 'timeout')).kind).toBe('retry');
    expect(classifyFailure(flow('finalizing', new Error('x'), 'x', 'signature')).kind).toBe('retry');
    expect(classifyFailure(flow('submitting', new NodeError('unavailable', 'down'), 'down', 'unavailable')).kind).toBe('retry');
  });

  it('changed inputs and invalid transactions need a re-plan', () => {
    for (const code of ['orphan', 'double-spend', 'fee', 'script', 'invalid']) {
      expect(classifyFailure(flow('submitting', new NodeError(code as 'orphan', 'x'), 'x', code)).kind, code).toBe('replan');
    }
    expect(classifyFailure(flow('validating', new Error('x'), 'x', 'validation')).kind).toBe('replan');
  });

  it('a transaction the node already has counts as submitted', () => {
    expect(classifyFailure(flow('submitting', new NodeError('already-known', 'x'), 'x', 'already-known')).kind).toBe('known');
  });

  // C5 R-3: the raw text of an unknown failure is kept for "Details", the sentence is translated (this test pinned the raw text as the sentence)
  it('unknown errors become a retryable, translated message with the raw text behind Details', () => {
    const f = classifyFailure(new Error('boom'));
    expect(f).toMatchObject({ kind: 'retry', raw: 'boom' });
    expect(f.text).toMatch(/unexpected reason/);
    expect(classifyFailure('plain string').raw).toBe('plain string');
    expect(classifyFailure('plain string').text).not.toContain('plain string');
  });
});

describe('explorer links', () => {
  const id = 'ab'.repeat(32);
  it('knows the two networks and refuses malformed ids', () => {
    expect(explorerTxUrl('mainnet', id)).toBe(`https://kaspa.stream/transactions/${id}`);
    expect(explorerTxUrl('testnet-10', id)).toBe(`https://tn10.kaspa.stream/transactions/${id}`);
    expect(explorerTxUrl('devnet', id)).toBeNull();
    expect(explorerTxUrl('mainnet', 'xyz')).toBeNull();
    expect(explorerTxUrl('testnet-10', id, 'https://explorer.example')).toBe(`https://explorer.example/transactions/${id}`);
  });
});
