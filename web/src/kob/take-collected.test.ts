// Selling into a Collected (mode 1) bid that has bought: every fill spends its bought-custody and,
// while the bid continues, continues it with bought + n; the fill that ends the bid releases the
// whole custody to the maker (KobBid.sil fill). Each take is built by kob-wasm, signed with the
// taker's key only and run in the script engine, and the pre-sign decoder accepts it.
import { describe, expect, it } from 'vitest';
import type { BookOrderUtxo, BookView, OrderPlan, PlanEnv } from './plan-types';
import { errors } from './plan-types';
import { planOrder } from './plan';
import { decodeSigning } from './decode';
import { expectedFromPlan } from '../ui/ticket/plan-expected';
import { custodyState } from './token-state';
import type { OrderState, TemplateName, TokenUtxo, TxJson } from './types';
import { CLOCK, KAS, MAKER_PK, MAKER_SK, OTHER_PK, TOK, consensusCheck, keyUtxo, kob, makeEnv, market3x3, tokenUtxo } from '../testing/fixtures';
import { nodeFactsOf, snapshotOfRecovered } from '../testing/chain-fixtures';

const K = kob();
const M = market3x3();
const BOUGHT = 3n * TOK;
const PRICE = 250_000_000n;

/** A Collected bid of OTHER_PK for `amount` that has bought BOUGHT, with its bought-custody. */
function collectedBid(amount: bigint, minFill = TOK): { row: { covenantId: string; price: bigint; amount: bigint; tip: bigint }; utxo: BookOrderUtxo; custody: TokenUtxo } {
  const env: PlanEnv = { ...makeEnv({ market: M, book: { asks: [], bids: [] } }), maker: OTHER_PK, funding: [keyUtxo(10_000n * KAS, OTHER_PK)], tokenUtxos: [tokenUtxo(M, 1_000n * TOK, 10n * KAS, OTHER_PK)] };
  const p = planOrder(env, { type: 'limit', side: 'buy', amount, price: PRICE, minFill });
  expect(errors(p)).toEqual([]);
  const snap = snapshotOfRecovered(K.recoverOrders(p.built!.tx)[0]!, CLOCK.daa - 1000n);
  const st = { ...snap.order.state, state: { ...snap.order.state.state, mode: '1', recvThreshold: '0', topupTip: '0', bought: BOUGHT.toString() } } as OrderState;
  const order = { ...snap.order, state: st, amount: snap.order.amount };
  const custody: TokenUtxo = { transactionId: 'cb'.repeat(32), index: 1, amount: KAS.toString(), blockDaaScore: String(CLOCK.daa - 900n), covenantId: M.covenantId, state: custodyState('kcc20', BOUGHT, snap.covenantId, M.extensionCommitment) };
  return { row: { covenantId: snap.covenantId, price: PRICE, amount, tip: 0n }, utxo: { covenantId: snap.covenantId, order, custody: null, boughtCustody: custody }, custody };
}

function takerEnv(bid: ReturnType<typeof collectedBid>, utxo: BookOrderUtxo = bid.utxo): PlanEnv {
  const book: BookView = { asks: [], bids: [{ price: bid.row.price, amount: bid.row.amount, orders: 1 }], askOrders: [], bidOrders: [bid.row] };
  return { ...makeEnv({ book, tokenAmounts: [100n * TOK] }), bookOrders: new Map([[bid.row.covenantId.toLowerCase(), utxo]]) };
}
const sell = (env: PlanEnv, amount: bigint): OrderPlan => planOrder(env, { type: 'limit', side: 'sell', amount, price: PRICE, crossing: 'take' });
const outOf = (p: OrderPlan, owner: string) => p.built!.tx.outputs.findIndex((o) => o.covenant?.covenantId === M.covenantId && JSON.stringify(o).includes(owner));

