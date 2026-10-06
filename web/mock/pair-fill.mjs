// Simulated fills, arms and trails of PAIR orders (KobPair, KobCondPair, KobIfdPair) for the mock's control API, as REAL transactions: a batch
// built by kob-wasm (`build` action `batch`, the matcher's own builder), signed by the mock's filler key, finalized and accepted through
// `MockChain.submit`, so the script engine runs every covenant (the pair order's guarantees, the stray guards, its exits and custodies).
//
//   counterparty (`via`): `inventory` (the filler's own tokens: minted on demand), `netting` (an opposite pair order of the same pair: an existing
//   one given as `against`, or one seeded for the occasion) or `route` (KAS-book orders of A and of B, seeded for the occasion and filled in the
//   same transaction: they record their own KAS trades);
//   trigger evidence (`mode`): 0, two resting KAS-book orders (one of A, one of B) filled together at quotes that imply the rate; 1, a resting KobPair
//   of the same pair. Evidence orders are seeded by another key ("carol"), backdated so they have rested minRestDaa, and filled in the batch.
//
// What a batch does to the orders it spends (their continuation states, the exit an if-done entry's fill creates) is derived here: each candidate
// state's script is computed by kob-wasm and matched against the batch's outputs (the real indexer splices the mutable windows instead). The chain
// then applies the batch with those states (`MockChain.applyKnownSpend`): fill events with the pair fields and NO price, `pair_fills` rows.
import { schnorr } from '@noble/curves/secp256k1.js';
import { TEST_PUBKEYS, TEST_SECRETS } from './keys.mjs';
import { MAX_IDLE, baseKind, bidEscrow, isPairKind, pairTokensOf } from './model.mjs';
import { pairOrderState, seedPairOrder } from './pairs.mjs';
import { buildOrderState } from './seed.mjs';
import { badRequest } from './util.mjs';

const FILLER = TEST_PUBKEYS.filler;
const FILLER_SK = TEST_SECRETS.filler;
const J = JSON.stringify;
const b = (v) => BigInt(v ?? 0);
const KAS = 100_000_000n;
const ZERO32 = '00'.repeat(32);
const hexBytes = (h) => Uint8Array.from(Buffer.from(h, 'hex'));

// ------------------------------------------------------------------------------------------------ utxos of the request

function orderUtxo(chain, o, tagged = false) {
  const u = chain.utxos.get(`${o.current.txid}:${o.current.index}`);
  if (!u || u.spentBy) throw badRequest(`order ${o.covenantId} has no live UTXO`);
  return { transactionId: u.txid, index: u.index, amount: u.amount.toString(), blockDaaScore: String(u.daa), covenantId: o.covenantId, state: tagged ? o.state : o.state.state };
}
const tokenUtxoOf = (rec) => ({ transactionId: rec.txid, index: rec.index, amount: rec.value.toString(), blockDaaScore: String(rec.created_daa), covenantId: rec.token, state: rec.state });
function custodyOf(chain, o, token) {
  const r = chain.liveTokenUtxos(o.covenantId, 'custody').find((u) => u.token === token);
  return r ? tokenUtxoOf(r) : null;
}
/** Mints `amount` of `token` to the filler (one P2PK-owned token UTXO). */
function mintTokens(chain, token, amount) {
  const g = chain.giveTokens(FILLER, token, amount);
  return tokenUtxoOf(chain.tokenUtxos.find((t) => t.txid === g.transactionId && t.index === 0));
}
/** Mints KAS to the filler (the batch's funding). */
function mintKas(chain, sompi) {
  const g = chain.giveKas(FILLER, sompi);
  const u = chain.utxos.get(`${g.transactionId}:0`);
  return { transactionId: g.transactionId, index: 0, amount: g.amount, blockDaaScore: String(u.daa), covenantId: null, pubkey: FILLER };
}

