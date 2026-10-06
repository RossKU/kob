// REST views of the mock indexer: the exact JSON shapes of crates/kob-executor/src/indexer/reads.rs (snake_case, string sompi and base units,
// numeric DAA / counts / scales, `confirmations` + `settled` on every chain-derived view) built from the MockChain state.
import { KINDS, auctionOf, baseKind, bidBudgetRate, custodiesOf, custodyAmount, ifdBudgetRate, isPairKind, pairTokensOf, refundDue, repeatOf, termsOf } from './model.mjs';
import { HEX64, badRequest } from './util.mjs';

export const MAX_PAGE_SIZE = 200;
export const ORDER_STATUSES = ['open', 'partial', 'filled', 'cancelled', 'refunded', 'closed', 'active'];

// ------------------------------------------------------------------------------------------------ query parsing (api/rest.rs)

export function limitParam(q, key, dflt, max = MAX_PAGE_SIZE) {
  const raw = q.get(key);
  if (raw === null) return Math.min(dflt, max);
  const n = Number(raw);
  if (!/^\d+$/.test(raw) || n < 1) throw badRequest(`${key} must be a positive integer`);
  return Math.min(n, max);
}
export const flagParam = (q, key) => {
  const v = q.get(key);
  if (v === null) return null;
  if (v === 'true' || v === '1') return true;
  if (v === 'false' || v === '0') return false;
  throw badRequest(`${key} must be true or false`);
};
export const hashParam = (q, key) => {
  const v = q.get(key);
  if (v === null) return null;
  return parseHash(key, v);
};
export const parseHash = (what, v) => {
  const h = v.toLowerCase();
  if (!HEX64.test(h)) throw badRequest(`${what} must be 32 bytes of hex`);
  return h;
};
export const intParam = (q, key) => {
  const v = q.get(key);
  if (v === null) return null;
  if (!/^-?\d+$/.test(v)) throw badRequest(`${key} must be an integer`);
  return Number(v);
};
export const hexParam = (q, key) => {
  const v = q.get(key);
  if (v === null) return null;
  if (!/^([0-9a-fA-F]{2})+$/.test(v)) throw badRequest(`${key} must be hex`);
  return v.toLowerCase();
};

// ------------------------------------------------------------------------------------------------ context

export function ctxOf(chain) {
  return {
    nodeDaa: chain.health.state === 'node_unavailable' ? null : chain.daa(),
    settle: chain.settleDepthDaa,
    nowUnix: chain.nowUnix(),
  };
}
const conf = (ctx, daa) => (ctx.nodeDaa === null ? null : Math.max(0, ctx.nodeDaa - daa));
const settled = (ctx, daa) => {
  const c = conf(ctx, daa);
  return c !== null && c >= ctx.settle;
};
const active = (o) => o.status === 'open' || o.status === 'partial';
const str = (v) => (v === null || v === undefined ? null : String(v));

// ------------------------------------------------------------------------------------------------ health / tokens

export function counters(chain) {
  const byStatus = {};
  for (const o of chain.orders.values()) byStatus[o.status] = (byStatus[o.status] ?? 0) + 1;
  return {
    orders_total: chain.orders.size,
    orders_listed: [...chain.orders.values()].filter((o) => o.listed).length,
    orders_by_status: byStatus,
    rejects_total: chain.rejects.length,
    fills_total: chain.events.filter((e) => e.kind === 'fill').length,
    last_block_seq: chain.blockSeq,
    last_block_daa: chain.daa(),
  };
}

export function healthView(chain) {
  const ctx = ctxOf(chain);
  const st = chain.health.state;
  const lagDaa = ctx.nodeDaa === null ? null : chain.health.lagDaa;
  const lagSeconds = lagDaa === null ? null : Math.floor(lagDaa / 10);
  const running = st === 'following' || st === 'catching_up';
  return {
    state: st,
    ok: running,
    network: chain.network,
    node_version: 'kob-mock-node/1.0',
    cursor_hash: chain.blockSeq === 0 ? null : chain.events.length ? chain.events[chain.events.length - 1].txid : null,
    cursor_daa: ctx.nodeDaa === null ? 0 : ctx.nodeDaa - (chain.health.lagDaa ?? 0),
    node_daa: ctx.nodeDaa,
    last_error: st === 'node_unavailable' ? 'mock: node unavailable' : null,
    gap_reason: null,
    lag_daa: lagDaa,
    lag_seconds: lagSeconds,
    alarms: [],
    settle_depth_daa: chain.settleDepthDaa,
    counters: counters(chain),
  };
}