describe('selling into a Collected bid that has bought', () => {
  it('a partial fill spends the bought-custody and continues it with bought + n; consensus-valid; the decoder accepts it', () => {
    const bid = collectedBid(10n * TOK);
    const p = sell(takerEnv(bid), 2n * TOK);
    expect(errors(p)).toEqual([]);
    expect(p.take!.amount).toBe(2n * TOK);
    const tx = p.built!.tx;
    expect(tx.inputs.some((i) => i.transactionId === bid.custody.transactionId && i.index === bid.custody.index)).toBe(true);
    consensusCheck(K, p.built!, [MAKER_SK]);
    // the bid continues with bought + n, and the custody continues owned by the order
    const cont = (p.built!.plans as { nextStates?: { amount?: string; owner?: string }[] }[]).flatMap((x) => x.nextStates ?? []);
    expect(cont.some((s) => s.owner === bid.utxo.covenantId && s.amount === (BOUGHT + 2n * TOK).toString())).toBe(true);
    const d = decodeSigning({ kob: K, built: p.built!, maker: MAKER_PK, expected: expectedFromPlan(p), nodeInputs: nodeFactsOf(p.built!) });
    expect(d.blocking.map((b) => `${b.code}: ${b.message}`)).toEqual([]);
    // the decoder names the bought-custody the fill moves, and the take of 2 tokens
    expect(d.spends.find((x) => x.covenantId === bid.utxo.covenantId)?.bought).toMatchObject({ amount: BOUGHT });
    expect(d.spends.find((x) => x.covenantId === bid.utxo.covenantId)?.bought?.input).not.toBeNull();
    expect(d.take?.amount).toBe(2n * TOK);
    // the taker receives KAS for the tokens it sold and nothing of the custody
    const mine = d.net.tokens.find((t) => t.ref.covenantId === M.covenantId)!;
    expect(mine.fromWallet - mine.toMaker).toBe(2n * TOK);
    expect(outOf(p, OTHER_PK)).toBe(-1);
  });

  it('the fill that ends the bid releases the whole custody, bought + n, to the maker; consensus-valid', () => {
    const bid0 = collectedBid(2n * TOK);
    // listed at more than its escrow can buy: the take takes all its buying power, which ends it
    const bid = { ...bid0, row: { ...bid0.row, amount: 1_000n * TOK } };
    const p = sell(takerEnv(bid), 1_000n * TOK);
    expect(errors(p)).toEqual([]);
    consensusCheck(K, p.built!, [MAKER_SK]);
    const cont = (p.built!.plans as { nextStates?: { amount?: string; owner?: string }[] }[]).flatMap((x) => x.nextStates ?? []);
    expect(p.take!.amount).toBeGreaterThanOrEqual(2n * TOK);
    expect(cont.some((s) => s.owner === OTHER_PK && s.amount === (BOUGHT + p.take!.amount).toString())).toBe(true);
    // no output stays bound to the order
    expect(p.built!.tx.outputs.some((o) => o.covenant?.covenantId === bid.utxo.covenantId)).toBe(false);
    const d = decodeSigning({ kob: K, built: p.built!, maker: MAKER_PK, expected: expectedFromPlan(p), nodeInputs: nodeFactsOf(p.built!) });
    expect(d.blocking.map((b) => `${b.code}: ${b.message}`)).toEqual([]);
  });

  it('an ending fill with 1 KAS or more left refunds it to the maker; consensus-valid; the decoder accepts it (R4A-1)', () => {
    // 55 KAS of escrow (it buys 22), minFill 3: selling 21 leaves 2.5 KAS, less than one more minimum
    // fill buys
    const bid0 = collectedBid(10n * TOK, 3n * TOK);
    const bid = { ...bid0, row: { ...bid0.row, amount: 21n * TOK } };
    const p = sell(takerEnv(bid), 21n * TOK);
    expect(errors(p)).toEqual([]);
    expect(p.take!.amount).toBe(21n * TOK);
    consensusCheck(K, p.built!, [MAKER_SK]);
    const tx = p.built!.tx;
    const i = tx.inputs.findIndex((x) => x.utxo.covenantId === bid.utxo.covenantId);
    const r = tx.outputs.length - 1 - i;
    expect(tx.outputs[r]).toMatchObject({ scriptPublicKey: `000020${OTHER_PK}ac`, value: (BigInt(bid.utxo.order.amount) - 21n * PRICE).toString(), covenant: null });
    const d = decodeSigning({ kob: K, built: p.built!, maker: MAKER_PK, expected: expectedFromPlan(p), nodeInputs: nodeFactsOf(p.built!) });
    expect(d.blocking.map((b) => `${b.code}: ${b.message}`)).toEqual([]);
    expect(d.outputs[r]).toMatchObject({ kind: 'fill-payout', recipient: OTHER_PK });
  });

  it('negative: without its bought-custody the bid is not taken (it was refused at build before)', () => {
    const bid = collectedBid(10n * TOK);
    const p = sell(takerEnv(bid, { ...bid.utxo, boughtCustody: null }), 2n * TOK);
    expect(p.take?.amount ?? 0n).toBe(0n);
  });
});

