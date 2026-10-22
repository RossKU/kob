// The soak tokens as markets: registry entries (generated at setup), TokenMarkets, planner environments, the books. The primary token
// (TUSD, `state.token`) and, when configured, the asset tokens beside it (`state.token2` TETH, `state.token3` TBTC): all KCC-20 on KOB's
// standard program, the KCC-20 reference with 3 token inputs / 3 token outputs per transaction (`KCC20Ref`, registry `kcc20-ref-3x3`).
import { mkdirSync, writeFileSync } from 'node:fs';
import { join } from 'node:path';
import { bookFromIndexer, kasReferenceOf, ownOrderRefs } from '@/app/env';
import { impliedPairRate, isPairOrderView, ownPairOrderRefs, pairBookToBookView } from '@/kob/pair';
import type { CancelEnv } from '@/kob/cancel';
import { isPairEnv, type BookView, type Clock, type PairPlanEnv, type PlanEnv, type TokenMarket } from '@/kob/plan-types';
import { readPairReferences, readReferences } from '@/data/indexer-crosscheck';
import { toTokenMarket } from '@/kob/token-market';
import type { TokenRef } from '@/data/token-tracker';
import type { OrderView } from '@/data/indexer-types';
import { ASSET_SLOTS, TOKEN_SLOTS, type AssetSlot, type ExternalTokenConfig, type ExternalTradeConfig, type Token2Config, type TokenSlot } from './config';
import { externalRegistryToken, soakTemplates, TEMPLATE_STANDARD } from './registry-templates';
import { issuedOf, type Env, type IssuedToken } from './env';
import { feeContext } from './fees';
import type { RefSource } from './price';
import type { BotWallet } from './wallet';

/** The program every soak token is issued with (kob-wasm `issue`, its default): KOB's standard KCC-20 program. */
export const SOAK_PROGRAM = 'KCC20Ref' as const;

/**
 * Refuses a soak state whose tokens were issued under another program (the 8 / 8 program of the soak before 2026-10-10): this build lists,
 * builds and matches only the pinned templates, so such a token is re-issued (remove its entries from run/state.json, then `soak setup`).
 */
export function checkSoakProgram(t: IssuedToken): void {
  if (t.templateHash !== TEMPLATE_STANDARD.template_hash) {
    throw new Error(
      `the soak token ${t.ticker} (${t.covenantId}) was issued under template ${t.templateHash}, not KOB's standard program ` +
        `${SOAK_PROGRAM} (${TEMPLATE_STANDARD.template_hash}): re-issue it (remove its entries from run/state.json and run soak setup)`,
    );
  }
}

const TUSD_DESCRIPTION = 'TN10 soak test token (tUSD): 1 TUSD tracks 1 USD worth of KAS (Binance KASUSDT reference, off chain). Fixed supply, no value.';

function registryToken(t: IssuedToken, description: string): Record<string, unknown> {
  checkSoakProgram(t);
  return {
    ticker: t.ticker,
    name: t.name,
    family: 'kcc20',
    covenant_id: t.covenantId,
    template_id: TEMPLATE_STANDARD.id,
    extension_commitment: t.extensionCommitment,
    extension_class: 'fixed-supply-standard',
    decimals: t.decimals,
    // no `lot_size` / `tick`: legacy protocol v2 fields that kob_protocol::registry reads and ignores (protocol v3: any amount of base
    // units, any whole-sompi price per whole token); the web app then prices on a 1-sompi step
    max_token_inputs: TEMPLATE_STANDARD.max_token_inputs,
    max_token_outputs: TEMPLATE_STANDARD.max_token_outputs,
    verified: true,
    display: { description },
  };
}

/** The registry JSON of the soak (testnet-10, the soak tokens), also the executors' `--tokens` allowlist. */
export function soakRegistry(env: Env): unknown {
  const t = env.state.token;
  if (!t) throw new Error('the soak token is not issued yet (run `soak setup`)');
  const tokens = [registryToken(t, TUSD_DESCRIPTION)];
  for (const slot of ASSET_SLOTS) {
    const ti = env.state[slot];
    if (ti) tokens.push(registryToken(ti, ti.description ?? env.cfg[slot]?.description ?? `TN10 soak test token ${ti.ticker}. Fixed supply, no value.`));
  }
  const external = env.cfg.externalTokens ?? [];
  for (const t of external) tokens.push(externalRegistryToken(t));
  return { schema_version: 1, network: 'testnet-10', templates: soakTemplates(external, env.cfg.testPrograms ?? []), tokens };
}

export function writeRegistry(env: Env): string {
  const dir = join(env.cfg.runPath, 'registry');
  mkdirSync(dir, { recursive: true });
  const p = join(dir, 'tokens.json');
  writeFileSync(p, JSON.stringify(soakRegistry(env), null, 2) + '\n');
  return p;
}

/** Which soak market: omitted = the primary token (TUSD); a slot ('token', 'token2', 'token3'); a covenant id; or a TokenMarket. */
export type Which = TokenMarket | string | undefined;

const cachedMarkets = new Map<string, TokenMarket>();


