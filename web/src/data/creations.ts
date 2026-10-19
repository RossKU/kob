// Token creations of this browser that were signed but may not have reached the chain whole (KCC-23: the preparation, then the genesis). A
// record is written once both transactions are signed and validated, BEFORE the preparation is sent, and removed once the genesis is
// accepted; a record that outlives the page is a creation whose genesis may not have gone out. The Create token page offers it again: the
// saved, signed genesis is sent as it is (no new signature), after the preparation (sent again: a node that has it refuses it, harmlessly).
//
// Nothing here can lose funds: the preparation's output 0 is an ordinary P2PK UTXO of the creator, spendable by their wallet like any other
// coin, and discarding a record only forgets the unsent genesis. The signed transactions are not secrets (they are meant to be public);
// anyone who could read this storage could only complete the creation the user approved. Per-browser convenience: a blocked storage keeps no
// record, and the flow still works while the page is open.
import { defaultStorage, type StorageLike } from '../config';
import type { IssueDeclaration, IssueDocs, IssueToken } from '../kob/issue';
import type { Hex, TxJson } from '../kob/types';

const KEY = 'kob.creations.v1';
/** Most records kept per network (the oldest are dropped). */
const MAX_RECORDS = 20;

export interface CreationRecord {
  network: string;
  /** the creator's x-only key: the records of another wallet are not offered */
  maker: Hex;
  /** ms since the epoch */
  createdAt: number;
  covenantId: Hex;
  prepTxid: Hex;
  genesisTxid: Hex;
  /** both signed and validated */
  prep: TxJson;
  genesis: TxJson;
  token: IssueToken;
  declaration: IssueDeclaration;
  docs: IssueDocs;
  /** the preparation was accepted by the node (sent): the genesis is what remains */
  prepSent: boolean;
}

const HEX32 = /^[0-9a-f]{64}$/;

function isRecord(v: unknown): v is CreationRecord {
  if (!v || typeof v !== 'object' || Array.isArray(v)) return false;
  const r = v as Record<string, unknown>;
  const tx = (t: unknown, id: unknown): boolean =>
    !!t && typeof t === 'object' && (t as { id?: unknown }).id === id && Array.isArray((t as { inputs?: unknown }).inputs) && Array.isArray((t as { outputs?: unknown }).outputs);
  return (
    typeof r.network === 'string' && HEX32.test(String(r.maker)) && typeof r.createdAt === 'number' && HEX32.test(String(r.covenantId)) &&
    HEX32.test(String(r.prepTxid)) && HEX32.test(String(r.genesisTxid)) && tx(r.prep, r.prepTxid) && tx(r.genesis, r.genesisTxid) &&
    !!r.token && typeof r.token === 'object' && (r.token as IssueToken).covenantId === r.covenantId && !!r.docs && !!r.declaration && typeof r.prepSent === 'boolean'
  );
}

function readAll(storage: StorageLike | null): CreationRecord[] {
  try {
    const raw = storage?.getItem(KEY);
    const v: unknown = raw ? JSON.parse(raw) : null;
    return Array.isArray(v) ? v.filter(isRecord) : [];
  } catch {
    return [];
  }
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

/** Marks the preparation of a record as sent. */
export function markPrepSent(network: string, covenantId: Hex, storage: StorageLike | null = defaultStorage()): void {
  const all = readAll(storage);
  const r = all.find((x) => x.network === network && x.covenantId === covenantId);
  if (r && !r.prepSent) {
    r.prepSent = true;
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
