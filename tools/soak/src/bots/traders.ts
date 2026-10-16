// Trader bots. Each trader wakes up at exponential intervals and performs one action drawn from a weighted mix that covers EVERY order
// type of docs/spec/order-types.md (market with auction, limit GTC / GTD / day, timed activation, IOC, FOK, streaming, stop-market,
// stop-limit, trailing stop, take-profit, OCO, IFD both ways, IFO, IFO with a stop entry, repeat IFD / IFO, TWAP, DCA, Dutch and rising
// bid, close) plus order management (cancel, cancel-replace, cancel-all). GTD / IOC orders exercise the keepers' refunds and kills.
// Prices are drawn around the Binance-derived reference and the live book, sizes are small and random. A share of the actions (each asset's
// `pairShare`) are PAIR orders of the asset token (A) against TUSD (B) through the web planner (`planOrder` with a PairPlanEnv): limits on
// both sides, IOC and market, Dutch, TWAP, stop-market and trailing stops, OCO, IFD buy-first and sell-first, repeat IFD; they rest, cross
// each other (the matcher nets them), route through the two KAS books and arm the pair stops (both evidence modes).
// Scenarios of the 2026-10-13 gap review (each through the web code a user runs; checkable ones recorded in run/state/scenarios.jsonl for the
// checker): limit ladders of 2 to 20 same-size rungs (the ticket's ladder, one order and transaction per rung) and the cancel of one rung, GTC
// renewal (My orders' renew), token merge (My orders' "Merge tokens", a chain 3 -> 1, then 1 + 2 -> 1), the pair amend (the pair ticket's
// replace), the pair cancel-all, dated pair limits (GTD, day, timed activation), and the KAS book of external tokens with a `trade` block (KRON).
import type { Intent } from '@/kob/plan';
import type { BookView, TokenMarket } from '@/kob/plan-types';
import type { OrderState } from '@/kob/types';
import { describeOrder } from '@/kob/order-facts';
import { amendKind, isGtcExpiry, orderTypeKey } from '@/ui/orders/orders-model';
import { expandLimitLadder } from '@/ui/ticket/ladder';
import { pairRateOf, pairReference } from './pair-math';
import type { OrderView } from '@/data/indexer-types';
import type { TraderConfig } from '../config';
import type { Env } from '../env';
import { errText, type Logger, type Stats } from '../log';
import { isPair, myOrders, readBook, readClock, tokenMarket, touch, type AssetBook, type ExternalBook } from '../market';
import type { RefSource } from '../price';
import { recordScenario } from '../scenarios';
import { bps, expDelay, onTick, pick, randInt, randUnits, sleep, usdToUnits, weighted } from '../util';
import { walletFor, type BotWallet } from '../wallet';
import { amend, cancel, cancelAll, cancelAllDetailed, mergeTokens, pairAmend, priceOf, place, placePair, renew, spendsOf, type PlaceResult, type ReplaceOutcome } from './common';
import { DEFAULT_PAIR_WEIGHTS, DEFAULT_WEIGHTS } from './weights';
import { bandsOf, capToDepth } from './bands';
import { datedPairLimit, externalReference, ladderLevels, ladderSetting, mergeTotal, oldestFirst, pairAmendPrice, rungSize } from './scenario-math';
import { consolidateTokens } from './consolidate';
import { consolidateConfig } from './consolidate-math';
import { fanoutTarget } from './fanout';

const side = (): 'buy' | 'sell' => (Math.random() < 0.5 ? 'buy' : 'sell');
const frac = (lo: number, hi: number) => lo + Math.random() * (hi - lo);

export class Trader {
  readonly w: BotWallet;
  private stopped = false;
  private readonly weights: Record<string, number>;
  /** how far from the reference the takers and auctions go (bands.ts) */
  private readonly bands: ReturnType<typeof bandsOf>;

  constructor(
    private readonly env: Env,
    private readonly cfg: TraderConfig,
    private readonly feed: RefSource,
    private readonly log: Logger,
    readonly stats: Stats,
    /** the soak books beside TUSD (TETH, TBTC): their markets, configs and references */
    private readonly assets: AssetBook[] = [],
    /** the external tokens with a `trade` block (a KRON token): their KAS books, no pair orders */
    private readonly external: ExternalBook[] = [],
  ) {
    this.w = walletFor(env, cfg.key, stats, log);
    this.weights = { ...DEFAULT_WEIGHTS, ...(cfg.weights ?? {}) };
    this.bands = bandsOf(env.cfg.bands);
  }

  stop(): void {
    this.stopped = true;
  }

  async run(): Promise<void> {
    await sleep(randInt(5, 40) * 1000);
    while (!this.stopped) {
      // no trading against a book that lags the chain: wait here instead of planning and being refused at submit
      if (!(await this.env.gate?.waitOpen(() => this.stopped) ?? true)) break;
      const action = weighted(this.weights);
      try {
        await this.act(action);
      } catch (e) {
        this.stats.inc(`act_error:${action}`);
        this.log.warn('action failed', { action, error: errText(e) });
      }
      await this.consolidate();
      await sleep(expDelay(this.cfg.meanIntervalSec, 3));
    }
  }

  /** when this trader last looked at its token UTXOs for a merge */
  private lastTokenMerge = 0;

