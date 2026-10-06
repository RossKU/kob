// `soak.mjs consolidate [<key>...]`: the redeploy helper that merges every soak key's plain token UTXOs into ONE per token (the bots'
// own consolidation keeps the fan-out target; this one does not). Each round sends `sendTokens` transactions of up to the program's token
// inputs (smallest first) into one output of the same key, the fee and the change from the freed carriers (no KAS funding), then waits until
// the key's view (indexer + tracker) shows the merges before the next round. The merged output carries 10 KAS when the inputs' carriers
// allow it, else 1 KAS (x402 payments leave 1 KAS carriers). Run it with the bots stopped: it reserves nothing across processes.
import type { Env } from '../env';
import { logger, Stats } from '../log';
import { buildAtUrgency } from '../fees';
import { markets, tokenRef } from '../market';
import { KAS, sleep } from '../util';
import { walletFor } from '../wallet';

const log = logger('consolidate');

export interface MergeRow {
  before: number;
  after: number;
  txs: number;
  freedSompi: string;
}

export async function mergeAll(env: Env, names: string[]): Promise<Record<string, Record<string, MergeRow>>> {
  const stats = new Stats(`${env.cfg.runPath}/stats`, 'consolidate-cli');
  const out: Record<string, Record<string, MergeRow>> = {};
  const keys = names.length ? names : Object.keys(env.keys);
  for (const name of keys) {
    const w = walletFor(env, name, stats);
    for (const m of markets(env)) {
      const ref = tokenRef(env, m);
      const row: MergeRow = { before: (await w.tokenUtxos(ref)).length, after: 0, txs: 0, freedSompi: '0' };
      let freed = 0n;
      for (let round = 0; round < 20; round++) {
        const utxos = [...(await w.tokenUtxos(ref))].filter((u) => BigInt(u.state.amount) > 0n);
        if (utxos.length <= 1) break;
        utxos.sort((a, b) => (BigInt(a.state.amount) < BigInt(b.state.amount) ? -1 : BigInt(a.state.amount) > BigInt(b.state.amount) ? 1 : 0));
        const batches: (typeof utxos)[] = [];
        for (let i = 0; i < utxos.length; i += m.slots.inputs) batches.push(utxos.slice(i, i + m.slots.inputs));
        if (batches.length > 1 && batches[batches.length - 1].length === 1) batches.pop(); // a lone UTXO waits for the next round
        const sent: string[] = [];
        for (const batch of batches) {
          const carriersIn = batch.reduce((s, u) => s + BigInt(u.amount), 0n);
          const carrier = carriersIn >= 11n * KAS ? 10n * KAS : KAS;
          if (carriersIn < carrier + KAS / 2n) continue;
          const total = batch.reduce((s, u) => s + BigInt(u.state.amount), 0n);
          const built = await buildAtUrgency(
            env,
            'low',
            {
              action: 'sendTokens',
              token: { covenantId: m.covenantId, program: m.program },
              tokens: batch,
              recipients: [{ pubkey: w.pk, amount: total.toString(), carrier: carrier.toString() }],
              funding: [],
              change: w.pk,
            } as const,
            (r) => env.kob.build(r as unknown as Parameters<typeof env.kob.build>[0]),
          );
          const r = await w.submit(built, 'consolidate-all', { token: m.ticker, inputs: batch.length, base: total });
          if (!r.ok) {
            log.warn('merge refused', { key: name, token: m.ticker, error: r.error });
            continue;
          }
          row.txs++;
          freed += carriersIn - carrier;
          sent.push(r.txid!);
        }
        if (!sent.length) break;
        // the next round starts once the view lists every merged output (the inputs are reserved by the wallet meanwhile), at most 3 minutes
        for (let i = 0; i < 36; i++) {
          await sleep(5000);
          const seen = new Set((await w.tokenUtxos(ref)).map((u) => u.transactionId));
          if (sent.every((t) => seen.has(t))) break;
        }
      }
      row.after = (await w.tokenUtxos(ref)).length;
      row.freedSompi = freed.toString();
      (out[name] ??= {})[m.ticker] = row;
      log.info('merged', { key: name, token: m.ticker, ...row });
    }
  }
  stats.flush();
  return out;
}
