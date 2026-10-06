// Every amendment is verified end to end: ReplacementSpec -> planCancelReplace (kob-wasm builder) -> sign locally -> finalize -> consensus validate
// (the script engine runs the covenants) -> the pre-sign decoder has nothing blocking.
import { describe, expect, it } from 'vitest';
import { planCancelReplace, type CancelEnv, type CancelPlan, type OrderSnapshot } from '../../kob/cancel';
import { decodeSigning } from '../../kob/decode';
import { parseRegistry } from '../../kob/registry';
import type { CondAskState, CondBidState, AskState, BidState } from '../../kob/types';
import { loadKobNode } from '../../kob/wasm.node';
import { MAKER, keyUtxo, placeGolden, signAndValidate, tokenUtxo, tradableRegistryJson, nodeFactsOf } from '../../testing/chain-fixtures';
import { MAX_IDLE_DAA } from '../../kob/daa';
import { buildReplacement } from './amend';

const kob = loadKobNode();
const registry = parseRegistry(tradableRegistryJson(), { kob });
const KAS = 100_000_000n;

const env = (over: Partial<CancelEnv> = {}): CancelEnv => ({
  kob, maker: MAKER.pk, funding: [keyUtxo(MAKER.pk, 100n * KAS, 201)], tokenUtxos: [], clock: { daa: 1000n }, ...over,
});
const snapOf = (name: string): OrderSnapshot => placeGolden(kob, name).snapshots[0];

function verify(plan: CancelPlan, label: string) {
  expect(plan.issues.filter((i) => i.severity === 'error'), label).toEqual([]);
  expect(plan.ok, label).toBe(true);
  signAndValidate(kob, plan.built!, [MAKER.sk]);
  const s = decodeSigning({ kob, built: plan.built!, maker: MAKER.pk, registry, expected: plan.expected, nodeInputs: nodeFactsOf(plan.built!) });
  expect(s.blocking, label).toEqual([]);
}