/** The batch leg of order `o` filling `n` base units (`extra`: a conditional's leg, evidence leg indices, a merge). */
function legOf(chain, o, n, extra = {}) {
  const order = orderUtxo(chain, o);
  const pt = pairTokensOf(o.state);
  const ev = extra.evidence !== undefined && extra.evidence !== null ? { evidence: extra.evidence, ...(extra.evidenceB !== undefined ? { evidenceB: extra.evidenceB } : {}) } : { evidence: null };
  switch (baseKind(o.kind)) {
    case 'KobPair':
      return { kind: 'pair', order, custody: custodyOf(chain, o, pt.side === 1 ? pt.a.covId : pt.b.covId), amount: String(n), t: null };
    case 'KobCondPair':
      return { kind: 'condPair', order, custody: custodyOf(chain, o, pt.side === 1 ? pt.a.covId : pt.b.covId), amount: String(n), leg: extra.leg ?? 0, ...ev, t: null, merge: extra.merge ?? null };
    case 'KobIfdPair':
      return { kind: 'ifdPair', order, aCustody: custodyOf(chain, o, pt.a.covId), bCustody: custodyOf(chain, o, pt.b.covId), amount: String(n), ...ev, t: null };
    case 'KobAsk':
      return { kind: 'ask', order, custody: custodyOf(chain, o, o.token), amount: String(n), t: null };
    case 'KobBid':
      return { kind: 'bid', order, amount: String(n), t: null };
    default:
      throw badRequest(`${o.kind} cannot be a leg of a simulated batch`);
  }
}

// ------------------------------------------------------------------------------------------------ quotes and amounts (kob-wasm)

const raw = (chain) => chain.kob.raw;
const opt = (v) => (v === null || v === undefined ? null : BigInt(v));

/** The quote of a pair order at DAA `t` (B per whole A): a KobPair's price (decay / rise), a conditional's leg price, an entry's price. */
function quoteAt(chain, o, t, { leg = 0, trigger = false } = {}) {
  const any = J(o.state);
  const u = String(o.currentDaa);
  if (o.kind === 'KobPair') return opt(raw(chain).orderPriceAt(any, String(t), u));
  if (o.kind === 'KobCondPair') return opt(raw(chain).condPairLegPrice(any, String(leg), String(trigger), String(t), u));
  if (o.kind === 'KobIfdPair') return opt(raw(chain).ifdPairPriceAt(any, String(trigger), String(t), u));
  return null;
}

/** What the order's counterparty must supply for a fill of `n` at `p`: {token, amount} of what the pair order receives (T). */
function supplyOf(chain, o, n, p) {
  const pt = pairTokensOf(o.state);
  if (pt.side === 2) return { token: pt.a.covId, amount: n };
  const any = J(o.state);
  if (o.kind === 'KobIfdPair') {
    const a = JSON.parse(raw(chain).ifdPairAmounts(any, String(n), String(p)));
    return { token: pt.b.covId, amount: b(a.proceeds) };
  }
  return { token: pt.b.covId, amount: b(raw(chain).pairTOutMin(any, String(n), String(p))) };
}

// ------------------------------------------------------------------------------------------------ evidence

/**
 * Seeds the trigger evidence of pair stop `o` in `mode` (0 KAS books, 1 a resting pair order) for `effect` (`arm` or `trail`), backdated so it has
 * rested, and returns its legs and what the filler must supply for them. Quotes follow the order's rule (kob-wasm `pairTriggerRule`), checked with
 * kob-wasm `pairArms` / `condPairTrailK` before anything is built.
 */
