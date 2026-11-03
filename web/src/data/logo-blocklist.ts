// The token-logo blocklist: which tokens' logos this app does not load, and the report that adds one (founder decision 2026-11-03).
//
// KOB keeps no third-party image. A logo that should not be shown is taken down by a REPORT: the first report of a token hides its logo for everybody
// (the letter avatar shows instead) until the operator of the indexer reviews it and restores it. The indexer (`GET /v1/logo-blocklist`, `POST
// /v1/logo-reports`; docs/api.md 3.19 and 3.20) is the single source of truth; the GitHub Issue form feeds the same list.
//
//   * The list is read at start and every 2 minutes and the last good one is kept in localStorage (so the first paint after a reload already hides what
//     the last visit knew). The list FAILS OPEN: when the indexer cannot be reached, answers anything but a well-formed list, or has no such route, the
//     last known list stays (empty on a first visit) and logos show. It never hides everything because it could not be read.
//   * A token the viewer has just reported is hidden for this viewer at once, whatever the answer (a recorded report during the operator's cooldown
//     hides nothing for anyone else, but this viewer has said what they think of the logo).
import { defaultStorage, type StorageLike } from '../config';

export const LOGO_BLOCKLIST_KEY = 'kob.logo-blocklist.v1';
/** How often the list is read again while the app is open. */
export const LOGO_BLOCKLIST_REFRESH_MS = 120_000;
const MAX_BODY = 1_000_000;
const HEX64 = /^[0-9a-f]{64}$/;

export type LogoReason = 'illegal' | 'impersonation' | 'other';
export const LOGO_REASONS: readonly LogoReason[] = ['illegal', 'impersonation', 'other'];

/** The indexer's list URL: the configured one, else `<indexer base>/v1/logo-blocklist`; '' when there is neither. */
export function logoBlocklistUrlOf(c: { logoBlocklistUrl: string; indexerUrl: string }): string {
  if (c.logoBlocklistUrl) return c.logoBlocklistUrl;
  const base = c.indexerUrl.replace(/\/+$/, '');
  return /^https?:\/\//i.test(base) ? `${base}/v1/logo-blocklist` : '';
}

/** Where a report is posted: the list's URL with its last segment `logo-reports`; null when the list's URL has another shape. */
export function logoReportsUrlOf(blocklistUrl: string): string | null {
  return blocklistUrl.endsWith('/v1/logo-blocklist') ? `${blocklistUrl.slice(0, -'logo-blocklist'.length)}logo-reports` : null;
}

/** The hidden covenant ids of a list document, or null when it is not `{hidden: [64-hex ids]}`. */
export function parseBlocklist(doc: unknown): Set<string> | null {
  if (!doc || typeof doc !== 'object' || Array.isArray(doc)) return null;
  const hidden = (doc as { hidden?: unknown }).hidden;
  if (!Array.isArray(hidden) || hidden.length > 100_000) return null;
  const out = new Set<string>();
  for (const id of hidden) {
    if (typeof id !== 'string' || !HEX64.test(id.toLowerCase())) return null;
    out.add(id.toLowerCase());
  }
  return out;
}

export interface BlocklistOptions {
  fetch?: typeof fetch;
  storage?: StorageLike | null;
  intervalMs?: number;
  timeoutMs?: number;
}

export interface LogoBlocklist {
  isHidden(covenantId: string): boolean;
  /** Hides `covenantId` for this viewer now (after their own report). */
  hideLocally(covenantId: string): void;
  subscribe(fn: () => void): () => void;
  /** Reads the list now and then every `intervalMs`; returns the stop. Starting again with the same URL is a no-op (one timer per URL). */
  start(url: string): () => void;
  /** One read; false when the list was not read (the last known list stays). */
  refresh(url: string): Promise<boolean>;
  /** A change counter (for hooks). */
  version(): number;
}

