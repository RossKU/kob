// Placement records: what the maker needs to find and cancel an order later without the indexer.
//
// Order UTXOs are P2SH. Their terms are on chain only in the placement record of the creating tx
// (docs/spec/matcher.md section 1.1), so the maker keeps their own copy.
//
// - `recordsFromBuilt` derives records from a built create or cancel-replace tx with kob-wasm
//   `recoverOrders`, so nothing is trusted.
// - `amendedRecords` moves a record to the state an in-place amend continued it with
//   (`recoverAmends`).
// - `sweptRecords` keeps a swept order's record and moves its last proven state to the
//   continuation, because a sweep never ends the order.
// - `RecordStore` has memory and localStorage implementations. Every localStorage access is guarded
//   against private mode, quota errors and corrupt data.
// - Export and import use the indexer's maker-recovery format (docs/ops/executor.md, section 7.5)
//   and a fuller KOB backup that also keeps the placed txs. Every import is re-validated with
//   kob-wasm, so an import never adds a record the maker could not have placed.
// - `resolveRecord` asks a node what became of an order.
//
// resolveRecord algorithm. A P2SH address is a hash, so it cannot reveal a state we do not already
// know. A partial fill splices new values into the redeem script, which gives a new script and a
// new address. Only the covenant id continues. The search is therefore:
//
// 0. A record of a template this build neither pins nor keeps in the cancel-only table is never
//    searched. Its script cannot be derived, so the answer is 'old-template'. A retired template of
//    the cancel-only table is searched under its own script.
// 1. Ask the node about the address of the record's original state and of its last proven state. A
//    UTXO there carrying the record's covenant id means unchanged and live.
// 2. Otherwise enumerate candidate continuation states. They differ from the recorded one only in
//    the fields that kob-wasm `mutableWindows(kind)` lists. Each candidate is derived by editing
//    the decoded state and letting kob-wasm compute its script public key, with no byte splicing in
//    TypeScript. A node UTXO counts only when it carries the covenant id and its script equals the
//    candidate's script.
// 3. `hints` (UTXOs learned elsewhere, such as from the indexer) are matched against the same
//    candidate set, so a hint can never smuggle in a state.
// 4. For ask-side kinds the custody token UTXO is looked up at the address of the custody state
//    that the resolved state implies.
// 5. If nothing is found the answer is 'spent' only when the search was exhaustive. That holds for
//    kinds whose only mutable field is amountLeft, or that have none, with an amount small enough
//    to try every value. Otherwise the answer is 'unknown'.
//
// Records also keep the last proven state of the order in `last`, from a proven indexer view or a
// node resolution. The placed `state` stays the identity. The search starts from `last`, and the
// DAA of its UTXO is a candidate for `armed`, because a band stop armed inside a fill stores the
// DAA of the UTXO it spent.
//
// The candidates are walked breadth first and bounded.
//
// - amountLeft and rptAmount walk jointly for repeat entries (rpt = rptOrig - filled, amount =
//   amountOrig - filled + merged).
// - A trailing stop's stopPrice walks in trailStep steps.
// - `armed` takes the values {recorded, 0, 1, last seen UTXO DAA, placement DAA}.
//
// Amounts are base units. Every value is tried while the amount is small. A larger amount is walked
// in steps of the order's minimum fill, which covers fills of exactly minFill, and is never
// exhaustive. The node alone then answers 'unknown' rather than 'spent'. The indexer's proven state
// finds such an order at once.
import type { OrderView } from '../data/indexer-types';
import type { NodeApi, NodeUtxo } from '../data/node';
import { asOrderState, big, custodiesOf, extensionOf, familyOfKind, familyOfOrder, isIfdKind, isOrderKind, pairExtFor, pairFactsOf, sideOf, tokenTplHashOf } from './order-facts';
import { custodyState, extensionOfState } from './token-state';
import { recoverSweeps } from './sweep';
import type { Hex, OrderKind, OrderState, OrderUtxo, RetiredTemplateInfo, SigPlan, TokenProgram, TokenState, TokenUtxo, TxJson, U64 } from './types';
import type { KobWasm } from './wasm';


/**
 * The kind a record names. These are today's order kinds, including the pair kinds `KobPair`,
 * `KobCondPair` and `KobIfdPair`, and the cross limit that earlier builds placed (`KobCross`,
 * `KobCrossKron`). A cross record is kept and shown as an order of a template this build does not
 * pin.
 */
export type RecordKind = OrderKind | 'KobCross' | 'KobCrossKron';
/** True when `k` is a kind a record may name. */
export const isRecordKind = (k: string): k is RecordKind => isOrderKind(k) || k === 'KobCross' || k === 'KobCrossKron';

/** What the maker keeps about one order, enough to find it on a node and to cancel it. */
export interface PlacementRecord {
  version: 1;
  network: string;
  maker: Hex;
  /** The placing tx. Null only for records imported from the indexer recovery format. */
  txid: Hex | null;
  output: number | null;
  covenantId: Hex;
  /** The kind as the placing build tagged it. */
  kind: RecordKind;
  templateHash: Hex;
  /** The original state span as placed, in hex. */
  state: Hex;
  /**
   * Base units the order was placed for, as a decimal string. Null for a plain bid, whose quantity
   * is its escrow.
   */
  amount?: U64 | null;
  custody: { output: number; value: U64; tokenState?: TokenState } | null;
  /**
   * A sell-first `KobIfdPair` only: its B prefund custody at placement, the second custody of its
   * record.
   */
  prefund?: { output: number; value: U64; tokenState?: TokenState } | null;
  /** KAS on the order UTXO at placement. */
  value: U64;
  deadline?: U64 | null;
  placedAtUnix: U64;
  placedAtDaa: U64;
  note?: string;
  /** Plan label shown in the UI. */
  label?: string;
  /**
   * Extension commitment of the order's token (for the ask side, its custody), when known from a
   * recovery file v2, an indexer view or the registry.
   */
  ext?: Hex | null;
  /**
   * The last proven current state of the order (an indexer view with `state_known`, or a node
   * resolution) and the UTXO it was seen at.
   */
  last?: { state: Hex; txid: Hex; index: number; daa: U64 } | null;
  /**
   * A cancel, refund or replace of this order was submitted (tx id, the outpoint it spends, when).
   * The record is dropped once that is final.
   */
  cancelling?: { txid: Hex; spends: string; atUnix: U64 } | null;
  /**
   * How the record came to be. 'placed' is placed by this wallet, the default. 'indexer' is adopted
   * from the indexer, for exits and orders placed elsewhere.
   */
  origin?: 'placed' | 'indexer';
}