function seedEvidence(chain, o, mode, effect) {
  const rule = JSON.parse(raw(chain).pairTriggerRule(J(o.state)));
  if (!rule) throw badRequest(`${o.kind} has no stop`);
  const pt = pairTokensOf(o.state);
  const s = o.state.state;
  const stop = b(rule.stop);
  const minTouch = b(rule.minTouch);
  const back = chain.daa() - Number(rule.minRestDaa) - 20;
  const sides = effect === 'trail' ? rule.trail : rule.arm;
  if (!sides) throw badRequest('this stop does not trail');
  const step = b(s.trailStep);
  const gap = b(s.trailGap);
  const up = rule.trail?.direction === 'up';
  // the rate the evidence implies: at the stop (arm), or one step beyond it (trail)
  const target = effect === 'trail' ? (up ? stop + step + gap : stop - step - gap) : stop;
  const check = (ev) => {
    const ok = effect === 'trail' ? raw(chain).condPairTrailK(J(o.state), J(ev)) != null : raw(chain).pairArms(J(o.state), J(ev)) === true;
    if (!ok) throw badRequest(`the evidence ${J(ev)} does not ${effect} this order`);
  };
  if (mode === 1) {
    check({ mode: 'pair', price: target.toString() });
    const ev = seedPairOrder(chain, { kind: 'KobPair', side: sides.pair, base: pt.a.covId, quote: pt.b.covId, amount: minTouch, minFill: minTouch, price: target, maker: 'carol', daa: back });
    return { orders: [ev], amounts: [minTouch], supply: [supplyOf(chain, ev, minTouch, target)] };
  }
  // mode 0: B quoted at 1 KAS per whole B; A at the quote that implies the target rate (r = a x scale(B) / b)
  const bq = KAS;
  const aq = sides.kasBooks.a === 'ask' ? (target * bq) / pt.b.scale : (target * bq + pt.b.scale - 1n) / pt.b.scale;
  check({ mode: 'kasBooks', a: aq.toString(), b: bq.toString() });
  const nA = minTouch;
  const nB = b(rule.minTouchB ?? 1) > 0n ? b(rule.minTouchB) : 1n;
  const kas = (token, side, price, n, scale) => {
    const any = buildOrderState(chain, { token, side, price: price.toString(), amount: n, min_fill: 1, maker: 'carol', scale: scale.toString() });
    const value = side === 'bid' ? bidEscrow(chain.kob, any, n, 1) : undefined;
    return chain.seedOrder(any, { value, daa: back });
  };
  const evA = kas(pt.a.covId, sides.kasBooks.a, aq, nA, pt.a.scale);
  const evB = kas(pt.b.covId, sides.kasBooks.b, bq, nB, pt.b.scale);
  // a resting ask is bought with KAS (funding); a resting bid is sold to with the filler's tokens
  const supply = [sides.kasBooks.a === 'bid' ? { token: pt.a.covId, amount: nA } : null, sides.kasBooks.b === 'bid' ? { token: pt.b.covId, amount: nB } : null];
  return { orders: [evA, evB], amounts: [nA, nB], supply };
}

// ------------------------------------------------------------------------------------------------ build, sign, derive, submit

/** Builds the batch, signs the filler's inputs, finalizes it, derives what it does to the orders and submits it to the mock chain. */
function run(chain, request) {
  let built;
  try {
    built = JSON.parse(raw(chain).build(J(request)));
  } catch (e) {
    const msg = String(e?.message ?? e);
    // kob-protocol's budget table has no role for some pair updates (e.g. a buy stop's arm on mode 1 evidence): arm it in its own fill instead
    const hint = /no compute budget for input role `Kob(Cond|Ifd)Pair\.update/.test(msg) ? ' (no keeper update for this case: arm it in its fill, POST /mock/fill with leg 1 and mode)' : '';
    throw badRequest(`kob-wasm refused the batch: ${msg}${hint}`);
  }
  const sigs = built.sign.map((r) => {
    if (r.pubkey !== FILLER) throw badRequest(`the batch asks for a signature by ${r.pubkey}, not the mock's filler`);
    return { inputIndex: r.inputIndex, signature: Buffer.from(schnorr.sign(hexBytes(r.sighash), hexBytes(FILLER_SK))).toString('hex') + '01' };
  });
  const signed = JSON.parse(raw(chain).finalize(J(built), J(sigs), J({ tightenBudgets: true })));
  const pending = derive(chain, built, signed.tx, b(request.lockTime));
  chain.pending.set(signed.tx.id, pending);
  try {
    chain.submit(signed.tx);
  } finally {
    chain.pending.delete(signed.tx.id);
  }
  return { transactionId: signed.tx.id, pending, built };
}