export function readyView(chain) {
  const v = healthView(chain);
  const ready = v.ok;
  return { status: ready ? 200 : 503, body: { ready, reason: ready ? 'ok' : 'indexer is not following the chain', state: v.state, lag_seconds: v.lag_seconds } };
}

export function tokensView(chain) {
  const count = (tok, side) =>
    [...chain.orders.values()].filter((o) => o.token === tok && o.side === side && o.listed && o.inBook && active(o) && !o.possiblyFrozen).length;
  return {
    tokens: [...chain.tokens.values()].filter((t) => !t.hidden).map((t) => ({
      ticker: t.ticker,
      covenant_id: t.covenant_id,
      template_hash: t.template_hash,
      extension_commitment: t.extension_commitment,
      decimals: t.decimals,
      scale: t.scale,
      open_asks: count(t.covenant_id, 1),
      open_bids: count(t.covenant_id, 2),
      standing: t.standing,
      powers: t.powers,
      template_id: t.template_id,
      family: t.family,
    })),
  };
}

// ------------------------------------------------------------------------------------------------ token UTXOs

/** The tokens an order's own transfers move: its token and a pair order's quote token B (a stray of any other token is foreign). */
const orderTokens = (o) => [o.token, o.quote ?? null].filter(Boolean);

export function tokenUtxoView(rec, ctx, extra = {}, chain = null) {
  const tok = chain?.tokens.get(rec.token) ?? null;
  // a stray of a token other than its order's own: FOREIGN (reads.rs TokenUtxoView `foreign`); `program` = the program its state was proven under
  const owner = rec.role === 'stray' ? chain?.orders.get(rec.owner) : null;
  const foreign = !!owner && !orderTokens(owner).includes(rec.token);
  return {
    txid: rec.txid,
    index: rec.index,
    token: rec.token,
    ...(tok ? { family: tok.family ?? 'kcc20', program: tok.program, template_hash: tok.template_hash } : {}),
    ...(foreign ? { foreign: true } : {}),
    owner: rec.owner,
    owner_kind: rec.state.owner_scheme,
    amount: rec.amount.toString(),
    value: rec.value.toString(),
    role: rec.role,
    created_daa: rec.created_daa,
    spent: rec.spent,
    spent_txid: rec.spent_txid,
    state: rec.state,
    confirmations: conf(ctx, rec.created_daa),
    settled: settled(ctx, rec.created_daa),
    ...extra,
  };
}

/** `/v1/token-utxos?owner=&token=&spent=&limit=&cursor=` (api/rest.rs): at least one of owner / token, `spent=false` by default, keyset paged oldest first. */
export function tokenUtxosPage(chain, q) {
  const ctx = ctxOf(chain);
  const owner = hashParam(q, 'owner');
  const token = hashParam(q, 'token');
  if (owner === null && token === null) throw badRequest('owner or token is required');
  const role = q.get('role');
  const spent = flagParam(q, 'spent') ?? false; // true = include spent holdings
  const limit = limitParam(q, 'limit', 100);
  const cursor = q.get('cursor') === null ? 0 : intParam(q, 'cursor');
  const rows = chain.tokenUtxos.filter(
    (t) => t.seq > cursor && (owner === null || t.owner === owner) && (token === null || t.token === token) && (role === null || t.role === role) && (spent || !t.spent),
  );
  const items = rows.slice(0, limit).map((t) => tokenUtxoView(t, ctx, {}, chain));
  return { items, next_cursor: rows.length > limit ? String(rows[limit - 1].seq) : null };
}

