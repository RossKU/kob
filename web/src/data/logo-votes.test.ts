// The token-logo votes (data/logo-votes.ts): the display rule, the viewer's setting, the message and the work kobd checks, the
// signature a wallet returns, casting a vote and reading the totals.
import { schnorr } from '@noble/curves/secp256k1.js';
import { blake2b } from '@noble/hashes/blake2.js';
import { sha256 } from '@noble/hashes/sha2.js';
import { beforeEach, describe, expect, it, vi } from 'vitest';
import { readVoteFilterPref, resetIconPrefMemory, VOTE_FILTER_PREF_KEY, writeVoteFilterPref } from '../app/icon-pref';
import { castVote, createLogoVotes, hiddenForViewer, leadingZeroBits, logoVotesUrlOf, LOGO_VOTES_KEY, normalizeSignature, parseVotes, powOk, powPrefix, solvePow, utcDay, verdictOf, voteMessage } from './logo-votes';

const A = 'a'.repeat(64);
const B = 'b'.repeat(64);
const URL_ = 'https://idx.example/v1/logo-votes';

const mem = (initial: Record<string, string> = {}) => {
  const m = new Map(Object.entries(initial));
  return { m, getItem: (k: string) => m.get(k) ?? null, setItem: (k: string, v: string) => void m.set(k, v), removeItem: (k: string) => void m.delete(k) };
};
const json = (body: unknown, status = 200, headers: Record<string, string> = {}) => vi.fn(() => Promise.resolve(new Response(typeof body === 'string' ? body : JSON.stringify(body), { status, headers })));
const asFetch = (f: unknown) => f as unknown as typeof fetch;
const doc = (logos: { covenant_id: string; up: number; down: number }[]) => ({ logos, hidden: [], min_voters: 3, hide_at: -1, pow_bits: 8, interval_secs: 86_400, updated_ms: 1 });

describe('the display rule', () => {
  it('judges a logo only from 3 voters, then hides it at a net score of -1 or lower', () => {
    expect(verdictOf(undefined)).toBe('unjudged');
    expect(verdictOf({ up: 0, down: 2 })).toBe('unjudged');
    expect(verdictOf({ up: 1, down: 2 })).toBe('hidden');
    expect(verdictOf({ up: 0, down: 3 })).toBe('hidden');
    expect(verdictOf({ up: 2, down: 2 })).toBe('shown');
    expect(verdictOf({ up: 3, down: 0 })).toBe('shown');
    expect(verdictOf({ up: 4, down: 5 })).toBe('hidden');
  });

  it('follows the rule the indexer names', () => {
    expect(verdictOf({ up: 0, down: 2 }, { minVoters: 2, hideAt: -1 })).toBe('hidden');
    expect(verdictOf({ up: 1, down: 1 }, { minVoters: 2, hideAt: 0 })).toBe('hidden');
  });

  it('hides for the viewer only with the default setting and until the logo is shown once', () => {
    expect(hiddenForViewer('hidden', true, false)).toBe(true);
    expect(hiddenForViewer('hidden', false, false)).toBe(false);
    expect(hiddenForViewer('hidden', true, true)).toBe(false);
    expect(hiddenForViewer('shown', true, false)).toBe(false);
    expect(hiddenForViewer('unjudged', true, false)).toBe(false);
  });
});

describe('the viewer setting', () => {
  beforeEach(() => resetIconPrefMemory());

  it('is "Show logos at 0 and above" by default; "Show all logos" stores 0 under its key', () => {
    expect(readVoteFilterPref(mem())).toBe(true);
    const s = mem();
    expect(writeVoteFilterPref(false, s)).toBe(true);
    expect(s.m.get(VOTE_FILTER_PREF_KEY)).toBe('0');
    expect(readVoteFilterPref(s)).toBe(false);
    expect(writeVoteFilterPref(true, s)).toBe(true);
    expect(s.m.has(VOTE_FILTER_PREF_KEY)).toBe(false);
    expect(readVoteFilterPref(null)).toBe(true);
  });
});

