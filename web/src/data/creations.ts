// Token creations of this browser that may not have reached the chain whole (KCC-23: the preparation, then the genesis spending its output 0).
//
// A record is written once the preparation is signed and validated, BEFORE it is sent, and removed once the genesis is accepted. It holds the
// signed preparation and the genesis: as an unsigned plan (`genesisBuilt`) until the wallet signed it, then signed (`genesis`). An extension
// wallet signs the genesis only after the preparation went out (wallet/chain.ts), so a record may hold no signed genesis: the user declined the
// second approval, or the page died in between. The Create token page offers it again ("Finish the creation"): a saved signed genesis is sent
// as it is; an unsigned one is signed (one approval) once the preparation's output 0 is visible.
//
// The preparation's output 0 is an ordinary P2PK UTXO of the creator, spendable by their wallet like any other coin, and discarding a record
// only forgets the unsent genesis. A record is UNTRUSTED input: anything that can write this origin's localStorage (an extension, another user
// of the device, a script) can plant one. So the shapes and sizes are checked here, and a stored genesis is never signed: "Finish the creation"
// plans it again from the record's facts and checks it as a fresh creation (wallet/creation-replan.ts, wallet/create.ts). Per-browser
// convenience: a blocked storage keeps no record, and the flow still works while the page is open.
//
// `kob.creations.v2`: a v1 record (`kob.creations.v1`, always with a signed genesis) is moved here on the first read. A page of an older build
// reads only v1 and would drop a record without a signed genesis on its next write: the new records never go there.
import { defaultStorage, type StorageLike } from '../config';
import type { IssueCurve, IssueDeclaration, IssueDocs, IssueToken } from '../kob/issue';
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
  /** "Sell on a curve": the curve the genesis places (its result line and placement record when the creation is finished later) */
  curve?: IssueCurve;
  /** "Sell at one price": what the result offers to place (base units, sompi per whole token) */
  askSale?: { sell: string; price: string };
}

const HEX32 = /^[0-9a-f]{64}$/;
const HEX = /^(?:[0-9a-f]{2})*$/;
const U64 = /^[0-9]{1,20}$/;
/** Bounds of a stored record (a planted one is dropped, never parsed further): the raw text, the records read, inputs, outputs, text. */
const MAX_RAW = 4_000_000;
const MAX_READ = 64;
const MAX_IO = 256;
const MAX_TEXT = 4096;

const obj = (v: unknown): v is Record<string, unknown> => !!v && typeof v === 'object' && !Array.isArray(v);
const str = (v: unknown, max = MAX_TEXT): v is string => typeof v === 'string' && v.length <= max;
const u64 = (v: unknown): boolean => (typeof v === 'string' && U64.test(v)) || (typeof v === 'number' && Number.isSafeInteger(v) && v >= 0);
const int = (v: unknown, max: number): boolean => typeof v === 'number' && Number.isInteger(v) && v >= 0 && v <= max;

const isInput = (i: unknown): boolean =>
  obj(i) && HEX32.test(String(i.transactionId)) && int(i.index, 0xffffffff) && str(i.signatureScript, 100_000) && HEX.test(i.signatureScript as string) &&
  obj(i.utxo) && u64(i.utxo.amount) && str(i.utxo.scriptPublicKey, 1000);
const isOutput = (o: unknown): boolean => obj(o) && u64(o.value) && str(o.scriptPublicKey, 100_000) && (o.covenant === null || o.covenant === undefined || obj(o.covenant));
const isTx = (t: unknown, id: unknown): boolean =>
  obj(t) && t.id === id && Array.isArray(t.inputs) && t.inputs.length <= MAX_IO && t.inputs.every(isInput) &&
  Array.isArray(t.outputs) && t.outputs.length <= MAX_IO && t.outputs.every(isOutput) && str(t.payload, 100_000) && HEX.test(t.payload as string);
const isToken = (t: unknown, covenantId: unknown): boolean =>
  obj(t) && t.covenantId === covenantId && str(t.name, 256) && str(t.ticker, 64) && int(t.decimals, 18) && u64(t.supply) && u64(t.carrier) &&
  str(t.program, 64) && HEX32.test(String(t.templateHash)) && HEX32.test(String(t.extensionCommitment)) && Array.isArray(t.outputs) && t.outputs.length >= 1 && t.outputs.length <= 64 &&
  t.outputs.every((o) => obj(o) && int(o.index, 63) && u64(o.amount) && HEX32.test(String(o.owner)) && int(o.ownerScheme, 255) && int(o.borrowScheme, 255) && HEX32.test(String(o.borrowGuard)));

function isRecord(v: unknown): v is CreationRecord {
  if (!v || typeof v !== 'object' || Array.isArray(v)) return false;
  const r = v as Record<string, unknown>;
  const built = r.genesisBuilt as { tx?: unknown; sign?: unknown } | undefined;
  const hasBuilt = obj(built) && isTx(built.tx, r.genesisTxid) && Array.isArray(built.sign) && built.sign.length <= MAX_IO;
  const hasSigned = r.genesis !== undefined && isTx(r.genesis, r.genesisTxid);
  if (r.genesisBuilt !== undefined && !hasBuilt) return false;
  if (r.genesis !== undefined && !hasSigned) return false;
  return (
    str(r.network, 64) && HEX32.test(String(r.maker)) && Number.isFinite(r.createdAt) && HEX32.test(String(r.covenantId)) &&
    HEX32.test(String(r.prepTxid)) && HEX32.test(String(r.genesisTxid)) && isTx(r.prep, r.prepTxid) && (hasBuilt || hasSigned) &&
    isToken(r.token, r.covenantId) && obj(r.docs) && obj(r.declaration) && str(r.declaration.json, 100_000) && typeof r.prepSent === 'boolean' &&
    (r.prepSentAt === undefined || Number.isFinite(r.prepSentAt))
  );
}

function parse(raw: string | null | undefined): CreationRecord[] {
  try {
    if (!raw || raw.length > MAX_RAW) return [];
    const v: unknown = JSON.parse(raw);
    return Array.isArray(v) ? v.slice(-MAX_READ).filter(isRecord) : [];
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
