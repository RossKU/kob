// P2PK-owned KCC-20 UTXOs of a wallet key. A token UTXO is a P2SH covenant output whose address depends on its whole state (amount, owner,
// ...), so the node cannot list "the tokens of a key": someone has to know the states. Sources, in order:
//   1. the indexer's proposed `GET /v1/token-utxos` (when it exists; a 404/501 degrades silently),
//   2. this LOCAL tracker: candidate outpoints learned from the app's own transactions (issuance outputs, token CHANGE outputs of
//      placements / cancels / sends: `trackFromBuilt`) and from manual import.
// Whatever the source, every candidate is VERIFIED against the node before it is offered for spending: its P2SH address is derived from
// the claimed state (kob-wasm `tokenScriptPublicKey`), the node must list the outpoint there, and the covenant id must match. An indexer or a
// storage entry can therefore be stale or wrong, never dangerous.
//
// The reverse also holds: the local list is the only record of holdings the indexer never saw (issuance outputs, anything from before
// the indexer's database started), so nothing here may forget a candidate on weak evidence. A candidate the node does not list is only
// pruned after MISS_CONFIRMATIONS misses on separate checks that span the grace period, an empty node answer counts for nothing, and a
// store this code cannot read is copied aside before it is overwritten.
import type { Hex, TokenProgram, TokenState, TokenUtxo, BuiltTx, U64 } from '../kob/types';
import { familyOfProgram, isKeyOwned, isKronState, keyState } from '../kob/token-state';
import type { KobWasm } from '../kob/wasm';
import type { NodeApi } from './node';
import type { IndexerApi } from './indexer';
import type { TokenUtxoView } from './indexer-types';
import { spkStringToAddress, normalizeNetwork, type KaspaSdk } from './kaspa-sdk';
import type { StorageLike } from '../config';
import { defaultStorage } from '../config';

export interface TokenRef {
  covenantId: Hex;
  program: TokenProgram;
  /** the token's extension commitment (registry): lets an indexer answer without a `state` be rebuilt into a full state */
  extensionCommitment?: Hex;
}

/** A remembered candidate: the outpoint plus the state its script commits to. */
export interface TrackedToken {
  transactionId: Hex;
  index: number;
  tokenCovId: Hex;
  program: TokenProgram;
  state: TokenState;
  /** KAS (sompi) carried by the output when it was created */
  carrier: U64;
  /** ms since the epoch: unverified candidates survive a grace period (a just-submitted tx is not visible to the node at once) */
  addedAt: number;
  /**
   * Misses: checks (spaced apart) at which the node did not list this outpoint. Persisted so that a candidate is pruned only after the
   * miss is confirmed, not on one wrong or partial node answer; all cleared when the candidate verifies again. Absent in older stores.
   */
  missCount?: number;
  firstMissAt?: number;
  lastMissAt?: number;
}

export interface TokenTrackerDeps {
  kob: KobWasm;
  node: NodeApi;
  sdk: KaspaSdk;
  network: string;
  /** default: localStorage when usable, else in memory */
  storage?: StorageLike | null;
  /** optional first source; only `tokenUtxos` is used */
  indexer?: Pick<IndexerApi, 'tokenUtxos'> | null;
  now?: () => number;
  /** how long an unverifiable candidate is kept before it is pruned (default 15 min) */
  pendingGraceMs?: number;
}

const STORE_VERSION = 1;
/** a candidate the node does not list is pruned only after this many misses (and once they span the grace period) */
const MISS_CONFIRMATIONS = 3;
const HEX32 = /^[0-9a-f]{64}$/;
const DEC = /^\d+$/;
const PROGRAMS: readonly string[] = ['KCC20Ref', 'KCC20Ref_4x5', 'KCC20Ref_8x8', 'KCC20Ref_16x16', 'KCC20P2', 'KCC20KaspaCom_0_2_5', 'KCC20PublicMint', 'KronToken2433', 'KronToken2732'];

const outpointKey = (txid: string, index: number) => `${txid}:${index}`;

class MemoryStorage implements StorageLike {
  private m = new Map<string, string>();
  getItem(k: string) {
    return this.m.get(k) ?? null;
  }
  setItem(k: string, v: string) {
    this.m.set(k, v);
  }
  removeItem(k: string) {
    this.m.delete(k);
  }
}

