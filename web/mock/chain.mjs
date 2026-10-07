// The in-memory "chain + indexer" behind the mock server: a fake Kaspa node (UTXO set, DAA clock, submit with FULL script validation through
// kob-wasm) and the indexer state derived from what gets submitted (orders recovered from KOB1 placement records, custody / stray token
// UTXOs, lifecycle events). Simulated fills / arming (control API) perform REAL UTXO transitions with correct redeem scripts, so an order
// that was partially filled can still be cancelled by the app and validates in the script engine. Pair orders (KobPair, KobCondPair,
// KobIfdPair) are filled, armed and trailed by REAL transactions: a batch built by kob-wasm (inventory, netting or a route through the KAS books;
// trigger evidence in either mode), signed by the mock's filler key and accepted through `submit` like any other transaction (pair-fill.mjs).
import { addressOfSpk, addressPrefix, p2pkSpk, spkOfAddress } from './address.mjs';
import { KINDS, auctionOf, baseKind, bidBuyingPower, custodiesOf, custodyAmount, isPairKind, pairTokensOf, quoteOf, sideOf, termsOf } from './model.mjs';
import { simulatePairArm, simulatePairFill } from './pair-fill.mjs';
import { leaderNextStates, revealOf } from './script.mjs';
import { ApiError, HEX64, Rejection, badRequest, notFound, syntheticId } from './util.mjs';

const ZERO_FEE = { fee: '0', minFee: '0', feeRate: '0', mass: { size: 0, compute: 0, transient: 0, transientNormalized: 0, storage: 0, feeMass: 0 }, changeOutput: null };
const KOB1 = '4b4f4231';
export const DEFAULT_CARRIER = 1_000_000_000n; // 10 KAS per covenant UTXO
const outKey = (txid, index) => `${txid}:${index}`;
/** Tokens whose book an order touches: its own, and a pair order's quote token B (`book:<A>` and `book:<B>` fire). */
const bookTokens = (o) => [o.token, o.quote ?? null].filter(Boolean);
/** The order's state as base units / sompi (bigint) of a numeric spec value (number, decimal string or bigint). */
const big = (v, what) => {
  try {
    return BigInt(v);
  } catch {
    throw badRequest(`${what} must be an integer`);
  }
};

// The exit order an if-done entry commits to is stored as a prefix (kind code, first N state bytes); the covenant appends the repeat
// fields. Zero tails of the plain (unbooked) exit: `0x20 parent(32) 0x08 rptPrice(8) [0x08 rptPre(8)] 0x08 rptUntil(8)`.
const EXIT_TAIL = { KobCondAsk: '20' + '00'.repeat(32) + '08' + '00'.repeat(8) + '08' + '00'.repeat(8), KobCondBid: '20' + '00'.repeat(32) + '08' + '00'.repeat(8) + '08' + '00'.repeat(8) + '08' + '00'.repeat(8) };

export class MockChain {
  /**
   * @param {import('../src/kob/wasm').KobWasm} kob
   * @param {{network?: string, settleDepthDaa?: number, daaPerSecond?: number, startDaa?: number}} [opts]
   */
  constructor(kob, opts = {}) {
    this.kob = kob;
    this.network = opts.network ?? 'testnet-10';
    this.prefix = addressPrefix(this.network);
    this.settleDepthDaa = opts.settleDepthDaa ?? 100;
    this.daaPerSecond = opts.daaPerSecond ?? 10;
    this.startDaa = opts.startDaa ?? 150_000_000;
    this.templates = new Map(kob.templates().map((t) => [t.name, t]));
    this.tokenPrograms = kob.templates().filter((t) => t.tokenSlots).map((t) => t.name);
    this.transferTag = this.templates.get('KCC20Ref').entries.transfer;
    /** @type {Set<(ev: object) => void>} */
    this.listeners = new Set();
    this.reset();
  }

  reset() {
    this.t0 = Date.now();
    this.daaOffset = 0;
    this.blockSeq = 0;
    this.eventId = 0;
    this.tokenUtxoSeq = 0;
    this.synth = 0;
    /** @type {Map<string, object>} outpoint -> UTXO */
    this.utxos = new Map();
    /** @type {Map<string, Set<string>>} script public key -> outpoints */
    this.bySpk = new Map();
    this.tokens = new Map();
    this.orders = new Map();
    this.tokenUtxos = [];
    this.events = [];
    this.tokenEvents = [];
    this.rejects = [];
    /** fill event id -> the order's row before that fill (what `revertLastFill` puts back) */
    this.fillUndo = new Map();
    this.txs = new Set();
    this.submissions = [];
    this.failNext = { remaining: 0, message: 'mock: injected submit failure' };
    this.health = { state: 'following', lagDaa: 0 };
    /** pair fills (`pair_fills` rows: volume only, never a price) */
    this.pairFills = [];
    this.pairFillSeq = 0;
    /** txid -> what a batch the mock built itself does to the orders it spends (pair-fill.mjs): the states the indexer would splice */
    this.pending = new Map();
  }

  // ------------------------------------------------------------------------------------------------ clock

  daa() {
    return this.startDaa + this.daaOffset + Math.floor(((Date.now() - this.t0) * this.daaPerSecond) / 1000);
  }
  /** Wall clock in ms; advancing the DAA by `n` moves it by `n / 10` seconds so day orders stay consistent with the DAA. */
  nowMs() {
    return Date.now() + Math.floor(this.daaOffset * 100);
  }
  nowUnix() {
    return Math.floor(this.nowMs() / 1000);
  }
  advanceDaa(n) {
    if (!Number.isFinite(n) || n < 0) throw badRequest('daa must be a non-negative number');
    this.daaOffset += Math.floor(n);
    this.emit({ orders: [], tokens: [], fills: [], added: 1, reverted: 0 });
    return this.daa();
  }

  // ------------------------------------------------------------------------------------------------ listeners / events

  /** Emits one "committed batch" (drives the WebSocket frames). */
  emit(ev) {
    const full = { cursorHash: syntheticId('cursor', this.blockSeq), cursorDaa: this.daa(), ...ev };
    for (const l of this.listeners) l(full);
  }

  pushEvent(e) {
    const ev = {
      id: ++this.eventId,
      covenant_id: e.cov,
      block_seq: e.seq,
      daa: e.daa,
      ts: this.nowMs(),
      txid: e.txid,
      tx_pos: 0,
      kind: e.kind,
      token: e.token ?? null,
      side: e.side ?? null,
      // base units filled (decimal string), like the indexer's `amount`
      amount: e.amount == null ? null : String(e.amount),
      price: e.price ?? null,
      payout: e.payout ?? null,
      closes: !!e.closes,
      detail: e.detail ?? null,
    };
    this.events.push(ev);
    return ev;
  }

  // ------------------------------------------------------------------------------------------------ UTXO primitives

  addUtxo({ txid, index, amount, spk, covenantId = null, daa, kind = 'other' }) {
    const u = { txid, index, amount: BigInt(amount), spk, covenantId, daa, kind, spentBy: null, orderCov: null, tokenRec: null };
    const key = outKey(txid, index);
    this.utxos.set(key, u);
    let set = this.bySpk.get(spk);
    if (!set) this.bySpk.set(spk, (set = new Set()));
    set.add(key);
    return u;
  }

