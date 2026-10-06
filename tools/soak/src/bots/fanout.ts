// Keeps a bot's tokens spread over several UTXOs (one token input per placement; a single UTXO would serialise every sell-side
// placement behind the acceptance of the previous one). The transfer also moves genesis outputs, which the indexer never lists, into
// indexed holdings.
import { slotOfToken, type Env } from '../env';
import { errText } from '../log';
import type { TokenMarket } from '@/kob/plan-types';
import { buildAtUrgency } from '../fees';
import { tokenMarket, tokenRef, type Which } from '../market';
import { KAS } from '../util';
import type { BotWallet } from '../wallet';

/** the fan-out of an asset token without a `fanout` config: outputs and whole tokens per output */
export const DEFAULT_ASSET_FANOUT: { mm: [number, string]; traders: [number, string] } = { mm: [8, '0.3'], traders: [4, '0.06'] };

/** the fan-out target (token UTXOs kept per holder) of a role on a market: what token consolidation must never merge below */
export function fanoutTarget(env: Env, role: 'mm' | 'trader', m: TokenMarket): number {
  const slot = slotOfToken(env.state, m.covenantId);
  if (!slot || slot === 'token') return role === 'mm' ? 8 : 4;
  const fo = env.cfg[slot]?.fanout ?? DEFAULT_ASSET_FANOUT;
  return role === 'mm' ? fo.mm[0] : fo.traders[0];
}

/** Splits the largest token UTXO of `w` into outputs of `each` base units until it holds `want` token UTXOs (at most 7 per transaction). */
export async function fanOut(env: Env, w: BotWallet, want: number, each: bigint, which?: Which): Promise<boolean> {
  try {
    const m = tokenMarket(env, which);
    const utxos = await w.tokenUtxos(tokenRef(env, m));
    if (utxos.length >= want) return false;
    const big = [...utxos].sort((a, b) => (BigInt(b.state.amount) > BigInt(a.state.amount) ? 1 : -1))[0];
    if (!big) return false;
    const amount = BigInt(big.state.amount);
    if (each <= 0n) return false;
    const n = Math.min(want - utxos.length + 1, 7, Number(amount / each));
    if (n < 2) return false;
    const funding = (await w.funding()).slice(0, 20);
    if (funding.reduce((s, u) => s + BigInt(u.amount), 0n) < BigInt(n + 1) * 10n * KAS + KAS) return false;
    // housekeeping: the LOW bucket of the node's fee estimate (fee policy)
    const built = await buildAtUrgency(
      env,
      'low',
      {
        action: 'sendTokens',
        token: { covenantId: m.covenantId, program: m.program },
        tokens: [big],
        recipients: Array.from({ length: n }, () => ({ pubkey: w.pk, amount: each.toString(), carrier: (10n * KAS).toString() })),
        tokenChange: w.pk,
        tokenChangeCarrier: (10n * KAS).toString(),
        funding,
        change: w.pk,
      },
      (r) => env.kob.build(r as unknown as Parameters<typeof env.kob.build>[0]),
    );
    const r = await w.submit(built, 'fanout', { token: m.ticker, outputs: n });
    return r.ok;
  } catch (e) {
    w.log.warn('fanout failed', { error: errText(e) });
    return false;
  }
}