// ------------------------------------------------------------------------------ validation helpers

const isHex = (v: unknown, bytes?: number): v is string =>
  typeof v === 'string' && /^([0-9a-f]{2})*$/.test(v) && (bytes === undefined || v.length === bytes * 2);
const isDec = (v: unknown): v is string => typeof v === 'string' && /^[0-9]+$/.test(v);
const isObj = (v: unknown): v is Record<string, unknown> => typeof v === 'object' && v !== null && !Array.isArray(v);

/**
 * Structural check of a stored or imported record. It checks types only, and semantic checks run
 * against kob-wasm.
 */
export function isPlacementRecord(x: unknown): x is PlacementRecord {
  if (!isObj(x)) return false;
  return (
    x.version === 1 && typeof x.network === 'string' && isHex(x.maker, 32) && (x.txid === null || isHex(x.txid, 32)) &&
    (x.output === null || (Number.isInteger(x.output) && (x.output as number) >= 0)) && isHex(x.covenantId, 32) &&
    typeof x.kind === 'string' && isRecordKind(x.kind) && isHex(x.templateHash, 32) && isHex(x.state) && (x.state as string).length > 0 &&
    (x.amount === undefined || x.amount === null || isDec(x.amount)) && isDec(x.value) && isDec(x.placedAtUnix) && isDec(x.placedAtDaa) &&
    (x.custody === null || (isObj(x.custody) && Number.isInteger(x.custody.output) && isDec(x.custody.value))) &&
    (x.prefund === undefined || x.prefund === null || (isObj(x.prefund) && Number.isInteger(x.prefund.output) && isDec(x.prefund.value))) &&
    (x.deadline === undefined || x.deadline === null || isDec(x.deadline)) &&
    (x.ext === undefined || x.ext === null || isHex(x.ext, 32)) &&
    (x.last === undefined || x.last === null || (isObj(x.last) && isHex(x.last.state) && (x.last.state as string).length > 0 && isHex(x.last.txid, 32) && Number.isInteger(x.last.index) && isDec(x.last.daa))) &&
    (x.cancelling === undefined || x.cancelling === null || (isObj(x.cancelling) && isHex(x.cancelling.txid, 32) && typeof x.cancelling.spends === 'string' && isDec(x.cancelling.atUnix))) &&
    (x.origin === undefined || x.origin === 'placed' || x.origin === 'indexer')
  );
}

const amountOf = (o: OrderState): U64 | null => ('amountLeft' in o.state ? String((o.state as { amountLeft: string }).amountLeft) : null);

function pinnedHash(kob: KobWasm, kind: OrderKind): Hex {
  const t = kob.templates().find((x) => x.name === kind);
  if (!t) throw new Error(`template ${kind} is not pinned by this build`);
  return t.hash;
}

// ------------------------------------------------------------------------------------------ stores

/** Storage of placement records by covenant id. */
export interface RecordStore {
  get(covenantId: Hex): Promise<PlacementRecord | null>;
  put(rec: PlacementRecord): Promise<void>;
  list(): Promise<PlacementRecord[]>;
  remove(covenantId: Hex): Promise<void>;
  /**
   * False when the records live in memory only (browser storage is blocked, full or unreadable).
   * They are then lost when the tab closes, so the UI asks the user to export a backup. Absent
   * means persistent.
   */
  persistent?(): boolean;
}

/** A record store that keeps everything in memory. */
export class MemoryRecordStore implements RecordStore {
  private m = new Map<Hex, PlacementRecord>();
  async get(id: Hex): Promise<PlacementRecord | null> {
    return this.m.get(id) ?? null;
  }
  async put(rec: PlacementRecord): Promise<void> {
    this.m.set(rec.covenantId, rec);
  }
  async list(): Promise<PlacementRecord[]> {
    return [...this.m.values()];
  }
  async remove(id: Hex): Promise<void> {
    this.m.delete(id);
  }
}

/** The part of the Web Storage interface the record store uses. */
export interface StorageLike {
  getItem(key: string): string | null;
  setItem(key: string, value: string): void;
  removeItem(key: string): void;
}

/**
 * A localStorage-backed store with one JSON object per (network, maker) under
 * `kob.records.v1:<network>:<maker>`, so a wallet key on one network never sees another's records.
 * Every access is guarded. When the storage throws (blocked, quota) or holds corrupt data, the
 * store keeps working from memory, and the UI should remind the user to export a backup.
 */
export class LocalStorageRecordStore implements RecordStore {
  private mem = new Map<Hex, PlacementRecord>();
  /**
   * True when a read or write of the storage failed or there is no storage, so the records are in
   * memory only.
   */
  private degraded: boolean;
  readonly key: string;
  constructor(private storage: StorageLike | null, private network: string, private maker: Hex) {
    this.key = `kob.records.v1:${network}:${maker}`;
    this.degraded = storage === null;
  }
  persistent(): boolean {
    return !this.degraded;
  }
  private load(): Map<Hex, PlacementRecord> {
    if (!this.storage) return this.mem;
    try {
      const raw = this.storage.getItem(this.key);
      if (raw === null) return this.mem;
      const parsed: unknown = JSON.parse(raw);
      if (!isObj(parsed)) return this.mem;
      const m = new Map<Hex, PlacementRecord>();
      for (const [id, r] of Object.entries(parsed)) {
        if (isPlacementRecord(r) && r.covenantId === id && r.network === this.network && r.maker === this.maker) m.set(id, r);
      }
      this.mem = m;
    } catch {
      /* unreadable storage or corrupt JSON: keep the in-memory copy */
      this.degraded = true;
    }
    return this.mem;
  }
  private save(m: Map<Hex, PlacementRecord>): void {
    this.mem = m;
    if (!this.storage) return;
    try {
      if (m.size === 0) this.storage.removeItem(this.key);
      else this.storage.setItem(this.key, JSON.stringify(Object.fromEntries(m)));
      this.degraded = false;
    } catch {
      /* quota / blocked: memory only */
      this.degraded = true;
    }
  }
  async get(id: Hex): Promise<PlacementRecord | null> {
    return this.load().get(id) ?? null;
  }
  async put(rec: PlacementRecord): Promise<void> {
    if (rec.network !== this.network || rec.maker !== this.maker) throw new Error('record belongs to another network or maker');
    const m = new Map(this.load());
    m.set(rec.covenantId, rec);
    this.save(m);
  }
  async list(): Promise<PlacementRecord[]> {
    return [...this.load().values()];
  }
  async remove(id: Hex): Promise<void> {
    const m = new Map(this.load());
    m.delete(id);
    this.save(m);
  }
}

