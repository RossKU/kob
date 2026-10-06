// The indexer gate: the bots trade only while the indexer they read (`env.indexer`, GET /v1/health) is following the chain, or is at most
// `maxLagSecs` (default 30 s, the executor's `max_lag_secs`) behind it.
//
// Why: on 2026-10-01 the TN10 indexers fell 3.4 h behind in a transaction flood and the bots kept placing, amending and cancelling
// against the frozen book (216 transactions on chain that no indexer saw). A book that lags the chain lists orders that may already
// be spent and misses the bots' own transactions, so every plan made from it is a guess. The decision logic below is pure (no clock,
// no I/O) so it can be unit-tested; `IndexerGate` adds the polling, the one-line transition logs and the counters.
//
// Scope (see `BotWallet.submit`, the single choke point): every transaction that goes through a BotWallet is gated (orders, amends,
// cancels, cancel-all, fan-out, consolidation, the market maker and the traders). NOT gated:
//   - `bank.ts` (plain KAS payouts through the SDK's own generator: nothing in them reads the indexer);
//   - `setup.ts` and the operator CLIs (cancel, balances): they run without a gate (`env.gate` is set only by `runBots`), setup because
//     the issuance happens before there is anything to follow, the CLIs because an operator cancelling orders during a stall must not
//     be blocked by the very condition they are repairing.
//   - the x402 payer submits through the x402 SDK, not a BotWallet; its LOOP waits on the gate instead (`bots/x402.ts`): the facilitator
//     settles and the checker verifies through the indexers, and swap quotes read the book, so a payment made while the indexer
//     lags cannot be accounted for.
import { errText, type Logger, type Stats } from './log.ts';

/** the fields of the indexer's GET /v1/health the gate reads (the web `HealthView` plus `caught_up`) */
export interface HealthLike {
  state?: string;
  caught_up?: boolean;
  lag_daa?: number | null;
  last_error?: string | null;
}

/** DAA per second of the chain (10 BPS): converts the lag tolerance in seconds to DAA */
export const DAA_PER_SECOND = 10;
/** the default lag tolerance of the gate and of the executors' planning gates (`max_lag_secs`) */
export const DEFAULT_MAX_LAG_SECS = 30;

export interface GateVerdict {
  /** trading allowed */
  open: boolean;
  state: string;
  lagDaa: number | null;
  lastError: string | null;
  /** one line for logs and refusals */
  reason: string;
}

/**
 * What a health answer (or the failure to get one) means. Trading is allowed when the indexer says `caught_up === true`; an
 * indexer that predates the field is judged by `state === 'following'`. With `maxLagDaa > 0` it is also allowed while the indexer is
 * `following` or `catching_up` and its `lag_daa` is at most `maxLagDaa` (a book that is seconds old only costs the bots a lost race;
 * a batch of a flooded chain takes 10 to 30 s, which flips the indexer to `catching_up` with the book still fresh). An unknown lag, or
 * any other state (starting, node_unavailable, gap, stopped), stays closed. A failed request is "not following": a book that cannot
 * even be asked about its own health is not one to trade against.
 */
export function judgeHealth(h: HealthLike | null | undefined, error?: unknown, maxLagDaa = 0): GateVerdict {
  if (error !== undefined || !h || typeof h !== 'object') {
    const msg = error === undefined ? 'empty health answer' : errText(error);
    return { open: false, state: 'unreachable', lagDaa: null, lastError: null, reason: `health request failed: ${msg}` };
  }
  const state = typeof h.state === 'string' ? h.state : 'unknown';
  const lagDaa = typeof h.lag_daa === 'number' && Number.isFinite(h.lag_daa) ? h.lag_daa : null;
  const lastError = typeof h.last_error === 'string' && h.last_error ? h.last_error : null;
  const strict = typeof h.caught_up === 'boolean' ? h.caught_up : state === 'following';
  const within = maxLagDaa > 0 && (state === 'following' || state === 'catching_up') && lagDaa !== null && lagDaa <= maxLagDaa;
  const open = strict || within;
  if (open) return { open, state, lagDaa, lastError, reason: strict ? 'following' : `${state}, ${lagDaa} DAA behind (within ${maxLagDaa})` };
  const parts = [state === 'unknown' ? 'state unknown' : state];
  if (lagDaa !== null) parts.push(`${lagDaa} DAA behind`);
  if (lastError) parts.push(`last error: ${lastError}`);
  return { open, state, lagDaa, lastError, reason: parts.join(', ') };
}

export interface GateState {
  /** null until the first verdict */
  open: boolean | null;
  /** when the current state began (ms) */
  since: number;
}

export type GateTransition = { kind: 'paused'; verdict: GateVerdict } | { kind: 'resumed'; pausedMs: number; verdict: GateVerdict };

