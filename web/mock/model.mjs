// Order-model helpers of the MOCK indexer: which fields a state exposes and the quote / refund arithmetic behind the read-API views.
// Protocol v3: every quantity is token base units, every price and tip sompi per whole token (`scale` base units of the
// order's token); a pair order (KobPair, KobCondPair, KobIfdPair) trades base token A for quote token B at B base units per whole A (its
// tip stays KAS). Ported (bigint) from crates/kob-protocol/src/state.rs for the READ side only; the amounts the covenants check (a bid's
// buying power and escrow, a pair order's custodies and auction prices) come from kob-wasm's own helpers. Anything consensus-relevant (state encoding, scripts, transactions) goes through kob-wasm; the mock never re-implements that.

export const MAX_IDLE = 77_760_000n; // GTC = 90 days idle at 10 DAA/s
export const IOC_LIFE = 600n;

const BASE_KINDS = {
  KobAsk: { side: 1, inBook: true },
  KobBid: { side: 2, inBook: true },
  KobCondAsk: { side: 1, inBook: false },
  KobCondBid: { side: 2, inBook: false },
  KobIfdBid: { side: 2, inBook: true },
  KobIfdAsk: { side: 1, inBook: true },
  // pair orders (token A for token B, one template each for both sides and families): their side is in their state; they are never in the
  // KAS books (pair book: pairs.mjs)
  KobPair: { side: null, inBook: false },
  KobCondPair: { side: null, inBook: false },
  KobIfdPair: { side: null, inBook: false },
};
export const PAIR_KINDS = ['KobPair', 'KobCondPair', 'KobIfdPair'];
export const isPairKind = (kind) => PAIR_KINDS.includes(kind);

/** Side / book membership per order kind, both families (`KobAskKron` ...; the pair kinds have no KRON twin). */
export const KINDS = Object.fromEntries(
  Object.entries(BASE_KINDS).flatMap(([k, v]) => (isPairKind(k) ? [[k, v]] : [[k, v], [`${k}Kron`, v]])),
);

/** The family-neutral kind (`KobAskKron` -> `KobAsk`). */
export const baseKind = (kind) => (kind.endsWith('Kron') ? kind.slice(0, -4) : kind);

const famName = (code) => (String(code) === '2' ? 'kron' : 'kcc20');
const tokenOf = (s, p) => ({
  covId: s[`${p}CovId`], tplHash: s[`${p}TplHash`], pre: Number(s[`${p}Pre`]), suf: Number(s[`${p}Suf`]), family: famName(s[`${p}Family`]),
  familyCode: Number(s[`${p}Family`]), scale: BigInt(s[`${p}Scale`]), ext: s[`${p}Ext`] ?? null,
});
/**
 * A pair order's tokens oriented A (base) / B (quote) and its side (1 ASK sells A, 2 BID buys A; a buy-first entry is 2), null for every other
 * kind. A KobPair / KobCondPair names S (sold) and T (bought); a KobIfdPair names A and B.
 */
export function pairTokensOf(any) {
  if (!isPairKind(any.kind)) return null;
  const s = any.state;
  const side = Number(s.side);
  if (any.kind === 'KobIfdPair') return { side, a: tokenOf(s, 'a'), b: tokenOf(s, 'b') };
  const S = tokenOf(s, 's');
  const T = tokenOf(s, 't');
  return side === 1 ? { side, a: S, b: T } : { side, a: T, b: S };
}

/** The side of an order (1 sells its token, 2 buys it). */
export const sideOf = (any) => (isPairKind(any.kind) ? Number(any.state.side) : BASE_KINDS[baseKind(any.kind)].side);

/** kob-wasm `custodies`: the exact custodies an order holds, in record order, `[{token, amount: bigint}]`. */
export function custodiesOf(kob, any) {
  return JSON.parse(rawOf(kob).custodies(JSON.stringify(any))).map((c) => ({ token: c.token, amount: BigInt(c.amount) }));
}

const b = (v) => BigInt(v ?? 0);
const max = (x, y) => (x > y ? x : y);
const min = (x, y) => (x < y ? x : y);

/**
 * The quote rule of every covenant (`state::quote_of`): `n * rate / scale`, rounded UP (`'up'`, what a maker receives) or DOWN (`'down'`,
 * what a maker pays). The covenant's split multiplication equals the exact rational rounding whenever it does not overflow, so bigint
 * division gives the same value.
 */