function marketOfIssued(env: Env, t: IssuedToken): TokenMarket {
  let m = cachedMarkets.get(t.covenantId);
  if (!m) {
    const reg = soakRegistry(env) as { tokens: Parameters<typeof toTokenMarket>[1][]; templates: Parameters<typeof toTokenMarket>[2] };
    const entry = reg.tokens.find((x) => x.covenant_id === t.covenantId);
    if (!entry) throw new Error(`${t.ticker} is not in the soak registry`);
    m = toTokenMarket(env.kob, entry, reg.templates);
    cachedMarkets.set(t.covenantId, m);
  }
  return m;
}

/** the issued soak markets, primary first */
export function markets(env: Env): TokenMarket[] {
  const out: TokenMarket[] = [];
  for (const s of TOKEN_SLOTS) {
    const t = issuedOf(env.state, s);
    if (t) out.push(marketOfIssued(env, t));
  }
  return out;
}

export function tokenMarket(env: Env, which?: Which): TokenMarket {
  if (which && typeof which === 'object') return which;
  if (which === undefined || (TOKEN_SLOTS as readonly string[]).includes(which)) {
    const slot = (which ?? 'token') as TokenSlot;
    const t = issuedOf(env.state, slot);
    if (!t) throw new Error(`the soak ${slot} is not issued yet (run soak setup)`);
    return marketOfIssued(env, t);
  }
  const m = markets(env).find((x) => x.covenantId === which.toLowerCase()) ?? externalMarket(env, which.toLowerCase());
  if (!m) throw new Error(`${which} is not a soak token`);
  return m;
}

/** an external token of the soak registry (`externalTokens`) as a market (the operator's `order` command; the traders trade those with a `trade` block) */
function externalMarket(env: Env, covenantId: string): TokenMarket | undefined {
  if (!(env.cfg.externalTokens ?? []).some((t) => t.covenantId === covenantId)) return undefined;
  let m = cachedMarkets.get(covenantId);
  if (!m) {
    const reg = soakRegistry(env) as { tokens: Parameters<typeof toTokenMarket>[1][]; templates: Parameters<typeof toTokenMarket>[2] };
    const entry = reg.tokens.find((x) => x.covenant_id === covenantId);
    if (!entry) return undefined;
    m = toTokenMarket(env.kob, entry, reg.templates);
    cachedMarkets.set(covenantId, m);
  }
  return m;
}

/** A soak token beside TUSD that is configured and issued: its slot, its config and its market. */
export interface AssetMarket {
  slot: AssetSlot;
  cfg: Token2Config;
  m: TokenMarket;
}

/** A soak book beside TUSD as the bots trade it: the asset market and its per-token KAS reference (Binance asset / KASUSDT). */
export interface AssetBook extends AssetMarket {
  feed: RefSource;
}

/** An external token the traders trade (`externalTokens[]` with a `trade` block, e.g. a KRON token): its market, config and reference. */
export interface ExternalBook {
  cfg: ExternalTokenConfig;
  trade: ExternalTradeConfig;
  m: TokenMarket;
  feed: RefSource;
}

/** the external tokens with a `trade` block, as markets (their config order) */
export function tradedExternalMarkets(env: Env): Omit<ExternalBook, 'feed'>[] {
  return (env.cfg.externalTokens ?? []).flatMap((t) => (t.trade && t.trade.bookShare > 0 ? [{ cfg: t, trade: t.trade, m: tokenMarket(env, t.covenantId) }] : []));
}

/** the configured and issued asset tokens (TETH, TBTC, ...) in slot order */
export function assetMarkets(env: Env): AssetMarket[] {
  const out: AssetMarket[] = [];
  for (const slot of ASSET_SLOTS) {
    const cfg = env.cfg[slot];
    if (cfg && env.state[slot]) out.push({ slot, cfg, m: tokenMarket(env, slot) });
  }
  return out;
}

export function tokenRef(env: Env, which?: Which): TokenRef {
  const m = tokenMarket(env, which);
  return { covenantId: m.covenantId, program: m.program, extensionCommitment: m.extensionCommitment };
}

export async function readClock(env: Env): Promise<Clock> {
  const c = await env.node.getClock();
  return { daa: c.daa, unixSeconds: c.unixSeconds, rateMilli: c.rateMilli ?? 10_000 };
}

export async function readBook(env: Env, which?: Which): Promise<BookView> {
  const m = tokenMarket(env, which);
  const bv = await env.indexer.book(m.covenantId, { depth: 100, aggregate: false });
  return bookFromIndexer(m, bv);
}

/** a pair order (KobPair, KobCondPair, KobIfdPair: token A for token B; no KAS price) */
export const isPair = (v: { contract: string; pair?: unknown }): boolean => isPairOrderView(v as Parameters<typeof isPairOrderView>[0]);

/** open / partial orders of a maker on a soak token (indexer view; a pair order is listed under both its tokens) */
export async function myOrders(env: Env, maker: string, which?: Which): Promise<OrderView[]> {
  const m = tokenMarket(env, which);
  return env.indexer.allOrders({ maker, token: m.covenantId, status: 'active' }, { maxPages: 10 });
}