/** Validates an untrusted candidate (storage, import text); returns null if any field is malformed. */
export function sanitizeTracked(v: unknown, now = 0): TrackedToken | null {
  if (typeof v !== 'object' || v === null) return null;
  const o = v as Record<string, unknown>;
  const s = o.state as Record<string, unknown> | undefined;
  if (typeof o.transactionId !== 'string' || !HEX32.test(o.transactionId)) return null;
  if (typeof o.index !== 'number' || !Number.isInteger(o.index) || o.index < 0 || o.index > 0xffff) return null;
  if (typeof o.tokenCovId !== 'string' || !HEX32.test(o.tokenCovId)) return null;
  if (typeof o.program !== 'string' || !PROGRAMS.includes(o.program)) return null;
  if (!s || typeof s !== 'object') return null;
  if (typeof s.amount !== 'string' || !DEC.test(s.amount)) return null;
  if (typeof s.owner !== 'string' || !HEX32.test(s.owner)) return null;
  const program = o.program as TokenProgram;
  let state: TokenState;
  if (familyOfProgram(program) === 'kron') {
    // KRON layout (46 bytes): id_type + is_minter; a KCC-20 state under a KRON program (or the reverse) is malformed
    if (typeof s.id_type !== 'number' || typeof s.is_minter !== 'number' || 'owner_scheme' in s) return null;
    state = { amount: s.amount, owner: s.owner, id_type: s.id_type, is_minter: s.is_minter };
  } else {
    if (typeof s.owner_scheme !== 'number' || typeof s.borrow_scheme !== 'number') return null;
    if (typeof s.borrow_guard !== 'string' || !HEX32.test(s.borrow_guard)) return null;
    if (typeof s.extension_commitment !== 'string' || !HEX32.test(s.extension_commitment)) return null;
    state = { amount: s.amount, owner: s.owner, owner_scheme: s.owner_scheme, borrow_scheme: s.borrow_scheme, borrow_guard: s.borrow_guard, extension_commitment: s.extension_commitment };
  }
  if (typeof o.carrier !== 'string' || !DEC.test(o.carrier)) return null;
  const out: TrackedToken = {
    transactionId: o.transactionId,
    index: o.index,
    tokenCovId: o.tokenCovId,
    program,
    state,
    carrier: o.carrier,
    addedAt: typeof o.addedAt === 'number' && Number.isFinite(o.addedAt) ? o.addedAt : now,
  };
  // miss bookkeeping (optional: stores written before it load unchanged; malformed values are dropped, not fatal)
  if (typeof o.missCount === 'number' && Number.isInteger(o.missCount) && o.missCount > 0 && o.missCount < 1_000_000) out.missCount = o.missCount;
  if (typeof o.firstMissAt === 'number' && Number.isFinite(o.firstMissAt)) out.firstMissAt = o.firstMissAt;
  if (typeof o.lastMissAt === 'number' && Number.isFinite(o.lastMissAt)) out.lastMissAt = o.lastMissAt;
  return out;
}

export class TokenTracker {
  private readonly kob: KobWasm;
  private readonly node: NodeApi;
  private readonly sdk: KaspaSdk;
  private readonly network: string;
  private readonly storage: StorageLike;
  private readonly indexer: Pick<IndexerApi, 'tokenUtxos'> | null;
  private readonly now: () => number;
  private readonly graceMs: number;
  /** two misses count as separate checks only this far apart (also bounds how often a miss is written to the store) */
  private readonly missSpacingMs: number;

  constructor(d: TokenTrackerDeps) {
    this.kob = d.kob;
    this.node = d.node;
    this.sdk = d.sdk;
    this.network = normalizeNetwork(d.network);
    this.storage = (d.storage === undefined ? defaultStorage() : d.storage) ?? new MemoryStorage();
    this.indexer = d.indexer ?? null;
    this.now = d.now ?? Date.now;
    this.graceMs = d.pendingGraceMs ?? 15 * 60_000;
    this.missSpacingMs = Math.max(1000, Math.floor(this.graceMs / 10));
  }

  // ---------------------------------------------------------------------------------------------- store

  private key(pubkey: Hex): string {
    return `kob.tokens.v${STORE_VERSION}.${this.network}.${pubkey.toLowerCase()}`;
  }

  /** the stored candidates plus whether the raw value is something this code cannot represent in full (corrupt, foreign version, rejected items) */
  private read(pubkey: Hex): { items: TrackedToken[]; raw: string | null; damaged: boolean } {
    let raw: string | null = null;
    try {
      raw = this.storage.getItem(this.key(pubkey));
      if (!raw) return { items: [], raw: null, damaged: false };
      const parsed = JSON.parse(raw) as { v?: number; items?: unknown[] };
      if (parsed?.v !== STORE_VERSION || !Array.isArray(parsed.items)) return { items: [], raw, damaged: true };
      const items = parsed.items.map((i) => sanitizeTracked(i, this.now())).filter((t): t is TrackedToken => t !== null);
      return { items, raw, damaged: items.length !== parsed.items.length };
    } catch {
      return { items: [], raw, damaged: raw !== null };
    }
  }

