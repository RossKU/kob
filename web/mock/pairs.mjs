// Token/token pairs of the MOCK indexer: the pair orders (KobPair, KobCondPair, KobIfdPair; token A for token B at B base units per whole A,
// one template each for both sides and both token families), their state builder and seed, and the four pair endpoints of
// docs/ops/executor.md 5.4 (crates/kob-executor/src/indexer/pairs.rs):
//   GET /v1/pairs/{base}/{quote}/book?depth=     direct (KobPair), entry (KobIfdPair resting at its limit / armed stop entry) and route levels
//                                                (implied through the two KAS books); price = price_num / price_den QUOTE base units per
//                                                BASE base unit (reduced), amount in BASE base units
//   GET /v1/pairs?token=                         oriented pairs with listed live pair orders and their counts per kind
//   GET /v1/pairs/{base}/{quote}/candles         pair candles DERIVED from the two KAS series (never from pair fills)
//   GET /v1/pairs/{base}/{quote}/fills           pair fills (volume, counterparty, price_source none) and the 24 h pair volume
// Every amount of a pair order is computed by kob-wasm (custodies, escrows, prices at a time); the mock re-implements no covenant rule.
import { DEFAULT_CARRIER } from './chain.mjs';
import { resolveKey, seedBook } from './seed.mjs';
import { bookView, limitParam, intParam, parseHash } from './views.mjs';
import { custodiesOf, isPairKind, pairTokensOf, quoteOf } from './model.mjs';
import { INTERVALS, MAX_CANDLES, kasCandleBuckets } from './market.mjs';
import { badRequest, notFound } from './util.mjs';

const b = (v) => BigInt(v ?? 0);
const ZERO32 = '00'.repeat(32);
const gcd = (x, y) => {
  let a = x < 0n ? -x : x;
  let c = y < 0n ? -y : y;
  while (c) [a, c] = [c, a % c];
  return a || 1n;
};
const reduce = (num, den) => {
  const g = gcd(num, den);
  return { num: num / g, den: den / g };
};
const cmp = (a, c) => {
  const l = a.num * c.den;
  const r = c.num * a.den;
  return l < r ? -1 : l > r ? 1 : 0;
};
const active = (o) => o.status === 'open' || o.status === 'partial';
const raw = (chain) => chain.kob.raw;
const J = (v) => JSON.stringify(v);

// ------------------------------------------------------------------------------------------------ state builder

/** A token's pin in a pair state: template hash, prefix / suffix, family code, standard scale, the extension of new outputs. */
function pin(chain, ref) {
  const t = chain.tokenRef(ref);
  const tpl = chain.templates.get(t.program);
  const kron = t.family === 'kron';
  return {
    covId: t.covenant_id, tplHash: t.template_hash, pre: String(tpl.prefixLen), suf: String(tpl.suffixLen), family: kron ? '2' : '1',
    scale: String(t.scale ?? t.order_scale ?? 1000), ext: kron ? ZERO32 : (t.extension_commitment ?? ZERO32),
  };
}
const st = (p, x) => ({ [`${p}CovId`]: x.covId, [`${p}TplHash`]: x.tplHash, [`${p}Pre`]: x.pre, [`${p}Suf`]: x.suf, [`${p}Family`]: x.family });

/**
 * A complete pair order state and the KAS its UTXO needs (kob-wasm `minOrderValue`), validated with kob-wasm `checkNewOrder`. Spec (prices B
 * base units per WHOLE A, amounts base units of A, tips KAS sompi per whole A):
 *   kind 'KobPair' | 'KobCondPair' | 'KobIfdPair'; side 'ask' (sell A; a sell-first entry) | 'bid' (buy A; a buy-first entry); base, quote (ticker or
 *   covenant id); amount; price (KobPair / KobIfdPair; a conditional's take-profit is \`tp\`); tip; tif; minFill; maker; activeFrom; expiryDaa;
 *   KobPair: slope, priceEnd, decayStep, interval, maxFill; a bid's escrow covers \`fills\` fills (default amount / minFill);
 *   KobCondPair: stop, tp, slipBps, trailStep, trailGap, trailWait, minTouch, minRestDaa, bandDaa;
 *   KobIfdPair: entryStop, prefund (sell-first), rptAmount, exit: { tp, stop, slipBps, trailStep, trailGap, ... } (the committed KobCondPair exit).
 */
