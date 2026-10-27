// Indexer failover. The app reads the book, charts, tokens and orders from ONE indexer at a time: the primary (`config.indexerUrl`) while it is healthy,
// else the best healthy one of the further indexers the host listed (`extraIndexerUrls`), and back to the primary once it has recovered. Nothing is
// built from the node: an indexer that is not caught up, or on another network, is never used.
//
// What counts as healthy is what the app already uses for the primary: `assessIndexerHealth` (ui/shell/health.ts) over `/v1/health` (level "ok": following
// or within the lag tolerance, same network). A backup must also report the KOB1 payload version this build writes. A backup whose store started late
// (`book_complete: false`) can serve the live book but its history may be partial: it ranks after the complete ones and the banner says so.
//
// The controller is fed by the status poll (`report`: the health read of the ACTIVE indexer, ui/shell/status.ts) and probes the others only when it
// has to: a failing active indexer is re-checked a few times with a growing delay, then every indexer is probed once and one is chosen; while on a backup
// the primary is probed on a slow, backing-off timer and used again after `RECOVER_PROBES` good answers in a row. Evaluations are rate limited.
import type { IndexerApi } from './indexer';
import type { HealthView } from './indexer-types';
import { assessIndexerHealth } from '../ui/shell/health';

/** The KOB1 payload version this build writes (crates/kob-protocol `PAYLOAD_VERSION`; a test keeps the two equal). A backup must report it. */
export const KOB1_PAYLOAD_VERSION = 4;

/** Re-checks of a failing active indexer before the others are probed (delays `RECHECK_BASE_MS`, doubled each time). */
export const RECHECKS = 2;
export const RECHECK_BASE_MS = 2_000;
/** Spacing of evaluations while nothing usable is found, and of probes of a primary that is still down: base, doubled up to the cap. */
export const BACKOFF_BASE_MS = 5_000;
export const BACKOFF_MAX_MS = 60_000;
/** Back to the primary after this many good probes in a row, `CONFIRM_MS` apart. */
export const RECOVER_PROBES = 2;
export const CONFIRM_MS = 5_000;
/** A health probe of a member is cut off after this long. */
export const PROBE_TIMEOUT_MS = 6_000;

export interface Probe {
  /** the last answer; null when the read failed */
  health: HealthView | null;
  error: Error | null;
}

export type MemberCode = ReturnType<typeof assessIndexerHealth>['code'] | 'payload-version';
/** `complete`: the store reports `book_complete: true`; `partial`: `false` (started late: charts and older orders may be missing); `unknown`: not reported. */
export type History = 'complete' | 'partial' | 'unknown';

export interface Verdict {
  usable: boolean;
  code: MemberCode;
  history: History;
}

const historyOf = (h: HealthView | null): History => (h?.book_complete === true ? 'complete' : h?.book_complete === false ? 'partial' : 'unknown');

/**
 * Whether one indexer may serve the app, from its last health probe (pure). The primary is judged exactly as the status bar judges it (reachable, same
 * network, following within the lag tolerance); a backup also has to report the payload version of this build.
 */
export function judge(probe: Probe | null | undefined, o: { network: string; primary: boolean }): Verdict {
  if (!probe) return { usable: false, code: 'loading', history: 'unknown' };
  const a = assessIndexerHealth({ health: probe.health, error: probe.error, configured: true, network: o.network });
  const history = historyOf(probe.health);
  if (a.level !== 'ok') return { usable: false, code: a.code, history };
  if (!o.primary && probe.health?.payload_version !== KOB1_PAYLOAD_VERSION) return { usable: false, code: 'payload-version', history };
  return { usable: true, code: a.code, history };
}

const rank = (h: History): number => (h === 'complete' ? 0 : h === 'unknown' ? 1 : 2);

/**
 * The member to read from (pure; `v[0]` is the primary). The primary whenever it is usable; else the best usable backup (complete history before unknown
 * before partial, host order within a rank, the one in use kept on a tie so two equal backups do not take turns); none usable: the primary (the banners
 * of its own state apply).
 */
