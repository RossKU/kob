// Token creations of this browser that may not have reached the chain whole (KCC-23: the preparation, then the genesis spending its output 0).
//
// A record is written once the preparation is signed and validated, BEFORE it is sent, and removed once the genesis is accepted. It holds the
// signed preparation and the genesis: as an unsigned plan (`genesisBuilt`) until the wallet signed it, then signed (`genesis`). An extension
// wallet signs the genesis only after the preparation went out (wallet/chain.ts), so a record may hold no signed genesis: the user declined the
// second approval, or the page died in between. The Create token page offers it again ("Finish the creation"): a saved signed genesis is sent
// as it is; an unsigned one is signed (one approval) once the preparation's output 0 is visible.
//
// Nothing here can lose funds: the preparation's output 0 is an ordinary P2PK UTXO of the creator, spendable by their wallet like any other
// coin, and discarding a record only forgets the unsent genesis. The transactions are not secrets (they are meant to be public); anyone who
// could read this storage could only complete the creation the user approved. Per-browser convenience: a blocked storage keeps no record, and
// the flow still works while the page is open.
//
// `kob.creations.v2`: a v1 record (`kob.creations.v1`, always with a signed genesis) is moved here on the first read. A page of an older build
// reads only v1 and would drop a record without a signed genesis on its next write: the new records never go there.
import { defaultStorage, type StorageLike } from '../config';
import type { IssueDeclaration, IssueDocs, IssueToken } from '../kob/issue';
import type { BuiltTx, Hex, TxJson } from '../kob/types';

const KEY = 'kob.creations.v2';
const KEY_V1 = 'kob.creations.v1';
/** Most records kept per network (the oldest are dropped). */
const MAX_RECORDS = 20;
/**
 * The executor's indexer drops a preparation no genesis spent after 72 hours (crates/kobd/src/config.rs): a genesis sent later is valid, but
 * other apps may no longer show the name. Past this age (60 h) the page warns and recommends a new creation.
 */
export const DECLARATION_WARN_MS = 60 * 3600_000;

export interface CreationRecord {
  network: string;
  /** the creator's x-only key: the records of another wallet are not offered */
  maker: Hex;
  /** ms since the epoch */
  createdAt: number;
  covenantId: Hex;
  prepTxid: Hex;
  genesisTxid: Hex;
  /** signed and validated */
  prep: TxJson;
  /** the genesis as planned, unsigned (absent in a record moved from v1, which always holds a signed genesis) */
  genesisBuilt?: BuiltTx;
  /** the genesis signed and validated, once the wallet signed it */
  genesis?: TxJson;
  token: IssueToken;
  declaration: IssueDeclaration;
  docs: IssueDocs;
  /** the preparation was accepted by the node (sent): the genesis is what remains */
  prepSent: boolean;
  /** when the node took the preparation (ms since the epoch) */
  prepSentAt?: number;
}

const HEX32 = /^[0-9a-f]{64}$/;

const isTx = (t: unknown, id: unknown): boolean =>
  !!t && typeof t === 'object' && (t as { id?: unknown }).id === id && Array.isArray((t as { inputs?: unknown }).inputs) && Array.isArray((t as { outputs?: unknown }).outputs);

function isRecord(v: unknown): v is CreationRecord {
  if (!v || typeof v !== 'object' || Array.isArray(v)) return false;
  const r = v as Record<string, unknown>;
  const built = r.genesisBuilt as { tx?: unknown; sign?: unknown } | undefined;
  const hasBuilt = built !== undefined && !!built && typeof built === 'object' && isTx(built.tx, r.genesisTxid) && Array.isArray(built.sign);
  const hasSigned = r.genesis !== undefined && isTx(r.genesis, r.genesisTxid);
  if (r.genesisBuilt !== undefined && !hasBuilt) return false;
  if (r.genesis !== undefined && !hasSigned) return false;
  return (
    typeof r.network === 'string' && HEX32.test(String(r.maker)) && typeof r.createdAt === 'number' && HEX32.test(String(r.covenantId)) &&
    HEX32.test(String(r.prepTxid)) && HEX32.test(String(r.genesisTxid)) && isTx(r.prep, r.prepTxid) && (hasBuilt || hasSigned) &&
    !!r.token && typeof r.token === 'object' && (r.token as IssueToken).covenantId === r.covenantId && !!r.docs && !!r.declaration && typeof r.prepSent === 'boolean' &&
    (r.prepSentAt === undefined || typeof r.prepSentAt === 'number')
  );
}