export function pairOrderState(chain, spec) {
  const kob = chain.kob;
  const kind = spec.kind ?? 'KobPair';
  if (!isPairKind(kind)) throw badRequest(`not a pair kind: ${kind}`);
  const ask = spec.side === 'ask' || spec.side === 'sell' || spec.side === 1 || spec.side === '1';
  const A = pin(chain, spec.base);
  const B = pin(chain, spec.quote);
  if (A.covId === B.covId) throw badRequest('a pair needs two different tokens');
  const amount = b(spec.amount ?? b(A.scale) * 5n);
  const minFill = b(spec.minFill ?? spec.min_fill ?? (amount < b(A.scale) ? amount : b(A.scale)));
  const maker = resolveKey(spec.maker, 'maker');
  const expiryDaa = String(spec.expiryDaa ?? spec.expiry_daa ?? chain.daa() + 70_000_000);
  const tips = (state) => JSON.parse(raw(chain).tipsFor(J(state)));
  const S = ask ? A : B;
  const T = ask ? B : A;
  const common = { maker, side: ask ? '1' : '2', ...st('s', S), sScale: S.scale, ...st('t', T), tExt: T.ext, tScale: T.scale, sExt: S.ext };
  let any;
  if (kind === 'KobPair') {
    any = {
      kind, state: {
        ...common, minFill: String(minFill), price: String(spec.price), tip: String(spec.tip ?? 0), tif: String(spec.tif ?? 0), activeFrom: String(spec.activeFrom ?? 0),
        expiryDaa, refundTip: '0', deliveryCarrier: String(spec.deliveryCarrier ?? 200_000_000), interval: String(spec.interval ?? 0), maxFill: String(spec.maxFill ?? 0),
        slope: String(spec.slope ?? 0), priceEnd: String(spec.priceEnd ?? 0), decayStep: String(spec.decayStep ?? (spec.slope ? 10 : 0)), amountLeft: String(amount),
        custody: ask ? String(amount) : '0',
      },
    };
    any.state.refundTip = tips(any).refundTip;
    if (!ask) {
      const fills = b(spec.fills ?? (amount + minFill - 1n) / minFill);
      const v = raw(chain).pairBidEscrow(J(any), String(amount), String(fills));
      if (v == null) throw badRequest('the pair bid escrow overflows');
      any.state.custody = String(v);
    }
  } else if (kind === 'KobCondPair') {
    any = {
      kind, state: {
        ...common, minFill: String(minFill), tip: String(spec.tip ?? 0), activeFrom: String(spec.activeFrom ?? 0), expiryDaa, refundTip: '0',
        deliveryCarrier: String(spec.deliveryCarrier ?? 200_000_000), tpPrice: String(spec.tp ?? 0), slipBps: String(spec.slipBps ?? 300), trailStep: String(spec.trailStep ?? 0),
        trailGap: String(spec.trailGap ?? 0), trailWait: String(spec.trailWait ?? 0), minTouch: String(spec.minTouch ?? minFill), minRestDaa: String(spec.minRestDaa ?? 50),
        bandDaa: String(spec.bandDaa ?? 300), keeperTip: '0', stopPrice: String(spec.stop ?? 0), armed: '0', amountLeft: String(amount), custody: ask ? String(amount) : '0',
        parent: ZERO32, rptPrice: '0', rptPre: '0', rptUntil: '0',
      },
    };
    const t = tips(any);
    Object.assign(any.state, { refundTip: t.refundTip, keeperTip: t.keeperTip });
    if (!ask) {
      const fills = b(spec.fills ?? (amount + minFill - 1n) / minFill);
      const v = raw(chain).condPairBidEscrow(J(any), String(fills));
      if (v == null) throw badRequest('the conditional pair bid escrow overflows');
      any.state.custody = String(v);
    }
  } else {
    const x = spec.exit ?? {};
    // the committed exit: the opposite side (a buy-first entry's exit sells A, a sell-first entry's buys it back)
    const exitAsk = !ask;
    const XS = exitAsk ? A : B;
    const XT = exitAsk ? B : A;
    const exit = {
      kind: 'KobCondPair', state: {
        maker, side: exitAsk ? '1' : '2', ...st('s', XS), sScale: XS.scale, ...st('t', XT), tExt: XT.ext, tScale: XT.scale, minFill: String(x.minFill ?? minFill),
        tip: String(x.tip ?? 0), activeFrom: '0', expiryDaa: String(x.expiryDaa ?? 499_999_999_999), refundTip: '0', deliveryCarrier: String(spec.deliveryCarrier ?? 200_000_000),
        tpPrice: String(x.tp ?? 0), slipBps: String(x.slipBps ?? 300), trailStep: String(x.trailStep ?? 0), trailGap: String(x.trailGap ?? 0), trailWait: String(x.trailWait ?? 0),
        minTouch: String(x.minTouch ?? minFill), minRestDaa: String(x.minRestDaa ?? 50), bandDaa: String(x.bandDaa ?? 300), keeperTip: '0', stopPrice: String(x.stop ?? 0),
        armed: '0', amountLeft: '0', custody: '0', parent: ZERO32, rptPrice: '0', rptPre: '0', rptUntil: '0', sExt: XS.ext,
      },
    };
    const xt = tips(exit);
    Object.assign(exit.state, { refundTip: xt.refundTip, keeperTip: xt.keeperTip });
    any = {
      kind, state: {
        maker, side: ask ? '1' : '2', ...st('a', A), aScale: A.scale, aExt: A.ext, ...st('b', B), bScale: B.scale, bExt: B.ext, price: String(spec.price),
        prefund: String(ask ? (spec.prefund ?? 0) : 0), tip: String(spec.tip ?? 0), activeFrom: String(spec.activeFrom ?? 0), expiryDaa, refundTip: '0',
        deliveryCarrier: String(spec.deliveryCarrier ?? 200_000_000), exitCarrier: String(spec.exitCarrier ?? 600_000_000), minFill: String(minFill),
        entryStop: String(spec.entryStop ?? 0), bandDaa: String(spec.bandDaa ?? 300), minTouch: String(spec.minTouch ?? minFill), minRestDaa: String(spec.minRestDaa ?? 50),
        keeperTip: '0', armed: '0', amountLeft: String(amount), custody: '0', rptAmount: String(spec.rptAmount ?? 0), exitState: raw(chain).ifdPairCommitExit(J(exit)),
      },
    };
    const t = tips(any);
    Object.assign(any.state, { refundTip: t.refundTip, keeperTip: t.keeperTip });
    const need = raw(chain).ifdPairBCustodyNeeded(J(any));
    if (need == null) throw badRequest('the entry B custody overflows');
    any.state.custody = String(need);
  }
  try {
    raw(chain).checkNewOrder(J(any));
  } catch (e) {
    throw badRequest(`invalid pair order: ${String(e?.message ?? e)}`);
  }
  // the order UTXO's KAS: the carriers of every possible fill and the tip of the whole amount (kob-wasm `pairKasValue` over `pairMaxFills`), its
  // refund and keeper tips, and never below kob-wasm's `minOrderValue`
  const fills = raw(chain).pairMaxFills(J(any));
  const carriers = BigInt(raw(chain).pairKasValue(J(any), fills) ?? 0);
  const least = BigInt(raw(chain).minOrderValue(J(any)));
  const v = carriers + b(any.state.refundTip) + b(any.state.keeperTip);
  return { any, value: v > least ? v : least };
}