  spendUtxo(u, byTxid) {
    u.spentBy = byTxid;
    if (u.tokenRec) {
      u.tokenRec.spent = true;
      u.tokenRec.spent_txid = byTxid;
    }
  }

  pkSpk(pubkey) {
    if (!HEX64.test(pubkey ?? '')) throw badRequest('pubkey must be 64 hex characters (x-only)');
    return p2pkSpk(pubkey);
  }

  addToken(t) {
    if (!HEX64.test(t.covenant_id)) throw badRequest('token covenant_id must be 64 hex characters');
    const program = t.program ?? 'KCC20Ref_8x8';
    const tpl = this.templates.get(program);
    if (!tpl || !tpl.tokenSlots) throw badRequest(`unknown token program ${program}`);
    const rec = {
      ticker: t.ticker,
      name: t.name ?? t.ticker,
      covenant_id: t.covenant_id,
      template_hash: t.template_hash ?? tpl.hash,
      extension_commitment: t.extension_commitment ?? null,
      decimals: t.decimals ?? null,
      // the token's standard scale (`10^decimals`, at most 10^9): an order is listed only at this scale (`/v1/tokens` `scale`); a token without
      // decimals (open list, no registry entry) has none and its orders are listed whatever their scale
      scale: t.decimals == null ? null : 10 ** Math.min(Number(t.decimals), 9),
      // the scale seeded orders of a token WITHOUT decimals use (mock seeding only, not served)
      order_scale: t.order_scale ?? null,
      // price step of the seeded books, sompi per whole token (mock seeding only)
      tick: t.tick ?? null,
      program,
      // registry standing (`GET /v1/tokens`): official | unverified | delisted; powers of the program template; the audited template id; the ticker is '' for tokens outside the registry
      standing: t.standing ?? 'unverified',
      powers: t.powers ?? [],
      template_id: t.template_id ?? null,
      family: t.family ?? 'kcc20',
      // a token the indexer tracks only as foreign strays: not listed by `/v1/tokens`
      hidden: !!t.hidden,
    };
    this.tokens.set(rec.covenant_id, rec);
    this.tokenEvents.push({
      token: rec.covenant_id,
      template_hash: rec.template_hash,
      extension_commitment: rec.extension_commitment,
      kind: 'seen',
      txid: syntheticId('token', rec.covenant_id),
      daa: this.daa(),
    });
    return rec;
  }

  tokenRef(ref) {
    const t = this.tokens.get(ref) ?? [...this.tokens.values()].find((x) => x.ticker === ref);
    if (!t) throw badRequest(`unknown token ${ref}`);
    return t;
  }

  /** Creates a token UTXO (KCC-20 state) with its on-chain output and indexer record. */
  addTokenUtxo({ txid, index, token, state, value, role, daa, seq }) {
    const tok = typeof token === 'string' ? this.tokenRef(token) : token;
    // the output may already exist (submitted tx): then only its role changes
    let utxo = this.utxos.get(outKey(txid, index));
    if (utxo) utxo.kind = 'token';
    else {
      const spk = this.kob.tokenScriptPublicKey(tok.program, state);
      utxo = this.addUtxo({ txid, index, amount: value, spk, covenantId: tok.covenant_id, daa, kind: 'token' });
    }
    const rec = {
      seq: ++this.tokenUtxoSeq,
      txid,
      index,
      token: tok.covenant_id,
      owner: state.owner,
      amount: BigInt(state.amount),
      value: BigInt(value),
      role,
      created_daa: daa,
      created_seq: seq,
      spent: false,
      spent_txid: null,
      state,
    };
    utxo.tokenRec = rec;
    this.tokenUtxos.push(rec);
    return rec;
  }

  tokenState(tok, owner, scheme, amount) {
    // KRON (46-byte state): id_type 2 = covenant id (custody), 3 = address presence (a key's holdings)
    if (tok.family === 'kron') return { amount: String(amount), owner, id_type: scheme === 4 ? 2 : 3, is_minter: 0 };
    return {
      amount: String(amount),
      owner,
      owner_scheme: scheme,
      borrow_scheme: 0,
      borrow_guard: '0'.repeat(64),
      extension_commitment: tok.extension_commitment ?? '0'.repeat(64),
    };
  }

  liveTokenUtxos(owner, role) {
    return this.tokenUtxos.filter((t) => t.owner === owner && t.role === role && !t.spent);
  }

  // ------------------------------------------------------------------------------------------------ control: fund keys

  /** Gives a key KAS (one P2PK UTXO of `sompi`). */
  giveKas(pubkey, sompi) {
    const txid = syntheticId('kas', ++this.synth);
    const u = this.addUtxo({ txid, index: 0, amount: BigInt(sompi), spk: this.pkSpk(pubkey), daa: this.daa(), kind: 'p2pk' });
    this.blockSeq++;
    return { transactionId: txid, index: 0, amount: u.amount.toString() };
  }

  /** Gives a key one P2PK-owned token UTXO (`amount` base units, on a `carrier` KAS output). */
  giveTokens(pubkey, tokenRef, amount, carrier = DEFAULT_CARRIER) {
    const tok = this.tokenRef(tokenRef);
    const txid = syntheticId('tok', ++this.synth);
    const seq = ++this.blockSeq;
    const rec = this.addTokenUtxo({ txid, index: 0, token: tok, state: this.tokenState(tok, pubkey, 0, amount), value: carrier, role: 'owned', daa: this.daa(), seq });
    return { transactionId: txid, index: 0, token: tok.covenant_id, amount: rec.amount.toString(), value: rec.value.toString() };
  }

  /**
   * A token UTXO sent to an order's covenant id from outside the protocol (owner scheme 4 without a custody role): inert, never liquidity. `token`
   * (optional): another token than the order's own, a FOREIGN stray (matcher.md 1.2): a known token (ticker or covenant id), or a new covenant id
   * that is registered as an unlisted token (kept out of `/v1/tokens`) with `program` (default KCC20Ref_8x8) and `ticker`.
   */
  seedStray(cov, amount, carrier = DEFAULT_CARRIER, token = null) {
    const o = this.orders.get(cov);
    if (!o) throw notFound('unknown covenant id');
    let tok;
    if (token && typeof token === 'object') {
      tok = this.tokens.get(token.covenant_id) ?? this.addToken({ covenant_id: token.covenant_id, ticker: token.ticker ?? `X${token.covenant_id.slice(0, 4).toUpperCase()}`, program: token.program, decimals: token.decimals ?? null, extension_commitment: token.extension_commitment ?? null, hidden: true });
    } else tok = this.tokenRef(token ?? o.token);
    const txid = syntheticId('stray', ++this.synth);
    const seq = ++this.blockSeq;
    const rec = this.addTokenUtxo({ txid, index: 0, token: tok, state: this.tokenState(tok, cov, 4, amount), value: carrier, role: 'stray', daa: this.daa(), seq });
    o.lastSeq = seq;
    this.emit({ orders: [cov], tokens: [o.token], fills: [], added: 1, reverted: 0 });
    return { transactionId: txid, index: 0, token: tok.covenant_id, amount: rec.amount.toString(), value: rec.value.toString() };
  }

