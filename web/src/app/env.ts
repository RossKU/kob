// Builds the planners' environment (PlanEnv and CancelEnv) from live services. That is the node
// clock and UTXOs, the indexer book and own orders, and the token tracker. Every dependency comes
// through `Services` and nothing touches the DOM, so tests can use fakes.
import type { BookOrderView, LevelView, OrderView, PairBookView, PairLevelView } from '../data/indexer-types';
import { indexerPresent, judgePresent } from '../kob/chain-present';
import { incompleteAsNull } from '../data/indexer';
import { declaredToken } from '../data/token-declarations';
import type { CancelEnv } from '../kob/cancel';
import { normalizePolicy, readFeeContext, type FeeContext } from '../kob/fee-policy';
import type { FeeMode, KeyUtxo, TokenUtxo } from '../kob/types';
import { isPairEnv, type BookLevel, type BookOrder, type BookOrderUtxo, type BookView, type Clock, type DroppedBookOrder, type OwnOrderRef, type PairPlanEnv, type PlanEnv, type TokenMarket } from '../kob/plan-types';
import type { TokenInfo } from '../kob/registry';
import { impliedPairRate, isPairOrderView, ownPairOrderRefs } from '../kob/pair';
import { firmReferencePrice } from '../kob/guards';
import { bookWithout } from '../kob/take';
import { toTokenMarket } from '../kob/token-market';
import type { Hex } from '../kob/types';
import type { WalletInfo } from '../wallet/types';
import { freshTxids } from '../wallet/visibility';
import { pendingSpends } from './pending-spends';
import { spkStringToAddress } from '../data/kaspa-sdk';
import { describeNodeError } from '../data/node-error';
import { verifyBook } from './book-verify';
import { routeLevels, verifyPairBook } from './pair-book';
import type { Services } from './services';

const bi = (v: string | number | null | undefined): bigint => (v === null || v === undefined ? 0n : BigInt(v));
const DEFAULT_RATE_MILLI = 10_000;

/** The TokenMarket of a registry token. Throws for a token this build cannot trade. */
export function tokenMarketOf(s: Pick<Services, 'kob' | 'registry'>, token: TokenInfo): TokenMarket {
  return toTokenMarket(s.kob, token.json, s.registry.templates);
}

/**
 * The clock the planners sign times from (R9FB3-1): the node's DAA judged against the indexer's
 * verified present (its cursor's verified header time advanced by the wall clock, never the node DAA
 * it relays) and, when given, the highest `blockDaaScore` the plan read. Added load: none on the node
 * (the same one call as `readClock`); one `/v1/health` read of the indexer, at plan time only.
 */
export async function readAgreedClock(s: Pick<Services, 'node'> & Partial<Pick<Services, 'indexer'>>, floorDaa: bigint | null = null): Promise<Clock> {
  const ix = s.indexer;
  const [c, h] = await Promise.all([readClock(s), ix && typeof ix.health === 'function' ? Promise.resolve().then(() => ix.health({ priority: 'high' })).catch(() => null) : Promise.resolve(null)]);
  const p = judgePresent(c.daa, { indexer: indexerPresent(h, Date.now()), floor: floorDaa });
  return { ...c, witness: p.verdict, witnessReason: p.reason };
}

/** The highest `blockDaaScore` of the coins a plan read: the present is not below it. */
export function floorOf(...lists: readonly (readonly { blockDaaScore?: string | number | bigint | null }[] | undefined)[]): bigint | null {
  let top: bigint | null = null;
  for (const l of lists) for (const u of l ?? []) {
    if (u.blockDaaScore === undefined || u.blockDaaScore === null) continue;
    const v = BigInt(u.blockDaaScore);
    if (top === null || v > top) top = v;
  }
  return top;
}

/** Reads the node clock. The rate falls back to a default when the node gives none. */
export async function readClock(s: Pick<Services, 'node'>): Promise<Clock> {
  const c = await s.node.getClock();
  return { daa: c.daa, unixSeconds: c.unixSeconds, rateMilli: c.rateMilli ?? DEFAULT_RATE_MILLI };
}

