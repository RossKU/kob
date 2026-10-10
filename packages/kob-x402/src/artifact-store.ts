// Durable record of signed payment artifacts. The client saves the signed artifact BEFORE it discloses it (the
// paid retry), so a crash between signing and the response can never lose a payment that a merchant may already
// hold: after a restart the caller lists the records, reconciles them (was the transaction accepted?) or revokes.

import { mkdir, open, readFile, readdir, rename, unlink } from 'node:fs/promises';
import { join } from 'node:path';
import { randomBytes } from 'node:crypto';
import { KobX402Error } from './errors.ts';
import type { OfferKind, Outpoint, PaymentPayload } from './types.ts';

export type ArtifactStatus =
  /** Signed and stored, not yet sent. */
  | 'signed'
  /** Sent; the outcome is unknown (network error, missing or invalid response): the merchant may hold a valid payment. */
  | 'pending'
  /** The merchant returned a valid settlement for exactly this transaction. */
  | 'settled'
  /** The merchant reported a failure (see `failure`); the transaction may still have been broadcast. */
  | 'rejected'
  /** The payer spent one of the inputs back to itself. */
  | 'revoked'
  /**
   * An earlier attempt of a payment whose later attempt settled: the later one spent the payment's anchor (an input every
   * attempt spends), so this one can never be accepted.
   */
  | 'superseded';

export interface ArtifactRecord {
  paymentId: string;
  createdAtMs: number;
  updatedAtMs: number;
  status: ArtifactStatus;
  url: string;
  method: string;
  requestHash: string;
  transactionId: string;
  kind: OfferKind;
  amount: string;
  asset: string;
  network: string;
  expiresAtMs: number;
  consumed: Outpoint[];
  /** The complete signed artifact (contains the signed transaction). */
  paymentPayload: PaymentPayload;
  failure?: { diagnostic?: string; retryable?: boolean; message?: string; details?: unknown };
  revokeTransactionId?: string;
  note?: string;
  /** Attempt number of the payment (1 = the first signed one; a retry that rebuilt the payment counts up). */
  attempt?: number;
  /** The payment id of the attempt this one replaces (a rebuild after that one failed). */
  replaces?: string;
  /** `txid:index` of the payment's anchor: an input of the first attempt the payer owns, spent by every attempt. */
  anchor?: string;
}

export interface ArtifactStore {
  /** Must resolve only once the record is durable. A rejection aborts the payment before disclosure. */
  save(record: ArtifactRecord): Promise<void>;
  /** Merges `patch` into the record (and bumps `updatedAtMs`). */
  update(paymentId: string, patch: Partial<Omit<ArtifactRecord, 'paymentId'>>): Promise<ArtifactRecord>;
  load(paymentId: string): Promise<ArtifactRecord | undefined>;
  list(): Promise<ArtifactRecord[]>;
}

const ID_RE = /^[A-Za-z0-9_-]{16,128}$/;

function assertId(id: string): void {
  if (!ID_RE.test(id)) throw new KobX402Error('artifact_store', 'payment id must match ^[A-Za-z0-9_-]{16,128}$');
}

/**
 * A payment id names one signed transaction. Saving another transaction under an id that already holds an artifact would
 * destroy the only record of a payment the merchant may hold, so it is refused; updates of the same transaction pass.
 */
function assertNotOverwriting(existing: ArtifactRecord | undefined, next: ArtifactRecord): void {
  if (existing && existing.transactionId !== next.transactionId) {
    throw new KobX402Error(
      'artifact_store',
      `payment id ${next.paymentId} already holds the artifact of transaction ${existing.transactionId}: it is not overwritten with ${next.transactionId}`,
      { paymentId: next.paymentId, transactionId: existing.transactionId },
    );
  }
}

export class MemoryArtifactStore implements ArtifactStore {
  #records = new Map<string, ArtifactRecord>();

  async save(record: ArtifactRecord): Promise<void> {
    assertId(record.paymentId);
    assertNotOverwriting(this.#records.get(record.paymentId), record);
    this.#records.set(record.paymentId, structuredClone(record));
  }

  async update(paymentId: string, patch: Partial<Omit<ArtifactRecord, 'paymentId'>>): Promise<ArtifactRecord> {
    const cur = this.#records.get(paymentId);
    if (!cur) throw new KobX402Error('artifact_store', `no artifact ${paymentId}`);
    const next: ArtifactRecord = { ...cur, ...structuredClone(patch), updatedAtMs: Date.now() };
    this.#records.set(paymentId, next);
    return structuredClone(next);
  }

  async load(paymentId: string): Promise<ArtifactRecord | undefined> {
    const r = this.#records.get(paymentId);
    return r && structuredClone(r);
  }

  async list(): Promise<ArtifactRecord[]> {
    return [...this.#records.values()].map((r) => structuredClone(r)).sort((a, b) => a.createdAtMs - b.createdAtMs);
  }
}

/** One JSON file per payment id under `dir` (mode 0600), written to a temp file, fsynced and renamed into place. */
export class FileArtifactStore implements ArtifactStore {
  #dir: string;
  #ready: Promise<unknown> | undefined;

  constructor(dir: string) {
    this.#dir = dir;
  }

  #path(id: string): string {
    assertId(id);
    return join(this.#dir, `${id}.json`);
  }

  async #write(record: ArtifactRecord): Promise<void> {
    this.#ready ??= mkdir(this.#dir, { recursive: true });
    await this.#ready;
    const path = this.#path(record.paymentId);
    const tmp = `${path}.${randomBytes(6).toString('hex')}.tmp`;
    const fh = await open(tmp, 'w', 0o600);
    try {
      await fh.writeFile(JSON.stringify(record, null, 2), 'utf8');
      await fh.sync();
    } finally {
      await fh.close();
    }
    try {
      await rename(tmp, path);
    } catch (e) {
      await unlink(tmp).catch(() => {});
      throw e;
    }
  }

  async save(record: ArtifactRecord): Promise<void> {
    assertNotOverwriting(await this.load(record.paymentId), record);
    try {
      await this.#write(record);
    } catch (e) {
      throw new KobX402Error('artifact_store', 'cannot persist the payment artifact', { cause: e });
    }
  }

  async update(paymentId: string, patch: Partial<Omit<ArtifactRecord, 'paymentId'>>): Promise<ArtifactRecord> {
    const cur = await this.load(paymentId);
    if (!cur) throw new KobX402Error('artifact_store', `no artifact ${paymentId}`);
    const next: ArtifactRecord = { ...cur, ...patch, updatedAtMs: Date.now() };
    await this.save(next);
    return next;
  }

  async load(paymentId: string): Promise<ArtifactRecord | undefined> {
    try {
      return JSON.parse(await readFile(this.#path(paymentId), 'utf8')) as ArtifactRecord;
    } catch (e) {
      if ((e as NodeJS.ErrnoException).code === 'ENOENT') return undefined;
      throw new KobX402Error('artifact_store', 'cannot read the payment artifact', { cause: e });
    }
  }

  async list(): Promise<ArtifactRecord[]> {
    let names: string[];
    try {
      names = await readdir(this.#dir);
    } catch (e) {
      if ((e as NodeJS.ErrnoException).code === 'ENOENT') return [];
      throw new KobX402Error('artifact_store', 'cannot list the artifact directory', { cause: e });
    }
    const out: ArtifactRecord[] = [];
    for (const n of names) {
      if (!n.endsWith('.json')) continue;
      out.push(JSON.parse(await readFile(join(this.#dir, n), 'utf8')) as ArtifactRecord);
    }
    return out.sort((a, b) => a.createdAtMs - b.createdAtMs);
  }
}