// ------------------------------------------------------------------------- records from a built tx

/** The context of a placement that a record needs besides the built tx. */
export interface RecordOpts {
  maker: Hex;
  network: string;
  placedAtUnix: bigint | number | string;
  placedAtDaa: bigint | number | string;
  note?: string;
  label?: string;
}

/**
 * Placement records of the orders a create or cancel-replace tx creates. An unsigned tx is fine,
 * because the tx id does not cover signatures.
 */
export function recordsFromBuilt(kob: KobWasm, built: { tx: TxJson }, opts: RecordOpts): PlacementRecord[] {
  const out: PlacementRecord[] = [];
  for (const r of kob.recoverOrders(built.tx)) {
    const order = asOrderState(r.order);
    if (!order) continue;
    if (order.state.maker !== opts.maker) continue;
    out.push({
      version: 1, network: opts.network, maker: opts.maker, txid: r.transactionId, output: r.output, covenantId: r.covenantId, kind: order.kind,
      templateHash: pinnedHash(kob, order.kind), state: kob.encodeState(order), amount: amountOf(order),
      custody: r.custody ? { output: r.custody.output, value: r.custody.value, tokenState: r.custody.state } : null,
      ...(r.prefund ? { prefund: { output: r.prefund.output, value: r.prefund.value, tokenState: r.prefund.state } } : {}),
      value: r.value, deadline: r.deadline ?? null, placedAtUnix: String(opts.placedAtUnix), placedAtDaa: String(opts.placedAtDaa),
      ...(opts.note !== undefined ? { note: opts.note } : {}), ...(opts.label !== undefined ? { label: opts.label } : {}),
    });
  }
  return out;
}

/**
 * The maker's records of the orders an in-place amend continues (kob-wasm `recoverAmends` over the
 * built tx and its signing plans, so nothing is trusted).
 *
 * Each record takes the amended state, because a node resolution starts from it and the old state's
 * script no longer exists. It also takes the continuation's value and the record's deadline. The
 * custody did not move, so its part of the record stays. The amend creates no placement, so the
 * record gets `txid: null` like an imported record. An import re-checks its state and a resolution
 * its UTXO. Orders without a record are skipped.
 */
export async function amendedRecords(kob: KobWasm, built: { tx: TxJson; plans: SigPlan[] }, store: RecordStore, maker: Hex): Promise<PlacementRecord[]> {
  const out: PlacementRecord[] = [];
  for (const a of kob.recoverAmends(built.tx, built.plans)) {
    const order = asOrderState(a.order);
    const prev = await store.get(a.covenantId);
    if (!order || !prev || order.state.maker !== maker || prev.maker !== maker || prev.kind !== order.kind) continue;
    out.push({
      ...prev, txid: null, output: null, state: kob.encodeState(order), amount: amountOf(order), value: a.value, deadline: a.deadline ?? null,
      // The amended state is the current one. An older `last` would steer the node search to a
      // script that no longer exists, and the order was not cancelled, so a `cancelling` mark of
      // this tx does not apply.
      last: null, cancelling: null,
    });
  }
  return out;
}

/**
 * The maker's records of the orders a sweep in place continues. The SWEEP records are re-verified
 * by sweep.ts against the built tx and its signing plans.
 *
 * A sweep does not end the order. Its record stays, with identity, placement and custody part
 * unchanged, and only its last proven state moves to the continuation. That is the same state at a
 * new outpoint, with `daa` set to when the sweep was accepted, so a node resolution starts there. A
 * `cancelling` mark of this tx does not apply, because the order was not cancelled. Orders without
 * a record are skipped.
 */
export async function sweptRecords(kob: KobWasm, built: { tx: TxJson; plans: SigPlan[] }, store: RecordStore, maker: Hex, daa: U64 | number | bigint): Promise<PlacementRecord[]> {
  const out: PlacementRecord[] = [];
  for (const s of recoverSweeps(kob, built.tx, built.plans).sweeps) {
    const prev = await store.get(s.covenantId);
    if (!prev || prev.maker !== maker || s.order.state.maker !== maker || prev.kind !== s.order.kind) continue;
    const next = withLastState(kob, prev, s.order, { txid: built.tx.id, index: s.output, daa });
    out.push(prev.cancelling?.txid === built.tx.id ? { ...next, cancelling: null } : next);
  }
  return out;
}

// ------------------------------------------------------------------------- indexer recovery format

/** The indexer's maker-recovery file. */
export interface IndexerRecovery {
  /**
   * Version 2 entries carry the custody token's extension commitment (docs/ops/executor.md, section
   * 7.5). Version 1 files carry none and are still imported.
   */
  version: 1 | 2;
  network: string;
  orders: { template_hash: Hex; state: Hex; covenant_id: Hex; extension_commitment?: Hex }[];
}
/** The records an import accepted and the entries it rejected, by index with the reason. */
export interface ImportResult { records: PlacementRecord[]; rejected: { index: number; reason: string }[] }
/** The maker and network an import must match. */
export interface ImportOpts { maker: Hex; network: string }

/**
 * Extension commitment of a record's token, when the record knows it. It is its own field, else the
 * placed custody state's.
 */
