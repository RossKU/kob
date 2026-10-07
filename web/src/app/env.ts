// Builds the planners' environment (PlanEnv / CancelEnv) from live services: node clock + UTXOs, indexer book + own orders, token tracker.
// Pure with respect to the DOM; every dependency comes through `Services`, so it is unit-testable with fakes.
import type { BookOrderView, LevelView, OrderView } from '../data/indexer-types';
import type { CancelEnv } from '../kob/cancel';
import { normalizePolicy, readFeeContext, type FeeContext } from '../kob/fee-policy';
import type { FeeMode } from '../kob/types';
import type { BookLevel, BookOrder, BookView, Clock, OwnOrderRef, PairPlanEnv, PlanEnv, TokenMarket } from '../kob/plan-types';
import type { TokenInfo } from '../kob/registry';
import { impliedPairRate, isPairOrderView, ownPairOrderRefs, pairBookToBookView } from '../kob/pair';
import { firmReferencePrice } from '../kob/guards';
import { toTokenMarket } from '../kob/token-market';
import type { Hex } from '../kob/types';
import type { WalletInfo } from '../wallet/types';
import type { Services } from './services';

const bi = (v: string | number | null | undefined): bigint => (v === null || v === undefined ? 0n : BigInt(v));
const DEFAULT_RATE_MILLI = 10_000;

/** TokenMarket of a registry token (throws for tokens this build cannot trade: check `token.tradable` first). */
export function tokenMarketOf(s: Pick<Services, 'kob' | 'registry'>, token: TokenInfo): TokenMarket {
  return toTokenMarket(s.kob, token.json, s.registry.templates);
}

export async function readClock(s: Pick<Services, 'node'>): Promise<Clock> {
  const c = await s.node.getClock();
  return { daa: c.daa, unixSeconds: c.unixSeconds, rateMilli: c.rateMilli ?? DEFAULT_RATE_MILLI };
}

const isBookOrder = (o: LevelView | BookOrderView): o is BookOrderView => 'covenant_id' in o;

/**
 * An order carries its own `scale` (base units per whole token, its price denominator). The app writes the token's standard scale (`10^decimals`);
 * an order of another scale quotes another whole token, so it is not part of the standard book and is skipped (the indexer lists it only when its
 * own rule accepts it). An order without a scale (an older view) is taken as the standard one.
 */
const sameScale = (market: TokenMarket, o: { scale?: number | null }): boolean => o.scale === null || o.scale === undefined || BigInt(o.scale) === market.scale;

/** Indexer book (aggregate=false preferred: per-order entries make the FOK pre-check exact) -> planner BookView (prices per whole token, base units). */
export function bookFromIndexer(market: TokenMarket, view: { asks: (LevelView | BookOrderView)[]; bids: (LevelView | BookOrderView)[] }): BookView {
  const side = (rows: (LevelView | BookOrderView)[]): { levels: BookLevel[]; orders: BookOrder[] | undefined } => {
    if (rows.length === 0) return { levels: [], orders: [] };
    if (!rows.every(isBookOrder)) {
      const levels = new Map<bigint, BookLevel>();
      for (const l of rows as LevelView[]) {
        if (!sameScale(market, l)) continue;
        const price = BigInt(l.price);
        const cur = levels.get(price);
        if (cur) {
          cur.amount += bi(l.amount);
          cur.orders += l.orders;
        } else levels.set(price, { price, amount: bi(l.amount), orders: l.orders });
      }
      return { levels: [...levels.values()], orders: undefined };
    }
    const orders: BookOrder[] = [];
    for (const o of rows as BookOrderView[]) {
      if (!sameScale(market, o)) continue;
      orders.push({
        covenantId: o.covenant_id, price: BigInt(o.price), amount: bi(o.amount_left ?? 1),
        ...(o.tip ? { tip: bi(o.tip) } : {}), ...(o.min_fill ? { minFill: bi(o.min_fill) } : {}),
      });
    }
    const levels: BookLevel[] = [];
    for (const o of orders) {
      const last = levels[levels.length - 1];
      if (last && last.price === o.price) {
        last.amount += o.amount;
        last.orders += 1;
      } else levels.push({ price: o.price, amount: o.amount, orders: 1 });
    }
    return { levels, orders };
  };
  const a = side(view.asks);
  const b = side(view.bids);
  return { asks: a.levels, bids: b.levels, ...(a.orders ? { askOrders: a.orders } : {}), ...(b.orders ? { bidOrders: b.orders } : {}) };
}

const isLive = (o: OrderView): boolean => o.status === 'open' || o.status === 'partial';

/**
 * Own resting orders of the wallet (indexer `orders?maker=&token=`) in `market`'s KAS book, for self-trade prevention. Pair orders are left out:
 * `orders?token=` lists them under both their tokens but they carry no KAS price (`price` null) and trade in their pair book (a pair plan
 * compares them there: `ownPairOrderRefs`).
 */