/** Field variants (cartesian product of `fields` value lists) of a state, as tagged states. */
function variants(any, fields) {
  let out = [{}];
  for (const [k, vals] of Object.entries(fields)) {
    const next = [];
    for (const o of out) for (const v of [...new Set(vals.filter((x) => x !== null && x !== undefined).map(String))]) next.push({ ...o, [k]: v });
    out = next;
  }
  return out.map((f) => ({ kind: any.kind, state: { ...any.state, ...f } }));
}

/** The 8-byte little-endian fill argument `nb` (signed). */
function fillArg(a) {
  if (!a || a.kind !== 'bytes' || !/^[0-9a-f]{16}$/i.test(a.value)) return null;
  const v = Buffer.from(a.value, 'hex').readBigUInt64LE(0);
  return v >= 1n << 63n ? -(v & ((1n << 63n) - 1n)) : v;
}
const intArg = (args, k) => (args[k] && args[k].kind === 'int' ? BigInt(args[k].value) : null);

/** Token output states of a built tx by output index (the leader plan of each token lists them in output order). */
function tokenOutputs(built) {
  const out = new Map();
  const seen = new Set();
  built.plans.forEach((p, i) => {
    if (p.kind !== 'tokenLeader' && p.kind !== 'kronToken') return;
    const cov = built.tx.inputs[i].utxo.covenantId;
    if (!cov || seen.has(cov)) return;
    seen.add(cov);
    built.tx.outputs.map((o, j) => [o, j]).filter(([o]) => o.covenant?.covenantId === cov).forEach(([, j], k) => {
      if (p.nextStates[k]) out.set(j, { token: cov, state: p.nextStates[k] });
    });
  });
  return out;
}

/** The pair counterparty of the pair order at input `self` (processor.rs `pair_counterparty`): route, netting or inventory. */
function counterpartyOf(chain, built, self, pt) {
  let netting = false;
  for (const [i, p] of built.plans.entries()) {
    if (i === self || p.kind !== 'entry') continue;
    const n = fillArg(p.args[0]);
    if (n === null || n <= 0n) continue;
    let st;
    try {
      st = JSON.parse(raw(chain).decodeState(p.template, p.state));
    } catch {
      continue;
    }
    const q = pairTokensOf(st);
    if (!q) {
      if (st.state.tokenCovId === pt.a.covId || st.state.tokenCovId === pt.b.covId) return 'route';
    } else if (q.a.covId === pt.a.covId && q.b.covId === pt.b.covId && q.side !== pt.side) netting = true;
  }
  return netting ? 'netting' : 'inventory';
}

/** `detail.evidence` of a pair spend that used trigger evidence (processor.rs `pair_evidence_detail`). */
function evidenceOf(chain, built, args, idx) {
  const [ia, ib, im] = idx;
  const mode = intArg(args, im);
  const ka = Number(intArg(args, ia));
  const stateAt = (k) => {
    const p = built.plans[k];
    return p && p.kind === 'entry' ? JSON.parse(raw(chain).decodeState(p.template, p.state)) : null;
  };
  const cov = (k) => built.tx.inputs[k]?.utxo.covenantId ?? null;
  if (mode === 0n) {
    const kb = Number(intArg(args, ib));
    return { mode: 0, inputs: [ka, kb], orders: [cov(ka), cov(kb)], a: stateAt(ka)?.state.price ?? null, b: stateAt(kb)?.state.price ?? null };
  }
  return { mode: Number(mode), inputs: [ka], orders: [cov(ka)], price: stateAt(ka)?.state.price ?? null };
}

