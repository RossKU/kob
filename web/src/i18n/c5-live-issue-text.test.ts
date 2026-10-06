// C5 liveness (error messages): the orders UI renders planner findings with `tIssue('orders.issue', issue)` (i18n/index.ts), which passes ONLY
// `issue.params` to the sentence. `cancel.build-failed` (cancel.ts `issue('cancel.build-failed', 'error', msg)`) carries the builder's refusal in
// `message` and has NO params, so the sentence 'The transaction could not be built: {message}' is shown with the literal "{message}":
// every kob-wasm rejection of a cancel / refund / amend (custody mismatch, unknown token program, storage mass, ...) reaches the user as an
// unexplained dead end. (issue-text.ts `issueText` fills `message`, but the orders flow does not use it.)
// Uncompiled when written (2026-10-02).
// C5: expected to fail until the orders flow formats findings with issueText() / tIssue fills {message} from issue.message when no params are given.
import { describe, expect, it } from 'vitest';
import { tIssue } from './index';

describe('C5-I1: a refused cancel / refund names the reason', () => {
  it("cancel.build-failed shows the builder's message, not a raw placeholder", () => {
    const text = tIssue('orders.issue', { code: 'cancel.build-failed', message: 'token-holding order: custody token UTXO required' });
    expect(text).toContain('custody token UTXO required');
    expect(text).not.toContain('{message}');
  });
});