/**
 * The state machine: one verdict at time `now` -> the next state and the transition it causes, if any. A transition happens only when the
 * open/closed answer CHANGES (a stream of "catching_up" verdicts is one pause), except that a first verdict of "closed" counts as a pause
 * (the bots start while the indexer lags: say so) and a first verdict of "open" is silent.
 */
export function advance(prev: GateState, v: GateVerdict, now: number): { next: GateState; transition: GateTransition | null } {
  if (prev.open === v.open) return { next: prev, transition: null };
  const next: GateState = { open: v.open, since: now };
  if (!v.open) return { next, transition: { kind: 'paused', verdict: v } };
  if (prev.open === null) return { next, transition: null };
  return { next, transition: { kind: 'resumed', pausedMs: Math.max(0, now - prev.since), verdict: v } };
}

export interface IndexerGateDeps {
  health: () => Promise<HealthLike>;
  now?: () => number;
  log?: Pick<Logger, 'info' | 'warn'>;
  stats?: Pick<Stats, 'inc' | 'set'>;
  /** how long a verdict is trusted before the next request (default 5 s) */
  pollMs?: number;
  /** trade while the indexer lags by at most this many DAA (default 0: only while caught up); see `judgeHealth` */
  maxLagDaa?: number;
}

export class IndexerGate {
  private state: GateState = { open: null, since: 0 };
  private verdict: GateVerdict | null = null;
  private checkedAt = -Infinity;
  private inflight: Promise<GateVerdict> | null = null;
  private readonly now: () => number;
  readonly pollMs: number;
  private readonly d: IndexerGateDeps;

  constructor(d: IndexerGateDeps) {
    this.d = d;
    this.now = d.now ?? Date.now;
    this.pollMs = d.pollMs ?? 5000;
  }

  /** last known verdict (closed before the first check) */
  isOpen(): boolean {
    return this.state.open === true;
  }

  /**
   * The current verdict: the cached one while it is younger than `pollMs`, else a fresh health request (concurrent callers share one
   * request). Callers drive the polling (every submit, every wait loop), so no timer is needed and an idle bot makes no requests.
   */
  check(force = false): Promise<GateVerdict> {
    if (!force && this.verdict && this.now() - this.checkedAt < this.pollMs) return Promise.resolve(this.verdict);
    this.inflight ??= this.refresh().finally(() => {
      this.inflight = null;
    });
    return this.inflight;
  }

  private async refresh(): Promise<GateVerdict> {
    let v: GateVerdict;
    try {
      v = judgeHealth(await this.d.health(), undefined, this.d.maxLagDaa ?? 0);
    } catch (e) {
      v = judgeHealth(null, e);
    }
    const now = this.now();
    this.verdict = v;
    this.checkedAt = now;
    const { next, transition } = advance(this.state, v, now);
    this.state = next;
    this.d.stats?.set('gate_open', v.open);
    this.d.stats?.set('gate_state', v.state);
    if (transition?.kind === 'paused') {
      this.d.stats?.inc('gate_paused');
      this.d.log?.warn('indexer not following: trading paused', { state: v.state, lagDaa: v.lagDaa, lastError: v.lastError, reason: v.reason });
    } else if (transition?.kind === 'resumed') {
      this.d.log?.info('indexer following again: trading resumed', { pausedSec: Math.round(transition.pausedMs / 1000), lagDaa: v.lagDaa, state: v.state });
    }
    return v;
  }

  /** A refusal for `what` (an action name) while the indexer is not following; null when trading is allowed. Counts `gate_blocked:<what>`. */
  async refusal(what: string): Promise<GateRefusal | null> {
    const v = await this.check();
    if (v.open) return null;
    this.d.stats?.inc('gate_blocked');
    this.d.stats?.inc(`gate_blocked:${what}`);
    return { ok: false, error: `indexer not following: ${v.reason}`, paused: true };
  }

  /**
   * Resolves once the indexer is following (true), or as soon as `stop()` says so (false). Re-checks every `pollMs`: bot loops call this
   * instead of spinning through plan-and-refuse cycles while paused.
   */
  async waitOpen(stop: () => boolean = () => false, sleep: (ms: number) => Promise<void> = defaultSleep): Promise<boolean> {
    for (;;) {
      if (stop()) return false;
      if ((await this.check()).open) return true;
      await sleep(this.pollMs);
    }
  }
}

/** what `BotWallet.submit` returns instead of submitting while the gate is closed */
export interface GateRefusal {
  ok: false;
  error: string;
  paused: true;
}

const defaultSleep = (ms: number) => new Promise<void>((r) => setTimeout(r, ms));

/** The gate decision at the submit choke point; no gate (setup, CLIs) means no refusal. */
export function gateRefusal(gate: IndexerGate | undefined, what: string): Promise<GateRefusal | null> {
  return gate ? gate.refusal(what) : Promise.resolve(null);
}
