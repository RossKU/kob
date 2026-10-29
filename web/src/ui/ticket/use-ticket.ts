// Data hooks of the order ticket: the planning environment kept fresh in the background, and the debounced plan.
//
// Responsiveness: typing only re-plans against the environment already on hand (synchronous), it never waits for the network. The environment is
// read in parts that are kept fresh separately (app/env.ts): the MARKET (book per order, newest trade) as soon as the ticket is shown, with or
// without a wallet, and on every book notice; the COINS (node clock, spendable KAS, fees: the node only, fast) and the HOLDINGS (token UTXOs, own
// resting orders: the indexer) once a wallet is connected, every ENV_REFRESH_MS, on a fill of one of the wallet's orders and after every
// submission. The ticket plans as soon as the market and the coins are there; holdings still being read count as none (a sell waits for them, a buy
// does not), and Review re-checks against them before anything can be signed (OrderTicket, the freshness rule below).
import { useCallback, useEffect, useMemo, useRef, useState } from 'preact/hooks';
import { useServices, useWallet } from '../../app/context';
import type { Services } from '../../app/services';
import { assemblePlanEnv, buildPairPlanEnv, dropPendingSpends, readCoinsPart, readHoldingsPart, readMarketPart, tokenMarketOf, type CoinsPart, type HoldingsPart, type MarketPart } from '../../app/env';
import type { Intent } from '../../kob/plan';
import { planOrder } from '../../kob/plan';
import type { OrderPlan, PlanEnv } from '../../kob/plan-types';
import type { TokenInfo } from '../../kob/registry';
import { pendingSpends } from '../../app/pending-spends';
import { useInterval } from '../kit/hooks';
import { partCurrent, type PartFacts } from './review-check';
import { useLiveRefresh } from '../market/live';
import { envErrorSource, type EnvErrorSource } from './env-error';

export const PLAN_DEBOUNCE_MS = 300;
/** every part of the environment is re-read at least this often (UTXOs and the book move; feed events also trigger reads); paused on a hidden page */
export const ENV_REFRESH_MS = 20_000;

/**
 * Serialises a load: one run at a time; every call made during a run shares ONE follow-up run that starts after it ends, so its result
 * reflects state read after the call. A feed that fires faster than a load takes never starves the result (each run completes and is shown).
 * Runs are timed by their START (when their reads began): `since(t)` waits only as long as needed for a result read at or after `t`.
 */
export class Serialized<T> {
  private running: { p: Promise<T>; at: number } | null = null;
  private next: Promise<T> | null = null;
  private last: { value: T; at: number; doneAt: number } | null = null;
  private readonly now: () => number;

  constructor(
    private readonly load: () => Promise<T>,
    o: { now?: () => number } = {},
  ) {
    this.now = o.now ?? Date.now;
  }

  run(): Promise<T> {
    if (!this.running) return this.start();
    if (!this.next) {
      const after = this.running.p.then(
        () => undefined,
        () => undefined,
      );
      this.next = after.then(() => {
        this.next = null;
        return this.start();
      });
    }
    return this.next;
  }

  /**
   * A result whose run started at or after `t`: the newest one when it qualifies (at once), else the run in flight when it started at or after `t`,
   * else a new run (or the follow-up already queued). Never a result read before `t`.
   */
  since(t: number): Promise<T> {
    const l = this.last;
    if (l && l.at >= t) return Promise.resolve(l.value);
    if (this.running && this.running.at >= t) return this.running.p;
    return this.run();
  }

  /** The newest successful result, with the start and the end of its run. */
  latest(): { value: T; at: number; doneAt: number } | null {
    return this.last;
  }

  /** A read is running or queued (a change is known to be on its way). */
  busy(): boolean {
    return !!this.running || !!this.next;
  }

  private start(): Promise<T> {
    const at = this.now();
    const p = this.load();
    const run = { p, at };
    this.running = run;
    const clear = (): void => {
      if (this.running === run) this.running = null;
    };
    p.then((value) => {
      if (!this.last || this.last.at <= at) this.last = { value, at, doneAt: this.now() };
      clear();
    }, clear);
    return p;
  }
}

