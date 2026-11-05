// What standalone mode reads from a block's transactions, on consensus-valid transactions (the golden vectors, signed and validated by kob-wasm) as a
// node announces them: without the UTXO entries of their inputs.
import { describe, expect, it } from 'vitest';
import { loadKobNode } from './wasm.node';
import { fillOf, followSpend, revealSpend, snapshotOfPlacement, txFacts } from './stream-orders';
import { goldenTx, placeGolden, snapshotOfRecovered } from '../testing/chain-fixtures';
import type { OrderSnapshot } from './cancel';
import type { OrderState, PayloadRecord, TokenUtxo, TxJson } from './types';

const kob = loadKobNode();
const EMPTY = { address: null, amount: '0', scriptPublicKey: '', blockDaaScore: '0', isCoinbase: false, covenantId: null };
/** A transaction as a node's block notification carries it: no input UTXO entries. */
const asAnnounced = (tx: TxJson): TxJson => ({ ...tx, inputs: tx.inputs.map((i) => ({ ...i, utxo: { ...EMPTY } })) });
const clone = <T>(v: T): T => JSON.parse(JSON.stringify(v)) as T;

/** The resting order a golden batch leg spends, as the book would hold it (its order UTXO and custody). */
function legSnapshot(name: string): OrderSnapshot {
  const t = goldenTx(name).request as unknown as { legs: { kind: string; order: { transactionId: string; index: number; amount: string; covenantId: string; state: Record<string, string>; blockDaaScore: string }; custody?: TokenUtxo | null }[] };
  const l = t.legs[0]!;
  const kind = l.kind === 'ask' ? 'KobAsk' : 'KobBid';
  return {
    covenantId: l.order.covenantId,
    order: { transactionId: l.order.transactionId, index: l.order.index, amount: l.order.amount, blockDaaScore: l.order.blockDaaScore, covenantId: l.order.covenantId, state: { kind, state: l.order.state } as unknown as OrderState },
    custody: l.custody ?? null,
    strays: [], refundDueDaa: null, deadline: null, source: 'chain',
  };
}

describe('placements (txFacts)', () => {
  it('re-derives the order a placement creates from the announced transaction alone', () => {
    const p = placeGolden(kob, 'create.ask');
    const f = txFacts(kob, asAnnounced(p.signed.tx));
    expect(f.placements).toHaveLength(1);
    expect(f.placements[0]!.covenantId).toBe(p.recovered[0]!.covenantId);
    expect(snapshotOfPlacement(f.placements[0]!, 1000n)).toEqual(snapshotOfRecovered(p.recovered[0]!, 1000n));
    expect(f.spends).toEqual(p.signed.tx.inputs.map((i) => `${i.transactionId}:${i.index}`));
  });

  it('checks every order record on its own: a bad record next to a good one hides nothing', () => {
    const p = placeGolden(kob, 'create.bid');
    const tx = asAnnounced(p.signed.tx);
    const good = (kob.decodePayload(tx.payload)!.records.find((r) => r.type === 'order') as Extract<PayloadRecord, { type: 'order' }>);
    const bad = { ...good, output: good.output + 5 };
    const both = { ...tx, payload: kob.encodePayload([bad, good]) };
    expect(() => kob.recoverOrders(both)).toThrow();
    expect(txFacts(kob, both).placements.map((r) => r.covenantId)).toEqual([p.recovered[0]!.covenantId]);
  });

  it('follows only the plain ask and bid of either family; another payload or none creates nothing', () => {
    expect(txFacts(kob, asAnnounced(placeGolden(kob, 'kron.create.ask').signed.tx)).placements).toHaveLength(1);
    expect(txFacts(kob, asAnnounced(placeGolden(kob, 'create.condAsk').signed.tx)).placements).toEqual([]);
    expect(txFacts(kob, asAnnounced(placeGolden(kob, 'create.ifdBid').signed.tx)).placements).toEqual([]);
    const tx = asAnnounced(placeGolden(kob, 'create.ask').signed.tx);
    expect(txFacts(kob, { ...tx, payload: '' }).placements).toEqual([]);
    expect(txFacts(kob, { ...tx, payload: 'deadbeef' }).placements).toEqual([]);
  });
});