export function straysView(chain, q) {
  const ctx = ctxOf(chain);
  const maker = hexParam(q, 'maker');
  const limit = limitParam(q, 'limit', 50);
  const items = chain.tokenUtxos
    .filter((t) => t.role === 'stray' && !t.spent)
    .map((t) => ({ t, o: chain.orders.get(t.owner) }))
    .filter(({ o }) => maker === null || o?.state.state.maker === maker)
    .sort((a, b) => b.t.seq - a.t.seq)
    .slice(0, limit)
    .map(({ t, o }) => tokenUtxoView(t, ctx, { order_status: o?.status ?? null, maker: o?.state.state.maker ?? null, lost: !(o && active(o)) }, chain));
  return { items };
}

export function tokenEventsView(chain, q) {
  const token = hashParam(q, 'token');
  const limit = limitParam(q, 'limit', 50);
  return { items: chain.tokenEvents.filter((e) => token === null || e.token === token).slice(0, limit) };
}

// ------------------------------------------------------------------------------------------------ orders

function custodyView(chain, o, ctx) {
  if (isPairKind(o.kind)) {
    // a pair order: its FIRST custody (`ok` only when every custody of the state is exact)
    const parts = pairCustodyParts(chain, o, ctx, true);
    const first = parts[0] ?? null;
    return { expected_amount: first ? first.expected_amount : null, utxo: first?.utxo ?? null, ok: parts.length > 0 && parts.every((x) => x.ok) };
  }
  const expected = o.current && o.stateKnown ? custodyAmount(o.state) : null;
  const live = chain.liveTokenUtxos(o.covenantId, 'custody');
  const ok = expected === null ? false : expected === 0n ? live.length === 0 : live.length === 1 && live[0].amount === expected;
  return { expected_amount: str(expected), utxo: live.length === 1 ? tokenUtxoView(live[0], ctx) : null, ok };
}

const gcd = (x, y) => (y === 0n ? x : gcd(y, x % y));
const frac = (num, den) => {
  if (num <= 0n || den <= 0n) return { num: null, den: null };
  const g = gcd(num, den) || 1n;
  return { num: (num / g).toString(), den: (den / g).toString() };
};

/** The custodies of a pair order's current state in record order (`utxo` / `ok` with `withUtxos`); empty once closed. */
function pairCustodyParts(chain, o, ctx, withUtxos) {
  if (!o.current || !o.stateKnown) return [];
  const pt = pairTokensOf(o.state);
  const live = chain.liveTokenUtxos(o.covenantId, 'custody');
  return custodiesOf(chain.kob, o.state).map((c) => {
    const mine = live.filter((u) => u.token === c.token);
    const exact = mine.length === 1 && mine[0].amount === c.amount;
    return {
      token: c.token, role: c.token === pt.b.covId ? 'quote' : 'base', expected_amount: c.amount.toString(),
      ...(withUtxos ? { utxo: mine.length === 1 ? tokenUtxoView(mine[0], ctx, {}, chain) : null, ok: c.amount === 0n ? mine.length === 0 : exact } : {}),
    };
  });
}

/**
 * The `pair` object of a pair order's `OrderView` (docs/ops/executor.md 5.4, reads.rs `PairView`): both tokens, the side, the price in B base units
 * per whole A (a KobPair's price, a KobIfdPair's limit, a KobCondPair's take-profit / limit leg or null), its stop, the quote now, the custodies.
 */
function pairView(chain, o, ctx, auction, withUtxos) {
  const any = o.state;
  const s = any.state;
  const pt = pairTokensOf(any);
  const price = o.kind === 'KobCondPair' ? (BigInt(s.tpPrice) > 0n ? BigInt(s.tpPrice) : null) : BigInt(s.price);
  const stop = o.kind === 'KobCondPair' ? s.stopPrice : o.kind === 'KobIfdPair' ? s.entryStop : null;
  const f = price !== null ? frac(price, pt.a.scale) : { num: null, den: null };
  const live = o.current !== null && active(o);
  return {
    base: pt.a.covId, quote: pt.b.covId, base_family: pt.a.family, quote_family: pt.b.family, base_template_hash: pt.a.tplHash, quote_template_hash: pt.b.tplHash,
    base_scale: Number(pt.a.scale), quote_scale: Number(pt.b.scale), side: pt.side === 1 ? 'ask' : 'bid',
    price: price === null ? null : price.toString(), price_num: f.num, price_den: f.den,
    ...(stop !== null ? { stop_price: BigInt(stop) > 0n ? String(stop) : null } : {}),
    quote_now: auction ? auction.current_price : price === null ? null : price.toString(),
    amount_left: live ? String(s.amountLeft) : null,
    custodies: live ? pairCustodyParts(chain, o, ctx, withUtxos) : [],
    ...(o.kind === 'KobIfdPair' && pt.side === 1 ? { prefund: String(s.prefund) } : {}),
    delivery_carrier: String(s.deliveryCarrier),
  };
}

