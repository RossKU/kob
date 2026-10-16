// Data hooks of the order ticket: the planning environment (node clock + UTXOs + indexer book + own orders) kept fresh, and the debounced plan.
import { useCallback, useEffect, useMemo, useRef, useState } from 'preact/hooks';
import { useServices, useWallet } from '../../app/context';
import { buildPairPlanEnv, buildPlanEnv } from '../../app/env';
import type { Intent } from '../../kob/plan';
import { planOrder } from '../../kob/plan';
import type { OrderPlan, PlanEnv } from '../../kob/plan-types';
import type { TokenInfo } from '../../kob/registry';
import { useInterval } from '../kit/hooks';
import { useLiveRefresh } from '../market/live';

export const PLAN_DEBOUNCE_MS = 300;
/** the environment is re-read at least this often (UTXOs and the book move; a feed event or a poll also triggers it); paused on a hidden page */
export const ENV_REFRESH_MS = 20_000;
/**
 * Review reuses an environment whose build started at most this long ago (or the build in flight) instead of reading everything again: the
 * transaction is still built from data a few seconds old at most, and the confirmation screen re-reads every input on the NODE before anything is
 * signed (kob/node-verify.ts), so a coin spent meanwhile blocks the signature instead of slipping through. The node clock is read anew at Review
 * (OrderTicket): DAA times are counted from the moment of the click, not from the reused read.
 */
export const REVIEW_REUSE_MS = 3_000;

export interface TicketEnv {
  env: PlanEnv | null;
  /** the environment could not be read (node unreachable ...): the message and a retry */
  error: string | null;
  loading: boolean;
  /** re-reads everything now: the result reflects state read after the call (after a submission: the spent coins are gone) */
  refresh(): Promise<PlanEnv | null>;
  /** an environment read at most `maxAgeMs` ago (default REVIEW_REUSE_MS), else the build in flight, else a new build (`Serialized.fresh`) */
  fresh(maxAgeMs?: number): Promise<PlanEnv | null>;
}

/**
 * Serialises a load: one run at a time; every call made during a run shares ONE follow-up run that starts after it ends, so its result
 * reflects state read after the call. A feed that fires faster than a load takes never starves the result (each run completes and is shown).
 * `fresh` reuses a recent result or the run in flight instead (Review); `invalidate` makes every run started before it unusable for that.
 */
export class Serialized<T> {
  private running: { p: Promise<T>; at: number } | null = null;
  private next: Promise<T> | null = null;
  private last: { value: T; at: number } | null = null;
  private notBefore = -Infinity;
  private readonly now: () => number;
  private readonly reusable: (v: T) => boolean;

  constructor(
    private readonly load: () => Promise<T>,
    o: { now?: () => number; reusable?: (v: T) => boolean } = {},
  ) {
    this.now = o.now ?? Date.now;
    this.reusable = o.reusable ?? (() => true);
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

  /** Runs started before now (and their results) no longer count as fresh: the state they read is known to have changed (a submission). */
  invalidate(): void {
    this.notBefore = this.now();
  }

  /**
   * A result at most `maxAgeMs` old (measured from the start of its run, when its reads began): the last one when it is that recent, else the run in
   * flight (whatever its age: it is reading now, and waiting for it is never slower than queueing a new run behind it), else a new run (or the
   * follow-up already queued). Nothing started before the last `invalidate` is reused, nor a result `reusable` rejects (a failure).
   */
  fresh(maxAgeMs: number): Promise<T> {
    const now = this.now();
    const l = this.last;
    if (l && l.at >= now - maxAgeMs && l.at >= this.notBefore && this.reusable(l.value)) return Promise.resolve(l.value);
    if (this.running && this.running.at >= this.notBefore) return this.running.p;
    return this.run();
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
      if (!this.last || this.last.at <= at) this.last = { value, at };
      clear();
    }, clear);
    return p;
  }
}

type EnvState = { env: PlanEnv | null; error: string | null; loading: boolean };

