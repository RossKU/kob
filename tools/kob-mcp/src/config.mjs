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
    // A limit price or take bound this far from the reference is refused, and so is any priced write
    // on a token with no reference (R2-T2). The reference is tokens.<id>.refPrice when set, which the
    // book may only tighten; else the book's, when refDepthKas stands behind it on both the fills and
    // the resting side. null: off.
    priceBandPct: 25,
    refDepthKas: 1000, // the KAS of fills and of resting orders (not the wallet's own) a book reference needs
    // The tip goes to the executor that fills the order: at most this % of the price, and this
    // much KAS over the whole order (a sell's custody, a buy's escrow). null: that cap is off.
    maxTipPct: 1,
    maxTipKas: 1,
    launchesPerDay: 3, // token creations per UTC day
    // Tokens that leave the wallet (a send, the escrow of a sell, a sale into a buy order), from
    // the dry run's net effect. Per token: { "<covenant id>": { perTx, perDay } } in base units.
    // Any other token gets a share of the wallet's free holding confirmed on the node at the start
    // of the UTC day (free now + what left today). null: only listed tokens.
    tokens: {},
    tokenDefault: { perTxPct: 25, perDayPct: 50 },
  },
  // The operator's reference per token: { "<covenant id>": { refPrice: "0.25" (KAS per whole token), bandPct: 10 (optional) } }.
  tokens: {},
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

// KOB_MCP_* variables (the Claude Desktop extension's form) override the file. Empty ones are
// ignored.
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
  KOB_MCP_LAUNCHES_PER_DAY: (c, v) => { c.limits = { ...c.limits, launchesPerDay: Number(v) }; },
  KOB_MCP_WITHDRAW_TO: (c, v) => { c.withdrawTo = v.split(/[\s,]+/).filter(Boolean); },
};

/**
 * Reads and checks the config (the file, then KOB_MCP_* overrides); relative paths are relative to
 * the file.
 */
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
  c = { ...c, limits: { ...DEFAULTS.limits, ...c.limits } };
  const lim = {};
  for (const k of ['perTxKas', 'perDayKas', 'maxFeeKas']) {
    try {
      lim[k] = toSompi(c.limits[k]);
      if (lim[k] <= 0n) errs.push(`limits.${k} must be above 0`);
    } catch {
      errs.push(`limits.${k}: ${JSON.stringify(c.limits[k])}`);
    }
  }
  const pct = c.limits.maxTipPct;
  if (pct != null && !(typeof pct === 'number' && pct >= 0 && pct <= 100 && Math.abs(pct * 100 - Math.round(pct * 100)) < 1e-9)) {
    errs.push('limits.maxTipPct must be a number 0..100 (two decimals at most), or null');
  }
  let maxTip = null;
  if (c.limits.maxTipKas != null) {
    try {
      maxTip = toSompi(c.limits.maxTipKas);
    } catch {
      errs.push(`limits.maxTipKas: ${JSON.stringify(c.limits.maxTipKas)}`);
    }
  }
  if (c.limits.priceBandPct != null && (!/^\d+(\.\d+)?$/.test(String(c.limits.priceBandPct)) || !(Number(c.limits.priceBandPct) > 0))) errs.push('limits.priceBandPct must be a number above 0, or null');
  let refDepth = null;
  try {
    refDepth = toSompi(c.limits.refDepthKas);
    if (refDepth <= 0n) errs.push('limits.refDepthKas must be above 0');
  } catch {
    errs.push(`limits.refDepthKas: ${JSON.stringify(c.limits.refDepthKas)}`);
  }
  const refs = new Map();
  for (const [id, v] of Object.entries(c.tokens ?? {})) {
    if (!/^[0-9a-fA-F]{64}$/.test(id)) { errs.push(`tokens: ${id} is not a covenant id (64 hex)`); continue; }
    let sompi = null;
    try {
      sompi = toSompi(v?.refPrice);
    } catch { /* below */ }
    if (!(sompi > 0n)) errs.push(`tokens.${id}.refPrice must be a KAS price per whole token above 0 (a decimal string)`);
    const band = v?.bandPct;
    if (band != null && (!/^\d+(\.\d+)?$/.test(String(band)) || !(Number(band) > 0))) errs.push(`tokens.${id}.bandPct must be a number above 0`);
    refs.set(id.toLowerCase(), { sompi, bandPct: band ?? null });
  }
  const tokenCaps = { byToken: new Map(), perTxPct: null, perDayPct: null };
  for (const [id, v] of Object.entries(c.limits.tokens ?? {})) {
    if (!/^[0-9a-fA-F]{64}$/.test(id)) { errs.push(`limits.tokens: ${id} is not a covenant id (64 hex)`); continue; }
    const cap = {};
    for (const k of ['perTx', 'perDay']) {
      if (!/^\d{1,39}$/.test(String(v?.[k] ?? ''))) errs.push(`limits.tokens.${id}.${k} must be a whole number of base units`);
      else cap[k] = BigInt(v[k]);
    }
    tokenCaps.byToken.set(id.toLowerCase(), cap);
  }
  if (c.limits.tokenDefault != null) {
    for (const k of ['perTxPct', 'perDayPct']) {
      const p = c.limits.tokenDefault[k];
      if (!Number.isInteger(p) || p < 0 || p > 100) errs.push(`limits.tokenDefault.${k} must be a whole number 0..100`);
      else tokenCaps[k] = BigInt(p);
    }
  }
  if (!Number.isInteger(c.limits.launchesPerDay) || c.limits.launchesPerDay < 0) errs.push('limits.launchesPerDay must be a whole number, 0 or more');
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
    refs,
    refDepth,
    caps: { perTx: lim.perTxKas, perDay: lim.perDayKas, maxFee: lim.maxFeeKas, tokens: tokenCaps, tips: { maxTipPct: pct ?? null, maxTip } },
  };
}

/** What kob_status shows. It never shows the key path. */
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
