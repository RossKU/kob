// The supervisor's miner child: config.miner -> tn10-miner settings file (scripts/miner-config.mjs).
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { mkdtempSync, readFileSync, rmSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
// @ts-expect-error plain ESM script without types
import { minerSettings, minerSpec } from '../scripts/miner-config.mjs';

const base = { nodeUrl: 'ws://127.0.0.1:18210', minerThreads: 12 };

test('config.miner absent: continuous, threads from minerThreads (the old config keeps mining as before)', () => {
  assert.deepEqual(minerSettings(base), { threads: 12 });
  assert.deepEqual(minerSettings({ nodeUrl: 'x' }), { threads: 6 });
});

test('config.miner: backend, watermarks, duty cycle and GPU options pass through in the settings-file shape', () => {
  const m = {
    backend: 'gpu',
    threads: 4,
    lowKas: 20000,
    highKas: 30000,
    maxDuty: 0.5,
    dutyWindowSec: 600,
    pollSec: 15,
    roundMs: 3000,
    maturityDaa: 1000,
    gpu: { device: 0, dispatchMs: 60, local: 256 },
  };
  assert.deepEqual(minerSettings({ ...base, miner: m }), { ...m, gpu: { device: '0', dispatchMs: 60, local: 256 } });
});

test('config.miner: invalid blocks throw (a typo must not silently mine around the clock)', () => {
  const bad = (miner: unknown, re: RegExp) => assert.throws(() => minerSettings({ ...base, miner }), re);
  bad({ lowKas: 100 }, /both lowKas and highKas/);
  bad({ highKas: 100 }, /both lowKas and highKas/);
  bad({ lowKas: 100, highKas: 100 }, /below highKas/);
  bad({ lowKas: -1, highKas: 100 }, /lowKas/);
  bad({ lowKas: '100', highKas: 200 }, /lowKas/);
  bad({ maxDuty: 0 }, /maxDuty/);
  bad({ maxDuty: 1.2 }, /maxDuty/);
  bad({ backend: 'cuda' }, /backend/);
  bad({ lowkas: 1 }, /unknown key/);
  bad({ threads: 0 }, /threads/);
  bad({ pollSec: 1.5 }, /pollSec/);
  bad({ gpu: { global: -1 } }, /gpu.global/);
  bad({ gpu: { platform: 1 } }, /unknown key/);
  bad([], /object/);
});

test('minerSpec: pays the bank, passes the node and the settings file, writes run/miner.json', () => {
  const run = mkdtempSync(join(tmpdir(), 'soak-miner-'));
  try {
    const cfg = { ...base, miner: { backend: 'auto', lowKas: 1000, highKas: 5000 } };
    const s = minerSpec(cfg, { run, minerBin: 'C:/x/tn10-miner.exe', bankAddress: 'kaspatest:qbank' });
    assert.equal(s.name, 'miner');
    assert.equal(s.cmd, 'C:/x/tn10-miner.exe');
    assert.deepEqual(s.env, { WALLET: 'kaspatest:qbank', NODE: cfg.nodeUrl, MINER_CONFIG: join(run, 'miner.json') });
    assert.equal(s.env.THREADS, undefined, 'threads go through the (hot-reloaded) file, not the environment');
    assert.deepEqual(JSON.parse(readFileSync(join(run, 'miner.json'), 'utf8')), { backend: 'auto', threads: 12, lowKas: 1000, highKas: 5000 });
    // a bad block fails before anything is written or started
    assert.throws(() => minerSpec({ ...base, miner: { lowKas: 1 } }, { run, minerBin: 'm', bankAddress: 'a' }), /both/);
  } finally {
    rmSync(run, { recursive: true, force: true });
  }
});
