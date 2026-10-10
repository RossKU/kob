// Declarative seeding of the mock chain (`POST /mock/seed`) and the default seed (fictional TN10 token from registry/tokens.example.json,
// a 10-level book per side, recent fills). Seeded orders are REAL: correct P2SH scripts and custody UTXOs, cancellable by the
// maker key with the kob-wasm builders.
//
// Units of every seed spec (protocol v3): `amount` = token BASE UNITS (number, decimal string or bigint); `price`, `tip`, `tick`,
// `mid`, `price_end`, `slope` = SOMPI PER WHOLE TOKEN, i.e. per `scale` base units (the token's standard scale 10^min(decimals, 9), or its
// `order_scale` for a token without decimals; a spec may override `scale`); `min_fill` = base units (default 1). Examples: an EXKCC
// (8 decimals) ask of 3 tokens at 0.0251 KAS each is `{side: 'ask', amount: 300000000, price: 2510000}`; a history fill
// `{amount: 200000000, price: 2500000}`; a mock fill `POST /mock/fill {covenant_id, amount: 100000000}`.
import { readFileSync } from 'node:fs';
import { DEFAULT_CARRIER } from './chain.mjs';
import { TEST_PUBKEYS } from './keys.mjs';
import { bidEscrow } from './model.mjs';
import { HEX64, badRequest, syntheticId } from './util.mjs';

const PROGRAM_OF_TEMPLATE = { 'kcc20-ref-3x3': 'KCC20Ref', 'kcc20-ref-8x8': 'KCC20Ref_8x8', 'kcc20-kaspacom-0-2-5': 'KCC20KaspaCom_0_2_5', 'kcc20-ref-public-mint': 'KCC20PublicMint' };
const registryPath = new URL('../../registry/tokens.example.json', import.meta.url);

/** The default token: the first KCC-20 entry of the example registry (ticker EXKCC, 8 decimals: scale 10^8 base units per whole token). */
export function defaultTokenSpec() {
  const reg = JSON.parse(readFileSync(registryPath, 'utf8'));
  const t = reg.tokens.find((x) => x.family === 'kcc20');
  return {
    ticker: t.ticker,
    name: t.name,
    covenant_id: t.covenant_id,
    program: PROGRAM_OF_TEMPLATE[t.template_id] ?? 'KCC20Ref_8x8',
    extension_commitment: t.extension_commitment,
    decimals: t.decimals,
    // price step of the seeded book: 0.0001 KAS per whole token
    tick: 10_000,
  };
}

/** Sompi per whole token the default book is centred on (0.025 KAS per EXKCC). */
export const DEFAULT_MID = 2_500_000;

/**
 * An OPEN-LIST token as the executor serves it: a program on the strict template list, no registry entry (empty ticker, no decimals, so no
 * standard scale), `standing: unverified`, `template_id` = the program name. Its seeded orders quote at `order_scale` (1000 base units) and are
 * the only source of its scale.
 */
export function openListTokenSpec(over = {}) {
  return {
    ticker: '',
    covenant_id: '0f'.repeat(32),
    program: 'KCC20Ref_8x8',
    template_id: 'KCC20Ref_8x8',
    extension_commitment: 'ee'.repeat(32),
    standing: 'unverified',
    powers: [],
    decimals: null,
    order_scale: 1000,
    ...over,
  };
}

/** Registers the open-list token with a small book (asks above `mid`, bids below; sompi per 1000 base units; 4000 base units per order). */
export function seedOpenListToken(chain, over = {}, { mid = 50_000, levels = 3, maker = 'maker' } = {}) {
  const tok = chain.addToken(openListTokenSpec(over));
  for (let i = 1; i <= levels; i++) {
    seedOneOrder(chain, { token: tok.covenant_id, side: 'ask', price: mid + i * 1000, amount: 4000, maker });
    seedOneOrder(chain, { token: tok.covenant_id, side: 'bid', price: mid - i * 1000, amount: 4000, maker });
  }
  return tok;
}

/** A key spec (test key name or 64-hex x-only pubkey) to a pubkey. */
export const resolveKey = (v, dflt) => {
  const x = v ?? dflt;
  if (x in TEST_PUBKEYS) return TEST_PUBKEYS[x];
  if (!HEX64.test(x ?? '')) throw badRequest(`unknown key "${x}": use a 64-hex x-only pubkey or one of ${Object.keys(TEST_PUBKEYS).join(', ')}`);
  return x;
};