  kasBalance(pubkey) {
    let sum = 0n;
    for (const k of this.bySpk.get(this.pkSpk(pubkey)) ?? []) {
      const u = this.utxos.get(k);
      if (!u.spentBy) sum += u.amount;
    }
    return sum;
  }

  tokenBalance(pubkey, tokenRef) {
    const tok = this.tokenRef(tokenRef);
    return this.tokenUtxos.filter((t) => t.token === tok.covenant_id && t.owner === pubkey && t.role === 'owned' && !t.spent).reduce((a, t) => a + t.amount, 0n);
  }

  // ------------------------------------------------------------------------------------------------ node API

  nodeInfo() {
    return {
      network: this.network,
      virtualDaaScore: String(this.daa()),
      serverVersion: 'kob-mock-node/1.0',
      daaRateMilli: this.daaPerSecond * 1000,
      unixSeconds: String(this.nowUnix()),
    };
  }

  utxosByAddresses(addresses) {
    if (!Array.isArray(addresses)) throw badRequest('addresses must be an array');
    const out = [];
    for (const address of addresses) {
      let spk;
      try {
        spk = spkOfAddress(address);
      } catch (e) {
        throw badRequest(String(e.message));
      }
      for (const k of this.bySpk.get(spk) ?? []) {
        const u = this.utxos.get(k);
        if (u.spentBy) continue;
        out.push({ address, transactionId: u.txid, index: u.index, amount: u.amount.toString(), scriptPublicKey: u.spk, blockDaaScore: String(u.daa), isCoinbase: false, covenantId: u.covenantId });
      }
    }
    return out;
  }

  // ------------------------------------------------------------------------------------------------ submit

  /**
   * Consensus-style acceptance of a fully signed tx (kaspa safe JSON): every input known, unspent and matching its embedded entry, then
   * the whole tx through the script engine (`kob.validate`: scripts with enforced budgets, storage mass, fee floor, block limits).
   * On success spends / creates UTXOs and updates the indexer view. Throws `Rejection` with the reason otherwise.
   */
  submit(tx) {
    const fail = (m) => {
      throw new Rejection(m);
    };
    if (this.failNext.remaining > 0) {
      this.failNext.remaining--;
      fail(this.failNext.message);
    }
    if (!tx || typeof tx !== 'object' || !Array.isArray(tx.inputs) || !Array.isArray(tx.outputs)) fail('malformed transaction');
    if (!HEX64.test(tx.id ?? '')) fail('malformed transaction id');
    if (this.txs.has(tx.id)) fail(`transaction ${tx.id} is already accepted`);
    if (tx.inputs.length === 0) fail('transaction has no inputs');
    const seen = new Set();
    const spent = [];
    let inSum = 0n;
    tx.inputs.forEach((input, i) => {
      const key = outKey(input.transactionId, input.index);
      if (seen.has(key)) fail(`input ${i}: outpoint ${key} is used twice`);
      seen.add(key);
      const u = this.utxos.get(key);
      if (!u) fail(`input ${i}: outpoint ${key} is not in the UTXO set`);
      if (u.spentBy) fail(`input ${i}: outpoint ${key} is already spent by ${u.spentBy} (double spend)`);
      const e = input.utxo;
      if (!e) fail(`input ${i}: the transaction carries no UTXO entry`);
      if (BigInt(e.amount) !== u.amount || e.scriptPublicKey !== u.spk || (e.covenantId ?? null) !== u.covenantId) {
        fail(`input ${i}: the embedded UTXO entry does not match the node's UTXO ${key}`);
      }
      if (!input.signatureScript) fail(`input ${i}: empty signature script`);
      inSum += u.amount;
      spent.push({ u, index: i, sigscript: input.signatureScript });
    });
    let outSum = 0n;
    for (const o of tx.outputs) outSum += BigInt(o.value);
    if (outSum > inSum) fail(`outputs (${outSum}) exceed inputs (${inSum})`);
    try {
      this.kob.validate({ tx, fee: ZERO_FEE });
    } catch (e) {
      fail(String(e?.message ?? e).replace(/^kob-wasm validate: /, ''));
    }
    return this.applyTx(tx, spent, inSum - outSum);
  }

