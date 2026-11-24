// The token page's creator-lock line (R9B-L5, R9FA2-3): a lock of another class is not this market's
// lock, and a lock whose date has passed locks nothing.
import { describe, expect, it } from 'vitest';
import { creatorLockText } from './TokenHeader';
import type { VerifiedLock } from '../../kob/creator-lock';

const EXT = 'aa'.repeat(32);
const lock = (o: Partial<VerifiedLock> = {}): VerifiedLock => ({
  kind: 'manual', until: String(Date.now() + 86_400_000 * 30), key: '11'.repeat(32), script: '', authority: '22'.repeat(32),
  amount: '400000', outputs: [], program: 'KCC20Ref', extensionCommitment: EXT, supply: '1000000', ...o,
} as VerifiedLock);

describe('the creator-lock line', () => {
  it('names the amount and share for the market\'s class', () => {
    expect(creatorLockText(lock(), 0, 'LOCKD', EXT)).toMatch(/400,000 LOCKD \(40% of supply\) until/);
  });
  it('a lock of another class says so, without an amount in this market\'s ticker', () => {
    const s = creatorLockText(lock(), 0, 'LOCKD', 'bb'.repeat(32));
    expect(s).toMatch(/another class/);
    expect(s).not.toMatch(/LOCKD/);
    expect(creatorLockText(lock({ otherClass: true } as Partial<VerifiedLock>), 0, 'LOCKD', 'bb'.repeat(32))).toMatch(/more than one class/);
  });
  it('a lock whose date has passed is not shown as a lock', () => {
    const s = creatorLockText(lock({ until: String(Date.now() - 60_000) }), 0, 'LOCKD', EXT);
    expect(s).toMatch(/^none now/);
    expect(s).toMatch(/not locked/);
  });
});