/** `OrderView`. `detailed` adds the single-lookup fields (children, custody, strays). */
export function orderView(chain, o, ctx, detailed = false) {
  const any = o.state;
  const s = any.state;
  const t = termsOf(any);
  const tpl = chain.templates.get(o.kind);
  const stateKnown = o.stateKnown && o.current !== null;
  const auction = stateKnown ? auctionOf(any, o.currentDaa, ctx.nodeDaa, chain.kob) : null;
  const repeat = stateKnown ? repeatOf(any) : null;
  let refundDueDaa = null;
  let killDaa = null;
  if (stateKnown && o.currentDaa != null) {
    refundDueDaa = Number(refundDue(t.expiryDaa, t.tif, t.activeFrom, BigInt(o.currentDaa)));
    if (t.tif === 1n || t.tif === 2n) killDaa = refundDueDaa;
  }
  const inBook = KINDS[o.kind].inBook;
  const kind = baseKind(o.kind);
  const pair = isPairKind(o.kind);
  const pt = pair ? pairTokensOf(any) : null;
  const hasTif = kind === 'KobAsk' || kind === 'KobBid' || kind === 'KobPair';
  const expiry = Number(t.expiryDaa);
  const view = {
    covenant_id: o.covenantId,
    contract: o.kind,
    template_hash: tpl.hash,
    family: o.kind.endsWith('Kron') ? 2 : pt ? pt.a.familyCode : 1,
    side: o.side,
    maker: s.maker ?? null,
    token: o.token,
    token_template_hash: pt ? pt.a.tplHash : (s.tokenTplHash ?? null),
    extension_commitment: o.ext,
    scale: Number(t.scale),
    min_fill: t.minFill.toString(),
    price: inBook ? t.price.toString() : null,
    tip: t.tip.toString(),
    tif: hasTif ? Number(t.tif) : null,
    expiry_daa: expiry,
    active_from: Number(t.activeFrom),
    in_book: inBook,
    // the rate a fill consumes the escrow at, sompi per whole token: a bid's pMax + tip, a buy-first entry's price + tip
    budget_rate: kind === 'KobBid' ? bidBudgetRate(any).toString() : kind === 'KobIfdBid' ? ifdBudgetRate(any).toString() : null,
    reserve: kind === 'KobBid' ? String(s.reserve) : null,
    initial_amount: o.initialAmount === null ? null : o.initialAmount.toString(),
    listed: o.listed,
    unlisted_reason: o.unlistedReason,
    origin: o.origin,
    parent: o.parent,
    genesis: { txid: o.genesis.txid, out: o.genesis.out, block_seq: o.genesis.seq, daa: o.genesis.daa, confirmations: conf(ctx, o.genesis.daa), settled: settled(ctx, o.genesis.daa) },
    status: o.status,
    filled_amount: o.filledAmount.toString(),
    amount_left: o.amountLeft === null ? null : o.amountLeft.toString(),
    amount_estimated: !o.amountExact || o.amountLeft === null,
    current: o.current ? { txid: o.current.txid, index: o.current.index, value: o.current.value.toString() } : null,
    state_known: o.stateKnown,
    state: stateKnown ? any : null,
    current_daa: o.current ? o.currentDaa : null,
    deadline: o.deadline,
    deadline_passed: active(o) && o.deadline !== null && ctx.nowUnix >= o.deadline,
    refund_due_daa: refundDueDaa,
    kill_daa: killDaa,
    auction,
    quote: pair ? null : auction ? auction.current_price : inBook ? t.price.toString() : null,
    repeat,
    last_block: o.lastSeq,
    last_daa: o.lastDaa,
    confirmations: conf(ctx, o.lastDaa),
    settled: settled(ctx, o.lastDaa),
    expired: active(o) && ctx.nodeDaa !== null && ctx.nodeDaa >= expiry,
    possibly_frozen: !!o.possiblyFrozen,
  };
  // a pair order: its pair, prices in B per whole A and custodies (utxo / ok on single lookups and a maker's list)
  if (pair) view.pair = pairView(chain, o, ctx, stateKnown ? auction : null, detailed || ctx.makerList === true);
  if (detailed) {
    view.children = [...o.children];
    if (o.side === 1 || pair) view.custody = custodyView(chain, o, ctx);
    view.strays = chain.liveTokenUtxos(o.covenantId, 'stray').map((r) => tokenUtxoView(r, ctx, {}, chain));
  }
  return view;
}