const big = (v, what) => {
  try {
    return BigInt(v);
  } catch {
    throw badRequest(`${what} must be an integer`);
  }
};

/** The scale seeded orders of `tok` quote at: its standard scale, else its `order_scale` (a token without decimals), else 1000. */
export const scaleOf = (tok) => BigInt(tok.scale ?? tok.order_scale ?? 1000);

/** Builds an `AnyState` for a KobAsk / KobBid from a compact spec (other kinds must pass a complete `state`). */
export function buildOrderState(chain, spec) {
  const tok = chain.tokenRef(spec.token ?? [...chain.tokens.keys()][0]);
  const kron = tok.family === 'kron';
  const kind = spec.kind ?? (spec.side === 'bid' || spec.side === 2 ? 'KobBid' : 'KobAsk') + (kron ? 'Kron' : '');
  if (spec.state && spec.state.tokenCovId && spec.state.maker && spec.kind) return { kind, state: spec.state };
  if (kind !== 'KobAsk' && kind !== 'KobBid' && kind !== 'KobAskKron' && kind !== 'KobBidKron') throw badRequest(`seeding ${kind} needs a complete "state" and "kind"`);
  const tpl = chain.templates.get(tok.program);
  const tips = chain.kob.keeperTips()[tok.program];
  const daa = chain.daa();
  const scale = spec.scale === undefined ? scaleOf(tok) : big(spec.scale, 'scale');
  const amount = spec.amount === undefined ? 5n * scale : big(spec.amount, 'amount');
  const base = {
    maker: resolveKey(spec.maker, 'maker'),
    tokenCovId: tok.covenant_id,
    tokenTplHash: tok.template_hash,
    tplPrefixLen: String(tpl.prefixLen),
    tplSuffixLen: String(tpl.suffixLen),
    scale: String(scale),
    minFill: String(spec.min_fill ?? 1),
    price: String(spec.price ?? DEFAULT_MID),
    tip: String(spec.tip ?? 0),
    tif: String(spec.tif ?? 0),
    activeFrom: String(spec.active_from ?? 0),
    expiryDaa: String(spec.expiry_daa ?? daa + 100_000_000),
    refundTip: String(tips.refundTip),
    interval: String(spec.interval ?? 0),
    maxFill: String(spec.max_fill ?? 0),
    slope: String(spec.slope ?? 0),
    priceEnd: String(spec.price_end ?? 0),
    decayStep: String(spec.decay_step ?? 1000),
  };
  // an ask pins the extension commitment of its custody (zero for KRON, whose ask layout has none)
  const ext = { extensionCommitment: kron ? '0'.repeat(64) : (tok.extension_commitment ?? '0'.repeat(64)) };
  if (kind === 'KobAsk' || kind === 'KobAskKron') return { kind, state: { ...base, amountLeft: String(amount), ...ext, ...(spec.state ?? {}) } };
  const bid = { ...base, reserve: '0', deliveryCarrier: String(DEFAULT_CARRIER), ...(spec.state ?? {}) };
  // the KRON bid layout has no extension commitment
  return { kind, state: kron ? bid : { ...bid, extensionCommitment: tok.extension_commitment ?? '0'.repeat(64) } };
}

/** Seeds one order (open, or `history: 'filled' | 'cancelled'`). Returns the order record. */
export function seedOneOrder(chain, spec) {
  const any = buildOrderState(chain, spec);
  const tok = chain.tokenRef(spec.token ?? [...chain.tokens.keys()][0]);
  const amount = spec.amount === undefined ? 5n * scaleOf(tok) : big(spec.amount, 'amount');
  if (spec.history) {
    const fills = (spec.fills ?? [{ amount, price: spec.price ?? DEFAULT_MID }]).map((f) => ({ amount: big(f.amount, 'fill amount'), price: f.price }));
    return chain.seedHistory(any, { status: spec.history, fills: spec.history === 'filled' ? fills : [], agoDaa: spec.ago_daa ?? 1000 });
  }
  let value = spec.value;
  // a bid's quantity is its escrow: what buys `amount` base units in one fill (kob-wasm `bidEscrow`: budget, delivery carrier, reserve)
  if (value === undefined) value = any.kind.startsWith('KobBid') ? bidEscrow(chain.kob, any, amount, 1) : DEFAULT_CARRIER;
  const o = chain.seedOrder(any, { value: BigInt(value), covenantId: spec.covenant_id, deadline: spec.deadline ?? null });
  if (spec.possibly_frozen) chain.setPossiblyFrozen(o.covenantId, true);
  return o;
}