describe('spends (revealSpend, followSpend, fillOf)', () => {
  const taken = (name: string) => asAnnounced(clone(goldenTx(name).signed.tx) as TxJson);

  it('reads an ask fill: the spent state from its redeem script, the settle entry and its size', () => {
    const snap = legSnapshot('take.ask.partial');
    const s = revealSpend(kob, taken('take.ask.partial'), 0)!;
    expect(s.state).toEqual(snap.order.state);
    expect(s).toMatchObject({ input: 0, entry: 'settle', n: 4000n });
    // the funding input of the taker is no order
    expect(revealSpend(kob, taken('take.ask.partial'), 2)).toBeNull();
  });

  it('a cancel and a refund are spends without a fill', () => {
    expect(revealSpend(kob, taken('cancel.ask'), 0)).toMatchObject({ entry: 'cancel', n: null });
    expect(revealSpend(kob, taken('refund.ask.expiry'), 0)).toMatchObject({ entry: 'settle', n: null });
    expect(revealSpend(kob, taken('refund.bid'), 0)).toMatchObject({ entry: 'refund', n: null });
    expect(fillOf(kob, revealSpend(kob, taken('cancel.ask'), 0)!, 'aa'.repeat(32), null)).toBeNull();
  });

  it('a redeem script that is not exactly the pinned template of its state is not read as an order', () => {
    const tx = taken('take.ask.partial');
    // one byte of the suffix changed: the state still decodes, the script is not the template's
    const sig = tx.inputs[0]!.signatureScript;
    tx.inputs[0]!.signatureScript = sig.slice(0, -2) + (sig.slice(-2) === '00' ? '01' : '00');
    expect(revealSpend(kob, tx, 0)).toBeNull();
  });

  it('follows an ask through a partial fill: amountLeft - n at the output continuing its covenant id, with the new custody', async () => {
    const snap = legSnapshot('take.ask.partial');
    const tx = taken('take.ask.partial');
    const s = revealSpend(kob, tx, 0)!;
    const next = (await followSpend(kob, s, tx, 2000n, 'testnet-10', snap))!;
    expect(next.covenantId).toBe(snap.covenantId);
    expect(next.order).toMatchObject({ transactionId: tx.id, index: 1, amount: '1000000000', blockDaaScore: '2000' });
    expect((next.order.state.state as unknown as { amountLeft: string }).amountLeft).toBe('6000');
    expect(kob.scriptPublicKey(next.order.state)).toBe(tx.outputs[1]!.scriptPublicKey);
    expect(next.custody).toMatchObject({ transactionId: tx.id, covenantId: snap.custody!.covenantId });
    expect(next.custody!.state.amount).toBe('6000');
    // the same continuation without the book's snapshot (an order placed before the mode started, learned from its fill)
    expect((await followSpend(kob, s, tx, 2000n, 'testnet-10', null))!.order).toEqual(next.order);
    expect(fillOf(kob, s, tx.id, next.covenantId)).toEqual({ txid: tx.id, covenantId: snap.covenantId, token: '70'.repeat(32), side: 'sell', amount: 4000n, price: 250000000n, scale: 1000n });
  });

  it('follows a bid through a fill (same state, a smaller escrow) and ends an order a cancel spends', async () => {
    const bid = legSnapshot('take.bid.partial');
    const tx = taken('take.bid.partial');
    const s = revealSpend(kob, tx, 0)!;
    expect(s).toMatchObject({ entry: 'fill', n: 4000n });
    const next = (await followSpend(kob, s, tx, 2000n, 'testnet-10', bid))!;
    expect(next.order.state).toEqual(bid.order.state);
    expect(next.order).toMatchObject({ transactionId: tx.id, index: 1, amount: tx.outputs[1]!.value });
    expect(fillOf(kob, s, tx.id, bid.covenantId)).toMatchObject({ side: 'buy', amount: 4000n });
    const c = taken('cancel.ask');
    expect(await followSpend(kob, revealSpend(kob, c, 0)!, c, 2000n, 'testnet-10', legSnapshot('take.ask.partial'))).toBeNull();
  });

  it('a continuation bound to another covenant id than the book holds is not the order', async () => {
    const snap = { ...legSnapshot('take.ask.partial'), covenantId: 'ff'.repeat(32) };
    const tx = taken('take.ask.partial');
    expect(await followSpend(kob, revealSpend(kob, tx, 0)!, tx, 2000n, 'testnet-10', snap)).toBeNull();
  });
});
