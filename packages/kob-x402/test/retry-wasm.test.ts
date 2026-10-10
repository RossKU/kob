// The SDK's retry table (src/retry.ts) equals the Rust one (`kob_x402::client::retry`, through the wasm build) for every
// diagnostic the Rust verifier and facilitator know, both retryable flags, and the HTTP statuses a paywall answers with.
// Skipped when the wasm build is absent (scripts/build-wasm.sh --test requires it).

import { test } from 'node:test';
import assert from 'node:assert/strict';
import { classifyFailure, classifyStatus, REBUILD_DIAGNOSTICS, RESEND_DIAGNOSTICS } from '../src/retry.ts';
import { realWasm, skipUnlessWasm } from './helpers/real-wasm.ts';

test('the TS retry classification equals the Rust one for every diagnostic and status', { skip: skipUnlessWasm }, () => {
  const w = realWasm()!;
  const diags = w.diagnostics!();
  assert.ok(diags.length >= 50 && diags.includes('order_conflict') && diags.includes('settlement_pending'));
  for (const d of [...RESEND_DIAGNOSTICS, ...REBUILD_DIAGNOSTICS]) assert.ok(diags.includes(d), `${d} is a Rust diagnostic`);
  for (const d of [...diags, 'not_a_diagnostic']) {
    for (const retryable of [false, true]) {
      assert.equal(classifyFailure(d, retryable), w.retryDecision!({ diagnostic: d, retryable }), `${d} retryable=${retryable}`);
      for (const status of [0, 200, 402, 409, 413, 429, 500, 502, 503]) {
        assert.equal(classifyStatus(status, d, retryable), w.retryDecision!({ status, diagnostic: d, retryable }), `${status} ${d} ${retryable}`);
      }
    }
  }
  for (const status of [0, 200, 402, 409, 413, 429, 500, 502, 503]) {
    assert.equal(classifyStatus(status, undefined, false), w.retryDecision!({ status }), `${status} without a diagnostic`);
  }
  assert.equal(classifyFailure(undefined, false), w.retryDecision!({}));
});