export async function planEnv(env: Env, w: BotWallet, o: { book?: BookView; mine?: OrderView[] } = {}, which?: Which): Promise<PlanEnv> {
  const m = tokenMarket(env, which);
  const [clock, funding, tokenUtxos, book, mine, fees] = await Promise.all([
    readClock(env),
    w.funding(),
    w.tokenUtxos(tokenRef(env, m)),
    o.book ? Promise.resolve(o.book) : readBook(env, m).catch(() => ({ asks: [], bids: [] }) as BookView),
    o.mine ? Promise.resolve(o.mine) : myOrders(env, w.pk, m).catch(() => [] as OrderView[]),
    feeContext(env),
  ]);
  // Self-trade guard over the maker's resting orders of THIS book: its KAS-quoted orders (a pair order has no KAS price; its own guard is the
  // pair planner's, over the pair book: pairPlanEnv), less those its own pending transactions already cancel or amend (the listing lags)
  const own = w.actionable(mine).filter((v) => !isPair(v));
  return { kob: env.kob, token: m, maker: w.pk, clock, book, ownOrders: ownOrderRefs(m, own), funding, tokenUtxos, fees };
}

/**
 * The environment of a pair order on `base`/`quote` (A/B, oriented): what the web app's `buildPairPlanEnv` reads, from the soak's own sources
 * (the bot wallet, the bots' indexer): the maker's KAS and UTXOs of BOTH tokens, the pair book (direct / entry / route) as a BookView in B per whole
 * A, the maker's live pair orders of the pair (self-trade prevention), the KAS references of A and B (default minimum fill) and the rate their
 * newest KAS trades imply. A guard whose data could not be read is reported (`guardsUnavailable`) as in the app.
 */
export async function pairPlanEnv(env: Env, w: BotWallet, base: Which, quote: Which): Promise<PairPlanEnv> {
  const a = tokenMarket(env, base);
  const b = tokenMarket(env, quote);
  if (a.covenantId === b.covenantId) throw new Error('a pair needs two different tokens');
  const [clock, funding, tokenUtxos, quoteTokenUtxos, fees, view, mine, refA, refB] = await Promise.all([
    readClock(env),
    w.funding(),
    w.tokenUtxos(tokenRef(env, a)),
    w.tokenUtxos(tokenRef(env, b)),
    feeContext(env),
    env.indexer.pairBook(a.covenantId, b.covenantId, { depth: 100 }).catch(() => null),
    myOrders(env, w.pk, a).catch(() => null),
    kasReferenceOf(env.indexer, a),
    kasReferenceOf(env.indexer, b),
  ]);
  const guardsUnavailable: ('own-orders' | 'book')[] = [];
  if (!view) guardsUnavailable.push('book');
  if (!mine) guardsUnavailable.push('own-orders');
  return {
    kob: env.kob, token: a, maker: w.pk, clock, book: view ? pairBookToBookView(view, a.scale) : { asks: [], bids: [] },
    ownOrders: mine ? ownPairOrderRefs(w.actionable(mine), a.covenantId, b.covenantId) : [], ...(guardsUnavailable.length ? { guardsUnavailable } : {}),
    lastFillPrice: impliedPairRate(refA.last, refB.last, b.scale), funding, tokenUtxos, fees,
    pair: { quote: b, quoteTokenUtxos, view, kasPerWholeA: refA.book ?? refA.last, kasPerWholeB: refB.book ?? refB.last },
  };
}

/**
 * A market / close order's planner environment with the start references of the verifier indexers (`env.verifiers`, the app's
 * `extraIndexerUrls`): each one's touch of the KAS book, or for a pair the rate its two KAS books imply. Any other order type: unchanged.
 */
export async function withStartReferences<E extends PlanEnv>(env: Env, e: E, intentType: string): Promise<E> {
  if ((intentType !== 'market' && intentType !== 'close') || !env.verifiers.length) return e;
  const a = e.token;
  const refs = isPairEnv(e)
    ? await readPairReferences(
        env.verifiers,
        { covenantId: a.covenantId, toBook: (v) => bookFromIndexer(a, v) },
        { covenantId: e.pair.quote.covenantId, toBook: (v) => bookFromIndexer(e.pair.quote, v), scale: e.pair.quote.scale },
      )
    : await readReferences(env.verifiers, a.covenantId, (v) => bookFromIndexer(a, v));
  return { ...e, referenceTouches: refs.touches, referencesUnavailable: refs.unavailable };
}

export async function cancelEnv(env: Env, w: BotWallet, which?: Which): Promise<CancelEnv> {
  const [clock, funding, tokenUtxos, fees] = await Promise.all([readClock(env), w.funding(), w.tokenUtxos(tokenRef(env, which)), feeContext(env)]);
  return { kob: env.kob, maker: w.pk, funding, tokenUtxos, clock: { daa: clock.daa, unixSeconds: clock.unixSeconds }, fees };
}

/** best bid / ask, sompi per whole token, of a planner book */
export function touch(book: BookView): { bid: bigint | null; ask: bigint | null } {
  return { bid: book.bids[0]?.price ?? null, ask: book.asks[0]?.price ?? null };
}