/**
 * What the batch does to every tracked order it spends: the continuation state (the candidate whose script is the output bound to the order's
 * covenant id), the fill size, the pair fields of a pair fill, the evidence of an arm or trail; and the exits an if-done pair entry's fill creates.
 */
function derive(chain, built, tx, lock) {
  const orders = new Map();
  const exits = [];
  const tokens = tokenOutputs(built);
  const spkOf = (st) => raw(chain).scriptPublicKey(J(st));
  built.plans.forEach((p, i) => {
    if (p.kind !== 'entry') return;
    const cov = tx.inputs[i].utxo.covenantId;
    const o = cov ? chain.orders.get(cov) : null;
    if (!o) return;
    const any = o.state;
    const s = any.state;
    const args = p.args;
    const nb = fillArg(args[0]);
    const pt = pairTokensOf(any);
    const kind = baseKind(any.kind);
    const utxoDaa = BigInt(o.currentDaa ?? 0);
    const armedSet = [s.armed, '0', '1', lock, utxoDaa];
    let fields = {};
    let act = 'fill';
    let n = nb ?? 0n;
    let evidence = null;
    let trail = null;
    let amountB = null;
    let price = null;
    if (kind === 'KobPair') {
      fields = { amountLeft: [b(s.amountLeft) - n], custody: [b(s.custody) - b(intArg(args, 4))] };
      amountB = pt.side === 1 ? intArg(args, 5) : intArg(args, 4);
      price = quoteAt(chain, o, lock);
    } else if (kind === 'KobCondPair') {
      const upd = intArg(args, 11) === 1n;
      const leg = Number(intArg(args, 6) ?? 0n);
      if (upd) {
        const k = intArg(args, 12) ?? 0n;
        act = k > 0n ? 'trail' : 'arm';
        const step = b(s.trailStep);
        fields = { armed: armedSet, stopPrice: [s.stopPrice, b(s.stopPrice) + k * step, b(s.stopPrice) - k * step] };
        evidence = evidenceOf(chain, built, args, [7, 8, 10]);
        if (k > 0n) trail = { steps: k.toString() };
      } else if (nb !== null && nb > 0n) {
        fields = { amountLeft: [b(s.amountLeft) - n], custody: [b(s.custody) - b(intArg(args, 4))], armed: armedSet };
        const trigger = b(s.armed) === 0n && leg === 1;
        if (trigger) evidence = evidenceOf(chain, built, args, [7, 8, 10]);
        amountB = pt.side === 1 ? intArg(args, 5) : intArg(args, 4);
        price = quoteAt(chain, o, lock, { leg, trigger });
      } else return;
    } else if (kind === 'KobIfdPair') {
      const upd = intArg(args, 17) === 1n;
      if (upd) {
        act = 'arm';
        fields = { armed: armedSet };
        evidence = evidenceOf(chain, built, args, [9, 10, 12]);
      } else if (nb !== null && nb < 0n) {
        // a merge: m base units re-armed by the booked exit's take-profit
        act = 'rearm';
        n = -nb & ((1n << 53n) - 1n);
        const a = JSON.parse(raw(chain).ifdPairAmounts(J(any), String(n), s.price));
        const grown = pt.side === 2 ? [b(s.custody) + b(a.mergeBudget)] : [b(s.custody) + b(a.pre)];
        fields = { amountLeft: [b(s.amountLeft) + n], custody: [...grown, s.custody], rptAmount: [s.rptAmount, b(s.rptAmount) - n], armed: armedSet };
      } else if (nb !== null && nb > 0n) {
        const amt = b(intArg(args, 6));
        const trigger = b(s.entryStop) > 0n && b(s.armed) === 0n;
        price = quoteAt(chain, o, lock, { trigger });
        const a = JSON.parse(raw(chain).ifdPairAmounts(J(any), String(n), String(price ?? s.price)));
        const custody = pt.side === 2 ? [b(s.custody) - amt] : [b(s.custody) - b(a.pre), 0n];
        const rpt = b(s.rptAmount);
        fields = { amountLeft: [b(s.amountLeft) - n], custody, rptAmount: [rpt > n ? rpt - n : rpt, rpt], armed: armedSet };
        if (trigger) evidence = evidenceOf(chain, built, args, [9, 10, 12]);
        amountB = amt;
        // the exit this fill creates: a fresh KobCondPair holding the leg's positional custody
        const c = built.covenants.find((x) => x.template === 'KobCondPair' && x.authorizingInput === i);
        if (c) {
          const out = c.outputs[0];
          const held = [...tokens].find(([, t]) => t.state.owner === c.covenantId && (t.state.owner_scheme === 4 || t.state.id_type === 2));
          const xc = held ? held[1].state.amount : String(pt.side === 2 ? n : amt);
          const untils = [lock, utxoDaa].map((x) => {
            const t0 = x > utxoDaa ? x : utxoDaa;
            const u = t0 + MAX_IDLE;
            return b(s.expiryDaa) < u ? b(s.expiryDaa) : u;
          });
          const cands = [null, ...untils.map((u) => ({ parent: cov, until: u }))];
          for (const bk of cands) {
            let x;
            try {
              x = JSON.parse(raw(chain).ifdPairExitFor(J(any), String(n), xc, bk ? bk.parent : '', bk ? String(bk.until) : '0'));
            } catch {
              continue;
            }
            if (spkOf(x) === tx.outputs[out].scriptPublicKey) {
              exits.push({ cov: c.covenantId, any: x, output: out, parent: cov });
              break;
            }
          }
        }
      } else return;
    } else if (kind === 'KobAsk') {
      if (nb === null || nb <= 0n) return;
      fields = { amountLeft: [b(s.amountLeft) - n] };
      price = b(s.price);
    } else if (kind === 'KobBid') {
      if (nb === null || nb <= 0n) return;
      fields = {};
      price = b(s.price);
    } else return;
    const cont = tx.outputs.find((x) => x.covenant?.covenantId === cov);
    let next = null;
    if (cont) next = variants(any, fields).find((c) => spkOf(c) === cont.scriptPublicKey) ?? null;
    const info = { kind: act, entry: p.entry, n, next, evidence, trail, price };
    if (isPairKind(any.kind) && act === 'fill') {
      let tipKas = null;
      try {
        tipKas = opt(raw(chain).pairTipKas(J(any), String(n)));
      } catch {
        tipKas = null;
      }
      info.pair = {
        side: pt.side === 1 ? 'ask' : 'bid', base: pt.a.covId, quote: pt.b.covId, a_scale: Number(pt.a.scale), amount_a: n.toString(),
        amount_b: amountB === null ? null : amountB.toString(), price: price === null ? null : price.toString(),
        tip_kas: tipKas === null ? null : tipKas.toString(), counterparty: counterpartyOf(chain, built, i, pt), price_source: 'none',
      };
    }
    orders.set(cov, info);
  });
  for (const x of exits) {
    const e = orders.get(x.parent);
    if (e) e.exit = x.cov;
  }
  return { orders, exits };
}

