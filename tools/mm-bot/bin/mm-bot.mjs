#!/usr/bin/env node
// kob-mm-bot: a market maker for one token on a KOB KAS book. See README.md.
//   node bin/mm-bot.mjs run    --config FILE [--dry-run]   the keepalive: runs the bot and
//                                                          restarts it after a crash
//   node bin/mm-bot.mjs once   --config FILE [--dry-run]   one cycle in the foreground, then exit
//   node bin/mm-bot.mjs status --config FILE [--json]      what the bot did last: mode, fair value,
//                                                          venues, quotes, inventory, fees
//   node bin/mm-bot.mjs stop   --config FILE               write the STOP file: the bot cancels its
//                                                          quotes and exits
//   node bin/mm-bot.mjs check  --config FILE               check the config and print it
//                                                          merged with the defaults
// --dry-run plans with the CLI's --dry-run only: nothing is signed and the state file is not
// written.

import { spawn } from 'node:child_process';
import { existsSync, mkdirSync, readFileSync, rmSync, writeFileSync } from 'node:fs';
import { fileURLToPath } from 'node:url';
import { Bot } from '../src/bot.mjs';
import { loadConfig } from '../src/config.mjs';
import { fmtAmount } from '../src/funds.mjs';
import { Logger, paths } from '../src/store.mjs';

const USAGE = 'usage: mm-bot.mjs run|once|status|stop|check --config FILE [--dry-run] [--json]';
const argv = process.argv.slice(2);
const cmd = argv[0];
const opt = (name) => {
  const i = argv.indexOf(name);
  return i >= 0 ? argv[i + 1] : null;
};
const flag = (name) => argv.includes(name);
const configPath = opt('--config');
if (!cmd || !configPath || !['run', 'once', 'status', 'stop', 'check'].includes(cmd)) {
  console.error(USAGE);
  process.exit(2);
}

let cfg;
try {
  cfg = loadConfig(configPath);
} catch (e) {
  console.error(e.message);
  process.exit(2);
}
mkdirSync(cfg.dataDir, { recursive: true });
const P = paths(cfg.dataDir);
const wait = (ms) => new Promise((r) => setTimeout(r, ms));
const alive = (pid) => { try { process.kill(pid, 0); return true; } catch { return false; } };

if (cmd === 'check') {
  const { configPath: _p, ...shown } = cfg;
  console.log(JSON.stringify(shown, null, 2));
  console.log(`config ok${cfg.keyFile ? '' : process.env.KOB_KEY ? ' (key from KOB_KEY)' : '; WARNING: no keyFile and no KOB_KEY: signing will fail'}`);
  process.exit(0);
}

if (cmd === 'stop') {
  writeFileSync(P.stop, `stop requested ${new Date().toISOString()}\n`);
  console.log(`wrote ${P.stop}: the bot cancels its quotes and exits within one cycle`);
  process.exit(0);
}

if (cmd === 'status') {
  if (!existsSync(P.state)) {
    console.log('no state file yet: the bot has not run with this config');
    process.exit(0);
  }
  const st = JSON.parse(readFileSync(P.state, 'utf8'));
  if (flag('--json')) {
    console.log(JSON.stringify({ mode: st.mode, last: st.last, fees: st.fees, orders: st.orders }, null, 2));
    process.exit(0);
  }
  printStatus(st);
  process.exit(0);
}

if (cmd === 'once') {
  const bot = new Bot(cfg, { dryRun: flag('--dry-run'), once: true });
  await bot.run();
  process.exit(0);
}

// run
if (flag('--child')) {
  const bot = new Bot(cfg, { dryRun: flag('--dry-run') });
  for (const sig of ['SIGINT', 'SIGTERM', 'SIGBREAK']) process.on(sig, () => { bot.stopSignal = true; bot.log.info(`${sig}: stopping`); });
  // Nothing that escapes a cycle may end the bot.
  process.on('unhandledRejection', (e) => bot.log.info(`unhandled rejection: ${e?.stack ?? e}`));
  process.on('uncaughtException', (e) => bot.log.info(`uncaught exception: ${e?.stack ?? e}`));
  await bot.run();
  process.exit(0);
}
const log = new Logger(cfg.dataDir, { tag: 'keep' });
if (existsSync(P.stop)) {
  log.info(`STOP file present (${P.stop}): not starting. Delete it to start.`);
  process.exit(0);
}
if (existsSync(P.pid)) {
  const pid = Number(readFileSync(P.pid, 'utf8'));
  if (pid && pid !== process.pid && alive(pid)) {
    log.info(`another bot (pid ${pid}) runs with this dataDir: not starting`);
    process.exit(1);
  }
}
writeFileSync(P.pid, `${process.pid}\n`);
process.on('exit', () => { try { if (Number(readFileSync(P.pid, 'utf8')) === process.pid) rmSync(P.pid); } catch { /* gone */ } });
// A signal reaches the child too. Wait for it to cancel its quotes and exit.
for (const sig of ['SIGINT', 'SIGTERM', 'SIGBREAK']) process.on(sig, () => log.info(`${sig}: waiting for the bot to stop`));
log.info(`keepalive pid ${process.pid}`);
for (;;) {
  const args = [fileURLToPath(import.meta.url), 'run', '--child', '--config', cfg.configPath, ...(flag('--dry-run') ? ['--dry-run'] : [])];
  const child = spawn(process.execPath, args, { stdio: 'inherit', windowsHide: true });
  const code = await new Promise((r) => child.on('exit', (c) => r(c)));
  if (code === 0 || existsSync(P.stop)) {
    log.info(`bot exited (${code}); keepalive stops`);
    process.exit(0);
  }
  log.info(`bot exited ${code}; restart in 60 s`);
  for (let i = 0; i < 60 && !existsSync(P.stop); i++) await wait(1000);
  if (existsSync(P.stop)) {
    log.info('STOP during the restart wait; keepalive stops');
    process.exit(0);
  }
}

