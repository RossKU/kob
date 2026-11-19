// Token facts from the chain: KCC-23 declarations (docs/spec/token-metadata.md).
//
// A token created under KCC-23 recorded its name, ticker and decimals in its preparation
// transaction, and its covenant id commits to them. KOB holds no authoritative token-metadata
// database. The facts come from the chain, and the registry is only a pinned snapshot of them.
// Curation, such as anti-impersonation and logos, belongs to external community lists the user
// chooses, never to KOB. So:
// - A declaration arrives as a launch proof from anyone: the indexer's
//   `GET /v1/tokens/{id}/declaration`, this browser's own creation or any list. It is used only
//   after kob-wasm `verifyLaunchProof` recomputed both transaction ids and the covenant id, and
//   only for the covenant id asked for. A supplier can withhold a declaration, never forge one.
// - Verified declarations are kept in memory, and as their proofs in localStorage. A declaration
//   never changes, and it is verified again when read back.
// - An indexer without the route, or a token without a declaration, gives an undeclared token. A
//   token created before KCC-23 has no declaration, because its id cannot be changed to commit to
//   one. The display then falls back to the other sources (app/token-display.ts).
import { defaultStorage, type StorageLike } from '../config';
import { verifyLaunchProof, type VerifiedDeclaration } from '../kob/kcc23';
import type { VerifiedLock } from '../kob/creator-lock';
import type { Hex } from '../kob/types';
import type { KobWasm } from '../kob/wasm';

/**
 * The facts a token declared on chain, in KOB's convention. A field the declaration lacks, or
 * states unacceptably, is null.
 */
export interface DeclaredToken {
  covenantId: Hex;
  name: string | null;
  symbol: string | null;
  decimals: number | null;
  description: string | null;
  /** The picture's address (`image`, or the older `logo`). */
  image: string | null;
  /** The SHA-256 the creator committed to for the picture file (`image_hash`), 64 lowercase hex. */
  imageHash: string | null;
  website: string | null;
  x: string | null;
  telegram: string | null;
  preparationTxid: Hex;
  /** The genesis's id. The locked outputs of a creator lock are its outputs. */
  genesisTxid?: Hex;
  /** The JSON text as committed. */
  json: string;
  /**
   * The declared creator lock, verified against the genesis, with what it holds and until when.
   * Null for none.
   */
  lock?: VerifiedLock | null;
}

const KEY = 'kob.declarations.v1';
const MAX_KEPT = 500;
/**
 * A token found without a declaration is asked again after this, because its genesis may be
 * younger than the indexer's view.
 */
const NONE_TTL_MS = 60_000;

type Entry = { decl: DeclaredToken } | { none: number };
const memo = new Map<string, Entry>();
const inflight = new Map<string, Promise<DeclaredToken | null>>();
/**
 * At most this many indexer reads at once. A cold start of a list of declared tokens asks a few
 * at a time, not all in one burst.
 */
export const MAX_PARALLEL_READS = 4;
let reading = 0;
const waiting: Array<() => void> = [];
async function withSlot<T>(fn: () => Promise<T>): Promise<T> {
  if (reading >= MAX_PARALLEL_READS) await new Promise<void>((r) => waiting.push(r));
  reading++;
  try {
    return await fn();
  } finally {
    reading--;
    waiting.shift()?.();
  }
}
const listeners = new Set<() => void>();
let loadedFrom: StorageLike | null | undefined;

const k = (network: string, id: Hex) => `${network}:${id.toLowerCase()}`;
const notify = () => {
  for (const f of [...listeners]) {
    try {
      f();
    } catch {
      /* A listener must not stop the others. */
    }
  }
};

function factsOf(v: VerifiedDeclaration): DeclaredToken {
  const d = v.token;
  return {
    covenantId: v.covenantId, name: d.name, symbol: d.symbol, decimals: d.decimals, description: d.description, image: d.image, imageHash: d.imageHash, website: d.website,
    x: d.x, telegram: d.telegram, preparationTxid: v.preparationTxid, genesisTxid: v.genesisTxid, json: v.json, lock: v.lock,
  };
}

/** Proofs kept per network, as `{network: {covenantId: proof}}`. */
function readProofs(storage: StorageLike | null): Record<string, Record<string, unknown>> {
  try {
    const raw = storage?.getItem(KEY);
    const v: unknown = raw ? JSON.parse(raw) : null;
    return v && typeof v === 'object' && !Array.isArray(v) ? (v as Record<string, Record<string, unknown>>) : {};
  } catch {
    return {};
  }
}

