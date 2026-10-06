// Coin selection for placement transactions: KAS funding inputs and token inputs. Deterministic (ties broken by outpoint) and pure.
import { isKeyOwned } from './token-state';
import type { KeyUtxo, TokenUtxo } from './types';

/** Default network-fee reserve added to the KAS a placement needs (0.05 KAS; the planner re-selects if the built fee turns out higher). */
export const DEFAULT_FEE_RESERVE = 5_000_000n;
/** Most KAS inputs one placement uses: beyond it the transaction mass approaches the standard limit, the wallet must consolidate first. */
export const MAX_FUNDING_INPUTS = 50;

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
 * Largest-first selection of plain P2PK KAS UTXOs covering `needSompi + reserve` with the FEWEST inputs (every extra input costs
 * mass). UTXOs that are covenant outputs are never spent as funding. Always returns at least one input (the first one authorises the
 * order genesis). Throws InsufficientFunds (`kas` shortfall, or `fragmented` beyond MAX_FUNDING_INPUTS).
 */
export function selectFunding(utxos: KeyUtxo[], needSompi: bigint, reserve: bigint = DEFAULT_FEE_RESERVE, maxInputs: number = MAX_FUNDING_INPUTS): KeyUtxo[] {
  const target = (needSompi > 0n ? needSompi : 0n) + (reserve > 0n ? reserve : 0n);
  const sorted = utxos.filter((u) => !u.covenantId && amountOf(u) > 0n).sort(desc);
  const have = sorted.reduce((s, u) => s + amountOf(u), 0n);
  if (have < target || sorted.length === 0) throw new InsufficientFunds('kas', target, have);
  const picked: KeyUtxo[] = [];
  let sum = 0n;
  for (const u of sorted) {
    picked.push(u);
    sum += amountOf(u);
    if (sum >= target) break;
  }
  if (picked.length > maxInputs) throw new InsufficientFunds('fragmented', target, have, maxInputs);
  return picked;
}

/**
 * Selects token UTXOs covering `amount` base units with the fewest inputs: the smallest single UTXO that suffices (least change), else
 * the largest ones first. Never more than `maxInputs` (the token program's input slots and the 8-input covenant limit): a balance that
 * is large enough but too fragmented throws InsufficientFunds('fragmented'). Only P2PK-owned UTXOs are considered (custody and strays
 * of orders are not spendable this way).
 */
export function selectTokens(utxos: TokenUtxo[], amount: bigint, maxInputs: number): TokenUtxo[] {
  if (amount <= 0n) throw new RangeError('selectTokens: amount must be positive');
  const usable = utxos.filter((u) => isKeyOwned(u.state) && BigInt(u.state.amount) > 0n);
  const amt = (u: TokenUtxo): bigint => BigInt(u.state.amount);
  const have = usable.reduce((s, u) => s + amt(u), 0n);
  if (have < amount) throw new InsufficientFunds('tokens', amount, have);
  const asc = [...usable].sort((a, b) => (amt(a) === amt(b) ? outpointCmp(a, b) : amt(a) < amt(b) ? -1 : 1));
  const single = asc.find((u) => amt(u) >= amount);
  if (single) return [single];
  const bigFirst = [...asc].reverse();
  const picked: TokenUtxo[] = [];
  let sum = 0n;
  for (const u of bigFirst) {
    picked.push(u);
    sum += amt(u);
    if (sum >= amount) break;
  }
  if (picked.length > maxInputs) throw new InsufficientFunds('fragmented', amount, have, maxInputs);
  return picked;
}
