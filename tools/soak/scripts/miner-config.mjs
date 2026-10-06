// The supervisor's miner child: run/config.json `miner` -> the tn10-miner settings file (run/miner.json, passed as MINER_CONFIG).
// The supervisor rewrites the file on every start of the miner; the miner re-reads it whenever it changes, so an edit of run/miner.json
// moves the watermarks / duty cycle of the running miner at once (until the next supervisor start rewrites it from config.json).
// See README "Miner".
import { writeFileSync } from 'node:fs';
import { join } from 'node:path';

const KEYS = ['backend', 'threads', 'roundMs', 'lowKas', 'highKas', 'maxDuty', 'dutyWindowSec', 'pollSec', 'maturityDaa', 'balanceAddress', 'gpu'];
const GPU_KEYS = ['device', 'global', 'local', 'dispatchMs'];

const isObj = (v) => typeof v === 'object' && v !== null && !Array.isArray(v);
const posInt = (v) => Number.isInteger(v) && v > 0;

/** The validated tn10-miner settings (its settings-file shape) of a soak config; throws on a bad `miner` block. */
export function minerSettings(cfg) {
  const m = cfg.miner ?? {};
  if (!isObj(m)) throw new Error('config.miner must be an object');
  for (const k of Object.keys(m)) if (!KEYS.includes(k)) throw new Error(`config.miner.${k}: unknown key (known: ${KEYS.join(', ')})`);
  const out = {};
  if (m.backend !== undefined) {
    if (!['cpu', 'gpu', 'auto'].includes(m.backend)) throw new Error('config.miner.backend must be "cpu", "gpu" or "auto"');
    out.backend = m.backend;
  }
  // `minerThreads` (top level) is the older spelling; the CPU backend's threads (unused by the GPU backend)
  const threads = m.threads ?? cfg.minerThreads ?? 6;
  if (!posInt(threads)) throw new Error('config.miner.threads must be a positive integer');
  out.threads = threads;
  for (const k of ['roundMs', 'dutyWindowSec', 'pollSec']) {
    if (m[k] === undefined) continue;
    if (!posInt(m[k])) throw new Error(`config.miner.${k} must be a positive integer`);
    out[k] = m[k];
  }
  if ((m.lowKas === undefined) !== (m.highKas === undefined)) throw new Error('config.miner: set both lowKas and highKas, or neither (continuous mining)');
  if (m.lowKas !== undefined) {
    for (const k of ['lowKas', 'highKas']) {
      if (typeof m[k] !== 'number' || !Number.isFinite(m[k]) || m[k] < 0) throw new Error(`config.miner.${k} must be a KAS amount >= 0`);
    }
    if (!(m.lowKas < m.highKas)) throw new Error('config.miner.lowKas must be below highKas');
    out.lowKas = m.lowKas;
    out.highKas = m.highKas;
  }
  if (m.maxDuty !== undefined) {
    if (typeof m.maxDuty !== 'number' || !(m.maxDuty > 0 && m.maxDuty <= 1)) throw new Error('config.miner.maxDuty must be in (0, 1]');
    out.maxDuty = m.maxDuty;
  }
  if (m.maturityDaa !== undefined) {
    if (!Number.isInteger(m.maturityDaa) || m.maturityDaa < 0) throw new Error('config.miner.maturityDaa must be an integer >= 0');
    out.maturityDaa = m.maturityDaa;
  }
  if (m.balanceAddress !== undefined) {
    if (typeof m.balanceAddress !== 'string' || !m.balanceAddress) throw new Error('config.miner.balanceAddress must be an address string');
    out.balanceAddress = m.balanceAddress;
  }
  if (m.gpu !== undefined) {
    if (!isObj(m.gpu)) throw new Error('config.miner.gpu must be an object');
    const g = {};
    for (const k of Object.keys(m.gpu)) if (!GPU_KEYS.includes(k)) throw new Error(`config.miner.gpu.${k}: unknown key (known: ${GPU_KEYS.join(', ')})`);
    if (m.gpu.device !== undefined) {
      if (typeof m.gpu.device !== 'string' && !Number.isInteger(m.gpu.device)) throw new Error('config.miner.gpu.device must be an index or a name');
      g.device = String(m.gpu.device);
    }
    for (const k of ['global', 'local', 'dispatchMs']) {
      if (m.gpu[k] === undefined) continue;
      if (!posInt(m.gpu[k])) throw new Error(`config.miner.gpu.${k} must be a positive integer`);
      g[k] = m.gpu[k];
    }
    out.gpu = g;
  }
  return out;
}

/**
 * The supervisor's spec of the miner child. Writes `<run>/miner.json` unless `write` is false. Pays the bank key; the watermarks apply
 * to the bank's spendable (coinbase-matured) balance unless `balanceAddress` says otherwise.
 */
export function minerSpec(cfg, { run, minerBin, bankAddress, write = true }) {
  const settings = minerSettings(cfg);
  const file = join(run, 'miner.json');
  if (write) writeFileSync(file, JSON.stringify(settings, null, 2) + '\n');
  return { name: 'miner', cmd: minerBin, args: [], env: { WALLET: bankAddress, NODE: cfg.nodeUrl, MINER_CONFIG: file }, settings };
}