/** Seeds a complete, real pair order (its UTXO and every custody of A and / or B); \`spec\` as {@link pairOrderState} plus \`value\`, \`daa\` (backdate). */
export function seedPairOrder(chain, spec) {
  const { any, value } = pairOrderState(chain, spec);
  return chain.seedOrder(any, { value: spec.value !== undefined ? BigInt(spec.value) : value, daa: spec.daa, covenantId: spec.covenant_id });
}

// ------------------------------------------------------------------------------------------------ live pair orders

/** Every custody the state holds is exactly one live custody UTXO of that token at that amount. */
export function pairCustodiesOk(chain, o) {
  const live = chain.liveTokenUtxos(o.covenantId, 'custody');
  const cs = custodiesOf(chain.kob, o.state).filter((c) => c.amount > 0n);
  if (live.length !== cs.length) return false;
  return cs.every((c) => live.filter((u) => u.token === c.token && u.amount === c.amount).length === 1);
}

/** Listed live pair orders of the oriented pair (base, quote): open / partial, exact custodies, active, not past a refund time or deadline. */
function livePairOrders(chain, base, quote) {
  const daa = BigInt(chain.daa());
  return [...chain.orders.values()].filter((o) => {
    if (!isPairKind(o.kind) || !active(o) || !o.listed || !o.stateKnown || o.possiblyFrozen || !o.current) return false;
    if ((base && o.token !== base) || (quote && o.quote !== quote)) return false;
    const s = o.state.state;
    if (b(s.activeFrom) > daa || b(s.expiryDaa) <= daa) return false;
    if (o.deadline !== null && chain.nowUnix() >= o.deadline) return false;
    return pairCustodiesOk(chain, o);
  });
}

