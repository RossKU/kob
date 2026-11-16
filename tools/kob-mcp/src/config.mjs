// The local config file: network, CLI, key path, limits, allow-list. README.md lists the fields.

import { existsSync, readFileSync } from 'node:fs';
import { dirname, isAbsolute, join, resolve } from 'node:path';
import { toSompi } from './units.mjs';

export const DEFAULTS = {
  network: 'testnet-10', // testnet-10 | mainnet
  allowMainnet: false, // mainnet needs this set to true as well
  node: null, // wRPC URL; null: the CLI's default (public nodes). Token create needs a JSON wRPC URL (ws://host:18210)
  indexer: null, // null: the network's public indexer
  kobCli: 'kob', // path of the kob binary
  kobCliArgs: [], // arguments put before every command (tests run a stand-in through node)
  keyFile: null, // file with the 64-hex key, passed to the CLI as --key-file; null: the KOB_KEY environment variable
  dataDir: './kob-mcp-data', // ledger, audit log, plans of token creations, the CLI's state
  mode: 'confirm', // confirm: a write returns its plan and sends only when called again with confirm; auto: sends within the limits
  planTtlSec: 300, // a plan not confirmed within this is void
  limits: {
    perTxKas: 100, // the most KAS one transaction may take out of the wallet (escrow, carriers, sends, fee)
    perDayKas: 500, // the same over a UTC day, from the ledger
    maxFeeKas: 1, // the most network fee of one transaction (a token creation: of both)
    priceBandPct: 25, // a limit price or take bound this far from the last fill (else the mid) is refused; null: off
  },
  withdrawTo: [], // the only addresses kob_send may pay (the primary wallet)
  killSwitchFile: null, // while this file exists every write is refused; null: <dataDir>/STOP
  cli: { timeoutSec: 180, priorityFee: false },
};

const NETWORKS = ['testnet-10', 'mainnet'];

function merge(base, over) {
  const out = { ...base };
  for (const [k, v] of Object.entries(over ?? {})) {
    out[k] = v && typeof v === 'object' && !Array.isArray(v) && base[k] && typeof base[k] === 'object' ? merge(base[k], v) : v;
  }
  return out;
}

// KOB_MCP_* variables (the Claude Desktop extension's form) override the file; empty ones are ignored.
const ENV = {
  KOB_MCP_NETWORK: (c, v) => { c.network = v; },
  KOB_MCP_KOB_CLI: (c, v) => { c.kobCli = v; },
  KOB_MCP_KEY_FILE: (c, v) => { c.keyFile = v; },
  KOB_MCP_NODE: (c, v) => { c.node = v; },
  KOB_MCP_DATA_DIR: (c, v) => { c.dataDir = v; },
  KOB_MCP_MODE: (c, v) => { c.mode = v; },
  KOB_MCP_PER_TX_KAS: (c, v) => { c.limits = { ...c.limits, perTxKas: v }; },
  KOB_MCP_PER_DAY_KAS: (c, v) => { c.limits = { ...c.limits, perDayKas: v }; },
  KOB_MCP_MAX_FEE_KAS: (c, v) => { c.limits = { ...c.limits, maxFeeKas: v }; },
  KOB_MCP_WITHDRAW_TO: (c, v) => { c.withdrawTo = v.split(/[\s,]+/).filter(Boolean); },
};

/** Reads and checks the config (the file, then KOB_MCP_* overrides); relative paths are relative to the file. */
export function loadConfig(file, env = process.env) {
  let raw = {};
  if (file) {
    if (!existsSync(file)) throw new Error(`config not found: ${file}`);
    raw = JSON.parse(readFileSync(file, 'utf8'));
  }
  const c = merge(DEFAULTS, raw);
  let overridden = false;
  for (const [k, set] of Object.entries(ENV)) {
    const v = env[k]?.trim();
    if (v && !v.startsWith('${')) { // an unset form field may arrive as its placeholder
      set(c, v);
      overridden = true;
    }
  }
  if (!file && !overridden) throw new Error('no config: pass --config FILE, set KOB_MCP_CONFIG, or set KOB_MCP_* variables');
  return checkConfig(c, file ? dirname(resolve(file)) : process.cwd(), env);
}

export function checkConfig(c, baseDir, env = process.env) {
  const errs = [];
  const abs = (p) => (p == null ? null : isAbsolute(p) ? p : resolve(baseDir, p));
  if (!NETWORKS.includes(c.network)) errs.push(`network: ${c.network} (testnet-10 or mainnet)`);
  if (c.network === 'mainnet' && c.allowMainnet !== true) errs.push('network is mainnet but allowMainnet is not true');
  if (!['confirm', 'auto'].includes(c.mode)) errs.push(`mode: ${c.mode} (confirm or auto)`);
  if (!c.keyFile && !env.KOB_KEY) errs.push('keyFile is not set and KOB_KEY is empty');
  const lim = {};
  for (const k of ['perTxKas', 'perDayKas', 'maxFeeKas']) {
    try {
      lim[k] = toSompi(c.limits[k]);
      if (lim[k] <= 0n) errs.push(`limits.${k} must be above 0`);
    } catch {
      errs.push(`limits.${k}: ${JSON.stringify(c.limits[k])}`);
    }
  }
  if (c.limits.priceBandPct != null && !(Number(c.limits.priceBandPct) > 0)) errs.push('limits.priceBandPct must be above 0 or null');
  const prefix = c.network === 'mainnet' ? 'kaspa:' : 'kaspatest:';
  const withdrawTo = (c.withdrawTo ?? []).map((a) => String(a).trim().toLowerCase());
  for (const a of withdrawTo) if (!a.startsWith(prefix) || !/^[a-z]+:[02-9ac-hj-np-z]{61,63}$/.test(a)) errs.push(`withdrawTo: ${a} is not a ${c.network} address`);
  if (c.node != null && !/^wss?:\/\//.test(c.node)) errs.push(`node: ${c.node} (ws:// or wss://)`);
  if (errs.length) throw new Error(`config: ${errs.join('; ')}`);
  const dataDir = abs(c.dataDir);
  return {
    ...c,
    withdrawTo,
    keyFile: abs(c.keyFile),
    kobCli: /[\\/]/.test(c.kobCli) ? abs(c.kobCli) : c.kobCli,
    kobCliArgs: (c.kobCliArgs ?? []).map(String),
    dataDir,
    killSwitchFile: abs(c.killSwitchFile) ?? join(dataDir, 'STOP'),
    caps: { perTx: lim.perTxKas, perDay: lim.perDayKas, maxFee: lim.maxFeeKas },
  };
}

/** What kob_status shows: never the key path. */
export function publicConfig(c) {
  return {
    network: c.network,
    mode: c.mode,
    node: c.node ? c.node.replace(/\/\/[^@/]*@/, '//') : '(the CLI default: public nodes)',
    indexer: c.indexer ?? '(the network default)',
    key: c.keyFile ? 'a key file is set' : 'KOB_KEY',
    limits: c.limits,
    withdrawTo: c.withdrawTo,
    planTtlSec: c.planTtlSec,
  };
}
