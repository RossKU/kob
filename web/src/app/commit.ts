// What the app does after the node accepts a transaction. It remembers placement records, so
// orders can be recovered without the indexer. It tracks the wallet's new token UTXOs, such as
// change and issuance outputs, which wallets do not show. It drops what the transaction spent.
import type { RecordStore, PlacementRecord } from '../kob/records';
import { amendedRecords, recordsFromBuilt, sweptRecords } from '../kob/records';
import type { BuiltTx, Hex } from '../kob/types';
import type { Clock } from '../kob/plan-types';
import type { Services } from './services';

/** What `commitAccepted` needs to know about an accepted transaction. */
export interface CommitArgs {
  services: Pick<Services, 'kob' | 'tracker' | 'config'>;
  records: RecordStore;
  built: BuiltTx;
  maker: Hex;
  txid: Hex;
  clock: Clock;
  label?: string;
}

/**
 * Remembers an accepted transaction. Storage problems never turn it into an error. Returns the
 * records saved.
 */
export async function commitAccepted(a: CommitArgs): Promise<PlacementRecord[]> {
  const saved: PlacementRecord[] = [];
  try {
    const recs = recordsFromBuilt(a.services.kob, a.built, {
      maker: a.maker, network: a.services.config.network, placedAtUnix: a.clock.unixSeconds, placedAtDaa: a.clock.daa, label: a.label,
    });
    for (const r of recs) {
      await a.records.put(r);
      saved.push(r);
    }
  } catch {
    /* recordsFromBuilt throws for transactions without order records (cancels). */
  }
  try {
    // An in-place amend. The order's record takes its new state. Its covenant id and custody stay.
    for (const r of await amendedRecords(a.services.kob, a.built, a.records, a.maker)) {
      await a.records.put(r);
      saved.push(r);
    }
  } catch {
    /* Best effort, like the placement records. */
  }
  try {
    // A sweep in place. The order lives on with the same state at a new outpoint, so its record
    // stays and points at the continuation.
    for (const r of await sweptRecords(a.services.kob, a.built, a.records, a.maker, a.clock.daa)) {
      await a.records.put(r);
      saved.push(r);
    }
  } catch {
    /* Best effort, like the placement records. */
  }
  try {
    a.services.tracker.trackFromBuilt(a.services.kob, a.built, a.maker, a.txid);
  } catch {
    /* Tracking is a cache. The indexer or a re-import can rebuild it. */
  }
  return saved;
}
