import { describe, expect, it } from 'vitest';
import { currentStep } from '../kit/stepper-model';
import { creationStepperItems, pendingCreationItems, type PendingCreation } from './creation-steps';

const pick = (xs: { state: string }[]) => xs.map((x) => x.state);

describe('creationStepperItems (the confirmation screen)', () => {
  it('Prepare, then Create token, as the chain runner moves', () => {
    const at = (o: Partial<Parameters<typeof creationStepperItems>[0]>) => creationStepperItems({ stage: null, step: null, prepSent: false, ...o });
    expect(pick(at({}))).toEqual(['waiting', 'waiting']);
    const prep = at({ stage: 'signing', step: 'prep' });
    expect(pick(prep)).toEqual(['current', 'waiting']);
    expect(prep[0]).toMatchObject({ label: 'Prepare', note: 'Sign in your wallet', busy: true, testid: 'create-step-prep' });
    expect(at({ stage: 'submitting', step: 'prep' })[0]!.note).toBe('Sending');
    // the preparation went out: the genesis waits for the wallet's node, then is signed
    const wait = at({ stage: 'signing', step: 'genesis', waiting: true, prepSent: true });
    expect(pick(wait)).toEqual(['done', 'current']);
    expect(wait[1]).toMatchObject({ label: 'Create token', note: 'Waiting for your wallet', testid: 'create-step-genesis' });
    expect(at({ stage: 'signing', step: 'genesis', prepSent: true })[1]!.note).toBe('Sign in your wallet');
    expect(at({ stage: 'validating', step: 'genesis', prepSent: true })[1]!.note).toBe('Checking');
    const done = at({ stage: 'submitted', step: 'genesis', prepSent: true });
    expect(pick(done)).toEqual(['done', 'done']);
    expect(currentStep(done)).toBe(-1);
  });

  it('one approval (the private-key signer) signs both at the first step', () => {
    const items = creationStepperItems({ stage: 'signing', step: 'prep', prepSent: false, oneApproval: true });
    expect(pick(items)).toEqual(['current', 'waiting']);
    expect(items[0]!.note).toBe('Sign both in your wallet');
    // both signed, the preparation sent, the genesis being sent
    expect(pick(creationStepperItems({ stage: 'submitting', step: 'genesis', prepSent: true, oneApproval: true }))).toEqual(['done', 'current']);
  });

  it('a declined first approval marks Prepare failed', () => {
    const items = creationStepperItems({ stage: null, step: 'prep', prepSent: false, stopped: 'declined' });
    expect(pick(items)).toEqual(['failed', 'waiting']);
    expect(items[0]!.note).toBe('Declined');
  });
});

describe('pendingCreationItems (Finish the creation)', () => {
  const rec = (o: Partial<PendingCreation>): PendingCreation => ({ prepSent: true, signed: false, resuming: false, note: null, failed: false, ...o });

  it('a record whose first step went out shows Create token as the step left, after a reload too', () => {
    const items = pendingCreationItems(rec({}));
    expect(pick(items)).toEqual(['done', 'current']);
    expect(items[1]).toMatchObject({ note: 'Not signed yet' });
    expect(items[1]!.busy).toBeUndefined();
    expect(pendingCreationItems(rec({ signed: true }))[1]!.note).toBe('Signed, not sent');
  });

  it('a record whose first step may not have gone out starts at Prepare', () => {
    const items = pendingCreationItems(rec({ prepSent: false }));
    expect(pick(items)).toEqual(['current', 'waiting']);
    expect(items[0]!.note).toBe('Not sent yet');
    // finishing: the first step is sent again, then the wallet is asked
    expect(pendingCreationItems(rec({ prepSent: false, resuming: true }))[0]).toMatchObject({ state: 'current', busy: true, note: 'Sending' });
    expect(pick(pendingCreationItems(rec({ prepSent: false, resuming: true, note: 'genesis' })))).toEqual(['done', 'current']);
  });

  it('finishing says what the wallet is asked for; a failed attempt marks the step', () => {
    expect(pendingCreationItems(rec({ resuming: true, note: 'wait' }))[1]).toMatchObject({ busy: true, note: 'Waiting for your wallet' });
    expect(pendingCreationItems(rec({ resuming: true, note: 'genesis' }))[1]!.note).toBe('Sign in your wallet');
    expect(pendingCreationItems(rec({ resuming: true, signed: true }))[1]!.note).toBe('Sending');
    expect(pick(pendingCreationItems(rec({ failed: true })))).toEqual(['done', 'failed']);
  });
});