  applyTx(tx, spent, fee) {
    const txid = tx.id;
    const daa = this.daa();
    const seq = ++this.blockSeq;
    this.txs.add(txid);

    // 1. inputs
    const spentOrders = [];
    const spentCovs = new Set();
    for (const s of spent) {
      const cov = s.u.orderCov;
      this.spendUtxo(s.u, txid);
      if (cov) {
        spentOrders.push({ cov, order: this.orders.get(cov), input: s.index, reveal: revealOf(s.sigscript), spk: s.u.spk });
        spentCovs.add(cov);
      }
    }

    // 2. outputs
    const outs = tx.outputs.map((o, j) => this.addUtxo({ txid, index: j, amount: BigInt(o.value), spk: o.scriptPublicKey, covenantId: o.covenant?.covenantId ?? null, daa, kind: o.covenant ? 'covenant' : 'p2pk' }));

    // 3. token outputs: states come from the leader input's signature script (this is how the real indexer learns them)
    const tokenMatches = new Map(); // output index -> Kcc20State
    const leaders = new Map();
    const used = new Set();
    tx.outputs.forEach((o, j) => {
      const tok = o.covenant ? this.tokens.get(o.covenant.covenantId) : null;
      if (!tok) return;
      const auth = o.covenant.authorizingInput;
      if (!leaders.has(auth)) leaders.set(auth, leaderNextStates(tx.inputs[auth]?.signatureScript ?? '', this.transferTag));
      const states = leaders.get(auth);
      if (!states) return;
      states.forEach((st, k) => {
        if (tokenMatches.has(j) || used.has(`${auth}:${k}`)) return;
        const programs = [tok.program, ...this.tokenPrograms.filter((p) => p !== tok.program)];
        for (const p of programs) {
          let spk;
          try {
            spk = this.kob.tokenScriptPublicKey(p, st);
          } catch {
            continue;
          }
          if (spk === o.scriptPublicKey) {
            used.add(`${auth}:${k}`);
            tokenMatches.set(j, st);
            break;
          }
        }
      });
    });

    // 4. placement records -> new orders
    const created = new Set();
    const createdInfo = [];
    if (typeof tx.payload === 'string' && tx.payload.startsWith(KOB1)) {
      let recovered = [];
      try {
        recovered = this.kob.recoverOrders(tx);
      } catch (e) {
        this.rejects.push({ txid, reason: String(e?.message ?? e) });
      }
      for (const r of recovered) {
        if (this.orders.has(r.covenantId)) {
          this.rejects.push({ txid, reason: 'duplicate_covenant' });
          continue;
        }
        const u = outs[r.output];
        u.orderCov = r.covenantId;
        u.kind = 'order';
        if (r.custody && !tokenMatches.has(r.custody.output)) tokenMatches.set(r.custody.output, r.custody.state);
        if (r.prefund && !tokenMatches.has(r.prefund.output)) tokenMatches.set(r.prefund.output, r.prefund.state);
        this.registerOrder({ covenantId: r.covenantId, any: r.order, txid, out: r.output, value: BigInt(r.value), daa, seq, custodyState: r.custody?.state ?? null, deadline: r.deadline != null ? Number(r.deadline) : null, origin: 'chain' });
        created.add(r.covenantId);
        createdInfo.push({ covenantId: r.covenantId, kind: r.order.kind, output: r.output });
      }
    }

    // 4b. the exits a known batch creates (an if-done pair entry's fill): fresh covenants without a placement record
    const pend = this.pending.get(txid) ?? null;
    for (const x of pend?.exits ?? []) {
      const u = outs[x.output];
      if (!u || this.orders.has(x.cov)) continue;
      u.orderCov = x.cov;
      u.kind = 'order';
      this.registerOrder({ covenantId: x.cov, any: x.any, txid, out: x.output, value: u.amount, daa, seq, custodyState: null, origin: 'chain', parent: x.parent });
      this.orders.get(x.parent)?.children.push(x.cov);
      created.add(x.cov);
      createdInfo.push({ covenantId: x.cov, kind: x.any.kind, output: x.output });
    }

    // 5. register token UTXOs (custody / stray / owned) now that the orders exist
    const tokenOutputs = [];
    for (const [j, st] of tokenMatches) {
      const tok = this.tokens.get(tx.outputs[j].covenant.covenantId);
      let role = 'owned';
      if (st.owner_scheme === 4) {
        if (!this.orders.has(st.owner)) role = null;
        else role = created.has(st.owner) || spentCovs.has(st.owner) ? 'custody' : 'stray';
      }
      if (role === null) continue; // owned by an order the mock does not know: not indexed (like the real indexer)
      const rec = this.addTokenUtxo({ txid, index: j, token: tok, state: st, value: BigInt(tx.outputs[j].value), role, daa, seq });
      tokenOutputs.push({ output: j, owner: st.owner, ownerScheme: st.owner_scheme, amount: st.amount, role: rec.role });
      if (role === 'stray') {
        this.orders.get(st.owner).lastSeq = seq;
      }
    }

    // 6. spends of tracked orders
    const closed = [];
    const touched = new Set(created);
    const fills = [];
    // in-place amends (AMEND records, plain asks and bids): the maker's cancel continues the order's covenant id with the recorded state, verified by
    // kob-wasm against the state the input reveals (as the real indexer does); anything else continuing a cancel stays unproven
    let amends = [];
    if (typeof tx.payload === 'string' && tx.payload.startsWith(KOB1)) {
      try {
        amends = this.kob.recoverAmends(tx);
      } catch (e) {
        this.rejects.push({ txid, reason: `amend:${String(e?.message ?? e)}` });
      }
    }
    // sweeps in place (SWEEP records, payload.rs `verify_sweep`): the maker's cancel continues the order under the SAME script; only strays move
    const sweepRecords = this.decodeRecords(tx.payload).filter((r) => r.type === 'sweep');
    for (const so of spentOrders) {
      const o = so.order;
      const entry = so.reveal ? this.entryName(o.state.kind, so.reveal.tag) : null;
      const cont = outs.find((u) => u.covenantId === so.cov);
      const sw = entry === 'cancel' && cont ? this.verifySweep(tx, sweepRecords, so, cont) : null;
      if (sw) {
        cont.orderCov = so.cov;
        cont.kind = 'order';
        o.current = { txid, index: cont.index, value: cont.amount };
        o.currentDaa = daa;
        o.lastSeq = seq;
        o.lastDaa = daa;
        this.pushEvent({ cov: so.cov, kind: 'sweep', seq, daa, txid, token: o.token, side: o.side, closes: false, detail: { input: so.input, output: cont.index, entry } });
        touched.add(so.cov);
        continue;
      }
      if (entry === 'cancel' && cont && sweepRecords.length) this.rejects.push({ txid, reason: 'sweep:record does not continue the same script' });
      // a spend of a batch the mock built itself: the new state (and the fill, arm or trail) is known, as the real indexer splices it
      const known = pend?.orders.get(so.cov);
      if (known) {
        this.applyKnownSpend(o, so, known, { txid, daa, seq, cont, closed, fills });
        touched.add(so.cov);
        continue;
      }
      const am = entry === 'cancel' && cont ? amends.find((a) => a.covenantId === so.cov && a.output === cont.index) : undefined;
      if (am) {
        const before = termsOf(o.state);
        cont.orderCov = so.cov;
        cont.kind = 'order';
        o.state = am.order;
        o.stateKnown = true;
        o.tif = termsOf(am.order).tif;
        o.current = { txid, index: cont.index, value: cont.amount };
        o.currentDaa = daa;
        if (am.deadline != null) o.deadline = Number(am.deadline);
        if (baseKind(am.order.kind) === 'KobBid') o.amountLeft = bidBuyingPower(this.kob, am.order, BigInt(cont.amount));
        o.lastSeq = seq;
        o.lastDaa = daa;
        const previous = { price: String(before.price), tip: String(before.tip ?? 0n), tif: String(before.tif), expiryDaa: String(before.expiryDaa), activeFrom: String(before.activeFrom ?? 0n) };
        this.pushEvent({ cov: so.cov, kind: 'amend', seq, daa, txid, token: o.token, side: o.side, closes: false, detail: { input: so.input, entry, previous } });
        touched.add(so.cov);
        continue;
      }
      const ioc = o.tif === 1n || o.tif === 2n;
      let kind = entry ?? 'unknown';
      let status;
      // `settle(0)` is the refund of a token-holding order (docs/spec/order-types.md): its first argument, the fill amount `n`, is an 8-byte little-endian zero (build.rs `nb(0)`)
      const refundsAsSettle = entry === 'settle' && so.reveal.pushes.length >= 3 && so.reveal.pushes[0].length === 8 && so.reveal.pushes[0].every((b) => b === 0);
      if (entry === 'cancel') status = 'cancelled';
      else if (entry === 'refund' || refundsAsSettle) {
        if (refundsAsSettle) kind = 'refund';
        status = 'refunded';
        if (ioc) kind = 'kill';
      } else if (entry === 'fill' || entry === 'settle') {
        kind = 'fill';
        status = 'filled';
      } else status = 'closed';
      const closes = !cont;
      const token = o.token;
      const ev = this.pushEvent({ cov: so.cov, kind, seq, daa, txid, token, side: o.side, closes, detail: { input: so.input, entry, verified: false } });
      if (cont) {
        cont.orderCov = so.cov;
        cont.kind = 'order';
        o.current = { txid, index: cont.index, value: cont.amount };
        o.currentDaa = daa;
        o.stateKnown = false; // a continuation whose new state the mock cannot derive from the chain alone
      } else {
        o.status = status;
        o.current = null;
        closed.push({ covenantId: so.cov, entry: refundsAsSettle ? 'refund' : entry, status });
      }
      o.lastSeq = seq;
      o.lastDaa = daa;
      touched.add(so.cov);
      if (kind === 'fill') fills.push(this.fillNotice(o, ev, txid, 0, null));
    }

    const submission = {
      txid,
      daa,
      block_seq: seq,
      at: new Date(this.nowMs()).toISOString(),
      fee: fee.toString(),
      inputs: tx.inputs.length,
      outputs: tx.outputs.length,
      created: createdInfo,
      closed,
      token_outputs: tokenOutputs,
      spent: spent.map((s) => ({ outpoint: outKey(s.u.txid, s.u.index), kind: s.u.kind, covenantId: s.u.covenantId })),
      records: this.decodeRecords(tx.payload),
      tx,
    };
    this.submissions.push(submission);
    const tokens = new Set([...touched].flatMap((c) => (this.orders.get(c) ? bookTokens(this.orders.get(c)) : [])));
    this.emit({ orders: [...touched], tokens: [...tokens], fills, added: 1, reverted: 0 });
    return { transactionId: txid };
  }