export function selectMember(v: readonly Verdict[], current: number): number {
  if (v[0]?.usable) return 0;
  let best = Infinity;
  for (let i = 1; i < v.length; i++) if (v[i]!.usable) best = Math.min(best, rank(v[i]!.history));
  if (best === Infinity) return 0;
  if (current > 0 && v[current]?.usable && rank(v[current]!.history) === best) return current;
  for (let i = 1; i < v.length; i++) if (v[i]!.usable && rank(v[i]!.history) === best) return i;
  return 0;
}

export interface FailoverState {
  /** index into the members (0 = the primary) */
  readonly active: number;
  /** the active indexer's base URL, and its host for the banner */
  readonly label: string;
  readonly host: string;
  readonly onBackup: boolean;
  /** the active indexer is a backup whose store reports `book_complete: false`: charts and older orders may be missing */
  readonly historyPartial: boolean;
  /** counts the switches: data read under an older epoch came from another indexer */
  readonly epoch: number;
  /** the members the key facts are cross-checked against: every one but the active, and the primary only while it answers */
  readonly verifiers: readonly number[];
}

export interface FailoverMember {
  /** the base URL as configured */
  label: string;
  api: Pick<IndexerApi, 'health'>;
}

export interface FailoverOptions {
  members: readonly FailoverMember[];
  network: string;
  /** injectable for tests */
  setTimer?: (fn: () => void, ms: number) => unknown;
  clearTimer?: (h: unknown) => void;
  now?: () => number;
  /** probes of a hidden page wait until it is shown */
  visible?: () => boolean;
}

const hostOf = (url: string): string => {
  try {
    return new URL(url).host || url;
  } catch {
    return url;
  }
};
const backoff = (n: number): number => Math.min(BACKOFF_MAX_MS, BACKOFF_BASE_MS * 2 ** Math.min(n, 10));
const toError = (e: unknown): Error => (e instanceof Error ? e : new Error(String(e)));

export class IndexerFailover {
  private readonly members: readonly FailoverMember[];
  private readonly network: string;
  private readonly setTimer: (fn: () => void, ms: number) => unknown;
  private readonly clearTimer: (h: unknown) => void;
  private readonly now: () => number;
  private readonly visible: () => boolean;
  private readonly probes: (Probe | null)[];
  private readonly listeners = new Set<(s: FailoverState) => void>();
  private active = 0;
  private epochN = 0;
  private everGood = false;
  private bad = 0;
  private evalAttempts = 0;
  private nextEvalAt = 0;
  private evaluating = false;
  private goodPrimary = 0;
  private recoverFails = 0;
  private recheckTimer: unknown = null;
  private evalTimer: unknown = null;
  private recoverTimer: unknown = null;
  private disposed = false;
  private snap: FailoverState;

  constructor(o: FailoverOptions) {
    if (o.members.length < 2) throw new Error('IndexerFailover needs the primary and at least one backup');
    this.members = o.members;
    this.network = o.network;
    this.setTimer = o.setTimer ?? ((fn, ms) => setTimeout(fn, ms));
    this.clearTimer = o.clearTimer ?? ((h) => clearTimeout(h as ReturnType<typeof setTimeout>));
    this.now = o.now ?? Date.now;
    this.visible = o.visible ?? (() => typeof document === 'undefined' || document.visibilityState !== 'hidden');
    this.probes = o.members.map(() => null);
    this.snap = this.makeState();
  }

  /** The state now (a new object whenever something it holds changed). */
  get state(): FailoverState {
    return this.snap;
  }

  get epoch(): number {
    return this.epochN;
  }

  /** The base URL of the member `i`. */
  labelOf(i: number): string {
    return this.members[i]!.label;
  }

  /** Calls `cb` on every change of the state (a switch, or a change of the verifier set or the history flag). */
  subscribe(cb: (s: FailoverState) => void): () => void {
    this.listeners.add(cb);
    return () => this.listeners.delete(cb);
  }