/** A part of the environment the ticket is still waiting for (no answer yet): the status line says which. */
export type EnvWait = 'market' | 'coins' | 'holdings' | 'pair';

/** What the order ticket reads its environment through. */
export interface TicketEnv {
  /** the environment of the newest parts (null until the market and the coins are there); holdings not read yet count as none */
  env: PlanEnv | null;
  /** parts not read yet */
  waiting: EnvWait[];
  /** a part could not be read (node unreachable ...): the message and a retry */
  error: string | null;
  /** where `error` came from: the node, the indexer or neither (a token whose decimals disagree ...) */
  errorSource: EnvErrorSource | null;
  loading: boolean;
  /** every part read again, starting now: the result reflects state read after the call (after a submission, a retry) */
  refresh(): Promise<PlanEnv | null>;
  /**
   * A complete environment (holdings included) whose `parts` (default: all) were read at or after `t` (`Serialized.since`); the other parts as
   * they are on hand.
   */
  since(t: number, parts?: readonly EnvWait[]): Promise<PlanEnv | null>;
  /**
   * The environment of the parts on hand, assembled NOW (so the spends of a submission made since are already left out), with the start of its
   * oldest read (`at`), the end of its node read (`clockAt`: when its clock was read) and whether the holdings are in.
   */
  snapshot(): { env: PlanEnv; at: number; clockAt: number; complete: boolean } | null;
  /** the parts on hand that may no longer be the state at `clickAt` (review-check.ts `partCurrent`): the re-check reads only these */
  stale(clickAt: number): EnvWait[];
}

interface PartState<T> {
  value: T | null;
  error: string | null;
  source: EnvErrorSource | null;
  loading: boolean;
}

/** One part of the environment: a `Serialized` load per `deps` (a result of an earlier generation is dropped), its newest value as state. */
function usePart<T>(load: (() => Promise<T>) | null, deps: unknown[]): [Serialized<T> | null, PartState<T>] {
  const [state, setState] = useState<PartState<T>>({ value: null, error: null, source: null, loading: false });
  const gen = useRef(0);
  const alive = useRef(true);
  useEffect(() => {
    alive.current = true;
    return () => {
      alive.current = false;
    };
  }, []);
  const runner = useMemo(() => {
    const g = ++gen.current;
    if (!load) return null;
    const mine = () => alive.current && g === gen.current;
    return new Serialized<T>(async () => {
      if (mine()) setState((s) => ({ ...s, loading: true }));
      try {
        const value = await load();
        if (mine()) setState({ value, error: null, source: null, loading: false });
        return value;
      } catch (e) {
        if (mine()) setState((s) => ({ value: s.value, error: e instanceof Error ? e.message : String(e), source: envErrorSource(e), loading: false }));
        throw e;
      }
    });
  }, deps);
  useEffect(() => {
    if (!runner) setState({ value: null, error: null, source: null, loading: false });
    else void runner.run().catch(() => undefined);
  }, [runner]);
  return [runner, state];
}

/**
 * The depth the book is verified to for an order that walks it: `amount` rounded up to a power of two (0: the order does not walk the book). The steps keep
 * the market part from being read again at every digit typed.
 */
export function depthBucket(amount: bigint): bigint {
  if (amount <= 0n) return 0n;
  let b = 1n;
  while (b < amount) b <<= 1n;
  return b;
}

const quiet = (r: Serialized<unknown> | null): void => void r?.run().catch(() => undefined);
const factsOf = (r: Serialized<unknown> | null, noticeAt: number): PartFacts => ({ at: r?.latest()?.at ?? null, busy: !!r?.busy(), noticeAt });

/**
 * When the newest notice that concerns a part arrived (0: none), recorded the moment it arrives, before the feed's debounce starts its read: a change
 * known but not read yet makes the part stale (review-check.ts `partCurrent`). A reorg or a resync concerns every part.
 */
