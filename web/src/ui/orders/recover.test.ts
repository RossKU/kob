import { describe, expect, it } from 'vitest';
import { MemoryRecordStore, recordsFromBuilt, type PlacementRecord } from '../../kob/records';
import { loadKobNode } from '../../kob/wasm.node';
import { MAKER, OTHER, placeGolden } from '../../testing/chain-fixtures';
import { applyImport, backupFileText, detectImportKind, parseImport, recoveryFileText } from './recover';

const kob = loadKobNode();
const NET = 'testnet-10';

const recs = (names: string[], maker = MAKER): PlacementRecord[] =>
  names.flatMap((n) => recordsFromBuilt(kob, placeGolden(kob, n, maker).built, { maker: maker.pk, network: NET, placedAtUnix: 1_789_999_000n, placedAtDaa: 900n, label: n }));

const ctx = { network: NET, maker: MAKER.pk };

describe('export -> import round trips', () => {
  const records = recs(['create.ask', 'create.bid', 'create.condAsk', 'create.ifdBid']);

  it('the indexer recovery file re-imports every order with the same covenant ids', () => {
    const p = parseImport(recoveryFileText(records, NET), kob, ctx);
    expect(p.ok && p.kind).toBe('indexer-recovery');
    if (!p.ok) return;
    expect(p.result.rejected).toEqual([]);
    expect(p.result.records.map((r) => r.covenantId).sort()).toEqual(records.map((r) => r.covenantId).sort());
    // the file carries the ORIGINAL state span, so the terms survive
    expect(p.result.records.map((r) => r.state).sort()).toEqual(records.map((r) => r.state).sort());
  });

  it('the KOB backup re-imports without the placing txs (records keep their terms, deadline and label)', () => {
    const text = backupFileText(records, NET, Date.UTC(2026, 8, 9));
    expect(JSON.parse(text)).toMatchObject({ format: 'kob-backup', version: 1, network: NET, exportedAt: '2026-09-09T00:00:00.000Z', txs: {} });
    const p = parseImport(text, kob, ctx);
    expect(p.ok && p.kind).toBe('kob-backup');
    if (!p.ok) return;
    expect(p.result.rejected).toEqual([]);
    expect(p.result.records).toHaveLength(records.length);
    const back = p.result.records.find((r) => r.covenantId === records[0].covenantId)!;
    expect(back).toMatchObject({ state: records[0].state, label: records[0].label, kind: records[0].kind, txid: null });
  });

  it('exports only the records of the given network', () => {
    const other = { ...records[0], network: 'mainnet' };
    expect(JSON.parse(recoveryFileText([...records, other], NET)).orders).toHaveLength(records.length);
    expect(JSON.parse(backupFileText([...records, other], NET)).records).toHaveLength(records.length);
  });
});

describe('import validation', () => {
  const records = recs(['create.ask']);

  it('rejects garbage and unknown formats without throwing', () => {
    expect(parseImport('not json', kob, ctx)).toEqual({ ok: false, reason: 'not-json' });
    expect(parseImport('{"hello":1}', kob, ctx)).toEqual({ ok: false, reason: 'unknown-format' });
    expect(parseImport('[]', kob, ctx)).toEqual({ ok: false, reason: 'unknown-format' });
    expect(detectImportKind(null)).toBeNull();
  });

  it('rejects a file of another network as a whole', () => {
    const p = parseImport(recoveryFileText(records, NET), kob, { network: 'mainnet', maker: MAKER.pk });
    expect(p.ok && p.result.records).toEqual([]);
    expect(p.ok && p.result.rejected[0].reason).toContain('network');
  });

  it('never adds an order the wallet key did not place', () => {
    const theirs = recs(['create.bid'], OTHER);
    const p = parseImport(recoveryFileText([...records, ...theirs], NET), kob, ctx);
    expect(p.ok && p.result.records.map((r) => r.covenantId)).toEqual([records[0].covenantId]);
    expect(p.ok && p.result.rejected).toHaveLength(1);
    expect(p.ok && p.result.rejected[0].reason).toContain('another maker');
  });

  it('rejects a truncated state (does not decode for its template)', () => {
    const doc = JSON.parse(recoveryFileText(records, NET));
    doc.orders[0].state = doc.orders[0].state.slice(0, -4);
    const p = parseImport(JSON.stringify(doc), kob, ctx);
    expect(p.ok && p.result.records).toEqual([]);
    expect(p.ok && p.result.rejected).toHaveLength(1);
  });
});

describe('applyImport', () => {
  it('stores new records and keeps the stored version of known ones', async () => {
    const store = new MemoryRecordStore();
    const [a, b] = recs(['create.ask', 'create.bid']);
    await store.put({ ...a, label: 'my label', txid: 'aa'.repeat(32), output: 0 });
    const res = await applyImport(store, { records: [a, b], rejected: [] });
    expect(res).toEqual({ added: 1, kept: 1 });
    const all = await store.list();
    expect(all).toHaveLength(2);
    expect(all.find((r) => r.covenantId === a.covenantId)?.label).toBe('my label');
  });
});