const isBookOrder = (o: LevelView | BookOrderView): o is BookOrderView => 'covenant_id' in o;

/**
 * Whether an order is quoted in the market's standard scale. An order carries its own `scale`,
 * the base units per whole token, which is its price denominator. The app writes the standard scale
 * (`10^decimals`). An order of another scale quotes another whole token, so it is not part of the
 * standard book and is skipped. An order without a scale counts as standard.
 */
export const sameScale = (market: TokenMarket, o: { scale?: number | null }): boolean => o.scale === null || o.scale === undefined || BigInt(o.scale) === market.scale;

/**
 * The planner BookView of an indexer book, with prices per whole token in base units. Per-order
 * entries (aggregate=false) are preferred because they make the FOK pre-check exact.
 */
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
 * The wallet's own resting orders in `market`'s KAS book, for self-trade prevention.
 * Pair orders are left out. `orders?token=` lists them under both their tokens, but they carry no
 * KAS price and trade in their pair book, where a pair plan compares them (`ownPairOrderRefs`).
 * An order with no KAS price is left out too, for example an unarmed stop (`KobCondAsk`,
 * `KobCondBid`). Nothing can trade against it, and reading its price as 0 would refuse every buy
 * of the wallet as a self-trade.
 */
export function ownOrderRefs(market: TokenMarket, orders: OrderView[]): OwnOrderRef[] {
  const out: OwnOrderRef[] = [];
  for (const o of orders) {
    if (!isLive(o) || isPairOrderView(o)) continue;
    if (o.token !== market.covenantId || !sameScale(market, o)) continue;
    const price = o.quote ?? o.price;
    if (price === null || price === undefined) continue;
    out.push({
      covenantId: o.covenant_id,
      side: o.side === 1 ? 'sell' : 'buy',
      price: bi(price),
      tip: bi(o.tip),
      amountLeft: bi(o.amount_left ?? o.initial_amount ?? 0),
      active: !o.expired,
    });
  }
  return out;
}

/** The fee mode of the planners: the node relay floor, or the priority fee when Settings ask. */
const feeModeOf = (s: Pick<Services, 'config'>): { feeMode?: FeeMode } => (s.config?.features?.priorityFee ? { feeMode: 'priority' as const } : {});

/**
 * The fee policy input of one planning moment: the app's policy and the node's newest usable
 * estimate. The estimate is a cached read, at most one node call per refresh interval, and never
 * rejects. With the policy off, or on services without one, no call is made and the planners pay
 * the floor.
 */
export async function readFees(s: Pick<Services, 'fees'>): Promise<FeeContext> {
  const f = s.fees;
  return readFeeContext(f?.policy ?? normalizePolicy({ dynamic: false }), f?.oracle);
}

/** Options of the environment builders. */
export interface BuildEnvOptions {
  /** Planner overrides, for tests. */
  feeRate?: bigint;
  carrier?: bigint;
  /** Skips the indexer, so the book is empty and there are no own orders (node-only mode). */
  offline?: boolean;
  /**
   * Token base units the book must offer in node-confirmed orders per side, which is the size
   * being planned. The check reads deeper for it (app/book-verify.ts).
   */
  depth?: bigint;
}

// The parts of a KAS-market environment. The order ticket keeps three parts fresh separately
// (ui/ticket/use-ticket.ts), and `buildPlanEnv` reads all three at once.
// - The market is the same for every wallet and moves with the book.
// - The coins come from the node alone, so they are fast.
// - The holdings need the indexer: its token-UTXO listing, verified on the node, and the wallet's
//   resting orders.

/**
 * The wallet-independent part: the book per order and the newest trade. It never rejects. An
 * unreadable book is reported in `bookUnavailable`.
 */