function useNoticeClock(feed: Services['feed']): { stamp(): void; at(): number } {
  const at = useRef(0);
  useEffect(() => {
    if (!feed) return;
    const stamp = () => void (at.current = Date.now());
    const offs = [feed.on('reorg', stamp), feed.on('resync', stamp)];
    return () => offs.forEach((off) => off());
  }, [feed]);
  return useMemo(() => ({ stamp: () => void (at.current = Date.now()), at: () => at.current }), []);
}
const maybe = <T,>(r: Serialized<T> | null, f: (r: Serialized<T>) => Promise<T>): Promise<T | null> => (r ? f(r) : Promise.resolve(null));

/**
 * Keeps the environment of (wallet, token)'s KAS market (see the head of this file). `enabled` false (no wallet, wrong network, untradable token)
 * keeps only the market part, read once (so a wallet that connects plans at once), and stops every wallet read.
 */
export function useTicketEnv(token: TokenInfo, enabled: boolean, depth: bigint = 0n): TicketEnv {
  const services = useServices();
  const pubkey = useWallet().info?.pubkey ?? null;
  const tradable = token.tradable && !!token.program;
  const market = useMemo(() => (tradable ? tokenMarketOf(services, token) : null), [services, token.covenantId, tradable]);
  const wallet = enabled && pubkey && market ? { pubkey } : null;

  // the book is verified as deep as the order being planned walks it (a market / close order: its amount), in steps so that typing does not read again at every digit
  const bucket = depthBucket(depth);
  const [marketRun, marketState] = usePart<MarketPart>(market ? () => readMarketPart(services, market, bucket > 0n ? { depth: bucket } : {}) : null, [services, market, bucket]);
  const [coinsRun, coinsState] = usePart<CoinsPart>(wallet ? () => readCoinsPart(services, wallet) : null, [services, wallet?.pubkey]);
  const [holdRun, holdState] = usePart<HoldingsPart>(wallet && market ? () => readHoldingsPart(services, wallet, token, market) : null, [services, wallet?.pubkey, market]);

  // a wallet that connects (or comes back) reads the market again too: the part read without one may be minutes old
  useEffect(() => {
    if (wallet) void marketRun?.since(Date.now() - 5_000).catch(() => undefined);
  }, [!!wallet]);
  const walletParts = useCallback(() => {
    quiet(coinsRun);
    quiet(holdRun);
  }, [coinsRun, holdRun]);
  useInterval(() => {
    quiet(marketRun);
    walletParts();
  }, ENV_REFRESH_MS, !!wallet);
  // the book moved: the market part; a fill of one of the wallet's orders: its coins and holdings too (a payout, a smaller order)
  const own = useRef<Set<string>>(new Set());
  own.current = new Set((holdState.value?.ownOrders ?? []).map((o) => o.covenantId.toLowerCase()));
  const bookNotice = useNoticeClock(services.feed);
  const ownNotice = useNoticeClock(services.feed);
  const noticed = (clock: { stamp(): void }, yes: boolean): boolean => (yes && clock.stamp(), yes);
  useLiveRefresh(services, wallet ? [`book:${token.covenantId}`] : [], (e) => noticed(bookNotice, !e.token || e.token === token.covenantId), () => quiet(marketRun));
  useLiveRefresh(services, wallet ? [`fills:${token.covenantId}`] : [], (e) => noticed(ownNotice, !!e.order && own.current.has(e.order.toLowerCase())), walletParts);

  const env = useMemo(
    () => (wallet && market && marketState.value && coinsState.value ? assemblePlanEnv(services, wallet, market, { market: marketState.value, coins: coinsState.value, holdings: holdState.value }) : null),
    [wallet?.pubkey, market, marketState.value, coinsState.value, holdState.value],
  );
  const waiting: EnvWait[] = wallet
    ? [...(marketState.value ? [] : (['market'] as const)), ...(coinsState.value ? [] : (['coins'] as const)), ...(holdState.value ? [] : (['holdings'] as const))]
    : [];

  const assemble = useCallback(
    (m: MarketPart, c: CoinsPart, h: HoldingsPart | null): PlanEnv | null => (wallet && market ? assemblePlanEnv(services, wallet, market, { market: m, coins: c, holdings: h }) : null),
    [wallet?.pubkey, market],
  );
  const since = useCallback(
    async (t: number, parts: readonly EnvWait[] = ['market', 'coins', 'holdings']): Promise<PlanEnv | null> => {
      // never a read older than a change known since \`t\`: one that started after the newest notice concerning the part and this page's newest submission
      const one = <T,>(r: Serialized<T> | null, part: EnvWait, noticeAt: number): Promise<T | null> =>
        maybe(r, (x) => {
          const l = x.latest();
          return parts.includes(part) || !l ? x.since(Math.max(t, noticeAt + 1, pendingSpends.lastAddedAt)) : Promise.resolve(l.value);
        });
      const [m, c, h] = await Promise.all([one(marketRun, 'market', bookNotice.at()), one(coinsRun, 'coins', ownNotice.at()), one(holdRun, 'holdings', ownNotice.at())]);
      return m && c && h ? assemble(m, c, h) : null;
    },
    [marketRun, coinsRun, holdRun, assemble, bookNotice, ownNotice],
  );
  const refresh = useCallback(async (): Promise<PlanEnv | null> => {
    const [m, c, h] = await Promise.all([maybe(marketRun, (r) => r.run()), maybe(coinsRun, (r) => r.run()), maybe(holdRun, (r) => r.run())]);
    return m && c && h ? assemble(m, c, h) : null;
  }, [marketRun, coinsRun, holdRun, assemble]);
  const snapshot = useCallback(() => {
    const m = marketRun?.latest();
    const c = coinsRun?.latest();
    const h = holdRun?.latest() ?? null;
    const e = m && c ? assemble(m.value, c.value, h?.value ?? null) : null;
    return e && m && c ? { env: e, at: Math.min(m.at, c.at, h?.at ?? Infinity), clockAt: c.doneAt, complete: !!h } : null;
  }, [marketRun, coinsRun, holdRun, assemble]);
  const stale = useCallback(
    (clickAt: number): EnvWait[] => {
      // the book's notices and the fills of the wallet's own orders are what would have started a newer read
      const ctx = { liveSince: services.feed?.liveSince([`book:${token.covenantId}`, `fills:${token.covenantId}`]) ?? null, lastSubmitAt: pendingSpends.lastAddedAt };
      const parts: [EnvWait, Serialized<unknown> | null, number][] = [['market', marketRun, bookNotice.at()], ['coins', coinsRun, ownNotice.at()], ['holdings', holdRun, ownNotice.at()]];
      return parts.filter(([, r, n]) => !partCurrent(factsOf(r, n), clickAt, ctx)).map(([k]) => k);
    },
    [services, token.covenantId, marketRun, coinsRun, holdRun, bookNotice, ownNotice],
  );

  const failed = wallet ? [coinsState, holdState, marketState].find((s) => s.error !== null) : undefined;
  const error = failed?.error ?? null;
  return { env, waiting, error, errorSource: failed?.source ?? null, loading: marketState.loading || coinsState.loading || holdState.loading, refresh, since, snapshot, stale };
}

