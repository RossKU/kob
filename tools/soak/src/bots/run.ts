// The bots process: price feeds, bank, market maker (one ladder per soak book), traders, x402 merchant + payer. Stats are flushed to
// run/stats/bots.json.
import { genesisOf, type Env, type GenesisOutputs } from '../env';
import { symbolOfUrl } from '../config';
import { logger, Stats } from '../log';
import { assetMarkets, tokenMarket, SOAK_PROGRAM, type AssetBook } from '../market';
import { Bank } from '../bank';
import { DAA_PER_SECOND, DEFAULT_MAX_LAG_SECS, IndexerGate } from '../indexer-gate';
import { DerivedFeed, PriceFeed, RefRecorder, UsdFeed } from '../price';
import { sleep, unitsOf } from '../util';
import { walletFor } from '../wallet';
import type { TokenMarket } from '@/kob/plan-types';
import { DEFAULT_ASSET_FANOUT, fanOut, fanoutTarget } from './fanout';
import { MarketMaker } from './mm';
import { Trader } from './traders';
import { X402Bots } from './x402';

const log = logger('bots');

/**
 * Re-registers the genesis outputs of a token in the holders' trackers (`setup` adds them, but a bots process running at the same time
 * rewrites the tracker files from its own memory): the tracker verifies every candidate on the node, so a spent one is simply dropped.
 */
function seedGenesis(env: Env, stats: Stats, m: TokenMarket, g: GenesisOutputs | undefined): void {
  for (const [name, o] of Object.entries(g ?? {})) {
    const k = env.keys[name];
    if (!k) continue;
    const w = walletFor(env, name, stats, log.child(name));
    w.tracker.add(w.pk, {
      transactionId: o.transactionId,
      index: o.index,
      tokenCovId: m.covenantId,
      program: m.program as typeof SOAK_PROGRAM,
      state: { amount: o.amount, owner: k.publicKey, owner_scheme: 0, borrow_scheme: 0, borrow_guard: '00'.repeat(32), extension_commitment: m.extensionCommitment ?? '00'.repeat(32) },
      carrier: o.carrier,
    });
  }
}

export async function runBots(env: Env): Promise<void> {
  const stats = new Stats(`${env.cfg.runPath}/stats`, 'bots');
  // trading (every BotWallet.submit, the bot loops, the x402 payer) pauses while the indexer is not following; the bank's plain KAS payouts do not
  // (also while it lags by at most `maxLagSecs`, default 30 s: the executors' `max_lag_secs`, written by the supervisor)
  const maxLagDaa = Math.round((env.cfg.maxLagSecs ?? DEFAULT_MAX_LAG_SECS) * DAA_PER_SECOND);
  env.gate = new IndexerGate({ health: () => env.indexer.health(), log: log.child('gate'), stats, maxLagDaa });
  stats.set('gate_max_lag_daa', maxLagDaa);
  await env.gate.check(true);
  const m = tokenMarket(env);
  // every good Binance poll is recorded for the UI server's reference overlay (price.ts RefRecorder)
  const rec = new RefRecorder(env.cfg.runPath);
  const feed = new PriceFeed(env.cfg, log.child('price'), stats, rec.onPrice);
  await feed.start();
  const assets: AssetBook[] = [];
  for (const a of assetMarkets(env)) {
    const usd = new UsdFeed(a.cfg.priceUrl, env.cfg.price.pollMs, env.cfg.price.staleMs, a.m.ticker, log.child('price'), stats, rec.onPrice);
    await usd.start();
    assets.push({ ...a, feed: new DerivedFeed(feed, usd) });
    seedGenesis(env, stats, a.m, genesisOf(env.state, a.slot));
  }
  RefRecorder.writeSymbols(
    env.cfg.runPath,
    [
      { symbol: symbolOfUrl(env.cfg.price.url) ?? 'KASUSDT', ticker: 'KAS', covenantId: null, role: 'kas' },
      ...assets.map((a) => ({ symbol: symbolOfUrl(a.cfg.priceUrl) ?? a.m.ticker, ticker: a.m.ticker, covenantId: a.m.covenantId, role: 'asset' as const })),
    ],
    m.covenantId,
  );
  const bank = new Bank(env, log.child('bank'), stats);
  void (async () => {
    for (;;) {
      await bank.tick();
      await sleep(env.cfg.bank.intervalSec * 1000);
    }
  })();
  setInterval(() => {
    for (const a of assets) {
      const r = a.feed.get();
      if (r) stats.set(`ref_per_token_sompi:${a.m.ticker}`, r.perToken.toString());
    }
    stats.flush();
  }, 15_000);
  log.info('bots starting', { token: m.ticker, covenantId: m.covenantId, ref: feed.get(), assets: assets.map((a) => ({ ticker: a.m.ticker, covenantId: a.m.covenantId, ref: a.feed.get() })) });

  // token fan-out before trading (and every 10 minutes): several token UTXOs per bot and token (TUSD: 300 per market-maker output, 60 per
  // trader output, 20 per payer output; an asset token: its `fanout` config, whole tokens per output)
  const tusd = (whole: string) => unitsOf(whole, m.decimals);
  const fan = async () => {
    await fanOut(env, walletFor(env, env.cfg.mm.key, stats, log.child('mm')), fanoutTarget(env, 'mm', m), tusd('300'), m);
    for (const t of env.cfg.traders) await fanOut(env, walletFor(env, t.key, stats, log.child(t.key)), fanoutTarget(env, 'trader', m), tusd('60'), m);
    if (env.cfg.x402.enabled) await fanOut(env, walletFor(env, env.cfg.x402.payerKey, stats, log.child('payer')), 3, tusd('20'), m);
    for (const a of assets) {
      const fo = a.cfg.fanout ?? DEFAULT_ASSET_FANOUT;
      await fanOut(env, walletFor(env, env.cfg.mm.key, stats, log.child('mm')), fanoutTarget(env, 'mm', a.m), unitsOf(fo.mm[1], a.m.decimals), a.m);
      for (const t of env.cfg.traders) await fanOut(env, walletFor(env, t.key, stats, log.child(t.key)), fanoutTarget(env, 'trader', a.m), unitsOf(fo.traders[1], a.m.decimals), a.m);
    }
  };
  await env.gate.waitOpen();
  await fan();
  setInterval(() => void fan(), 10 * 60_000);

  void new MarketMaker(env, feed, m, log.child('mm'), stats).run();
  for (const a of assets) void new MarketMaker(env, a.feed, a.m, log.child(`mm@${a.m.ticker}`), stats, a.cfg.mm ?? {}, false).run();
  // the traders start once the market maker has quoted
  await sleep(60_000);
  for (const t of env.cfg.traders) void new Trader(env, t, feed, log.child(t.key), stats, assets).run();
  if (env.cfg.x402.enabled) void new X402Bots(env, log.child('x402'), stats).run();
  await new Promise(() => {});
}