describe('amend a plain limit sell (KobAsk)', () => {
  const snap = snapOf('create.ask');
  const s = snap.order.state.state as AskState;
  const price = BigInt(s.price);

  // C5 W-3: a GTC ends 90 days after placement whatever its fills; renewing gives the new order a fresh on-chain expiry
  it('a renewal alone sets expiryDaa = now + 90 days, keeps every other term and is consensus valid', () => {
    const r = buildReplacement(snap, 'limit', { renew: true }, { nowDaa: 5000n });
    expect(r.ok).toBe(true);
    const next = r.spec!.order.state as AskState;
    expect(next.expiryDaa).toBe((5000n + MAX_IDLE_DAA).toString());
    expect({ ...next, expiryDaa: s.expiryDaa }).toEqual(s);
    verify(planCancelReplace(env(), snap, r.spec!), 'renew');
    expect(buildReplacement(snap, 'limit', { renew: true }).issues.map((i) => i.code)).toContain('renew-no-clock');
  });

  it('a price change (sompi per whole token) is consensus valid and keeps every other term', () => {
    const r = buildReplacement(snap, 'limit', { price: price + 100n });
    expect(r.ok).toBe(true);
    const next = r.spec!.order.state as AskState;
    expect(BigInt(next.price)).toBe(price + 100n);
    expect({ ...next, price: s.price }).toEqual(s);
    expect(r.spec!.value).toBe(BigInt(snap.order.amount));
    verify(planCancelReplace(env(), snap, r.spec!), 'price');
  });

  it('a tip change and a smaller amount return the surplus base units', () => {
    const left = BigInt(s.amountLeft);
    const smaller = left - left / 3n;
    const r = buildReplacement(snap, 'limit', { tip: 200_000n, amount: smaller });
    expect(r.ok).toBe(true);
    expect(r.next).toMatchObject({ amount: smaller, tip: 200_000n });
    const plan = planCancelReplace(env(), snap, r.spec!);
    verify(plan, 'amount down');
    expect(plan.tokensReturned).toBe(left - smaller);
  });

  it('an amount below the minimum fill lowers the minimum fill to it (a valid order); one base unit is enough', () => {
    const r = buildReplacement(snap, 'limit', { amount: 1n });
    expect(r.ok).toBe(true);
    const next = r.spec!.order.state as AskState;
    expect(next.amountLeft).toBe('1');
    expect(BigInt(next.minFill)).toBeLessThanOrEqual(1n);
    verify(planCancelReplace(env(), snap, r.spec!), 'one base unit');
  });

  it('a larger amount is topped up from free tokens (and refused when there are none)', () => {
    const more = BigInt(s.amountLeft) + 3n;
    const r = buildReplacement(snap, 'limit', { amount: more });
    const withTokens = planCancelReplace(env({ tokenUtxos: [tokenUtxo(3n, MAKER.pk, 151)] }), snap, r.spec!);
    verify(withTokens, 'amount up');
    expect(planCancelReplace(env(), snap, r.spec!).ok).toBe(false);
  });

  it('rejects bad input with a coded, field-tagged issue instead of building', () => {
    const codes = (i: Parameters<typeof buildReplacement>[2], tick?: bigint) => buildReplacement(snap, 'limit', i, { tick }).issues.map((x) => `${x.field}:${x.code}`);
    expect(codes({ price: 0n })).toEqual(['price:price-not-positive']);
    expect(codes({ price: price + 1n }, 100n)).toEqual(['price:price-off-tick']);
    // any whole sompi per token is a valid price without a tick (the state price is per whole token)
    expect(codes({ price: price + 1n })).toEqual([]);
    expect(codes({ amount: 0n })).toEqual(['amount:amount-not-positive']);
    expect(codes({ tip: -1n })).toEqual(['tip:tip-negative']);
    expect(codes({ tip: price })).toEqual(['tip:tip-exceeds-price']);
    expect(buildReplacement(snap, 'limit', { price: 0n }).spec).toBeNull();
  });

  it('a day order keeps its deadline through the amendment', () => {
    const day = snapOf('create.ask.day');
    expect(day.deadline).not.toBeNull();
    const r = buildReplacement(day, 'limit', { tip: 1n });
    expect(r.spec!.deadline).toBe(day.deadline);
    verify(planCancelReplace(env(), day, r.spec!), 'day');
  });
});

describe('amend a plain limit buy (KobBid)', () => {
  const snap = snapOf('create.bid');
  const s = snap.order.state.state as BidState;

  it('keeps the KAS budget: price and tip change, the value does not, the amount follows from the price', () => {
    const r = buildReplacement(snap, 'limit', { price: BigInt(s.price) - 100n, tip: 50_000n, amount: 99n });
    expect(r.ok).toBe(true);
    expect(r.spec!.value).toBe(BigInt(snap.order.amount));
    expect(r.issues.map((i) => i.code)).toEqual(expect.arrayContaining(['bid-amount-fixed', 'bid-budget-kept']));
    verify(planCancelReplace(env(), snap, r.spec!), 'bid');
  });
});

