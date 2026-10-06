// Trader bots. Each trader wakes up at exponential intervals and performs one action drawn from a weighted mix that covers EVERY order
// type of docs/spec/order-types.md (market with auction, limit GTC / GTD / day, timed activation, IOC, FOK, streaming, stop-market,
// stop-limit, trailing stop, take-profit, OCO, IFD both ways, IFO, IFO with a stop entry, repeat IFD / IFO, TWAP, DCA, Dutch and rising
// bid, close) plus order management (cancel, cancel-replace, cancel-all). GTD / IOC orders exercise the keepers' refunds and kills.
// Prices are drawn around the Binance-derived reference and the live book, sizes are small and random. A share of the actions (each asset's
// `pairShare`) are PAIR orders of the asset token (A) against TUSD (B) through the web planner (`planOrder` with a PairPlanEnv): limits on
// both sides, IOC and market, Dutch, TWAP, stop-market and trailing stops, OCO, IFD buy-first and sell-first, repeat IFD; they rest, cross
// each other (the matcher nets them), route through the two KAS books and arm the pair stops (both evidence modes).
import type { Intent } from '@/kob/plan';
import type { BookView, TokenMarket } from '@/kob/plan-types';
import { pairRateOf, pairReference } from './pair-math';
import type { OrderView } from '@/data/indexer-types';
import type { TraderConfig } from '../config';
import type { Env } from '../env';
import { errText, type Logger, type Stats } from '../log';
import { isPair, myOrders, readBook, readClock, tokenMarket, touch, type AssetBook } from '../market';
import type { RefSource } from '../price';
import { bps, expDelay, onTick, pick, randInt, randUnits, sleep, usdToUnits, weighted } from '../util';
import { walletFor, type BotWallet } from '../wallet';
import { amend, cancel, cancelAll, priceOf, place, placePair } from './common';
import { consolidateTokens } from './consolidate';
import { consolidateConfig } from './consolidate-math';
import { fanoutTarget } from './fanout';

export const DEFAULT_WEIGHTS: Record<string, number> = {
  market: 16,
  limit: 10,
  limitCross: 8,
  gtd: 5,
  day: 3,
  timed: 3,
  ioc: 8,
  fok: 4,
  streaming: 5,
  stopMarket: 6,
  stopLimit: 4,
  trailingStop: 4,
  takeProfit: 3,
  oco: 5,
  ifd: 4,
  ifo: 3,
  ifoStopEntry: 3,
  repeatIfd: 2,
  repeatIfo: 1,
  twap: 2,
  dca: 2,
  dutch: 2,
  close: 1,
  cancel: 7,
  amend: 5,
  cancelAll: 1,
};

/**
 * The pair actions (an asset token A against TUSD B): every family of pair kinds. `limitSell` / `limitBuy` rest near the reference (and cross
 * each other: netting), `ioc` / `market` take (the route through the two KAS books, or resting pair orders), `stop` / `trailing` / `oco` are
 * KobCondPair orders the matcher arms from pair evidence (two KAS-book fills, or a resting pair order filled), `ifdBuy` / `ifdSell` / `repeatIfd`
 * KobIfdPair entries (buy-first and sell-first) whose fills create exits.
 */
export const DEFAULT_PAIR_WEIGHTS: Record<string, number> = {
  limitSell: 6,
  limitBuy: 6,
  ioc: 3,
  market: 3,
  dutch: 2,
  twap: 1,
  stop: 2,
  trailing: 2,
  oco: 2,
  ifdBuy: 2,
  ifdSell: 2,
  repeatIfd: 1,
  // the rest of the KAS ticket's order types, on the pair
  fok: 1,
  streaming: 1,
  close: 1,
  stopLimit: 1,
  takeProfit: 1,
  ifo: 1,
  ifoStopEntry: 1,
  repeatIfo: 1,
  cancel: 3,
};

const side = (): 'buy' | 'sell' => (Math.random() < 0.5 ? 'buy' : 'sell');
const frac = (lo: number, hi: number) => lo + Math.random() * (hi - lo);

export class Trader {
  readonly w: BotWallet;
  private stopped = false;
  private readonly weights: Record<string, number>;