  /**
   * merges this trader's surplus token UTXOs (own loop: never concurrent with its own action); rate-limited per key and token. The token UTXOs
   * are looked at once per `intervalSec` (the merge rate window), not after every action: each look reads every soak token's UTXOs from the
   * indexer, and one pass merges whatever a window's actions added (up to the window's cap of merge transactions)
   */
  private async consolidate(): Promise<void> {
    const cc = consolidateConfig(this.env.cfg.consolidate);
    if (!cc.enabled || !cc.traders) return;
    if (Date.now() - this.lastTokenMerge >= cc.intervalSec * 1000) {
      this.lastTokenMerge = Date.now();
      const ms = [tokenMarket(this.env), ...this.assets.map((a) => a.m)];
      for (const m of ms) await consolidateTokens(this.env, this.w, m, fanoutTarget(this.env, 'trader', m), cc);
    }
    await this.w.mergeKas({ above: cc.kasAbove, maxInputs: cc.kasInputs });
  }

  /** the book being traded: its decimals and the USD value of one whole token (TUSD: 1, an asset token: its reference) */
  private book = { decimals: 8, usd: 1 };

  /** base units worth `usd` dollars of the traded token */
  private usd(usd: number): bigint {
    return usdToUnits(usd, this.book.usd, this.book.decimals);
  }

  /** a random order size: any amount of base units worth between half a dollar and `maxUsd` (default: the trader's `maxUsd`) */
  private size(maxUsd = this.cfg.maxUsd): bigint {
    return randUnits(this.usd(0.5), this.usd(Math.max(0.5, maxUsd)));
  }

