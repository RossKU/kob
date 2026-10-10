// KOB TN10 soak: `node dist/soak.mjs <command> --config run/config.json`
//   setup     one-time: TUSD and asset-token (token2, token3) issuance, registry / allowlist (--recover-issue <slot>:<txid> after an interrupted setup)
//   cancel    cancel orders by covenant id with their makers' keys (the web app's cancel path)
//   bots      bank (funding), market maker, traders, x402 merchant + payer (one process)
//   checker   protocol invariants from indexer + node, incident log, periodic report
//   balances  KAS / token balances of every soak key (JSON)
//   consolidate  merge every key's token UTXOs into one per token (redeploy helper; bots stopped), optional key names
//   report    print the summary report once
//   carry-history  move the market history of re-issued tokens to their new covenant ids, by ticker (one indexer database, its
//             executor stopped): --db <index.sqlite3> --from-registry <old tokens.json> [--to-registry <file>] [--dry-run]
import { loadConfig, type TokenSlot } from './config';
import { createEnv } from './env';
import { errText, logger } from './log';

const log = logger('main');

function arg(name: string, dflt?: string): string | undefined {
  const i = process.argv.indexOf(name);
  return i >= 0 ? process.argv[i + 1] : dflt;
}

async function main(): Promise<void> {
  const cmd = process.argv[2];
  const cfg = loadConfig(arg('--config', 'run/config.json')!);
  process.on('unhandledRejection', (e) => log.error('unhandled rejection', { error: errText(e) }));
  switch (cmd) {
    case 'setup': {
      const env = await createEnv(cfg);
      const { runSetup } = await import('./setup');
      // --recover-issue <token|token2|token3>:<txid>: record an issuance that was broadcast by an interrupted setup
      const rec = arg('--recover-issue');
      const m = rec ? /^(token|token2|token3):([0-9a-f]{64})$/.exec(rec) : null;
      if (rec && !m) throw new Error('--recover-issue <token|token2|token3>:<txid>');
      await runSetup(env, m ? { slot: m[1] as TokenSlot, txid: m[2] } : undefined);
      process.exit(0);
      break;
    }
    case 'bots': {
      const env = await createEnv(cfg);
      const { runBots } = await import('./bots/run');
      await runBots(env);
      break;
    }
    case 'checker': {
      const env = await createEnv(cfg);
      const { runChecker } = await import('./checker/run');
      await runChecker(env);
      break;
    }
    case 'cancel': {
      // operator tool: cancel the given orders with their makers' keys through the web app's cancel path (order view -> snapshot -> planCancel)
      const env = await createEnv(cfg);
      const { cancelOrders } = await import('./bots/cancel-cli');
      const ids = process.argv.slice(3).filter((a, i, all) => /^[0-9a-f]{64}$/i.test(a) && all[i - 1] !== '--config');
      const ok = await cancelOrders(env, ids);
      process.exit(ok ? 0 : 1);
      break;
    }
    case 'balances': {
      // KAS / token balances of every soak key as JSON (read-only)
      const env = await createEnv(cfg);
      const { balances } = await import('./bots/balances-cli');
      process.stdout.write(JSON.stringify(await balances(env), null, 1) + '\n');
      process.exit(0);
      break;
    }
    case 'consolidate': {
      // redeploy helper: one token UTXO per key and token (the bots' own consolidation keeps their fan-out); prints the per-key result
      const env = await createEnv(cfg);
      const { mergeAll } = await import('./bots/merge-cli');
      const names = process.argv.slice(3).filter((a, i, all) => !a.startsWith('--') && all[i - 1] !== '--config');
      process.stdout.write(JSON.stringify(await mergeAll(env, names), null, 1) + '\n');
      process.exit(0);
      break;
    }
    case 'report': {
      const env = await createEnv(cfg);
      const { writeReport } = await import('./checker/report');
      const r = await writeReport(env);
      process.stdout.write(r.text + '\n');
      process.exit(0);
      break;
    }
    case 'carry-history': {
      // redeploy helper (re-issued tokens, src/history-carry.ts): no node, no keys; the new registry defaults to run/registry/tokens.json
      const { readFileSync } = await import('node:fs');
      const { join } = await import('node:path');
      const { carryHistory, carryPairs } = await import('./history-carry');
      const db = arg('--db');
      const fromReg = arg('--from-registry');
      if (!db || !fromReg) throw new Error('carry-history --db <index.sqlite3> --from-registry <old tokens.json> [--to-registry <file>] [--dry-run]');
      const toReg = arg('--to-registry', join(cfg.runPath, 'registry', 'tokens.json'))!;
      const pairs = carryPairs(JSON.parse(readFileSync(fromReg, 'utf8')), JSON.parse(readFileSync(toReg, 'utf8')));
      if (process.argv.includes('--dry-run')) {
        process.stdout.write(JSON.stringify({ db, pairs }, null, 1) + '\n');
        process.exit(0);
      }
      const { DatabaseSync } = await import('node:sqlite');
      const conn = new DatabaseSync(db);
      try {
        conn.exec('PRAGMA busy_timeout = 5000');
        process.stdout.write(JSON.stringify({ db, carried: carryHistory(conn, pairs) }, null, 1) + '\n');
      } finally {
        conn.close();
      }
      process.exit(0);
      break;
    }
    default:
      process.stderr.write('usage: soak.mjs setup|bots|checker|cancel|balances|consolidate|report|carry-history --config <file>\n');
      process.exit(2);
  }
}

main().catch((e) => {
  log.error('fatal', { error: errText(e), stack: e instanceof Error ? e.stack : undefined });
  process.exit(1);
});
