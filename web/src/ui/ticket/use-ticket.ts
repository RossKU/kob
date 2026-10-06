// Data hooks of the order ticket: the planning environment (node clock + UTXOs + indexer book + own orders) kept fresh, and the debounced plan.
import { useCallback, useEffect, useMemo, useRef, useState } from 'preact/hooks';
import { useServices, useWallet } from '../../app/context';
import { buildPairPlanEnv, buildPlanEnv } from '../../app/env';
import type { Intent } from '../../kob/plan';
import { planOrder } from '../../kob/plan';
import type { OrderPlan, PlanEnv } from '../../kob/plan-types';
import type { TokenInfo } from '../../kob/registry';
import { useLiveRefresh } from '../market/live';

export const PLAN_DEBOUNCE_MS = 300;
/** the environment is re-read at least this often (UTXOs and the book move; a feed event or a poll also triggers it) */
export const ENV_REFRESH_MS = 20_000;

export interface TicketEnv {
  env: PlanEnv | null;
  /** the environment could not be read (node unreachable ...): the message and a retry */
  error: string | null;
  loading: boolean;
  refresh(): Promise<PlanEnv | null>;
}

/** Keeps a PlanEnv for (wallet, token). `enabled` false (no wallet, wrong network, untradable token) stops all requests. */
export function useTicketEnv(token: TokenInfo, enabled: boolean): TicketEnv {
  const services = useServices();
  const wallet = useWallet();
  const pubkey = wallet.info?.pubkey ?? null;
  const [state, setState] = useState<{ env: PlanEnv | null; error: string | null; loading: boolean }>({ env: null, error: null, loading: false });
  const seq = useRef(0);
  const alive = useRef(true);
  useEffect(() => {
    alive.current = true;
    return () => {
      alive.current = false;
    };
  }, []);

  const refresh = useCallback(async (): Promise<PlanEnv | null> => {
    if (!enabled || !pubkey) return null;
    const id = ++seq.current;
    setState((s) => ({ ...s, loading: true }));
    try {
      const env = await buildPlanEnv(services, { pubkey }, token);
      if (alive.current && id === seq.current) setState({ env, error: null, loading: false });
      return env;
    } catch (e) {
      if (alive.current && id === seq.current) setState((s) => ({ env: s.env, error: e instanceof Error ? e.message : String(e), loading: false }));
      return null;
    }
  }, [services, pubkey, token.covenantId, enabled]);

  useEffect(() => {
    if (!enabled || !pubkey) {
      seq.current++;
      setState({ env: null, error: null, loading: false });
      return;
    }
    void refresh();
    const id = setInterval(() => void refresh(), ENV_REFRESH_MS);
    return () => clearInterval(id);
  }, [refresh, enabled, pubkey]);

  // a book change or a poll while the socket is down: the book (FOK / self-trade checks, market prices) is part of the environment
  useLiveRefresh(services, [`book:${token.covenantId}`], (e) => !e.token || e.token === token.covenantId, () => void refresh());

  return { env: state.env, error: state.error, loading: state.loading, refresh };
}

/**
 * Keeps a PairPlanEnv for (wallet, base A, quote B): both tokens' UTXOs, the pair book, own pair orders of the pair (app/env.ts `buildPairPlanEnv`).
 * A change of either token's book (`book:<A>`, `book:<B>`: a pair order notifies both) refreshes it.
 */
export function usePairTicketEnv(base: TokenInfo, quote: TokenInfo, enabled: boolean): TicketEnv {
  const services = useServices();
  const wallet = useWallet();
  const pubkey = wallet.info?.pubkey ?? null;
  const [state, setState] = useState<{ env: PlanEnv | null; error: string | null; loading: boolean }>({ env: null, error: null, loading: false });
  const seq = useRef(0);
  const alive = useRef(true);
  useEffect(() => {
    alive.current = true;
    return () => {
      alive.current = false;
    };
  }, []);

  const refresh = useCallback(async (): Promise<PlanEnv | null> => {
    if (!enabled || !pubkey) return null;
    const id = ++seq.current;
    setState((s) => ({ ...s, loading: true }));
    try {
      const env = await buildPairPlanEnv(services, { pubkey }, base, quote);
      if (alive.current && id === seq.current) setState({ env, error: null, loading: false });
      return env;
    } catch (e) {
      if (alive.current && id === seq.current) setState((s) => ({ env: s.env, error: e instanceof Error ? e.message : String(e), loading: false }));
      return null;
    }
  }, [services, pubkey, base.covenantId, quote.covenantId, enabled]);

  useEffect(() => {
    if (!enabled || !pubkey) {
      seq.current++;
      setState({ env: null, error: null, loading: false });
      return;
    }
    void refresh();
    const id = setInterval(() => void refresh(), ENV_REFRESH_MS);
    return () => clearInterval(id);
  }, [refresh, enabled, pubkey]);

  useLiveRefresh(services, [`book:${base.covenantId}`, `book:${quote.covenantId}`], (e) => !e.token || e.token === base.covenantId || e.token === quote.covenantId, () => void refresh());

  return { env: state.env, error: state.error, loading: state.loading, refresh };
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
