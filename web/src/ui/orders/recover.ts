// Recover: export / import of the maker's placement records (the fallback that keeps cancel working when the indexer disappears).
// Two formats, both re-validated with kob-wasm on import (kob/records.ts): the indexer's maker-recovery file and the fuller KOB backup.
//
// The placing transactions are not stored by the app (records only), so the KOB backup written here carries records WITHOUT their tx
// (`txid` / `output` null): importBackup then applies the same semantic checks as for an indexer file (template, canonical state, maker).
import {
  exportBackup, exportIndexerRecovery, importBackup, importIndexerRecovery, type ImportResult, type PlacementRecord, type RecordStore,
} from '../../kob/records';
import type { Hex } from '../../kob/types';
import type { KobWasm } from '../../kob/wasm';
import { jsonText } from '../kit/download';

export type ImportKind = 'kob-backup' | 'indexer-recovery';

/** The indexer's maker-recovery format (docs/ops/indexer.md 7.5) as JSON text. */
export function recoveryFileText(records: readonly PlacementRecord[], network: string): string {
  return jsonText(exportIndexerRecovery([...records], network));
}

/** KOB backup text: every record of the network, without placing txs (see file header). */
export function backupFileText(records: readonly PlacementRecord[], network: string, now: Date | number = new Date()): string {
  const bare = records.map((r) => ({ ...r, txid: null, output: null }));
  return jsonText(exportBackup(bare, {}, network, now));
}

const isObj = (v: unknown): v is Record<string, unknown> => typeof v === 'object' && v !== null && !Array.isArray(v);

/** Which format a parsed file is (by its marker fields), or null. */
export function detectImportKind(json: unknown): ImportKind | null {
  if (!isObj(json)) return null;
  if (json.format === 'kob-backup') return 'kob-backup';
  if ((json.version === 1 || json.version === 2) && Array.isArray(json.orders)) return 'indexer-recovery';
  return null;
}

export type ParsedImport = { ok: true; kind: ImportKind; result: ImportResult } | { ok: false; reason: 'not-json' | 'unknown-format' };

/** Parses and validates an import file for one maker on one network. Nothing is stored here. */
export function parseImport(text: string, kob: KobWasm, o: { network: string; maker: Hex }): ParsedImport {
  let json: unknown;
  try {
    json = JSON.parse(text);
  } catch {
    return { ok: false, reason: 'not-json' };
  }
  const kind = detectImportKind(json);
  if (!kind) return { ok: false, reason: 'unknown-format' };
  const result = kind === 'kob-backup' ? importBackup(json, kob, { network: o.network, maker: o.maker }) : importIndexerRecovery(json, kob, { network: o.network, maker: o.maker });
  return { ok: true, kind, result };
}

export interface ApplyResult {
  added: number;
  /** already stored (same covenant id): the stored record wins, it may carry more facts (placing tx, label) */
  kept: number;
}

/** Stores the accepted records; an existing record of the same order is kept as it is. */
export async function applyImport(store: RecordStore, result: ImportResult): Promise<ApplyResult> {
  const have = new Set((await store.list()).map((r) => r.covenantId));
  let added = 0;
  let kept = 0;
  for (const r of result.records) {
    if (have.has(r.covenantId)) {
      kept++;
      continue;
    }
    await store.put(r);
    have.add(r.covenantId);
    added++;
  }
  return { added, kept };
}
