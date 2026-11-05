// The mock node's blocks, for standalone mode (src/data/standalone.ts): every transaction the mock chain accepts (`MockChain.applyTx`: the app's
// submissions, the real batches of the pair fills, `placeReal` below) becomes one block in the wRPC `block-added` layout (src/data/block-stream.ts),
// served by `GET /node/blocks?after=<seq>`. The mock's synthetic state changes (seeded orders, `simulateFill`) have no transaction and are no block: an
// order seeded before is an order standalone mode never saw, as on a real node.
//
// `placeReal` places an order with a REAL transaction (kob-wasm builds it, the test key signs it, the script engine validates it), so that standalone
// mode can see it in a block: POST /mock/place { key, side: 'ask' | 'bid', price, amount, token? }.
import { schnorr } from '@noble/curves/secp256k1.js';
import { DEFAULT_CARRIER } from './chain.mjs';
import { TEST_PUBKEYS, TEST_SECRETS } from './keys.mjs';
import { bidEscrow } from './model.mjs';
import { buildOrderState } from './seed.mjs';
import { badRequest, syntheticId } from './util.mjs';

/** Blocks kept (the oldest are forgotten). */
const MAX_BLOCKS = 5_000;
/** Most blocks one answer holds. */
const PAGE = 200;
const KAS = 100_000_000n;
const J = JSON.stringify;
const hexBytes = (h) => Uint8Array.from(Buffer.from(h, 'hex'));

/** A transaction (kaspa-wasm safe JSON, as the mock accepts it) in the layout a node's block notification carries: no input UTXO entries. */
export function rpcTxOf(tx) {
  return {
    version: tx.version,
    inputs: tx.inputs.map((i) => ({
      previousOutpoint: { transactionId: i.transactionId, index: i.index },
      signatureScript: i.signatureScript,
      sequence: String(i.sequence),
      sigOpCount: i.sigOpCount,
      computeBudget: i.computeBudget,
    })),
    outputs: tx.outputs.map((o) => ({ value: String(o.value), scriptPublicKey: o.scriptPublicKey, ...(o.covenant ? { covenant: { authorizingInput: o.covenant.authorizingInput, covenantId: o.covenant.covenantId } } : {}) })),
    lockTime: String(tx.lockTime),
    subnetworkId: tx.subnetworkId,
    gas: String(tx.gas),
    payload: tx.payload,
    storageMass: String(tx.storageMass ?? '0'),
    verboseData: { transactionId: tx.id },
  };
}

/** The block log of a chain: `record` is called by `MockChain.applyTx` for every accepted transaction. */
export class BlockLog {
  constructor() {
    this.reset();
  }
  reset() {
    this.seq = 0;
    this.blocks = [];
  }
  record(tx, daa, tsMs) {
    const seq = ++this.seq;
    this.blocks.push({ seq, header: { hash: syntheticId('block', seq), daaScore: String(daa), timestamp: String(tsMs) }, transactions: [rpcTxOf(tx)] });
    if (this.blocks.length > MAX_BLOCKS) this.blocks.splice(0, this.blocks.length - MAX_BLOCKS);
  }
  /** `GET /node/blocks`: the tip, and the blocks after `after` (none without it), oldest first. */
  view(after) {
    if (after === null || after === undefined) return { tip: this.seq, blocks: [] };
    const blocks = this.blocks.filter((b) => b.seq > after).slice(0, PAGE);
    return { tip: blocks.length ? blocks[blocks.length - 1].seq : Math.max(after, this.seq), blocks };
  }
}

/**
 * Places a plain ask or bid of a test key with a real transaction: funds the key (and gives an ask its tokens), builds the placement with kob-wasm,
 * signs it with the key's test secret and submits it to the chain. Returns the transaction id and the order's covenant id.
 */