function parse(raw: string | null | undefined): CreationRecord[] {
  try {
    const v: unknown = raw ? JSON.parse(raw) : null;
    return Array.isArray(v) ? v.filter(isRecord) : [];
  } catch {
    return [];
  }
}

function readAll(storage: StorageLike | null): CreationRecord[] {
  let v2: CreationRecord[];
  let v1raw: string | null | undefined;
  try {
    v2 = parse(storage?.getItem(KEY));
    v1raw = storage?.getItem(KEY_V1);
  } catch {
    return [];
  }
  if (!v1raw) return v2;
  // move the v1 records here (a record of the same creation already here wins), then drop the v1 key
  const v1 = parse(v1raw).filter((r) => !v2.some((x) => x.network === r.network && x.covenantId === r.covenantId));
  const all = [...v1, ...v2];
  if (writeAll(storage, all)) {
    try {
      storage?.removeItem?.(KEY_V1);
    } catch {
      /* moved again on the next read */
    }
  }
  return all;
}

function writeAll(storage: StorageLike | null, all: CreationRecord[]): boolean {
  if (!storage) return false;
  try {
    const byNet = new Map<string, CreationRecord[]>();
    for (const r of all) byNet.set(r.network, [...(byNet.get(r.network) ?? []), r]);
    const kept = [...byNet.values()].flatMap((rs) => rs.slice(-MAX_RECORDS));
    if (kept.length) storage.setItem(KEY, JSON.stringify(kept));
    else storage.removeItem?.(KEY);
    return true;
  } catch {
    return false;
  }
}

/** Saves (or replaces, by covenant id) a creation record. False when the storage is unavailable. */
export function saveCreation(rec: CreationRecord, storage: StorageLike | null = defaultStorage()): boolean {
  const all = readAll(storage).filter((r) => !(r.network === rec.network && r.covenantId === rec.covenantId));
  return writeAll(storage, [...all, rec]);
}

/** Marks the preparation of a record as sent (at `at`, ms). */
export function markPrepSent(network: string, covenantId: Hex, storage: StorageLike | null = defaultStorage(), at: number = Date.now()): void {
  const all = readAll(storage);
  const r = all.find((x) => x.network === network && x.covenantId === covenantId);
  if (r && !r.prepSent) {
    r.prepSent = true;
    r.prepSentAt = at;
    writeAll(storage, all);
  }
}

/** Forgets a record (the genesis was accepted, or the user discarded it). */
export function removeCreation(network: string, covenantId: Hex, storage: StorageLike | null = defaultStorage()): void {
  const all = readAll(storage);
  const rest = all.filter((r) => !(r.network === network && r.covenantId === covenantId));
  if (rest.length !== all.length) writeAll(storage, rest);
}

/** The records of `maker` on `network`, oldest first. */
export function listCreations(network: string, maker: Hex, storage: StorageLike | null = defaultStorage()): CreationRecord[] {
  const pk = maker.toLowerCase();
  return readAll(storage).filter((r) => r.network === network && r.maker === pk);
}

/**
 * The preparation output 0 of every record (`txid:0`, lower case): reserved for its genesis, never funding of another transaction while the
 * record exists (app/pending-spends.ts).
 */
export function reservedCreationOutpoints(storage: StorageLike | null = defaultStorage()): Set<string> {
  return new Set(readAll(storage).map((r) => `${r.prepTxid.toLowerCase()}:0`));
}

/** The record holds a signed genesis: finishing it needs no signature. */
export const genesisSigned = (r: CreationRecord): r is CreationRecord & { genesis: TxJson } => r.genesis !== undefined;

/** The record is old enough that the declaration window of other apps may be closing (`DECLARATION_WARN_MS`). */
export const creationIsOld = (r: Pick<CreationRecord, 'createdAt' | 'prepSentAt'>, now: number = Date.now()): boolean => now - (r.prepSentAt ?? r.createdAt) > DECLARATION_WARN_MS;