  private async act(action0: string): Promise<void> {
    const env = this.env;
    // which market: a pair order of an asset token against TUSD (each asset's `pairShare`), an asset token's KAS book (its `bookShare`), or the
    // primary (TUSD) book (the rest)
    let r = Math.random();
    for (const a of this.assets) {
      const share = a.cfg.pairShare ?? a.cfg.crossShare ?? 0;
      if (r < share) return this.pair(tokenMarket(env), a);
      r -= share;
    }
    // an external token's KAS book (`externalTokens[].trade.bookShare`, a KRON token), then the asset books (`bookShare`), else TUSD
    let asset: { m: TokenMarket; feed: RefSource } | null = null;
    let externalBook = false;
    for (const x of this.external) {
      if (r < x.trade.bookShare) {
        asset = x;
        externalBook = true;
        break;
      }
      r -= x.trade.bookShare;
    }
    if (!asset) {
      for (const a of this.assets) {
        if (r < a.cfg.bookShare) {
          asset = a;
          break;
        }
        r -= a.cfg.bookShare;
      }
    }
    const m = asset ? asset.m : tokenMarket(env);
    const ref = (asset ? asset.feed : this.feed).get();
    if (!ref) return;
    // stat tags: the primary book keeps the plain action names, an asset book's carry '@<ticker>'
    const action = asset ? `${action0}@${m.ticker}` : action0;
    this.book = { decimals: m.decimals, usd: ref.usd };
    const tick = m.tick;
    const book: BookView = await readBook(env, m);
    // sompi per whole token: every intent price below is in this unit (an external token: its book's midpoint when near its configured reference)
    const R = externalBook ? externalReference(touch(book), ref.perToken, 2000) : ref.perToken;
    const at = (b: number) => onTick(R + bps(R, b), tick);
    // the trader's own orders of this book, less those its pending cancels / amends already spend (the listing lags the chain)
    const mine: OrderView[] = this.w.actionable(await myOrders(env, this.w.pk, m).catch(() => [] as OrderView[]));
    const t = touch(book);
    const clock = await readClock(env);
    // taker: a limit that is marketable when planned is placed as an IOC at its own price (a taker with that slippage bound), never as a resting or auctioning limit
    const o = { book, mine, taker: true };
    const s = side();
    const amount = this.size();
    const B = this.bands;
    // a taker takes at most a share of what rests on the other side near the touch (bands.ts), never less than the smallest order
    const taking = (a: bigint): bigint => capToDepth(a, s === 'buy' ? book.asks : book.bids, B.depthFraction, B.depthBandBps, this.usd(0.5));
    let intent: Intent | null = null;

    switch (action0) {
      case 'market':
        intent = { type: 'market', side: s, amount: taking(this.size(3)), slippageBps: BigInt(B.marketBps) };
        break;
      case 'limit':
        // passive: a little behind the reference
        intent = { type: 'limit', side: s, amount, price: at(s === 'buy' ? -frac(5, 60) : frac(5, 60)), ...(s === 'buy' ? { maxFills: 3n } : {}) };
        break;
      case 'limitCross': {
        // aggressive limit through the touch: placed as an IOC taker at that price (the slippage bound), see common.ts `taker`
        const lim = s === 'buy' ? (t.ask ?? R) + bps(t.ask ?? R, B.crossBps) : (t.bid ?? R) - bps(t.bid ?? R, B.crossBps);
        intent = { type: 'limit', side: s, amount: taking(this.size(3)), price: onTick(lim, tick) };
        break;
      }
      case 'gtd':
        intent = {
          type: 'limit', side: s, amount, price: at(s === 'buy' ? -frac(40, 120) : frac(40, 120)),
          lifetime: { kind: 'gtd', at: clock.unixSeconds + BigInt(randInt(10, 40) * 60) },
          ...(s === 'buy' ? { maxFills: 2n } : {}),
        };
        break;
      case 'day':
        intent = { type: 'limit', side: s, amount, price: at(s === 'buy' ? -frac(30, 150) : frac(30, 150)), lifetime: { kind: 'day' } };
        break;
      case 'timed':
        intent = {
          type: 'limit', side: s, amount, price: at(s === 'buy' ? -frac(0, 40) : frac(0, 40)),
          activeFrom: { unixSeconds: clock.unixSeconds + BigInt(randInt(90, 360)) },
          lifetime: { kind: 'gtd', at: clock.unixSeconds + 3600n },
        };
        break;
      case 'ioc': {
        const p = s === 'buy' ? t.ask : t.bid;
        if (p == null) return;
        intent = { type: 'ioc', side: s, amount: taking(this.size(3)), price: onTick(s === 'buy' ? p + bps(p, 10) : p - bps(p, 10), tick) };
        break;
      }
      case 'fok': {
        const p = s === 'buy' ? t.ask : t.bid;
        if (p == null) return;
        intent = { type: 'fok', side: s, amount: taking(this.size(2)), price: onTick(s === 'buy' ? p + bps(p, B.crossBps) : p - bps(p, B.crossBps), tick) };
        break;
      }
      case 'streaming': {
        const p = s === 'buy' ? t.ask : t.bid;
        if (p == null) return;
        intent = { type: 'streaming', side: s, amount: taking(this.size(2)), displayedPrice: p, toleranceBps: BigInt(B.streamingBps) };
        break;
      }
      case 'stopMarket':
        intent = { type: 'stopMarket', side: s, amount: this.size(3), stop: at(s === 'sell' ? -frac(8, 40) : frac(8, 40)), slipBps: B.stopBps, expiry: { kind: 'gtdUnix', atUnixSeconds: clock.unixSeconds + 6n * 3600n } };
        break;
      case 'stopLimit': {
        const stop = at(s === 'sell' ? -frac(8, 40) : frac(8, 40));
        const limit = onTick(s === 'sell' ? stop - bps(stop, B.stopLimitBps) : stop + bps(stop, B.stopLimitBps), tick);
        intent = { type: 'stopLimit', side: s, amount: this.size(3), stop, limit, expiry: { kind: 'gtdUnix', atUnixSeconds: clock.unixSeconds + 6n * 3600n } };
        break;
      }
      case 'trailingStop': {
        const gap = onTick(bps(R, 30), tick);
        intent = {
          type: 'trailingStop', side: s, amount: this.size(2), stop: s === 'sell' ? onTick(R - gap, tick) : onTick(R + gap, tick),
          trail: { step: onTick(bps(R, 10), tick), gap, wait: 600n, expectedUpdates: 10 }, slipBps: B.stopBps,
          expiry: { kind: 'gtdUnix', atUnixSeconds: clock.unixSeconds + 6n * 3600n },
        };
        break;
      }
      case 'takeProfit':
        intent = { type: 'takeProfit', side: s, amount: this.size(3), price: at(s === 'sell' ? frac(40, 150) : -frac(40, 150)) };
        break;
      case 'oco':
        intent = {
          type: 'oco', side: s, amount: this.size(3),
          takeProfit: at(s === 'sell' ? frac(60, 150) : -frac(60, 150)),
          stop: at(s === 'sell' ? -frac(10, 50) : frac(10, 50)),
          slipBps: B.stopBps,
        };
        break;
      case 'ifd':
        intent = s === 'buy'
          ? { type: 'ifd', side: 'buy', amount: this.size(3), entry: { price: at(-frac(5, 40)) }, exit: { takeProfit: at(frac(30, 80)) } }
          : { type: 'ifd', side: 'sell', amount: this.size(3), entry: { price: at(frac(5, 40)) }, exit: { takeProfit: at(-frac(30, 80)) } };
        break;
      case 'ifo':
        intent = s === 'buy'
          ? { type: 'ifo', side: 'buy', amount: this.size(3), entry: { price: at(-frac(5, 40)) }, exit: { takeProfit: at(frac(40, 90)), stop: at(-frac(15, 45)), slipBps: B.stopBps } }
          : { type: 'ifo', side: 'sell', amount: this.size(3), entry: { price: at(frac(5, 40)) }, exit: { takeProfit: at(-frac(40, 90)), stop: at(frac(15, 45)), slipBps: B.stopBps } };
        break;
      case 'ifoStopEntry':
        // breakout entry: buy stop above the market (stop <= price), sell stop below (stop >= price); the entry's auction runs from the stop to a
        // price `entryBandBps` beyond it
        {
          const off = frac(10, 35);
          intent = s === 'buy'
            ? { type: 'ifo', side: 'buy', amount: this.size(2), entry: { stop: at(off), price: at(off + B.entryBandBps) }, exit: { takeProfit: at(frac(160, 220)), stop: at(-frac(20, 60)), slipBps: B.stopBps } }
            : { type: 'ifo', side: 'sell', amount: this.size(2), entry: { stop: at(-off), price: at(-off - B.entryBandBps) }, exit: { takeProfit: at(-frac(160, 220)), stop: at(frac(20, 60)), slipBps: B.stopBps } };
        }
        break;
      case 'repeatIfd':
        intent = s === 'buy'
          ? { type: 'repeatIfd', side: 'buy', amount: this.size(2), entry: { price: at(-frac(5, 25)) }, exit: { takeProfit: at(frac(20, 45)) }, repeat: { count: BigInt(randInt(2, 4)) } }
          : { type: 'repeatIfd', side: 'sell', amount: this.size(2), entry: { price: at(frac(5, 25)) }, exit: { takeProfit: at(-frac(20, 45)) }, repeat: { count: BigInt(randInt(2, 4)) } };
        break;
      case 'repeatIfo':
        intent = s === 'buy'
          ? { type: 'repeatIfo', side: 'buy', amount: this.size(2), entry: { price: at(-frac(5, 25)) }, exit: { takeProfit: at(frac(20, 45)), stop: at(-frac(120, 200)), slipBps: B.stopBps }, repeat: { count: 2n } }
          : { type: 'repeatIfo', side: 'sell', amount: this.size(2), entry: { price: at(frac(5, 25)) }, exit: { takeProfit: at(-frac(20, 45)), stop: at(frac(120, 200)), slipBps: B.stopBps }, repeat: { count: 2n } };
        break;
      case 'twap':
        intent = { type: 'twap', side: 'sell', amount: randUnits(this.usd(3), this.usd(6)), sliceAmount: this.usd(1), interval: { seconds: BigInt(randInt(60, 150)) }, price: at(-frac(0, 30)), lifetime: { kind: 'gtd', at: clock.unixSeconds + 2n * 3600n } };
        break;
      case 'dca':
        intent = { type: 'dca', side: 'buy', amount: randUnits(this.usd(3), this.usd(6)), sliceAmount: this.usd(1), interval: { seconds: BigInt(randInt(60, 150)) }, price: at(frac(0, 30)), maxFills: 6n, lifetime: { kind: 'gtd', at: clock.unixSeconds + 2n * 3600n } };
        break;
      case 'dutch':
        intent = s === 'sell'
          ? { type: 'dutch', side: 'sell', amount: this.size(3), price: at(frac(...B.dutchStartBps)), priceEnd: at(-frac(...B.dutchEndBps)), duration: { seconds: BigInt(randInt(300, 900)) } }
          : { type: 'dutch', side: 'buy', amount: this.size(3), price: at(-frac(...B.dutchStartBps)), priceEnd: at(frac(...B.dutchEndBps)), duration: { seconds: BigInt(randInt(300, 900)) } };
        break;
      case 'close':
        intent = { type: 'close', amount: this.size(2), slippageBps: BigInt(B.marketBps) };
        break;
      case 'cancel': {
        const live = mine.filter((v) => v.current && v.state_known && !v.parent);
        if (!live.length) return;
        const v = pick(live);
        const res = await cancel(env, this.w, v, `cancel:${v.contract}`);
        this.stats.inc(res.ok ? 'cancel_ok' : 'cancel_failed');
        return;
      }
      case 'amend': {
        const limits = mine.filter((v) => (v.contract === 'KobAsk' || v.contract === 'KobBid') && v.tif === 0 && v.state_known && !v.auction);
        if (!limits.length) return;
        const v = pick(limits);
        const p = priceOf(v);
        if (p == null) return;
        const np = onTick(p + bps(p, (v.side === 2 ? 1 : -1) * frac(5, 25)), tick);
        const res = await amend(env, this.w, v, np);
        this.stats.inc(res.ok ? 'amend_ok' : 'amend_failed');
        return;
      }
      case 'cancelAll': {
        const n = await cancelAll(env, this.w, mine.filter((v) => v.current && v.state_known));
        this.stats.inc('cancelAll_orders', n);
        return;
      }
      case 'ladder':
        return this.ladder(m, s, R, clock.unixSeconds, action);
      case 'ladderCancel':
        return this.ladderCancel(m, action);
      case 'renew':
        return this.renewOldest(m, mine, clock.daa, action);
      case 'mergeTokens':
        return this.merge(m, action);
      default:
        return;
    }
    if (!intent) return;
    const placed = intent;
    const res = await place(env, this.w, placed, action, o, m);
    // a real user pulls their own resting orders the order would trade against first (matcher.md 10.12: self-trade prevention is the
    // wallet's job): exactly the orders the refusal names are cancelled together, then the same intent is planned again
    await this.clearSelfTrade(res, () => place(env, this.w, placed, action, { taker: true }, m), (views) => cancelAll(env, this.w, views, 'selfTradeCancel'));
  }