// ------------------------------------------------------------------------------------------------ control API

/** The order behind `cov`, live, with its current UTXO. */
function live(chain, cov) {
  const { o } = chain.fillContext(cov);
  if (!o.stateKnown) throw badRequest('the order state is not known to the mock');
  return o;
}

/** The default fill size: one whole A (or the minimum fill if larger), at most everything left. */
function defaultAmount(o) {
  const s = o.state.state;
  const pt = pairTokensOf(o.state);
  const left = b(s.amountLeft);
  const one = pt.a.scale > b(s.minFill) ? pt.a.scale : b(s.minFill);
  return one < left ? one : left;
}

function batchRequest(chain, legs, supply, updates = []) {
  const takerTokens = [];
  const need = new Map();
  for (const x of supply) if (x && x.amount > 0n) need.set(x.token, (need.get(x.token) ?? 0n) + x.amount);
  for (const [token, amount] of need) takerTokens.push(mintTokens(chain, token, amount));
  return {
    action: 'batch', lockTime: String(chain.daa()), legs, updates, takerTokens, taker: FILLER, takerTokenCarrier: '1000000000', receivers: [], payments: [],
    funding: [mintKas(chain, 5_000n * KAS)], change: FILLER, records: [], fee: { feeRate: null, feeMode: 'relay' },
  };
}