  /**
   * The health read of the ACTIVE indexer (the status poll). A report made under an older epoch (the answer of the indexer used before a switch) is
   * ignored.
   */
  report(epoch: number, probe: Probe): void {
    if (this.disposed || epoch !== this.epochN) return;
    this.record(this.active, probe);
    this.afterActive(probe);
  }

  dispose(): void {
    this.disposed = true;
    this.clearAll();
    this.listeners.clear();
  }

  // ------------------------------------------------------------------------------------------ internals

  private verdict(i: number): Verdict {
    return judge(this.probes[i], { network: this.network, primary: i === 0 });
  }

  private makeState(): FailoverState {
    const active = this.active;
    // a member whose last probe showed it unfit (down, behind, on another network) would only raise false disagreements: left out until a probe shows it fit.
    // The primary is a reference only while it answers well; a member never probed (the primary is in use, the others have not been needed) is one.
    const verifiers: number[] = [];
    for (let i = 0; i < this.members.length; i++) {
      if (i === active) continue;
      if (i === 0 ? this.verdict(0).usable : !this.probes[i] || this.verdict(i).usable) verifiers.push(i);
    }
    const label = this.members[active]!.label;
    return {
      active, label, host: hostOf(label), onBackup: active > 0, historyPartial: active > 0 && this.verdict(active).history === 'partial', epoch: this.epochN, verifiers,
    };
  }

  private publish(): void {
    const next = this.makeState();
    const prev = this.snap;
    if (
      next.active === prev.active && next.epoch === prev.epoch && next.historyPartial === prev.historyPartial &&
      next.verifiers.length === prev.verifiers.length && next.verifiers.every((v, k) => v === prev.verifiers[k])
    ) {
      return;
    }
    this.snap = next;
    for (const cb of [...this.listeners]) {
      try {
        cb(next);
      } catch {
        /* a subscriber's bug must not stop the failover */
      }
    }
  }

  private record(i: number, probe: Probe): void {
    this.probes[i] = probe;
    if (probe.health && !probe.error && this.verdict(i).usable && i === this.active) this.everGood = true;
    this.publish();
  }

  private clearAll(): void {
    for (const k of ['recheckTimer', 'evalTimer', 'recoverTimer'] as const) {
      if (this[k] !== null) this.clearTimer(this[k]);
      this[k] = null;
    }
  }

  /** One health read of member `i`, ended by `PROBE_TIMEOUT_MS`; a failure is data. */
  private async probe(i: number): Promise<Probe> {
    const ctl = new AbortController();
    const timer = this.setTimer(() => ctl.abort(), PROBE_TIMEOUT_MS + 500);
    try {
      return { health: await this.members[i]!.api.health({ signal: ctl.signal, timeoutMs: PROBE_TIMEOUT_MS }), error: null };
    } catch (e) {
      return { health: null, error: toError(e) };
    } finally {
      this.clearTimer(timer);
    }
  }

  private afterActive(p: Probe): void {
    if (this.verdict(this.active).usable && p === this.probes[this.active]) {
      this.bad = 0;
      this.evalAttempts = 0;
      if (this.recheckTimer !== null) this.clearTimer(this.recheckTimer);
      this.recheckTimer = null;
      return;
    }
    this.bad++;
    // an indexer that never answered well is not given a second chance first: the page has nothing to show meanwhile
    if (this.everGood && this.bad <= RECHECKS) {
      if (this.recheckTimer === null) {
        const epoch = this.epochN;
        this.recheckTimer = this.setTimer(() => {
          this.recheckTimer = null;
          if (epoch !== this.epochN || this.disposed) return;
          void this.probe(this.active).then((q) => {
            if (epoch === this.epochN && !this.disposed) this.report(epoch, q);
          });
        }, RECHECK_BASE_MS * 2 ** (this.bad - 1));
      }
      return;
    }
    void this.evaluate();
  }