export function ownOrderRefs(market: TokenMarket, orders: OrderView[]): OwnOrderRef[] {
  const out: OwnOrderRef[] = [];
  for (const o of orders) {
    if (!isLive(o) || isPairOrderView(o)) continue;
    if (o.token !== market.covenantId || !sameScale(market, o)) continue;
    out.push({
      covenantId: o.covenant_id,
      side: o.side === 1 ? 'sell' : 'buy',
      price: bi(o.quote ?? o.price),
      tip: bi(o.tip),
      amountLeft: bi(o.amount_left ?? o.initial_amount ?? 0),
      active: !o.expired,
    });
  }
  return out;
}

/** Fee mode of the planners: the node relay floor, or the storage-inclusive priority fee when Settings ask for it. */
const feeModeOf = (s: Services): { feeMode?: FeeMode } => (s.config?.features?.priorityFee ? { feeMode: 'priority' as const } : {});

/**
 * The fee policy input of one planning moment: the app's policy and the node's newest usable estimate (a cached read, at most one node call per refresh
 * interval; never rejects). With the policy off, or on services without one, no call is made and the planners pay the floor.
 */
export async function readFees(s: Pick<Services, 'fees'>): Promise<FeeContext> {
  const f = s.fees;
  return readFeeContext(f?.policy ?? normalizePolicy({ dynamic: false }), f?.oracle);
}

export interface BuildEnvOptions {
  /** extra planner overrides (tests) */
  feeRate?: bigint;
  carrier?: bigint;
  /** skip the indexer (book empty, no own orders): node-only mode */
  offline?: boolean;
}

/** Everything `planOrder` needs for one token and the connected wallet. Rejects when the node is unreachable. */
export async function buildPlanEnv(s: Services, wallet: Pick<WalletInfo, 'pubkey'>, token: TokenInfo, o: BuildEnvOptions = {}): Promise<PlanEnv> {
  if (!token.tradable || !token.program) throw new Error(`token ${token.ticker} is not tradable in this build (${token.untradableReason ?? 'unsupported'})`);
  const market = tokenMarketOf(s, token);
  const [clock, funding, tokenUtxos, fees] = await Promise.all([
    readClock(s),
    s.utxos.fundingFor(wallet.pubkey),
    s.tracker.tokenUtxosFor(wallet.pubkey, { covenantId: token.covenantId, program: token.program }),
    readFees(s),
  ]);
  let book: BookView = { asks: [], bids: [] };
  let ownOrders: OwnOrderRef[] = [];
  let lastFillPrice: bigint | null = null;
  // a guard whose data could not be read is REPORTED (the planner blocks until the user acknowledges), never silently switched off
  const guardsUnavailable: ('own-orders' | 'book')[] = [];
  if (s.indexer && !o.offline) {
    const [bv, mine, trades] = await Promise.all([
      s.indexer.book(token.covenantId, { depth: 100, aggregate: false }).catch(() => null),
      s.indexer.allOrders({ maker: wallet.pubkey, token: token.covenantId, status: 'active' }).catch(() => null),
      s.indexer.trades(token.covenantId, { limit: 1 }).catch(() => null),
    ]);
    if (bv) book = bookFromIndexer(market, bv);
    else guardsUnavailable.push('book');
    if (mine) ownOrders = ownOrderRefs(market, mine);
    else guardsUnavailable.push('own-orders');
    const last = trades?.items?.[0];
    // trades are priced per `price_basis` base units (the token's standard scale): convert to the market's scale (per whole token)
    if (last && trades.price_basis && BigInt(trades.price_basis) > 0n) lastFillPrice = (BigInt(last.price) * market.scale) / BigInt(trades.price_basis);
  }
  return { kob: s.kob, token: market, maker: wallet.pubkey, clock, book, ownOrders, ...(guardsUnavailable.length ? { guardsUnavailable } : {}), lastFillPrice, funding, tokenUtxos, ...feeModeOf(s), fees, ...(o.feeRate ? { feeRate: o.feeRate } : {}), ...(o.carrier ? { carrier: o.carrier } : {}) };
}

/** Environment of the cancel / cancel-replace / refund planners. */
export async function buildCancelEnv(s: Services, wallet: Pick<WalletInfo, 'pubkey'>, token?: TokenInfo): Promise<CancelEnv> {
  const [clock, funding, tokenUtxos, fees] = await Promise.all([
    readClock(s),
    s.utxos.fundingFor(wallet.pubkey),
    token && token.program ? s.tracker.tokenUtxosFor(wallet.pubkey, { covenantId: token.covenantId, program: token.program }) : Promise.resolve([]),
    readFees(s),
  ]);
  return { kob: s.kob, maker: wallet.pubkey, funding, tokenUtxos, clock: { daa: clock.daa, unixSeconds: clock.unixSeconds }, ...feeModeOf(s), fees };
}

// ------------------------------------------------------------------------------------------------ token/token pairs

/**
 * The KAS references of a token (sompi per whole token of `market`'s scale): `book` = the midpoint of its KAS book's touch (one side when only
 * one exists), `last` = its newest KAS trade (indexer trades, priced per `price_basis`, converted to the market's scale). Null where the
 * indexer has nothing (or failed: a reference is a default, never a guard).
 */
