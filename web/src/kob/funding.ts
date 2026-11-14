// Coin selection for placement transactions, for KAS funding inputs and token inputs.
// It is pure and deterministic, and ties break by outpoint.
import { KRON_MAX_OUTPUT_AMOUNT, familyOfProgram, isKeyOwned } from './token-state';
import type { KeyUtxo, TokenProgram, TokenUtxo } from './types';

/**
 * The default network-fee reserve added to the KAS a placement needs, 0.05 KAS.
 * The planner selects again when the built fee turns out higher.
 */
export const DEFAULT_FEE_RESERVE = 5_000_000n;
/**
 * The most KAS inputs one placement uses. Beyond it the mass nears the standard limit, so the
 * wallet must consolidate first.
 */
export const MAX_FUNDING_INPUTS = 50;

/**
 * The most token inputs of one token that the spare-slot fill of `selectTokens` takes a placement
 * to. It equals the slots of KOB's standard program, KCC20Ref 3 by 3. The spare slots of a larger
 * program are not filled, because a sell placement with 8 token inputs exceeds the 100,000 standard
 * mass that KOB's tools keep (kob-protocol issue.rs `MAX_STANDARD_MASS`), and every extra input
 * only adds fee. At 3 inputs the mass stays far below it, as funding.test.ts measures. The covering
 * set itself may still use every slot.
 */
export const MAX_SPARE_TOKEN_INPUTS = 3;

/** Why a selection failed. */
export type InsufficientKind = 'kas' | 'tokens' | 'fragmented';

/**
 * Thrown by `selectFunding` and `selectTokens`.
 * For `kind` 'kas' and 'tokens' the balance is short by `shortfall`. For 'fragmented' the balance
 * suffices but needs more inputs than one transaction may carry (`maxInputs`), so the UTXOs must be
 * merged first.
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
 * Selects plain P2PK KAS UTXOs covering `needSompi + reserve`, smallest first until covered.
 * Ties break by outpoint. This is the order the Kaspa SDK spends in, since its UtxoContext keeps
 * mature UTXOs ascending by amount (rusty-kaspa wallet/core/src/utxo/context.rs).
 *
 * When the smallest `maxInputs` do not cover the target, it falls back to the largest-first
 * selection with the fewest inputs. It throws InsufficientFunds('fragmented') only when even that
 * needs more than `maxInputs`. A balance below the target throws InsufficientFunds('kas').
 *
 * Smallest first spends more inputs than largest first. Each input adds a P2PK signature, so more
 * compute mass, fee and lines for an external wallet to show. In return every placement
 * consolidates the wallet's dust into its change.
 *
 * Covenant outputs are never spent as funding. There is always at least one input. The builders
 * take the first funding input to authorise the order genesis, because the new covenant id is bound
 * to its outpoint (kob-protocol `build_create_order`, `auth`). They also take its key as the
 * default change owner. Any P2PK input of the wallet key does both, so the selection order only
 * changes which outpoint the covenant id derives from, and it is deterministic.
 */