  /** the ladders this trader placed (2 rungs or more), for `ladderCancel`: token and rung ids, newest last */
  private ladders: { token: string; rungs: string[] }[] = [];

  /**
   * A limit ladder through the ticket's ladder (web/src/ui/ticket/ladder.ts `expandLimitLadder`): 2 to 20 rungs of ONE size, the first a little
   * behind the reference, the next ones stepping away from it (a percent step or an end price), GTC or GTD 30 to 90 min. Every rung is its own
   * limit order placed by its own transaction, one after the other (as the ticket signs them); the ladder stops at the first rung refused or
   * not submitted, the rungs placed before it stay. Counters `ladder_placed`, `ladder_rungs`, `ladder_stopped`; record `ladder`.
   */
  private async ladder(m: TokenMarket, side: 'buy' | 'sell', R: bigint, nowUnix: bigint, tag: string): Promise<void> {
    const env = this.env;
    const levels = ladderLevels();
    const off = bps(R, frac(5, 40));
    const start = onTick(side === 'buy' ? R - off : R + off, m.tick);
    const set = ladderSetting(side, start, levels);
    // the ladder's budget is twice the trader's largest order, split evenly: every rung the same size, at least half a dollar
    const size = rungSize(this.usd(this.cfg.maxUsd * 2), levels, this.usd(0.5));
    const gtd = Math.random() < 0.5;
    const base: Intent = { type: 'limit', side, amount: size, price: start, ...(gtd ? { lifetime: { kind: 'gtd' as const, at: nowUnix + BigInt(randInt(30, 90) * 60) } } : {}) };
    const lad = expandLimitLadder(base, set, m.tick);
    if (!lad.ok) {
      this.stats.inc(`plan_refused:${tag}:ladder-${lad.errors[0]?.code ?? 'unknown'}`);
      return;
    }
    const rungs: { id: string; price: string; txid: string }[] = [];
    let stoppedAt: number | undefined;
    for (const l of lad.levels) {
      if (this.stopped) break;
      // as every trader limit: a rung marketable when planned (a book skewed past the reference) goes as a taker at its own price
      const res = await place(env, this.w, l.intent, tag, { taker: true }, m);
      const id = res.created?.[0];
      if (!res.ok || !res.txid || !id) {
        stoppedAt = l.level;
        break;
      }
      rungs.push({ id, price: l.entryPrice.toString(), txid: res.txid });
      // the next rung is planned from fresh coins (the ticket re-reads the chain before each signature): this rung's change is spendable once accepted
      await sleep(2000);
    }
    this.stats.inc('ladder_rungs', rungs.length);
    if (stoppedAt !== undefined) this.stats.inc('ladder_stopped');
    if (!rungs.length) return;
    this.stats.inc('ladder_placed');
    this.log.info('ladder placed', { tag, side, levels, placed: rungs.length, size, mode: set.mode, first: rungs[0]!.price, last: rungs[rungs.length - 1]!.price });
    recordScenario(env.cfg.runPath, { kind: 'ladder', ts: Date.now(), bot: this.w.name, token: m.covenantId, side, size: size.toString(), levels, rungs, ...(stoppedAt !== undefined ? { stoppedAt } : {}) });
    if (rungs.length >= 2) {
      this.ladders.push({ token: m.covenantId, rungs: rungs.map((x) => x.id) });
      if (this.ladders.length > 20) this.ladders.shift();
    }
  }