/**
 * The shared core of the two environment hooks: `build` re-runs on the interval (paused on a hidden page) and on `topics` feed events, serialised
 * (`Serialized`).
 */
function useEnv(build: (() => Promise<PlanEnv>) | null, deps: unknown[], topics: string[], matches: (e: { token?: string | null }) => boolean): TicketEnv {
  const services = useServices();
  const [state, setState] = useState<EnvState>({ env: null, error: null, loading: false });
  // a generation per (deps): a result of an earlier generation (another token, wallet gone) is dropped
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
    if (!build) return null;
    return new Serialized<PlanEnv | null>(
      async () => {
        if (alive.current && g === gen.current) setState((s) => ({ ...s, loading: true }));
        try {
          const env = await build();
          if (alive.current && g === gen.current) setState({ env, error: null, loading: false });
          return env;
        } catch (e) {
          if (alive.current && g === gen.current) setState((s) => ({ env: s.env, error: e instanceof Error ? e.message : String(e), loading: false }));
          return null;
        }
      },
      { reusable: (env) => env !== null },
    );
  }, deps);

  // the background re-reads (interval, feed) only queue a run; an explicit refresh also makes everything read before it stale for `fresh`
  const update = useCallback((): void => void runner?.run(), [runner]);
  const refresh = useCallback(async (): Promise<PlanEnv | null> => {
    if (!runner) return null;
    runner.invalidate();
    return runner.run();
  }, [runner]);
  const fresh = useCallback(async (maxAgeMs: number = REVIEW_REUSE_MS): Promise<PlanEnv | null> => (runner ? runner.fresh(maxAgeMs) : null), [runner]);

  useEffect(() => {
    if (!runner) {
      setState({ env: null, error: null, loading: false });
      return;
    }
    update();
  }, [runner, update]);
  useInterval(update, ENV_REFRESH_MS, !!runner);

  useLiveRefresh(services, topics, matches, update);

  return { env: state.env, error: state.error, loading: state.loading, refresh, fresh };
}

/** Keeps a PlanEnv for (wallet, token). `enabled` false (no wallet, wrong network, untradable token) stops all requests. */
export function useTicketEnv(token: TokenInfo, enabled: boolean): TicketEnv {
  const services = useServices();
  const pubkey = useWallet().info?.pubkey ?? null;
  const build = enabled && pubkey ? () => buildPlanEnv(services, { pubkey }, token) : null;
  // a book change or a poll while the socket is down: the book (FOK / self-trade checks, market prices) is part of the environment
  return useEnv(build, [services, pubkey, token.covenantId, enabled], [`book:${token.covenantId}`], (e) => !e.token || e.token === token.covenantId);
}

/**
 * Keeps a PairPlanEnv for (wallet, base A, quote B): both tokens' UTXOs, the pair book, own pair orders of the pair (app/env.ts `buildPairPlanEnv`).
 * A change of either token's book (`book:<A>`, `book:<B>`: a pair order notifies both) refreshes it.
 */
export function usePairTicketEnv(base: TokenInfo, quote: TokenInfo, enabled: boolean): TicketEnv {
  const services = useServices();
  const pubkey = useWallet().info?.pubkey ?? null;
  const build = enabled && pubkey ? () => buildPairPlanEnv(services, { pubkey }, base, quote) : null;
  return useEnv(
    build,
    [services, pubkey, base.covenantId, quote.covenantId, enabled],
    [`book:${base.covenantId}`, `book:${quote.covenantId}`],
    (e) => !e.token || e.token === base.covenantId || e.token === quote.covenantId,
  );
}

export interface TicketPlan {
  plan: OrderPlan | null;
  /** the plan belongs to the current form and environment (false while a re-plan is pending) */
  current: boolean;
  /** a re-plan is waiting for the debounce or running */
  pending: boolean;
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
  return { plan: out.plan, current, pending: intent !== null && env !== null && !current, crash: out.crash };
}
