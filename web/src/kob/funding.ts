// Coin selection for placement transactions: KAS funding inputs and token inputs. Deterministic (ties broken by outpoint) and pure.
import { KRON_MAX_OUTPUT_AMOUNT, familyOfProgram, isKeyOwned } from './token-state';
import type { KeyUtxo, TokenProgram, TokenUtxo } from './types';

/** Default network-fee reserve added to the KAS a placement needs (0.05 KAS; the planner re-selects if the built fee turns out higher). */
export const DEFAULT_FEE_RESERVE = 5_000_000n;
/** Most KAS inputs one placement uses: beyond it the transaction mass approaches the standard limit, the wallet must consolidate first. */
export const MAX_FUNDING_INPUTS = 50;

/**
 * Most token inputs (of one token) the spare-slot fill of `selectTokens` takes a placement to: KOB's standard program's slots (KCC20Ref 3 / 3).
 * A larger program's slots are not filled up: measured on the 8 / 8 prototype, a sell placement with 8 token inputs weighs 105,546 grams
 * (max(compute, normalized transient), the mempool's standard-mass check, limit 100,000) and would not be relayed, at 3 inputs it stays far
 * below (funding.test.ts measures it, and a pair placement taking 3 of each token). The covering set itself may still use every slot (unchanged).
 */
export const MAX_SPARE_TOKEN_INPUTS = 3;

export type InsufficientKind = 'kas' | 'tokens' | 'fragmented';

/**
 * Thrown by selectFunding / selectTokens. `kind`: 'kas' and 'tokens' = the balance is short by `shortfall`; 'fragmented' = the balance
 * suffices but needs more inputs than one transaction may carry (`maxInputs`), so the UTXOs must be merged first.
 */
export class InsufficientFunds extends Error {
  readonly kind: InsufficientKind;
  readonly needed: bigint;
  readonly have: bigint;
  readonly shortfall: bigint;
  readonly maxInputs?: number;
  constructor(kind: InsufficientKind, needed: bigint, have: bigint, maxInputs?: number) {
    const short = needed > have ? needed - have : 0n;
    super(
      kind === 'fragmented'
        ? `covering ${needed} needs more than ${maxInputs} inputs: merge your UTXOs first`
        : `insufficient ${kind === 'kas' ? 'KAS' : 'tokens'}: need ${needed}, have ${have} (short by ${short})`,
    );
    this.name = 'InsufficientFunds';
    this.kind = kind;
    this.needed = needed;
    this.have = have;
    this.shortfall = short;
    this.maxInputs = maxInputs;
  }
}

const amountOf = (u: { amount: string }): bigint => BigInt(u.amount);
const outpointCmp = (a: { transactionId: string; index: number }, b: { transactionId: string; index: number }): number =>
  a.transactionId < b.transactionId ? -1 : a.transactionId > b.transactionId ? 1 : a.index - b.index;
const desc = <T extends { amount: string; transactionId: string; index: number }>(a: T, b: T): number => {
  const x = amountOf(a);
  const y = amountOf(b);
  return x === y ? outpointCmp(a, b) : x > y ? -1 : 1;
};

/**
 * Plain P2PK KAS UTXOs covering `needSompi + reserve`, SMALLEST FIRST until covered (ties by outpoint), as the Kaspa SDK does: its
 * UtxoContext keeps the mature UTXOs ascending by amount and the Generator consumes them in that order until the target is reached
 * (rusty-kaspa wallet/core/src/utxo/context.rs:300, 483). When the smallest `maxInputs` do not cover it, the largest-first selection with
 * the fewest inputs (the former rule), and InsufficientFunds('fragmented') only when even that needs more than `maxInputs`; a balance below
 * the target is InsufficientFunds('kas').
 *
 * The trade-off: smallest-first spends more inputs than largest-first (each a P2PK signature: more compute mass and fee, and more for an
 * external wallet to show), but every placement consolidates the wallet's dust into its change instead of leaving it to pile up.
 *
 * UTXOs that are covenant outputs are never spent as funding. Always at least one input: the builders take the FIRST funding input as the
 * one that authorises the order genesis (kob-protocol `build_create_order`: `auth` = the first input after the token inputs; the new
 * covenant id is bound to its outpoint) and its key as the default change owner. Any P2PK input of the wallet key does both (the inputs
 * are all of that key), so the order of the selection only changes which outpoint the covenant id is derived from: it is deterministic.
 */
export function selectFunding(utxos: KeyUtxo[], needSompi: bigint, reserve: bigint = DEFAULT_FEE_RESERVE, maxInputs: number = MAX_FUNDING_INPUTS): KeyUtxo[] {
  const target = (needSompi > 0n ? needSompi : 0n) + (reserve > 0n ? reserve : 0n);
  const usable = utxos.filter((u) => !u.covenantId && amountOf(u) > 0n);
  const have = usable.reduce((s, u) => s + amountOf(u), 0n);
  if (have < target || usable.length === 0) throw new InsufficientFunds('kas', target, have);
  // smallest first, at most `maxInputs`
  const asc = [...usable].sort((a, b) => (amountOf(a) === amountOf(b) ? outpointCmp(a, b) : amountOf(a) < amountOf(b) ? -1 : 1));
  const small: KeyUtxo[] = [];
  let sum = 0n;
  for (const u of asc) {
    if (small.length >= maxInputs) break;
    small.push(u);
    sum += amountOf(u);
    if (sum >= target) return small;
  }
  // the smallest `maxInputs` do not cover it: the fewest inputs, largest first
  const picked: KeyUtxo[] = [];
  sum = 0n;
  for (const u of [...usable].sort(desc)) {
    picked.push(u);
    sum += amountOf(u);
    if (sum >= target) break;
  }
  if (picked.length > maxInputs) throw new InsufficientFunds('fragmented', target, have, maxInputs);
  return picked;
}