  decodeRecords(payload) {
    if (!payload) return [];
    try {
      return this.kob.decodePayload(payload)?.records ?? [];
    } catch {
      return [];
    }
  }

  /**
   * The SWEEP record continuing order input `so` at `cont`, verified as the real indexer does (payload.rs `verify_sweep`): the record names this
   * output and input, the output carries the SAME script as the spent order UTXO, it is bound to the order's covenant id from that input, and no
   * other input or output carries the id. Null when there is none or it does not verify (the continuation then stays unproven).
   */
  verifySweep(tx, records, so, cont) {
    const rec = records.find((r) => r.output === cont.index && r.input === so.input);
    if (!rec) return null;
    const out = tx.outputs[cont.index];
    if (out.scriptPublicKey !== so.spk) return null;
    if (out.covenant?.authorizingInput !== so.input || out.covenant?.covenantId !== so.cov) return null;
    if (tx.outputs.filter((x) => x.covenant?.covenantId === so.cov).length !== 1) return null;
    if (tx.inputs.filter((x) => (x.utxo?.covenantId ?? null) === so.cov).length !== 1) return null;
    return rec;
  }

  entryName(kind, tagHex) {
    const entries = this.templates.get(kind)?.entries ?? {};
    return Object.keys(entries).find((n) => entries[n] === tagHex) ?? null;
  }

  /**
   * A spend of a batch the mock built itself (pair-fill.mjs): the order's new state is known (as the real indexer splices it from the reveal), so
   * the order row, its event (`fill` with the pair fields and NO price for a pair order; `arm` / `trail` with the evidence; `rearm` on a merged
   * entry) and the `pair_fills` row follow from it.
   */
  applyKnownSpend(o, so, k, { txid, daa, seq, cont, closed, fills }) {
    const cov = so.cov;
    const pair = isPairKind(o.kind);
    if (cont && k.next) {
      cont.orderCov = cov;
      cont.kind = 'order';
      o.state = k.next;
      o.stateKnown = true;
      o.current = { txid, index: cont.index, value: cont.amount };
      o.currentDaa = daa;
      if (baseKind(o.kind) === 'KobBid') o.amountLeft = bidBuyingPower(this.kob, k.next, cont.amount);
      else if (k.next.state.amountLeft !== undefined) o.amountLeft = BigInt(k.next.state.amountLeft);
    } else if (cont) {
      cont.orderCov = cov;
      cont.kind = 'order';
      o.current = { txid, index: cont.index, value: cont.amount };
      o.currentDaa = daa;
      o.stateKnown = false;
    } else {
      o.status = k.kind === 'fill' ? 'filled' : 'closed';
      // closed: nothing is left to trade (an IOC's unfilled rest went back to its maker)
      if (o.amountLeft !== null) o.amountLeft = 0n;
      o.current = null;
      closed.push({ covenantId: cov, entry: k.entry, status: o.status });
    }
    o.lastSeq = seq;
    o.lastDaa = daa;
    if (k.kind === 'fill') {
      o.filledAmount += k.n;
      if (cont) o.status = 'partial';
      const detail = { input: so.input, entry: k.entry, simulated: true, ...(k.pair ? { pair: k.pair } : {}), ...(k.evidence ? { evidence: k.evidence } : {}), ...(k.exit ? { exit: k.exit } : {}) };
      // a pair fill is never a price (founder rule): its event carries `price` null and `detail.pair` (volume, counterparty, price_source none)
      const ev = this.pushEvent({ cov, kind: 'fill', seq, daa, txid, token: o.token, side: o.side, amount: k.n, price: pair ? null : (k.price === null ? null : k.price.toString()), closes: !cont, detail });
      fills.push(this.fillNotice(o, ev, txid, k.n, null));
      if (pair && k.pair) {
        this.pairFills.push({
          id: ++this.pairFillSeq, txid, ts: ev.ts, daa, order: cov, contract: o.kind, side: k.pair.side, base: k.pair.base, quote: k.pair.quote, amount_a: k.n,
          amount_b: k.pair.amount_b === null ? null : BigInt(k.pair.amount_b), price: k.pair.price === null ? null : BigInt(k.pair.price), a_scale: BigInt(k.pair.a_scale),
          tip_kas: k.pair.tip_kas === null ? null : BigInt(k.pair.tip_kas), counterparty: k.pair.counterparty,
        });
      }
    } else {
      this.pushEvent({ cov, kind: k.kind, seq, daa, txid, token: o.token, side: o.side, amount: k.kind === 'rearm' ? k.n : null, closes: !cont, detail: { input: so.input, entry: k.entry, simulated: true, ...(k.evidence ? { evidence: k.evidence } : {}), ...(k.trail ? { trail: k.trail } : {}) } });
    }
  }

  /** The WebSocket `fill` notice (`FillNotice` of the indexer): integers are JSON numbers there (`amount` base units, `price`, `payout`). */
  fillNotice(o, ev, txid, amount, payout) {
    const num = (v) => (v === null || v === undefined ? null : Number(v));
    return { order: o.covenantId, token: o.token, side: o.side, price: num(ev.price), amount: Number(amount || ev.amount || 0), payout: num(payout), txid, block: syntheticId('block', this.blockSeq), daa: ev.daa };
  }

  // ------------------------------------------------------------------------------------------------ orders

