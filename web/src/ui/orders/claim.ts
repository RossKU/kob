// Planning of Claim, Claim and merge, Claim and sell of what a mode-1 order bought (bid-delivery-spec §7, §10). Only PLANS: the
// transaction goes through <TxFlow> like a cancel.
import { buildCancelEnv, tokenMarketOf } from '../../app/env';
import type { Services } from '../../app/services';
import type { StrayView } from '../../data/indexer-types';
import { boughtOf } from '../../kob/bought';
import { claimMergeRoom, planClaim, type CancelEnv, type CancelPlan, type OrderSnapshot, type ReplacementSpec } from '../../kob/cancel';
import { expiryFor } from '../../kob/daa';
import { DEFAULT_CARRIER, makeAsk } from '../../kob/orders/common';
import { big } from '../../kob/order-facts';
import type { PlanEnv } from '../../kob/plan-types';
import type { TokenInfo } from '../../kob/registry';
import { extensionMatches, isPlainState } from '../../kob/token-state';
import type { Hex, TokenUtxo } from '../../kob/types';
import { snapshotFor } from './actions';
import type { OrderEntry } from './orders-model';

/** A claim fee at or above this, with wallet UTXOs merged, gets the "merge fewer" hint (B5: up to 0.42 KAS on heavy programs). */
export const HEAVY_CLAIM_FEE = 10_000_000n;

/** What the claim dialog plans with: the node-confirmed order, the cancel env and the wallet's plain UTXOs of the bought token. */
export interface ClaimContext {
  snapshot: OrderSnapshot;
  env: CancelEnv;
  /** plain wallet UTXOs of the bought token that may join a claim and merge, smallest first */
  wallet: TokenUtxo[];
  /** most of them one claim takes */
  room: number;
  token: TokenInfo | undefined;
  /** the bought token's market scale (base units per whole token of a sell's price); null without a registry token */
  scale: bigint | null;
}

/** The plain wallet UTXOs a claim may merge: the maker's, of the bought token and its commitment, not reserved, smallest first. */
export function mergeCandidates(utxos: readonly TokenUtxo[], maker: Hex, token: Hex, ext: Hex | null, reserved: ReadonlySet<string> = new Set()): TokenUtxo[] {
  return utxos
    .filter((u) => (u.covenantId ?? token) === token && u.state.owner === maker && isPlainState(u.state) && extensionMatches(u.state, ext) && !reserved.has(`${u.transactionId}:${u.index}`))
    .sort((a, b) => (big(a.state.amount) < big(b.state.amount) ? -1 : big(a.state.amount) > big(b.state.amount) ? 1 : 0));
}

/** Reads the order from the node and the wallet's coins for a claim. */
export async function prepareClaim(s: Services, pubkey: Hex, entry: OrderEntry, strays: readonly StrayView[] | null, reserved: ReadonlySet<string> = new Set()): Promise<ClaimContext> {
  const snapshot = await snapshotFor(s, entry, strays);
  const bf = boughtOf(snapshot.order.state);
  const token = bf ? s.registry.byCovenantId.get(bf.token) : undefined;
  const env = await buildCancelEnv(s, { pubkey }, token ?? undefined);
  const wallet = bf ? mergeCandidates(env.tokenUtxos ?? [], pubkey, bf.token, bf.ext, reserved) : [];
  return { snapshot, env, wallet, room: claimMergeRoom(s.kob, snapshot.order.state), token, scale: token ? tokenMarketOf(s, token).scale : null };
}

/**
 * The limit sell a claim and sell places: a GTC `KobAsk` of `amount` (default: everything claimed) at `price` (sompi per whole token
 * of the token's scale), its custody on the claimed tokens' commitment. Null without a registry token or a clock.
 */
export function claimAskSpec(s: Pick<Services, 'kob' | 'registry'>, ctx: ClaimContext, price: bigint, amount?: bigint): ReplacementSpec | null {
  const bf = boughtOf(ctx.snapshot.order.state);
  if (!bf || !ctx.token || !ctx.env.clock || price <= 0n) return null;
  const market = { ...tokenMarketOf(s, ctx.token), ...(bf.ext ? { extensionCommitment: bf.ext } : {}) };
  const n = amount ?? bf.amount;
  const clock: PlanEnv['clock'] = { daa: ctx.env.clock.daa, unixSeconds: ctx.env.clock.unixSeconds ?? 0n, rateMilli: 10_000 };
  const minFill = s.kob.defaultMinFill(n, price, market.scale);
  const order = makeAsk({ kob: s.kob, token: market, maker: ctx.env.maker } as PlanEnv, { amount: n, price, minFill: minFill > n ? n : minFill, expiryDaa: expiryFor('gtc', clock).expiryDaa });
  const min = s.kob.minOrderValue(order);
  return { order, value: min > DEFAULT_CARRIER ? min : DEFAULT_CARRIER };
}

/** What the user picked in the claim dialog. */
export type ClaimChoice =
  | { kind: 'claim' }
  | { kind: 'merge'; count: number }
  | { kind: 'sell'; place: ReplacementSpec; end: boolean };

/** Plans the picked claim; a merge takes the `count` smallest wallet UTXOs (the dust first). */
export function planClaimChoice(ctx: ClaimContext, choice: ClaimChoice): CancelPlan {
  if (choice.kind === 'merge') return planClaim(ctx.env, ctx.snapshot, { merge: ctx.wallet.slice(0, Math.max(0, Math.min(choice.count, ctx.room))) });
  if (choice.kind === 'sell') return planClaim(ctx.env, ctx.snapshot, { place: choice.place, end: choice.end });
  return planClaim(ctx.env, ctx.snapshot);
}

/** A claim and sell that would sell into the order's own bid while it stays open (a self-trade). */
export function sellCrossesOwnBid(ctx: ClaimContext, askPrice: bigint, end: boolean): boolean {
  if (end) return false;
  const st = ctx.snapshot.order.state;
  if (st.kind !== 'KobBid' && st.kind !== 'KobCondBid') return false;
  const s = st.state as unknown as Record<string, string>;
  const bidTop = big(st.kind === 'KobCondBid' ? s.tpPrice : s.price) + big(s.tip);
  return askPrice <= bidTop;
}

/**
 * Claims of several orders (Claim all), one transaction each with the funding of the others left out, so the sends do not
 * spend one KAS UTXO twice. Failures are returned as plans with their issues.
 */
export async function planClaimMany(s: Services, pubkey: Hex, entries: readonly OrderEntry[], strays: readonly StrayView[] | null): Promise<{ plans: CancelPlan[]; failed: { id: Hex; error: unknown }[] }> {
  const plans: CancelPlan[] = [];
  const failed: { id: Hex; error: unknown }[] = [];
  const spent = new Set<string>();
  for (const e of entries) {
    try {
      const ctx = await prepareClaim(s, pubkey, e, strays);
      const env = { ...ctx.env, funding: (ctx.env.funding ?? []).filter((f) => !spent.has(`${f.transactionId}:${f.index}`)) };
      const plan = planClaimChoice({ ...ctx, env }, { kind: 'claim' });
      for (const f of plan.fundingUsed) spent.add(`${f.transactionId}:${f.index}`);
      plans.push(plan);
    } catch (error) {
      failed.push({ id: e.id, error });
    }
  }
  return { plans, failed };
}