const cmpKey = (a, b) => (a[0] !== b[0] ? a[0] - b[0] : a[1] < b[1] ? -1 : a[1] > b[1] ? 1 : 0);

export function ordersPage(chain, q) {
  const ctx = ctxOf(chain);
  const maker = hexParam(q, 'maker');
  const token = hashParam(q, 'token');
  const status = q.get('status');
  if (status !== null && !ORDER_STATUSES.includes(status)) throw badRequest(`status must be one of ${ORDER_STATUSES.join(', ')}`);
  let cursor = null;
  if (q.get('cursor') !== null) {
    const m = /^(\d+):([0-9a-fA-F]+)$/.exec(q.get('cursor'));
    if (!m) throw badRequest('invalid cursor');
    cursor = [Number(m[1]), m[2].toLowerCase()];
  }
  const limit = limitParam(q, 'limit', 50);
  const rows = [...chain.orders.values()]
    // a pair order is listed under both its tokens
    .filter((o) => (maker === null || o.state.state.maker === maker) && (token === null || o.token === token || o.quote === token) && (status === null || (status === 'active' ? active(o) : o.status === status)))
    .filter((o) => cursor === null || cmpKey([o.genesis.seq, o.covenantId], cursor) < 0)
    .sort((a, b) => cmpKey([b.genesis.seq, b.covenantId], [a.genesis.seq, a.covenantId]));
  const page = rows.slice(0, limit);
  const last = page[page.length - 1];
  const listCtx = maker !== null ? { ...ctx, makerList: true } : ctx;
  return { items: page.map((o) => orderView(chain, o, active(o) ? listCtx : ctx)), next_cursor: rows.length > limit && last ? `${last.genesis.seq}:${last.covenantId}` : null };
}

// ------------------------------------------------------------------------------------------------ book

/** Ask-side liquidity needs exactly one live custody UTXO holding `amountLeft` base units (matcher.md 1.2); bids hold KAS. */
function custodyOk(chain, o) {
  if (o.side === 2) return true;
  const live = chain.liveTokenUtxos(o.covenantId, 'custody');
  return live.length === 1 && live[0].amount === custodyAmount(o.state);
}

/** Price per base unit (`price / scale`) compared exactly by cross-multiplication. */
const cmpPerBaseUnit = (pa, sa, pb, sb) => {
  const x = pa * sb;
  const y = pb * sa;
  return x < y ? -1 : x > y ? 1 : 0;
};

function bookOrders(chain, token, side) {
  return [...chain.orders.values()]
    .filter((o) => o.token === token && o.side === side && o.listed && o.inBook && active(o) && o.stateKnown && !o.possiblyFrozen && custodyOk(chain, o))
    .sort((a, b) => {
      const ta = termsOf(a.state);
      const tb = termsOf(b.state);
      const c = cmpPerBaseUnit(ta.price, ta.scale > 0n ? ta.scale : 1n, tb.price, tb.scale > 0n ? tb.scale : 1n);
      if (c !== 0) return side === 1 ? c : -c;
      return a.genesis.seq - b.genesis.seq || (a.covenantId < b.covenantId ? -1 : 1);
    });
}

