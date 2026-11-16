import { describe, expect, it } from 'vitest';
import { currentStep, stepGlyph, stepperLayout } from '../kit/stepper-model';
import { flowStepperItems } from './flow-steps';

const titles = ['Merge 1 of 3', 'Merge 2 of 3', 'Merge 3 of 3'];
const pick = (xs: { state: string }[]) => xs.map((x) => x.state);

describe('stepper model', () => {
  it('a short run is a row, a long one a column', () => {
    expect(stepperLayout(2)).toBe('row');
    expect(stepperLayout(3)).toBe('row');
    expect(stepperLayout(4)).toBe('column');
  });

  it('the circle shows a tick, a cross or the number', () => {
    expect(stepGlyph({ state: 'done' }, 0)).toBe('✓');
    expect(stepGlyph({ state: 'failed' }, 1)).toBe('✗');
    expect(stepGlyph({ state: 'waiting' }, 2)).toBe('3');
  });
});

describe('flowStepperItems (TxFlow)', () => {
  it('maps every flow state, and keeps the flow\'s own state and test id', () => {
    const items = flowStepperItems({ titles, states: ['submitted', 'sending', 'pending'], phase: 'signing' });
    expect(pick(items)).toEqual(['done', 'current', 'waiting']);
    expect(items[1]).toMatchObject({ label: 'Merge 2 of 3', busy: true, note: 'Sign in your wallet', testid: 'flow-step-1', data: 'sending' });
    expect(currentStep(items)).toBe(1);
  });

  it('the running step says what it waits for', () => {
    const note = (o: Parameters<typeof flowStepperItems>[0]) => flowStepperItems(o)[0]!.note;
    expect(note({ titles, states: ['sending'], phase: 'checking' })).toBe('Checking');
    expect(note({ titles, states: ['sending'], phase: 'sending' })).toBe('Sending');
    expect(note({ titles, states: ['sending'], phase: 'signing', waiting: true })).toBe('Waiting for your wallet');
    expect(note({ titles, states: ['preparing'] })).toBe('Preparing');
  });

  it('a declined or failed step is failed, the later ones still wait', () => {
    const items = flowStepperItems({ titles, states: ['submitted', 'cancelled', 'pending'] });
    expect(pick(items)).toEqual(['done', 'failed', 'waiting']);
    expect(items[1]).toMatchObject({ note: 'Declined', data: 'cancelled' });
    expect(flowStepperItems({ titles, states: ['failed', 'pending', 'pending'] })[0]!.note).toBe('Failed');
    expect(currentStep(items)).toBe(-1);
  });

  it('a private-key session is approved first: no step runs before it', () => {
    const asking = flowStepperItems({ titles, states: ['sending', 'pending', 'pending'], session: 'asking' });
    expect(pick(asking)).toEqual(['current', 'waiting', 'waiting', 'waiting']);
    expect(asking[0]).toMatchObject({ label: 'Approve', testid: 'flow-session-step', busy: true });
    const open = flowStepperItems({ titles, states: ['submitted', 'sending', 'pending'], session: 'open', phase: 'signing' });
    expect(pick(open)).toEqual(['done', 'done', 'current', 'waiting']);
    expect(open[2]!.note).toBe('Signing');
    // declined in the signer: the approval failed, no step started
    const declined = flowStepperItems({ titles, states: ['cancelled', 'pending', 'pending'], session: 'declined' });
    expect(pick(declined)).toEqual(['failed', 'waiting', 'waiting', 'waiting']);
    expect(declined[0]!.note).toBe('Declined');
    expect(declined[1]!.data).toBe('cancelled');
  });

  it('Merge, then sell: the sale waits until every merge went through, then is current', () => {
    const then = { label: 'Sell', note: 'Confirm the sale' };
    const running = flowStepperItems({ titles: titles.slice(0, 2), states: ['submitted', 'sending'], then });
    expect(pick(running)).toEqual(['done', 'current', 'waiting']);
    expect(running[2]).toMatchObject({ label: 'Sell', testid: 'flow-then-step' });
    const through = flowStepperItems({ titles: titles.slice(0, 2), states: ['submitted', 'submitted'], then });
    expect(pick(through)).toEqual(['done', 'done', 'current']);
    expect(through[2]!.note).toBe('Confirm the sale');
    const stopped = flowStepperItems({ titles: titles.slice(0, 2), states: ['submitted', 'cancelled'], then });
    expect(pick(stopped)).toEqual(['done', 'failed', 'waiting']);
  });
});