  /** Cancels ONE live rung of one of this trader's ladders on `m` (My orders' cancel of that order); record `ladderCancel` (the others stay). */
  private async ladderCancel(m: TokenMarket, tag: string): Promise<void> {
    const env = this.env;
    const own = this.ladders.filter((l) => l.token === m.covenantId);
    if (!own.length) return;
    const lad = pick(own);
    const views = await Promise.all(lad.rungs.map((id) => env.indexer.order(id).catch(() => null)));
    const live = this.w.actionable(views.filter((v): v is OrderView => !!v && (v.status === 'open' || v.status === 'partial') && !!v.current && v.state_known));
    if (live.length < 2) {
      // one rung or none left: nothing to show about the others
      this.ladders = this.ladders.filter((l) => l !== lad);
      return;
    }
    const v = pick(live);
    const spends = spendsOf(v);
    const res = await cancel(env, this.w, v, tag);
    this.stats.inc(res.ok ? 'ladderCancel_ok' : 'ladderCancel_failed');
    if (!res.ok || !res.txid) return;
    recordScenario(env.cfg.runPath, { kind: 'ladderCancel', ts: Date.now(), bot: this.w.name, token: m.covenantId, rungs: lad.rungs, cancelled: { id: v.covenant_id, spends }, txid: res.txid });
    lad.rungs = lad.rungs.filter((id) => id !== v.covenant_id);
  }

  /**
   * GTC renewal (My orders' renew, before day 85 of 90): the OLDEST live GTC order of this book that the web lets the user renew (a plain limit:
   * kind `limit`; a stop / take-profit / OCO: kind `cond`; never an exit or a pair order), renewed through `renew` (common.ts): same terms, a fresh
   * 90-day expiry, one atomic cancel-replace. Counters `renew_ok` / `renew_failed`; record `renew`.
   */
  private async renewOldest(m: TokenMarket, mine: OrderView[], nowDaa: bigint, tag: string): Promise<void> {
    const cands: { v: OrderView; kind: 'limit' | 'cond'; genesis: { daa: number } }[] = [];
    for (const v of mine) {
      if (!v.current || !v.state_known || !v.state || v.parent || isPair(v)) continue;
      try {
        const st = v.state as unknown as OrderState;
        const desc = describeOrder(st, this.env.kob);
        const g = BigInt(v.genesis.daa);
        const genesisDaa = g > 0n ? g : null;
        const typeKey = orderTypeKey({ kind: st.kind, desc, deadline: v.deadline != null ? BigInt(v.deadline) : null, genesisDaa, nowDaa });
        const kind = amendKind(st, typeKey);
        if ((kind === 'limit' || kind === 'cond') && isGtcExpiry(desc.expiryDaa, genesisDaa)) cands.push({ v, kind, genesis: v.genesis });
      } catch {
        /* a state the web cannot describe is not renewable from My orders either */
      }
    }
    const c = oldestFirst(cands);
    if (!c) return;
    const res = await renew(this.env, this.w, c.v, c.kind, tag);
    this.stats.inc(res.ok ? 'renew_ok' : 'renew_failed');
    if (res.ok) this.recordReplace('renew', m, res);
  }

  /** records a submitted renewal / pair amend for the checker (invariant `replace`) */
  private recordReplace(kind: 'renew' | 'pairAmend', m: TokenMarket, r: ReplaceOutcome): void {
    if (!r.ok || !r.txid || !r.old || !r.id || !r.expect) return;
    recordScenario(this.env.cfg.runPath, {
      kind, ts: Date.now(), bot: this.w.name, token: m.covenantId, old: { id: r.old.id, spends: r.old.spends }, oldExpiryDaa: r.old.expiryDaa,
      id: r.id, inPlace: !!r.inPlace, txid: r.txid, before: r.old.state, expect: r.expect,
    });
  }