/** The result of a simulated pair action, shaped like `simulateFill`'s. */
function result(chain, cov, r) {
  const o = chain.orders.get(cov);
  const info = r.pending.orders.get(cov);
  const ev = [...chain.events].reverse().find((e) => e.covenant_id === cov && e.txid === r.transactionId);
  return {
    transactionId: r.transactionId, covenant_id: cov, status: o.status, filled_amount: o.filledAmount.toString(), amount_left: o.amountLeft === null ? null : o.amountLeft.toString(),
    state_known: o.stateKnown, children: r.pending.exits.map((x) => x.cov), event_id: ev?.id ?? null, kind: info?.kind ?? null, counterparty: info?.pair?.counterparty ?? null,
    evidence: info?.evidence ?? null,
  };
}

/**
 * Fills a pair order by a real batch. Options: `amount` (base units of A; default one whole A or the minimum fill, at most all left), `via`
 * (`inventory` default, `netting`, `route`), `against` (netting: the covenant id of an opposite pair order of the same pair; default a seeded one),
 * `leg` (a conditional's leg: 0 take-profit / limit, 1 stop; default the take-profit when it has one), `mode` (evidence mode 0 / 1 when the fill arms
 * an unarmed stop leg or stop entry; default 1). A booked exit's take-profit fill merges its entry when the entry is live (a repeat re-arm).
 */