export const recordExt = (r: PlacementRecord): Hex | null => r.ext ?? (r.custody?.tokenState ? extensionOfState(r.custody.tokenState) : null);

/**
 * The maker's order receipt in the indexer recovery format, version 2 (docs/ops/executor.md,
 * section 7.5).
 *
 * The state is the current script when the record knows a proven later state (`last`), else the
 * placed one. Ask-side entries carry the custody token's extension commitment, so that the custody
 * can be rebuilt and verified. Without it an importer can find the order but not its custody, and a
 * cancel cannot be built.
 */
export function exportIndexerRecovery(records: PlacementRecord[], network: string): IndexerRecovery {
  return {
    version: 2, network,
    orders: records.filter((r) => r.network === network).map((r) => {
      const ext = recordExt(r);
      return {
        template_hash: r.templateHash, state: r.last?.state ?? r.state, covenant_id: r.covenantId,
        ...(ext ? { extension_commitment: ext } : {}),
      };
    }),
  };
}

/**
 * Semantic checks shared by the indexer import and record-only backup entries. Returns the decoded
 * order or a rejection reason.
 */
function checkOrderEntry(kob: KobWasm, templateHash: unknown, state: unknown, covenantId: unknown, maker: Hex | undefined): { order: OrderState; kind: RecordKind } | string {
  if (!isHex(templateHash, 32)) return 'template_hash must be 64 lowercase hex characters';
  if (!isHex(state) || state.length === 0) return 'state must be hex';
  if (!isHex(covenantId, 32)) return 'covenant_id must be 64 lowercase hex characters';
  const tpl = kob.templates().find((t) => t.hash === templateHash);
  if (!tpl || !isOrderKind(tpl.name)) return 'template_hash is not a pinned KOB order template';
  let order: OrderState;
  try {
    order = kob.decodeState(tpl.name, state) as OrderState;
    if (order.kind !== tpl.name || kob.encodeState(order) !== state) return 'state is not canonical for its template';
  } catch (e) {
    return `state does not decode: ${e instanceof Error ? e.message : String(e)}`;
  }
  if (maker !== undefined && order.state.maker !== maker) return 'the order belongs to another maker';
  return { order, kind: tpl.name };
}

function recordFromEntry(kob: KobWasm, o: OrderState, kind: RecordKind, base: Pick<PlacementRecord, 'network' | 'covenantId' | 'templateHash' | 'state'>): PlacementRecord {
  return {
    version: 1, ...base, maker: o.state.maker, txid: null, output: null, kind, amount: amountOf(o), custody: null, value: '0', deadline: null, placedAtUnix: '0', placedAtDaa: '0',
  };
}

/** Imports an indexer recovery file, re-validating every entry with kob-wasm. */
export function importIndexerRecovery(json: unknown, kob: KobWasm, opts: ImportOpts): ImportResult {
  const res: ImportResult = { records: [], rejected: [] };
  if (!isObj(json) || (json.version !== 1 && json.version !== 2) || !Array.isArray(json.orders)) return { records: [], rejected: [{ index: -1, reason: 'not a version 1 or 2 recovery file' }] };
  if (json.network !== opts.network) return { records: [], rejected: [{ index: -1, reason: `file is for network ${String(json.network)}, not ${opts.network}` }] };
  json.orders.forEach((e: unknown, index: number) => {
    if (!isObj(e)) return void res.rejected.push({ index, reason: 'entry is not an object' });
    const c = checkOrderEntry(kob, e.template_hash, e.state, e.covenant_id, opts.maker);
    if (typeof c === 'string') return void res.rejected.push({ index, reason: c });

    const ext = e.extension_commitment;
    if (ext !== undefined && ext !== null && !isHex(ext, 32)) return void res.rejected.push({ index, reason: 'extension_commitment must be 64 lowercase hex characters' });
    const rec = recordFromEntry(kob, c.order, c.kind, { network: opts.network, covenantId: e.covenant_id as Hex, templateHash: e.template_hash as Hex, state: e.state as Hex });

    // The custody state of an ask is rebuilt from this, because the state does not carry it.
    // Without it the order cannot be cancelled from the node.
    const known = typeof ext === 'string' ? ext : extensionOf(c.order);
    if (known) rec.ext = known;
    res.records.push(rec);
  });
  return res;
}

// ------------------------------------------------------------------------------------- full backup

/** A KOB backup file: records and the txs that placed them. */
export interface KobBackup {
  format: 'kob-backup';
  version: 1;
  network: string;
  exportedAt: string;
  records: PlacementRecord[];
  /** Placing txs by tx id, as safe JSON, so every record can be re-derived on import. */
  txs: Record<Hex, TxJson>;
}

/** Builds a backup of the records of one network, with the placing txs they name. */
export function exportBackup(records: PlacementRecord[], txs: Record<Hex, TxJson>, network: string, now: Date | number = new Date()): KobBackup {
  const rs = records.filter((r) => r.network === network);
  const kept: Record<Hex, TxJson> = {};
  for (const r of rs) if (r.txid && txs[r.txid]) kept[r.txid] = txs[r.txid];
  return { format: 'kob-backup', version: 1, network, exportedAt: new Date(now).toISOString(), records: rs, txs: kept };
}

/**
 * Imports a KOB backup. A record with its placed tx is re-derived from that tx. Its state, covenant
 * id, output, kind and maker must agree with what the file claims, and its custody, value and
 * deadline are taken from the tx. A record without a tx must pass the indexer-import checks.
 */