export async function kasReferenceOf(indexer: Pick<NonNullable<Services['indexer']>, 'book' | 'trades'>, market: TokenMarket): Promise<{ book: bigint | null; last: bigint | null }> {
  const [bv, trades] = await Promise.all([
    indexer.book(market.covenantId, { depth: 20, aggregate: true }).catch(() => null),
    indexer.trades(market.covenantId, { limit: 1 }).catch(() => null),
  ]);
  // a quote nobody has to take (an ask far above the bids, or the only side of the book) does not set the reference, nor does a bid
  // worth less than one default minimum fill above the others
  const book = bv && Array.isArray(bv.asks) && Array.isArray(bv.bids) ? firmReferencePrice(bookFromIndexer(market, bv), market.scale) : null;
  const t = trades?.items?.[0];
  let last: bigint | null = null;
  try {
    if (t && trades.price_basis && BigInt(trades.price_basis) > 0n) last = (BigInt(t.price) * market.scale) / BigInt(trades.price_basis);
  } catch {
    last = null;
  }
  return { book: book !== null && book > 0n ? book : null, last: last !== null && last > 0n ? last : null };
}

/**
 * Everything `planOrder` needs to plan on the pair `base`/`quote` (A/B, oriented) for the connected wallet: node clock, the maker's KAS funding and
 * UTXOs of BOTH tokens, the pair book (`indexer.pairBook`, every source: direct / entry / route) as a BookView in B per whole A, the wallet's live
 * pair orders of this pair (self-trade prevention), the KAS references of A and B (their KAS books, else their newest trades: the default
 * minimum fill) and the rate the newest KAS trades imply (`lastFillPrice`, cross-checked against the pair book; pair fills never make a price).
 * A guard whose data could not be read is reported (`guardsUnavailable`), as in `buildPlanEnv`. Rejects when the node is unreachable or the two
 * tokens are not two different tradable tokens.
 */
export async function buildPairPlanEnv(s: Services, wallet: Pick<WalletInfo, 'pubkey'>, base: TokenInfo, quote: TokenInfo, o: BuildEnvOptions = {}): Promise<PairPlanEnv> {
  for (const t of [base, quote]) {
    if (!t.tradable || !t.program) throw new Error(`token ${t.ticker} is not tradable in this build (${t.untradableReason ?? 'unsupported'})`);
  }
  if (base.covenantId === quote.covenantId) throw new Error('a pair needs two different tokens');
  const a = tokenMarketOf(s, base);
  const b = tokenMarketOf(s, quote);
  const [clock, funding, tokenUtxos, quoteTokenUtxos, fees] = await Promise.all([
    readClock(s),
    s.utxos.fundingFor(wallet.pubkey),
    s.tracker.tokenUtxosFor(wallet.pubkey, { covenantId: base.covenantId, program: base.program! }),
    s.tracker.tokenUtxosFor(wallet.pubkey, { covenantId: quote.covenantId, program: quote.program! }),
    readFees(s),
  ]);
  let book: BookView = { asks: [], bids: [] };
  let view: PairPlanEnv['pair']['view'] = null;
  let ownOrders: OwnOrderRef[] = [];
  let kasPerWholeA: bigint | null = null;
  let kasPerWholeB: bigint | null = null;
  let lastFillPrice: bigint | null = null;
  const guardsUnavailable: ('own-orders' | 'book')[] = [];
  if (s.indexer && !o.offline) {
    const [pv, mine, refA, refB] = await Promise.all([
      s.indexer.pairBook(a.covenantId, b.covenantId, { depth: 100 }).catch(() => null),
      s.indexer.allOrders({ maker: wallet.pubkey, token: a.covenantId, status: 'active' }).catch(() => null),
      kasReferenceOf(s.indexer, a),
      kasReferenceOf(s.indexer, b),
    ]);
    // an indexer without the pair routes (null) or a failed read: the crossing / FOK / band guards have no book
    if (pv) {
      view = pv;
      book = pairBookToBookView(pv, a.scale);
    } else guardsUnavailable.push('book');
    if (mine) ownOrders = ownPairOrderRefs(mine, a.covenantId, b.covenantId);
    else guardsUnavailable.push('own-orders');
    kasPerWholeA = refA.book ?? refA.last;
    kasPerWholeB = refB.book ?? refB.last;
    lastFillPrice = impliedPairRate(refA.last, refB.last, b.scale);
  }
  return {
    kob: s.kob, token: a, maker: wallet.pubkey, clock, book, ownOrders, ...(guardsUnavailable.length ? { guardsUnavailable } : {}), lastFillPrice, funding, tokenUtxos,
    ...feeModeOf(s), fees, ...(o.feeRate ? { feeRate: o.feeRate } : {}), ...(o.carrier ? { carrier: o.carrier } : {}),
    pair: { quote: b, quoteTokenUtxos, view, kasPerWholeA, kasPerWholeB },
  };
}