export function quoteOf(n, rate, scale, round) {
  const [nn, r, s] = [b(n), b(rate), b(scale)];
  if (nn < 0n || r < 0n || s <= 0n) return null;
  return round === 'up' ? (nn * r + s - 1n) / s : (nn * r) / s;
}

/** Normalised numeric terms of an order state (`terms_of` in the indexer). All bigint; `price` is the conditional kinds' tpPrice. */
export function termsOf(any) {
  const s = any.state;
  const k = baseKind(any.kind);
  const price = k === 'KobCondAsk' || k === 'KobCondBid' || k === 'KobCondPair' ? b(s.tpPrice) : b(s.price);
  const pt = pairTokensOf(any);
  return {
    scale: pt ? pt.a.scale : b(s.scale),
    minFill: b(s.minFill),
    price,
    tip: b(s.tip),
    tif: b(s.tif),
    expiryDaa: b(s.expiryDaa),
    activeFrom: b(s.activeFrom),
    amountLeft: s.amountLeft === undefined ? null : b(s.amountLeft),
  };
}

/** Exact custody a side-1 KAS order must hold: `amountLeft` token base units (a pair order: `custodiesOf`). */
export const custodyAmount = (any) => termsOf(any).amountLeft;

/** The rate a bid's fill consumes its escrow at: `pMax + tip` sompi per whole token (a rising bid's cap is priceEnd). */
export function bidBudgetRate(any) {
  const s = any.state;
  const price = b(s.slope) !== 0n && b(s.priceEnd) > b(s.price) ? b(s.priceEnd) : b(s.price);
  return price + b(s.tip);
}

/** A buy-first if-done entry's budget rate: `price + tip`. */
export const ifdBudgetRate = (any) => b(any.state.price) + b(any.state.tip);

/** kob-wasm's raw bindings (`raw` of either facade: src/kob/wasm.ts or kob.mjs) behind a mock `kob`. */
const rawOf = (kob) => kob?.raw ?? kob;

/**
 * Base units a bid can still buy (its buying power, `BidState::buying_power` through kob-wasm): the largest n whose budget
 * `ceil(n * (pMax + tip) / scale)` leaves the delivery carrier and the reserve of the escrow `curValue`.
 */
export function bidBuyingPower(kob, any, curValue) {
  return BigInt(rawOf(kob).bidBuyingPower(JSON.stringify(any), String(curValue)));
}

/** KAS a bid escrow needs to buy `amount` base units in at most `fills` fills (`BidState::escrow` through kob-wasm). */
export function bidEscrow(kob, any, amount, fills = 1) {
  const v = rawOf(kob).bidEscrow(JSON.stringify(any), String(amount), String(fills));
  if (v == null) throw new Error('the bid escrow overflows');
  return BigInt(v);
}

const decayOrigin = (activeFrom, interval, utxoDaa) => (interval > 0n ? (activeFrom > utxoDaa + interval ? activeFrom : utxoDaa + interval) : activeFrom);
const armedOrigin = (armed, utxoDaa) => (armed === 0n ? null : armed === 1n ? utxoDaa : armed);

/** Refund-due DAA: soft expiry, 90 days idle, or the IOC / FOK kill time. */
export function refundDue(expiryDaa, tif, activeFrom, utxoDaa) {
  let idle = utxoDaa + MAX_IDLE;
  if (tif !== 0n) idle = min(idle, max(activeFrom, utxoDaa) + IOC_LIFE);
  return min(expiryDaa, idle);
}

const bandBps = (slip, band, origin, t) => {
  if (band <= 0n) return slip;
  const e = t - origin;
  return e < band ? (slip * e) / band : slip;
};

/**
 * The auction path of a tip state (decaying ask, rising bid, armed stop leg, armed stop entry, pair market order) at `nodeDaa`, or null.
 * Shape of `AuctionView` in src/data/indexer-types.ts (bigints as decimal strings).
 */