export interface MarketPart {
  book: BookView;
  /** Book orders the node did not confirm, left out of `book` (app/book-verify.ts). */
  dropped: DroppedBookOrder[];
  bookUnavailable: boolean;
  lastFillPrice: bigint | null;
  /**
   * The node-confirmed UTXOs of the book's orders, by covenant id in lower case. A take spends
   * them.
   */
  orders?: ReadonlyMap<string, BookOrderUtxo>;
}

/** The node part: clock, spendable KAS and the fee context. Rejects when the node is down. */
export interface CoinsPart {
  clock: Clock;
  funding: KeyUtxo[];
  fees: FeeContext;
}

/**
 * The wallet's indexer-backed part: its token UTXOs (the indexer listing verified on the node) and
 * its resting orders, which are null when they could not be read.
 */
export interface HoldingsPart {
  tokenUtxos: TokenUtxo[];
  ownOrders: OwnOrderRef[] | null;
}

/**
 * The market part: the book per order, checked against the node, and the newest trade.
 * The first orders of each side (app/book-verify.ts) must exist unspent at the script their
 * state derives. An order the node does not confirm is left out and listed in `dropped`.
 * It rejects only with a `NodeError` when the node cannot be asked for that check, and then the
 * ticket shows its node banner and plans nothing. An unreadable book is reported in
 * `bookUnavailable`, and so is a side whose touch could not be checked because an order read of
 * the indexer failed (`VerifiedBook.unread`): planned as empty, a take shown as "Fill now" would
 * become a resting limit at the click, without the marketable warning (R6B-L1).
 */
export async function readMarketPart(s: Pick<Services, 'indexer' | 'kob' | 'node' | 'sdk' | 'config'>, market: TokenMarket, o: Pick<BuildEnvOptions, 'offline' | 'depth'> = {}): Promise<MarketPart> {
  // The indexer in use now serves the book and the order reads behind it alike. With failover,
  // `s.indexer` follows the active one from call to call.
  const ix = s.indexer;
  if (!ix || o.offline) return { book: { asks: [], bids: [] }, dropped: [], bookUnavailable: false, lastFillPrice: null };
  // The node's clock is asked beside the book. The expiry and activation of a confirmed order are
  // read against it, and an unreadable clock means the node is unavailable.
  const daaP = readClock(s).then((c) => c.daa, (e) => { throw describeNodeError(e, 'read'); });
  daaP.catch(() => undefined);
  // The ticket's reads go before the page's other reads in the indexer queue (data/indexer.ts).
  const hi = { priority: 'high' } as const;
  const [bv0, trades] = await Promise.all([
    ix.book(market.covenantId, { depth: 100, aggregate: false }, hi).catch(() => null),
    ix.trades(market.covenantId, { limit: 1 }, hi).catch(() => null),
  ]);
  const daa = bv0 ? await daaP : undefined;
  const checked = bv0
    ? await verifyBook(
        {
          kob: s.kob, node: s.node, indexer: ix, toAddress: (spk) => spkStringToAddress(s.sdk, spk, s.config.network), inScale: (r) => sameScale(market, r),
          market: { covenantId: market.covenantId, templateHash: market.templateHash, extensionCommitment: market.extensionCommitment },
          ...(daa !== undefined ? { daa } : {}), ...(o.depth !== undefined ? { depth: o.depth } : {}),
        },
        bv0,
      )
    : null;
  const bv = checked?.view ?? null;
  // The planners get only what the node confirmed. The console has the rest, and the user is not
  // told, because a note on every refresh was noise.
  if (checked?.dropped.length) console.info(`book check: ${checked.dropped.length} order(s) of the indexer's book left out (${[...new Set(checked.dropped.map((d) => d.reason))].join(', ')})`);
  if (checked?.beyond) console.info(`book check: ${checked.beyond} order(s) behind the verified depth are not part of the book`);
  const last = trades?.items?.[0];
  // Trades are priced per `price_basis` base units (the standard scale). Convert to the market's
  // scale, per whole token.
  const lastFillPrice = last && trades.price_basis && BigInt(trades.price_basis) > 0n ? (BigInt(last.price) * market.scale) / BigInt(trades.price_basis) : null;
  return { book: bv ? bookFromIndexer(market, bv) : { asks: [], bids: [] }, dropped: checked?.dropped ?? [], bookUnavailable: !bv || checked!.unread.length > 0, lastFillPrice, ...(checked ? { orders: checked.orders } : {}) };
}