const tokenAmt = (u: TokenUtxo): bigint => BigInt(u.state.amount);
const tokenAsc = (a: TokenUtxo, b: TokenUtxo): number => (tokenAmt(a) === tokenAmt(b) ? outpointCmp(a, b) : tokenAmt(a) < tokenAmt(b) ? -1 : 1);
const usableTokens = (utxos: readonly TokenUtxo[], program: TokenProgram | null): TokenUtxo[] => utxos.filter((u) => isKeyOwned(u.state, program) && tokenAmt(u) > 0n);
const I64_MAX = (1n << 63n) - 1n;

/**
 * The COVERING set of token UTXOs for `amount` base units, with the fewest inputs: the smallest single UTXO that suffices (least change), else
 * the largest ones first. Never more than `maxInputs` (the token program's input slots and the 8-input covenant limit): a balance that is large
 * enough but too fragmented throws InsufficientFunds('fragmented'). Only key-owned UTXOs are considered (custody and strays of orders are not
 * spendable this way). `selectTokens` starts from this set.
 */
export function coverTokens(utxos: TokenUtxo[], amount: bigint, maxInputs: number, program: TokenProgram | null = null): TokenUtxo[] {
  if (amount <= 0n) throw new RangeError('selectTokens: amount must be positive');
  const usable = usableTokens(utxos, program);
  const have = usable.reduce((s, u) => s + tokenAmt(u), 0n);
  if (have < amount) throw new InsufficientFunds('tokens', amount, have);
  const asc = [...usable].sort(tokenAsc);
  const single = asc.find((u) => tokenAmt(u) >= amount);
  if (single) return [single];
  const bigFirst = [...asc].reverse();
  const picked: TokenUtxo[] = [];
  let sum = 0n;
  for (const u of bigFirst) {
    picked.push(u);
    sum += tokenAmt(u);
    if (sum >= amount) break;
  }
  if (picked.length > maxInputs) throw new InsufficientFunds('fragmented', amount, have, maxInputs);
  return picked;
}

/**
 * Token inputs of a placement: the covering set (`coverTokens`, so whatever placed before still places: need 100 of [1, 1, 1, 200] still takes
 * the 200), then, while token input slots are spare (`maxInputs`, filled up to MAX_SPARE_TOKEN_INPUTS), the wallet's SMALLEST other usable
 * UTXOs of the token (ascending, ties by outpoint). The Kaspa SDK's Generator spends its UTXOs smallest-first (wallet/core/src/utxo/context.rs keeps them ascending); a token
 * placement cannot simply take UTXOs until covered (3 slots), so it covers first and spends the spare slots on the dust instead. The extras ride
 * on the order's token change: the builder (kob-protocol `build_create_order` / `build_create_pair`) returns everything above the custody as ONE
 * token output of the maker per token, and the extras' carriers are recycled into the outputs, the surplus coming back as KAS change. So each
 * placement consolidates the wallet's token UTXOs and frees their carriers' KAS (one more input of the token program each: more mass and fee).
 *
 * Not added: an extra that would push the change above `changeCap` (KRON's per-output amount limit; the default for a KRON `program`), and a
 * SINGLE extra next to an exact covering set: it would come back as a new change UTXO of the same amount, re-creating the UTXO at a fee
 * (two or more extras, or a covering set with change, do reduce the wallet's UTXO count). `spare: false` returns the covering set alone.
 */
export function selectTokens(
  utxos: TokenUtxo[],
  amount: bigint,
  maxInputs: number,
  program: TokenProgram | null = null,
  opts: { spare?: boolean; changeCap?: bigint } = {},
): TokenUtxo[] {
  const cover = coverTokens(utxos, amount, maxInputs, program);
  if (opts.spare === false) return cover;
  const cap = opts.changeCap ?? (program && familyOfProgram(program) === 'kron' ? KRON_MAX_OUTPUT_AMOUNT : I64_MAX);
  const taken = new Set(cover);
  const rest = usableTokens(utxos, program).filter((u) => !taken.has(u)).sort(tokenAsc);
  let change = cover.reduce((s, u) => s + tokenAmt(u), 0n) - amount;
  const extras: TokenUtxo[] = [];
  for (const u of rest) {
    if (cover.length + extras.length >= Math.min(maxInputs, MAX_SPARE_TOKEN_INPUTS)) break;
    if (change + tokenAmt(u) > cap) break;
    extras.push(u);
    change += tokenAmt(u);
  }
  const exact = cover.reduce((s, u) => s + tokenAmt(u), 0n) === amount;
  if (exact && extras.length === 1) return cover;
  return [...cover, ...extras];
}