  private load(pubkey: Hex): TrackedToken[] {
    return this.read(pubkey).items;
  }

  private save(pubkey: Hex, items: TrackedToken[]): void {
    try {
      // a stored value that load() could not take in full (corrupt JSON, another version, entries it rejected) would be silently replaced
      // by this write and its holdings lost: keep the raw value under a side key first, and do not write at all if that fails
      const cur = this.read(pubkey);
      if (cur.damaged && cur.raw !== null) this.storage.setItem(`${this.key(pubkey)}.unreadable.${this.now()}`, cur.raw);
      this.storage.setItem(this.key(pubkey), JSON.stringify({ v: STORE_VERSION, items }));
    } catch {
      /* storage full or blocked: tracking is best effort, the indexer / a re-import can rebuild it */
    }
  }

  /** Remembered candidates of a key (unverified). */
  list(pubkey: Hex): TrackedToken[] {
    return this.load(pubkey);
  }

  /** Remembers a candidate; returns false when it is malformed or already known. */
  add(pubkey: Hex, t: Omit<TrackedToken, 'addedAt'> & { addedAt?: number }): boolean {
    const c = sanitizeTracked({ ...t, addedAt: t.addedAt ?? this.now() }, this.now());
    if (!c) return false;
    const items = this.load(pubkey);
    if (items.some((i) => i.transactionId === c.transactionId && i.index === c.index)) return false;
    items.push(c);
    this.save(pubkey, items);
    return true;
  }

  remove(pubkey: Hex, transactionId: Hex, index: number): boolean {
    const items = this.load(pubkey);
    const rest = items.filter((i) => !(i.transactionId === transactionId && i.index === index));
    if (rest.length === items.length) return false;
    this.save(pubkey, rest);
    return true;
  }

  clear(pubkey: Hex): void {
    try {
      this.storage.removeItem?.(this.key(pubkey));
    } catch {
      /* ignore */
    }
    this.save(pubkey, []);
  }

  /**
   * Manual import (Settings): JSON of one candidate or an array of them, in the `TrackedToken` shape (`addedAt` optional).
   * Returns how many were new; throws on malformed input (nothing is imported then).
   */
  importJson(pubkey: Hex, text: string): number {
    let data: unknown;
    try {
      data = JSON.parse(text);
    } catch {
      throw new Error('The import is not valid JSON.');
    }
    const list = Array.isArray(data) ? data : [data];
    const parsed = list.map((x) => sanitizeTracked(x, this.now()));
    if (parsed.length === 0 || parsed.some((p) => p === null)) throw new Error('The import contains an entry that is not a valid token UTXO description.');
    let n = 0;
    for (const p of parsed as TrackedToken[]) if (this.add(pubkey, p)) n++;
    return n;
  }

  // ---------------------------------------------------------------------------------------------- learning from our own transactions

  /**
   * Records the token outputs of a built transaction that are owned by `maker` (KCC-20 P2PK scheme; KRON address presence): the token change of a
   * placement, cancel or send, the tokens returned by a replacement, ... Reads `built.plans[..].nextStates` (the states the leader authorises; KRON
   * token inputs all carry the same list, the first one of each token is read) and pairs each
   * with the transaction output whose script it derives to, so an output is only tracked if its script really commits to that state.
   * Call it when the transaction is submitted (the outputs then appear on chain; until then they are just "pending" candidates).
   * `txId` defaults to `built.tx.id`; pass the SIGNED transaction's id if finalizing could ever change it.
   */
  trackFromBuilt(kob: KobWasm, built: BuiltTx, maker: Hex, txId: Hex = built.tx.id): TrackedToken[] {
    const found: TrackedToken[] = [];
    const me = maker.toLowerCase();
    const seen = new Set<Hex>();
    built.plans.forEach((plan, inputIndex) => {
      if (plan.kind !== 'tokenLeader' && plan.kind !== 'kronToken') return;
      const tokenCovId = built.tx.inputs[inputIndex]?.utxo.covenantId;
      if (!tokenCovId) return;
      if (plan.kind === 'kronToken') {
        if (seen.has(tokenCovId)) return;
        seen.add(tokenCovId);
      }
      const expected = (plan.nextStates as TokenState[]).map((state) => ({ state, spk: kob.tokenScriptPublicKey(plan.template, state).toLowerCase(), used: false }));
      built.tx.outputs.forEach((out, index) => {
        if (out.covenant?.covenantId !== tokenCovId) return;
        const hit = expected.find((e) => !e.used && e.spk === out.scriptPublicKey.toLowerCase());
        if (!hit) return;
        hit.used = true;
        if (!isKeyOwned(hit.state) || hit.state.owner.toLowerCase() !== me) return;
        if (BigInt(hit.state.amount) === 0n) return;
        found.push({ transactionId: txId, index, tokenCovId, program: plan.template, state: hit.state, carrier: out.value, addedAt: this.now() });
      });
    });
    const items = this.load(me);
    let changed = false;
    for (const f of found) {
      if (!items.some((i) => i.transactionId === f.transactionId && i.index === f.index)) {
        items.push(f);
        changed = true;
      }
    }
    if (changed) this.save(me, items);
    return found;
  }

