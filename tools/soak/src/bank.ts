// The bank: mined coinbase lands on the bank key; the bank consolidates the coinbase dust and tops up every wallet (bots, executors)
// in chunks, using the official SDK's transaction generator (plain P2PK KAS transfers).
import type { Env } from './env';
import { key } from './env';
import { errText, type Logger, type Stats } from './log';
import { KAS, kas } from './util';
import { consolidationOutputs, marginCoins, marginFor, planWithFeePolicy, RecentSpends } from './bank-plan';
import { createUtxoService } from '@/data/utxos';
import { pickFor } from '@/kob/fee-policy';
import { feeContext } from './fees';

const MATURITY = 1000n + 50n;

export class Bank {
  // eslint-disable-next-line @typescript-eslint/no-explicit-any
  private readonly sdk: any;
  // eslint-disable-next-line @typescript-eslint/no-explicit-any
  private rpc: any = null;
  private busy = false;
  /** inputs and payees of transactions the node has not confirmed yet (bank-plan.ts) */
  private readonly recent = new RecentSpends();
  constructor(
    private readonly env: Env,
    private readonly log: Logger,
    private readonly stats: Stats,
  ) {
    this.sdk = env.sdk;
  }

  private async client() {
    if (this.rpc?.isConnected) return this.rpc;
    const rpc = new this.sdk.RpcClient({ url: this.env.cfg.nodeUrl, networkId: this.env.cfg.network, encoding: this.sdk.Encoding.SerdeJson });
    await rpc.connect({ blockAsyncConnect: true, timeoutDuration: 15_000 });
    this.rpc = rpc;
    return rpc;
  }

  /** spendable plain KAS of a key per the node (what the planners would see) */
  private async spendable(pubkey: string): Promise<bigint> {
    const svc = createUtxoService({ node: this.env.node, sdk: this.env.sdk, network: this.env.cfg.network });
    return (await svc.fundingFor(pubkey)).reduce((s, u) => s + BigInt(u.amount), 0n);
  }

  async tick(): Promise<void> {
    if (this.busy) return;
    this.busy = true;
    try {
      await this.run();
    } catch (e) {
      this.stats.inc('bank_errors');
      this.log.warn('bank tick failed', { error: errText(e) });
    } finally {
      this.busy = false;
    }
  }