export function simulatePairFill(chain, cov, opts = {}) {
  const o = live(chain, cov);
  const s = o.state.state;
  const pt = pairTokensOf(o.state);
  const n = opts.amount !== undefined && opts.amount !== null ? b(opts.amount) : defaultAmount(o);
  if (n <= 0n) throw badRequest('amount must be positive');
  if (n > b(s.amountLeft)) throw badRequest(`only ${s.amountLeft} base units left`);
  const lock = BigInt(chain.daa());
  const extra = {};
  let trigger = false;
  if (o.kind === 'KobCondPair') {
    extra.leg = opts.leg !== undefined ? Number(opts.leg) : b(s.tpPrice) > 0n ? 0 : 1;
    trigger = extra.leg === 1 && b(s.armed) === 0n;
    // a booked exit's take-profit re-arms its entry in the same transaction (merge), when the entry is live
    const parent = s.parent && s.parent !== ZERO32 ? chain.orders.get(s.parent) : null;
    if (extra.leg === 0 && parent && (parent.status === 'open' || parent.status === 'partial') && parent.current && opts.merge !== false) {
      const ppt = pairTokensOf(parent.state);
      extra.merge = { entry: orderUtxo(chain, parent), aCustody: custodyOf(chain, parent, ppt.a.covId), bCustody: custodyOf(chain, parent, ppt.b.covId) };
    }
  } else if (o.kind === 'KobIfdPair') trigger = b(s.entryStop) > 0n && b(s.armed) === 0n;
  const p = quoteAt(chain, o, lock, { leg: extra.leg ?? 0, trigger });
  if (p === null) throw badRequest('the order has no quote now (a stop leg that is not armed, or its arithmetic fails)');
  const legs = [null];
  const supply = [];
  if (trigger) {
    const mode = opts.mode !== undefined ? Number(opts.mode) : 1;
    const ev = seedEvidence(chain, o, mode, 'arm');
    ev.orders.forEach((x, k) => legs.push(legOf(chain, x, ev.amounts[k])));
    supply.push(...ev.supply);
    extra.evidence = 1;
    if (mode === 0) extra.evidenceB = 2;
  }
  legs[0] = legOf(chain, o, n, extra);
  const via = opts.via ?? 'inventory';
  if (via === 'inventory') supply.push(supplyOf(chain, o, n, p));
  else if (via === 'netting') {
    const other = opts.against ? live(chain, opts.against) : seedPairOrder(chain, { kind: 'KobPair', side: pt.side === 1 ? 'bid' : 'ask', base: pt.a.covId, quote: pt.b.covId, amount: n, minFill: n, price: p, maker: 'carol' });
    const opp = pairTokensOf(other.state);
    if (opp.a.covId !== pt.a.covId || opp.b.covId !== pt.b.covId || opp.side === pt.side) throw badRequest('netting needs an opposite order of the same pair');
    legs.push(legOf(chain, other, n));
    // the bid pays the floor, the ask receives the ceil: one base unit of B covers the rounding
    supply.push({ token: pt.b.covId, amount: 1n });
  } else if (via === 'route') {
    // through the KAS books: a sell of A meets a KAS bid of A and buys B from a KAS ask of B (a buy of A: the reverse); the filler's KAS bridges
    const need = supplyOf(chain, o, n, p);
    const kas = (token, side, amount, scale) => {
      const any = buildOrderState(chain, { token, side, price: String(KAS), amount, min_fill: 1, maker: 'carol', scale: scale.toString() });
      return chain.seedOrder(any, { value: side === 'bid' ? bidEscrow(chain.kob, any, amount, 1) : undefined });
    };
    if (pt.side === 1) {
      legs.push(legOf(chain, kas(pt.a.covId, 'bid', n, pt.a.scale), n));
      legs.push(legOf(chain, kas(pt.b.covId, 'ask', need.amount, pt.b.scale), need.amount));
    } else {
      const sOut = b(raw(chain).pairSOut(J(o.state), String(n), String(p)) ?? 0n);
      legs.push(legOf(chain, kas(pt.a.covId, 'ask', n, pt.a.scale), n));
      if (sOut > 0n) legs.push(legOf(chain, kas(pt.b.covId, 'bid', sOut, pt.b.scale), sOut));
    }
  } else throw badRequest('via must be inventory, netting or route');
  return result(chain, cov, run(chain, batchRequest(chain, legs, supply)));
}

/**
 * Arms (`trail: false`) or trails (`trail: true`) a pair stop without a fill: an update next to its trigger evidence (`mode` 0 two KAS-book fills,
 * 1 a resting pair order; default 1), seeded and filled in the same batch.
 */
export function simulatePairArm(chain, cov, opts = {}) {
  const o = live(chain, cov);
  const s = o.state.state;
  if (o.kind === 'KobPair') throw badRequest('a KobPair has no stop to arm');
  if (o.kind === 'KobIfdPair' && b(s.entryStop) <= 0n) throw badRequest('the entry has no stop to arm');
  const effect = opts.trail ? 'trail' : 'arm';
  if (effect === 'arm' && b(s.armed) !== 0n) throw badRequest('order is already armed');
  const mode = opts.mode !== undefined ? Number(opts.mode) : 1;
  const ev = seedEvidence(chain, o, mode, effect);
  const legs = ev.orders.map((x, k) => legOf(chain, x, ev.amounts[k]));
  // the evidence legs need a counterparty too: the filler's inventory (a resting KAS ask is bought with the filler's KAS)
  const supply = [...ev.supply];
  const update = { order: orderUtxo(chain, o, true), evidence: 0, ...(mode === 0 ? { evidenceB: 1 } : {}), take: null };
  const r = run(chain, batchRequest(chain, legs, supply, [update]));
  const out = result(chain, cov, r);
  return { ...out, armed: chain.orders.get(cov).state.state.armed, stop_price: chain.orders.get(cov).state.state.stopPrice ?? null };
}

export { pairOrderState };