/**
 * A book of `levels` price levels per side around `mid` (sompi per whole token), `tick` apart; the best levels hold two orders. `amounts`
 * (optional, base units per level) overrides the default sizes (3..11 whole tokens, bids one whole token more, the second best order one more).
 */
export function seedBook(chain, { token, mid, levels = 10, tick, maker = 'maker', amounts }) {
  const tok = chain.tokenRef(token ?? [...chain.tokens.keys()][0]);
  const scale = scaleOf(tok);
  const step = BigInt(tick ?? tok.tick ?? 10_000);
  const midp = BigInt(mid ?? DEFAULT_MID);
  const created = [];
  for (let i = 0; i < levels; i++) {
    const n = amounts?.[i] !== undefined ? big(amounts[i], 'amount') : BigInt(3 + ((i * 7) % 9)) * scale;
    const dup = i === 0 ? 2 : 1;
    for (let d = 0; d < dup; d++) {
      created.push(seedOneOrder(chain, { token: tok.covenant_id, side: 'ask', price: midp + step * BigInt(i + 1), amount: n + BigInt(d) * scale, maker }));
      created.push(seedOneOrder(chain, { token: tok.covenant_id, side: 'bid', price: midp - step * BigInt(i + 1), amount: n + BigInt(1 + d) * scale, maker }));
    }
  }
  return created;
}

/** The default seed: one fictional token, a plausible book, recent fills. Nothing is funded (use `utxos`). */
export function applyDefaultSeed(chain) {
  const spec = defaultTokenSpec();
  const tok = chain.addToken(spec);
  const scale = scaleOf(tok);
  const mid = DEFAULT_MID;
  seedBook(chain, { token: tok.covenant_id, mid, tick: spec.tick, levels: 10 });
  const history = [];
  for (let i = 0; i < 8; i++) {
    const side = i % 2 === 0 ? 'ask' : 'bid';
    const price = mid + (i % 3 - 1) * spec.tick;
    const amount = BigInt(2 + i) * scale;
    history.push({ token: tok.covenant_id, side, price, amount, history: 'filled', fills: [{ amount, price }], ago_daa: 600 - i * 60 });
  }
  for (const h of history) seedOneOrder(chain, h);
  return tok;
}

/** `POST /mock/seed`: additive declarative seed (see `reset` / `default` flags). */
export function applySeed(chain, spec = {}) {
  if (spec.reset) chain.reset();
  if (spec.default) applyDefaultSeed(chain);
  for (const t of spec.tokens ?? []) chain.addToken(t);
  if (spec.open_token) seedOpenListToken(chain, spec.open_token === true ? {} : spec.open_token);
  for (const o of spec.orders ?? []) seedOneOrder(chain, o);
  if (spec.book) seedBook(chain, spec.book);
  for (const f of spec.fills ?? []) {
    const tok = chain.tokenRef(f.token ?? [...chain.tokens.keys()][0]);
    const amount = f.amount ?? scaleOf(tok);
    seedOneOrder(chain, { ...f, amount, history: 'filled', fills: [{ amount, price: f.price ?? DEFAULT_MID }] });
  }
  for (const t of spec.trades ?? []) seedTrade(chain, t);
  if ((spec.trades ?? []).length) sortEventsByDaa(chain);
  const history = spec.history ? seedHistory(chain, spec.history === true ? {} : spec.history) : undefined;
  for (const u of spec.utxos ?? []) giveSpec(chain, u);
  return history ? { ...summary(chain), history } : summary(chain);
}

// ------------------------------------------------------------------------------------------------ market history (trades at explicit times)

/** mulberry32: a tiny deterministic PRNG in [0, 1). */
export function mulberry32(seed) {
  let a = seed >>> 0;
  return () => {
    a = (a + 0x6d2b79f5) | 0;
    let t = Math.imul(a ^ (a >>> 15), 1 | a);
    t = (t + Math.imul(t ^ (t >>> 7), 61 | t)) ^ t;
    return ((t ^ (t >>> 14)) >>> 0) / 4294967296;
  };
}