/** The quote of a pair order now (B per whole A): a KobPair's price at DAA (decay / rise), an entry's limit or armed auction price. */
function quoteNow(chain, o, daa) {
  const s = o.state.state;
  const u = String(o.currentDaa ?? daa);
  if (o.kind === 'KobPair') return b(raw(chain).orderPriceAt(J(o.state), String(daa), u) ?? s.price);
  if (o.kind === 'KobIfdPair') return b(s.entryStop) > 0n ? b(raw(chain).ifdPairPriceAt(J(o.state), 'false', String(daa), u) ?? s.price) : b(s.price);
  return null;
}

/** Direct (KobPair) and entry (KobIfdPair at its limit / armed) levels of one side. */
function orderLevels(chain, base, quote, side) {
  const daa = chain.daa();
  const out = new Map();
  for (const o of livePairOrders(chain, base, quote)) {
    const s = o.state.state;
    const source = o.kind === 'KobPair' ? 'direct' : o.kind === 'KobIfdPair' ? 'entry' : null;
    if (!source || (side === 'ask') !== (o.side === 1)) continue;
    // an unarmed stop entry is a stop, not resting liquidity
    if (o.kind === 'KobIfdPair' && b(s.entryStop) > 0n && b(s.armed) === 0n) continue;
    const p = quoteNow(chain, o, daa);
    const left = b(s.amountLeft);
    const sA = pairTokensOf(o.state).a.scale;
    if (p === null || p <= 0n || left <= 0n) continue;
    const price = reduce(p, sA);
    const key = `${source}:${price.num}/${price.den}`;
    const cur = out.get(key) ?? { source, price, amount: 0n, orders: 0 };
    cur.amount += left;
    cur.orders += 1;
    out.set(key, cur);
  }
  return [...out.values()];
}

/**
 * The plain KAS orders (KobAsk / KobBid, either family) of a token's listed book at their ALL-IN quote now (`rate`: an ask's quote - tip, a
 * bid's quote + tip, sompi per whole token of `scale` base units) and their `amount` (a bid's: its buying power), best first.
 */
function kasOrders(chain, token, side) {
  const v = bookView(chain, token, new URLSearchParams({ depth: '200', aggregate: 'false' }));
  const rows = side === 'ask' ? v.asks : v.bids;
  const out = [];
  for (const r of rows) {
    const k = r.contract.replace(/Kron$/, '');
    if (k !== 'KobAsk' && k !== 'KobBid') continue;
    const quoteNow = b(r.quote ?? r.price);
    const rate = side === 'ask' ? quoteNow - b(r.tip) : quoteNow + b(r.tip);
    const scale = b(r.scale);
    const amount = b(r.amount_left);
    if (rate > 0n && scale > 0n && amount > 0n) out.push({ rate, scale, amount });
  }
  out.sort((x, y) => {
    const l = x.rate * y.scale;
    const r = y.rate * x.scale;
    const c = l < r ? -1 : l > r ? 1 : 0;
    return side === 'ask' ? c : -c;
  });
  return out;
}

/**
 * Route levels (pairs.rs `route_levels`): buying BASE with QUOTE sells QUOTE into QUOTE's KAS bids and buys BASE from BASE's KAS asks (asks of the
 * pair); selling BASE for QUOTE sells BASE into BASE's KAS bids and buys QUOTE from QUOTE's KAS asks (bids of the pair). Each step pairs the current
 * order of both books and consumes the smaller of their KAS amounts (`floor(amount * rate / scale)`); its price is the ratio of the two all-in
 * prices per base unit, its amount the base units that KAS buys or sells (floored). Consecutive steps at one price merge.
 */
