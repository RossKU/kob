// The claim dialog's planning (claim.ts): which wallet UTXOs a merge may take, the ask a claim and sell places, and the self-trade guard.
import { describe, expect, it } from 'vitest';
import type { CancelEnv, OrderSnapshot } from '../../kob/cancel';
import { parseRegistry } from '../../kob/registry';
import type { OrderState } from '../../kob/types';
import { loadKobNode } from '../../kob/wasm.node';
import { MAKER, OTHER, TOKEN, keyUtxo, placeGolden, signAndValidate, strayFor, tokenUtxo, tradableRegistryJson } from '../../testing/chain-fixtures';
import { claimAskSpec, mergeCandidates, planClaimChoice, sellCrossesOwnBid, type ClaimContext } from './claim';

const kob = loadKobNode();
const registry = parseRegistry(tradableRegistryJson(), { kob });
const KAS = 100_000_000n;

function ctxOf(): ClaimContext {
  const base = placeGolden(kob, 'create.bid').snapshots[0]!;
  const state = { ...base.order.state, state: { ...base.order.state.state, mode: '1', bought: '4000' } } as OrderState;
  const s0: OrderSnapshot = { ...base, order: { ...base.order, state } };
  const snapshot = { ...s0, boughtCustody: strayFor(s0, 4000n, 91, 10n * KAS) };
  const env: CancelEnv = { kob, maker: MAKER.pk, funding: [keyUtxo(MAKER.pk, 100n * KAS, 201)], tokenUtxos: [], clock: { daa: 1_000_000n, unixSeconds: 1_790_000_000n } };
  const token = registry.byCovenantId.get(TOKEN.covenantId);
  return { snapshot, env, wallet: [], room: 2, token, scale: 1000n };
}

describe('mergeCandidates', () => {
  it('takes the maker\'s plain UTXOs of the token and commitment, not reserved, smallest first', () => {
    const big = tokenUtxo(900n, MAKER.pk, 150);
    const small = tokenUtxo(5n, MAKER.pk, 151);
    const borrow = tokenUtxo(7n, MAKER.pk, 152, 1_000_000_000n, 0);
    borrow.state = { ...borrow.state, borrow_scheme: 1 } as typeof borrow.state;
    const other = tokenUtxo(3n, OTHER.pk, 153);
    const otherExt = tokenUtxo(4n, MAKER.pk, 154, 1_000_000_000n, 0, 'dd'.repeat(32));
    const reserved = tokenUtxo(2n, MAKER.pk, 155);
    const out = mergeCandidates([big, small, borrow, other, otherExt, reserved], MAKER.pk, TOKEN.covenantId, TOKEN.ext, new Set([`${reserved.transactionId}:${reserved.index}`]));
    expect(out.map((u) => u.index)).toEqual([151, 150]);
  });
});

describe('claim and sell', () => {
  it('places a GTC ask of everything claimed on the claimed commitment, validated in the script engine', () => {
    const ctx = ctxOf();
    expect(ctx.token).toBeTruthy();
    const price = 2n * BigInt((ctx.snapshot.order.state.state as { price: string }).price);
    const place = claimAskSpec({ kob, registry }, ctx, price)!;
    expect(place.order.kind).toBe('KobAsk');
    expect(place.order.state).toMatchObject({ amountLeft: '4000', price: price.toString(), extensionCommitment: TOKEN.ext, maker: MAKER.pk });
    const plan = planClaimChoice(ctx, { kind: 'sell', place, end: false });
    expect(plan.issues.filter((i) => i.severity === 'error')).toEqual([]);
    signAndValidate(kob, plan.built!);
    expect(plan.claim).toMatchObject({ placed: 4000n, toWallet: 0n });
  });

  it('a price at or below the bid\'s own while it stays open would sell into it (negative: ending the bid, or a higher price)', () => {
    const ctx = ctxOf();
    const s = ctx.snapshot.order.state.state as { price: string; tip: string };
    const top = BigInt(s.price) + BigInt(s.tip);
    expect(sellCrossesOwnBid(ctx, top, false)).toBe(true);
    expect(sellCrossesOwnBid(ctx, top + 1n, false)).toBe(false);
    expect(sellCrossesOwnBid(ctx, top, true)).toBe(false);
  });
});