/** Reads the node part of the environment. */
export async function readCoinsPart(s: Pick<Services, 'node' | 'utxos' | 'fees'> & Partial<Pick<Services, 'indexer'>>, wallet: Pick<WalletInfo, 'pubkey'>): Promise<CoinsPart> {
  const [clock0, funding, fees] = await Promise.all([readAgreedClock(s), s.utxos.fundingFor(wallet.pubkey), readFees(s)]);
  // the node's DAA is not below a coin it reported itself
  const floor = floorOf(funding as never);
  const clock = floor !== null && clock0.daa < floor ? { ...clock0, witness: 'disagree' as const, witnessReason: `the node's DAA ${clock0.daa} is below ${floor}, the DAA of a coin it reported itself` } : clock0;
  return { clock, funding, fees };
}

/** Reads the wallet's token UTXOs and resting orders. Throws when the token is not tradable. */
export async function readHoldingsPart(
  s: Pick<Services, 'indexer' | 'tracker' | 'standalone'>,
  wallet: Pick<WalletInfo, 'pubkey'>,
  token: TokenInfo,
  market: TokenMarket,
  o: Pick<BuildEnvOptions, 'offline'> = {},
): Promise<HoldingsPart> {
  if (!token.program) throw new Error(`token ${token.ticker} is not tradable in this build`);
  // Standalone mode (data/standalone.ts) lists the wallet's orders from the book it can see, as
  // seen on the node or resolved from the wallet's receipts. No indexer is asked.
  const c = s.standalone ?? null;
  const own = () => c!.ownOrders(wallet.pubkey, token.covenantId);
  const mine = c?.state.active
    ? Promise.resolve(own())
    : s.indexer && !o.offline
      ? // A list cut at its page bound is not read: self-trade prevention needs every own order.
        s.indexer.allOrders({ maker: wallet.pubkey, token: token.covenantId, status: 'active', limit: 200 }, { priority: 'high', maxPages: OWN_LIST_PAGES }).then(incompleteAsNull, async (e: unknown) => {
          // The indexer is down. Once standalone mode is on, its book lists the wallet's orders,
          // and the failed read starts its check.
          return c && typeof c.readFailed === 'function' && (await c.readFailed(e)) ? own() : null;
        })
      : Promise.resolve([]);
  const [tokenUtxos, orders] = await Promise.all([s.tracker.tokenUtxosFor(wallet.pubkey, { covenantId: token.covenantId, program: token.program }), mine]);
  return { tokenUtxos, ownOrders: orders ? ownOrderRefs(market, orders) : null };
}

/**
 * Pages of 200 of the wallet's own active orders of one token the ticket reads for self-trade
 * prevention: 10,000 orders. A list cut there is not read (R8B-L3). Only orders that name the wallet
 * as maker make it long, and a maker with more live orders of one token than that is refused until
 * some end.
 */
export const OWN_LIST_PAGES = 50;

/** Pages of 200 active orders read per token for the pair ticket's book check. */
export const PAIR_LIST_PAGES = 20;

/**
 * The resting pair orders of A/B for the pair book check, or null when they cannot be known. A pair
 * order is listed under both of its tokens, so the active orders of A and of B are read side by
 * side, at most PAIR_LIST_PAGES of 200 each. A complete list of either holds every pair order of
 * A/B. When both are cut (more than 4,000 active orders of each token), what was read is used only
 * if it holds, on each side, at least as many A/B pair orders as the indexer's own pair book counts
 * in its direct and entry levels; otherwise the book is unavailable. Many orders of one token can
 * no longer shut the pair ticket (R8B-M2); only flooding both tokens while the pair's own orders are
 * old can, and then the order waits, it is never planned against a book that may miss them
 * (R7B-L3). The indexer's pair book names no covenant ids, so its orders cannot be read one by one.
 */