  private async run(): Promise<void> {
    const cfg = this.env.cfg.bank;
    const bank = key(this.env, cfg.key);
    const rpc = await this.client();
    const { virtualDaaScore } = await rpc.getBlockDagInfo();
    const daa = BigInt(virtualDaaScore);
    const { entries } = await rpc.getUtxosByAddresses({ addresses: [bank.address] });
    // eslint-disable-next-line @typescript-eslint/no-explicit-any
    const now = Date.now();
    const mature = entries.filter(
      (e: any) => !(e.isCoinbase && daa - BigInt(e.blockDaaScore) < MATURITY) && !e.entry?.covenantId && !this.recent.isSpent(RecentSpends.key(e.outpoint.transactionId, e.outpoint.index), now),
    );
    // eslint-disable-next-line @typescript-eslint/no-explicit-any
    // the generator refuses huge input sets (mass): the largest 80 coins per tick, the rest next tick
    // eslint-disable-next-line @typescript-eslint/no-explicit-any
    mature.sort((a: any, b: any) => (BigInt(b.amount) > BigInt(a.amount) ? 1 : -1));
    mature.splice(80);
    const total = mature.reduce((s: bigint, e: any) => s + BigInt(e.amount), 0n);
    this.stats.set('bank_spendable_kas', kas(total));
    this.stats.set('bank_utxos', mature.length);

    const outputs: { address: string; amount: bigint }[] = [];
    let budget = total - 5n * KAS;
    const chunk = BigInt(cfg.chunkKas) * KAS;
    for (const [name, w] of Object.entries(cfg.wallets)) {
      const k = this.env.keys[name];
      if (!k) continue;
      if (this.recent.isPaid(k.address, now)) continue; // its last top-up is still unconfirmed
      const have = await this.spendable(k.publicKey);
      if (have >= BigInt(w.min) * KAS) continue;
      let need = BigInt(w.target) * KAS - have;
      while (need > 0n && budget >= chunk && outputs.length < 12) {
        outputs.push({ address: k.address, amount: chunk });
        need -= chunk;
        budget -= chunk;
      }
      if (need > 0n) this.stats.inc('bank_short');
    }
    if (outputs.length === 0 && mature.length <= cfg.consolidateAbove) return;
    // nothing to pay out: merge the coins into one (the generator would otherwise spend a single input back to the bank each tick)
    const consolidating = outputs.length === 0;
    if (consolidating) outputs.push(...consolidationOutputs(bank.address, total));
    const privateKey = new this.sdk.PrivateKey(bank.secretKey);
    // payouts and consolidations are housekeeping: the LOW bucket of the node's fee estimate (fee policy). The SDK generator takes the rate as
    // `feeRate` (sompi per gram; absent = the 100 relay floor) and `priorityFee` stays 0n. A plan whose change lands in the generator's
    // storage-mass dead band is refused with `Mass calculation error`: see bank-plan.ts
    const ctx = await feeContext(this.env);
    const pick = pickFor(ctx, 'low');
    const planned = await planWithFeePolicy<{ transactions: any[]; summary?: any }, any[]>(
      (outs, rate, coins) =>
        this.sdk.createTransactions({
          entries: coins ?? mature,
          outputs: outs,
          changeAddress: bank.address,
          priorityFee: 0n,
          feeRate: Number(rate),
          networkId: this.env.cfg.network,
        }),
      outputs,
      ctx.policy,
      pick.rate,
      undefined,
      // the coin set that cannot stop in the storage-mass dead band (bank-plan.ts), tried after the plan itself is refused
      (outs, rate) => {
        const set = marginCoins(mature, outs.reduce((s2, o) => s2 + o.amount, 0n), marginFor(rate));
        return set ? [set] : [];
      },
    );
    const rate = planned.rate;
    if (planned.fellBackToFloor) this.log.info('bank plan failed at the estimated rate, paid at the floor', { picked: pick.rate.toString() });
    this.stats.set('bank_fee_rate', Number(rate));
    this.stats.inc(`bank_fee_source:${pick.source === 'estimate' && !planned.fellBackToFloor ? 'estimate' : 'floor'}`);
    if (planned.retries > 0) {
      this.stats.inc('bank_mass_retries', planned.retries);
      this.log.info('bank payout re-planned after a storage-mass refusal', { retries: planned.retries, outputs: planned.outputs.length, planned: outputs.length });
    }
    const { transactions, summary } = planned.result;
    for (const tx of transactions) {
      tx.sign([privateKey]);
      const id = await tx.submit(rpc);
      this.recent.spend(
        tx.serializeToObject().inputs.map((i: { transactionId: string; index: number }) => RecentSpends.key(i.transactionId, i.index)),
        now,
      );
      this.stats.inc('bank_txs');
      this.log.info('bank tx', { txid: id, rate: rate.toString(), urgency: 'low', feeSource: pick.source, fee: tx.feeAmount?.toString?.() });
    }
    if (!consolidating) for (const o of planned.outputs) this.recent.pay(o.address, now);
    // (a consolidation pays the bank itself: nothing is sent out)
    const sent = consolidating ? 0n : planned.outputs.reduce((s, o) => s + o.amount, 0n);
    this.stats.inc('bank_sent_kas', Number(sent / KAS));
    this.log.info('bank paid out', { outputs: consolidating ? 0 : planned.outputs.length, consolidated: consolidating ? mature.length : 0, sent: kas(sent), txs: transactions.length, fees: summary?.fees?.toString?.() });
  }
}