/**
 * Keeps a PairPlanEnv for (wallet, base A, quote B): both tokens' UTXOs, the pair book, own pair orders of the pair (app/env.ts `buildPairPlanEnv`),
 * read as one part. A change of either token's book (`book:<A>`, `book:<B>`: a pair order notifies both) refreshes it.
 */
export function usePairTicketEnv(base: TokenInfo, quote: TokenInfo, enabled: boolean, depth: bigint = 0n): TicketEnv {
  const services = useServices();
  const pubkey = useWallet().info?.pubkey ?? null;
  const on = enabled && !!pubkey;
  const bucket = depthBucket(depth);
  const [run, state] = usePart<PlanEnv>(on && pubkey ? () => buildPairPlanEnv(services, { pubkey }, base, quote, bucket > 0n ? { depth: bucket } : {}) : null, [services, pubkey, base.covenantId, quote.covenantId, on, bucket]);
  useInterval(() => quiet(run), ENV_REFRESH_MS, !!run);
  const notice = useNoticeClock(services.feed);
  useLiveRefresh(
    services,
    on ? [`book:${base.covenantId}`, `book:${quote.covenantId}`] : [],
    (e) => {
      const yes = !e.token || e.token === base.covenantId || e.token === quote.covenantId;
      if (yes) notice.stamp();
      return yes;
    },
    () => quiet(run),
  );
  const since = useCallback(
    async (t: number) => maybe(run, (r) => r.since(Math.max(t, notice.at() + 1, pendingSpends.lastAddedAt))).then((e) => (e ? dropPendingSpends(e) : null)),
    [run, notice],
  );
  const refresh = useCallback(async () => maybe(run, (r) => r.run()).then((e) => (e ? dropPendingSpends(e) : null)), [run]);
  const snapshot = useCallback(() => {
    const l = run?.latest();
    return l ? { env: dropPendingSpends(l.value), at: l.at, clockAt: l.doneAt, complete: true } : null;
  }, [run]);
  const stale = useCallback(
    (clickAt: number): EnvWait[] => {
      const ctx = { liveSince: services.feed?.liveSince([`book:${base.covenantId}`, `book:${quote.covenantId}`]) ?? null, lastSubmitAt: pendingSpends.lastAddedAt };
      return partCurrent(factsOf(run, notice.at()), clickAt, ctx) ? [] : ['pair'];
    },
    [services, base.covenantId, quote.covenantId, run, notice],
  );
  return { env: on ? state.value : null, waiting: on && !state.value ? ['pair'] : [], error: on ? state.error : null, errorSource: on ? state.source : null, loading: state.loading, refresh, since, snapshot, stale };
}