export function auctionOf(any, utxoDaa, nodeDaa, kob = null) {
  if (utxoDaa == null) return null;
  const now = nodeDaa == null ? null : BigInt(nodeDaa);
  const u = BigInt(utxoDaa);
  const view = (kind, origin, start, end, cur) => {
    const t = now === null ? origin : max(now, origin);
    const current = cur(t);
    return {
      kind,
      origin_daa: Number(origin),
      start_price: start.toString(),
      end_price: end.toString(),
      current_price: current.toString(),
      elapsed_daa: Number(t - origin),
      complete: current === end,
    };
  };
  const s = any.state;
  switch (baseKind(any.kind)) {
    case 'KobAsk': {
      if (b(s.slope) <= 0n || b(s.decayStep) <= 0n) return null;
      const o = decayOrigin(b(s.activeFrom), b(s.interval), u);
      return view('decay', o, b(s.price), b(s.priceEnd), (t) => max(b(s.price) - b(s.slope) * ((t - o) / b(s.decayStep)), b(s.priceEnd)));
    }
    case 'KobBid': {
      if (b(s.slope) <= 0n || b(s.decayStep) <= 0n) return null;
      const o = decayOrigin(b(s.activeFrom), b(s.interval), u);
      return view('rise', o, b(s.price), b(s.priceEnd), (t) => min(b(s.price) + b(s.slope) * ((t - o) / b(s.decayStep)), b(s.priceEnd)));
    }
    case 'KobCondAsk':
    case 'KobCondBid': {
      const o = armedOrigin(b(s.armed), u);
      if (o === null || b(s.bandDaa) <= 0n) return null;
      const stop = b(s.stopPrice);
      const slip = b(s.slipBps);
      const sign = baseKind(any.kind) === 'KobCondAsk' ? -1n : 1n;
      const at = (t) => stop + sign * (stop / 10_000n) * bandBps(slip, b(s.bandDaa), o, t);
      return view('stop', o, stop, at(o + b(s.bandDaa)), at);
    }
    case 'KobIfdBid':
    case 'KobIfdAsk': {
      const entryStop = b(s.entryStop);
      const o = armedOrigin(b(s.armed), u);
      if (entryStop <= 0n || o === null || b(s.bandDaa) <= 0n) return null;
      const price = b(s.price);
      return view('entry', o, entryStop, price, (t) => {
        const e = t - o;
        return e < b(s.bandDaa) ? entryStop + ((price - entryStop) * e) / b(s.bandDaa) : price;
      });
    }
    // pair orders: prices in B base units per whole A, from kob-wasm (the covenants' own arithmetic)
    case 'KobPair': {
      if (!kob || b(s.slope) <= 0n || b(s.decayStep) <= 0n) return null;
      const o = decayOrigin(b(s.activeFrom), b(s.interval), u);
      const at = (t) => b(rawOf(kob).orderPriceAt(JSON.stringify(any), String(t), String(u)) ?? s.price);
      return view(Number(s.side) === 1 ? 'pair_decay' : 'pair_rise', o, b(s.price), b(s.priceEnd), at);
    }
    case 'KobCondPair': {
      const o = armedOrigin(b(s.armed), u);
      if (!kob || o === null || b(s.bandDaa) <= 0n || b(s.stopPrice) <= 0n) return null;
      const at = (t) => b(rawOf(kob).condPairLegPrice(JSON.stringify(any), '1', 'false', String(t), String(u)) ?? s.stopPrice);
      return view('pair_stop', o, b(s.stopPrice), at(o + b(s.bandDaa)), at);
    }
    case 'KobIfdPair': {
      const o = armedOrigin(b(s.armed), u);
      if (!kob || b(s.entryStop) <= 0n || o === null || b(s.bandDaa) <= 0n) return null;
      const at = (t) => b(rawOf(kob).ifdPairPriceAt(JSON.stringify(any), 'false', String(t), String(u)) ?? s.price);
      return view('pair_entry', o, b(s.entryStop), b(s.price), at);
    }
    default:
      return null;
  }
}

/** Repeat IFD / IFO role (`RepeatView`) of a tip state, or null. */
export function repeatOf(any) {
  const s = any.state;
  const zero = '0'.repeat(64);
  const k = baseKind(any.kind);
  if ((k === 'KobIfdBid' || k === 'KobIfdAsk' || k === 'KobIfdPair') && b(s.rptAmount) > 0n) {
    const n = b(s.rptAmount);
    return { role: 'entry', rpt_amount: n.toString(), rearm_amount: max(n - 1n, 0n).toString(), parent: null, rpt_until: null, rpt_price: null };
  }
  if ((k === 'KobCondAsk' || k === 'KobCondBid' || k === 'KobCondPair') && s.parent && s.parent !== zero) {
    return { role: 'exit', rpt_amount: null, rearm_amount: null, parent: s.parent, rpt_until: String(s.rptUntil), rpt_price: String(s.rptPrice) };
  }
  return null;
}