  /**
   * Registers an order the way the indexer does after recovering a placement record: the order row, its `create` event and the
   * token registry sighting. The order UTXO / custody UTXO themselves are created by the caller.
   */
  registerOrder({ covenantId, any, txid, out, value, daa, seq, custodyState, deadline = null, origin = 'chain', parent = null }) {
    const info = KINDS[any.kind];
    const s = any.state;
    const t = termsOf(any);
    // a pair order: its token is A, its quote B (both must pass the listing rules; B's refusal is `quote_<reason>`)
    const pt = pairTokensOf(any);
    const tokenId = pt ? pt.a.covId : s.tokenCovId;
    const tplHash = pt ? pt.a.tplHash : s.tokenTplHash;
    const tok = this.tokens.get(tokenId) ?? null;
    const listing = (tk, hash, scale) =>
      !tk ? 'token_not_allowlisted'
        : tk.template_hash && hash && tk.template_hash !== hash ? 'template_mismatch'
          : scale <= 0n ? 'bad_state:scale:not positive'
            // every listed order of a token quotes the same whole token (kob-executor tokens.rs ListingRules)
            : tk.scale != null && scale !== BigInt(tk.scale) ? 'non_standard_scale' : null;
    let reason = listing(tok, tplHash, t.scale);
    if (!reason && pt) {
      const q = listing(this.tokens.get(pt.b.covId) ?? null, pt.b.tplHash, pt.b.scale);
      if (q) reason = `quote_${q}`;
    }
    const listed = reason === null;
    const ext = s.extensionCommitment ?? custodyState?.extension_commitment ?? (pt ? (pt.side === 1 ? null : pt.b.ext) : null) ?? tok?.extension_commitment ?? null;
    const o = {
      covenantId,
      kind: any.kind,
      state: any,
      stateKnown: true,
      side: sideOf(any),
      inBook: info.inBook,
      token: tokenId,
      quote: pt ? pt.b.covId : null,
      ext,
      tif: t.tif,
      genesis: { txid, out, seq, daa },
      status: 'open',
      // base units (bigint): filled so far, left (`amountLeft`; a bid's buying power, an upper bound), at creation (null for a bid)
      filledAmount: 0n,
      amountLeft: t.amountLeft,
      amountExact: t.amountLeft !== null,
      initialAmount: t.amountLeft,
      current: { txid, index: out, value: BigInt(value) },
      currentDaa: daa,
      deadline,
      lastSeq: seq,
      lastDaa: daa,
      listed,
      unlistedReason: reason,
      // the executor's pre-simulation of the next fill failed (a frozen / blacklisted token state): omitted from the book, flagged on the order
      possiblyFrozen: false,
      origin,
      parent,
      children: [],
    };
    if (baseKind(any.kind) === 'KobBid') {
      o.amountLeft = bidBuyingPower(this.kob, any, BigInt(value));
      o.amountExact = false;
    }
    this.orders.set(covenantId, o);
    this.pushEvent({
      cov: covenantId,
      kind: 'create',
      seq,
      daa,
      txid,
      token: o.token,
      side: o.side,
      amount: o.initialAmount,
      price: info.inBook ? t.price.toString() : null,
      detail: { contract: any.kind, parent, origin, deadline, booked: parent !== null, ...(pt ? { pair: { base: pt.a.covId, quote: pt.b.covId, side: pt.side === 1 ? 'ask' : 'bid' } } : {}) },
    });
    if (tok && !this.tokenEvents.some((e) => e.token === o.token && e.template_hash === tplHash && e.extension_commitment === ext)) {
      this.tokenEvents.push({ token: o.token, template_hash: tplHash ?? null, extension_commitment: ext, kind: 'seen', txid, daa });
    }
    return o;
  }

  /**
   * Seeds a complete, real order: order UTXO with the correct P2SH script for its state, plus the custody token UTXO of ask kinds.
   * `any` is a full `AnyState`; the maker can cancel it with the matching secret key.
   */
  seedOrder(any, { value, custodyCarrier = DEFAULT_CARRIER, covenantId, deadline = null, parent = null, daa: at } = {}) {
    const info = KINDS[any.kind];
    if (!info) throw badRequest(`not an order kind: ${any.kind}`);
    const n = ++this.synth;
    const txid = syntheticId('order-tx', n);
    const cov = covenantId ?? syntheticId('order-cov', n);
    // `daa`: an order placed earlier (it has rested: trigger evidence must have rested minRestDaa)
    const daa = at ?? this.daa();
    const seq = ++this.blockSeq;
    const val = BigInt(value ?? DEFAULT_CARRIER);
    const u = this.addUtxo({ txid, index: 0, amount: val, spk: this.kob.scriptPublicKey(any), covenantId: cov, daa, kind: 'order' });
    u.orderCov = cov;
    // the custodies: an ask's token; a pair order's custodies of A and / or B (kob-wasm `custodies`, record order)
    const parts = [];
    if (isPairKind(any.kind)) {
      const pt = pairTokensOf(any);
      for (const c of custodiesOf(this.kob, any)) {
        if (c.amount <= 0n) continue;
        const tok = this.tokens.get(c.token);
        if (!tok) throw badRequest('pair orders need known tokens');
        const ext = c.token === pt.a.covId ? (pt.a.family === 'kron' ? null : (pt.side === 1 && any.kind !== 'KobIfdPair' ? tok.extension_commitment : pt.a.ext)) : (pt.b.family === 'kron' ? null : (pt.side === 2 && any.kind !== 'KobIfdPair' ? tok.extension_commitment : pt.b.ext));
        parts.push({ tok, state: this.tokenState({ ...tok, extension_commitment: ext ?? tok.extension_commitment }, cov, 4, c.amount) });
      }
    } else if (info.side === 1) {
      const tok = this.tokens.get(any.state.tokenCovId);
      if (!tok) throw badRequest('ask orders need a known token');
      parts.push({ tok, state: this.tokenState(tok, cov, 4, custodyAmount(any)) });
    }
    const o = this.registerOrder({ covenantId: cov, any, txid, out: 0, value: val, daa, seq, custodyState: parts[0]?.state ?? null, deadline, origin: 'seed', parent });
    parts.forEach((c, k) => this.addTokenUtxo({ txid, index: 1 + k, token: c.tok, state: c.state, value: custodyCarrier, role: 'custody', daa, seq }));
    this.emit({ orders: [cov], tokens: bookTokens(o), fills: [], added: 1, reverted: 0 });
    return o;
  }

  /**
   * A finished order (filled or cancelled) with its events: history for the trades list. No UTXOs. `genesisDaa` / `genesisTs` and a
   * fill's `daa` / `ts` / `txid` override the defaults (market history: explicit times, two orders filled in one transaction).
   */
  seedHistory(any, { status = 'filled', fills = [], agoDaa = 1000, genesisDaa, genesisTs } = {}) {
    const n = ++this.synth;
    const cov = syntheticId('hist-cov', n);
    const daa = genesisDaa ?? Math.max(1, this.daa() - agoDaa);
    const seq = ++this.blockSeq;
    const o = this.registerOrder({ covenantId: cov, any, txid: syntheticId('hist-tx', n), out: 0, value: DEFAULT_CARRIER, daa, seq, custodyState: null, origin: 'seed' });
    if (genesisTs !== undefined) this.events[this.events.length - 1].ts = genesisTs;
    let filled = 0n;
    const t = termsOf(any);
    fills.forEach((f, i) => {
      const amount = big(f.amount, 'fill amount');
      filled += amount;
      const last = i === fills.length - 1;
      const ev = this.pushEvent({
        cov,
        kind: 'fill',
        seq: ++this.blockSeq,
        daa: f.daa ?? daa + 10 * (i + 1),
        txid: f.txid ?? syntheticId('hist-fill', `${n}:${i}`),
        token: o.token,
        side: o.side,
        amount,
        price: String(f.price),
        // an ask's maker is paid ceil(n * (price - tip) / scale)
        payout: o.side === 1 ? String(quoteOf(amount, big(f.price, 'fill price') - t.tip, t.scale, 'up')) : null,
        closes: last && status === 'filled',
      });
      ev.ts = f.ts ?? this.nowMs() - agoDaa * 100 + 1000 * (i + 1);
    });
    o.status = status;
    o.filledAmount = filled;
    o.amountLeft = 0n;
    o.current = null;
    return o;
  }