export interface TicketPlan {
  plan: OrderPlan | null;
  /** the plan belongs to the current form and environment (false while a re-plan is pending) */
  current: boolean;
  /** a re-plan is waiting for the debounce or running */
  pending: boolean;
  /** the pending re-plan answers an edit of the form (a user action), not only a new environment (a tick): only that one may show "Calculating..." at once */
  editPending: boolean;
  /** planOrder threw: a bug, shown as an error (planners report user problems as issues) */
  crash: string | null;
}

const key = (i: Intent | null): string => (i === null ? '' : JSON.stringify(i, (_k, v) => (typeof v === 'bigint' ? `${v}n` : v)));

/** Plans `intent` against `env`, 300 ms after the last change of the intent; an environment refresh re-plans at once. */
export function useTicketPlan(env: PlanEnv | null, intent: Intent | null): TicketPlan {
  const k = useMemo(() => key(intent), [intent]);
  const [out, setOut] = useState<{ plan: OrderPlan | null; k: string; envId: PlanEnv | null; crash: string | null }>({ plan: null, k: '', envId: null, crash: null });
  const lastKey = useRef('');
  useEffect(() => {
    if (!env || intent === null) {
      lastKey.current = k;
      setOut({ plan: null, k, envId: env, crash: null });
      return;
    }
    const run = () => {
      try {
        setOut({ plan: planOrder(env, intent), k, envId: env, crash: null });
      } catch (e) {
        setOut({ plan: null, k, envId: env, crash: e instanceof Error ? e.message : String(e) });
      }
    };
    const changed = k !== lastKey.current;
    lastKey.current = k;
    if (!changed) {
      run();
      return;
    }
    const h = setTimeout(run, PLAN_DEBOUNCE_MS);
    return () => clearTimeout(h);
  }, [env, k]);
  const current = out.k === k && out.envId === env;
  const pending = intent !== null && env !== null && !current;
  return { plan: out.plan, current, pending, editPending: pending && out.k !== k, crash: out.crash };
}