function routeLevels(chain, base, quote, side) {
  const baseOrders = kasOrders(chain, base, side === 'ask' ? 'ask' : 'bid');
  const quoteOrders = kasOrders(chain, quote, side === 'ask' ? 'bid' : 'ask');
  const kas = (o) => quoteOf(o.amount, o.rate, o.scale, 'down');
  const leftB = baseOrders.map(kas);
  const leftQ = quoteOrders.map(kas);
  const out = [];
  let i = 0;
  let j = 0;
  while (i < baseOrders.length && j < quoteOrders.length) {
    const x = baseOrders[i];
    const y = quoteOrders[j];
    const k = leftB[i] < leftQ[j] ? leftB[i] : leftQ[j];
    const price = reduce(x.rate * y.scale, x.scale * y.rate);
    const amount = (k * x.scale) / x.rate;
    if (amount > 0n) {
      const last = out[out.length - 1];
      if (last && cmp(last.price, price) === 0) {
        last.amount += amount;
        last.orders.add(`b${i}`).add(`q${j}`);
      } else out.push({ source: 'route', price, amount, orders: new Set([`b${i}`, `q${j}`]) });
    }
    leftB[i] -= k;
    leftQ[j] -= k;
    if (leftB[i] <= 0n) i++;
    if (leftQ[j] <= 0n) j++;
  }
  return out.map((l) => ({ ...l, orders: l.orders.size }));
}

const levelView = (l) => ({ source: l.source, price_num: l.price.num.toString(), price_den: l.price.den.toString(), amount: l.amount.toString(), orders: l.orders });
const SOURCE_RANK = { direct: 0, entry: 1, route: 2 };

/** `GET /v1/pairs/{base}/{quote}/book?depth=`. */
export function pairBookView(chain, baseId, quoteId, q) {
  const base = parseHash('base', baseId);
  const quote = parseHash('quote', quoteId);
  if (base === quote) throw badRequest('base and quote must differ');
  const depth = limitParam(q, 'depth', 20);
  const known = chain.tokens.has(base) && chain.tokens.has(quote);
  const src = (x) => SOURCE_RANK[x.source];
  const asks = known ? [...orderLevels(chain, base, quote, 'ask'), ...routeLevels(chain, base, quote, 'ask')].sort((x, y) => cmp(x.price, y.price) || src(x) - src(y)).slice(0, depth) : [];
  const bids = known ? [...orderLevels(chain, base, quote, 'bid'), ...routeLevels(chain, base, quote, 'bid')].sort((x, y) => cmp(y.price, x.price) || src(x) - src(y)).slice(0, depth) : [];
  return { base, quote, daa_score: chain.daa(), asks: asks.map(levelView), bids: bids.map(levelView) };
}

/** `GET /v1/pairs?token=`: one entry per oriented pair with listed live pair orders, sorted by (base, quote). */
export function pairsView(chain, q) {
  const token = q.get('token') === null ? null : parseHash('token', q.get('token'));
  const pairs = new Map();
  for (const o of livePairOrders(chain, null, null)) {
    if (token !== null && o.token !== token && o.quote !== token) continue;
    const key = `${o.token}:${o.quote}`;
    const p = pairs.get(key) ?? { base: o.token, quote: o.quote, direct_asks: 0, direct_bids: 0, entry_asks: 0, entry_bids: 0, conditionals: 0 };
    if (o.kind === 'KobPair') p[o.side === 1 ? 'direct_asks' : 'direct_bids'] += 1;
    else if (o.kind === 'KobIfdPair') p[o.side === 1 ? 'entry_asks' : 'entry_bids'] += 1;
    else p.conditionals += 1;
    pairs.set(key, p);
  }
  return [...pairs.values()].sort((x, y) => (x.base < y.base ? -1 : x.base > y.base ? 1 : x.quote < y.quote ? -1 : x.quote > y.quote ? 1 : 0));
}

// ------------------------------------------------------------------------------------------------ candles (derived from the two KAS series)

/** `pa / pb` as B base units per `basisA` base units of A: `pa x basisB / pb`, exact (reduced) and floored. */
function pairRate(pa, pb, basisB) {
  if (pb <= 0n) return null;
  const r = reduce(pa * basisB, pb);
  return { value: (r.num / r.den).toString(), num: r.num.toString(), den: r.den.toString() };
}

/**
 * `GET /v1/pairs/{base}/{quote}/candles?interval=&from=&to=&limit=`: for every bucket in which A or B traded in KAS, o = open(A) / open(B),
 * c = close(A) / close(B), h = high(A) / low(B), l = low(A) / high(B); a side without a KAS trade in the bucket carries its last close forward;
 * buckets before both tokens have a KAS price are omitted. Pair volume per bucket from the pair fills (never a price).
 */
