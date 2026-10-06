// The bank's payout request and the SDK generator's storage-mass dead band. No network, no wallet: the generator call is injected.
//
// Root cause of the bank's `Mass calculation error` (80 ticks in one run, 1 in the next): the SDK generator picks inputs in the order
// given until they cover the outputs plus the fee, and the change is whatever is left. KIP-9 storage mass is
// C * (sum 1/output - |inputs| / mean(inputs)) with C = 1e12 sompi, so a change output of a few hundredths of a KAS costs about 100,000
// mass, the standard limit. When the leftover lands in that band the generator returns `Mass calculation error` (a bit lower it says
// `Storage mass exceeds maximum`) instead of taking one more input or folding the change into the fee. The bank's wallets are topped
// up in whole 100 KAS chunks from coinbase coins of about 3.09 KAS, so the leftover is a fixed residue (the same 80 largest coins, the
// same outputs) and every tick fails identically until the coin set changes (a 40-minute cluster at 05:25 on 10-02). Measured on the
// live bank (88,000 coins): 33 coins of 3.09 KAS, one output leaving 0.05 KAS: error; leaving 0.1 KAS: mass 54,013; 0.2 KAS: 37,812.
//
// Second root cause (10-03, 56 of 274 ticks `Storage mass exceeds maximum`, the same ticks failing every 40 s until a coinbase coin arrived): the
// output ladder only raises the first output, i.e. it SHRINKS the leftover, and a consolidation (one output, total minus 1 KAS) has no
// room to raise. The bank held one giant coin (461,621 KAS, the merged earlier consolidations) and 79 coins of 0.9 to 3.1 KAS, sorted
// largest first. The generator stops taking inputs at the first prefix that covers output + fee: at keep = 1 KAS the 79-coin prefix
// covers output + fee by a hair (0.0904 KAS left, fee 0.0892: a change of ~0.001 KAS, in the dead band), so it refuses (measured live: keep 0.20 to 0.95 KAS ok with 80 inputs, 1.00 to 1.05
// refused, 1.10 to 1.85 ok with 79, and so on: a refused band of ~0.1 KAS at the end of every coin). The fix is to control the
// stopping point instead of hoping for a good residue: `marginCoins` hands the generator only the coins the request needs, with the
// largest coin LAST, so no shorter prefix can cover the outputs and the change is the planned margin (a healthy ~0.4 KAS), never a
// dead-band residue.

// relative (not `@/`): unit-tested by node --test, which has no path alias
import { capForTotal, floorOf, type FeePolicy } from '../../../web/src/kob/fee-policy.ts';

export const KAS = 100_000_000n;

export interface PayoutOutput {
  address: string;
  amount: bigint;
}

const text = (e: unknown): string => (e instanceof Error ? e.message : String(e));

/**
 * The consolidation request: spend every given coin into one (the SDK generator with NO outputs spends a single input back to the
 * address: a no-op that re-sends the same transaction every tick and fails with "already in the mempool"). The output is the total
 * minus `keep` (default 1 KAS: the fee, about 0.09 KAS for 80 coins, comes out of the change). Nothing when the total is too small.
 */
export function consolidationOutputs(address: string, total: bigint, keep = KAS): PayoutOutput[] {
  return total > 2n * keep ? [{ address, amount: total - keep }] : [];
}

/**
 * What the bank sent that the node's confirmed UTXO set does not show yet. At the relay-floor fee rate a transaction can wait minutes in
 * a full mempool (TN10 after a flood), and until it confirms its inputs are still listed as unspent: the next tick would build the SAME
 * transaction (same inputs, same outputs, same id: "already in the mempool", 30 failures in 25 minutes), or, with the old inputs left
 * out, pay a wallet a second time. So the inputs of every submitted transaction are not offered again, and a wallet that was paid is not
 * paid again, for `ttlMs` (default 5 minutes; an unconfirmed transaction that vanished frees its coins then).
 */
export class RecentSpends {
  private readonly inputs = new Map<string, number>();
  private readonly paid = new Map<string, number>();
  private readonly ttlMs: number;
  constructor(ttlMs = 5 * 60_000) {
    this.ttlMs = ttlMs;
  }