  // ------------------------------------------------------------------------------------------------ control: simulate fills / arming

  /** Splits a stored order into what a fill needs (current UTXO, custody). */
  fillContext(cov) {
    const o = this.orders.get(cov);
    if (!o) throw notFound('unknown covenant id');
    if (o.status !== 'open' && o.status !== 'partial') throw badRequest(`order is ${o.status}, not open`);
    const cur = o.current && this.utxos.get(outKey(o.current.txid, o.current.index));
    if (!cur || cur.spentBy) throw badRequest('order has no live UTXO in the mock node');
    return { o, cur };
  }

  /**
   * Simulates a fill of `amount` base units (default: one whole token or the minimum fill if larger, at most everything left) at `price` (sompi per whole
   * token; default: the current quote): performs the real transition (order UTXO -> continuation with the new state and script, custody
   * replaced, maker payout / delivery outputs), records the event and emits the WebSocket frames. IFD entries also create their exit order as
   * a child. Nothing is signed: the transition is synthetic, but the amounts follow the covenants' quote rule (an ask's maker is paid
   * `ceil(n * (price - tip) / scale)`, a bid pays `floor(n * (price + tip) / scale)` and a continuing plain bid consumes
   * `ceil(n * (pMax + tip) / scale)` of its escrow; the fill must satisfy the order's minimum fill, kob-wasm `fillOk`). A PAIR order is filled by
   * a real batch instead (pair-fill.mjs `simulatePairFill`: `via` inventory / netting / route, `leg`, evidence `mode`).
   */
  simulateFill(cov, opts = {}) {
    const { amount, price, taker } = opts;
    const { o, cur } = this.fillContext(cov);
    if (isPairKind(o.kind)) return simulatePairFill(this, cov, opts);
    const undo = { status: o.status, filledAmount: o.filledAmount, amountLeft: o.amountLeft, state: o.state };
    const any = o.state;
    const s = any.state;
    const t = termsOf(any);
    const kind = baseKind(any.kind);
    const left = t.amountLeft ?? o.amountLeft ?? 0n;
    // default: one whole token (`scale` base units), at least the minimum fill, at most everything left
    const one = t.scale > t.minFill ? t.scale : t.minFill;
    const n = amount === undefined || amount === null ? (one < left ? one : left) : big(amount, 'amount');
    if (n <= 0n) throw badRequest('amount must be positive');
    if (n > left) throw badRequest(`only ${left} base units left`);
    if (!this.kob.raw.fillOk(JSON.stringify(any), String(n), String(cur.amount))) throw badRequest(`a fill of ${n} base units breaks the order's minimum fill (${t.minFill})`);
    const daa = this.daa();
    const nowQuote = auctionOf(any, o.currentDaa, daa)?.current_price;
    const p = price !== undefined ? big(price, 'price') : BigInt(nowQuote ?? t.price);
    const txid = syntheticId('fill-tx', ++this.synth);
    const seq = ++this.blockSeq;
    const tok = this.tokens.get(o.token);
    let idx = 0;
    const children = [];
    let payout = null;
    let newLeft;
    let newAny = null;
    let newValue = cur.amount;

    if (o.side === 1) {
      // ask kinds: custody shrinks, the maker is paid
      const custody = this.liveTokenUtxos(cov, 'custody')[0];
      if (!custody) throw badRequest('order has no live custody UTXO');
      newLeft = left - n;
      payout = quoteOf(n, p - t.tip > 0n ? p - t.tip : 0n, t.scale, 'up') ?? 0n;
      const custodyUtxo = this.utxos.get(outKey(custody.txid, custody.index));
      this.spendUtxo(custodyUtxo, txid);
      let childValue = 0n;
      if (kind === 'KobIfdAsk') {
        // the exit's buy-back budget: its carrier and the prefund of n, ceil(n * prefund / scale)
        childValue = BigInt(s.exitCarrier) + quoteOf(n, s.prefund, t.scale, 'up');
        if (childValue > cur.amount) childValue = cur.amount;
      }
      newValue = cur.amount - childValue;
      if (newLeft > 0n) newAny = { kind: any.kind, state: { ...s, amountLeft: String(newLeft) } };
      this.spendUtxo(cur, txid);
      if (newAny) {
        const u = this.addUtxo({ txid, index: idx++, amount: newValue, spk: this.kob.scriptPublicKey(newAny), covenantId: cov, daa, kind: 'order' });
        u.orderCov = cov;
        this.addTokenUtxo({ txid, index: idx++, token: tok, state: this.tokenState(tok, cov, 4, newLeft), value: custody.value, role: 'custody', daa, seq });
      } else {
        this.addUtxo({ txid, index: idx++, amount: newValue + custody.value, spk: this.pkSpk(s.maker), daa, kind: 'p2pk' });
      }
      if (payout > 0n) this.addUtxo({ txid, index: idx++, amount: payout, spk: this.pkSpk(s.maker), daa, kind: 'p2pk' });
      if (taker) this.addTokenUtxo({ txid, index: idx++, token: tok, state: this.tokenState(tok, taker, 0, n), value: custody.value, role: 'owned', daa, seq });
      if (kind === 'KobIfdAsk') {
        children.push(this.spawnExit(o, n, childValue, txid, idx++, daa, seq, null));
      }
    } else {
      // bid kinds: escrow shrinks, the maker receives tokens
      const spend = quoteOf(n, p + t.tip, t.scale, 'down');
      const delivery = BigInt(s.deliveryCarrier);
      const exitCarrier = kind === 'KobIfdBid' ? BigInt(s.exitCarrier) : 0n;
      let deliveryValue = delivery;
      if (kind === 'KobBid') {
        // a plain bid consumes its budget ceil(n * (pMax + tip) / scale); the difference to what it paid rides on the delivery
        const used = BigInt(this.kob.raw.bidUsed(JSON.stringify(any), String(n)));
        const rest = cur.amount - used;
        const cont = BigInt(s.tif) === 0n && this.kob.raw.bidCanContinue(JSON.stringify(any), String(rest));
        if (cont) {
          newValue = rest - delivery;
          deliveryValue = delivery + used - spend;
          newAny = any;
        } else newValue = cur.amount - spend - delivery;
      } else {
        newValue = cur.amount - spend - delivery - exitCarrier;
        if (left - n > 0n) newAny = { kind: any.kind, state: { ...s, amountLeft: String(left - n) } };
      }
      if (newValue < 0n) throw badRequest('the bid escrow cannot pay for that fill');
      this.spendUtxo(cur, txid);
      if (newAny) {
        const u = this.addUtxo({ txid, index: idx++, amount: newValue, spk: this.kob.scriptPublicKey(newAny), covenantId: cov, daa, kind: 'order' });
        u.orderCov = cov;
      } else if (newValue > 0n) {
        this.addUtxo({ txid, index: idx++, amount: newValue, spk: this.pkSpk(s.maker), daa, kind: 'p2pk' });
      }
      if (kind === 'KobIfdBid') {
        children.push(this.spawnExit(o, n, exitCarrier, txid, idx++, daa, seq, { carrier: delivery, amount: n }));
      } else {
        this.addTokenUtxo({ txid, index: idx++, token: tok, state: this.tokenState(tok, s.maker, 0, n), value: deliveryValue, role: 'owned', daa, seq });
      }
      if (taker) this.addUtxo({ txid, index: idx++, amount: quoteOf(n, p, t.scale, 'down'), spk: this.pkSpk(taker), daa, kind: 'p2pk' });
      newLeft = kind === 'KobBid' ? (newAny ? bidBuyingPower(this.kob, any, newValue) : 0n) : left - n;
    }

    o.filledAmount += n;
    o.amountLeft = o.amountExact || kind === 'KobBid' ? newLeft : o.amountLeft;
    o.lastSeq = seq;
    o.lastDaa = daa;
    const closes = !newAny;
    if (newAny) {
      o.state = newAny;
      o.status = 'partial';
      const cu = [...this.utxos.values()].find((u) => u.txid === txid && u.orderCov === cov);
      o.current = { txid, index: cu.index, value: cu.amount };
      o.currentDaa = daa;
    } else {
      o.status = 'filled';
      o.current = null;
    }
    const ev = this.pushEvent({ cov, kind: 'fill', seq, daa, txid, token: o.token, side: o.side, amount: n, price: p.toString(), payout: payout === null ? null : payout.toString(), closes, detail: { simulated: true, exit: children[0] ?? null } });
    if (!closes && children.length === 0) this.fillUndo.set(ev.id, undo);
    const notice = this.fillNotice(o, ev, txid, n, ev.payout);
    this.emit({ orders: [cov, ...children], tokens: bookTokens(o), fills: [notice], added: 1, reverted: 0 });
    return { transactionId: txid, covenant_id: cov, status: o.status, filled_amount: o.filledAmount.toString(), amount_left: o.amountLeft === null ? null : o.amountLeft.toString(), children, event_id: ev.id };
  }