export function selectFunding(
  utxos: KeyUtxo[], needSompi: bigint, reserve: bigint = DEFAULT_FEE_RESERVE, maxInputs: number = MAX_FUNDING_INPUTS, fresh?: ReadonlySet<string>,
): KeyUtxo[] {
  // Settled coins first: a coin of a transaction sent moments ago makes the wallet wait until its
  // node sees it. It is spent only when the others cannot cover the target.
  const settled = fresh?.size ? utxos.filter((u) => !fresh.has(u.transactionId.toLowerCase())) : utxos;
  if (settled.length < utxos.length && settled.length > 0) {
    try {
      return selectFunding(settled, needSompi, reserve, maxInputs);
    } catch (e) {
      if (!(e instanceof InsufficientFunds)) throw e;
    }
  }
  const target = (needSompi > 0n ? needSompi : 0n) + (reserve > 0n ? reserve : 0n);
  const usable = utxos.filter((u) => !u.covenantId && amountOf(u) > 0n);
  const have = usable.reduce((s, u) => s + amountOf(u), 0n);
  if (have < target || usable.length === 0) throw new InsufficientFunds('kas', target, have);
  // Smallest first, at most `maxInputs`.
  const asc = [...usable].sort((a, b) => (amountOf(a) === amountOf(b) ? outpointCmp(a, b) : amountOf(a) < amountOf(b) ? -1 : 1));
  const small: KeyUtxo[] = [];
  let sum = 0n;
  for (const u of asc) {
    if (small.length >= maxInputs) break;
    small.push(u);
    sum += amountOf(u);
    if (sum >= target) return small;
  }
  // The smallest `maxInputs` do not cover it, so take the fewest inputs, largest first.
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
 * Selects the covering set of token UTXOs for `amount` base units, with the fewest inputs.
 * That is the smallest single UTXO that suffices, which leaves the least change, else the largest
 * ones first. It never takes more than `maxInputs`, the token program's input slots and the
 * 8-input covenant limit. A balance that is large enough but too fragmented throws
 * InsufficientFunds('fragmented'). Only key-owned UTXOs count, because the custody and strays of
 * orders are not spendable this way. `selectTokens` starts from this set.
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
 * Selects the token inputs of a placement.
 *
 * It starts with the covering set from `coverTokens`. For example, needing 100 of [1, 1, 1, 200]
 * still takes the 200. While token input slots are spare, up to `maxInputs` and
 * MAX_SPARE_TOKEN_INPUTS, it adds the wallet's smallest other usable UTXOs of the token, ascending
 * and ties by outpoint. A token placement has only a few slots, so it cannot take UTXOs until
 * covered as the Kaspa SDK does. It covers first and spends the spare slots on dust instead.
 *
 * The extras ride on the order's token change. The builder (kob-protocol `build_create_order` and
 * `build_create_pair`) returns everything above the custody as one token output of the maker per
 * token. The extras' carriers are recycled into the outputs, and the surplus comes back as KAS
 * change. Each placement therefore consolidates the wallet's token UTXOs and frees their carriers'
 * KAS, at the cost of more mass and fee per input.
 *
 * Two kinds of extra are not added.
 * - One that would push the change above `changeCap`. That is KRON's per-output amount limit, which
 *   is also the default for a KRON `program`.
 * - A single extra next to an exact covering set. It would come back as a new change UTXO of the
 *   same amount and re-create the UTXO at a fee. Two or more extras do reduce the UTXO count.
 *
 * `spare: false` returns the covering set alone.
 */
export function selectTokens(
  utxos: TokenUtxo[],
  amount: bigint,
  maxInputs: number,
  program: TokenProgram | null = null,
  opts: { spare?: boolean; changeCap?: bigint; fresh?: ReadonlySet<string> } = {},
): TokenUtxo[] {
  // Settled UTXOs first, as for the funding: one of a transaction sent moments ago covers the amount
  // only when the others cannot, and never rides along as a spare extra.
  const isFresh = (u: TokenUtxo) => !!opts.fresh?.has(u.transactionId.toLowerCase());
  const settled = opts.fresh?.size ? utxos.filter((u) => !isFresh(u)) : utxos;
  let cover: TokenUtxo[] | null = null;
  if (settled.length < utxos.length && settled.length > 0) {
    try {
      cover = coverTokens(settled, amount, maxInputs, program);
    } catch (e) {
      if (!(e instanceof InsufficientFunds)) throw e;
    }
  }
  cover ??= coverTokens(utxos, amount, maxInputs, program);
  if (opts.spare === false) return cover;
  const cap = opts.changeCap ?? (program && familyOfProgram(program) === 'kron' ? KRON_MAX_OUTPUT_AMOUNT : I64_MAX);
  const taken = new Set(cover);
  const rest = usableTokens(settled, program).filter((u) => !taken.has(u)).sort(tokenAsc);
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