describe('amend a partly filled buy (KobBid): add KAS', () => {
  const snap = snapOf('create.bid');
  const s = snap.order.state.state as BidState;
  // kob-protocol min_order_value of a bid: the escrow of ONE minimum fill
  const minValue = kob.bidEscrow(snap.order.state, s.minFill, 1)!;
  /** the same bid after fills left less than one minimum fill in its escrow */
  const drained = (amount: bigint): OrderSnapshot => ({ ...snap, order: { ...snap.order, amount: amount.toString() } });

  it('a top-up raises the escrow, is funded from the wallet in the same transaction and is consensus valid', () => {
    const add = 7n * KAS;
    const r = buildReplacement(snap, 'limit', { addKas: add });
    expect(r.ok).toBe(true);
    expect(r.spec!.value).toBe(BigInt(snap.order.amount) + add);
    expect(r.issues.map((i) => i.code)).toContain('bid-budget-topped-up');
    expect(r.next!.value).toBe(r.spec!.value);
    expect(r.next!.maxAmount).toBeGreaterThan(0n);
    // the bigint rule and kob-wasm agree on the buying power
    expect(buildReplacement(snap, 'limit', { addKas: add }, { kob }).next!.maxAmount).toBe(r.next!.maxAmount);
    const plan = planCancelReplace(env(), snap, r.spec!);
    verify(plan, 'top-up');
    expect(plan.fundingUsed).toHaveLength(1);
    expect((plan.built!.tx.outputs[0] as { value: string | number | bigint }).value.toString()).toBe(r.spec!.value.toString());
  });

  it('a top-up combines with a price change', () => {
    const r = buildReplacement(snap, 'limit', { price: BigInt(s.price) - 100n, addKas: 3n * KAS });
    expect(r.ok).toBe(true);
    verify(planCancelReplace(env(), snap, r.spec!), 'top-up + price');
  });

  it('a bid drained below one minimum fill is refused with the missing amount; a top-up above the minimum revives it', () => {
    const low = drained(minValue - 1n);
    for (const o of [{}, { kob }]) {
      const refused = buildReplacement(low, 'limit', { tip: BigInt(s.tip) }, o);
      expect(refused.ok).toBe(false);
      expect(refused.spec).toBeNull();
      expect(refused.issues.map((i) => `${i.field}:${i.code}`)).toEqual(['addKas:bid-below-min-fill']);
    }
    // the kob-wasm builder refuses the same replacement (kob-protocol `min_order_value`): the check above turns that into an actionable message
    const raw = planCancelReplace(env(), low, { order: low.order.state, value: BigInt(low.order.amount) });
    expect(raw.ok).toBe(false);
    // exactly the minimum is accepted
    expect(buildReplacement(drained(minValue), 'limit', {}).ok).toBe(true);
    const ok = buildReplacement(low, 'limit', { addKas: 2n * KAS });
    expect(ok.ok).toBe(true);
    expect(ok.next!.maxAmount).toBeGreaterThanOrEqual(BigInt(s.minFill));
    verify(planCancelReplace(env(), low, ok.spec!), 'revived');
  });

  it('a top-up the wallet cannot pay is a plan failure, not a crash', () => {
    const r = buildReplacement(snap, 'limit', { addKas: 500n * KAS });
    const plan = planCancelReplace(env(), snap, r.spec!);
    expect(plan.ok).toBe(false);
    expect(plan.issues.map((i) => i.code)).toContain('cancel.insufficient-funds');
  });

  it('refuses a negative top-up and a top-up on a sell', () => {
    expect(buildReplacement(snap, 'limit', { addKas: -1n }).issues.map((i) => i.code)).toEqual(['add-kas-negative']);
    expect(buildReplacement(snapOf('create.ask'), 'limit', { addKas: KAS }).issues.map((i) => i.code)).toEqual(['add-kas-bid-only']);
  });
});

