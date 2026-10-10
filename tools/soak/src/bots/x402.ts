// x402 on the soak market: a merchant paywall (packages/kob-x402 `createPaywall`) with three priced resources, settled through the
// `kob-executor run --x402-config` facilitator, and a payer that pays them in turn:
//   /native  0.5 KAS                      standard-native KAS payment
//   /token   0.25 TUSD                    KCC-20 exact payment
//   /swap    0.5 KAS paid WITH TUSD       swap-and-pay: the payer sells TUSD into resting KOB bids in the payment transaction
import { createServer, type Server } from 'node:http';
import { readFileSync } from 'node:fs';
import { join } from 'node:path';
import { createPaywall } from '../../../../packages/kob-x402/src/server.ts';
import { KobX402Client } from '../../../../packages/kob-x402/src/client.ts';
import { KobX402Error } from '../../../../packages/kob-x402/src/errors.ts';
import { connectRpc, listUtxos, rpcContextProvider, rpcSubmitter } from '../../../../packages/kob-x402/src/kaspa.ts';
import { buildOffer } from '../../../../packages/kob-x402/src/offers.ts';
import { fetchInvoice, InvoiceClient, newInvoice } from '../../../../packages/kob-x402/src/invoice.ts';
import { payInvoiceWithIntent } from '../../../../packages/kob-x402/src/intent.ts';
import { createKobWasm } from '../../../../packages/kob-x402/src/wasm-node.ts';
import { MemoryArtifactStore } from '../../../../packages/kob-x402/src/artifact-store.ts';
import type { OfferSpec, NetworkId } from '../../../../packages/kob-x402/src/types.ts';
import type { SwapQuote, TokenUtxoJson } from '../../../../packages/kob-x402/src/wasm.ts';
import type { OrderView } from '@/data/indexer-types';
import type { Env } from '../env';
import { key } from '../env';
import { errText, type Logger, type Stats } from '../log';
import { readClock, tokenMarket, tokenRef } from '../market';
import { expDelay, KAS, sleep } from '../util';
import { walletFor, type BotWallet } from '../wallet';
import { payerMaxAmount, payerMaxPayAmount, soakPrices } from './x402-caps';
import { feeContext } from '../fees';
import { pickFor, type RatePick } from '@/kob/fee-policy';

const NETWORK: NetworkId = 'kaspa:testnet-10';

export class X402Bots {
  private server: Server | null = null;
  private stopped = false;
  /** the rate the payer's next transaction is built at (the x402 SDK's feeRate source) and the floor a failed build is retried at */
  private readonly payFee: { rate: number | undefined; floor: number } = { rate: undefined, floor: 100 };
  readonly payer: BotWallet;

  constructor(
    private readonly env: Env,
    private readonly log: Logger,
    readonly stats: Stats,
  ) {
    this.payer = walletFor(env, env.cfg.x402.payerKey, stats, log);
  }

  stop(): void {
    this.stopped = true;
    this.server?.close();
  }

  private merchant(): Server {
    const cfg = this.env.cfg.x402;
    const wasm = createKobWasm({ pkgDir: this.env.cfg.kobWasmDir });
    const m = tokenMarket(this.env);
    const apiKey = readFileSync(join(this.env.cfg.runPath, 'x402-merchant.key'), 'utf8').trim();
    const payTo = key(this.env, cfg.merchantKey).address;
    // the wasm resolves unknown tokens from its builtin (mainnet) registry: pin the TN10 token's program and extension explicitly
    const token = { custody: 'unconditional' as const, ticker: m.ticker, decimals: m.decimals, templateHash: m.templateHash, extensionCommitment: m.extensionCommitment };
    const price = soakPrices(m);
    const offers: Record<string, OfferSpec[]> = {
      '/native': [{ kind: 'native', amount: price['/native'].amount.toString() }],
      '/token': [{ kind: 'kcc20', asset: m.covenantId, amount: price['/token'].amount.toString(), token }],
      '/swap': [{ kind: 'swap', receive: 'kas', amount: price['/swap'].amount.toString(), payAssets: [{ asset: m.covenantId, templateHash: m.templateHash, extensionCommitment: m.extensionCommitment }] }],
    };
    const walls = Object.fromEntries(
      Object.entries(offers).map(([path, o]) => [
        path,
        createPaywall({
          wasm,
          network: NETWORK,
          payTo,
          offers: o,
          facilitator: { url: cfg.facilitatorUrl, apiKey },
          resource: { description: `KOB soak paid resource ${path}`, mimeType: 'application/json' },
          handler: (_req, paid) => {
            this.stats.inc(`x402_served:${path}`);
            return Response.json({ ok: true, path, tx: paid.transactionId, asset: paid.asset, amount: paid.amount, replayed: paid.replayed });
          },
        }).nodeListener(),
      ]),
    );
    const [host, port] = cfg.listen.split(':');
    const srv = createServer({ maxHeaderSize: 256 * 1024 }, (req, res) => {
      const path = new URL(req.url ?? '/', 'http://x').pathname;
      const wall = walls[path];
      if (!wall) return void res.writeHead(404).end('not found');
      wall(req, res);
    });
    srv.listen(Number(port), host);
    this.log.info('x402 merchant listening', { listen: cfg.listen, payTo });
    return srv;
  }