export function bookView(chain, tokenId, q) {
  const token = parseHash('token', tokenId);
  const ctx = ctxOf(chain);
  const depth = limitParam(q, 'depth', 20);
  const aggregate = flagParam(q, 'aggregate') ?? true;
  const side = (n) => {
    const orders = bookOrders(chain, token, n);
    if (!aggregate) {
      return orders.slice(0, depth).map((o) => {
        const t = termsOf(o.state);
        const auction = auctionOf(o.state, o.currentDaa, ctx.nodeDaa);
        const expiry = Number(t.expiryDaa);
        return {
          covenant_id: o.covenantId,
          contract: o.kind,
          maker: o.state.state.maker,
          price: t.price.toString(),
          quote: auction ? auction.current_price : t.price.toString(),
          auction,
          tip: t.tip.toString(),
          min_fill: t.minFill.toString(),
          scale: Number(t.scale),
          amount_left: o.amountLeft === null ? null : o.amountLeft.toString(),
          amount_estimated: !o.amountExact || o.amountLeft === null,
          status: o.status,
          cur_value: o.current ? o.current.value.toString() : null,
          expiry_daa: expiry,
          expired: ctx.nodeDaa !== null && ctx.nodeDaa >= expiry,
          deadline: o.deadline,
          deadline_passed: o.deadline !== null && ctx.nowUnix >= o.deadline,
          genesis_daa: o.genesis.daa,
          confirmations: conf(ctx, o.genesis.daa),
          settled: settled(ctx, o.genesis.daa),
        };
      });
    }
    const levels = new Map();
    for (const o of orders) {
      // levels group by (price, scale): orders of another scale quote another whole token (kob-executor reads.rs `book_side_levels`)
      const t = termsOf(o.state);
      const scale = t.scale > 0n ? t.scale : 1n;
      const key = `${t.price}:${scale}`;
      const lv = levels.get(key) ?? { price: t.price, amount: 0n, amount_estimated: false, orders: 0, scale };
      lv.amount += o.amountLeft === null || o.amountLeft < 0n ? 0n : o.amountLeft;
      lv.orders += 1;
      lv.amount_estimated = lv.amount_estimated || !o.amountExact || o.amountLeft === null;
      levels.set(key, lv);
    }
    // ordered exactly by the price per base unit (asks ascending, bids descending), then by price, then by scale (same direction)
    const dir = n === 1 ? 1 : -1;
    const cmp = (a, b) => cmpPerBaseUnit(a.price, a.scale, b.price, b.scale) || (a.price < b.price ? -1 : a.price > b.price ? 1 : 0) || (a.scale < b.scale ? -1 : a.scale > b.scale ? 1 : 0);
    return [...levels.values()]
      .sort((a, b) => dir * cmp(a, b))
      .slice(0, depth)
      .map((l) => ({ price: l.price.toString(), amount: l.amount.toString(), amount_estimated: l.amount_estimated, orders: l.orders, scale: Number(l.scale) }));
  };
  return { token, aggregated: aggregate, node_daa: ctx.nodeDaa, asks: side(1), bids: side(2) };
}

// ------------------------------------------------------------------------------------------------ events / fills

const eventView = (e, ctx) => ({ ...e, confirmations: conf(ctx, e.daa), settled: settled(ctx, e.daa) });

export function orderEventsPage(chain, covId, q) {
  const id = parseHash('covenant_id', covId);
  const after = intParam(q, 'after');
  const limit = limitParam(q, 'limit', 100);
  const ctx = ctxOf(chain);
  const rows = chain.events.filter((e) => e.covenant_id === id && e.id > (after ?? 0));
  const page = rows.slice(0, limit);
  return { items: page.map((e) => eventView(e, ctx)), next_cursor: rows.length > limit ? String(page[page.length - 1].id) : null };
}

export function fillsPage(chain, q) {
  const token = hashParam(q, 'token');
  const sideRaw = q.get('side');
  let side = null;
  if (sideRaw !== null) {
    if (['1', 'ask', 'sell'].includes(sideRaw)) side = 1;
    else if (['2', 'bid', 'buy'].includes(sideRaw)) side = 2;
    else throw badRequest('side must be 1|2|ask|bid');
  }
  const before = intParam(q, 'before');
  const limit = limitParam(q, 'limit', 50);
  const ctx = ctxOf(chain);
  const rows = chain.events
    .filter((e) => e.kind === 'fill' && (token === null || e.token === token) && (side === null || e.side === side) && (before === null || e.id < before))
    .reverse();
  const page = rows.slice(0, limit);
  return { items: page.map((e) => eventView(e, ctx)), next_cursor: rows.length > limit ? String(page[page.length - 1].id) : null };
}
