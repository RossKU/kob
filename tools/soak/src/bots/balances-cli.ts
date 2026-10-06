// `soak.mjs balances`: KAS and token balances of every soak key (one JSON object on stdout; no secrets): the node's KAS total, the spendable
// KAS the bots' wallets would spend, and per soak token the holdings the token tracker proves on the node (count, amount, KAS locked in the carriers).
import type { Env } from '../env';
import { Stats } from '../log';
import { markets, tokenRef } from '../market';
import { walletFor } from '../wallet';

export async function balances(env: Env): Promise<Record<string, unknown>> {
  const stats = new Stats(`${env.cfg.runPath}/stats`, 'balances-cli');
  const tokens: [string, ReturnType<typeof tokenRef>][] = markets(env).map((m) => [m.ticker, tokenRef(env, m)]);
  const out: Record<string, unknown> = {};
  for (const name of Object.keys(env.keys)) {
    const w = walletFor(env, name, stats);
    const total = (await env.node.getUtxosByAddresses([w.address])).reduce((s, u) => s + BigInt(u.amount), 0n);
    const row: Record<string, unknown> = { kasTotalSompi: total.toString(), kasSpendableSompi: (await w.kasBalance()).toString() };
    for (const [ticker, ref] of tokens) {
      const us = await w.tokenUtxos(ref);
      row[ticker] = { utxos: us.length, base: us.reduce((s, u) => s + BigInt(u.state.amount), 0n).toString(), carrierSompi: us.reduce((s, u) => s + BigInt(u.amount), 0n).toString() };
    }
    out[name] = row;
  }
  return out;
}