export function importBackup(json: unknown, kob: KobWasm, opts: { network: string; maker?: Hex }): ImportResult {
  if (!isObj(json) || json.format !== 'kob-backup' || json.version !== 1 || !Array.isArray(json.records) || !isObj(json.txs)) {
    return { records: [], rejected: [{ index: -1, reason: 'not a kob-backup version 1 file' }] };
  }
  if (json.network !== opts.network) return { records: [], rejected: [{ index: -1, reason: `backup is for network ${String(json.network)}, not ${opts.network}` }] };
  const txs = json.txs as Record<string, TxJson>;
  const res: ImportResult = { records: [], rejected: [] };
  json.records.forEach((raw: unknown, index: number) => {
    const reject = (reason: string) => void res.rejected.push({ index, reason });
    if (!isPlacementRecord(raw)) return reject('malformed record');
    // A last-seen state that is not a canonical state of the same kind and maker is dropped. It
    // only steers the node search and never a spend.
    const rec: PlacementRecord = raw.last && !lastStateOf(kob, raw) ? { ...raw, last: null } : raw;
    if (rec.network !== opts.network) return reject('record is for another network');
    if (opts.maker !== undefined && rec.maker !== opts.maker) return reject('record belongs to another maker');
    const c = checkOrderEntry(kob, rec.templateHash, rec.state, rec.covenantId, rec.maker);
    if (typeof c === 'string') return reject(c);
    if (c.kind !== rec.kind) return reject('kind does not match the template hash');
    if (rec.txid === null) return void res.records.push(rec);
    const tx = txs[rec.txid];
    if (!tx) return reject('the placing tx is missing from the backup');
    if (tx.id !== rec.txid) return reject('tx id mismatch');
    let recovered;
    try {
      recovered = kob.recoverOrders(tx);
    } catch (e) {
      return reject(`placed tx does not verify: ${e instanceof Error ? e.message : String(e)}`);
    }
    const r = recovered.find((x) => x.covenantId === rec.covenantId);
    const order = r ? asOrderState(r.order) : null;
    if (!r || !order) return reject('the placed tx does not create this covenant id');
    if (order.kind !== rec.kind || kob.encodeState(order) !== rec.state) return reject('state does not match the placed tx');
    if (r.output !== rec.output) return reject('output index does not match the placed tx');
    res.records.push({
      ...rec, custody: r.custody ? { output: r.custody.output, value: r.custody.value, tokenState: r.custody.state } : null,
      ...(r.prefund ? { prefund: { output: r.prefund.output, value: r.prefund.value, tokenState: r.prefund.state } } : {}), value: r.value, deadline: r.deadline ?? null,
      amount: amountOf(order),
    });
  });
  return res;
}

// ------------------------------------------------------------------------------- resolve on a node

/** What a node says about a record. */
export interface ResolvedRecord {
  /**
   * 'old-template' means the record's template is neither pinned by this build nor kept for
   * cancels. It is never searched and never reported as spent.
   */
  status: 'live' | 'spent' | 'unknown' | 'old-template';
  covenantId: Hex;
  /** Live only: the current covenant UTXO with its re-derived state. */
  order: OrderUtxo<OrderState> | null;
  /**
   * Live token-holding orders: the custody token UTXO found on the node. A pair order gives the
   * first of its custodies.
   */
  custody: TokenUtxo | null;
  /** A live sell-first `KobIfdPair`: its B prefund custody found on the node. */
  prefund?: TokenUtxo | null;
  /** Always empty here, because strays are known only to the indexer. */
  strays: TokenUtxo[];
  stateChanged: boolean;
  /** Addresses asked of the node. */
  checked: number;
  /**
   * Live ask kinds without a custody: why. 'no-extension' means the token's extension commitment is
   * unknown, so its custody cannot be rebuilt.
   */
  custodyIssue?: 'no-extension' | 'not-found' | null;
  /**
   * The hash of the retired template of the cancel-only table the order is under. Only its maker's
   * cancel is ever built. Absent or null otherwise.
   */
  retired?: Hex | null;
}

/** Limits and hints for `resolveRecord`. */
export interface ResolveOpts {
  maxCandidates?: number;
  chunk?: number;
  hints?: NodeUtxo[];
}

interface Candidate { state: OrderState; spk: string }

/** Upper bound on the trailing-stop steps tried. */
const MAX_TRAIL_STEPS = 256n;

/** The template hash this build pins for `kind`, or null. */
export function pinnedTemplateHash(kob: KobWasm, kind: RecordKind): Hex | null {
  return kob.templates().find((x) => x.name === kind)?.hash ?? null;
}

/**
 * True when the record names an order template this build neither pins nor keeps in its cancel-only
 * table. This build can neither build for nor spend it. Its maker ends the order with the release
 * it was placed with, by a raw transaction that spends the order's own cancel entry.
 */
export const isOldTemplate = (kob: KobWasm, r: PlacementRecord): boolean => recordCodec(kob, r) === null;

const retiredCache = new WeakMap<KobWasm, RetiredTemplateInfo[]>();

/**
 * The cancel-only table of this build (kob-wasm `retiredTemplates`); empty when the module has
 * none.
 */
export function retiredTemplates(kob: KobWasm): RetiredTemplateInfo[] {
  let t = retiredCache.get(kob);
  if (!t) {
    try {
      t = typeof kob.retiredTemplates === 'function' ? kob.retiredTemplates() : [];
    } catch {
      t = [];
    }
    retiredCache.set(kob, t);
  }
  return t;
}

/**
 * The retired template (cancel-only table) a record names, which is its template hash under the
 * kind it was placed as. Null for a template this build pins and for one it does not know at all.
 */
export function retiredOf(kob: KobWasm, r: Pick<PlacementRecord, 'kind' | 'templateHash'>): RetiredTemplateInfo | null {
  return retiredTemplates(kob).find((x) => x.hash === r.templateHash && x.kind === r.kind) ?? null;
}

/**
 * How a record's state span maps to a state and a script under its template, pinned or a retired
 * one of the cancel-only table.
 */
export interface RecordCodec {
  /** State span of the record's template to its state. */
  decode(hex: Hex): OrderState;
  /** State to the state span of the record's template. Throws when it has no such span. */
  encode(state: OrderState): Hex;
  /** Script public key of the order with that state span. */
  spanSpk(hex: Hex): string;
  /**
   * The hash when the record's template is a retired one (cancel only). Null for a pinned template.
   */
  retired: Hex | null;
}

/**
 * The codec of a record's template, or null when this build neither pins it nor keeps it in its
 * cancel-only table. A retired template shares the state layout of today's kind of the same name
 * (kob-protocol `retired`), and only its script differs.
 */