async function pairOrdersOf(indexer: NonNullable<Services['indexer']>, a: Hex, b: Hex, pv: PairBookView): Promise<OrderView[] | null> {
  const list = (token: Hex) => indexer.allOrders({ token, status: 'active', limit: 200 }, { priority: 'high', maxPages: PAIR_LIST_PAGES }).catch(() => null);
  const [la, lb] = await Promise.all([list(a), list(b)]);
  const whole = [la, lb].find((l) => l && !l.truncated);
  if (whole) return whole;
  if (!la && !lb) return null;
  const byId = new Map<Hex, OrderView>();
  for (const v of [...(la ?? []), ...(lb ?? [])]) byId.set(v.covenant_id, v);
  const read = [...byId.values()];
  const ofPair = (side: 'ask' | 'bid') => read.filter((v) => v.pair && v.pair.base.toLowerCase() === a.toLowerCase() && v.pair.quote.toLowerCase() === b.toLowerCase() && v.pair.side === side).length;
  const named = (levels: readonly PairLevelView[]) => levels.filter((l) => l.source !== 'route').reduce((n, l) => n + Math.max(0, Math.trunc(Number(l.orders) || 0)), 0);
  return ofPair('ask') >= named(pv.asks) && ofPair('bid') >= named(pv.bids) ? read : null;
}

/**
 * A PlanEnv of the three parts. With `holdings` null, which means not read yet, there are no token
 * UTXOs and no own orders, and this is not reported as a guard the user must waive. The ticket
 * says the holdings are still being read, and the confirmation re-checks the plan against them
 * before anything is signed. Inputs this page has already spent in a submitted transaction are
 * dropped (app/pending-spends.ts), however old the parts are.
 */
export function assemblePlanEnv(
  s: Pick<Services, 'kob' | 'config'>,
  wallet: Pick<WalletInfo, 'pubkey'>,
  market: TokenMarket,
  parts: { market: MarketPart; coins: CoinsPart; holdings: HoldingsPart | null },
  o: BuildEnvOptions = {},
): PlanEnv {
  // A guard whose data could not be read is reported, so the planner refuses the order until a
  // read succeeds. It is never silently switched off.
  const guardsUnavailable: ('own-orders' | 'book')[] = [];
  if (parts.market.bookUnavailable) guardsUnavailable.push('book');
  if (parts.holdings && parts.holdings.ownOrders === null) guardsUnavailable.push('own-orders');
  // An order this page has just taken, its outpoint spent by a transaction not yet in a block, is
  // gone. It is out of the book and of what can be taken.
  const orders = new Map<string, BookOrderUtxo>();
  const taken = new Set<string>();
  for (const [k, o] of parts.market.orders ?? []) {
    if (pendingSpends.has(o.order.transactionId, o.order.index)) taken.add(k);
    else orders.set(k, o);
  }
  return {
    kob: s.kob, token: market, maker: wallet.pubkey, clock: parts.coins.clock, book: bookWithout(parts.market.book, taken), ...(orders.size ? { bookOrders: orders } : {}), ownOrders: parts.holdings?.ownOrders ?? [],
    ...(guardsUnavailable.length ? { guardsUnavailable } : {}), lastFillPrice: parts.market.lastFillPrice,
    funding: pendingSpends.funding(parts.coins.funding, wallet.pubkey), tokenUtxos: pendingSpends.tokens(parts.holdings?.tokenUtxos ?? [], wallet.pubkey, market.covenantId),
    freshTxids: freshTxids(),
    ...feeModeOf(s), fees: parts.coins.fees, ...(o.feeRate ? { feeRate: o.feeRate } : {}), ...(o.carrier ? { carrier: o.carrier } : {}),
  };
}