export function pairCandlesView(chain, baseId, quoteId, q) {
  const base = parseHash('base', baseId);
  const quote = parseHash('quote', quoteId);
  if (base === quote) throw badRequest('base and quote must differ');
  const tokA = chain.tokens.get(base);
  const tokB = chain.tokens.get(quote);
  if (!tokA || !tokB) throw notFound('unknown token');
  const interval = q.get('interval');
  if (interval === null || !Object.hasOwn(INTERVALS, interval)) throw badRequest(`interval must be one of ${Object.keys(INTERVALS).join(', ')}`);
  const ms = INTERVALS[interval];
  const from = intParam(q, 'from');
  const to = intParam(q, 'to');
  const limit = limitParam(q, 'limit', 500, MAX_CANDLES);
  const A = kasCandleBuckets(chain, tokA, ms);
  const B = kasCandleBuckets(chain, tokB, ms);
  const vol = new Map();
  for (const f of chain.pairFills) {
    if (f.base !== base || f.quote !== quote) continue;
    const t = Math.floor(f.ts / ms) * ms;
    const v = vol.get(t) ?? { a: 0n, b: 0n, n: 0 };
    v.a += f.amount_a;
    v.b += f.amount_b ?? 0n;
    v.n += 1;
    vol.set(t, v);
  }
  const times = [...new Set([...A.buckets.keys(), ...B.buckets.keys()])].sort((x, y) => x - y);
  let lastA = null;
  let lastB = null;
  const all = [];
  for (const t of times) {
    const ca = A.buckets.get(t) ?? null;
    const cb = B.buckets.get(t) ?? null;
    const a = ca ?? (lastA === null ? null : { o: lastA, h: lastA, l: lastA, c: lastA });
    const bb = cb ?? (lastB === null ? null : { o: lastB, h: lastB, l: lastB, c: lastB });
    if (ca) lastA = ca.c;
    if (cb) lastB = cb.c;
    if (!a || !bb) continue;
    const o = pairRate(a.o, bb.o, B.basis);
    const h = pairRate(a.h, bb.l, B.basis);
    const l = pairRate(a.l, bb.h, B.basis);
    const c = pairRate(a.c, bb.c, B.basis);
    if (!o || !h || !l || !c) continue;
    const v = vol.get(t) ?? { a: 0n, b: 0n, n: 0 };
    all.push({ t, o, h, l, c, a_traded: !!ca, b_traded: !!cb, pair_volume_a: v.a.toString(), pair_volume_b: v.b.toString(), pair_fills: v.n });
  }
  const windowed = all.filter((x) => (from === null || x.t >= from) && (to === null || x.t < to));
  const items = from !== null ? windowed.slice(0, limit) : windowed.slice(-limit);
  return {
    base, quote, interval, price_basis: A.basis.toString(), quote_price_basis: B.basis.toString(), decimals: tokA.decimals ?? null, quote_decimals: tokB.decimals ?? null,
    price_source: 'kas_books', items,
  };
}

// ------------------------------------------------------------------------------------------------ fills (volume only)

/** `GET /v1/pairs/{base}/{quote}/fills?limit=&before=`: pair fills newest first and the 24 h pair volume. */
export function pairFillsView(chain, baseId, quoteId, q) {
  const base = parseHash('base', baseId);
  const quote = parseHash('quote', quoteId);
  if (base === quote) throw badRequest('base and quote must differ');
  const limit = limitParam(q, 'limit', 50);
  const before = intParam(q, 'before');
  const nodeDaa = chain.daa();
  const rows = chain.pairFills.filter((f) => f.base === base && f.quote === quote && (before === null || f.id < before)).sort((x, y) => y.id - x.id);
  const page = rows.slice(0, limit);
  const now = chain.events.reduce((m, e) => (e.ts > m ? e.ts : m), 0);
  const since = now - 86_400_000;
  let va = 0n;
  let vb = 0n;
  let n = 0;
  for (const f of chain.pairFills) {
    if (f.base !== base || f.quote !== quote || f.ts < since) continue;
    va += f.amount_a;
    vb += f.amount_b ?? 0n;
    n += 1;
  }
  return {
    base, quote,
    volume_24h: { amount_a: va.toString(), amount_b: vb.toString(), fills: n },
    items: page.map((f) => {
      const frac = f.price !== null && f.price > 0n ? reduce(f.price, f.a_scale) : null;
      const conf = Math.max(0, nodeDaa - f.daa);
      return {
        id: f.id, txid: f.txid, ts: f.ts, daa: f.daa, order: f.order, contract: f.contract, side: f.side, amount_a: f.amount_a.toString(),
        amount_b: f.amount_b === null ? null : f.amount_b.toString(), price: f.price === null ? null : f.price.toString(),
        price_num: frac ? frac.num.toString() : null, price_den: frac ? frac.den.toString() : null, a_scale: Number(f.a_scale),
        tip_kas: f.tip_kas === null ? null : f.tip_kas.toString(), counterparty: f.counterparty, price_source: 'none', confirmations: conf,
        settled: conf >= chain.settleDepthDaa,
      };
    }),
    next_cursor: rows.length > limit && page.length ? String(page[page.length - 1].id) : null,
  };
}

