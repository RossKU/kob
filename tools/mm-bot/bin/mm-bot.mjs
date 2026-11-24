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
//   node bin/mm-bot.mjs unresolved --config FILE           the asks kept with no proof that they
//                                                          ended (unresolved, parked or proven)
//   node bin/mm-bot.mjs drop   --config FILE --id ID [--amount-left N] [--force]
//                                                          after checking it: drop one of them,
//                                                          unless the node shows it live (the bot
//                                                          must not be running); --force past an
//                                                          unproven template or reported fills
// --dry-run plans with the CLI's --dry-run only: nothing is signed and the state file is not
// written.

import { spawn } from 'node:child_process';
import { existsSync, mkdirSync, readFileSync, rmSync, writeFileSync } from 'node:fs';
import { fileURLToPath } from 'node:url';
import { Bot } from '../src/bot.mjs';
import { isJsonWrpc } from '../src/node.mjs';
import { loadConfig } from '../src/config.mjs';
import { fmtAmount } from '../src/funds.mjs';
import { Logger, paths } from '../src/store.mjs';

const USAGE = 'usage: mm-bot.mjs run|once|status|stop|check|unresolved --config FILE [--dry-run] [--json] | drop --config FILE --id ID [--amount-left N] [--force]';
const argv = process.argv.slice(2);
const cmd = argv[0];
const opt = (name) => {
  const i = argv.indexOf(name);
  return i >= 0 ? argv[i + 1] : null;
};
const flag = (name) => argv.includes(name);
const configPath = opt('--config');
if (!cmd || !configPath || !['run', 'once', 'status', 'stop', 'check', 'unresolved', 'drop'].includes(cmd)) {
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
// On mainnet a fully filled ask is proven only through a JSON wRPC node: without one every honest
// full fill parks its ask and takes a ladder level for good (R4T-5). Testnet-10 only warns.
const NODE_NEEDED = 'a JSON wRPC node (ws://host:18110, testnet-10 18210) is needed to prove a fully filled ask paid; without one it stays unresolved and holds its share of the ask cap (README, tracking)';
if (cfg.network === 'mainnet' && ['run', 'once', 'check'].includes(cmd) && !isJsonWrpc(cfg.node)) {
  console.error(`config ${configPath}: mainnet needs "node": a JSON wRPC URL of a node you run (ws://host:18110), not ${cfg.node ? 'a Borsh endpoint' : 'null (the CLI default)'}: ${NODE_NEEDED}`);
  process.exit(2);
}
// Partial or whole fills of the asks is the operator's informed choice on mainnet (README, tracking).
if (cfg.network === 'mainnet' && ['run', 'once', 'check'].includes(cmd) && cfg.quotes.askPartialFills == null) {
  console.error(`config ${configPath}: mainnet needs quotes.askPartialFills set: true (smaller takers trade, trusts the indexer) or false (whole levels only, node alone decides).`);
  process.exit(2);
}
mkdirSync(cfg.dataDir, { recursive: true });
const P = paths(cfg.dataDir);
const wait = (ms) => new Promise((r) => setTimeout(r, ms));
const alive = (pid) => { try { process.kill(pid, 0); return true; } catch { return false; } };

if (cmd === 'check') {
  const { configPath: _p, ...shown } = cfg;
  // Where the key lives and the webhook's URL (it may carry a token) stay out of the output (L25).
  if (shown.keyFile) shown.keyFile = '(set)';
  if (shown.alerts?.webhookUrl) shown.alerts = { ...shown.alerts, webhookUrl: '(set)' };
  console.log(JSON.stringify(shown, null, 2));
  console.log(`config ok${cfg.keyFile ? '' : process.env.KOB_KEY ? ' (key from KOB_KEY)' : '; WARNING: no keyFile and no KOB_KEY: signing will fail'}`);
  if (!isJsonWrpc(cfg.node)) console.log(`WARNING: node: ${NODE_NEEDED}`);
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

if (cmd === 'unresolved') {
  const bot = new Bot(cfg, { dryRun: true });
  const rows = bot.unresolvedOrders();
  // Their records, for kob recover: own-orders.json holds only what the bot last looked up (R5T-I3).
  const written = rows.length ? bot.writeRecords(rows.map((o) => o.id), undefined, P.unresolved) : null;
  if (flag('--json')) console.log(JSON.stringify(rows, null, 2));
  else if (!rows.length) console.log('no unresolved order');
  else {
    for (const o of rows) {
      const what = o.proven ? 'PROVEN (paid; still looked up, level free)' : o.parked ? 'PARKED (past its expiry)' : 'unresolved';
      console.log(`${o.id}  ${o.side} L${o.level}  ${what}  placed ${o.amount ?? '?'} base, left ${o.amountLeft ?? '?'} (last known), fills seen ${o.filledSeen}, expires ${o.expiresAt ? new Date(o.expiresAt).toISOString() : '-'}, hints ${o.hints.join(',') || '-'}`);
    }
    console.log(`Check each with kob recover --from ${written ? P.unresolved : '<no record to look it up by>'} --network ${cfg.network} --amount-left <id>=<amount left>; once it has ended: mm-bot drop --config FILE --id <id> (with the bot stopped).`);
  }
  process.exit(0);
}

if (cmd === 'drop') {
  const id = String(opt('--id') ?? '');
  const n = opt('--amount-left');
  if (!/^[0-9a-f]{64}$/.test(id) || (n != null && !/^\d+$/.test(n))) {
    console.error('usage: mm-bot.mjs drop --config FILE --id <64 hex> [--amount-left <base units>] [--force]');
    process.exit(2);
  }
  // The running bot would write its own state over the drop.
  if (existsSync(P.pid)) {
    const pid = Number(readFileSync(P.pid, 'utf8'));
    if (pid && alive(pid)) {
      console.error(`a bot (pid ${pid}) runs with this dataDir: stop it first (mm-bot stop)`);
      process.exit(1);
    }
  }
  const bot = new Bot(cfg);
  bot.log.quiet = true;
  const r = await bot.operatorDrop(id, { amountLeft: n == null ? null : Number(n), force: flag('--force') });
  console.log(`node: ${r.node ? JSON.stringify(r.node) : '-'}`);
  console.log(r.message);
  process.exit(r.ok ? 0 : 1);
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