  constructor(
    private readonly env: Env,
    private readonly cfg: TraderConfig,
    private readonly feed: RefSource,
    private readonly log: Logger,
    readonly stats: Stats,
    /** the soak books beside TUSD (TETH, TBTC): their markets, configs and references */
    private readonly assets: AssetBook[] = [],
  ) {
    this.w = walletFor(env, cfg.key, stats, log);
    this.weights = { ...DEFAULT_WEIGHTS, ...(cfg.weights ?? {}) };
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

  /** merges this trader's surplus token UTXOs (own loop: never concurrent with its own action); rate-limited per key and token */
  private async consolidate(): Promise<void> {
    const cc = consolidateConfig(this.env.cfg.consolidate);
    if (!cc.enabled || !cc.traders) return;
    const ms = [tokenMarket(this.env), ...this.assets.map((a) => a.m)];
    for (const m of ms) await consolidateTokens(this.env, this.w, m, fanoutTarget(this.env, 'trader', m), cc);
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
    let asset: AssetBook | null = null;
    for (const a of this.assets) {
      if (r < a.cfg.bookShare) {
        asset = a;
        break;
      }
      r -= a.cfg.bookShare;
    }
    const m = asset ? asset.m : tokenMarket(env);
    const ref = (asset ? asset.feed : this.feed).get();
    if (!ref) return;
    // stat tags: the primary book keeps the plain action names, an asset book's carry '@<ticker>'
    const action = asset ? `${action0}@${m.ticker}` : action0;
    this.book = { decimals: m.decimals, usd: ref.usd };
    const tick = m.tick;
    // sompi per whole token: every intent price below is in this unit
    const R = ref.perToken;
    const at = (b: number) => onTick(R + bps(R, b), tick);
    const book: BookView = await readBook(env, m);
    const mine: OrderView[] = await myOrders(env, this.w.pk, m).catch(() => []);
    const t = touch(book);
    const clock = await readClock(env);
    const o = { book, mine };
    const s = side();
    const amount = this.size();
    let intent: Intent | null = null;

    switch (action0) {
      case 'market':
        intent = { type: 'market', side: s, amount: this.size(3) };
        break;
      case 'limit':
        // passive: a little behind the reference
        intent = { type: 'limit', side: s, amount, price: at(s === 'buy' ? -frac(5, 60) : frac(5, 60)), ...(s === 'buy' ? { maxFills: 3n } : {}) };
        break;
      case 'limitCross': {
        // aggressive limit through the touch: the default crossing policy turns it into an auction from the touch to the limit
        const lim = s === 'buy' ? (t.ask ?? R) + bps(t.ask ?? R, 30) : (t.bid ?? R) - bps(t.bid ?? R, 30);
        intent = { type: 'limit', side: s, amount: this.size(3), price: onTick(lim, tick) };
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
        intent = { type: 'ioc', side: s, amount: this.size(3), price: onTick(s === 'buy' ? p + bps(p, 10) : p - bps(p, 10), tick) };
        break;
      }
      case 'fok': {
        const p = s === 'buy' ? t.ask : t.bid;
        if (p == null) return;
        intent = { type: 'fok', side: s, amount: this.size(2), price: onTick(s === 'buy' ? p + bps(p, 20) : p - bps(p, 20), tick) };
        break;
      }
      case 'streaming': {
        const p = s === 'buy' ? t.ask : t.bid;
        if (p == null) return;
        intent = { type: 'streaming', side: s, amount: this.size(2), displayedPrice: p, toleranceBps: 50n };
        break;
      }
      case 'stopMarket':
        intent = { type: 'stopMarket', side: s, amount: this.size(3), stop: at(s === 'sell' ? -frac(8, 40) : frac(8, 40)), expiry: { kind: 'gtdUnix', atUnixSeconds: clock.unixSeconds + 6n * 3600n } };
        break;
      case 'stopLimit': {
        const stop = at(s === 'sell' ? -frac(8, 40) : frac(8, 40));
        const limit = onTick(s === 'sell' ? stop - bps(stop, 100) : stop + bps(stop, 100), tick);
        intent = { type: 'stopLimit', side: s, amount: this.size(3), stop, limit, expiry: { kind: 'gtdUnix', atUnixSeconds: clock.unixSeconds + 6n * 3600n } };
        break;
      }
      case 'trailingStop': {
        const gap = onTick(bps(R, 30), tick);
        intent = {
          type: 'trailingStop', side: s, amount: this.size(2), stop: s === 'sell' ? onTick(R - gap, tick) : onTick(R + gap, tick),
          trail: { step: onTick(bps(R, 10), tick), gap, wait: 600n, expectedUpdates: 10 },
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
        };
        break;
      case 'ifd':
        intent = s === 'buy'
          ? { type: 'ifd', side: 'buy', amount: this.size(3), entry: { price: at(-frac(5, 40)) }, exit: { takeProfit: at(frac(30, 80)) } }
          : { type: 'ifd', side: 'sell', amount: this.size(3), entry: { price: at(frac(5, 40)) }, exit: { takeProfit: at(-frac(30, 80)) } };
        break;
      case 'ifo':
        intent = s === 'buy'
          ? { type: 'ifo', side: 'buy', amount: this.size(3), entry: { price: at(-frac(5, 40)) }, exit: { takeProfit: at(frac(40, 90)), stop: at(-frac(15, 45)) } }
          : { type: 'ifo', side: 'sell', amount: this.size(3), entry: { price: at(frac(5, 40)) }, exit: { takeProfit: at(-frac(40, 90)), stop: at(frac(15, 45)) } };
        break;
      case 'ifoStopEntry':
        // breakout entry: buy stop above the market (stop <= price), sell stop below (stop >= price)
        intent = s === 'buy'
          ? { type: 'ifo', side: 'buy', amount: this.size(2), entry: { stop: at(frac(10, 35)), price: at(frac(70, 110)) }, exit: { takeProfit: at(frac(160, 220)), stop: at(-frac(20, 60)) } }
          : { type: 'ifo', side: 'sell', amount: this.size(2), entry: { stop: at(-frac(10, 35)), price: at(-frac(70, 110)) }, exit: { takeProfit: at(-frac(160, 220)), stop: at(frac(20, 60)) } };
        break;
      case 'repeatIfd':
        intent = s === 'buy'
          ? { type: 'repeatIfd', side: 'buy', amount: this.size(2), entry: { price: at(-frac(5, 25)) }, exit: { takeProfit: at(frac(20, 45)) }, repeat: { count: BigInt(randInt(2, 4)) } }
          : { type: 'repeatIfd', side: 'sell', amount: this.size(2), entry: { price: at(frac(5, 25)) }, exit: { takeProfit: at(-frac(20, 45)) }, repeat: { count: BigInt(randInt(2, 4)) } };
        break;
      case 'repeatIfo':
        intent = s === 'buy'
          ? { type: 'repeatIfo', side: 'buy', amount: this.size(2), entry: { price: at(-frac(5, 25)) }, exit: { takeProfit: at(frac(20, 45)), stop: at(-frac(120, 200)) }, repeat: { count: 2n } }
          : { type: 'repeatIfo', side: 'sell', amount: this.size(2), entry: { price: at(frac(5, 25)) }, exit: { takeProfit: at(-frac(20, 45)), stop: at(frac(120, 200)) }, repeat: { count: 2n } };
        break;
      case 'twap':
        intent = { type: 'twap', side: 'sell', amount: randUnits(this.usd(3), this.usd(6)), sliceAmount: this.usd(1), interval: { seconds: BigInt(randInt(60, 150)) }, price: at(-frac(0, 30)), lifetime: { kind: 'gtd', at: clock.unixSeconds + 2n * 3600n } };
        break;
      case 'dca':
        intent = { type: 'dca', side: 'buy', amount: randUnits(this.usd(3), this.usd(6)), sliceAmount: this.usd(1), interval: { seconds: BigInt(randInt(60, 150)) }, price: at(frac(0, 30)), maxFills: 6n, lifetime: { kind: 'gtd', at: clock.unixSeconds + 2n * 3600n } };
        break;
      case 'dutch':
        intent = s === 'sell'
          ? { type: 'dutch', side: 'sell', amount: this.size(3), price: at(frac(150, 250)), priceEnd: at(-frac(20, 60)), duration: { seconds: BigInt(randInt(300, 900)) } }
          : { type: 'dutch', side: 'buy', amount: this.size(3), price: at(-frac(150, 250)), priceEnd: at(frac(20, 60)), duration: { seconds: BigInt(randInt(300, 900)) } };
        break;
      case 'close':
        intent = { type: 'close', amount: this.size(2) };
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
      default:
        return;
    }
    if (!intent) return;
    const res = await place(env, this.w, intent, action, o, m);
    if (res.planIssue === 'SELF_TRADE') {
      // a real user would pull their own resting orders on the other side first (matcher.md 10.12: self-trade prevention is the
      // wallet's job): cancel them now, the next action trades
      const sideOf = (intent as { side?: 'buy' | 'sell' }).side ?? 'sell';
      const opposite = mine.filter((v) => v.current && v.state_known && !v.parent && !isPair(v) && (sideOf === 'buy' ? v.side === 1 : v.side === 2));
      if (opposite.length) {
        const n = await cancelAll(env, this.w, opposite.slice(0, 6), 'selfTradeCancel');
        this.stats.inc('selftrade_cancelled_orders', n);
        // then trade: once the cancel is accepted and indexed, plan the same intent again against the fresh book
        if (n > 0) {
          await sleep(6000);
          await place(env, this.w, intent, action, {}, m);
        }
      }
    }
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
    const clock = await readClock(env);
    const gtd = (min: number, max: number) => ({ kind: 'gtd' as const, at: clock.unixSeconds + BigInt(randInt(min, max) * 60) });
    let intent: Intent | null = null;
    switch (action) {
      case 'limitSell':
        // a resting sell a little above the reference (35 %), or through it by 0.2 .. 1 % (a crossing limit: netting / route)
        intent = { type: 'limit', side: 'sell', amount: usd(), price: at(Math.random() < 0.35 ? frac(20, 100) : -frac(20, 100)), lifetime: gtd(5, 20) };
        break;
      case 'limitBuy':
        intent = { type: 'limit', side: 'buy', amount: usd(), price: at(Math.random() < 0.35 ? -frac(20, 100) : frac(20, 100)), lifetime: gtd(5, 20) };
        break;
      case 'ioc':
        intent = { type: 'ioc', side: s, amount: usd(3, 10), price: at(s === 'buy' ? frac(30, 100) : -frac(30, 100)) };
        break;
      case 'market':
        intent = { type: 'market', side: s, amount: usd(3, 10) };
        break;
      case 'dutch':
        intent = s === 'sell'
          ? { type: 'dutch', side: 'sell', amount: usd(), price: at(frac(150, 250)), priceEnd: at(-frac(20, 60)), duration: { seconds: BigInt(randInt(300, 900)) } }
          : { type: 'dutch', side: 'buy', amount: usd(), price: at(-frac(150, 250)), priceEnd: at(frac(20, 60)), duration: { seconds: BigInt(randInt(300, 900)) } };
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
        intent = { type: 'stopMarket', side: s, amount: usd(), stop: at(s === 'sell' ? -frac(8, 40) : frac(8, 40)), expiry: { kind: 'gtdUnix', atUnixSeconds: clock.unixSeconds + 6n * 3600n } };
        break;
      case 'trailing': {
        const gap = bps(R, 30) > 0n ? bps(R, 30) : 1n;
        const step = bps(R, 10) > 0n ? bps(R, 10) : 1n;
        intent = {
          type: 'trailingStop', side: s, amount: usd(), stop: s === 'sell' ? R - gap : R + gap, trail: { step, gap, wait: 600n, expectedUpdates: 10 },
          expiry: { kind: 'gtdUnix', atUnixSeconds: clock.unixSeconds + 6n * 3600n },
        };
        break;
      }
      case 'oco':
        intent = { type: 'oco', side: s, amount: usd(), takeProfit: at(s === 'sell' ? frac(60, 150) : -frac(60, 150)), stop: at(s === 'sell' ? -frac(10, 50) : frac(10, 50)) };
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
        intent = { type: 'fok', side: s, amount: usd(2, 6), price: at(s === 'buy' ? frac(30, 80) : -frac(30, 80)) };
        break;
      case 'streaming':
        // quote-and-execute from the reference the user saw, 0.5 % tolerance
        intent = { type: 'streaming', side: s, amount: usd(2, 6), displayedPrice: R, toleranceBps: 50n };
        break;
      case 'close':
        // a market sell of some of the A the wallet holds (refused by the planner when it holds none)
        intent = { type: 'close', amount: usd(2, 5) };
        break;
      case 'stopLimit': {
        const stop = at(s === 'sell' ? -frac(8, 40) : frac(8, 40));
        const limit = s === 'sell' ? stop - bps(stop, 100) : stop + bps(stop, 100);
        intent = { type: 'stopLimit', side: s, amount: usd(), stop, limit: limit < 1n ? 1n : limit, expiry: { kind: 'gtdUnix', atUnixSeconds: clock.unixSeconds + 6n * 3600n } };
        break;
      }
      case 'takeProfit':
        intent = { type: 'takeProfit', side: s, amount: usd(), price: at(s === 'sell' ? frac(40, 150) : -frac(40, 150)) };
        break;
      case 'ifo':
        intent = s === 'buy'
          ? { type: 'ifo', side: 'buy', amount: usd(), entry: { price: at(-frac(5, 40)) }, exit: { takeProfit: at(frac(40, 90)), stop: at(-frac(15, 45)) } }
          : { type: 'ifo', side: 'sell', amount: usd(), entry: { price: at(frac(5, 40)) }, exit: { takeProfit: at(-frac(40, 90)), stop: at(frac(15, 45)) } };
        break;
      case 'ifoStopEntry':
        // breakout entry armed by pair trigger evidence: buy stop above the market, sell stop below
        intent = s === 'buy'
          ? { type: 'ifo', side: 'buy', amount: usd(), entry: { stop: at(frac(10, 35)), price: at(frac(70, 110)) }, exit: { takeProfit: at(frac(160, 220)), stop: at(-frac(20, 60)) } }
          : { type: 'ifo', side: 'sell', amount: usd(), entry: { stop: at(-frac(10, 35)), price: at(-frac(70, 110)) }, exit: { takeProfit: at(-frac(160, 220)), stop: at(frac(20, 60)) } };
        break;
      case 'repeatIfo':
        intent = s === 'buy'
          ? { type: 'repeatIfo', side: 'buy', amount: usd(3, 8), entry: { price: at(-frac(5, 25)) }, exit: { takeProfit: at(frac(20, 45)), stop: at(-frac(120, 200)) }, repeat: { count: 2n } }
          : { type: 'repeatIfo', side: 'sell', amount: usd(3, 8), entry: { price: at(frac(5, 25)) }, exit: { takeProfit: at(-frac(20, 45)), stop: at(frac(120, 200)) }, repeat: { count: 2n } };
        break;
      case 'cancel': {
        const mine = (await myOrders(env, this.w.pk, baseM).catch(() => [] as OrderView[])).filter((v) => isPair(v) && v.current && v.state_known && !v.parent);
        if (!mine.length) return;
        const v = pick(mine);
        const res = await cancel(env, this.w, v, `cancel:${v.contract}`);
        this.stats.inc(res.ok ? 'pair_cancel_ok' : 'pair_cancel_failed');
        return;
      }
      default:
        return;
    }
    if (!intent) return;
    let res = await placePair(env, this.w, intent, tag, baseM, quoteM);
    // a pair order that would trade against one of this trader's own resting orders (on either KAS book of the route, or an opposite
    // pair order: netting) is refused (SELF_TRADE, the planner names the order); a real user cancels that order and trades: up to 6
    // such orders are cancelled, each followed by a fresh plan once the cancel is indexed
    for (let i = 0; i < 6 && res.planIssue === 'SELF_TRADE'; i++) {
      const own = String(res.planParams?.ownCovenantId ?? '');
      const v = own ? await env.indexer.order(own).catch(() => null) : null;
      if (!v) break;
      const c = await cancel(env, this.w, v, 'selfTradeCancel');
      if (!c.ok) break;
      this.stats.inc('selftrade_cancelled_orders');
      await sleep(6000);
      res = await placePair(env, this.w, intent, tag, baseM, quoteM);
    }
    if (res.ok) this.log.info('pair order placed', { tag, kind: res.pair?.kind, amount: (intent as { amount?: bigint }).amount, reference: R, fair, txid: res.txid });
  }
}