describe('the vote kobd checks', () => {
  it('the message has one field per line, the token in lower case', () => {
    expect(voteMessage('testnet-10', A.toUpperCase(), 'down', 'kaspatest:qq', '2026-11-20')).toBe(`KOB logo vote\nToken: ${A}\nVote: down\nAddress: kaspatest:qq\nNetwork: testnet-10\nDay: 2026-11-20`);
  });

  it('the day is UTC', () => {
    expect(utcDay(Date.UTC(2026, 10, 20, 23, 59))).toBe('2026-11-20');
    expect(utcDay(Date.UTC(2026, 10, 21, 0, 0))).toBe('2026-11-21');
  });

  it('a signature of the Kaspa personal-message standard verifies over the keyed BLAKE2b hash (the KIP vector)', () => {
    // rusty-kaspa wallet/core/src/message.rs, test_sign_and_verify_test_case_0
    const key = 'f9308a019258c31049344f85f89d5229b531c845836f99b08601f113bce036f9';
    const sig = '40b9bb2be0ae02607279eda64015a8d86e3763279170340b8243f7ce5344d77aff1191598baf2fd26149cac3b4b12c2c433261c00834db6098cb172aa48ef522';
    const enc = new TextEncoder();
    const hash = (m: string) => blake2b(enc.encode(m), { key: enc.encode('PersonalMessageSigningHash'), dkLen: 32 });
    const hex = (h: string) => Uint8Array.from(h.match(/../g)!.map((b) => parseInt(b, 16)));
    expect(schnorr.verify(hex(sig), hash('Hello Kaspa!'), hex(key))).toBe(true);
    expect(schnorr.verify(hex(sig), hash('Hello Kaspa?'), hex(key))).toBe(false);
  });

  it('the work counts leading zero bits of the SHA-256 of the prefix and the nonce', async () => {
    expect(leadingZeroBits(Uint8Array.from([0, 0, 0x1f]))).toBe(19);
    expect(leadingZeroBits(Uint8Array.from([0x80]))).toBe(0);
    const prefix = powPrefix('testnet-10', A, 'kaspatest:qq', '2026-11-20');
    expect(prefix).toBe(`KOB logo vote pow/1\ntestnet-10\n${A}\nkaspatest:qq\n2026-11-20\n`);
    const progress: number[] = [];
    const nonce = await solvePow(prefix, 12, { slice: 500, onProgress: (n) => progress.push(n) });
    expect(powOk(prefix, nonce, 12)).toBe(true);
    expect(leadingZeroBits(sha256(new TextEncoder().encode(prefix + nonce)))).toBeGreaterThanOrEqual(12);
    expect(/^\d+$/.test(nonce)).toBe(true);
    const ctl = new AbortController();
    ctl.abort();
    await expect(solvePow(prefix, 30, { signal: ctl.signal })).rejects.toThrow();
  });

  it('reads the signature a wallet returns: hex, 0x hex, base64 or an object with it', () => {
    const hex = 'ab'.repeat(64);
    expect(normalizeSignature(hex.toUpperCase())).toBe(hex);
    expect(normalizeSignature(`0x${hex}`)).toBe(hex);
    expect(normalizeSignature({ signature: hex })).toBe(hex);
    expect(normalizeSignature({ signedMessage: hex })).toBe(hex);
    expect(normalizeSignature(btoa(String.fromCharCode(...new Uint8Array(64).fill(0xab))))).toBe(hex);
    expect(normalizeSignature('ab'.repeat(65))).toBeNull();
    expect(normalizeSignature(42)).toBeNull();
    expect(normalizeSignature(null)).toBeNull();
  });
});

describe('casting a vote', () => {
  const body = { covenant_id: A, vote: 'up' as const, address: 'kaspatest:qq', day: '2026-11-20', nonce: '7', signature: 'ab'.repeat(64) };

  it('posts JSON without credentials and reads the totals of the answer', async () => {
    const f = json({ result: 'counted', covenant_id: A, up: 3, down: 1 });
    expect(await castVote(URL_, body, { fetch: asFetch(f) })).toEqual({ ok: true, result: 'counted', up: 3, down: 1 });
    const [, init] = (f.mock.calls[0] as unknown as [string, RequestInit]);
    expect(init.method).toBe('POST');
    expect(init.credentials).toBe('omit');
    expect(JSON.parse(String(init.body))).toEqual(body);
  });

  it('tells the reasons apart: too soon, rate limited, no history, refused, no route, unreachable', async () => {
    const err = (code: string) => ({ error: { code } });
    expect(await castVote(URL_, body, { fetch: asFetch(json(err('too_soon'), 429, { 'retry-after': '7200' })) })).toEqual({ ok: false, reason: 'too-soon', retryAfterSecs: 7200 });
    expect(await castVote(URL_, body, { fetch: asFetch(json(err('rate_limited'), 429)) })).toEqual({ ok: false, reason: 'rate-limited' });
    expect(await castVote(URL_, body, { fetch: asFetch(json(err('no_history'), 403)) })).toEqual({ ok: false, reason: 'no-history' });
    expect(await castVote(URL_, body, { fetch: asFetch(json(err('bad_signature'), 400)) })).toEqual({ ok: false, reason: 'invalid' });
    expect(await castVote(URL_, body, { fetch: asFetch(json(err('unknown_token'), 404)) })).toEqual({ ok: false, reason: 'invalid' });
    expect(await castVote(URL_, body, { fetch: asFetch(json(err('not_found'), 404)) })).toEqual({ ok: false, reason: 'unavailable' });
    expect(await castVote(URL_, body, { fetch: asFetch(vi.fn(() => Promise.reject(new Error('down')))) })).toEqual({ ok: false, reason: 'network' });
  });
});