  /** swap quote from the indexer book: the best plain constant-price bids, enough token base units for `amount` sompi (payer sells TUSD) */
  private async quote(q: { amount: string }): Promise<SwapQuote> {
    const m = tokenMarket(this.env);
    const book = await this.env.indexer.book(m.covenantId, { depth: 20, aggregate: false });
    const clock = await readClock(this.env);
    const orders: Record<string, unknown>[] = [];
    let need = BigInt(q.amount) + KAS / 10n;
    for (const row of book.bids) {
      if (!('covenant_id' in row) || row.contract !== 'KobBid') continue;
      const v: OrderView | null = await this.env.indexer.order(row.covenant_id);
      const st = v?.state as { kind: string; state: Record<string, string> } | null | undefined;
      if (!v || !v.current || !st || st.kind !== 'KobBid' || v.current.value == null) continue;
      const s = st.state;
      // plain GTC bids only: no auction, no activation, no TWAP/DCA rate limit (interval: a relative lock the payment cannot meet)
      if (s.tif !== '0' || s.slope !== '0' || s.interval !== '0' || s.maxFill !== '0' || BigInt(s.activeFrom) > clock.daa || s.maker === this.payer.pk) continue;
      // protocol v3: base units. The bid's buying power is its `amount_left`; `price` is sompi per whole token (per `scale` base units). The base
      // units that pay `need` are ceil(need x scale / price) (the tip the bid also pays is ignored: conservative), at least the bid's minimum fill
      const price = BigInt(s.price);
      const scale = BigInt(s.scale);
      const avail = BigInt(v.amount_left ?? 0);
      if (avail <= 0n || price <= 0n || scale <= 0n) continue;
      const wanted = (need * scale + price - 1n) / price;
      const minFill = BigInt(s.minFill ?? 0);
      const base = wanted > minFill ? wanted : minFill;
      const take = base < avail ? base : avail;
      orders.push({
        leg: {
          kind: 'bid',
          amount: take.toString(),
          order: { transactionId: v.current.txid, index: v.current.index, amount: v.current.value, blockDaaScore: String(v.last_daa), covenantId: v.covenant_id, state: s },
          t: null,
        },
      });
      need -= (take * price) / scale; // what the bid pays for `take` base units, rounded down
      if (need <= 0n) break;
    }
    if (need > 0n) throw new KobX402Error('unsupported', 'not enough bid depth for swap-and-pay');
    return { lock_time: (clock.daa - 10n).toString(), orders } as unknown as SwapQuote;
  }

