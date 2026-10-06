// KOB TN10 soak: `node dist/soak.mjs <command> --config run/config.json`
//   setup     one-time: TUSD and asset-token (token2, token3) issuance, registry / allowlist (--recover-issue <slot>:<txid> after an interrupted setup)
//   cancel    cancel orders by covenant id with their makers' keys (the web app's cancel path)
//   bots      bank (funding), market maker, traders, x402 merchant + payer (one process)
//   checker   protocol invariants from indexer + node, incident log, periodic report
//   balances  KAS / token balances of every soak key (JSON)
//   consolidate  merge every key's token UTXOs into one per token (redeploy helper; bots stopped), optional key names
//   report    print the summary report once
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
    default:
      process.stderr.write('usage: soak.mjs setup|bots|checker|cancel|balances|consolidate|report --config <file>\n');
      process.exit(2);
  }
}

main().catch((e) => {
  log.error('fatal', { error: errText(e), stack: e instanceof Error ? e.stack : undefined });
  process.exit(1);
});