export function createLogoBlocklist(o: BlocklistOptions = {}): LogoBlocklist {
  const fetchFn = o.fetch ?? ((...a: Parameters<typeof fetch>) => fetch(...a));
  const storage = o.storage === undefined ? defaultStorage() : o.storage;
  const interval = o.intervalMs ?? LOGO_BLOCKLIST_REFRESH_MS;
  let hidden = new Set<string>();
  let local = new Set<string>();
  let version = 0;
  const listeners = new Set<() => void>();
  const notify = () => {
    version++;
    for (const f of [...listeners]) {
      try {
        f();
      } catch {
        /* a listener must not stop the others */
      }
    }
  };
  // the last list this browser read: the first paint hides what the last visit knew
  try {
    const raw = storage?.getItem(LOGO_BLOCKLIST_KEY);
    const kept = raw ? parseBlocklist(JSON.parse(raw)) : null;
    if (kept) hidden = kept;
  } catch {
    /* an unreadable store is an empty one */
  }
  const keep = () => {
    try {
      storage?.setItem(LOGO_BLOCKLIST_KEY, JSON.stringify({ hidden: [...hidden].sort() }));
    } catch {
      /* storage full or blocked: this visit still has the list */
    }
  };
  const same = (a: Set<string>, b: Set<string>) => a.size === b.size && [...a].every((x) => b.has(x));

  async function refresh(url: string): Promise<boolean> {
    if (url === '') return false;
    const ctl = new AbortController();
    const timer = setTimeout(() => ctl.abort(), o.timeoutMs ?? 10_000);
    try {
      const res = await fetchFn(url, { signal: ctl.signal, credentials: 'omit', referrerPolicy: 'no-referrer', headers: { accept: 'application/json' } });
      if (!res.ok) return false;
      const text = await res.text();
      if (text.length > MAX_BODY) return false;
      const next = parseBlocklist(JSON.parse(text));
      if (!next) return false;
      if (!same(next, hidden)) {
        hidden = next;
        keep();
        notify();
      }
      return true;
    } catch {
      // fails open: unreachable, not JSON, or no such route on this indexer
      return false;
    } finally {
      clearTimeout(timer);
    }
  }

  const running = new Map<string, { stop: () => void; users: number }>();
  return {
    isHidden: (id) => hidden.has(id.toLowerCase()) || local.has(id.toLowerCase()),
    hideLocally(id) {
      const k = id.toLowerCase();
      if (!HEX64.test(k) || local.has(k)) return;
      local = new Set(local).add(k);
      notify();
    },
    subscribe(fn) {
      listeners.add(fn);
      return () => void listeners.delete(fn);
    },
    start(url) {
      if (url === '') return () => undefined;
      let entry = running.get(url);
      if (!entry) {
        let timer: ReturnType<typeof setTimeout> | undefined;
        let live = true;
        const tick = async () => {
          await refresh(url);
          if (live) timer = setTimeout(() => void tick(), interval);
        };
        void tick();
        entry = {
          users: 0,
          stop: () => {
            live = false;
            if (timer !== undefined) clearTimeout(timer);
          },
        };
        running.set(url, entry);
      }
      entry.users++;
      const e = entry;
      return () => {
        if (--e.users <= 0) {
          e.stop();
          running.delete(url);
        }
      };
    },
    refresh,
    version: () => version,
  };
}

/** The app's one blocklist. */
export const logoBlocklist: LogoBlocklist = createLogoBlocklist();

// ------------------------------------------------------------------------------------------------ the report

export type ReportResult =
  | { ok: true; result: 'hidden' | 'recorded' }
  | { ok: false; reason: 'unavailable' | 'rate-limited' | 'invalid' | 'network'; retryAfterSecs?: number };

/** Files a report with the indexer. Never rejects. `hidden`: the logo is hidden for everybody now; `recorded`: kept for the operator to review. */
export async function reportLogo(reportsUrl: string, covenantId: string, reason: LogoReason, o: { fetch?: typeof fetch; timeoutMs?: number } = {}): Promise<ReportResult> {
  const fetchFn = o.fetch ?? ((...a: Parameters<typeof fetch>) => fetch(...a));
  const ctl = new AbortController();
  const timer = setTimeout(() => ctl.abort(), o.timeoutMs ?? 10_000);
  try {
    const res = await fetchFn(reportsUrl, {
      method: 'POST',
      signal: ctl.signal,
      credentials: 'omit',
      referrerPolicy: 'no-referrer',
      headers: { 'content-type': 'application/json', accept: 'application/json' },
      body: JSON.stringify({ covenant_id: covenantId.toLowerCase(), reason }),
    });
    if (res.status === 429) {
      const wait = Number(res.headers.get('retry-after'));
      return { ok: false, reason: 'rate-limited', ...(Number.isFinite(wait) && wait > 0 ? { retryAfterSecs: wait } : {}) };
    }
    if (res.status === 404 || res.status === 405) return { ok: false, reason: 'unavailable' };
    if (res.status === 400) return { ok: false, reason: 'invalid' };
    if (!res.ok) return { ok: false, reason: 'network' };
    const doc: unknown = await res.json();
    const result = doc && typeof doc === 'object' ? (doc as { result?: unknown }).result : undefined;
    return result === 'hidden' || result === 'recorded' ? { ok: true, result } : { ok: false, reason: 'network' };
  } catch {
    return { ok: false, reason: 'network' };
  } finally {
    clearTimeout(timer);
  }
}

/** The pre-filled GitHub Issue form for a report (the repository's `logo-report.yml`): the covenant id is a query parameter of the form's field. */
export function githubReportUrl(covenantId: string, repo = 'RossKU/kob'): string {
  return `https://github.com/${repo}/issues/new?template=logo-report.yml&covenant_id=${encodeURIComponent(covenantId.toLowerCase())}`;
}