  async run(): Promise<void> {
    const cfg = this.env.cfg.x402;
    if (!cfg.enabled) return;
    this.server = this.merchant();
    const wasm = createKobWasm({ pkgDir: this.env.cfg.kobWasmDir });
    // payments are urgent (a merchant's HTTP request waits): they pay the HIGH bucket of the fee policy
    this.payFee.floor = Number(this.env.fees.policy.floor);
    // eslint-disable-next-line @typescript-eslint/no-explicit-any
    const rpc = await connectRpc(this.env.sdk as any, { url: this.env.cfg.nodeUrl, network: NETWORK });
    const m = tokenMarket(this.env);
    const tokens = async (): Promise<TokenUtxoJson[]> => (await this.payer.tokenUtxos(tokenRef(this.env))) as unknown as TokenUtxoJson[];
    const context = rpcContextProvider(rpc, { tokens, quote: (q) => this.quote(q) });
    const pk = key(this.env, cfg.payerKey);
    // 0.2 KAS at the floor; a dynamic fee at a busy moment is several times that, so the ceiling follows the policy's per-transaction cap
    const maxFee = this.env.fees.policy.dynamic && this.env.fees.policy.maxFeeSompi > 20_000_000n ? this.env.fees.policy.maxFeeSompi : 20_000_000n;
    const client = (caps: ConstructorParameters<typeof KobX402Client>[0]['capabilities']) =>
      new KobX402Client({
        wasm,
        network: NETWORK,
        payerAddress: pk.address,
        privateKeys: [pk.secretKey],
        tokens: [{ covenantId: m.covenantId, templateHash: m.templateHash, extensionCommitment: m.extensionCommitment, custody: 'unconditional', ticker: m.ticker, decimals: m.decimals }],
        context,
        store: new MemoryArtifactStore(),
        // eslint-disable-next-line @typescript-eslint/no-explicit-any
        submit: rpcSubmitter(this.env.sdk as any, rpc),
        capabilities: caps,
        maxFeeSompi: maxFee.toString(),
        maxPay: { [m.covenantId]: payerMaxPayAmount(m) },
        // every payment at the policy's HIGH rate (pickPayFee sets it before each one); one that cannot be built at it is rebuilt at the floor
        feeRate: () => this.payFee.rate,
        feeRateFloor: this.payFee.floor,
      });
    const bal = async () => ({ [m.covenantId]: (await this.payer.tokenBalance(tokenRef(this.env))).toString() });
    const base = `http://${cfg.listen}`;
    const kinds = cfg.invoiceIntent?.enabled ? ['/native', '/token', '/swap', 'invoice'] : ['/native', '/token', '/swap'];
    const merchantKey = readFileSync(join(this.env.cfg.runPath, 'x402-merchant.key'), 'utf8').trim();
    const invoices = new InvoiceClient({ url: cfg.facilitatorUrl, apiKey: merchantKey, allowInsecureHttp: true, payTimeoutMs: 90_000 });
    let i = 0;
    await sleep(30_000);
    while (!this.stopped) {
      // the payer submits through the x402 SDK, not a BotWallet, so it waits on the gate itself (see indexer-gate.ts for why even /native waits)
      if (!(await this.env.gate?.waitOpen(() => this.stopped) ?? true)) break;
      const path = kinds[i++ % kinds.length];
      if (path === 'invoice') {
        await this.payInvoice(wasm, rpc, invoices, tokens);
        await sleep(expDelay(cfg.meanIntervalSec, 20));
        continue;
      }
      try {
        const pick = await this.pickPayFee();
        // explicit spend ceilings per merchant asset: the SDK pays nothing without one
        const maxAmount = payerMaxAmount(m, maxFee);
        const caps = path === '/native' ? { kasOnly: true, maxAmount } : { tokens: await bal(), allowSwap: path === '/swap', maxAmount };
        const { response, payment } = await client(caps).paidFetch(base + path);
        const body = await response.text();
        if (response.status === 200 && payment) {
          this.stats.inc(`x402_paid:${path}`);
          // the SDK's own retry (re-send of an unknown outcome, rebuild after an order lost to another taker): counted apart
          if (payment.attempts > 1 || payment.resends > 0) this.stats.inc(`x402_retried:${path}`);
          this.log.info('x402 paid', { path, tx: payment.transactionId, amount: payment.amount, asset: payment.asset, kind: payment.kind, attempts: payment.attempts, resends: payment.resends });
          this.recordPayment(path, payment.transactionId, payment.amount, payment.asset, pick);
        } else {
          this.stats.inc(`x402_unpaid:${path}:${response.status}`);
          this.log.warn('x402 not paid', { path, status: response.status, body: body.slice(0, 300) });
        }
      } catch (e) {
        const code = e instanceof KobX402Error ? `${e.code}${e.diagnostic ? `/${e.diagnostic}` : ''}` : 'error';
        this.stats.inc(`x402_error:${path}:${code}`);
        this.log.warn('x402 payment failed', { path, code, error: errText(e), retryable: e instanceof KobX402Error ? e.retryable : undefined, attempts: e instanceof KobX402Error ? e.attempts?.map((a) => a.outcome) : undefined });
      }
      await sleep(expDelay(cfg.meanIntervalSec, 20));
    }
  }

