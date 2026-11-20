// Token-logo votes: what other wallets think of a token's logo, and this viewer's own vote
// (docs/design/logo-votes.md, docs/api-reference.md 3.19b).
//
// - A vote is a message the wallet signs (Kaspa's personal-message standard), no transaction and no
//   fee. Any wallet may vote, holder or not. The indexer checks the signature, a few seconds of
//   proof of work, the address's KOB history and one vote per wallet per 24 hours across all logos.
// - The indexer publishes the totals per token (`GET /v1/logo-votes`), never who voted. The list is
//   read at start and every 2 minutes, and the last good one is kept in localStorage.
// - The rule: a logo is judged once at least 3 addresses voted (`min_voters`). Then a net score
//   (up minus down) of -1 or lower (`hide_at`) hides it by default. The viewer can show every logo
//   (Settings), or show one hidden logo once.
// - It fails open like the report blocklist: no list, an odd one or no such route hides nothing.
// - Reports of illegal images are separate (data/logo-blocklist.ts). A vote never reports.
import { sha256 } from '@noble/hashes/sha2.js';
import { defaultStorage, type StorageLike } from '../config';

export const LOGO_VOTES_KEY = 'kob.logo-votes.v1';
/** How often the totals are read again while the app is open. */
export const LOGO_VOTES_REFRESH_MS = 120_000;
/** The rule's defaults, when the indexer's answer does not name them. */
export const MIN_VOTERS = 3;
export const HIDE_AT = -1;
const MAX_BODY = 2_000_000;
const HEX64 = /^[0-9a-f]{64}$/;

export type VoteDir = 'up' | 'down';

/** The totals of one token. */
export interface LogoTotals {
  up: number;
  down: number;
}

/** The indexer's answer, parsed. */
export interface VotesDoc {
  totals: Map<string, LogoTotals>;
  minVoters: number;
  hideAt: number;
  powBits: number;
  intervalSecs: number;
}

/** `<base>/v1/logo-votes` next to the blocklist's URL, or null when that has another shape. */
export function logoVotesUrlOf(blocklistUrl: string): string | null {
  return blocklistUrl.endsWith('/v1/logo-blocklist') ? `${blocklistUrl.slice(0, -'logo-blocklist'.length)}logo-votes` : null;
}

const count = (v: unknown): number | null => (typeof v === 'number' && Number.isSafeInteger(v) && v >= 0 ? v : null);

/** The totals of an answer, or null when it is not `{logos: [{covenant_id, up, down}], ...}`. */
export function parseVotes(doc: unknown): VotesDoc | null {
  if (!doc || typeof doc !== 'object' || Array.isArray(doc)) return null;
  const d = doc as Record<string, unknown>;
  if (!Array.isArray(d.logos) || d.logos.length > 200_000) return null;
  const totals = new Map<string, LogoTotals>();
  for (const row of d.logos) {
    if (!row || typeof row !== 'object') return null;
    const r = row as Record<string, unknown>;
    const id = typeof r.covenant_id === 'string' ? r.covenant_id.toLowerCase() : '';
    const up = count(r.up);
    const down = count(r.down);
    if (!HEX64.test(id) || up === null || down === null) return null;
    totals.set(id, { up, down });
  }
  const int = (v: unknown, dflt: number) => (typeof v === 'number' && Number.isSafeInteger(v) ? v : dflt);
  return {
    totals,
    minVoters: Math.max(1, int(d.min_voters, MIN_VOTERS)),
    hideAt: int(d.hide_at, HIDE_AT),
    powBits: Math.min(32, Math.max(0, int(d.pow_bits, 20))),
    intervalSecs: Math.max(0, int(d.interval_secs, 86_400)),
  };
}

/** What the rule says of a token's logo: not judged yet, shown, or hidden by default. */
export type VoteVerdict = 'unjudged' | 'shown' | 'hidden';

/** The display rule: judged from `minVoters` voters, then hidden at a net score of `hideAt` or lower. */
export function verdictOf(t: LogoTotals | undefined, rule: { minVoters: number; hideAt: number } = { minVoters: MIN_VOTERS, hideAt: HIDE_AT }): VoteVerdict {
  if (!t || t.up + t.down < rule.minVoters) return 'unjudged';
  return t.up - t.down <= rule.hideAt ? 'hidden' : 'shown';
}

/**
 * Whether a viewer sees the logo: hidden only when the rule hides it, the viewer keeps the default
 * ("Show logos at 0 and above") and has not shown this one once.
 */
export const hiddenForViewer = (verdict: VoteVerdict, hideVoted: boolean, shownOnce: boolean): boolean => verdict === 'hidden' && hideVoted && !shownOnce;

// --- The vote ---

/** The UTC day of `ms`, `YYYY-MM-DD`. */
export const utcDay = (ms: number): string => new Date(ms).toISOString().slice(0, 10);

/** The text the wallet signs, one field per line. kobd builds the same text to verify it. */
export function voteMessage(network: string, covenantId: string, vote: VoteDir, address: string, day: string): string {
  return `KOB logo vote\nToken: ${covenantId.toLowerCase()}\nVote: ${vote}\nAddress: ${address}\nNetwork: ${network}\nDay: ${day}`;
}