describe('the standalone book follows such a bid through a fill (R3A-3)', () => {
  it('to bought + n, with the bought-custody the fill continued; the order view names it for the next taker', async () => {
    const { followSpend, revealSpend } = await import('./stream-orders');
    const { orderViewOf } = await import('../data/stream-book');
    const bid = collectedBid(10n * TOK);
    const p = sell(takerEnv(bid), 2n * TOK);
    const signed = consensusCheck(K, p.built!, [MAKER_SK]);
    const i = signed.tx.inputs.findIndex((x) => x.transactionId === bid.utxo.order.transactionId && x.index === bid.utxo.order.index);
    const spend = revealSpend(K, signed.tx, i)!;
    expect(spend.n).toBe(2n * TOK);
    const known = { covenantId: bid.utxo.covenantId, order: bid.utxo.order, custody: null, boughtCustody: bid.custody, strays: [], refundDueDaa: null, deadline: null, source: 'chain' as const };
    const next = (await followSpend(K, spend, signed.tx, CLOCK.daa, 'testnet-10', known))!;
    expect(next).not.toBeNull();
    expect((next.order.state.state as unknown as { bought: string }).bought).toBe((BOUGHT + 2n * TOK).toString());
    expect(next.boughtCustody).toMatchObject({ transactionId: signed.tx.id, state: { amount: (BOUGHT + 2n * TOK).toString(), owner: bid.utxo.covenantId } });
    const view = orderViewOf({ snap: next, confirmed: true, origin: 'stream', genesis: { txid: null, out: null, daa: 0n }, firstAmount: null, seenMs: 0, misses: 0 }, { kob: K, daa: CLOCK.daa, nowUnix: CLOCK.unixSeconds });
    expect(view.bought_custody?.utxo).toMatchObject({ txid: signed.tx.id, amount: (BOUGHT + 2n * TOK).toString() });
  });

  // A transaction that spends the bid (input 0) by its cancel entry and continues it at output 0,
  // as the stream sees it: only the outpoints, the outputs and the payload are read.
  const cancelSpend = async (state: OrderState, payload = '', spendCustody = false) => {
    const { followSpend } = await import('./stream-orders');
    const bid = collectedBid(10n * TOK);
    const utxo = (u: { transactionId: string; index: number; amount: string }) => ({ transactionId: u.transactionId, index: u.index, sequence: '0', sigOpCount: 1, computeBudget: 0, signatureScript: '', utxo: { address: null, amount: u.amount, scriptPublicKey: '', blockDaaScore: '0', isCoinbase: false, covenantId: null } });
    const tx = {
      id: 'dd'.repeat(32), version: 0, subnetworkId: '00'.repeat(20), lockTime: '0', gas: '0', storageMass: '0', payload,
      inputs: [utxo(bid.utxo.order), ...(spendCustody ? [utxo(bid.custody)] : [])],
      outputs: [{ value: bid.utxo.order.amount, scriptPublicKey: K.scriptPublicKey(state), covenant: { authorizingInput: 0, covenantId: bid.utxo.covenantId } }],
    } as unknown as TxJson;
    const known = { covenantId: bid.utxo.covenantId, order: bid.utxo.order, custody: null, boughtCustody: bid.custody, strays: [], refundDueDaa: null, deadline: null, source: 'chain' as const };
    return { bid, next: await followSpend(K, { input: 0, state: bid.utxo.order.state, entry: 'cancel', n: null }, tx, CLOCK.daa, 'testnet-10', known) };
  };
  const bidState = (b: ReturnType<typeof collectedBid>, edit: Record<string, string>) => ({ ...b.utxo.order.state, state: { ...b.utxo.order.state.state, ...edit } }) as OrderState;

  it('a sweep in place keeps the bid listed with the bought-custody it did not move (R4A-2)', async () => {
    const st = collectedBid(10n * TOK).utxo.order.state;
    const { bid, next } = await cancelSpend(st);
    expect(next).not.toBeNull();
    expect(next!.boughtCustody).toMatchObject({ transactionId: bid.custody.transactionId, index: bid.custody.index });
  });

  it('an in-place amend keeps the bought-custody too; one that spends it drops the bid rather than list it untakeable (R4A-2)', async () => {
    const st = bidState(collectedBid(10n * TOK), { price: (PRICE + 1_000_000n).toString() });
    const payload = K.encodePayload([{ type: 'amend', output: 0, input: 0, family: 1, template: st.kind as TemplateName, state: K.encodeState(st) }]);
    const { bid, next } = await cancelSpend(st, payload);
    expect(next).not.toBeNull();
    expect((next!.order.state.state as unknown as { price: string }).price).toBe((PRICE + 1_000_000n).toString());
    expect(next!.boughtCustody).toMatchObject({ transactionId: bid.custody.transactionId, index: bid.custody.index });
    expect((await cancelSpend(st, payload, true)).next).toBeNull();
  });

  it('a claim continues the bid with bought = 0, listed without a custody (R4A-2)', async () => {
    const st = bidState(collectedBid(10n * TOK), { bought: '0' });
    const { next } = await cancelSpend(st, '', true);
    expect(next).not.toBeNull();
    expect((next!.order.state.state as unknown as { bought: string }).bought).toBe('0');
    expect(next!.boughtCustody ?? null).toBeNull();
  });
});