export function recordCodec(kob: KobWasm, r: Pick<PlacementRecord, 'kind' | 'templateHash' | 'state'>): RecordCodec | null {
  const decode = (hex: Hex) => kob.decodeState(r.kind as OrderKind, hex) as OrderState;
  if (pinnedTemplateHash(kob, r.kind) === r.templateHash) {
    return { decode, encode: (st) => kob.encodeState(st), spanSpk: (hex) => kob.scriptPublicKey(decode(hex)), retired: null };
  }
  const ret = retiredOf(kob, r);
  if (!ret) return null;
  return { decode, encode: (st) => kob.encodeState(st), spanSpk: (hex) => kob.retiredScriptPublicKey(ret.hash, decode(hex)), retired: ret.hash };
}

/**
 * The record's last proven state, decoded. Null when absent or not a canonical state of the
 * record's kind and maker.
 */
export function lastStateOf(kob: KobWasm, r: PlacementRecord): OrderState | null {
  if (!r.last) return null;
  try {
    const codec = recordCodec(kob, r);
    if (!codec) return null;
    const st = codec.decode(r.last.state);
    return st.kind === r.kind && st.state.maker === r.maker && codec.encode(st) === r.last.state ? st : null;
  } catch {
    return null;
  }
}

/**
 * Candidate continuation states of `base`, excluding `base` itself, most likely first, at most
 * `max`. `truncated` is set when more exist.
 *
 * The dimensions are amount (amountLeft, joint with rptAmount on repeat entries), armed and
 * stopPrice (trailing stops only). The product is walked breadth first by the sum of the
 * per-dimension ranks, so small departures from `base` in every field come before large ones in one
 * field. Amounts walk every base unit while `amountLeft` fits the candidate budget. Otherwise they
 * walk in steps of the order's minimum fill, which is never exhaustive.
 */
function candidates(
  kob: KobWasm,
  base: OrderState,
  max: number,
  extraArmed: readonly string[],
  spkOf: (st: OrderState) => string = (st) => kob.scriptPublicKey(st),
): { list: Candidate[]; truncated: boolean; fields: string[]; jointCustody: boolean } {
  const fields = kob.mutableWindows(base.kind).map(([f]) => f);
  const s = base.state as unknown as Record<string, string>;
  // A pair ask (KobPair or KobCondPair, side 1) holds exactly amountLeft of A, so its custody walks
  // with the amount. Any other pair custody (a bid's B escrow, an entry's B) moves by the rounded B
  // of each fill and is left as recorded, which is never exhaustive.
  const pf = pairFactsOf(base);
  const jointCustody = fields.includes('custody') && !!pf && base.kind !== 'KobIfdPair' && pf.side === 'sell';
  const cap = max + 1;
  // Each dimension is a list of partial assignments, and index 0 holds the base values.
  const dims: { values: Record<string, string>[]; complete: boolean }[] = [];

  const hasAmount = fields.includes('amountLeft');
  const hasRpt = fields.includes('rptAmount');
  if (hasAmount || hasRpt) {
    const values: Record<string, string>[] = [];
    let complete = true;
    const amountOrig = hasAmount ? big(s.amountLeft) : 0n;
    const lo = isIfdKind(base.kind) ? 0n : 1n;
    // Every base unit while the amount fits the budget. Otherwise fills of exactly the minimum
    // fill, which is a guess and never exhaustive.
    const minFill = s.minFill !== undefined && big(s.minFill) > 0n ? big(s.minFill) : 1n;
    const step = amountOrig <= BigInt(cap) ? 1n : minFill;
    if (step > 1n) complete = false;
    if (hasAmount && hasRpt) {
      // An if-done entry has f base units filled. For a repeat, rpt = rptOrig - f, never below 0.
      // Of the filled units, m were re-armed by exits that sold out while repeats remained, so
      // amount = amountOrig - f + m. A non-repeating entry (rptOrig = 0) only fills.
      const rptOrig = big(s.rptAmount);
      const seenPair = new Set<string>();
      outer: for (let f = 0n; ; f += step) {
        const rpt = f <= rptOrig ? rptOrig - f : 0n;
        const mMax = f < rptOrig ? f : rptOrig;
        let any = false;
        for (let m = mMax; m >= 0n; m -= step) {
          const amount = amountOrig - f + m;
          if (amount < lo) break;
          any = true;
          const key = `${amount}:${rpt}`;
          if (seenPair.has(key)) continue;
          if (values.length >= cap) {
            complete = false;
            break outer;
          }
          seenPair.add(key);
          values.push({ amountLeft: amount.toString(), rptAmount: rpt.toString() });
        }
        if (!any && f > rptOrig) break;
      }
    } else if (hasAmount) {
      for (let v = amountOrig; v >= lo; v -= step) {
        if (values.length >= cap) {
          complete = false;
          break;
        }
        values.push({ amountLeft: v.toString() });
      }
    } else {
      for (let v = big(s.rptAmount); v >= 0n; v -= step) {
        if (values.length >= cap) {
          complete = false;
          break;
        }
        values.push({ rptAmount: v.toString() });
      }
    }
    if (values.length === 0) values.push({});
    dims.push({ values, complete });
  }
  if (fields.includes('armed')) {
    // `armed` may hold the DAA of whichever UTXO armed the stop, so the set is a guess and never
    // exhaustive.
    const values = [...new Set([s.armed, '0', '1', ...extraArmed.filter((v) => /^[0-9]+$/.test(v))])].map((armed) => ({ armed }));
    dims.push({ values, complete: false });
  }
  const step = s.trailStep !== undefined ? big(s.trailStep) : 0n;
  if (fields.includes('stopPrice') && step > 0n) {
    // A trailing stop moves in whole steps. A sell stop moves up, staying below its take-profit. A
    // buy stop moves down, not below 0.
    const stop = big(s.stopPrice);
    const sell = sideOf(base) === 'sell';
    const tp = s.tpPrice !== undefined ? big(s.tpPrice) : s.price !== undefined ? big(s.price) : 0n;
    let kMax = sell ? (tp > stop ? (tp - 1n - stop) / step : MAX_TRAIL_STEPS) : stop / step;
    if (kMax > MAX_TRAIL_STEPS) kMax = MAX_TRAIL_STEPS;
    const values: Record<string, string>[] = [];
    for (let k = 0n; k <= kMax; k++) values.push({ stopPrice: (sell ? stop + k * step : stop - k * step).toString() });
    dims.push({ values, complete: false });
  }

  const list: Candidate[] = [];
  const seen = new Set<string>([spkOf(base)]);
  let truncated = dims.some((d) => !d.complete) || (fields.includes('custody') && !jointCustody);
  // Breadth first over the rank sum. Each tuple is visited exactly once.
  const maxSum = dims.reduce((a, d) => a + d.values.length - 1, 0);
  const idx = dims.map(() => 0);
  const walk = (k: number, left: number): boolean => {
    if (k === dims.length) {
      if (left !== 0) return true;
      const st = { ...s };
      dims.forEach((d, i) => Object.assign(st, d.values[idx[i]]));
      if (jointCustody && st.amountLeft !== undefined) st.custody = st.amountLeft;
      const cand = { kind: base.kind, state: st } as unknown as OrderState;
      let spk: string;
      try {
        spk = spkOf(cand);
      } catch {
        return true; // a state the order's template cannot express
      }
      if (!seen.has(spk)) {
        seen.add(spk);
        list.push({ state: cand, spk });
      }
      return list.length < max;
    }
    // The remaining dimensions must be able to absorb what is left.
    const rest = dims.slice(k + 1).reduce((a, d) => a + d.values.length - 1, 0);
    const lo = Math.max(0, left - rest);
    const hi = Math.min(left, dims[k].values.length - 1);
    for (let i = lo; i <= hi; i++) {
      idx[k] = i;
      if (!walk(k + 1, left - i)) return false;
    }
    idx[k] = 0;
    return true;
  };
  let sum = 1;
  for (; sum <= maxSum; sum++) if (!walk(0, sum)) break;
  if (sum <= maxSum) truncated = true;
  return { list, truncated, fields, jointCustody };
}