function printStatus(st) {
  const last = st.last;
  const ago = (ms) => (ms ? `${Math.round((Date.now() - ms) / 1000)} s ago` : '-');
  const label = cfg.token.label ?? 'token';
  const m = st.mode;
  console.log(`state      ${m.mode}${m.reason ? ` (${m.reason})` : ''}, since ${new Date(m.since).toISOString()} (${ago(m.since)})`);
  if (!last) return;
  console.log(`updated    ${new Date(last.at).toISOString()} (${ago(last.at)})${last.dryRun ? ' [dry run]' : ''}`);
  const r = last.reference;
  if (r.ok) {
    console.log(`fair       ${r.fair.toFixed(4)} KAS per ${label}  (KAS/USD ${r.kasUsd.toFixed(6)}, ${label}/USD ${r.tokenUsd.value.toFixed(5)} from ${r.tokenUsd.source}, USDT/USD ${r.usdtUsd?.value?.toFixed(5) ?? '-'})`);
  } else {
    console.log(`fair       none: ${r.reason}`);
  }
  console.log(`venues     used ${r.used.join(', ') || '-'}${r.dropped.length ? `; dropped ${r.dropped.map((d) => `${d.name} (${d.reason})`).join(', ')}` : ''}`);
  console.log(`address    ${last.address}`);
  console.log(`inventory  ${last.inventory.text ?? `${fmtAmount(last.inventory.tokens, 6)} ${label}, ${fmtAmount(last.inventory.kas)} KAS`}`);
  console.log(`           ${fmtAmount(last.inventory.tokens, 6)} ${label} and ${fmtAmount(last.inventory.kas)} KAS held; free ${fmtAmount(last.wallet.kasFree / 1e8)} KAS, ${fmtAmount(last.wallet.tokenFree / 10 ** cfg.token.decimals, 6)} ${label} in ${last.wallet.tokenUtxos} UTXO(s)`);
  for (const k of ['bids', 'asks']) console.log(`${k.padEnd(10)} ${last.sides[k].quoting ? 'quoting' : `idle (${last.sides[k].reason})`}`);
  console.log('quotes');
  const rows = [...last.quotes].sort((a, b) => (a.side === b.side ? a.level - b.level : a.side === 'sell' ? -1 : 1));
  if (!rows.length) console.log('           none');
  for (const q of rows) {
    const tok = q.amountLeft / 10 ** cfg.token.decimals;
    console.log(`  ${q.side.padEnd(4)} L${String(q.level).padEnd(3)} ${fmtAmount(q.price / 1e8, 8).padStart(12)} KAS  ${fmtAmount(tok, 6).padStart(12)} ${label}  value ${fmtAmount(q.value / 1e8, 4)} KAS  ${q.id}${q.expiresAt ? `  expires ${q.expiresAt.slice(11, 16)} UTC${q.expired ? ' (EXPIRED)' : ''}` : ''}${q.pending ? ` [${q.pending} pending]` : ''}`);
  }
  for (const f of last.funds ?? []) if (f.level !== 'ok') console.log(`${f.level.toUpperCase().padEnd(10)} ${f.message}`);
  const fees = st.fees && st.fees.day === new Date().toISOString().slice(0, 10) ? st.fees : null;
  const kinds = fees ? Object.entries(fees.byKind).map(([k, v]) => `${k} ${v.n} (${fmtAmount(v.sompi / 1e8, 6)} KAS)`).join(', ') : '';
  console.log(`fees today ${fees ? `${fmtAmount(fees.sompi / 1e8, 6)} KAS in ${fees.txs} tx: ${kinds}` : '0'} (cap ${cfg.safety.maxFeesKasPerDay} KAS)`);
}
