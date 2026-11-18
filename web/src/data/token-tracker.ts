// Key-owned token UTXOs of a wallet key, KCC-20 and KRON.
//
// A token UTXO is a P2SH covenant output whose address depends on its whole state, such as the
// amount and the owner. So the node cannot list "the tokens of a key", and someone has to know the
// states. The sources, in order:
// 1. The indexer's `GET /v1/token-utxos`. An indexer without the route is skipped silently.
// 2. This local tracker. It learns candidate outpoints from the app's own transactions
//    (`trackFromBuilt`) and from manual import. Those are issuance outputs and the token change
//    outputs of placements, cancels and sends.
// Whatever the source, every candidate is verified against the node before it is offered for
// spending. Its P2SH address is derived from the claimed state (kob-wasm `tokenScriptPublicKey`),
// the node must list the outpoint there, and the covenant id must match. An indexer or a storage
// entry can therefore be stale or wrong, but never dangerous.
//
// The indexer's last answer is kept in memory for the session. While the indexer cannot be read,
// because it is down or in standalone mode, its candidates are the ones it gave last. They are
// verified on the node like any other. One that was spent since is not listed there, so it is not
// offered, and it is forgotten. A wallet can thus still sell what it holds while the app trades
// through the node.
//
// Balances (`heldTokens`) read every token the wallet holds, not only the registry's. The indexer
// tracks the holdings of every token of a pinned program, such as a token received from another
// trading app, a launchpad or a bridge. Each candidate is verified the same way. The script is
// derived from the program kob-wasm pins, so a look-alike program or an output whose state the
// indexer misreports never verifies.
//
// The reverse also holds. The local list is the only record of holdings the indexer never saw,
// such as issuance outputs and anything from before the indexer's database started. So nothing
// here may forget a candidate on weak evidence.
// - A candidate the node does not list is pruned only after MISS_CONFIRMATIONS misses on separate
//   checks that span the grace period.
// - An empty node answer counts for nothing.
// - A store this code cannot read is copied aside before it is overwritten.
import type { Hex, TokenProgram, TokenState, TokenUtxo, BuiltTx, U64 } from '../kob/types';
import { TOKEN_PROGRAMS } from '../kob/programs.generated';
import { familyOfProgram, isKeyOwned, isKronState, isKronUnspendable, keyState } from '../kob/token-state';
import type { KobWasm } from '../kob/wasm';
import type { NodeApi } from './node';
import type { IndexerApi } from './indexer';
import type { TokenUtxoView } from './indexer-types';
import { spkStringToAddress, normalizeNetwork, type KaspaSdk } from './kaspa-sdk';
import type { StorageLike } from '../config';
import { defaultStorage } from '../config';

/** A token to look up: its covenant id and program. */
export interface TokenRef {
  covenantId: Hex;
  program: TokenProgram;
  /**
   * The token's extension commitment from the registry. It lets an indexer answer without a
   * `state` be rebuilt into a full state.
   */
  extensionCommitment?: Hex;
}

/**
 * A token UTXO of any token, verified on the node, with the program its script was derived under.
 * Balances use it.
 */
export interface HeldTokenUtxo extends TokenUtxo {
  covenantId: Hex;
  program: TokenProgram;
}

/** A remembered candidate: the outpoint plus the state its script commits to. */
export interface TrackedToken {
  transactionId: Hex;
  index: number;
  tokenCovId: Hex;
  program: TokenProgram;
  state: TokenState;
  /** KAS (sompi) carried by the output when it was created. */
  carrier: U64;
  /**
   * When it was added, in ms since the epoch. Unverified candidates survive a grace period,
   * because the node does not see a just-submitted transaction at once.
   */
  addedAt: number;
  /**
   * Misses are checks, spaced apart, at which the node did not list this outpoint. They are stored
   * so that a candidate is pruned only after the miss is confirmed, not on one wrong or partial
   * node answer. All are cleared when the candidate verifies again. Older stores lack them.
   */
  missCount?: number;
  firstMissAt?: number;
  lastMissAt?: number;
}

