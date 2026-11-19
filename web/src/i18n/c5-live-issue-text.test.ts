// Tests that a planner finding without params still renders its message in the orders UI.
//
// The orders UI renders findings with `tIssue('orders.issue', issue)` (i18n/index.ts), which
// passes ONLY `issue.params` to the sentence. `cancel.build-failed` (cancel.ts
// `issue('cancel.build-failed', 'error', msg)`) carries the builder's refusal in `message` and
// has NO params. The sentence 'The transaction could not be built: {message}' must not show the
// literal "{message}". Every kob-wasm rejection of a cancel, refund or amend (custody mismatch,
// unknown token program, storage mass, ...) would otherwise reach the user as an unexplained dead
// end. So the orders flow formats findings with issueText() (issue-text.ts), or tIssue fills
// {message} from issue.message when no params are given.
import { describe, expect, it } from 'vitest';
import { tIssue } from './index';

describe('C5-I1: a refused cancel / refund names the reason', () => {
  it("cancel.build-failed shows the builder's message, not a raw placeholder", () => {
    const text = tIssue('orders.issue', { code: 'cancel.build-failed', message: 'token-holding order: custody token UTXO required' });
    expect(text).toContain('custody token UTXO required');
    expect(text).not.toContain('{message}');
  });
});
