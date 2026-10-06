// Balances that wallets do not show: tokens in 0x04 custody, stray tokens, and KAS locked in the maker's orders (matcher.md 1.2, 10.9).
//
//   escrowedBalances(orders)            per token: custody / strays / KAS escrow / KAS carriers, from live OrderViews and/or OrderSnapshots
//                                       (a pair order counts its custody of A under A and of B under B, its KAS under its base token A)
//   withFreeBalances(escrowed, utxos)   adds the free (maker P2PK-owned) token balance -> TokenBalance[]
// Pure and DOM-free; all amounts bigint (token base units, sompi).
import type { OrderView } from '../data/indexer-types';
import type { OrderSnapshot } from './cancel';
import { asOrderState, big, custodiesOf, describeOrder, tokenCovIdOf } from './order-facts';
import { pairOfView } from './pair-view';
import { isKeyOwned } from './token-state';
import type { Hex, OrderState, TokenUtxo } from './types';

export interface EscrowedBalance {
  token: Hex;
  /** tokens in 0x04 custody of live orders (wallets do not show them), base units */
  custody: bigint;
  /** stray tokens owned by live orders' covenant ids: recoverable only by the maker's cancel, base units */
  strays: bigint;
  /** KAS budget of bid-side orders (order value minus carriers, reserves and tips), sompi */
  kasEscrow: bigint;
  /** KAS locked that is not trading budget: ask carriers, custody / stray token carriers, refund and keeper tips, entry reserves */
  kasCarriers: bigint;
  /** kasEscrow + kasCarriers */
  kasLocked: bigint;
  /** live orders counted */
  orders: number;
}

export interface TokenBalance {
  token: Hex;
  free: bigint;
  escrowed: bigint;
  strays: bigint;
  kasLocked: bigint;
  kasEscrow: bigint;
  kasCarriers: bigint;
  orderCount: number;
}

export interface SkippedOrder { covenantId: Hex; reason: 'terminal' | 'state-unknown' | 'no-current-utxo' }
export interface EscrowedResult { balances: EscrowedBalance[]; skipped: SkippedOrder[] }

// `killed`: an IOC / FOK / market order whose unfilled rest was returned (indexer processor.rs `(None, Some("kill")) => "killed"`)
const TERMINAL = new Set(['filled', 'cancelled', 'refunded', 'killed', 'closed']);
type Input = OrderView | OrderSnapshot;
const isView = (o: Input): o is OrderView => 'covenant_id' in o;

interface Normalised {
  covenantId: Hex;
  state: OrderState;
  /** order UTXO KAS */
  value: bigint;
  /** custody UTXOs (token, amount, KAS carrier) known (a pair order: of A and / or of B) */
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

/** Same as {@link escrowedBalances} plus the orders that could not be counted (terminal, state not proven, no current outpoint). */
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
    // tokens: each custody UTXO's own amount; a custody the input does not show counts what the state says it must hold (per token)
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
    // a pair order trades tokens only: its whole value is carriers and its tip prefund (never a KAS trading budget)
    if (d.side === 'sell' || d.pair) {
      // ask kinds: the order UTXO is a carrier (plus prefund / refund tip inside it), never trading budget
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
 * Per token: tokens in custody, strays and KAS locked across LIVE orders. OrderViews and OrderSnapshots may be mixed; terminal orders and
 * views without a proven state are skipped (see {@link escrowedBalancesDetailed} for the list).
 */
export function escrowedBalances(orders: Input[]): EscrowedBalance[] {
  return escrowedBalancesDetailed(orders).balances;
}

/**
 * Adds the maker's free tokens: the sum of key-owned (KCC-20 owner_scheme 0, KRON address presence; owner == maker) token UTXOs per token covenant id. UTXOs of other
 * owners and covenant-owned ones (custody, strays) are ignored. Tokens present on one side only still appear. Sorted by token id.
 */
export function withFreeBalances(escrowed: EscrowedBalance[], freeUtxos: TokenUtxo[], maker: Hex): TokenBalance[] {
  const free = new Map<Hex, bigint>();
  for (const u of freeUtxos) {
    if (!isKeyOwned(u.state) || u.state.owner !== maker || !u.covenantId) continue;
    free.set(u.covenantId, (free.get(u.covenantId) ?? 0n) + big(u.state.amount));
  }
  const tokens = new Set<Hex>([...free.keys(), ...escrowed.map((e) => e.token)]);
  const eBy = new Map(escrowed.map((e) => [e.token, e]));
  return [...tokens].sort().map((token) => {
    const e = eBy.get(token);
    return {
      token, free: free.get(token) ?? 0n, escrowed: e?.custody ?? 0n, strays: e?.strays ?? 0n, kasLocked: e?.kasLocked ?? 0n,
      kasEscrow: e?.kasEscrow ?? 0n, kasCarriers: e?.kasCarriers ?? 0n, orderCount: e?.orders ?? 0,
    };
  });
}

/** KAS view: spendable balance, KAS sitting in the maker's orders (all tokens), and the total. */
export function kasBalanceSummary(freeKas: bigint, balances: TokenBalance[]): { free: bigint; locked: bigint; total: bigint } {
  const locked = balances.reduce((s, b) => s + b.kasLocked, 0n);
  return { free: freeKas, locked, total: freeKas + locked };
}
