// A temporary data directory, a config that runs the stand-in CLI, and the calls it saw.
import { mkdirSync, mkdtempSync, readFileSync, existsSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join, dirname } from 'node:path';
import { fileURLToPath } from 'node:url';
import { checkConfig } from '../src/config.mjs';

export const HERE = dirname(fileURLToPath(import.meta.url));
export const PRIMARY = 'kaspatest:qpzfrnaut7sf9uv9vfzh6muu3ugc0el8qacrnc4lvrlhclpwrgff2twv4v8z7';

export function setup(over = {}) {
  const dir = mkdtempSync(join(tmpdir(), 'kob-mcp-'));
  const log = join(dir, 'calls.log');
  process.env.FAKE_KOB_LOG = log;
  process.env.FAKE_KOB_CHAIN = join(dir, 'chain.json');
  writeFileSync(join(dir, 'agent.key'), 'not a real key');
  const raw = {
    network: 'testnet-10',
    kobCli: process.execPath,
    kobCliArgs: [join(HERE, 'fake-kob.mjs')],
    keyFile: join(dir, 'agent.key'),
    indexer: 'http://127.0.0.1:9',
    dataDir: join(dir, 'data'),
    mode: 'confirm',
    planTtlSec: 300,
    limits: { perTxKas: 100, perDayKas: 150, maxFeeKas: 0.01, priceBandPct: 25 },
    // The operator's reference for the test token (R2-T2): 0.25 KAS, its last fill in fake-kob.mjs.
    tokens: { ['55'.repeat(32)]: { refPrice: '0.25' } },
    withdrawTo: [PRIMARY],
    killSwitchFile: null,
    cli: { timeoutSec: 30, priorityFee: false },
  };
  const limits = { ...raw.limits, ...over.limits };
  Object.assign(raw, over, { limits });
  const cfg = checkConfig(raw, dir, {});
  mkdirSync(cfg.dataDir, { recursive: true });
  const calls = () => (existsSync(log) ? readFileSync(log, 'utf8').split('\n').filter(Boolean).map((l) => JSON.parse(l)) : []);
  const yesCalls = () => calls().filter((a) => a.includes('--yes'));
  const lines = (f) => (existsSync(join(cfg.dataDir, f)) ? readFileSync(join(cfg.dataDir, f), 'utf8').split('\n').filter(Boolean).map((l) => JSON.parse(l)) : []);
  return { dir, cfg, raw, calls, yesCalls, audit: () => lines('audit.jsonl'), ledger: () => lines('ledger.jsonl') };
}