  /**
   * One invoice paid by an intent (docs/ops/executor.md A.7): the merchant registers an invoice of `amountKas` whose only entry is an intent
   * swap offer paid with TUSD; the payer signs the router intent's creation and posts it to `/invoices/{id}/pay`; the facilitator broadcasts
   * it and executes the intent against the TUSD bids as a keeper. The intent handle is logged before anything is sent (persist).
   */
  private async payInvoice(wasm: ReturnType<typeof createKobWasm>, rpc: Awaited<ReturnType<typeof connectRpc>>, invoices: InvoiceClient, tokens: () => Promise<TokenUtxoJson[]>): Promise<void> {
    const cfg = this.env.cfg.x402;
    const m = tokenMarket(this.env);
    const amount = BigInt(Math.round((cfg.invoiceIntent?.amountKas ?? 20) * 1e8));
    const maxSell = BigInt(Math.round((cfg.invoiceIntent?.maxSellTusd ?? 2) * 10 ** m.decimals));
    const t0 = Date.now();
    try {
      const pick = await this.pickPayFee();
      const offer = buildOffer(
        { kind: 'swap', mode: 'intent', receive: 'kas', amount: amount.toString(), payAssets: [{ asset: m.covenantId, templateHash: m.templateHash, extensionCommitment: m.extensionCommitment }] },
        { wasm, network: NETWORK, payTo: key(this.env, cfg.merchantKey).address, maxTimeoutSeconds: 300, finality: 'accepted' },
      );
      const inv = newInvoice({ network: NETWORK, reference: `soak-${t0}`, expiresAtMs: t0 + 10 * 60_000, memo: 'KOB soak invoice paid by intent', accepts: [offer] });
      const reg = await invoices.create(inv);
      this.stats.inc('x402_invoice_registered');
      const fetched = await fetchInvoice(invoices.urlOf(reg.id), { nowMs: Date.now(), wasm, allowInsecureHttp: true });
      const pk = key(this.env, cfg.payerKey);
      const { payment, settlement, sends } = await payInvoiceWithIntent(
        wasm,
        invoices,
        fetched,
        { payAsset: m.covenantId, privateKeys: [pk.secretKey], utxos: await listUtxos(rpc, pk.address), tokenUtxos: await tokens(), nowMs: Date.now(), options: { maxSell: maxSell.toString(), expiresInMs: 240_000, feeRate: Number(pick.rate) }, feeRateFloor: this.payFee.floor },
        { persist: (p) => this.log.info('x402 intent created', { invoice: reg.id, intent: p.intent.intent, actor: p.intent.actor, payerSpent: p.payerSpent }) },
      );
      let status = settlement.success ? 'paid' : String(settlement.errorReason ?? 'failed');
      if (!settlement.success && settlement.errorReason === 'settlement_pending') status = (await invoices.awaitPaid(reg.id, { timeoutMs: 240_000, intervalMs: 3_000 })).status;
      if (settlement.success || status === 'paid') {
        this.stats.inc('x402_paid:invoice-intent');
        const creation = (settlement.extensions as { kob?: { intent?: { creation?: string } } } | undefined)?.kob?.intent?.creation;
        this.log.info('x402 invoice paid by intent', { invoice: reg.id, tx: settlement.transaction, creation, payerSpent: payment.payerSpent, sends, ms: Date.now() - t0 });
        // the facilitator ledger keys an intent entry by its creation id (docs/ops/executor.md A.7): the checker looks it up by that
        this.recordPayment('invoice-intent', creation ?? settlement.transaction, amount.toString(), 'KAS', pick);
      } else {
        this.stats.inc(`x402_unpaid:invoice-intent:${status}`);
        this.log.warn('x402 invoice not paid', { invoice: reg.id, status, settlement: JSON.stringify(settlement).slice(0, 400) });
      }
    } catch (e) {
      const code = e instanceof KobX402Error ? `${e.code}${e.diagnostic ? `/${e.diagnostic}` : ''}` : 'error';
      this.stats.inc(`x402_error:invoice-intent:${code}`);
      this.log.warn('x402 invoice payment failed', { code, error: errText(e) });
    }
  }

  /** the checker verifies every recorded payment settled exactly once (run/state/x402-payments.jsonl) */
  /** the HIGH-bucket rate for the next payer transaction (the node's estimate through the cached oracle; the floor when it has none) */
  private async pickPayFee(): Promise<RatePick> {
    const pick = pickFor(await feeContext(this.env), 'high');
    this.payFee.rate = Number(pick.rate);
    return pick;
  }

  private recordPayment(path: string, txid: string, amount: string, asset: string, pick?: RatePick): void {
    const fee = pick ? { urgency: pick.urgency, feeRate: pick.rate.toString(), feeSource: pick.source, ...(pick.bucketFeerate !== undefined ? { bucketFeerate: pick.bucketFeerate } : {}) } : {};
    const line = JSON.stringify({ ts: Date.now(), path, txid, amount, asset, ...fee }) + '\n';
    import('node:fs').then((fs) => fs.appendFileSync(join(this.env.cfg.runPath, 'state', 'x402-payments.jsonl'), line));
  }
}