function tokenProgramOf(kob: KobWasm, tplHash: Hex): TokenProgram | null {
  const t = kob.templates().find((x) => x.hash === tplHash && x.tokenSlots);
  return t ? (t.name as TokenProgram) : null;
}

/**
 * Asks a node what became of a recorded order and returns its current state and custody, 'spent' or
 * 'unknown'. The search is described at the top of this file.
 */
export async function resolveRecord(
  kob: KobWasm,
  node: Pick<NodeApi, 'getUtxosByAddresses'>,
  record: PlacementRecord,
  spkToAddress: (spk: string) => string,
  opts: ResolveOpts = {},
): Promise<ResolvedRecord> {
  const max = Math.max(opts.maxCandidates ?? 2000, 0);
  const chunk = Math.max(opts.chunk ?? 100, 1);
  let checked = 0;
  const ask = async (spks: string[]): Promise<NodeUtxo[]> => {
    checked += spks.length;
    return node.getUtxosByAddresses(spks.map(spkToAddress));
  };
  const result = (
    status: ResolvedRecord['status'],
    order: OrderUtxo<OrderState> | null,
    custody: TokenUtxo | null,
    changed: boolean,
    custodyIssue: ResolvedRecord['custodyIssue'] = null,
  ): ResolvedRecord => ({ status, covenantId: record.covenantId, order, custody, strays: [], stateChanged: changed, checked, custodyIssue, ...(codec?.retired ? { retired: codec.retired } : {}) });
  // 0. A template this build neither pins nor keeps for cancels. Its script cannot be derived, so
  //    it would never be found and would look spent. A retired template of the cancel-only table is
  //    searched under its own script.
  const codec = recordCodec(kob, record);
  if (!codec) return result('old-template', null, null, false);
  const spkOf = (st: OrderState): string => codec.spanSpk(codec.encode(st));

  const base = codec.decode(record.state);
  const baseSpk = codec.spanSpk(record.state);
  const last = lastStateOf(kob, record);
  const asOrder = (u: NodeUtxo, state: OrderState): OrderUtxo<OrderState> => ({
    transactionId: u.transactionId, index: u.index, amount: u.amount, blockDaaScore: u.blockDaaScore, covenantId: record.covenantId, state,
  });
  const live = async (u: NodeUtxo, state: OrderState, changed: boolean): Promise<ResolvedRecord> => {
    const c = await findCustody(kob, ask, record, state);
    return { ...result('live', asOrder(u, state), c.utxo, changed, c.issue), ...(c.prefund ? { prefund: c.prefund } : {}) };
  };

  // 1. The placed state and the last proven one.
  const known = new Map<string, OrderState>([[baseSpk, base]]);
  if (last) known.set(spkOf(last), last);
  const direct = (await ask([...known.keys()])).find((u) => u.covenantId === record.covenantId && known.has(u.scriptPublicKey));
  if (direct) return live(direct, known.get(direct.scriptPublicKey)!, direct.scriptPublicKey !== baseSpk);

  // 2. Candidate continuations of the last proven state, else the placed one. The DAA of the last
  //    seen UTXO may be an `armed` value.
  const extraArmed = [record.last?.daa, record.placedAtDaa].filter((v): v is string => typeof v === 'string' && v !== '0');
  const { list, truncated, fields, jointCustody } = candidates(kob, last ?? base, max, extraArmed, spkOf);
  const bySpk = new Map(list.map((c) => [c.spk, c.state]));
  for (const [spk, st] of known) bySpk.set(spk, st);

  // 3. Hints.
  for (const h of opts.hints ?? []) {
    if (h.covenantId !== record.covenantId) continue;
    const st = bySpk.get(h.scriptPublicKey);
    if (st) return live(h, st, h.scriptPublicKey !== baseSpk);
  }

  for (let i = 0; i < list.length; i += chunk) {
    const part = list.slice(i, i + chunk);
    const found = (await ask(part.map((c) => c.spk))).find((u) => u.covenantId === record.covenantId && bySpk.has(u.scriptPublicKey));
    if (found) return live(found, bySpk.get(found.scriptPublicKey)!, true);
  }
  const exhaustive = !truncated && fields.every((f) => f === 'amountLeft' || (f === 'custody' && jointCustody));
  return result(exhaustive ? 'spent' : 'unknown', null, null, false);
}

