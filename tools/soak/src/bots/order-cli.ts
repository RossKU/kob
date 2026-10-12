// `soak.mjs order`: the operator places one order by hand through exactly the bots' path (the web app's planner over the live book and the
// key's wallet -> sign -> validate -> submit), on a soak token or on an external token of the soak registry (`externalTokens`, e.g. a Kasseal
// launchpad token). Keys beside run/keys.json come from `--keys <file>` (same format). A token UTXO the key received outside KOB (bought on a
// Kasseal curve) is announced with `--seed <txid>:<index>:<amount>:<carrier sompi>` (a KCC-20 holder owned by the key, owner scheme 0, no
// borrow, the token's extension commitment); the tracker verifies it on the node before it is spent.
//
//   node dist/soak.mjs order --config run/config.json --keys run/kasseal-keys.json --key ksA --token <covenant id> \
//        [--seed <txid>:<index>:<amount>:<carrier>] --intent '{"type":"limit","side":"sell","amount":"1000n","price":"20000000n"}'
//
// Intent numbers are strings with an `n` suffix (bigint), e.g. "1000n".
import { readFileSync } from 'node:fs';
import type { Intent } from '@/kob/plan';
import type { Env, KeyEntry } from '../env';
import { logger, Stats } from '../log';
import { tokenMarket } from '../market';
import { walletFor } from '../wallet';
import { place } from './common';

const log = logger('order');

export const parseIntent = (json: string): Intent =>
  JSON.parse(json, (_k, v) => (typeof v === 'string' && /^-?\d+n$/.test(v) ? BigInt(v.slice(0, -1)) : v)) as Intent;

/** merges extra keys (a file in the run/keys.json format) into the environment; a name already in run/keys.json is refused */
export function addKeys(env: Env, file: string | undefined): void {
  if (!file) return;
  const extra = JSON.parse(readFileSync(file, 'utf8')) as Record<string, KeyEntry>;
  for (const [n, k] of Object.entries(extra)) {
    if (env.keys[n]) throw new Error(`key ${n} of ${file} is already a soak key`);
    env.keys[n] = k;
  }
}

export async function orderCli(env: Env, o: { keys?: string; key: string; token: string; intent: string; seed?: string }): Promise<boolean> {
  addKeys(env, o.keys);
  const stats = new Stats(`${env.cfg.runPath}/stats`, 'order-cli');
  const w = walletFor(env, o.key, stats);
  const m = tokenMarket(env, o.token.toLowerCase());
  if (o.seed) {
    const [txid, index, amount, carrier] = o.seed.split(':');
    if (!/^[0-9a-f]{64}$/.test(txid ?? '') || !index || !amount || !carrier) throw new Error('--seed <txid>:<index>:<amount>:<carrier sompi>');
    const added = w.tracker.add(w.pk, {
      transactionId: txid,
      index: Number(index),
      tokenCovId: m.covenantId,
      program: m.program,
      state: { amount, owner: w.pk, owner_scheme: 0, borrow_scheme: 0, borrow_guard: '0'.repeat(64), extension_commitment: m.extensionCommitment ?? '0'.repeat(64) },
      carrier,
    });
    log.info('seeded', { key: o.key, outpoint: `${txid}:${index}`, amount, added });
  }
  const intent = parseIntent(o.intent);
  const r = await place(env, w, intent, `manual:${intent.type}`, {}, m);
  log.info(r.ok ? 'placed' : 'refused', { key: o.key, token: m.covenantId, type: intent.type, txid: r.txid, created: r.created, issue: r.planIssue, error: r.error });
  process.stdout.write(JSON.stringify({ ok: r.ok, txid: r.txid, created: r.created, issue: r.planIssue, error: r.error }, null, 1) + '\n');
  stats.flush();
  return r.ok;
}
