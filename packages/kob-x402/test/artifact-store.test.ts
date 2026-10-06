import { test } from 'node:test';
import assert from 'node:assert/strict';
import { mkdtemp, readFile, readdir, rm } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { FileArtifactStore, MemoryArtifactStore } from '../src/artifact-store.ts';
import type { ArtifactRecord, ArtifactStore } from '../src/artifact-store.ts';
import { KobX402Error } from '../src/errors.ts';

const rec = (id: string): ArtifactRecord => ({
  paymentId: id,
  createdAtMs: 1,
  updatedAtMs: 1,
  status: 'signed',
  url: 'http://m/r',
  method: 'GET',
  requestHash: 'aa'.repeat(32),
  transactionId: 'bb'.repeat(32),
  kind: 'native',
  amount: '5',
  asset: 'KAS',
  network: 'kaspa:testnet-10',
  expiresAtMs: 99,
  consumed: [{ txid: 'cc'.repeat(32), index: 0 }],
  paymentPayload: { x402Version: 2 } as unknown as ArtifactRecord['paymentPayload'],
});

async function contract(store: ArtifactStore): Promise<void> {
  await store.save(rec('payment-id-000000001'));
  await store.save(rec('payment-id-000000002'));
  assert.equal((await store.load('payment-id-000000001'))?.status, 'signed');
  assert.equal(await store.load('payment-id-999999999'), undefined);
  const updated = await store.update('payment-id-000000001', { status: 'settled', note: 'n' });
  assert.equal(updated.status, 'settled');
  assert.ok(updated.updatedAtMs > 1);
  assert.equal((await store.load('payment-id-000000001'))?.note, 'n');
  assert.equal((await store.list()).length, 2);
  await assert.rejects(store.update('payment-id-999999999', {}), KobX402Error);
  for (const bad of ['../evil', 'a/b', 'short', 'x'.repeat(200), 'has space in it 1234']) {
    await assert.rejects(store.save(rec(bad)), (e: unknown) => e instanceof KobX402Error && e.code === 'artifact_store', bad);
  }
}

test('memory store contract (and no aliasing of stored records)', async () => {
  const s = new MemoryArtifactStore();
  await contract(s);
  const r = (await s.load('payment-id-000000002'))!;
  r.status = 'revoked';
  assert.equal((await s.load('payment-id-000000002'))?.status, 'signed');
});

test('file store contract, atomic files, survives a restart', async () => {
  const dir = await mkdtemp(join(tmpdir(), 'kob-x402-store-'));
  try {
    const s = new FileArtifactStore(join(dir, 'nested', 'artifacts'));
    await contract(s);
    const names = await readdir(join(dir, 'nested', 'artifacts'));
    assert.deepEqual(names.sort(), ['payment-id-000000001.json', 'payment-id-000000002.json'], 'no temp files left behind');
    const again = new FileArtifactStore(join(dir, 'nested', 'artifacts'));
    assert.equal((await again.load('payment-id-000000001'))?.status, 'settled');
    assert.equal(JSON.parse(await readFile(join(dir, 'nested', 'artifacts', 'payment-id-000000002.json'), 'utf8')).transactionId, 'bb'.repeat(32));
    assert.deepEqual((await new FileArtifactStore(join(dir, 'missing')).list()), []);
  } finally {
    await rm(dir, { recursive: true, force: true });
  }
});
