import { describe, expect, it } from 'vitest';
import { claimSessionRun } from './session-run';

describe('one merge run under a wallet session at a time', () => {
  it('a second run is declined until the first releases; a release twice frees nothing else', () => {
    const first = claimSessionRun();
    expect(first).not.toBeNull();
    expect(claimSessionRun()).toBeNull();
    first!();
    const second = claimSessionRun();
    expect(second).not.toBeNull();
    first!(); // stale: does not free the second run's claim
    expect(claimSessionRun()).toBeNull();
    second!();
  });
});