function keepProof(network: string, id: Hex, proof: unknown, storage: StorageLike | null): void {
  if (!storage) return;
  try {
    const all = readProofs(storage);
    const net = Object.entries(all[network] ?? {}).filter(([x]) => x !== id);
    net.push([id, proof]);
    all[network] = Object.fromEntries(net.slice(-MAX_KEPT));
    storage.setItem(KEY, JSON.stringify(all));
  } catch {
    /* A full or blocked storage keeps it in memory only. */
  }
}

/**
 * Verifies `proof` for `covenantId` and keeps the declaration. Returns it, or null when the proof
 * binds nothing to that id, and then nothing is kept. `persist` (default true) also keeps the
 * proof in localStorage.
 */
export function adoptDeclaration(kob: KobWasm, network: string, covenantId: Hex, proof: unknown, storage: StorageLike | null = defaultStorage(), persist = true): DeclaredToken | null {
  const id = covenantId.toLowerCase();
  let v: VerifiedDeclaration;
  try {
    v = verifyLaunchProof(kob, proof);
  } catch {
    return null;
  }
  if (v.covenantId !== id) return null;
  const decl = factsOf(v);
  const prev = memo.get(k(network, id));
  memo.set(k(network, id), { decl });
  if (persist) keepProof(network, id, proof, storage);
  if (!prev || !('decl' in prev)) notify();
  return decl;
}

/**
 * Verifies the proofs kept in `storage` again, once per storage. A declaration never changes, so
 * a kept proof saves the indexer a request.
 */
export function restoreDeclarations(kob: KobWasm, storage: StorageLike | null = defaultStorage()): void {
  if (loadedFrom === storage) return;
  loadedFrom = storage;
  for (const [network, proofs] of Object.entries(readProofs(storage))) {
    if (!proofs || typeof proofs !== 'object') continue;
    for (const [id, proof] of Object.entries(proofs)) if (/^[0-9a-f]{64}$/.test(id)) adoptDeclaration(kob, network, id, proof, storage, false);
  }
}

/**
 * The verified declaration of a token. Null when it has none as far as known, undefined when not
 * looked up yet.
 */
export function declaredToken(network: string, covenantId: Hex | null | undefined): DeclaredToken | null | undefined {
  if (!covenantId) return undefined;
  const e = memo.get(k(network, covenantId));
  if (!e) return undefined;
  return 'decl' in e ? e.decl : null;
}

/**
 * Every verified declaration known on `network`, in memory and from the proofs kept in this
 * browser once restored.
 */
export function knownDeclarations(network: string): DeclaredToken[] {
  const out: DeclaredToken[] = [];
  const prefix = `${network}:`;
  for (const [key, e] of memo) if (key.startsWith(prefix) && 'decl' in e) out.push(e.decl);
  return out;
}

/** What `loadDeclaration` reads from: an indexer, or anything else that serves launch proofs. */
export interface DeclarationSource {
  declaration?(covenantId: Hex, o?: { signal?: AbortSignal }): Promise<unknown | null>;
}

/**
 * Looks a token's declaration up, verifies it and keeps it. It asks once per token, and again
 * after NONE_TTL_MS when the token had none. A failed read is not remembered and is asked again
 * next time. It resolves to the declaration or null and never rejects.
 */
export async function loadDeclaration(kob: KobWasm, source: DeclarationSource | null, network: string, covenantId: Hex, storage: StorageLike | null = defaultStorage()): Promise<DeclaredToken | null> {
  const id = covenantId.toLowerCase();
  const key = k(network, id);
  const e = memo.get(key);
  if (e && 'decl' in e) return e.decl;
  if (e && 'none' in e && Date.now() - e.none < NONE_TTL_MS) return null;
  if (!source?.declaration) return null;
  const running = inflight.get(key);
  if (running) return running;
  const p = (async () => {
    try {
      const proof = await withSlot(() => source.declaration!(id));
      if (proof === null) {
        memo.set(key, { none: Date.now() });
        return null;
      }
      const decl = adoptDeclaration(kob, network, id, proof, storage);
      // A proof that does not verify is treated as none, never shown and never kept.
      if (!decl) memo.set(key, { none: Date.now() });
      return decl;
    } catch {
      return null;
    } finally {
      inflight.delete(key);
    }
  })();
  inflight.set(key, p);
  return p;
}