/**
 * Re-establishes chain order after events were seeded in the past: events sorted by DAA (stable) and their ids renumbered, so trade ids
 * (= smallest fill event id) are monotonic in time like the indexer's. Block sequence numbers of orders are left alone.
 */
export function sortEventsByDaa(chain) {
  const ev = chain.events;
  if (ev.every((e, i) => i === 0 || ev[i - 1].daa <= e.daa)) return;
  ev.sort((a, b) => a.daa - b.daa || a.id - b.id);
  ev.forEach((e, i) => {
    e.id = i + 1;
  });
  chain.eventId = ev.length;
}

/**
 * ONE trade at an explicit time: every leg is a finished order filled in the same transaction (`txid`). Legs: `{side: 'ask'|'bid', price,
 * amount, maker?, scale?, age_daa?}` (price sompi per whole token, amount base units) where `age_daa` is how long before the trade the order was placed (default 100; the older
 * side is the resting one). The time is `ts` (unix ms) or `ago_ms` before now (default now); the DAA follows at 10 DAA per second.
 * Call `sortEventsByDaa` after seeding trades in the past (applySeed / seedHistory do).
 */
export function seedTrade(chain, spec) {
  const tok = chain.tokenRef(spec.token ?? [...chain.tokens.keys()][0]);
  const now = chain.nowMs();
  const ts = spec.ts !== undefined ? Number(spec.ts) : now - Number(spec.ago_ms ?? 0);
  if (!Number.isFinite(ts) || ts > now) throw badRequest('trade ts must be a unix ms time not in the future');
  const daa = Math.max(1, chain.daa() - Math.round((now - ts) / 100));
  const legs = spec.legs ?? [];
  if (!legs.length) throw badRequest('a trade needs at least one leg');
  const txid = spec.txid ?? syntheticId('hist-trade', `${chain.synth}:${ts}`);
  const orders = legs.map((l) => {
    const age = Math.max(0, Math.round(Number(l.age_daa ?? 100)));
    const amount = l.amount === undefined ? scaleOf(tok) : big(l.amount, 'amount');
    const any = buildOrderState(chain, { token: tok.covenant_id, side: l.side, price: l.price, amount, maker: l.maker, scale: l.scale });
    return chain.seedHistory(any, {
      status: 'filled',
      fills: [{ amount, price: l.price ?? DEFAULT_MID, daa, ts, txid }],
      genesisDaa: Math.max(1, daa - age),
      genesisTs: ts - age * 100,
    });
  });
  return { txid, ts, daa, orders: orders.map((o) => o.covenantId) };
}

/**
 * A deterministic market history of `trades` trades over the last `hours` (seeded PRNG): a log-price random walk with trend regimes,
 * volatility / activity clustering and mean reversion, pinned (Brownian bridge) to end at `mid` (sompi per whole token, like the default seed) so it
 * joins the seeded book; prices on the token's `tick`. Both aggressor sides, 1..20 whole tokens with occasional blocks, ~30% of the trades are
 * two-sided (an older resting order and the aggressor filled in one transaction). Returns `{token, trades, fills, from_ts, to_ts}`.
 */