// ------------------------------------------------------------------------------------------------ seed

/** The quote token of the pair seed: a second fictional KCC-20 (6 decimals: scale 10^6 base units per whole token). */
export function quoteTokenSpec(over = {}) {
  return {
    ticker: 'EXUSD',
    name: 'Example USD (fictional)',
    covenant_id: 'e7'.repeat(32),
    program: 'KCC20Ref_8x8',
    extension_commitment: 'ee'.repeat(32),
    decimals: 6,
    // price step of its seeded KAS book: 0.001 KAS per whole EXUSD
    tick: 100_000,
    ...over,
  };
}

/**
 * Seeds a pair around EXKCC / EXUSD (base = the first token of the chain, quote = a second KCC-20): a KAS book for the quote token (0.5 KAS per
 * EXUSD; the default EXKCC book rests around 0.025 KAS) so the route shows ~0.05 EXUSD per EXKCC, resting KobPair asks and a bid, a buy-first
 * if-done entry (an entry level) and a sell stop (a conditional). `quoteMid` is sompi per whole EXUSD. Returns {base, quote, orders}.
 */
export function seedPair(chain, { base, quote, maker = 'maker', quoteMid = 50_000_000, levels = 5 } = {}) {
  const baseTok = chain.tokenRef(base ?? [...chain.tokens.keys()][0]);
  const quoteTok = chain.tokens.get((quote ?? quoteTokenSpec()).covenant_id ?? '') ?? chain.addToken(typeof quote === 'object' ? quote : quoteTokenSpec());
  seedBook(chain, { token: quoteTok.covenant_id, mid: quoteMid, tick: quoteTok.tick ?? 100_000, levels, maker });
  const sA = BigInt(baseTok.scale ?? baseTok.order_scale ?? 1000);
  const sB = BigInt(quoteTok.scale ?? quoteTok.order_scale ?? 1000);
  // EXUSD base units per whole EXKCC at a rate of `r` micro-EXUSD
  const px = (micro) => (micro * sB) / 1_000_000n;
  const pair = { base: baseTok.covenant_id, quote: quoteTok.covenant_id, maker, minFill: sA / 10n };
  const orders = [];
  // asks: sell EXKCC for EXUSD at 0.0505 (2 orders: 2 and 1 EXKCC) and 0.0512 EXUSD per EXKCC (1 EXKCC)
  for (const [whole, micro] of [[2n, 50_500n], [1n, 50_500n], [1n, 51_200n]]) orders.push(seedPairOrder(chain, { ...pair, kind: 'KobPair', side: 'ask', amount: whole * sA, price: px(micro) }));
  // a bid: buy 3 EXKCC at 0.0495 EXUSD each (its EXUSD escrow)
  orders.push(seedPairOrder(chain, { ...pair, kind: 'KobPair', side: 'bid', amount: 3n * sA, price: px(49_500n) }));
  // a buy-first entry at 0.049 (take-profit exit at 0.054): an entry bid level
  orders.push(seedPairOrder(chain, { ...pair, kind: 'KobIfdPair', side: 'bid', amount: 2n * sA, price: px(49_000n), exit: { tp: px(54_000n) } }));
  // a sell stop at 0.045 (a conditional: in no level)
  orders.push(seedPairOrder(chain, { ...pair, kind: 'KobCondPair', side: 'ask', amount: sA, stop: px(45_000n), minTouch: sA / 10n }));
  return { base: baseTok.covenant_id, quote: quoteTok.covenant_id, orders: orders.map((o) => o.covenantId) };
}

export { DEFAULT_CARRIER };