/**
 * Refuses a token whose registry entry (or market scale) and KCC-23 declaration name different
 * decimals. The same base units would mean other whole-token amounts and prices, and neither side
 * is silently preferred. app/token-display.ts shows the on-chain value, flagged. An open-list
 * token takes the declared decimals once the declaration is known (kob/open-token.ts), and waits
 * until then.
 */
function assertDeclaredDecimals(s: Services, token: TokenInfo): void {
  const d = declaredToken(s.config.network, token.covenantId)?.decimals;
  if (d === undefined || d === null) return;
  if (d !== token.decimals) {
    throw new Error(`token ${token.ticker}: its ${token.openList ? 'market' : 'registry entry'} says ${token.decimals} decimals, what it recorded on chain says ${d}; orders wait until they agree`);
  }
}

/**
 * `s` with its indexer fixed to the one in use now. With failover (data/indexer-failover.ts),
 * `s.indexer` follows the active indexer from call to call, but the book and the wallet's orders
 * of one environment must come from the same one. Services without failover are returned as they
 * are.
 */
export function pinIndexer(s: Services): Services {
  const pinned = typeof s.pinnedIndexer === 'function' ? s.pinnedIndexer() : s.indexer;
  if (!s.failover || pinned === s.indexer) return s;
  return Object.create(s, { indexer: { value: pinned, enumerable: true } }) as Services;
}

/**
 * Everything `planOrder` needs for one token and the connected wallet. Rejects when the node is
 * unreachable. The three parts do not depend on each other, so they are read in one round.
 */
export async function buildPlanEnv(services: Services, wallet: Pick<WalletInfo, 'pubkey'>, token: TokenInfo, o: BuildEnvOptions = {}): Promise<PlanEnv> {
  const s = pinIndexer(services);
  if (!token.tradable || !token.program) throw new Error(`token ${token.ticker} is not tradable in this build (${token.untradableReason ?? 'unsupported'})`);
  assertDeclaredDecimals(s, token);
  const market = tokenMarketOf(s, token);
  // The market read rejects only when the node cannot be asked. A failure of the coins first must
  // not leave that unhandled.
  const marketPart = readMarketPart(s, market, o);
  marketPart.catch(() => undefined);
  const holdings = readHoldingsPart(s, wallet, token, market, o);
  holdings.catch(() => undefined);
  const coins = await readCoinsPart(s, wallet);
  return assemblePlanEnv(s, wallet, market, { market: await marketPart, coins, holdings: await holdings }, o);
}

/**
 * `e` without the inputs this page has already spent in a submitted transaction, plus the KAS
 * and token change of those the node does not list yet (app/pending-spends.ts). For an environment
 * read before the spend.
 */
export function dropPendingSpends<E extends PlanEnv>(e: E): E {
  const funding = pendingSpends.funding(e.funding, e.maker);
  const tokenUtxos = pendingSpends.tokens(e.tokenUtxos, e.maker, e.token.covenantId);
  const pair = isPairEnv(e) ? { ...e.pair, quoteTokenUtxos: pendingSpends.tokens(e.pair.quoteTokenUtxos, e.maker, e.pair.quote.covenantId) } : null;
  // The transactions sent since `e` was read make fresh coins too.
  const fresh = freshTxids();
  const sameFresh = fresh.size === (e.freshTxids?.size ?? 0) && [...fresh].every((id) => e.freshTxids!.has(id));
  if (funding === e.funding && tokenUtxos === e.tokenUtxos && sameFresh && (!pair || pair.quoteTokenUtxos === (e as unknown as PairPlanEnv).pair.quoteTokenUtxos)) return e;
  return { ...e, funding, tokenUtxos, freshTxids: fresh, ...(pair ? { pair } : {}) };
}