/** What the proof of work hashes, without the nonce. The vote's direction is not in it. */
export function powPrefix(network: string, covenantId: string, address: string, day: string): string {
  return `KOB logo vote pow/1\n${network}\n${covenantId.toLowerCase()}\n${address}\n${day}\n`;
}

/** The leading zero bits of `h`. */
export function leadingZeroBits(h: Uint8Array): number {
  let n = 0;
  for (const b of h) {
    if (b === 0) {
      n += 8;
      continue;
    }
    return n + Math.clz32(b) - 24;
  }
  return n;
}

const enc = new TextEncoder();

/** Whether `prefix + nonce` hashes (SHA-256) to at least `bits` leading zero bits. */
export const powOk = (prefix: string, nonce: string, bits: number): boolean => leadingZeroBits(sha256(enc.encode(prefix + nonce))) >= bits;

/**
 * Finds a nonce for `prefix` with `bits` of work. It hashes in slices of `slice` and yields to the
 * page between them, so the page stays responsive. It rejects with an AbortError when `signal`
 * aborts.
 */
export async function solvePow(prefix: string, bits: number, o: { signal?: AbortSignal; slice?: number; onProgress?: (tried: number) => void } = {}): Promise<string> {
  const base = sha256.create().update(enc.encode(prefix));
  const slice = o.slice ?? 20_000;
  for (let n = 0; ; ) {
    if (o.signal?.aborted) throw new DOMException('aborted', 'AbortError');
    const end = n + slice;
    for (; n < end; n++) {
      const s = String(n);
      if (leadingZeroBits(base.clone().update(enc.encode(s)).digest()) >= bits) return s;
    }
    o.onProgress?.(n);
    await new Promise((r) => setTimeout(r, 0));
  }
}

/** The 64-byte signature a wallet returned, as 128 lower-case hex, or null. It takes hex or base64. */
export function normalizeSignature(raw: unknown): string | null {
  const v = raw && typeof raw === 'object' ? ((raw as { signature?: unknown; signedMessage?: unknown }).signature ?? (raw as { signedMessage?: unknown }).signedMessage) : raw;
  if (typeof v !== 'string') return null;
  const s = v.trim().replace(/^0x/i, '');
  if (/^[0-9a-fA-F]{128}$/.test(s)) return s.toLowerCase();
  try {
    const bin = atob(s);
    if (bin.length !== 64) return null;
    return [...bin].map((c) => c.charCodeAt(0).toString(16).padStart(2, '0')).join('');
  } catch {
    return null;
  }
}

/** What casting a vote gave. */
export type VoteResult =
  | { ok: true; result: 'counted' | 'changed' | 'unchanged'; up: number; down: number }
  | { ok: false; reason: 'unavailable' | 'too-soon' | 'rate-limited' | 'no-history' | 'invalid' | 'network'; retryAfterSecs?: number };

/** The body of `POST /v1/logo-votes`. */
export interface VoteBody {
  covenant_id: string;
  vote: VoteDir;
  address: string;
  day: string;
  nonce: string;
  signature: string;
}

/** Sends a vote. It never rejects. */
export async function castVote(url: string, body: VoteBody, o: { fetch?: typeof fetch; timeoutMs?: number } = {}): Promise<VoteResult> {
  const fetchFn = o.fetch ?? ((...a: Parameters<typeof fetch>) => fetch(...a));
  const ctl = new AbortController();
  const timer = setTimeout(() => ctl.abort(), o.timeoutMs ?? 15_000);
  try {
    const res = await fetchFn(url, {
      method: 'POST',
      signal: ctl.signal,
      credentials: 'omit',
      referrerPolicy: 'no-referrer',
      headers: { 'content-type': 'application/json', accept: 'application/json' },
      body: JSON.stringify(body),
    });
    const wait = Number(res.headers.get('retry-after'));
    const retry = Number.isFinite(wait) && wait > 0 ? { retryAfterSecs: wait } : {};
    let doc: unknown = null;
    try {
      doc = await res.json();
    } catch {
      doc = null;
    }
    const code = doc && typeof doc === 'object' ? (doc as { error?: { code?: unknown } }).error?.code : undefined;
    if (res.status === 429) return { ok: false, reason: code === 'too_soon' ? 'too-soon' : 'rate-limited', ...retry };
    if (res.status === 404 && code !== 'unknown_token') return { ok: false, reason: 'unavailable' };
    if (res.status === 405) return { ok: false, reason: 'unavailable' };
    if (res.status === 403 && code === 'no_history') return { ok: false, reason: 'no-history' };
    if (res.status >= 400 && res.status < 500) return { ok: false, reason: 'invalid' };
    if (!res.ok) return { ok: false, reason: 'network' };
    const d = doc as { result?: unknown; up?: unknown; down?: unknown } | null;
    const result = d?.result;
    const up = count(d?.up);
    const down = count(d?.down);
    if ((result === 'counted' || result === 'changed' || result === 'unchanged') && up !== null && down !== null) return { ok: true, result, up, down };
    return { ok: false, reason: 'network' };
  } catch {
    return { ok: false, reason: 'network' };
  } finally {
    clearTimeout(timer);
  }
}