/** What `TokenTracker` needs. */
export interface TokenTrackerDeps {
  kob: KobWasm;
  node: NodeApi;
  sdk: KaspaSdk;
  network: string;
  /** Defaults to localStorage when usable, else memory. */
  storage?: StorageLike | null;
  /** The optional first source. Only `tokenUtxos` is used. */
  indexer?: Pick<IndexerApi, 'tokenUtxos'> | null;
  now?: () => number;
  /** How long an unverifiable candidate is kept before it is pruned (default 15 min). */
  pendingGraceMs?: number;
}

const STORE_VERSION = 1;
/**
 * Candidates asked of the indexer per page for a token's public lookup (`publicTokenUtxos`), two
 * pages at most.
 */
const PUBLIC_PAGE = 25;
/**
 * A candidate the node does not list is pruned only after this many misses that span the grace
 * period.
 */
const MISS_CONFIRMATIONS = 3;
const HEX32 = /^[0-9a-f]{64}$/;
const DEC = /^\d+$/;
const PROGRAMS: readonly string[] = TOKEN_PROGRAMS.map((p) => p.name);

const outpointKey = (txid: string, index: number) => `${txid}:${index}`;
/** Largest amount first, then by outpoint. */
const byAmountDesc = (a: TokenUtxo, b: TokenUtxo): number => {
  const d = BigInt(b.state.amount) - BigInt(a.state.amount);
  if (d !== 0n) return d > 0n ? 1 : -1;
  return a.transactionId === b.transactionId ? a.index - b.index : a.transactionId < b.transactionId ? -1 : 1;
};

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

/**
 * Validates an untrusted candidate from storage or import text. Returns null if any field is
 * malformed.
 */
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
    // The KRON layout (46 bytes) has id_type and is_minter. A KCC-20 state under a KRON program,
    // or the reverse, is malformed.
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
  // The miss bookkeeping is optional, so stores without it load unchanged. Malformed values are
  // dropped, not fatal.
  if (typeof o.missCount === 'number' && Number.isInteger(o.missCount) && o.missCount > 0 && o.missCount < 1_000_000) out.missCount = o.missCount;
  if (typeof o.firstMissAt === 'number' && Number.isFinite(o.firstMissAt)) out.firstMissAt = o.firstMissAt;
  if (typeof o.lastMissAt === 'number' && Number.isFinite(o.lastMissAt)) out.lastMissAt = o.lastMissAt;
  return out;
}

/** Finds, remembers and verifies the token UTXOs a wallet key holds. */
export class TokenTracker {
  private readonly kob: KobWasm;
  private readonly node: NodeApi;
  private readonly sdk: KaspaSdk;
  private readonly network: string;
  private readonly storage: StorageLike;
  private readonly indexer: Pick<IndexerApi, 'tokenUtxos'> | null;
  private readonly now: () => number;
  private readonly graceMs: number;
  /**
   * Two misses count as separate checks only this far apart. It also bounds how often a miss is
   * written to the store.
   */
  private readonly missSpacingMs: number;
  /**
   * The indexer's last candidates per owner and token, with `*` for every token of the owner. They
   * serve reads while it cannot answer.
   */
  private readonly lastFromIndexer = new Map<string, TrackedToken[]>();

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

  // --- Store ---

  private key(pubkey: Hex): string {
    return `kob.tokens.v${STORE_VERSION}.${this.network}.${pubkey.toLowerCase()}`;
  }