  /**
   * "Merge tokens" of this book's token (My orders' merge, kob/merge.ts): the wallet's plain UTXOs merged by a chain of transfers into ONE (3 -> 1, then
   * 1 + 2 -> 1, ...). The fan-out restores the trader's spread every 10 minutes. Counters `mergeTokens_ok`, `mergeTokens_links`, `mergeTokens_inputs`;
   * record `merge`.
   */
  private async merge(m: TokenMarket, tag: string): Promise<void> {
    const r = await mergeTokens(this.env, this.w, m, tag);
    if (!r.links.length || !r.final) {
      if (r.issue) this.stats.inc(`mergeTokens_skipped:${r.issue}`);
      return;
    }
    this.stats.inc('mergeTokens_ok');
    this.stats.inc('mergeTokens_links', r.links.length);
    this.stats.inc('mergeTokens_inputs', r.inputs.length);
    const inputs = r.inputs.map((u) => ({ txid: u.transactionId, index: u.index, amount: String(u.state.amount) }));
    this.log.info('tokens merged', { tag, token: m.ticker, before: r.before, inputs: inputs.length, links: r.links.length, final: r.final });
    if (inputs.length >= 2) {
      recordScenario(this.env.cfg.runPath, { kind: 'merge', ts: Date.now(), bot: this.w.name, token: m.covenantId, ticker: m.ticker, maker: this.w.pk, inputs, total: mergeTotal(inputs).toString(), links: r.links, final: r.final });
    }
  }

  /**
   * After a SELF_TRADE refusal: cancels the own orders it names (at most 6, those still live and not already being cancelled) and plans the
   * intent again, at most twice. The next plan no longer sees them (the wallet leaves its pending cancels out of the own orders), so it
   * does not wait for the indexer to list the cancels.
   */
  private async clearSelfTrade<R extends PlaceResult>(res: R, again: () => Promise<R>, cancelViews: (views: OrderView[]) => Promise<number>): Promise<R> {
    for (let round = 0; round < 2 && res.planIssue === 'SELF_TRADE'; round++) {
      const ids = (res.selfTrade ?? []).slice(0, 6);
      const views = this.w.actionable(
        (await Promise.all(ids.map((id) => this.env.indexer.order(id).catch(() => null)))).filter((v): v is OrderView => !!v && (v.status === 'open' || v.status === 'partial')),
      );
      if (!views.length) break;
      const n = await cancelViews(views);
      this.stats.inc('selftrade_cancelled_orders', n);
      if (n < 1) break;
      // the cancel reaches the node before the order that would have met the cancelled one
      await sleep(3000);
      res = await again();
    }
    return res;
  }