describe('amend a conditional order (take-profit / stop / OCO)', () => {
  it('sell side: moving the legs is consensus valid; the leg order is enforced', () => {
    const snap = snapOf('create.condAsk');
    const s = snap.order.state.state as CondAskState;
    const tp = BigInt(s.tpPrice);
    const stop = BigInt(s.stopPrice);
    const ok = buildReplacement(snap, 'cond', { price: tp + 100n, stop: stop - 100n });
    expect(ok.ok).toBe(true);
    const next = ok.spec!.order.state as CondAskState;
    expect(BigInt(next.tpPrice)).toBe(BigInt(s.tpPrice) + 100n);
    expect(BigInt(next.stopPrice)).toBe(BigInt(s.stopPrice) - 100n);
    expect(next.slipBps).toBe(s.slipBps); // the band stays as it was
    verify(planCancelReplace(env(), snap, ok.spec!), 'cond ask');
    const bad = buildReplacement(snap, 'cond', { price: stop - 1n });
    expect(bad.issues.map((i) => i.code)).toEqual(['legs-order']);
  });

  it('buy side: the escrow follows the new worst price so the order stays fully funded', () => {
    const snap = snapOf('create.condBid');
    const s = snap.order.state.state as CondBidState;
    const stop = BigInt(s.stopPrice);
    const amount = BigInt(s.amountLeft);
    const up = buildReplacement(snap, 'cond', { stop: stop + 200n });
    expect(up.ok).toBe(true);
    // the stop leg is the worst price of this buy: raising it raises the escrow by the spend of the amount at the new worst price (band included)
    expect(up.spec!.value).toBeGreaterThan(BigInt(snap.order.amount));
    verify(planCancelReplace(env(), snap, up.spec!), 'cond bid stop up');
    // with kob-wasm the escrow comes from condBidEscrow: the same value as the bigint rule
    expect(buildReplacement(snap, 'cond', { stop: stop + 200n }, { kob }).spec!.value).toBe(up.spec!.value);
    const tpDown = buildReplacement(snap, 'cond', { price: BigInt(s.tpPrice) - 100n, amount: amount - 1n });
    expect(tpDown.ok).toBe(true);
    verify(planCancelReplace(env(), snap, tpDown.spec!), 'cond bid tp down');
  });

  it('refuses trailing stops, exits of a position and legs the order does not have', () => {
    const cond = snapOf('create.condAsk');
    const s = cond.order.state.state as CondAskState;
    const trailing: OrderSnapshot = { ...cond, order: { ...cond.order, state: { kind: 'KobCondAsk', state: { ...s, trailStep: '100' } } } };
    expect(buildReplacement(trailing, 'cond', {}).issues[0].code).toBe('trailing-not-amendable');
    const exit: OrderSnapshot = { ...cond, order: { ...cond.order, state: { kind: 'KobCondAsk', state: { ...s, parent: 'ab'.repeat(32) } } } };
    expect(buildReplacement(exit, 'cond', {}).issues[0].code).toBe('exit-not-amendable');
    const tpOnly: OrderSnapshot = { ...cond, order: { ...cond.order, state: { kind: 'KobCondAsk', state: { ...s, stopPrice: '0' } } } };
    expect(buildReplacement(tpOnly, 'cond', { stop: 5n }).issues.map((i) => i.code)).toContain('no-stop-leg');
  });

  it('refuses the wrong order kind for the chosen form', () => {
    expect(buildReplacement(snapOf('create.condAsk'), 'limit', {}).issues[0].code).toBe('not-amendable');
    expect(buildReplacement(snapOf('create.ask'), 'cond', {}).issues[0].code).toBe('not-amendable');
  });

  // C5 R-5: moving a stop no longer moves a stop-limit's limit silently: the worst price is shown, and a given limit stays where it is
  it('stop-limit: the worst price is disclosed; with a limit the band is re-derived so the worst price stays at it', () => {
    const snap = snapOf('create.condAsk');
    const s = snap.order.state.state as CondAskState;
    const stop = BigInt(s.stopPrice);
    const worst = (st: bigint, bps: bigint) => st - (st * bps) / 10_000n;
    const moved = buildReplacement(snap, 'cond', { stop: stop - 100n });
    expect(moved.next!.stopWorst).toBe(worst(stop - 100n, BigInt(s.slipBps)));
    const limit = worst(stop, BigInt(s.slipBps));
    const kept = buildReplacement(snap, 'cond', { stop: stop + stop / 100n, stopLimit: limit });
    expect(kept.ok).toBe(true);
    const next = kept.spec!.order.state as CondAskState;
    expect(BigInt(next.slipBps)).toBeGreaterThan(BigInt(s.slipBps));
    expect(kept.next!.stopWorst).toBeGreaterThanOrEqual(limit);
    // never beyond the limit; above it by less than one basis point of the stop (the band is whole basis points)
    expect(kept.next!.stopWorst! - limit).toBeLessThan((stop + stop / 100n) / 10_000n + 1n);
    verify(planCancelReplace(env(), snap, kept.spec!), 'stop-limit');
    expect(buildReplacement(snap, 'cond', { stopLimit: stop + 1n }).issues.map((i) => i.code)).toEqual(['stop-limit-beyond-stop']);
  });
});