/** The environment of the cancel, cancel-replace and refund planners. */
export async function buildCancelEnv(s: Services, wallet: Pick<WalletInfo, 'pubkey'>, token?: Pick<TokenInfo, 'covenantId' | 'program'>): Promise<CancelEnv> {
  const [clock, funding, tokenUtxos, fees] = await Promise.all([
    readClock(s),
    s.utxos.fundingFor(wallet.pubkey),
    token && token.program ? s.tracker.tokenUtxosFor(wallet.pubkey, { covenantId: token.covenantId, program: token.program }) : Promise.resolve([]),
    readFees(s),
  ]);
  return {
    kob: s.kob, maker: wallet.pubkey, funding: pendingSpends.funding(funding, wallet.pubkey), tokenUtxos: pendingSpends.tokens(tokenUtxos, wallet.pubkey, token?.covenantId), clock: { daa: clock.daa, unixSeconds: clock.unixSeconds },
    ...feeModeOf(s), fees,
  };
}

// Token and token pairs.

/**
 * The KAS references of a token, in sompi per whole token of `market`'s scale.
 * - `book` is the midpoint of its KAS book's touch, or the one side when only one exists.
 * - `last` is its newest KAS trade (indexer trades priced per `price_basis`, converted to the
 *   market's scale).
 * Each is null where the indexer has nothing or failed. A reference is a default, never a guard.
 */