  /**
   * A pair order of the asset token A against TUSD B (the pair page `#/pair/<asset>/<TUSD>`) through the web planner: `planOrder` with a
   * PairPlanEnv (soak market.ts `pairPlanEnv`), prices B base units per WHOLE A, amounts base units of A, tips KAS. The reference rate R is the
   * pair book's touch midpoint when it lies within 3 % of the Binance-derived fair rate (refA / refB), else the fair rate. Sizes are
   * `pairUsd` dollars (3 to 20) of A. Plan refusals are counted per code (`plan_refused:pair:<action>@<A>/<B>:<code>`).
   */
  private async pair(quoteM: TokenMarket, asset: AssetBook): Promise<void> {
    const env = this.env;
    const baseM = asset.m;
    const rA = asset.feed.get();
    const rB = this.feed.get();
    if (!rA || !rB) return;
    const weights = { ...DEFAULT_PAIR_WEIGHTS, ...(asset.cfg.pairWeights ?? {}) };
    const action = weighted(weights);
    const tag = `pair:${action}@${baseM.ticker}/${quoteM.ticker}`;
    const fair = pairRateOf(rA.perToken, rB.perToken, quoteM.scale);
    if (fair === null || fair <= 0n) return;
    let view = null;
    try {
      view = await env.indexer.pairBook(baseM.covenantId, quoteM.covenantId, { depth: 30 });
    } catch {
      view = null;
    }
    const R = pairReference(view, baseM.scale, fair, 300) ?? fair;
    const at = (b: number) => {
      const v = R + bps(R, b);
      return v < 1n ? 1n : v;
    };
    const [uLo, uHi] = asset.cfg.pairUsd ?? asset.cfg.crossUsd ?? [3, 20];
    const usd = (lo = uLo, hi = uHi) => usdToUnits(lo + Math.random() * Math.max(0, hi - lo), rA.usd, baseM.decimals);
    const s = side();
    const B = this.bands;
    const cross = () => frac(B.crossBps, B.crossBps * 3);
    const clock = await readClock(env);
    const gtd = (min: number, max: number) => ({ kind: 'gtd' as const, at: clock.unixSeconds + BigInt(randInt(min, max) * 60) });
    let intent: Intent | null = null;
    switch (action) {
      case 'limitSell':
        // a resting sell a little above the reference (35 %), or through it by 0.2 .. 1 % (a crossing limit: netting / route)
        intent = { type: 'limit', side: 'sell', amount: usd(), price: at(Math.random() < 0.35 ? frac(20, 100) : -cross()), lifetime: gtd(5, 20) };
        break;
      case 'limitBuy':
        intent = { type: 'limit', side: 'buy', amount: usd(), price: at(Math.random() < 0.35 ? -frac(20, 100) : cross()), lifetime: gtd(5, 20) };
        break;
      case 'ioc':
        intent = { type: 'ioc', side: s, amount: usd(3, 10), price: at(s === 'buy' ? cross() : -cross()) };
        break;
      case 'market':
        intent = { type: 'market', side: s, amount: usd(3, 10), slippageBps: BigInt(B.marketBps) };
        break;
      case 'dutch':
        intent = s === 'sell'
          ? { type: 'dutch', side: 'sell', amount: usd(), price: at(frac(...B.dutchStartBps)), priceEnd: at(-frac(...B.dutchEndBps)), duration: { seconds: BigInt(randInt(300, 900)) } }
          : { type: 'dutch', side: 'buy', amount: usd(), price: at(-frac(...B.dutchStartBps)), priceEnd: at(frac(...B.dutchEndBps)), duration: { seconds: BigInt(randInt(300, 900)) } };
        break;
      case 'twap': {
        const amount = usd(6, 12);
        // TWAP sells in slices, DCA buys in slices
        const slice = amount / 3n > 0n ? amount / 3n : amount;
        const interval = { seconds: BigInt(randInt(60, 150)) };
        intent = s === 'sell'
          ? { type: 'twap', side: 'sell', amount, sliceAmount: slice, interval, price: at(-frac(0, 30)), lifetime: gtd(60, 120) }
          : { type: 'dca', side: 'buy', amount, sliceAmount: slice, interval, price: at(frac(0, 30)), maxFills: 6n, lifetime: gtd(60, 120) };
        break;
      }
      case 'stop':
        // a stop near the market: armed by the rate the KAS books imply (two KAS-book fills) or by a resting pair order filled at or beyond it
        intent = { type: 'stopMarket', side: s, amount: usd(), stop: at(s === 'sell' ? -frac(8, 40) : frac(8, 40)), slipBps: B.stopBps, expiry: { kind: 'gtdUnix', atUnixSeconds: clock.unixSeconds + 6n * 3600n } };
        break;
      case 'trailing': {
        const gap = bps(R, 30) > 0n ? bps(R, 30) : 1n;
        const step = bps(R, 10) > 0n ? bps(R, 10) : 1n;
        intent = {
          type: 'trailingStop', side: s, amount: usd(), stop: s === 'sell' ? R - gap : R + gap, trail: { step, gap, wait: 600n, expectedUpdates: 10 }, slipBps: B.stopBps,
          expiry: { kind: 'gtdUnix', atUnixSeconds: clock.unixSeconds + 6n * 3600n },
        };
        break;
      }
      case 'oco':
        intent = { type: 'oco', side: s, amount: usd(), takeProfit: at(s === 'sell' ? frac(60, 150) : -frac(60, 150)), stop: at(s === 'sell' ? -frac(10, 50) : frac(10, 50)), slipBps: B.stopBps };
        break;
      case 'ifdBuy':
        intent = { type: 'ifd', side: 'buy', amount: usd(), entry: { price: at(-frac(5, 40)) }, exit: { takeProfit: at(frac(30, 80)) } };
        break;
      case 'ifdSell':
        intent = { type: 'ifd', side: 'sell', amount: usd(), entry: { price: at(frac(5, 40)) }, exit: { takeProfit: at(-frac(30, 80)) } };
        break;
      case 'repeatIfd':
        intent = s === 'buy'
          ? { type: 'repeatIfd', side: 'buy', amount: usd(3, 8), entry: { price: at(-frac(5, 25)) }, exit: { takeProfit: at(frac(20, 45)) }, repeat: { count: BigInt(randInt(2, 3)) } }
          : { type: 'repeatIfd', side: 'sell', amount: usd(3, 8), entry: { price: at(frac(5, 25)) }, exit: { takeProfit: at(-frac(20, 45)) }, repeat: { count: BigInt(randInt(2, 3)) } };
        break;
      case 'fok':
        // all or nothing through the touch (netting / route / inventory in one transaction)
        intent = { type: 'fok', side: s, amount: usd(2, 6), price: at(s === 'buy' ? cross() : -cross()) };
        break;
      case 'streaming':
        // quote-and-execute from the reference the user saw, 0.5 % tolerance
        intent = { type: 'streaming', side: s, amount: usd(2, 6), displayedPrice: R, toleranceBps: BigInt(B.streamingBps) };
        break;
      case 'close':
        // a market sell of some of the A the wallet holds (refused by the planner when it holds none)
        intent = { type: 'close', amount: usd(2, 5), slippageBps: BigInt(B.marketBps) };
        break;
      case 'stopLimit': {
        const stop = at(s === 'sell' ? -frac(8, 40) : frac(8, 40));
        const limit = s === 'sell' ? stop - bps(stop, B.stopLimitBps) : stop + bps(stop, B.stopLimitBps);
        intent = { type: 'stopLimit', side: s, amount: usd(), stop, limit: limit < 1n ? 1n : limit, expiry: { kind: 'gtdUnix', atUnixSeconds: clock.unixSeconds + 6n * 3600n } };
        break;
      }
      case 'takeProfit':
        intent = { type: 'takeProfit', side: s, amount: usd(), price: at(s === 'sell' ? frac(40, 150) : -frac(40, 150)) };
        break;
      case 'ifo':
        intent = s === 'buy'
          ? { type: 'ifo', side: 'buy', amount: usd(), entry: { price: at(-frac(5, 40)) }, exit: { takeProfit: at(frac(40, 90)), stop: at(-frac(15, 45)), slipBps: B.stopBps } }
          : { type: 'ifo', side: 'sell', amount: usd(), entry: { price: at(frac(5, 40)) }, exit: { takeProfit: at(-frac(40, 90)), stop: at(frac(15, 45)), slipBps: B.stopBps } };
        break;
      case 'ifoStopEntry':
        // breakout entry armed by pair trigger evidence: buy stop above the market, sell stop below
        {
          const off = frac(10, 35);
          intent = s === 'buy'
            ? { type: 'ifo', side: 'buy', amount: usd(), entry: { stop: at(off), price: at(off + B.entryBandBps) }, exit: { takeProfit: at(frac(160, 220)), stop: at(-frac(20, 60)), slipBps: B.stopBps } }
            : { type: 'ifo', side: 'sell', amount: usd(), entry: { stop: at(-off), price: at(-off - B.entryBandBps) }, exit: { takeProfit: at(-frac(160, 220)), stop: at(frac(20, 60)), slipBps: B.stopBps } };
        }
        break;
      case 'repeatIfo':
        intent = s === 'buy'
          ? { type: 'repeatIfo', side: 'buy', amount: usd(3, 8), entry: { price: at(-frac(5, 25)) }, exit: { takeProfit: at(frac(20, 45)), stop: at(-frac(120, 200)), slipBps: B.stopBps }, repeat: { count: 2n } }
          : { type: 'repeatIfo', side: 'sell', amount: usd(3, 8), entry: { price: at(frac(5, 25)) }, exit: { takeProfit: at(-frac(20, 45)), stop: at(frac(120, 200)), slipBps: B.stopBps }, repeat: { count: 2n } };
        break;
      case 'cancel': {
        const mine = this.w.actionable(await myOrders(env, this.w.pk, baseM).catch(() => [] as OrderView[])).filter((v) => isPair(v) && v.current && v.state_known && !v.parent);
        if (!mine.length) return;
        const v = pick(mine);
        const res = await cancel(env, this.w, v, `cancel:${v.contract}`);
        this.stats.inc(res.ok ? 'pair_cancel_ok' : 'pair_cancel_failed');
        return;
      }
      case 'gtd':
      case 'day':
      case 'timed':
        // a dated pair limit resting 0.2 .. 1 % behind the reference: GTD 30 to 120 min (keeper refund), a day order, or a timed activation
        intent = datedPairLimit(action as 'gtd' | 'day' | 'timed', s, usd(), at(s === 'sell' ? frac(20, 100) : -frac(20, 100)), clock.unixSeconds) as Intent;
        break;
      case 'amend': {
        // the pair ticket's replace of a resting pair limit (no in-place amend for pair orders): a new price 5 to 25 bps further from the market
        const limits = this.w
          .actionable(await myOrders(env, this.w.pk, baseM).catch(() => [] as OrderView[]))
          .filter((v) => v.contract === 'KobPair' && v.pair?.base === baseM.covenantId && v.pair.quote === quoteM.covenantId && v.tif === 0 && !v.auction && !!v.current && v.state_known);
        if (!limits.length) return;
        const res = await pairAmend(env, this.w, pick(limits), baseM, quoteM, (side, price) => pairAmendPrice(side, price), tag);
        this.stats.inc(res.ok ? 'pair_amend_ok' : 'pair_amend_failed');
        if (res.ok) this.recordReplace('pairAmend', baseM, res);
        return;
      }
      case 'cancelAll': {
        // My orders' cancel-all of the pair: every live pair order of this pair (entries and their exits together), several per transaction
        const live = this.w
          .actionable(await myOrders(env, this.w.pk, baseM).catch(() => [] as OrderView[]))
          .filter((v) => isPair(v) && v.pair?.base === baseM.covenantId && v.pair.quote === quoteM.covenantId && !!v.current && v.state_known);
        if (!live.length) return;
        const out = await cancelAllDetailed(env, this.w, live, `pairCancelAll@${baseM.ticker}/${quoteM.ticker}`);
        this.stats.inc('pair_cancelAll_orders', out.orders.length);
        if (out.orders.length) recordScenario(env.cfg.runPath, { kind: 'cancelAll', ts: Date.now(), bot: this.w.name, scope: `pair:${baseM.ticker}/${quoteM.ticker}`, orders: out.orders, txids: out.txids });
        return;
      }
      default:
        return;
    }
    if (!intent) return;
    const placed = intent;
    // a pair order that would trade against one of this trader's own resting pair orders (netting) is refused (SELF_TRADE, the planner
    // names every such order); a real user cancels those and trades (one cancel each: a pair order's custody is in either token)
    const cancelEach = async (views: OrderView[]) => {
      let n = 0;
      for (const v of views) if ((await cancel(env, this.w, v, 'selfTradeCancel')).ok) n++;
      return n;
    };
    // a dated pair limit rests (never turned into a taker IOC); the rest take when marketable
    const taker = action !== 'gtd' && action !== 'day' && action !== 'timed';
    const res = await this.clearSelfTrade(await placePair(env, this.w, placed, tag, baseM, quoteM, { taker }), () => placePair(env, this.w, placed, tag, baseM, quoteM, { taker }), cancelEach);
    if (res.ok) this.log.info('pair order placed', { tag, kind: res.pair?.kind, amount: (intent as { amount?: bigint }).amount, reference: R, fair, txid: res.txid });
  }
}
