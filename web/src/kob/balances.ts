// Balances that wallets do not show: tokens in 0x04 custody, stray tokens, and KAS locked in the
// maker's orders (matcher.md 1.2 and 10.9).
//
// - `escrowedBalances(orders)` gives custody, strays, KAS escrow and KAS carriers per token, from
//   live OrderViews and OrderSnapshots. A pair order counts its custody of A under A and of B
//   under B, and its KAS under its base token A.
// - `withFreeBalances(escrowed, utxos)` adds the free token balance owned by the maker's key. It
//   also adds KRON tokens the key holds in a way KOB cannot spend (id_type 0 or 1).
//
// The module is pure and DOM-free. All amounts are bigint, in token base units or sompi.
import type { OrderView } from '../data/indexer-types';
import type { OrderSnapshot } from './cancel';
import { asOrderState, big, custodiesOf, describeOrder, tokenCovIdOf } from './order-facts';
import { pairOfView } from './pair-view';
import { isKeyOwned, isKronUnspendable } from './token-state';
import type { Hex, OrderState, TokenProgram, TokenUtxo } from './types';

/** What the live orders of one token hold in escrow. */
export interface EscrowedBalance {
  token: Hex;
  /** Tokens in 0x04 custody of live orders, in base units. Wallets do not show them. */
  custody: bigint;
  /**
   * Stray tokens owned by live orders' covenant ids, in base units. Only the maker's cancel
   * recovers them.
   */
  strays: bigint;
  /** KAS budget of bid-side orders, which is the order value minus carriers, reserves and tips. */
  kasEscrow: bigint;
  /**
   * KAS locked that is not trading budget, in sompi.
   * It covers ask carriers, custody and stray token carriers, refund and keeper tips, and entry
   * reserves.
   */
  kasCarriers: bigint;
  /** `kasEscrow` plus `kasCarriers`, in sompi. */
  kasLocked: bigint;
  /** The number of live orders counted. */
  orders: number;
}

/** The free and escrowed holdings of the maker for one token. */
export interface TokenBalance {
  token: Hex;
  free: bigint;
  /**
   * KRON tokens held by pubkey or script hash (`id_type` 0 or 1). The builders cannot spend them,
   * and `free` excludes them.
   */
  notSpendable: bigint;
  escrowed: bigint;
  strays: bigint;
  kasLocked: bigint;
  kasEscrow: bigint;
  kasCarriers: bigint;
  orderCount: number;
}

/** An order that could not be counted, with the reason. */
export interface SkippedOrder { covenantId: Hex; reason: 'terminal' | 'state-unknown' | 'no-current-utxo' }
/** The escrowed balances per token and the orders left out of them. */
export interface EscrowedResult { balances: EscrowedBalance[]; skipped: SkippedOrder[] }

// `killed` is an IOC, FOK or market order whose unfilled rest was returned.
const TERMINAL = new Set(['filled', 'cancelled', 'refunded', 'killed', 'closed']);
type Input = OrderView | OrderSnapshot;
const isView = (o: Input): o is OrderView => 'covenant_id' in o;

interface Normalised {
  covenantId: Hex;
  state: OrderState;
  /** KAS of the order UTXO. */
  value: bigint;
  /** The known custody UTXOs with token, amount and KAS carrier. A pair order may have A and B. */
  custodies: { token: Hex; amount: bigint; kas: bigint }[];
  strays: { token: Hex; amount: bigint; kas: bigint }[];
}

function normalise(o: Input): Normalised | SkippedOrder {
  if (isView(o)) {
    if (TERMINAL.has(o.status)) return { covenantId: o.covenant_id, reason: 'terminal' };
    const state = asOrderState(o.state);
    if (!o.state_known || !state) return { covenantId: o.covenant_id, reason: 'state-unknown' };
    if (!o.current || o.current.value == null) return { covenantId: o.covenant_id, reason: 'no-current-utxo' };
    const pv = pairOfView(o);
    const cus = pv
      ? pv.custodies.map((c) => c.utxo ?? null)
      : [o.custody?.utxo ?? null];
    return {
      covenantId: o.covenant_id, state, value: big(o.current.value),
      custodies: cus.filter((cu): cu is NonNullable<typeof cu> => !!cu && !cu.spent).map((cu) => ({ token: cu.token, amount: big(cu.state?.amount ?? cu.amount), kas: big(cu.value) })),
      strays: (o.strays ?? []).filter((s) => !s.spent).map((s) => ({ token: s.token, amount: big(s.state?.amount ?? s.amount), kas: big(s.value) })),
    };
  }
  const token = tokenCovIdOf(o.order.state);
  const cs = custodiesOf(o.order.state);
  return {
    covenantId: o.covenantId, state: o.order.state, value: big(o.order.amount),
    custodies: [o.custody ?? null, o.prefund ?? null].flatMap((u, k) => (u ? [{ token: u.covenantId ?? cs[k]?.token ?? token, amount: big(u.state.amount), kas: big(u.amount) }] : [])),
    strays: o.strays.map((s) => ({ token: s.covenantId ?? token, amount: big(s.state.amount), kas: big(s.amount) })),
  };
}

/**
 * Like {@link escrowedBalances}, plus the orders left out (terminal, unproven state, no outpoint).
 */
