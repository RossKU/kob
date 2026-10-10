// Merges a bot's own plain token UTXOs (see consolidate-math.ts for the selection rule). The reverse of fanout.ts: a `sendTokens` of up to
// `slots.inputs` token inputs into ONE output of the same key; the other carriers are freed (the fee comes out of them, the rest returns
// to the key's KAS as the change output, like the executor's own maintenance merge). A pass is a CHAIN (`selectMergeChain`): from the second
// transaction on, the previous merge's output (not yet accepted) is one of the inputs, so with KOB's standard program (3 token inputs) a
// pass of k transactions merges 2k + 1 UTXOs into one. Only token UTXOs are spent, never KAS funding, so the key's order placements (which
// fund from KAS UTXOs) are not affected; the inputs are reserved on submit like every other transaction.
import type { TokenMarket } from '@/kob/plan-types';
import type { Kcc20State, TokenUtxo } from '@/kob/types';
import type { Env } from '../env';
import { errText } from '../log';
import { buildAtUrgency } from '../fees';
import { tokenRef } from '../market';
import { KAS } from '../util';
import type { BotWallet } from '../wallet';
import { RateLimiter, selectMergeChain, type ConsolidateConfig } from './consolidate-math';

const CARRIER = 10n * KAS;

let limiter: { rl: RateLimiter; max: number; windowMs: number } | null = null;
function limiterFor(cfg: ConsolidateConfig): RateLimiter {
  const windowMs = cfg.intervalSec * 1000;
  if (!limiter || limiter.max !== cfg.maxTxPerInterval || limiter.windowMs !== windowMs) limiter = { rl: new RateLimiter(cfg.maxTxPerInterval, windowMs), max: cfg.maxTxPerInterval, windowMs };
  return limiter.rl;
}

/**
 * One consolidation pass for `w` on token market `m`, keeping at least `keep` token UTXOs. Returns the number of merge transactions
 * submitted. Never throws: refusals and failures are counted and logged.
 */
export async function consolidateTokens(env: Env, w: BotWallet, m: TokenMarket, keep: number, cfg: ConsolidateConfig): Promise<number> {
  if (!cfg.enabled) return 0;
  try {
    const utxos = await w.tokenUtxos(tokenRef(env, m));
    w.stats.set(`token_utxos:${w.name}:${m.ticker}`, utxos.length);
    const rl = limiterFor(cfg);
    const key = `${w.name}:${m.covenantId}`;
    const allowed = rl.allowance(key, Date.now());
    if (allowed < 1) return 0;
    const links = selectMergeChain(utxos, { keep, slack: cfg.slack, maxInputs: m.slots.inputs, maxBatches: allowed, reserved: w.reservedKeys() });
    let done = 0;
    let merged = 0;
    let carry: TokenUtxo | null = null;
    for (const fresh of links) {
      // the previous link's output (unaccepted) leads the next merge: one UTXO at the end of the pass
      const batch: TokenUtxo[] = carry ? [carry, ...fresh] : fresh;
      const carriersIn = batch.reduce((s, u) => s + BigInt(u.amount), 0n);
      if (carriersIn < CARRIER + KAS) {
        w.stats.inc(`consolidate_skipped:carriers`);
        break;
      }
      const total = batch.reduce((s, u) => s + BigInt(u.state.amount), 0n);
      rl.record(key, Date.now());
      // housekeeping: the LOW bucket of the node's fee estimate (fee policy), never ahead of trading
      const built = await buildAtUrgency(
        env,
        'low',
        {
          action: 'sendTokens',
          token: { covenantId: m.covenantId, program: m.program },
          tokens: batch,
          recipients: [{ pubkey: w.pk, amount: total.toString(), carrier: CARRIER.toString() }],
          funding: [],
          change: w.pk,
        } as const,
        (r) => env.kob.build(r as unknown as Parameters<typeof env.kob.build>[0]),
      );
      w.stats.inc('consolidate_try');
      const r = await w.submit(built, 'consolidate', { token: m.ticker, inputs: batch.length, chained: carry !== null, base: total, freedKas: Number(carriersIn - CARRIER) / 1e8 });
      if (!r.ok) break; // the view is stale (a lost race) or the node refused: the next pass starts from fresh state
      done++;
      merged += fresh.length;
      w.stats.inc(`consolidate_merged:${m.ticker}`, fresh.length);
      w.stats.inc('consolidate_freed_sompi', Number(carriersIn - CARRIER));
      const at = built.tx.outputs.findIndex((o) => o.covenant?.covenantId === m.covenantId);
      if (at < 0) break;
      const state: Kcc20State = {
        amount: total.toString(),
        owner: w.pk,
        owner_scheme: 0,
        borrow_scheme: 0,
        borrow_guard: '00'.repeat(32),
        extension_commitment: (batch[0]!.state as Kcc20State).extension_commitment,
      };
      carry = { transactionId: r.txid, index: at, amount: built.tx.outputs[at]!.value, covenantId: m.covenantId, state };
    }
    if (done) w.log.info('consolidated token utxos', { token: m.ticker, had: utxos.length, txs: done, merged });
    return done;
  } catch (e) {
    w.stats.inc('consolidate_errors');
    w.log.warn('consolidate failed', { token: m.ticker, error: errText(e) });
    return 0;
  }
}
