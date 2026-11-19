// The bank's payout request and the SDK generator's storage-mass dead band.
// No network, no wallet: the generator call is injected.
//
// The generator picks inputs in the order given until they cover the outputs plus the fee, and the
// change is whatever is left.
// KIP-9 storage mass is C * (sum 1/output - |inputs| / mean(inputs)) with C = 1e12 sompi.
// A change output of a few hundredths of a KAS costs about 100,000 mass, which is the standard
// limit.
// When the leftover lands in that band, the generator returns `Mass calculation error`
// (a bit lower, `Storage mass exceeds maximum`).
// It does not take one more input or fold the change into the fee.
//
// The bank's wallets are topped up in whole 100 KAS chunks from coinbase coins of about 3.09 KAS.
// The leftover is then a fixed residue, and every tick fails identically until the coin set
// changes.
// Measured on a live bank of 88,000 coins: 33 coins of 3.09 KAS and one output leaving 0.05 KAS
// gives the error.
// Leaving 0.1 KAS gives mass 54,013, and 0.2 KAS gives 37,812.
//
// The output ladder only raises the first output, so it shrinks the leftover.
// A consolidation (one output, total minus 1 KAS) has no room to raise.
// Take one giant coin and 79 coins of 0.9 to 3.1 KAS, sorted largest first.
// The generator stops taking inputs at the first prefix that covers output + fee.
// At keep = 1 KAS that prefix leaves a change of about 0.001 KAS, inside the dead band, and the
// request is refused.
// Measured live: keep 0.20 to 0.95 KAS is ok with 80 inputs, 1.00 to 1.05 is refused, and 1.10 to
// 1.85 is ok with 79.
// So a refused band of about 0.1 KAS follows the end of every coin.
//
// `marginCoins` controls the stopping point instead of hoping for a good residue.
// It hands the generator only the coins the request needs, with the largest coin LAST.
// No shorter prefix can cover the outputs, so the change is the planned margin (a healthy ~0.4
// KAS), never a dead-band residue.

// relative (not `@/`): unit-tested by node --test, which has no path alias
import { capForTotal, floorOf, type FeePolicy } from '../../../web/src/kob/fee-policy.ts';

export const KAS = 100_000_000n;

export interface PayoutOutput {
  address: string;
  amount: bigint;
}

const text = (e: unknown): string => (e instanceof Error ? e.message : String(e));

/**
 * The consolidation request: spend every given coin into one.
 * The SDK generator with NO outputs spends a single input back to the address.
 * That is a no-op that re-sends the same transaction every tick and fails with "already in the
 * mempool".
 * The output is the total minus `keep` (default 1 KAS), and the fee (about 0.09 KAS for 80 coins)
 * comes out of the change.
 * Nothing when the total is too small.
 */
export function consolidationOutputs(address: string, total: bigint, keep = KAS): PayoutOutput[] {
  return total > 2n * keep ? [{ address, amount: total - keep }] : [];
}

/**
 * What the bank sent that the node's confirmed UTXO set does not show yet.
 * At the relay-floor fee rate a transaction can wait minutes in a full mempool, and until it
 * confirms its inputs are still listed as unspent.
 * The next tick would build the SAME transaction (same inputs, same outputs, same id: "already in
 * the mempool").
 * With the old inputs left out, it would instead pay a wallet a second time.
 * So the inputs of every submitted transaction are not offered again, and a wallet that was paid is
 * not paid again, for `ttlMs`.
 * The default is 5 minutes. An unconfirmed transaction that vanished frees its coins then.
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

/**
 * The generator's refusals that are a property of this particular request, not of the network or
 * the wallet.
 */
export const isMassError = (e: unknown): boolean => /mass/i.test(text(e));

/**
 * The requests to try, in order:
 * - The plan itself.
 * - The first output raised by 1, 2, 3, 5 and 8 KAS (a different residue, the wallet simply
 *   receives a little more).
 * - The same with the last output dropped one by one (the next tick pays the rest).
 * Every attempt keeps at least one output. A request without outputs (consolidation) has no
 * alternative.
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
 * A coin set the generator cannot stop short of: the coins the request needs plus a margin, the
 * largest coin LAST.
 * - `coins` is any list with an `amount` (sompi, number / string / bigint), largest first or not.
 * - The needed coins are the largest ones until their sum covers `outputsTotal + margin`.
 *   So the change is at least `margin` less the fee.
 *  Then the largest of them moves to the end.
 *  The sum of everything before the last coin is below `outputsTotal`.
 *  The generator takes inputs in the order given and stops at the first prefix covering outputs +
 * fee, so it takes every one of them.
 *  The change is the margin (a ~0.4 KAS change costs ~25,000 storage mass), never a dead-band
 * residue of a few hundredths of a KAS.
 * - A single coin that covers it is returned alone (its change is the margin or more).
 * - null when the coins do not cover `outputsTotal + margin`, or when the largest needed coin is
 *   not above the leftover.
 *  That is the case of all coins tiny: no order forces the stop, so the output ladder is left to
 * try.
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

/**
 * The change the margin plan leaves, in sompi: 0.4 KAS plus the dearest fee one transaction can pay
 * at this rate (100,000 mass, the limit).
 */
export const marginFor = (rate: bigint): bigint => (4n * KAS) / 10n + rate * 100_000n;

export interface Planned<T> {
  result: T;
  /** the outputs the generator accepted */
  outputs: PayoutOutput[];
  /** how many requests were refused before this one */
  retries: number;
}

/**
 * Runs `create` with the plan. On a mass error it tries the alternatives, in this order:
 * - The plan with the default coins.
 * - The plan with each coin variant (`variants`, see `marginCoins`: the same outputs, a different
 *   stopping point).
 * - The output ladder of `payoutAttempts` with the default coins and with each variant.
 *  An alternative that the funds do not cover is skipped.
 * `create` gets the variant, or undefined for the default coins.
 * Any other error is thrown at once. When every attempt fails, the FIRST error is thrown (the
 * plan's own).
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
  /**
   * The plan was generated at the floor because the picked rate failed (the coins could not pay the
   * dearer fee).
   */
  fellBackToFloor: boolean;
  /** the total cap lowered the rate from this one */
  cappedFrom?: bigint;
}

/**
 * `createWithMassRetry` at the rate the fee policy picked (the SDK generator's `feeRate`), with the
 * policy's two safety nets:
 * - A plan that FAILS at the picked rate is generated again at the floor.
 *  The estimate must never make a payable payout impossible.
 * - One transaction of the plan above the total cap (`policy.maxFeeSompi`) at a rate above the
 *   floor is generated once more at `capForTotal`'s rate.
 *  A failing second plan keeps the first.
 * `fee` reads the fee of one generated transaction (the SDK's `feeAmount`).
 * `variants(outputs, rate)` lists the coin sets to try after the default coins (the bank passes
 * `marginCoins` at `marginFor(rate)`).
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