// --- The totals, kept current ---

/** Options of `createLogoVotes`. */
export interface LogoVotesOptions {
  fetch?: typeof fetch;
  storage?: StorageLike | null;
  intervalMs?: number;
  timeoutMs?: number;
}

/** The vote totals, kept current from the indexer. */
export interface LogoVotes {
  totals(covenantId: string): LogoTotals | undefined;
  verdict(covenantId: string): VoteVerdict;
  /** The work a vote needs now, in bits. */
  powBits(): number;
  /** Shows a hidden logo for the rest of this visit. */
  showOnce(covenantId: string): void;
  shownOnce(covenantId: string): boolean;
  /** This viewer's vote, remembered in this browser after it was accepted. */
  ownVote(covenantId: string): VoteDir | undefined;
  /** Takes an accepted vote: the token's totals from the answer and the viewer's own vote. */
  accepted(covenantId: string, vote: VoteDir, t: LogoTotals): void;
  subscribe(fn: () => void): () => void;
  /** Reads the totals now and every `intervalMs`, and returns the stop (one timer per URL). */
  start(url: string): () => void;
  refresh(url: string): Promise<boolean>;
  version(): number;
}

const OWN_KEY = 'kob.logo-votes.own';
const MAX_OWN = 2000;

/** Makes the totals, starting from what this browser kept. */
export function createLogoVotes(o: LogoVotesOptions = {}): LogoVotes {
  const fetchFn = o.fetch ?? ((...a: Parameters<typeof fetch>) => fetch(...a));
  const storage = o.storage === undefined ? defaultStorage() : o.storage;
  const interval = o.intervalMs ?? LOGO_VOTES_REFRESH_MS;
  let doc: VotesDoc = { totals: new Map(), minVoters: MIN_VOTERS, hideAt: HIDE_AT, powBits: 20, intervalSecs: 86_400 };
  let raw = '';
  const once = new Set<string>();
  let own = new Map<string, VoteDir>();
  let version = 0;
  const listeners = new Set<() => void>();
  const notify = () => {
    version++;
    for (const f of [...listeners]) {
      try {
        f();
      } catch {
        /* A listener must not stop the others. */
      }
    }
  };
  try {
    const kept = storage?.getItem(LOGO_VOTES_KEY);
    const parsed = kept ? parseVotes(JSON.parse(kept)) : null;
    if (parsed && kept) {
      doc = parsed;
      raw = kept;
    }
    const mine = storage?.getItem(OWN_KEY);
    const m: unknown = mine && mine.length < 200_000 ? JSON.parse(mine) : null;
    if (m && typeof m === 'object' && !Array.isArray(m)) {
      own = new Map(Object.entries(m as Record<string, unknown>).filter((e): e is [string, VoteDir] => HEX64.test(e[0]) && (e[1] === 'up' || e[1] === 'down')));
    }
  } catch {
    /* An unreadable store is an empty one. */
  }
  const keep = (key: string, value: string) => {
    try {
      storage?.setItem(key, value);
    } catch {
      /* Storage is full or blocked. This visit still has it. */
    }
  };

  async function refresh(url: string): Promise<boolean> {
    if (url === '') return false;
    const ctl = new AbortController();
    const timer = setTimeout(() => ctl.abort(), o.timeoutMs ?? 10_000);
    try {
      const res = await fetchFn(url, { signal: ctl.signal, credentials: 'omit', referrerPolicy: 'no-referrer', headers: { accept: 'application/json' } });
      if (!res.ok) return false;
      const text = await res.text();
      if (text.length > MAX_BODY) return false;
      const next = parseVotes(JSON.parse(text));
      if (!next) return false;
      if (text !== raw) {
        doc = next;
        raw = text;
        keep(LOGO_VOTES_KEY, text);
        notify();
      }
      return true;
    } catch {
      return false;
    } finally {
      clearTimeout(timer);
    }
  }

  const running = new Map<string, { stop: () => void; users: number }>();
  const id = (c: string) => c.toLowerCase();
  return {
    totals: (c) => doc.totals.get(id(c)),
    verdict: (c) => verdictOf(doc.totals.get(id(c)), doc),
    powBits: () => doc.powBits,
    showOnce(c) {
      if (once.has(id(c))) return;
      once.add(id(c));
      notify();
    },
    shownOnce: (c) => once.has(id(c)),
    ownVote: (c) => own.get(id(c)),
    accepted(c, vote, t) {
      const k = id(c);
      if (!HEX64.test(k)) return;
      own = new Map(own).set(k, vote);
      keep(OWN_KEY, JSON.stringify(Object.fromEntries([...own].slice(-MAX_OWN))));
      const totals = new Map(doc.totals).set(k, { up: t.up, down: t.down });
      doc = { ...doc, totals };
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

/** The app's one vote list. */
export const logoVotes: LogoVotes = createLogoVotes();
