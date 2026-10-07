// The payer's spend authorisation covers every resource of the soak's paywall (the x402 SDK refuses an offer whose merchant asset has
// no `maxAmount` ceiling: `spend_not_authorized`, seen on the 2026-10-01 redeploy).
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { payerMaxAmount, payerMaxPayAmount, soakPrices, TOKEN_CARRIER_SOMPI } from '../src/bots/x402-caps.ts';

const m = { covenantId: '7cfe8aa05f45fbabdd115bcdf2e369e43f0ab59d429c40d17f3f0fa4bbe75652', decimals: 8 };

test('every paid resource is within an explicit ceiling of its merchant asset', () => {
  const caps = payerMaxAmount(m, 20_000_000n);
  for (const [path, p] of Object.entries(soakPrices(m))) {
    const cap = caps[p.asset];
    assert.ok(cap !== undefined, `${path}: no ceiling for ${p.asset}`);
    assert.ok(BigInt(cap) >= p.amount, `${path}: ceiling ${cap} below the price ${p.amount}`);
  }
  assert.equal(soakPrices(m)['/token'].amount, 25_000_000n, 'a quarter of a whole token');
});

test('a swap may sell at least one whole token', () => {
  assert.ok(BigInt(payerMaxPayAmount(m)) >= 100_000_000n);
});

// The SDK holds a payment's whole KAS cost to the KAS ceiling (8e927d4): a token payment's carrier and fee, a native payment's price
// and fee. Under the former 1 KAS ceiling every /token payment (1 KAS carrier + fee) was refused after the 2026-10-08 redeploy.
test('the KAS ceiling covers a token payment carrier and a native price, each with a fee at the cap', () => {
  for (const fee of [20_000_000n, 100_000_000n]) {
    const kas = BigInt(payerMaxAmount(m, fee).KAS);
    assert.ok(kas >= TOKEN_CARRIER_SOMPI + fee, `fee ${fee}: carrier + fee above the ceiling ${kas}`);
    assert.ok(kas >= soakPrices(m)['/native'].amount + fee, `fee ${fee}: native price + fee above the ceiling ${kas}`);
  }
  assert.equal(TOKEN_CARRIER_SOMPI, 100_000_000n, "the paywall's kcc20 carrier");
});