/**
 * The custody token UTXOs of a live order, looked up at the address of the custody state its state
 * implies. There is one lookup per custody of kob-protocol `custodies`. A pair order has one of its
 * own token, A or B, on that token's program. A sell-first entry has A, then its B prefund. The
 * extension commitment is the one the record knows (the placed custody's) or the state names.
 */
async function findCustody(
  kob: KobWasm,
  ask: (spks: string[]) => Promise<NodeUtxo[]>,
  record: PlacementRecord,
  state: OrderState,
): Promise<{ utxo: TokenUtxo | null; prefund: TokenUtxo | null; issue: ResolvedRecord['custodyIssue'] }> {
  const cs = custodiesOf(state).filter((c, k) => c.amount > 0n || k === 0);
  if (!cs.length || cs[0]!.amount <= 0n) return { utxo: null, prefund: null, issue: null };
  const pf = pairFactsOf(state);
  const one = async (k: number): Promise<{ utxo: TokenUtxo | null; issue: ResolvedRecord['custodyIssue'] }> => {
    const c = cs[k]!;
    const tok = pf ? (c.token === pf.a.covId ? pf.a : pf.b) : null;
    const program = tokenProgramOf(kob, tok ? tok.tplHash : tokenTplHashOf(state));
    const family = tok ? (tok.family ?? 'kcc20') : familyOfOrder(state);
    const placed = k === 0 ? record.custody?.tokenState : record.prefund?.tokenState;
    const ext = (k === 0 ? recordExt(record) : null) ?? (placed ? extensionOfState(placed) : null) ?? pairExtFor(state, c.token) ?? (k === 0 && !pf ? extensionOf(state) : null);
    if (!program) return { utxo: null, issue: 'not-found' };
    if (family === 'kcc20' && !ext) return { utxo: null, issue: 'no-extension' };
    const st = custodyState(family, c.amount, record.covenantId, ext);
    const spk = kob.tokenScriptPublicKey(program, st);
    const u = (await ask([spk])).find((x) => x.covenantId === c.token && x.scriptPublicKey === spk);
    if (!u) return { utxo: null, issue: 'not-found' };
    return { utxo: { transactionId: u.transactionId, index: u.index, amount: u.amount, blockDaaScore: u.blockDaaScore, covenantId: u.covenantId, state: st }, issue: null };
  };
  const first = await one(0);
  const second = cs.length > 1 ? await one(1) : { utxo: null, issue: null };
  return { utxo: first.utxo, prefund: second.utxo, issue: first.issue ?? second.issue };
}

// ------------------------------------------------------------------------- keeping records current

/**
 * The record with its last proven state set to `state`, seen at the UTXO `at` (txid, index, daa).
 * Returns the same object when nothing changed.
 */
export function withLastState(kob: KobWasm, record: PlacementRecord, state: OrderState, at: { txid: Hex; index: number; daa: U64 | number | bigint }): PlacementRecord {
  if (state.kind !== record.kind || state.state.maker !== record.maker) return record;
  const codec = recordCodec(kob, record);
  let hex: Hex;
  try {
    if (!codec) return record;
    hex = codec.encode(state);
  } catch {
    return record;
  }
  if (record.last && record.last.state === hex && record.last.txid === at.txid && record.last.index === at.index) return record;
  return { ...record, last: { state: hex, txid: at.txid, index: at.index, daa: String(at.daa) } };
}

/** Refreshes a record from a node resolution. Only a live one changes it. */
export function refreshFromResolved(kob: KobWasm, record: PlacementRecord, r: ResolvedRecord): PlacementRecord {
  if (r.status !== 'live' || !r.order) return record;
  return withLastState(kob, record, r.order.state, { txid: r.order.transactionId, index: r.order.index, daa: r.order.blockDaaScore ?? '0' });
}

/**
 * Refreshes a record from an indexer view whose current state is proven, and fills in the extension
 * commitment the view serves.
 */
export function refreshFromView(kob: KobWasm, record: PlacementRecord, v: OrderView): PlacementRecord {
  let out = record;
  if (!recordExt(out) && v.extension_commitment && isHex(v.extension_commitment, 32)) out = { ...out, ext: v.extension_commitment };
  const st = v.state_known ? asOrderState(v.state) : null;
  if (!st || !v.current) return out;
  return withLastState(kob, out, st, { txid: v.current.txid, index: v.current.index, daa: v.current_daa ?? v.last_daa ?? 0 });
}

/**
 * A record for an own live order that the indexer shows but the wallet has no record of. Examples
 * are if-done exits, which their entry's fill creates, and orders placed from another device. The
 * wallet can then still find and cancel the order when the indexer is gone. Null when the view
 * cannot back a record, which is when its state is not proven, it has another maker or it has a
 * template this build does not pin.
 */
export function recordFromView(kob: KobWasm, v: OrderView, maker: Hex, network: string): PlacementRecord | null {
  const st = v.state_known ? asOrderState(v.state) : null;
  if (!st || st.state.maker !== maker || v.maker !== maker || !v.current) return null;
  if (pinnedTemplateHash(kob, st.kind) !== v.template_hash) return null;
  const hex = kob.encodeState(st);
  const ext = v.extension_commitment && isHex(v.extension_commitment, 32) ? v.extension_commitment : extensionOf(st);
  return {
    version: 1, network, maker, txid: v.genesis.txid, output: v.genesis.out, covenantId: v.covenant_id, kind: st.kind, templateHash: v.template_hash,
    state: hex, amount: v.initial_amount ?? amountOf(st), custody: null, value: v.current.value ?? '0', deadline: v.deadline != null ? String(v.deadline) : null,
    placedAtUnix: '0', placedAtDaa: String(v.genesis.daa), ext: ext ?? null,
    last: { state: hex, txid: v.current.txid, index: v.current.index, daa: String(v.current_daa ?? v.last_daa ?? 0) }, origin: 'indexer',
  };
}