  // ---------------------------------------------------------------------------------------------- resolving

  private stateFromView(v: TokenUtxoView, token: TokenRef): TokenState | null {
    if (v.state) return v.state;
    const family = familyOfProgram(token.program);
    if (family === 'kcc20' && !token.extensionCommitment) return null;
    // a key-owned token UTXO (KCC-20: P2PK owner without borrow guard; KRON: address presence)
    return keyState(family, v.amount, v.owner, token.extensionCommitment ?? null);
  }

  /**
   * Candidates from the indexer; `answered` is false when there is none, it does not know the endpoint or it failed (the indexer is optional).
   * The indexer is the PRIMARY source; the local list only adds what the indexer cannot know (issuance outputs, transactions it has not indexed yet).
   */
  private async fromIndexer(pubkey: Hex, token: TokenRef): Promise<{ answered: boolean; items: TrackedToken[] }> {
    if (!this.indexer) return { answered: false, items: [] };
    let views: TokenUtxoView[] | null;
    try {
      views = await this.indexer.tokenUtxos({ owner: pubkey, token: token.covenantId, spent: false });
    } catch {
      return { answered: false, items: [] };
    }
    if (!views) return { answered: false, items: [] };
    const out: TrackedToken[] = [];
    for (const v of views) {
      if (v.spent || v.token.toLowerCase() !== token.covenantId) continue;
      const state = this.stateFromView(v, token);
      if (!state || state.owner.toLowerCase() !== pubkey.toLowerCase() || !isKeyOwned(state)) continue;
      // the state must be of this token's family (a KCC-20 state under a KRON program cannot derive a script)
      if (isKronState(state) !== (familyOfProgram(token.program) === 'kron')) continue;
      const program = v.program && PROGRAMS.includes(v.program) && familyOfProgram(v.program as TokenProgram) === familyOfProgram(token.program) ? (v.program as TokenProgram) : token.program;
      const c = sanitizeTracked({ transactionId: v.txid, index: v.index, tokenCovId: token.covenantId, program, state, carrier: v.value, addedAt: this.now() }, this.now());
      if (c) out.push(c);
    }
    return { answered: true, items: out };
  }