export async function kasReferenceOf(indexer: Pick<NonNullable<Services['indexer']>, 'book' | 'trades'>, market: TokenMarket): Promise<{ book: bigint | null; last: bigint | null }> {
  const [bv, trades] = await Promise.all([
    indexer.book(market.covenantId, { depth: 20, aggregate: true }).catch(() => null),
    indexer.trades(market.covenantId, { limit: 1 }).catch(() => null),
  ]);
  // A quote nobody has to take does not set the reference. That is an ask far above the bids, the
  // only side of the book, or a bid worth less than one default minimum fill above the others.
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
 * Everything `planOrder` needs to plan on the pair `base`/`quote` (A/B, oriented) for the
 * connected wallet. It holds:
 * - the node clock, the maker's KAS funding and the UTXOs of both tokens;
 * - the pair book (`indexer.pairBook`, every source: direct, entry and route) as a BookView in B
 *   per whole A;
 * - the wallet's live orders in this pair, for self-trade prevention;
 * - the KAS references of A and B, from their KAS books and else their newest trades, which set
 *   the default minimum fill;
 * - the rate the newest KAS trades imply (`lastFillPrice`), cross-checked against the pair book.
 *   Pair fills never make a price.
 * A guard whose data could not be read is reported in `guardsUnavailable`, as in `buildPlanEnv`.
 * Rejects when the node is unreachable or the two tokens are not two different tradable tokens.
 */
export async function buildPairPlanEnv(services: Services, wallet: Pick<WalletInfo, 'pubkey'>, base: TokenInfo, quote: TokenInfo, o: BuildEnvOptions = {}): Promise<PairPlanEnv> {
  const s = pinIndexer(services);
  for (const t of [base, quote]) {
    if (!t.tradable || !t.program) throw new Error(`token ${t.ticker} is not tradable in this build (${t.untradableReason ?? 'unsupported'})`);
    assertDeclaredDecimals(s, t);
  }
  if (base.covenantId === quote.covenantId) throw new Error('a pair needs two different tokens');
  const a = tokenMarketOf(s, base);
  const b = tokenMarketOf(s, quote);
  // Read beside the wallet's coins, not after them, as in `buildPlanEnv`. Every read settles, and
  // a failure is null.
  const indexer = s.indexer;
  const indexed =
    indexer && !o.offline
      ? Promise.all([
          indexer.pairBook(a.covenantId, b.covenantId, { depth: 100 }, { priority: 'high' }).catch(() => null),
          indexer.allOrders({ maker: wallet.pubkey, token: a.covenantId, status: 'active', limit: 200 }, { priority: 'high', maxPages: OWN_LIST_PAGES }).then(incompleteAsNull, () => null),
          kasReferenceOf(indexer, a),
          kasReferenceOf(indexer, b),
        ])
      : null;
  // The two KAS books the node confirms (app/book-verify.ts). The route of the pair book is
  // derived from them and never taken from the indexer's levels.
  const kasBooks = indexer && !o.offline ? Promise.all([readMarketPart(s, a), readMarketPart(s, b)]) : null;
  // A malformed reference rejects the build when it is awaited below. Until then a node failure
  // must not leave that rejection unhandled.
  indexed?.catch(() => undefined);
  kasBooks?.catch(() => undefined);
  const [clock, funding, tokenUtxos, quoteTokenUtxos, fees] = await Promise.all([
    readAgreedClock(s),
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
  if (indexed) {
    const [pv, mine, refA, refB] = await indexed;
    const [ma, mb] = await kasBooks!;
    // An indexer without the pair routes (null) or a failed read leaves the crossing, FOK and band
    // guards without a book.
    if (pv) {
      view = pv;
      // The planners get what the node confirms (app/pair-book.ts). That is the resting pair
      // orders one by one, and the route from the two KAS books.
      const hasOrders = [...pv.asks, ...pv.bids].some((l) => l.source !== 'route');
      const listed = hasOrders && indexer ? await pairOrdersOf(indexer, a.covenantId, b.covenantId, pv) : [];
      if (listed === null) guardsUnavailable.push('book');
      else {
        const checked = await verifyPairBook(
          {
            kob: s.kob, node: s.node, indexer: indexer!, toAddress: (spk) => spkStringToAddress(s.sdk, spk, s.config.network), daa: clock.daa, ...(o.depth !== undefined ? { depth: o.depth } : {}),
            base: { covenantId: a.covenantId, templateHash: a.templateHash, extensionCommitment: a.extensionCommitment, scale: a.scale },
            quote: { covenantId: b.covenantId, templateHash: b.templateHash, extensionCommitment: b.extensionCommitment, scale: b.scale },
          },
          listed,
          routeLevels(ma.book, mb.book, a.scale, b.scale),
        );
        book = checked.book;
        // A side of direct orders that could not be read is unknown, not empty (R7B-L2).
        if (checked.unread.length) guardsUnavailable.push('book');
        if (checked.dropped.length) console.info(`pair book check: ${checked.dropped.length} order(s) of the indexer's pair book left out (${[...new Set(checked.dropped.map((d) => d.reason))].join(', ')})`);
        if (checked.beyond) console.info(`pair book check: ${checked.beyond} order(s) behind the verified depth are not part of the book`);
      }
    } else guardsUnavailable.push('book');
    // The route is derived from the two KAS books: one that could not be read or checked leaves
    // it unknown, not empty.
    if ((ma.bookUnavailable || mb.bookUnavailable) && !guardsUnavailable.includes('book')) guardsUnavailable.push('book');
    if (mine) ownOrders = ownPairOrderRefs(mine, a.covenantId, b.covenantId);
    else guardsUnavailable.push('own-orders');
    // The KAS price of each token sets the default minimum fill. It comes from the KAS book the
    // node confirmed, since the indexer's own book is nobody's word for a signed field, and else
    // from the newest trade the indexer reports.
    kasPerWholeA = firmReferencePrice(ma.book, a.scale) ?? refA.last;
    kasPerWholeB = firmReferencePrice(mb.book, b.scale) ?? refB.last;
    lastFillPrice = impliedPairRate(refA.last, refB.last, b.scale);
  }
  return {
    kob: s.kob, token: a, maker: wallet.pubkey, clock, book, ownOrders, ...(guardsUnavailable.length ? { guardsUnavailable } : {}), lastFillPrice,
    funding: pendingSpends.funding(funding, wallet.pubkey), tokenUtxos: pendingSpends.tokens(tokenUtxos, wallet.pubkey, a.covenantId), freshTxids: freshTxids(),
    ...feeModeOf(s), fees, ...(o.feeRate ? { feeRate: o.feeRate } : {}), ...(o.carrier ? { carrier: o.carrier } : {}),
    pair: { quote: b, quoteTokenUtxos: pendingSpends.tokens(quoteTokenUtxos, wallet.pubkey, b.covenantId), view, kasPerWholeA, kasPerWholeB },
  };
}