/**
 * Calls `f` whenever a declaration, a token program or listed tickers become known, and the views
 * read `declaredToken` again. Returns the unsubscribe.
 */
export function subscribeDeclarations(f: () => void): () => void {
  listeners.add(f);
  return () => listeners.delete(f);
}

// --- Token programs ---

/**
 * The token program of a covenant id as the chain shows it, kept in memory per network.
 *
 * It is the program the indexer lists the token under, by its template hash, a fact of the
 * token's scripts. Or it is the one the wallet's own token UTXOs of it verified under on the node.
 * A token shown without a registry entry or a declaration takes its program's default decimals
 * from it (app/token-display.ts, KaspaRocket 3). Views re-render when one becomes known, through
 * the same listeners as the declarations.
 */
const programs = new Map<string, string>();

/**
 * Remembers the program of a token. Call it from an effect or an async callback, never while
 * rendering.
 */
export function noteTokenProgram(network: string, covenantId: Hex | null | undefined, program: string | null | undefined): void {
  if (!covenantId || !program || !/^[0-9a-fA-F]{64}$/.test(covenantId)) return;
  const key = k(network, covenantId);
  if (programs.get(key) === program) return;
  programs.set(key, program);
  notify();
}

/** The program a token was seen under (`noteTokenProgram`), else null. */
export function tokenProgramOf(network: string, covenantId: Hex | null | undefined): string | null {
  return covenantId ? (programs.get(k(network, covenantId)) ?? null) : null;
}

/**
 * The decimals the market of a token trades with where nothing on chain states them. That is the
 * scale its first order set, as the indexer lists it or the token's page read it
 * (kob/open-token.ts). They are kept in memory per network for what is shown with no order at
 * hand, such as a balance or a token movement, in the ticket's whole token.
 */
const marketDecimals = new Map<string, number>();

/**
 * Remembers a market's decimals. Call it from an effect or an async callback, never while
 * rendering.
 */
export function noteMarketDecimals(network: string, covenantId: Hex | null | undefined, decimals: number | null | undefined): void {
  if (!covenantId || decimals === null || decimals === undefined || !/^[0-9a-fA-F]{64}$/.test(covenantId)) return;
  marketDecimals.set(k(network, covenantId), decimals);
}

/** The decimals noted for a token's market, else null. */
export function marketDecimalsOf(network: string, covenantId: Hex): number | null {
  return marketDecimals.get(k(network, covenantId)) ?? null;
}

/**
 * The ticker the indexer lists a token under. It is a label for what is shown with no list at
 * hand, such as an activity row or an order row. The id stays the identity.
 */
const listedTickers = new Map<string, string>();

/** Remembers the listed tickers and tells the views. `rows` are as `GET /v1/tokens` gives them. */
export function noteListedTickers(network: string, rows: readonly { covenant_id: Hex; ticker?: string | null }[]): void {
  for (const r of rows) if (r.ticker) listedTickers.set(k(network, r.covenant_id), r.ticker);
  notify();
}

/** The listed ticker of a token, else null. */
export function listedTickerOf(network: string, covenantId: Hex): string | null {
  return listedTickers.get(k(network, covenantId)) ?? null;
}

/** Forgets everything. For tests only. */
export function resetDeclarationsForTests(): void {
  listedTickers.clear();
  memo.clear();
  inflight.clear();
  programs.clear();
  marketDecimals.clear();
  loadedFrom = undefined;
}

/**
 * The facts on which a registry entry and a token's declaration disagree. None when the token
 * declares nothing.
 */
export function declarationConflicts(
  entry: { ticker: string; name: string; decimals: number },
  decl: DeclaredToken | null | undefined,
): ('decimals' | 'symbol' | 'name')[] {
  if (!decl) return [];
  const out: ('decimals' | 'symbol' | 'name')[] = [];
  if (decl.decimals !== null && decl.decimals !== entry.decimals) out.push('decimals');
  if (decl.symbol !== null && decl.symbol !== entry.ticker) out.push('symbol');
  if (decl.name !== null && decl.name !== entry.name) out.push('name');
  return out;
}