  /**
   * The stored candidates, and whether the raw value is something this code cannot represent in
   * full. That is corrupt JSON, a foreign version or rejected items.
   */
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
      // A stored value that load() could not take in full would be silently replaced by this
      // write, and its holdings lost. That is corrupt JSON, another version or entries it
      // rejected. Keep the raw value under a side key first, and do not write at all if that fails.
      const cur = this.read(pubkey);
      if (cur.damaged && cur.raw !== null) this.storage.setItem(`${this.key(pubkey)}.unreadable.${this.now()}`, cur.raw);
      this.storage.setItem(this.key(pubkey), JSON.stringify({ v: STORE_VERSION, items }));
    } catch {
      /* Storage is full or blocked. The indexer or a re-import can rebuild the list. */
    }
  }

  /** The remembered candidates of a key, unverified. */
  list(pubkey: Hex): TrackedToken[] {
    return this.load(pubkey);
  }

  /** Remembers a candidate. Returns false when it is malformed or already known. */
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
   * Imports candidates by hand from Settings. The text is JSON of one candidate or an array of
   * them, in the `TrackedToken` shape with `addedAt` optional. Returns how many were new. It
   * throws on malformed input, and nothing is imported then.
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

  // --- Learning from our own transactions ---

  /**
   * Records the token outputs of a built transaction that `maker` owns, by the KCC-20 P2PK scheme
   * or by KRON address presence.
   *
   * Examples are the token change of a placement, cancel or send, and the tokens returned by a
   * replacement. It reads `built.plans[..].nextStates`, the states the leader authorises. KRON
   * token inputs all carry the same list, so the first one of each token is read. Each state is
   * paired with the transaction output whose script it derives to, so an output is tracked only if
   * its script really commits to that state.
   *
   * Call it when the transaction is submitted. The outputs then appear on chain, and until then
   * they are only pending candidates. `txId` defaults to `built.tx.id`. Pass the signed
   * transaction's id if finalizing could ever change it.
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
        if (!isKeyOwned(hit.state, plan.template) || hit.state.owner.toLowerCase() !== me) return;
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

  // --- Recovering an issuance ---

  /**
   * Finds the issuance outputs of a token created before the genesis named its states in its
   * payload (docs/spec/token-metadata.md, 3).
   *
   * Such an output is a P2SH output of a state nobody but its issuer ever saw. So it is found the
   * one way left, by the issuer saying what the state is. Its owner is `pubkey` and its amount is
   * one of `amounts`, the base units the issuer gave the key at creation. The rest is the standard
   * issuance, with the extension commitment `token.extensionCommitment` (else zero) and no
   * borrowing. Without `token.program`, every KCC-20 program is tried.
   *
   * For each candidate the node is asked for the UTXOs at the script it derives. Whatever it lists
   * there under the token's covenant id is the output, because the script commits to the whole
   * state. It is remembered like any tracked output. Returns the outputs found, remembered now or
   * before. A failed node call rejects and changes nothing.
   */
  async recoverIssued(pubkey: Hex, token: { covenantId: Hex; program?: TokenProgram | null; extensionCommitment?: Hex | null }, amounts: readonly (bigint | string)[]): Promise<TrackedToken[]> {
    const me = pubkey.toLowerCase();
    const covId = token.covenantId.toLowerCase();
    const programs = token.program ? [token.program] : PROGRAMS.filter((p) => familyOfProgram(p as TokenProgram) === 'kcc20').map((p) => p as TokenProgram);
    const wanted = [...new Set(amounts.map((a) => BigInt(a)).filter((a) => a > 0n).map(String))];
    const claims: { program: TokenProgram; state: TokenState; spk: string; address: string }[] = [];
    for (const program of programs) {
      for (const amount of wanted) {
        const state = keyState(familyOfProgram(program), amount, me, token.extensionCommitment ?? null, program);
        try {
          const spk = this.kob.tokenScriptPublicKey(program, state).toLowerCase();
          claims.push({ program, state, spk, address: spkStringToAddress(this.sdk, spk, this.network) });
        } catch {
          /* A program that cannot encode the state has no such output. */
        }
      }
    }
    if (claims.length === 0) return [];
    const live = await this.node.getUtxosByAddresses([...new Set(claims.map((c) => c.address))]);
    const found: TrackedToken[] = [];
    for (const u of live) {
      if (u.covenantId !== covId) continue;
      const claim = claims.find((c) => c.spk === u.scriptPublicKey.toLowerCase());
      if (!claim) continue;
      const c = sanitizeTracked({ transactionId: u.transactionId, index: u.index, tokenCovId: covId, program: claim.program, state: claim.state, carrier: u.amount, addedAt: this.now() }, this.now());
      if (!c) continue;
      this.add(me, c);
      found.push(this.load(me).find((i) => i.transactionId === c.transactionId && i.index === c.index) ?? c);
    }
    return found;
  }

  // --- Resolving ---

  private stateFromView(v: TokenUtxoView, token: TokenRef): TokenState | null {
    if (v.state) return v.state;
    const family = familyOfProgram(token.program);
    if (family === 'kcc20' && !token.extensionCommitment) return null;
    // A key-owned token UTXO: for KCC-20 a P2PK owner without borrow guard, for KRON the program's
    // holder owner. This stand-in is plain. A holding with a borrow setting, which is spendable on
    // the owner path, needs the state the indexer proved (`v.state`, used above), since its script
    // commits to the scheme and guard. Without it the node check below finds no output at the
    // stand-in's script, and the candidate is dropped, never spent under a wrong state.
    return keyState(family, v.amount, v.owner, token.extensionCommitment ?? null, token.program);
  }

  /**
   * Candidates from the indexer. `answered` is false when there is no indexer, when it does not
   * know the endpoint and when it failed, because the indexer is optional. The indexer is the
   * primary source. The local list only adds what the indexer cannot know, such as issuance outputs
   * and transactions it has not indexed yet.
   */
  private async fromIndexer(pubkey: Hex, token: TokenRef): Promise<{ answered: boolean; items: TrackedToken[] }> {
    if (!this.indexer) return { answered: false, items: [] };
    const key = `${pubkey}|${token.covenantId}`;
    let views: TokenUtxoView[] | null;
    try {
      views = await this.indexer.tokenUtxos({ owner: pubkey, token: token.covenantId, spent: false });
    } catch {
      // The indexer cannot be read, so use what it gave last. It is verified on the node below like
      // any candidate.
      return { answered: false, items: this.lastFromIndexer.get(key) ?? [] };
    }
    if (!views) return { answered: false, items: [] };
    const out: TrackedToken[] = [];
    for (const v of views) {
      if (v.spent || v.token.toLowerCase() !== token.covenantId) continue;
      const state = this.stateFromView(v, token);
      if (!state || state.owner.toLowerCase() !== pubkey.toLowerCase() || !isKeyOwned(state, token.program)) continue;
      // The state must be of this token's family. A KCC-20 state under a KRON program cannot
      // derive a script.
      if (isKronState(state) !== (familyOfProgram(token.program) === 'kron')) continue;
      const program = v.program && PROGRAMS.includes(v.program) && familyOfProgram(v.program as TokenProgram) === familyOfProgram(token.program) ? (v.program as TokenProgram) : token.program;
      const c = sanitizeTracked({ transactionId: v.txid, index: v.index, tokenCovId: token.covenantId, program, state, carrier: v.value, addedAt: this.now() }, this.now());
      if (c) out.push(c);
    }
    this.remember(key, out);
    return { answered: true, items: out };
  }

  private remember(key: string, items: TrackedToken[]): void {
    this.lastFromIndexer.delete(key);
    this.lastFromIndexer.set(key, items);
    while (this.lastFromIndexer.size > 200) this.lastFromIndexer.delete(this.lastFromIndexer.keys().next().value!);
  }

  /**
   * Forgets the kept indexer candidates of `key` the node did not verify, which were spent since
   * the indexer listed them.
   */
  private forgetUnverified(key: string, verified: ReadonlySet<string>): void {
    const kept = this.lastFromIndexer.get(key);
    if (!kept) return;
    const next = kept.filter((c) => verified.has(outpointKey(c.transactionId, c.index)));
    if (next.length !== kept.length) this.lastFromIndexer.set(key, next);
  }

  /**
   * The spendable token UTXOs of `pubkey` for `token`, verified live on the node, largest amount
   * first.
   *
   * They are the indexer and local candidates whose derived P2SH address actually holds the
   * outpoint with the right covenant id. A local candidate the node does not list is not dropped on
   * that evidence alone, because a wrong, partial or empty answer must never wipe holdings. The
   * miss is recorded, and the candidate is pruned once it is confirmed (see `settle`). One the node
   * contradicts, with the outpoint under another covenant id, is dropped at once. A failed node
   * call throws before anything is written.
   */
  async tokenUtxosFor(pubkey: Hex, token: TokenRef): Promise<TokenUtxo[]> {
    const me = pubkey.toLowerCase();
    const covId = token.covenantId.toLowerCase();
    const localForToken = this.load(me).filter((t) => t.tokenCovId === covId);
    const { answered: indexerAnswered, items: fromIndexer } = await this.fromIndexer(me, { ...token, covenantId: covId });
    // An outpoint the indexer also lists stays in the local list. After an indexer reset to a fresh
    // database, neither source would know such holdings otherwise. They dedupe in the map below
    // and are pruned like any other candidate once spent.

    const byOutpoint = new Map<string, TrackedToken>();
    for (const c of [...fromIndexer, ...localForToken]) byOutpoint.set(`${outpointKey(c.transactionId, c.index)}:${c.state.amount}`, c);
    const candidates = [...byOutpoint.values()];
    if (candidates.length === 0) return [];

    const { verified: result, contradicted, answered } = await this.verifyOnNode(candidates);
    if (!indexerAnswered && fromIndexer.length) this.forgetUnverified(`${me}|${covId}`, new Set(result.keys()));
    this.settle(me, covId, { queried: new Set(localForToken.map((t) => outpointKey(t.transactionId, t.index))), verified: new Set(result.keys()), contradicted, answerEmpty: !answered });
    return [...result.values()].map(({ program: _p, ...u }) => u as TokenUtxo).sort(byAmountDesc);
  }

  /**
   * Checks candidates on the node.
   *
   * Each one's P2SH address is derived from its claimed state under its program, from kob-wasm's
   * pinned bytes. The node must list the outpoint there, with the candidate's covenant id.
   * - `contradicted`: the node lists the outpoint at that script under another covenant id.
   * - `answered`: the node listed anything at all. An empty answer is no evidence either way.
   * It throws when the node fails. It also throws when a script cannot be derived, unless
   * `skipUnderivable` is set. A candidate the program cannot encode is then simply not verified.
   */
  private async verifyOnNode(candidates: readonly TrackedToken[], skipUnderivable = false): Promise<{ verified: Map<string, HeldTokenUtxo>; contradicted: Set<string>; answered: boolean }> {
    const withAddress = candidates.flatMap((c) => {
      try {
        const spk = this.kob.tokenScriptPublicKey(c.program, c.state);
        return [{ c, spk: spk.toLowerCase(), address: spkStringToAddress(this.sdk, spk, this.network) }];
      } catch (e) {
        if (!skipUnderivable) throw e;
        return [];
      }
    });
    const verified = new Map<string, HeldTokenUtxo>();
    const contradicted = new Set<string>();
    if (withAddress.length === 0) return { verified, contradicted, answered: false };
    const live = await this.node.getUtxosByAddresses([...new Set(withAddress.map((w) => w.address))]);
    const liveByOutpoint = new Map(live.map((u) => [outpointKey(u.transactionId, u.index), u]));
    for (const { c, spk } of withAddress) {
      const k = outpointKey(c.transactionId, c.index);
      const u = liveByOutpoint.get(k);
      if (!u || u.scriptPublicKey.toLowerCase() !== spk) continue;
      if (u.covenantId === c.tokenCovId) {
        if (!verified.has(k)) verified.set(k, { transactionId: u.transactionId, index: u.index, amount: u.amount, blockDaaScore: u.blockDaaScore, covenantId: c.tokenCovId, state: c.state, program: c.program });
      } else contradicted.add(k); // the node says this outpoint is not a UTXO of this token: bad data
    }
    return { verified, contradicted, answered: live.length > 0 };
  }

  /**
   * Every token UTXO of `pubkey` that verifies on the node, of any token, for balances.
   *
   * It covers the registry's tokens and the others. The candidates are the indexer's holdings of
   * the owner (`GET /v1/token-utxos?owner=`, every token of a pinned program) and the local
   * candidates of every token. Besides the key-owned UTXOs the builders spend, it keeps KRON
   * pubkey-owned ones (`id_type` 0) for display only. Never pass those to a builder. `skip` names
   * tokens whose key-owned UTXOs the caller already reads with `tokenUtxosFor`. It is read-only and
   * does not change the local store. The result is sorted by token, then largest amount first.
   */
  async heldTokens(pubkey: Hex, o: { skip?: ReadonlySet<Hex> } = {}): Promise<HeldTokenUtxo[]> {
    const me = pubkey.toLowerCase();
    const skip = o.skip ?? new Set<Hex>();
    const wanted = (c: TrackedToken) =>
      (isKeyOwned(c.state, c.program) || isKronUnspendable(c.state, c.program)) && !(skip.has(c.tokenCovId) && isKeyOwned(c.state, c.program));
    const fromIndexer = (await this.allFromIndexer(me)).filter(wanted);
    const local = this.load(me).filter(wanted);
    const byClaim = new Map<string, TrackedToken>();
    for (const c of [...fromIndexer, ...local]) byClaim.set(`${outpointKey(c.transactionId, c.index)}:${c.state.amount}`, c);
    if (byClaim.size === 0) return [];
    const { verified } = await this.verifyOnNode([...byClaim.values()], true);
    return [...verified.values()].sort((a, b) => (a.covenantId === b.covenantId ? byAmountDesc(a, b) : a.covenantId < b.covenantId ? -1 : 1));
  }

  /**
   * The token UTXOs of one token, whoever holds them, that the node confirms, largest amount first.
   *
   * A market page needs them for a token of which the indexer lists no order, to learn its program
   * and its extension commitment. The indexer's `GET /v1/token-utxos?token=` names candidates with
   * a state, exactly as it does for a wallet's holdings (`allFromIndexer`). Each one is verified
   * like any other. The P2SH address derived from the claimed state under a program this build
   * pins must hold the outpoint on the node, under this covenant id. An indexer that lies, is stale
   * or knows nothing yields fewer or no confirmed UTXOs, never a wrong one. It is read-only and
   * does not touch the local store.
   * - `indexer`: whether the indexer answered at all. `unavailable` means none is configured, the
   *   route is not served or the request failed.
   * - `claimed`: how many candidates it named.
   * - `nodeFailed`: the node could not be asked, and the candidates stay unconfirmed.
   */
  async publicTokenUtxos(covenantId: Hex): Promise<{ indexer: 'ok' | 'unavailable'; claimed: number; nodeFailed: boolean; utxos: HeldTokenUtxo[] }> {
    const id = covenantId.toLowerCase();
    if (!this.indexer) return { indexer: 'unavailable', claimed: 0, nodeFailed: false, utxos: [] };
    let views: TokenUtxoView[] | null;
    try {
      views = await this.indexer.tokenUtxos({ token: id, spent: false, limit: PUBLIC_PAGE, maxPages: 2 });
    } catch {
      return { indexer: 'unavailable', claimed: 0, nodeFailed: false, utxos: [] };
    }
    if (!views) return { indexer: 'unavailable', claimed: 0, nodeFailed: false, utxos: [] };
    const candidates: TrackedToken[] = [];
    for (const v of views) {
      if (v.spent || !v.state || !v.program || !PROGRAMS.includes(v.program) || v.token.toLowerCase() !== id) continue;
      const program = v.program as TokenProgram;
      if (isKronState(v.state) !== (familyOfProgram(program) === 'kron')) continue;
      const c = sanitizeTracked({ transactionId: v.txid, index: v.index, tokenCovId: id, program, state: v.state, carrier: v.value, addedAt: this.now() }, this.now());
      if (c) candidates.push(c);
    }
    if (candidates.length === 0) return { indexer: 'ok', claimed: views.length, nodeFailed: false, utxos: [] };
    try {
      const { verified } = await this.verifyOnNode(candidates, true);
      return { indexer: 'ok', claimed: views.length, nodeFailed: false, utxos: [...verified.values()].sort(byAmountDesc) };
    } catch {
      return { indexer: 'ok', claimed: views.length, nodeFailed: true, utxos: [] };
    }
  }

  /**
   * The unspent token UTXOs a creator lock holds, verified on the node like any other candidate.
   *
   * A creator lock (kob/creator-lock.ts) holds them under owner scheme 0x03, with the lock's
   * `authority` as owner and no borrow setting. The candidates are the locked genesis outputs its
   * verified declaration names, and what the indexer lists for that owner
   * (`GET /v1/token-utxos?owner=<authority>&token=`), meaning tokens anyone sent to the lock later.
   * It is read-only and sorts the largest amount first. It throws when the node cannot be asked.
   */
  async lockedTokenUtxos(lock: { covenantId: Hex; program: TokenProgram; authority: Hex; extensionCommitment: Hex; genesis?: { txid: Hex; outputs: readonly { index: number; amount: U64 }[] } }): Promise<TokenUtxo[]> {
    const id = lock.covenantId.toLowerCase();
    const owner = lock.authority.toLowerCase();
    const zero = '0'.repeat(64);
    const held = (s: TokenState): boolean => !isKronState(s) && s.owner.toLowerCase() === owner && s.owner_scheme === 3 && s.borrow_scheme === 0 && s.borrow_guard === zero;
    const candidates: TrackedToken[] = [];
    for (const o of lock.genesis?.outputs ?? []) {
      const state: TokenState = { amount: o.amount, owner, owner_scheme: 3, borrow_scheme: 0, borrow_guard: zero, extension_commitment: lock.extensionCommitment };
      const c = sanitizeTracked({ transactionId: lock.genesis!.txid, index: o.index, tokenCovId: id, program: lock.program, state, carrier: '0', addedAt: this.now() }, this.now());
      if (c) candidates.push(c);
    }
    let views: TokenUtxoView[] | null = null;
    try {
      views = this.indexer ? await this.indexer.tokenUtxos({ owner, token: id, spent: false }) : null;
    } catch {
      views = null;
    }
    for (const v of views ?? []) {
      if (v.spent || !v.state || v.token.toLowerCase() !== id || !held(v.state)) continue;
      const c = sanitizeTracked({ transactionId: v.txid, index: v.index, tokenCovId: id, program: lock.program, state: v.state, carrier: v.value, addedAt: this.now() }, this.now());
      if (c) candidates.push(c);
    }
    const byClaim = new Map<string, TrackedToken>();
    for (const c of candidates) byClaim.set(`${outpointKey(c.transactionId, c.index)}:${c.state.amount}`, c);
    if (byClaim.size === 0) return [];
    const { verified } = await this.verifyOnNode([...byClaim.values()], true);
    return [...verified.values()].map(({ program: _p, ...u }) => u as TokenUtxo).sort(byAmountDesc);
  }

  /**
   * The indexer's holdings of an owner, of any token. Views without a state, or of a program this
   * build does not pin, are left out.
   */
  private async allFromIndexer(me: Hex): Promise<TrackedToken[]> {
    if (!this.indexer) return [];
    let views: TokenUtxoView[] | null;
    try {
      views = await this.indexer.tokenUtxos({ owner: me, spent: false });
    } catch {
      // The indexer cannot be read, so use what it gave last. The caller verifies it on the node.
      return this.lastFromIndexer.get(`${me}|*`) ?? [];
    }
    const out: TrackedToken[] = [];
    for (const v of views ?? []) {
      if (v.spent || !v.state || !v.program || !PROGRAMS.includes(v.program)) continue;
      const program = v.program as TokenProgram;
      if (isKronState(v.state) !== (familyOfProgram(program) === 'kron') || v.state.owner.toLowerCase() !== me) continue;
      const c = sanitizeTracked({ transactionId: v.txid, index: v.index, tokenCovId: v.token.toLowerCase(), program, state: v.state, carrier: v.value, addedAt: this.now() }, this.now());
      if (c) out.push(c);
    }
    if (views) this.remember(`${me}|*`, out);
    return out;
  }

  /**
   * Applies what one node check learned to the stored candidates of a token.
   *
   * It works on a fresh read of the store, because the awaits of the check can overlap an `add` or
   * a `trackFromBuilt`, whose write a stale snapshot must not revert. It touches only the outpoints
   * the check asked about.
   * - Verified: the miss record is cleared.
   * - Contradicted by the node, with the same outpoint under another covenant id: dropped at once,
   *   because that is node confirmation.
   * - An empty node answer: nothing changes. It is no evidence either way, because "all spent"
   *   looks the same as a broken answer.
   * - Otherwise a miss, recorded at most once per `missSpacingMs`. The candidate is dropped only
   *   when it is past the pending grace, has at least MISS_CONFIRMATIONS misses, and the first of
   *   them is at least the grace period old. So a bad answer or a short outage never accumulates
   *   into a deletion.
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

  /** Total token base units across the verified UTXOs. */
  async balanceFor(pubkey: Hex, token: TokenRef): Promise<bigint> {
    return (await this.tokenUtxosFor(pubkey, token)).reduce((s, u) => s + BigInt(u.state.amount), 0n);
  }
}