  static key(transactionId: string, index: number): string {
    return `${transactionId}:${index}`;
  }

  spend(keys: Iterable<string>, now: number): void {
    for (const k of keys) this.inputs.set(k, now + this.ttlMs);
  }

  isSpent(key: string, now: number): boolean {
    const until = this.inputs.get(key);
    if (until === undefined) return false;
    if (until <= now) {
      this.inputs.delete(key);
      return false;
    }
    return true;
  }

  pay(address: string, now: number): void {
    this.paid.set(address, now + this.ttlMs);
  }

  isPaid(address: string, now: number): boolean {
    const until = this.paid.get(address);
    if (until === undefined) return false;
    if (until <= now) {
      this.paid.delete(address);
      return false;
    }
    return true;
  }
}

/** the generator's refusals that are a property of this particular request, not of the network or the wallet */
export const isMassError = (e: unknown): boolean => /mass/i.test(text(e));

/**
 * The requests to try, in order: the plan itself; then the first output raised by 1, 2, 3, 5 and 8 KAS (a different residue, the wallet
 * simply receives a little more); then the same with the last output dropped one by one (the next tick pays the rest). Every attempt
 * keeps at least one output; a request without outputs (consolidation) has no alternative.
 */
export function payoutAttempts(outputs: readonly PayoutOutput[]): PayoutOutput[][] {
  const out: PayoutOutput[][] = [outputs.map((o) => ({ ...o }))];
  for (let keep = outputs.length; keep >= 1; keep--) {
    for (const bump of [1n, 2n, 3n, 5n, 8n]) {
      const set = outputs.slice(0, keep).map((o) => ({ ...o }));
      set[0].amount += bump * KAS;
      out.push(set);
    }
  }
  return out;
}

/**
 * A coin set the generator cannot stop short of: the coins the request needs plus a margin, the largest coin LAST.
 *   * `coins` is any list with an `amount` (sompi, number / string / bigint), largest first or not;
 *   * the needed coins are the largest ones until their sum covers `outputsTotal + margin` (so the change is at least `margin` less the
 *     fee), then the largest of them moves to the end: the sum of everything before the last coin is below `outputsTotal`, so the
 *     generator (inputs in the order given, stops at the first prefix covering outputs + fee) takes every one of them and the change is
 *     the margin (a ~0.4 KAS change costs ~25,000 storage mass), never a dead-band residue of a few hundredths of a KAS;
 *   * a single coin that covers it is returned alone (its change is the margin or more);
 *   * null when the coins do not cover `outputsTotal + margin`, or when the largest needed coin is not above the leftover (all coins tiny:
 *     there is no order that forces the stop; the output ladder is left to try).
 */
export function marginCoins<C extends { amount: bigint | number | string }>(coins: readonly C[], outputsTotal: bigint, margin: bigint): C[] | null {
  const sorted = [...coins].sort((a, b) => (BigInt(b.amount) > BigInt(a.amount) ? 1 : BigInt(b.amount) < BigInt(a.amount) ? -1 : 0));
  const need = outputsTotal + margin;
  const picked: C[] = [];
  let sum = 0n;
  for (const c of sorted) {
    picked.push(c);
    sum += BigInt(c.amount);
    if (sum >= need) break;
  }
  if (sum < need) return null;
  if (picked.length === 1) return picked;
  const leftover = sum - outputsTotal;
  if (BigInt(picked[0].amount) <= leftover) return null;
  return [...picked.slice(1), picked[0]];
}

/** the change the margin plan leaves, in sompi: 0.4 KAS plus the dearest fee one transaction can pay at this rate (100,000 mass, the limit) */
export const marginFor = (rate: bigint): bigint => (4n * KAS) / 10n + rate * 100_000n;

export interface Planned<T> {
  result: T;
  /** the outputs the generator accepted */
  outputs: PayoutOutput[];
  /** how many requests were refused before this one */
  retries: number;
}