export function placeReal(chain, spec) {
  const name = spec.key ?? 'maker';
  if (!(name in TEST_SECRETS)) throw badRequest(`unknown test key "${name}": use one of ${Object.keys(TEST_SECRETS).join(', ')}`);
  const pk = TEST_PUBKEYS[name];
  const sk = TEST_SECRETS[name];
  const side = spec.side === 'bid' ? 'bid' : 'ask';
  const any = buildOrderState(chain, { ...spec, side, maker: name });
  const tok = chain.tokenRef(spec.token ?? [...chain.tokens.keys()][0]);
  const isAsk = side === 'ask';
  const amount = BigInt(spec.amount ?? any.state.amountLeft ?? 0);
  const value = isAsk ? DEFAULT_CARRIER : bidEscrow(chain.kob, any, amount, 1);
  const g = chain.giveKas(pk, value + 1_000n * KAS);
  const funding = [{ transactionId: g.transactionId, index: 0, amount: g.amount, blockDaaScore: String(chain.daa()), covenantId: null, pubkey: pk }];
  const tokens = [];
  if (isAsk) {
    const t = chain.giveTokens(pk, tok.covenant_id, amount);
    const rec = chain.tokenUtxos.find((x) => x.txid === t.transactionId && x.index === 0);
    tokens.push({ transactionId: rec.txid, index: rec.index, amount: rec.value.toString(), blockDaaScore: String(rec.created_daa), covenantId: rec.token, state: rec.state });
  }
  const request = { action: 'createOrder', order: any, value: String(value), tokens, tokenCarrier: String(DEFAULT_CARRIER), funding, change: pk, fee: { feeRate: null, feeMode: 'relay' } };
  let built;
  try {
    built = JSON.parse(chain.kob.raw.build(J(request)));
  } catch (e) {
    throw badRequest(`kob-wasm refused the placement: ${String(e?.message ?? e)}`);
  }
  const sigs = built.sign.map((r) => {
    if (r.pubkey !== pk) throw badRequest(`the placement asks for a signature by ${r.pubkey}, not ${name}`);
    return { inputIndex: r.inputIndex, signature: Buffer.from(schnorr.sign(hexBytes(r.sighash), hexBytes(sk))).toString('hex') + '01' };
  });
  const signed = JSON.parse(chain.kob.raw.finalize(J(built), J(sigs), J({ tightenBudgets: true })));
  chain.submit(signed.tx);
  const created = chain.kob.recoverOrders(signed.tx);
  return { transactionId: signed.tx.id, covenant_id: created[0]?.covenantId ?? null, kind: any.kind };
}

/**
 * Fills a plain ask or bid with a REAL batch transaction (the mock's filler key takes it: KAS for an ask, minted tokens for a bid), so that standalone
 * mode sees the fill in a block: POST /mock/take { covenant_id, amount? } (base units; default one whole token or the minimum fill, at most all left).
 */
export function takeReal(chain, cov, amount) {
  const { o, cur } = chain.fillContext(cov);
  const kind = o.state.kind.replace(/Kron$/, '');
  if (kind !== 'KobAsk' && kind !== 'KobBid') throw badRequest(`only a plain ask or bid can be taken here, not ${o.state.kind}`);
  const s = o.state.state;
  const scale = BigInt(s.scale);
  const left = BigInt(s.amountLeft ?? o.amountLeft ?? 0);
  const one = scale > BigInt(s.minFill) ? scale : BigInt(s.minFill);
  const n = amount === undefined || amount === null ? (one < left ? one : left) : BigInt(amount);
  const filler = TEST_PUBKEYS.filler;
  const order = { transactionId: cur.txid, index: cur.index, amount: cur.amount.toString(), blockDaaScore: String(cur.daa), covenantId: cov, state: s };
  const tokenUtxoOf = (rec) => ({ transactionId: rec.txid, index: rec.index, amount: rec.value.toString(), blockDaaScore: String(rec.created_daa), covenantId: rec.token, state: rec.state });
  const custodyRec = chain.liveTokenUtxos(cov, 'custody').find((u) => u.token === o.token);
  const takerTokens = [];
  if (kind === 'KobBid') {
    const g = chain.giveTokens(filler, o.token, n);
    takerTokens.push(tokenUtxoOf(chain.tokenUtxos.find((t) => t.txid === g.transactionId && t.index === 0)));
  }
  const leg = kind === 'KobAsk' ? { kind: 'ask', order, custody: custodyRec ? tokenUtxoOf(custodyRec) : null, amount: String(n), t: null } : { kind: 'bid', order, amount: String(n), t: null };
  const g = chain.giveKas(filler, 5_000n * KAS);
  const request = {
    action: 'batch', lockTime: String(chain.daa()), legs: [leg], updates: [], takerTokens, taker: filler, takerTokenCarrier: '1000000000', receivers: [], payments: [],
    funding: [{ transactionId: g.transactionId, index: 0, amount: g.amount, blockDaaScore: String(chain.daa()), covenantId: null, pubkey: filler }], change: filler, records: [],
    fee: { feeRate: null, feeMode: 'relay' },
  };
  let built;
  try {
    built = JSON.parse(chain.kob.raw.build(J(request)));
  } catch (e) {
    throw badRequest(`kob-wasm refused the fill: ${String(e?.message ?? e)}`);
  }
  const sigs = built.sign.map((r) => ({ inputIndex: r.inputIndex, signature: Buffer.from(schnorr.sign(hexBytes(r.sighash), hexBytes(TEST_SECRETS.filler))).toString('hex') + '01' }));
  const signed = JSON.parse(chain.kob.raw.finalize(J(built), J(sigs), J({ tightenBudgets: true })));
  chain.submit(signed.tx);
  return { transactionId: signed.tx.id, covenant_id: cov, amount: n.toString() };
}