  /** Creates the exit order an if-done entry spawns on a fill (a real child with its UTXO and, for sells, custody tokens). */
  spawnExit(entry, n, value, txid, index, daa, seq, delivery) {
    const es = entry.state.state;
    const exitBase = baseKind(entry.kind) === 'KobIfdBid' ? 'KobCondAsk' : 'KobCondBid';
    const exitKind = entry.kind.endsWith('Kron') ? exitBase + 'Kron' : exitBase;
    // a KCC-20 buy-first entry writes its own extensionCommitment behind the repeat fields of its KobCondAsk exit
    const extTail = exitKind === 'KobCondAsk' ? '20' + es.extensionCommitment : '';
    const exit = this.kob.decodeState(exitKind, es.exitState + EXIT_TAIL[exitBase] + extTail);
    exit.state.amountLeft = String(n);
    const cov = syntheticId('exit-cov', ++this.synth);
    const u = this.addUtxo({ txid, index, amount: value, spk: this.kob.scriptPublicKey(exit), covenantId: cov, daa, kind: 'order' });
    u.orderCov = cov;
    const tok = this.tokens.get(entry.token);
    let custodyState = null;
    if (delivery) {
      custodyState = this.tokenState(tok, cov, 4, delivery.amount);
      this.addTokenUtxo({ txid, index: index + 1, token: tok, state: custodyState, value: delivery.carrier, role: 'custody', daa, seq });
    }
    this.registerOrder({ covenantId: cov, any: exit, txid, out: index, value, daa, seq, custodyState, origin: 'chain', parent: entry.covenantId });
    entry.children.push(cov);
    return cov;
  }

  /** Arms a stop / stop-entry: performs the `armed` transition of the state (new UTXO, new script), emits an `arm` event. */
  /** Marks / unmarks an order as possibly frozen (the executor's pre-simulation of its next fill failed). */
  setPossiblyFrozen(cov, frozen) {
    const o = this.orders.get(cov);
    if (!o) throw badRequest(`unknown order ${cov}`);
    o.possiblyFrozen = !!frozen;
    this.emit({ orders: [cov], tokens: [o.token], fills: [], added: 0, reverted: 0 });
    return { covenant_id: cov, possibly_frozen: o.possiblyFrozen };
  }

  armOrder(cov, opts = {}) {
    const { o, cur } = this.fillContext(cov);
    if (isPairKind(o.kind)) return simulatePairArm(this, cov, opts);
    const s = o.state.state;
    if (s.armed === undefined) throw badRequest(`${o.kind} has no armed state`);
    const daa = this.daa();
    if (BigInt(s.armed) !== 0n) throw badRequest('order is already armed');
    const isIfd = baseKind(o.kind) === 'KobIfdBid' || baseKind(o.kind) === 'KobIfdAsk';
    if (isIfd && BigInt(s.entryStop ?? 0) <= 0n) throw badRequest('the entry has no stop to arm');
    const armed = BigInt(s.bandDaa ?? 0) > 0n ? String(daa) : '1';
    const newAny = { kind: o.kind, state: { ...s, armed } };
    const txid = syntheticId('arm-tx', ++this.synth);
    const seq = ++this.blockSeq;
    this.spendUtxo(cur, txid);
    const u = this.addUtxo({ txid, index: 0, amount: cur.amount, spk: this.kob.scriptPublicKey(newAny), covenantId: cov, daa, kind: 'order' });
    u.orderCov = cov;
    // an ask-side order keeps its custody UTXO untouched (arming does not move tokens)
    o.state = newAny;
    o.current = { txid, index: 0, value: cur.amount };
    o.currentDaa = daa;
    o.lastSeq = seq;
    o.lastDaa = daa;
    this.pushEvent({ cov, kind: 'arm', seq, daa, txid, token: o.token, side: o.side, detail: { simulated: true } });
    this.emit({ orders: [cov], tokens: [o.token], fills: [], added: 1, reverted: 0 });
    return { transactionId: txid, covenant_id: cov, armed };
  }

  reorg(blocks = 1) {
    this.emit({ orders: [], tokens: [], fills: [], added: blocks, reverted: blocks });
  }

  /**
   * A re-org that takes the LAST simulated fill of an order back out (the indexer's view of the order: its fill event, amounts, status and state; the
   * mock's UTXOs are not rolled back, so the order should not be traded or cancelled afterwards). Only a partial fill of a plain order can be undone
   * (a fill that closed the order or spawned an exit cannot). Emits the order notice and a `reorg` frame of `blocks` blocks.
   */
  revertLastFill(cov, blocks = 1) {
    const o = this.orders.get(cov);
    if (!o) throw notFound('unknown covenant id');
    let at = -1;
    for (let i = this.events.length - 1; i >= 0; i--) if (this.events[i].covenant_id === cov && this.events[i].kind === 'fill') { at = i; break; }
    const ev = at >= 0 ? this.events[at] : null;
    const undo = ev ? this.fillUndo.get(ev.id) : null;
    if (!ev || !undo) throw badRequest('the last fill of this order cannot be reverted (none, or it closed the order)');
    this.events.splice(at, 1);
    this.fillUndo.delete(ev.id);
    Object.assign(o, undo);
    this.emit({ orders: [cov], tokens: bookTokens(o), fills: [], added: blocks, reverted: blocks });
    return { covenant_id: cov, status: o.status, filled_amount: o.filledAmount.toString(), reverted_amount: ev.amount };
  }
}