/**
 * Runs `create` with the plan; on a mass error tries the alternatives. Order: the plan with the default coins, the plan with each coin
 * variant (`variants`, see `marginCoins`: the same outputs, a different stopping point), then the output ladder of `payoutAttempts`
 * with the default coins and with each variant (an alternative that the funds do not cover is skipped). `create` gets the variant, or
 * undefined for the default coins. Any other error is thrown at once. When every attempt fails the FIRST error is thrown (the plan's own).
 */
export async function createWithMassRetry<T, C = never>(
  create: (outputs: PayoutOutput[], coins?: C) => Promise<T>,
  outputs: readonly PayoutOutput[],
  variants: readonly C[] = [],
): Promise<Planned<T>> {
  const ladder = payoutAttempts(outputs);
  const [plan, ...bumps] = ladder;
  const seq: { outputs: PayoutOutput[]; coins?: C }[] = [{ outputs: plan }, ...variants.map((coins) => ({ outputs: plan, coins }))];
  for (const b of bumps) seq.push({ outputs: b });
  for (const coins of variants) for (const b of bumps) seq.push({ outputs: b, coins });
  let first: unknown;
  let retries = 0;
  for (const attempt of seq) {
    try {
      return { result: await create(attempt.outputs, attempt.coins), outputs: attempt.outputs, retries };
    } catch (e) {
      if (!isMassError(e) && !(retries > 0 && /insufficient/i.test(text(e)))) throw e;
      first ??= e;
      retries++;
    }
  }
  throw first;
}

/** What the fee policy decided for a bank request (the plan, its rate and how it got there). */
export interface FeePlanned<T> extends Planned<T> {
  /** the feeRate the accepted plan was generated at, sompi per gram */
  rate: bigint;
  /** the plan was generated at the floor because the picked rate failed (the coins could not pay the dearer fee) */
  fellBackToFloor: boolean;
  /** the total cap lowered the rate from this one */
  cappedFrom?: bigint;
}

/**
 * `createWithMassRetry` at the rate the fee policy picked (the SDK generator's `feeRate`), with the policy's two safety nets:
 *   * a plan that FAILS at the picked rate is generated again at the floor (the estimate must never make a payable payout impossible);
 *   * one transaction of the plan above the total cap (`policy.maxFeeSompi`) at a rate above the floor: generated once more at
 *     `capForTotal`'s rate (a failing second plan keeps the first).
 * `fee` reads the fee of one generated transaction (the SDK's `feeAmount`). `variants(outputs, rate)` lists the coin sets to try after the
 * default coins (the bank passes `marginCoins` at `marginFor(rate)`).
 */
export async function planWithFeePolicy<T extends { transactions: unknown[] }, C = never>(
  create: (outputs: PayoutOutput[], rate: bigint, coins?: C) => Promise<T>,
  outputs: readonly PayoutOutput[],
  policy: FeePolicy,
  pickedRate: bigint,
  fee: (tx: unknown) => bigint = (tx) => BigInt((tx as { feeAmount?: bigint }).feeAmount ?? 0n),
  variants: (outputs: readonly PayoutOutput[], rate: bigint) => C[] = () => [],
): Promise<FeePlanned<T>> {
  const floor = floorOf(policy);
  let rate = pickedRate < floor ? floor : pickedRate;
  let fellBackToFloor = false;
  const at = (r: bigint) => createWithMassRetry<T, C>((o, c) => create(o, r, c), outputs, variants(outputs, r));
  let planned: Planned<T>;
  try {
    planned = await at(rate);
  } catch (e) {
    if (rate <= floor) throw e;
    rate = floor;
    fellBackToFloor = true;
    planned = await at(rate);
  }
  const worst = planned.result.transactions.reduce<bigint>((m, t) => (fee(t) > m ? fee(t) : m), 0n);
  const lowered = capForTotal(policy, rate, worst);
  let cappedFrom: bigint | undefined;
  if (lowered !== null) {
    try {
      const again = await at(lowered);
      cappedFrom = rate;
      rate = lowered;
      planned = again;
    } catch {
      /* keep the first plan */
    }
  }
  return { ...planned, rate, fellBackToFloor, ...(cappedFrom !== undefined ? { cappedFrom } : {}) };
}
