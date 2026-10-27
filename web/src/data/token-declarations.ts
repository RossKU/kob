// Token facts from the chain: KCC-23 declarations (docs/spec/token-metadata.md). A token created under KCC-23 recorded its name, ticker and
// decimals in its preparation transaction, and its covenant id commits to them. KOB holds no authoritative token-metadata database (founder
// decision 2026-10-19): the facts come from the chain, the registry is only a pinned snapshot of them, and curation (anti-impersonation, logos)
// belongs to external community lists the user chooses, never to KOB. So:
//
//   * a declaration arrives as a launch proof from anyone (the indexer's `GET /v1/tokens/{id}/declaration`, this browser's own creation, any
//     list) and is used only after kob-wasm `verifyLaunchProof` recomputed both transaction ids and the covenant id, and only for the covenant id
//     asked for: a supplier can withhold a declaration, never forge one;
//   * verified declarations are kept in memory and, as their proofs, in localStorage (a declaration never changes: re-verified when read back);
//   * an indexer without the route, or a token without a declaration (created before KCC-23: its id cannot be changed to commit to one), is
//     simply undeclared; the display falls back as before (app/token-display.ts).
import { defaultStorage, type StorageLike } from '../config';
import { verifyLaunchProof, type VerifiedDeclaration } from '../kob/kcc23';
import type { Hex } from '../kob/types';
import type { KobWasm } from '../kob/wasm';

/** The facts a token declared on chain (KOB's convention); a field the declaration lacks, or states unacceptably, is null. */
export interface DeclaredToken {
  covenantId: Hex;
  name: string | null;
  symbol: string | null;
  decimals: number | null;
  description: string | null;
  logo: string | null;
  website: string | null;
  x: string | null;
  telegram: string | null;
  preparationTxid: Hex;
  /** the JSON text as committed */
  json: string;
}

const KEY = 'kob.declarations.v1';
const MAX_KEPT = 500;
/** A token found without a declaration is asked again after this (its genesis may be younger than the indexer's view). */
const NONE_TTL_MS = 60_000;

type Entry = { decl: DeclaredToken } | { none: number };
const memo = new Map<string, Entry>();
const inflight = new Map<string, Promise<DeclaredToken | null>>();
const listeners = new Set<() => void>();
let loadedFrom: StorageLike | null | undefined;

const k = (network: string, id: Hex) => `${network}:${id.toLowerCase()}`;
const notify = () => {
  for (const f of [...listeners]) {
    try {
      f();
    } catch {
      /* a listener must not stop the others */
    }
  }
};

function factsOf(v: VerifiedDeclaration): DeclaredToken {
  const d = v.token;
  return {
    covenantId: v.covenantId, name: d.name, symbol: d.symbol, decimals: d.decimals, description: d.description, logo: d.logo, website: d.website, x: d.x,
    telegram: d.telegram, preparationTxid: v.preparationTxid, json: v.json,
  };
}

/** Proofs kept per network: `{network: {covenantId: proof}}`. */
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
    /* a full or blocked storage keeps it in memory only */
  }
}

/**
 * Verifies `proof` for `covenantId` and keeps the declaration. Returns it, or null when the proof binds nothing to that id (nothing is kept).
 * `persist` (default true) also keeps the proof in localStorage.
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

/** Re-verifies the proofs kept in `storage` (once per storage): a declaration never changes, so a kept proof saves the indexer a request. */
export function restoreDeclarations(kob: KobWasm, storage: StorageLike | null = defaultStorage()): void {
  if (loadedFrom === storage) return;
  loadedFrom = storage;
  for (const [network, proofs] of Object.entries(readProofs(storage))) {
    if (!proofs || typeof proofs !== 'object') continue;
    for (const [id, proof] of Object.entries(proofs)) if (/^[0-9a-f]{64}$/.test(id)) adoptDeclaration(kob, network, id, proof, storage, false);
  }
}

/** The verified declaration of a token, null when it has none (as far as known), undefined when not looked up yet. */
export function declaredToken(network: string, covenantId: Hex | null | undefined): DeclaredToken | null | undefined {
  if (!covenantId) return undefined;
  const e = memo.get(k(network, covenantId));
  if (!e) return undefined;
  return 'decl' in e ? e.decl : null;
}

/** What `loadDeclaration` reads from: an indexer (or anything) that serves launch proofs. */
export interface DeclarationSource {
  declaration?(covenantId: Hex, o?: { signal?: AbortSignal }): Promise<unknown | null>;
}

/**
 * Looks a token's declaration up (once per token, again after NONE_TTL_MS when it had none), verifies it and keeps it. A failed read is not
 * remembered (asked again next time). Resolves to the declaration or null; never rejects.
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
      const proof = await source.declaration!(id);
      if (proof === null) {
        memo.set(key, { none: Date.now() });
        return null;
      }
      const decl = adoptDeclaration(kob, network, id, proof, storage);
      // a proof that does not verify is treated as none: never shown, never kept
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

/** Called whenever a declaration is newly known (the views re-read `declaredToken`). Returns the unsubscribe. */
export function subscribeDeclarations(f: () => void): () => void {
  listeners.add(f);
  return () => listeners.delete(f);
}

// ---------------------------------------------------------------------------------------------- token programs

/**
 * The token program of a covenant id as the chain shows it: the program the indexer lists the token under (its template hash, a fact of the
 * token's scripts) or the one the wallet's own token UTXOs of it verified under on the node. Kept in memory per network; a token shown without a
 * registry entry or a declaration takes its program's default decimals from it (app/token-display.ts: KaspaRocket 3). Views re-render when one
 * becomes known (the same listeners as the declarations).
 */
const programs = new Map<string, string>();

/** Remembers the program of a token (call it from an effect or an async callback, never while rendering). */
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

/** Tests only: forget everything. */
export function resetDeclarationsForTests(): void {
  memo.clear();
  inflight.clear();
  programs.clear();
  loadedFrom = undefined;
}

/** The facts on which a registry entry and a token's declaration disagree (none when the token declares nothing). */
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