export function seedHistory(chain, { token, hours = 48, trades = 600, seed = 1, mid, tick } = {}) {
  if (chain.tokens.size === 0) throw badRequest('history needs a token (seed one first)');
  const tok = chain.tokenRef(token ?? [...chain.tokens.keys()][0]);
  const n = Number(trades);
  if (!Number.isInteger(n) || n < 1 || n > 20_000) throw badRequest('history trades must be an integer in 1..20000');
  if (!(Number(hours) > 0) || Number(hours) > 24 * 365) throw badRequest('history hours must be positive');
  const rnd = mulberry32(Number(seed) || 0);
  const gauss = () => {
    let u = 0;
    while (u === 0) u = rnd();
    return Math.sqrt(-2 * Math.log(u)) * Math.cos(2 * Math.PI * rnd());
  };
  const step = Number(tick ?? tok.tick ?? 10_000);
  const midp = Number(mid ?? DEFAULT_MID);
  const scale = scaleOf(tok);
  const span = Number(hours) * 3_600_000;
  const newest = Math.min(90_000, span / 10); // the last history trade is older than the default seed's recent fills

  // activity / volatility regime (log-AR(1)), inter-arrival gaps scaled to fill the span
  const regime = [];
  let r = 0;
  for (let i = 0; i < n; i++) {
    r = Math.max(-1.5, Math.min(1.5, 0.97 * r + 0.25 * gauss()));
    regime.push(r);
  }
  const gaps = regime.map((x) => -Math.log(1 - rnd()) / Math.exp(x));
  const total = gaps.reduce((a, b) => a + b, 0);
  let acc = 0;
  const times = gaps.map((g) => {
    acc += g;
    return acc / total; // (0, 1]
  });
  const t0 = chain.nowMs() - span;
  const tsOf = (f) => Math.round(t0 + f * (span - newest));

  // log price: drift regimes + mean reversion + regime-scaled diffusion, then a bridge to ln(mid)
  const lnMid = Math.log(midp);
  let x = lnMid + 0.06 * gauss();
  let drift = 0;
  let prevH = 0;
  const path = [];
  for (let i = 0; i < n; i++) {
    const h = (times[i] * span) / 3_600_000;
    const dt = Math.max(h - prevH, 1e-6);
    prevH = h;
    if (rnd() < 1 / 80) drift = 0.012 * gauss();
    const sigma = 0.012 * Math.exp(0.6 * regime[i]);
    x += (drift - 0.03 * (x - lnMid)) * dt + sigma * Math.sqrt(dt) * gauss();
    path.push(x);
  }
  const end = path[n - 1];
  const prices = path.map((v, i) => {
    const p = Math.exp(v - times[i] * (end - lnMid));
    return BigInt(Math.max(step * 5, Math.round(p / step) * step));
  });

  const nonce = chain.synth;
  let fills = 0;
  let prev = BigInt(Math.round(midp / step) * step);
  for (let i = 0; i < n; i++) {
    const price = prices[i];
    const pBuy = price > prev ? 0.8 : price < prev ? 0.2 : 0.5;
    prev = price;
    const buy = rnd() < pBuy; // aggressor bought (the resting side is the asks)
    const whole = rnd() < 0.06 ? 25 + Math.floor(rnd() * 100) : Math.min(20, 1 + Math.floor(-Math.log(1 - rnd()) * 4));
    const restAge = 20 + Math.floor(rnd() * 36_000); // placed 2 s .. 1 h before the trade
    const legs = [{ side: buy ? 'ask' : 'bid', price: price.toString(), amount: BigInt(whole) * scale, maker: 'maker', age_daa: restAge }];
    if (rnd() < 0.3) {
      const k = BigInt(rnd() < 0.5 ? 0 : 1) * BigInt(step);
      const aggr = buy ? price + k : price - k > 0n ? price - k : price;
      legs.push({ side: buy ? 'bid' : 'ask', price: aggr.toString(), amount: BigInt(whole) * scale, maker: 'carol', age_daa: 0 });
    }
    seedTrade(chain, { token: tok.covenant_id, ts: tsOf(times[i]), legs, txid: syntheticId('hist-trade', `${nonce}:${i}`) });
    fills += legs.length;
  }
  sortEventsByDaa(chain);
  chain.emit({ orders: [], tokens: [tok.covenant_id], fills: [], added: 1, reverted: 0 });
  return { token: tok.covenant_id, trades: n, fills, from_ts: tsOf(times[0]), to_ts: tsOf(times[n - 1]) };
}

/** `{pubkey|key, kas?, count?, tokens?: [{token, amount, count?, carrier?}]}` */
export function giveSpec(chain, u) {
  const pk = resolveKey(u.pubkey ?? u.key);
  const made = { kas: [], tokens: [] };
  if (u.kas !== undefined) for (let i = 0; i < (u.count ?? 1); i++) made.kas.push(chain.giveKas(pk, BigInt(u.kas)));
  for (const t of u.tokens ?? []) {
    for (let i = 0; i < (t.count ?? 1); i++) made.tokens.push(chain.giveTokens(pk, t.token ?? [...chain.tokens.keys()][0], BigInt(t.amount), t.carrier === undefined ? DEFAULT_CARRIER : BigInt(t.carrier)));
  }
  return made;
}

export const summary = (chain) => ({
  daa: chain.daa(),
  tokens: [...chain.tokens.values()].map((t) => ({ ticker: t.ticker, covenant_id: t.covenant_id })),
  orders: chain.orders.size,
  open_orders: [...chain.orders.values()].filter((o) => o.status === 'open' || o.status === 'partial').length,
  fills: chain.events.filter((e) => e.kind === 'fill').length,
  submissions: chain.submissions.length,
});
