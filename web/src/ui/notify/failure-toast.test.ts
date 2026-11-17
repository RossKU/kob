import { describe, expect, it } from 'vitest';
import { failureText, failureToast, isFailure } from './failure-toast';

describe('failure toast', () => {
  it('a stop the user did not choose raises one: blocked, declined in the wallet, refused by the node', () => {
    expect(failureToast('Order not sent', { phase: 'blocked', code: 'terms', text: 'The market moved past the price you set.' })).toBe('Order not sent: The market moved past the price you set.');
    expect(failureToast('Order not sent', { phase: 'rejected', code: 'rejected', text: 'You declined the request in your wallet. Nothing was sent.' })).toBe(
      'Order not sent: You declined the request in your wallet. Nothing was sent.',
    );
    expect(failureToast('Fill from the book stopped', { phase: 'failed', code: 'orphan', text: 'The node does not know an input.' })).toBe('Fill from the book stopped: The node does not know an input.');
  });

  it('the app\'s own Cancel, progress and a submitted send raise none', () => {
    expect(failureToast('Order not sent', { phase: 'rejected', code: 'cancelled', text: 'Cancelled. Nothing was sent.' })).toBeNull();
    for (const phase of ['checking', 'confirming', 'signing', 'sending', 'submitted']) expect(failureToast('x', { phase })).toBeNull();
    expect(failureToast('x', null)).toBeNull();
    expect(isFailure(undefined)).toBe(false);
  });

  it('one short line: spaces folded, a missing reason falls back, else the heading alone', () => {
    expect(failureText('Order not sent', '  The node\n refused   it. ')).toBe('Order not sent: The node refused it.');
    expect(failureText('Order not sent', '')).toBe('Order not sent');
    expect(failureToast('Order not sent', { phase: 'failed' }, 'Signing or sending failed.')).toBe('Order not sent: Signing or sending failed.');
  });
});