describe('the totals', () => {
  it('sit beside the blocklist; another path has none', () => {
    expect(logoVotesUrlOf('https://idx.example/v1/logo-blocklist')).toBe(URL_);
    expect(logoVotesUrlOf('https://l.example/list.json')).toBeNull();
  });

  it('parse only a well-formed answer', () => {
    expect(parseVotes(doc([{ covenant_id: A.toUpperCase(), up: 1, down: 2 }]))?.totals.get(A)).toEqual({ up: 1, down: 2 });
    expect(parseVotes({ logos: [{ covenant_id: 'x', up: 1, down: 2 }] })).toBeNull();
    expect(parseVotes({ logos: [{ covenant_id: A, up: -1, down: 2 }] })).toBeNull();
    expect(parseVotes({ hidden: [] })).toBeNull();
    expect(parseVotes([])).toBeNull();
    const d = parseVotes({ logos: [] })!;
    expect([d.minVoters, d.hideAt, d.powBits]).toEqual([3, -1, 20]);
  });

  it('are read, kept for the next visit, and fail open', async () => {
    const storage = mem();
    const v = createLogoVotes({ storage, fetch: asFetch(json(doc([{ covenant_id: A, up: 0, down: 3 }, { covenant_id: B, up: 2, down: 1 }]))) });
    expect(v.verdict(A)).toBe('unjudged');
    expect(await v.refresh(URL_)).toBe(true);
    expect(v.verdict(A)).toBe('hidden');
    expect(v.verdict(B)).toBe('shown');
    expect(v.powBits()).toBe(8);
    expect(storage.m.get(LOGO_VOTES_KEY)).toContain(A);
    // the next visit starts from the kept totals, and an unreachable indexer keeps them
    const next = createLogoVotes({ storage, fetch: asFetch(vi.fn(() => Promise.reject(new Error('down')))) });
    expect(next.verdict(A)).toBe('hidden');
    expect(await next.refresh(URL_)).toBe(false);
    expect(next.verdict(A)).toBe('hidden');
    // an odd answer changes nothing
    const odd = createLogoVotes({ storage: mem(), fetch: asFetch(json('<html>')) });
    expect(await odd.refresh(URL_)).toBe(false);
    expect(odd.verdict(A)).toBe('unjudged');
  });

  it('skip the reads while paused (standalone mode asks the indexer nothing)', async () => {
    vi.useFakeTimers();
    try {
      const f = json(doc([]));
      let paused = true;
      const v = createLogoVotes({ storage: mem(), fetch: asFetch(f), intervalMs: 1000 });
      const stop = v.start(URL_, { paused: () => paused });
      await vi.advanceTimersByTimeAsync(2500);
      expect(f).not.toHaveBeenCalled();
      paused = false;
      await vi.advanceTimersByTimeAsync(1000);
      expect(f).toHaveBeenCalledTimes(1);
      stop();
    } finally {
      vi.useRealTimers();
    }
  });

  it('show a hidden logo once, and take the viewer\'s own accepted vote at once', () => {
    const storage = mem({ [LOGO_VOTES_KEY]: JSON.stringify(doc([{ covenant_id: A, up: 0, down: 3 }])) });
    const v = createLogoVotes({ storage });
    let n = 0;
    v.subscribe(() => n++);
    expect(v.shownOnce(A)).toBe(false);
    v.showOnce(A.toUpperCase());
    expect(v.shownOnce(A)).toBe(true);
    expect(n).toBe(1);
    v.accepted(A, 'up', { up: 2, down: 3 });
    expect(v.totals(A)).toEqual({ up: 2, down: 3 });
    expect(v.ownVote(A)).toBe('up');
    expect(createLogoVotes({ storage }).ownVote(A)).toBe('up');
  });
});
