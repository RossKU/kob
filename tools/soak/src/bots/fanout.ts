// Keeps a bot's tokens spread over several UTXOs (one token input per placement; a single UTXO would serialise every sell-side
// placement behind the acceptance of the previous one). The transfer also moves genesis outputs, which the indexer never lists, into
// indexed holdings.
import { slotOfToken, type Env } from '../env';
import { errText } from '../log';
import type { TokenMarket } from '@/kob/plan-types';
import type { Kcc20State, TokenUtxo } from '@/kob/types';
import { buildAtUrgency } from '../fees';
import { tokenMarket, tokenRef, type Which } from '../market';
import { KAS } from '../util';
import type { BotWallet } from '../wallet';
import { fanoutPlan } from './consolidate-math';

const CARRIER = 10n * KAS;

/** the fan-out of an asset token without a `fanout` config: outputs and whole tokens per output */
export const DEFAULT_ASSET_FANOUT: { mm: [number, string]; traders: [number, string] } = { mm: [8, '0.3'], traders: [4, '0.06'] };

/** the fan-out target (token UTXOs kept per holder) of a role on a market: what token consolidation must never merge below */
export function fanoutTarget(env: Env, role: 'mm' | 'trader', m: TokenMarket): number {
  const slot = slotOfToken(env.state, m.covenantId);
  if (!slot || slot === 'token') return role === 'mm' ? 8 : 4;
  const fo = env.cfg[slot]?.fanout ?? DEFAULT_ASSET_FANOUT;
  return role === 'mm' ? fo.mm[0] : fo.traders[0];
}

/**
 * Splits the largest token UTXO of `w` into outputs of `each` base units until it holds `want` token UTXOs: a chain of transfers within
 * the program's token outputs (`fanoutPlan`; 2 pieces + the token change per transaction with KOB's standard 3 / 3 program), each next
 * transfer spending the previous one's token change.
 */
export async function fanOut(env: Env, w: BotWallet, want: number, each: bigint, which?: Which): Promise<boolean> {
  try {
    const m = tokenMarket(env, which);
    const utxos = await w.tokenUtxos(tokenRef(env, m));
    if (utxos.length >= want) return false;
    let big: TokenUtxo | undefined = [...utxos].sort((a, b) => (BigInt(b.state.amount) > BigInt(a.state.amount) ? 1 : -1))[0];
    if (!big) return false;
    const plan = fanoutPlan({ amount: BigInt(big.state.amount), each, need: want - utxos.length, maxOutputs: m.slots.outputs, maxTx: 8 });
    let done = 0;
    for (const n of plan) {
      const input: TokenUtxo = big;
      const funding = (await w.funding()).slice(0, 20);
      if (funding.reduce((s, u) => s + BigInt(u.amount), 0n) < BigInt(n + 1) * CARRIER + KAS) break;
      const left = BigInt(input.state.amount) - BigInt(n) * each;
      // housekeeping: the LOW bucket of the node's fee estimate (fee policy)
      const built = await buildAtUrgency(
        env,
        'low',
        {
          action: 'sendTokens',
          token: { covenantId: m.covenantId, program: m.program },
          tokens: [input],
          recipients: Array.from({ length: n }, () => ({ pubkey: w.pk, amount: each.toString(), carrier: CARRIER.toString() })),
          tokenChange: w.pk,
          tokenChangeCarrier: CARRIER.toString(),
          funding,
          change: w.pk,
        },
        (r) => env.kob.build(r as unknown as Parameters<typeof env.kob.build>[0]),
      );
      const r = await w.submit(built, 'fanout', { token: m.ticker, outputs: n, chained: done > 0 });
      if (!r.ok || !r.txid) break;
      done++;
      if (left <= 0n) break;
      // the token change (the token output after the n pieces) leads the next transfer
      const tokenOuts = built.tx.outputs.map((o, i) => [o, i] as const).filter(([o]) => o.covenant?.covenantId === m.covenantId);
      const ch = tokenOuts[n];
      if (!ch) break;
      const state: Kcc20State = { ...(input.state as Kcc20State), amount: left.toString(), owner: w.pk };
      big = { transactionId: r.txid, index: ch[1], amount: ch[0].value, covenantId: m.covenantId, state };
    }
    return done > 0;
  } catch (e) {
    w.log.warn('fanout failed', { error: errText(e) });
    return false;
  }
}