export function escrowedBalancesDetailed(orders: Input[]): EscrowedResult {
  const by = new Map<Hex, EscrowedBalance>();
  const skipped: SkippedOrder[] = [];
  for (const o of orders) {
    const n = normalise(o);
    if ('reason' in n) {
      skipped.push(n);
      continue;
    }
    const d = describeOrder(n.state);
    const token = tokenCovIdOf(n.state);
    const row = (t: Hex): EscrowedBalance =>
      by.get(t) ?? (by.set(t, { token: t, custody: 0n, strays: 0n, kasEscrow: 0n, kasCarriers: 0n, kasLocked: 0n, orders: 0 }).get(t) as EscrowedBalance);
    const b = row(token);
    b.orders++;
    // Count each custody UTXO's own amount. A custody the input does not show counts what the
    // state says it must hold.
    let carriers = 0n;
    const seen = new Set<Hex>();
    for (const c of n.custodies) {
      row(c.token).custody += c.amount;
      carriers += c.kas;
      seen.add(c.token);
    }
    for (const c of custodiesOf(n.state)) if (!seen.has(c.token)) row(c.token).custody += c.amount;
    for (const s of n.strays) {
      row(s.token).strays += s.amount;
      carriers += s.kas;
    }
    // A pair order trades tokens only. Its whole value is carriers and its tip prefund.
    if (d.side === 'sell' || d.pair) {
      // For ask kinds the order UTXO is a carrier, with the prefund and refund tip inside it.
      carriers += n.value;
    } else {
      const tips = d.refundTip + (d.trigger?.keeperTip ?? d.entry?.keeperTip ?? 0n);
      const reserved = tips + d.reservedKas;
      const nonBudget = reserved > n.value ? n.value : reserved;
      carriers += nonBudget;
      b.kasEscrow += n.value - nonBudget;
    }
    b.kasCarriers += carriers;
    b.kasLocked = b.kasEscrow + b.kasCarriers;
  }
  for (const b of by.values()) b.kasLocked = b.kasEscrow + b.kasCarriers;
  const balances = [...by.values()].sort((a, b) => (a.token < b.token ? -1 : a.token > b.token ? 1 : 0));
  return { balances, skipped };
}

/**
 * Sums custody, strays and locked KAS per token across live orders.
 * OrderViews and OrderSnapshots may be mixed. Terminal orders and views without a proven state are
 * skipped, and {@link escrowedBalancesDetailed} lists them.
 */
export function escrowedBalances(orders: Input[]): EscrowedBalance[] {
  return escrowedBalancesDetailed(orders).balances;
}

/**
 * Adds the maker's free tokens to the escrowed balances.
 *
 * Free is the sum per token covenant id of key-owned token UTXOs whose owner is the maker. That
 * means KCC-20 owner_scheme 0 or KRON address presence. `notSpendable` is the sum of the maker's
 * KRON UTXOs held by pubkey (`id_type` 0) or script hash (1), which the builders do not spend.
 * UTXOs of other owners and covenant-owned ones, such as custody and strays, are ignored. Tokens
 * present on one side only still appear. The result is sorted by token id.
 */
export function withFreeBalances(
  escrowed: EscrowedBalance[],
  freeUtxos: TokenUtxo[],
  maker: Hex,
  programOf: (token: Hex) => TokenProgram | null | undefined = () => null,
): TokenBalance[] {
  const free = new Map<Hex, bigint>();
  const other = new Map<Hex, bigint>();
  const seen = new Set<string>();
  for (const u of freeUtxos) {
    if (u.state.owner !== maker || !u.covenantId) continue;
    // Sources may overlap, so each outpoint counts once.
    const k = `${u.transactionId}:${u.index}`;
    if (seen.has(k)) continue;
    // For KRON the program's holder owner is spendable. That is address presence, or id_type 0 for
    // a program whose holders are id_type 0.
    const program = programOf(u.covenantId);
    const into = isKeyOwned(u.state, program) ? free : isKronUnspendable(u.state, program) ? other : null;
    if (!into) continue;
    seen.add(k);
    into.set(u.covenantId, (into.get(u.covenantId) ?? 0n) + big(u.state.amount));
  }
  const tokens = new Set<Hex>([...free.keys(), ...other.keys(), ...escrowed.map((e) => e.token)]);
  const eBy = new Map(escrowed.map((e) => [e.token, e]));
  return [...tokens].sort().map((token) => {
    const e = eBy.get(token);
    return {
      token, free: free.get(token) ?? 0n, notSpendable: other.get(token) ?? 0n, escrowed: e?.custody ?? 0n, strays: e?.strays ?? 0n, kasLocked: e?.kasLocked ?? 0n,
      kasEscrow: e?.kasEscrow ?? 0n, kasCarriers: e?.kasCarriers ?? 0n, orderCount: e?.orders ?? 0,
    };
  });
}

/**
 * Returns the free KAS, the KAS locked in the maker's orders across all tokens, and their total.
 */
export function kasBalanceSummary(freeKas: bigint, balances: TokenBalance[]): { free: bigint; locked: bigint; total: bigint } {
  const locked = balances.reduce((s, b) => s + b.kasLocked, 0n);
  return { free: freeKas, locked, total: freeKas + locked };
}