  /**
   * The spendable token UTXOs of `pubkey` for `token`, verified live on the node: (indexer candidates ∪ local candidates) whose derived
   * P2SH address actually holds the outpoint with the right covenant id. Largest amount first. A local candidate the node does not list is
   * NOT dropped on that evidence alone (a wrong, partial or empty answer must never wipe holdings): the miss is recorded and the candidate
   * is pruned once it is confirmed (see `settle`). One the node contradicts (the outpoint exists under another covenant id) is dropped at
   * once. A failed node call throws before anything is written.
   */
  async tokenUtxosFor(pubkey: Hex, token: TokenRef): Promise<TokenUtxo[]> {
    const me = pubkey.toLowerCase();
    const covId = token.covenantId.toLowerCase();
    const localForToken = this.load(me).filter((t) => t.tokenCovId === covId);
    const { items: fromIndexer } = await this.fromIndexer(me, { ...token, covenantId: covId });
    // an outpoint the indexer also lists stays in the local list: after an indexer reset (fresh database) such holdings are known to
    // neither source otherwise. They dedupe in the map below and are pruned like any other candidate once spent.

    const byOutpoint = new Map<string, TrackedToken>();
    for (const c of [...fromIndexer, ...localForToken]) byOutpoint.set(`${outpointKey(c.transactionId, c.index)}:${c.state.amount}`, c);
    const candidates = [...byOutpoint.values()];
    if (candidates.length === 0) return [];

    const withAddress = candidates.map((c) => {
      const spk = this.kob.tokenScriptPublicKey(c.program, c.state);
      return { c, spk: spk.toLowerCase(), address: spkStringToAddress(this.sdk, spk, this.network) };
    });
    const live = await this.node.getUtxosByAddresses([...new Set(withAddress.map((w) => w.address))]);
    const liveByOutpoint = new Map(live.map((u) => [outpointKey(u.transactionId, u.index), u]));

    const result = new Map<string, TokenUtxo>();
    const contradicted = new Set<string>();
    for (const { c, spk } of withAddress) {
      const k = outpointKey(c.transactionId, c.index);
      const u = liveByOutpoint.get(k);
      if (u && u.scriptPublicKey.toLowerCase() === spk) {
        if (u.covenantId === covId) {
          if (!result.has(k)) {
            result.set(k, { transactionId: u.transactionId, index: u.index, amount: u.amount, blockDaaScore: u.blockDaaScore, covenantId: covId, state: c.state });
          }
        } else contradicted.add(k); // the node says this outpoint is not a UTXO of this token: bad data
      }
    }
    this.settle(me, covId, { queried: new Set(localForToken.map((t) => outpointKey(t.transactionId, t.index))), verified: new Set(result.keys()), contradicted, answerEmpty: live.length === 0 });
    return [...result.values()].sort((a, b) => {
      const d = BigInt(b.state.amount) - BigInt(a.state.amount);
      if (d !== 0n) return d > 0n ? 1 : -1;
      return a.transactionId === b.transactionId ? a.index - b.index : a.transactionId < b.transactionId ? -1 : 1;
    });
  }

  /**
   * Applies what one node check learned to the stored candidates of a token. Works on a FRESH read of the store (the awaits of the check
   * can overlap an `add` / `trackFromBuilt`, whose write must not be reverted by a stale snapshot) and only on the outpoints the check
   * asked about.
   *  - verified: the miss record is cleared.
   *  - contradicted by the node (same outpoint, other covenant id): dropped at once, that is node confirmation.
   *  - an EMPTY node answer: no evidence either way ("all spent" looks the same as a broken answer), nothing changes.
   *  - otherwise a miss, recorded at most once per `missSpacingMs`; the candidate is dropped only when it is past the pending grace, has
   *    >= MISS_CONFIRMATIONS misses and the first of them is at least the grace period old (so a bad answer, or a short outage, never
   *    accumulates into a deletion).
   */
  private settle(me: Hex, covId: Hex, r: { queried: Set<string>; verified: Set<string>; contradicted: Set<string>; answerEmpty: boolean }): void {
    const nowMs = this.now();
    const items = this.load(me);
    const out: TrackedToken[] = [];
    let changed = false;
    for (const t of items) {
      const k = outpointKey(t.transactionId, t.index);
      if (t.tokenCovId !== covId || !r.queried.has(k)) {
        out.push(t);
        continue;
      }
      if (r.verified.has(k)) {
        if (t.missCount !== undefined || t.firstMissAt !== undefined || t.lastMissAt !== undefined) {
          const { missCount: _a, firstMissAt: _b, lastMissAt: _c, ...clean } = t;
          out.push(clean);
          changed = true;
        } else out.push(t);
        continue;
      }
      if (r.contradicted.has(k)) {
        changed = true;
        continue;
      }
      if (r.answerEmpty) {
        out.push(t);
        continue;
      }
      let n = t;
      if (t.lastMissAt === undefined || nowMs - t.lastMissAt >= this.missSpacingMs) {
        n = { ...t, missCount: (t.missCount ?? 0) + 1, firstMissAt: t.firstMissAt ?? nowMs, lastMissAt: nowMs };
        changed = true;
      }
      if (nowMs - t.addedAt > this.graceMs && (n.missCount ?? 0) >= MISS_CONFIRMATIONS && nowMs - (n.firstMissAt ?? nowMs) >= this.graceMs) continue; // confirmed gone
      out.push(n);
    }
    if (changed) this.save(me, out);
  }

  /** Total token base units across the verified UTXOs (bigint). */
  async balanceFor(pubkey: Hex, token: TokenRef): Promise<bigint> {
    return (await this.tokenUtxosFor(pubkey, token)).reduce((s, u) => s + BigInt(u.state.amount), 0n);
  }
}

