// What the app does AFTER a transaction was accepted by the node: remember placement records (recovery without the indexer), track the
// wallet's new token UTXOs (change / issuance outputs, which wallets do not show), and drop what the transaction spent.
import type { RecordStore, PlacementRecord } from '../kob/records';
import { amendedRecords, recordsFromBuilt, sweptRecords } from '../kob/records';
import type { BuiltTx, Hex } from '../kob/types';
import type { Clock } from '../kob/plan-types';
import type { Services } from './services';

export interface CommitArgs {
  services: Pick<Services, 'kob' | 'tracker' | 'config'>;
  records: RecordStore;
  built: BuiltTx;
  maker: Hex;
  txid: Hex;
  clock: Clock;
  label?: string;
}

/** Best effort: storage problems never turn an accepted transaction into an error. Returns the records saved. */
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
    /* recordsFromBuilt throws for txs without order records (cancels): nothing to remember */
  }
  try {
    // an in-place amend: the order's record takes its new state (its covenant id and custody did not change)
    for (const r of await amendedRecords(a.services.kob, a.built, a.records, a.maker)) {
      await a.records.put(r);
      saved.push(r);
    }
  } catch {
    /* best effort, like the placement records */
  }
  try {
    // a sweep in place: the order lives on (same state, new outpoint), its record stays and points at the continuation
    for (const r of await sweptRecords(a.services.kob, a.built, a.records, a.maker, a.clock.daa)) {
      await a.records.put(r);
      saved.push(r);
    }
  } catch {
    /* best effort, like the placement records */
  }
  try {
    a.services.tracker.trackFromBuilt(a.services.kob, a.built, a.maker, a.txid);
  } catch {
    /* tracking is a cache: the indexer / a re-import can rebuild it */
  }
  return saved;
}