  /** Probes every member once and moves to the one `selectMember` names; with nothing better, tries again later (rate limited, backing off). */
  private async evaluate(): Promise<void> {
    if (this.evaluating || this.disposed) return;
    if (this.now() < this.nextEvalAt) return;
    this.evaluating = true;
    const epoch = this.epochN;
    try {
      const results = await Promise.all(this.members.map((_, i) => this.probe(i)));
      if (this.disposed || epoch !== this.epochN) return;
      results.forEach((r, i) => (this.probes[i] = r));
      const target = selectMember(this.members.map((_, i) => this.verdict(i)), this.active);
      if (target !== this.active) return this.switchTo(target);
      this.publish();
      if (this.verdict(this.active).usable) {
        this.evalAttempts = 0;
        return;
      }
      // nothing usable anywhere: look again later, less and less often
      this.evalAttempts++;
      const delay = backoff(this.evalAttempts);
      this.nextEvalAt = this.now() + delay;
      if (this.evalTimer !== null) this.clearTimer(this.evalTimer);
      this.evalTimer = this.setTimer(() => {
        this.evalTimer = null;
        void this.evaluate();
      }, delay);
    } finally {
      this.evaluating = false;
    }
  }

  private switchTo(i: number): void {
    this.clearAll();
    this.active = i;
    this.epochN++;
    this.bad = 0;
    this.evalAttempts = 0;
    this.nextEvalAt = 0;
    this.goodPrimary = 0;
    this.recoverFails = 0;
    this.everGood = this.verdict(i).usable;
    this.publish();
    if (i !== 0) this.scheduleRecover(backoff(0) * 3);
  }

  private scheduleRecover(ms: number): void {
    if (this.recoverTimer !== null) this.clearTimer(this.recoverTimer);
    const epoch = this.epochN;
    this.recoverTimer = this.setTimer(() => {
      this.recoverTimer = null;
      if (epoch !== this.epochN || this.disposed || this.active === 0) return;
      if (!this.visible()) return this.scheduleRecover(ms);
      // the primary, and the other members last seen unfit (they may be fit again: they are references only then)
      const others = this.members.map((_, i) => i).filter((i) => i !== this.active && i !== 0 && this.probes[i] && !this.verdict(i).usable);
      void Promise.all([this.probe(0), ...others.map((i) => this.probe(i))]).then(([p, ...rest]) => {
        if (epoch !== this.epochN || this.disposed) return;
        others.forEach((i, k) => (this.probes[i] = rest[k]!));
        this.record(0, p!);
        if (this.verdict(0).usable) {
          this.recoverFails = 0;
          if (++this.goodPrimary >= RECOVER_PROBES) return this.switchTo(0);
          return this.scheduleRecover(CONFIRM_MS);
        }
        this.goodPrimary = 0;
        this.scheduleRecover(backoff(++this.recoverFails + 2));
      });
    }, ms);
  }
}

/**
 * An `IndexerApi` that is always the ACTIVE member of `failover`: every call goes to one indexer (chosen when the call is made), so a single read never
 * mixes two. The object is stable; what it answers from changes with the failover state.
 */
export function failoverIndexer(failover: Pick<IndexerFailover, 'state'>, apis: readonly IndexerApi[]): IndexerApi {
  const active = (): IndexerApi => apis[failover.state.active]!;
  return new Proxy({} as IndexerApi, {
    get: (_t, prop) => {
      const a = active() as unknown as Record<string | symbol, unknown>;
      const v = a[prop];
      return typeof v === 'function' ? (v as (...x: unknown[]) => unknown).bind(a) : v;
    },
    has: (_t, prop) => prop in active(),
  });
}

/** The member `i` itself (not the proxy): for a build that must read every part from the same indexer. */
export function pinnedIndexer(failover: Pick<IndexerFailover, 'state'>, apis: readonly IndexerApi[]): IndexerApi {
  return apis[failover.state.active]!;
}
